//! System clipboard sync for `clipboard = "unnamedplus"`, as LazyVim sets it: yanks reach the
//! system clipboard and `p` pastes what another program copied.
//!
//! It uses wl-copy/wl-paste, xclip or xsel like Neovim's clipboard provider. Without any of
//! them, yanks are sent to the terminal with OSC 52 and pastes use the internal register.

use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use crate::editor::{
    CommandInvocation, EditorWindowVersion, RegisterShape, RegisterStore, RegisterValue,
};
use std::collections::VecDeque;

const READ_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tool {
    Wayland,
    Xclip,
    Xsel,
}

impl Tool {
    fn detect() -> Option<Self> {
        let has = |name: &str| crate::tools::discover_executable(name).is_some();
        if std::env::var_os("WAYLAND_DISPLAY").is_some() && has("wl-copy") && has("wl-paste") {
            Some(Self::Wayland)
        } else if std::env::var_os("DISPLAY").is_some() && has("xclip") {
            Some(Self::Xclip)
        } else if std::env::var_os("DISPLAY").is_some() && has("xsel") {
            Some(Self::Xsel)
        } else {
            None
        }
    }

    fn copy_command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Wayland => ("wl-copy", &["--type", "text/plain"]),
            Self::Xclip => ("xclip", &["-i", "-selection", "clipboard"]),
            Self::Xsel => ("xsel", &["-i", "-b"]),
        }
    }

    fn paste_command(self, primary: bool) -> (&'static str, &'static [&'static str]) {
        if primary {
            return match self {
                Self::Wayland => ("wl-paste", &["--primary", "--no-newline"]),
                Self::Xclip => ("xclip", &["-o", "-selection", "primary"]),
                Self::Xsel => ("xsel", &["-o", "-p"]),
            };
        }
        match self {
            Self::Wayland => ("wl-paste", &["--no-newline"]),
            Self::Xclip => ("xclip", &["-o", "-selection", "clipboard"]),
            Self::Xsel => ("xsel", &["-o", "-b"]),
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct ClipboardSession {
    enabled: bool,
    tool: Option<Tool>,
    /// Register generation last sent to or read from the system clipboard.
    synced_generation: u64,
    /// Text last sent to or read from the system clipboard.
    synced_text: Option<String>,
    copying: Option<(mpsc::Receiver<bool>, String)>,
    queued: Option<String>,
    reading: Option<mpsc::Receiver<Option<String>>>,
    pastes: VecDeque<(CommandInvocation, EditorWindowVersion)>,
    paste_ready: bool,
}

impl ClipboardSession {
    /// Turn sync on for a real terminal session. Tests never call this, so they never touch
    /// the developer's clipboard.
    pub(super) fn enable(&mut self, registers: &RegisterStore) {
        self.enabled = true;
        self.tool = Tool::detect();
        self.synced_generation = registers.generation();
    }

    /// Send a changed unnamed register to the system clipboard. Returns the text when it must
    /// go through the terminal (OSC 52) because no clipboard program exists.
    pub(super) fn push(&mut self, registers: &mut RegisterStore) -> Option<String> {
        if !self.enabled {
            return None;
        }
        if registers.generation() != self.synced_generation {
            if let Some(value) = registers.get('"').cloned() {
                registers.set('+', value.clone(), true, false);
                let pending = self.copying.as_ref().map(|(_, text)| text.as_str());
                self.queued = if pending == Some(value.text.as_str())
                    || (pending.is_none()
                        && self.synced_text.as_deref() == Some(value.text.as_str()))
                {
                    None
                } else {
                    Some(value.text)
                };
            }
            self.synced_generation = registers.generation();
        }
        let mut fallback = None;
        if let Some((receiver, _)) = &self.copying
            && let Ok(success) = receiver.try_recv()
        {
            let (_, text) = self.copying.take().expect("completed copy");
            if success {
                self.synced_text = Some(text);
            } else if self.queued.is_none() {
                fallback = Some(text);
            }
        }
        if self.copying.is_none()
            && let Some(text) = self.queued.take()
        {
            if let Some(tool) = self.tool {
                let (sender, receiver) = mpsc::channel();
                let copied = text.clone();
                thread::spawn(move || {
                    let _ = sender.send(copy(tool, &copied));
                });
                self.copying = Some((receiver, text));
            } else {
                self.synced_text = Some(text.clone());
                fallback = Some(text);
            }
        }
        fallback
    }

    pub(super) fn pull(
        &mut self,
        invocation: &CommandInvocation,
        source: EditorWindowVersion,
    ) -> bool {
        if std::mem::take(&mut self.paste_ready) {
            return false;
        }
        if !self.enabled || self.tool.is_none() {
            return false;
        }
        self.pastes.push_back((invocation.clone(), source));
        if self.reading.is_none() {
            self.start_read();
        }
        true
    }

    fn start_read(&mut self) {
        let Some(tool) = self.tool.filter(|_| self.enabled) else { return };
        let Some((invocation, _)) = self.pastes.front() else { return };
        let primary = invocation.register == Some('*');
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(read(tool, primary));
        });
        self.reading = Some(receiver);
    }

    pub(super) fn finish_pull(
        &mut self,
        registers: &mut RegisterStore,
        enabled: bool,
        current: &EditorWindowVersion,
    ) -> Option<CommandInvocation> {
        let text = self.reading.as_ref()?.try_recv().ok()?;
        self.reading = None;
        let (invocation, source) = self.pastes.pop_front()?;
        if let Some(text) = text {
            let primary = invocation.register == Some('*');
            self.import(registers, enabled, text, primary);
        }
        if &source == current {
            self.paste_ready = true;
            return Some(invocation);
        }
        self.start_read();
        None
    }

    pub(super) fn continue_pastes(
        &mut self,
        before: &EditorWindowVersion,
        after: &EditorWindowVersion,
    ) {
        for (_, source) in &mut self.pastes {
            if source == before {
                *source = after.clone();
            }
        }
        self.start_read();
    }

    fn import(&mut self, registers: &mut RegisterStore, enabled: bool, text: String, primary: bool) {
        let cached = registers.get(if primary { '*' } else { '"' }).filter(|value| value.text == text);
        let shape = cached.map_or_else(
            || {
                if text.ends_with('\n') {
                    RegisterShape::Line
                } else {
                    RegisterShape::Character
                }
            },
            |value| value.shape,
        );
        let value = RegisterValue {
            text: text.clone(),
            shape,
        };
        if primary {
            registers.set('*', value, enabled, false);
        } else {
            registers.set('+', value.clone(), enabled, false);
            registers.set('"', value, enabled, false);
            self.synced_text = Some(text);
        }
        self.synced_generation = registers.generation();
    }
}

fn copy(tool: Tool, text: &str) -> bool {
    let (program, arguments) = tool.copy_command();
    let Ok(mut child) = Command::new(program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let stdin = child.stdin.take();
    let text = text.to_owned();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ =
            sender.send(stdin.is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok()));
    });
    let success = wait_child(&mut child, Instant::now() + READ_TIMEOUT);
    success && receiver.try_recv().unwrap_or(false)
}

fn wait_child(child: &mut std::process::Child, deadline: Instant) -> bool {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn read(tool: Tool, primary: bool) -> Option<String> {
    let (program, arguments) = tool.paste_command(primary);
    read_command(program, arguments)
}

fn read_command(program: &str, arguments: &[&str]) -> Option<String> {
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout
            .by_ref()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    let Ok(bytes) = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    if !wait_child(&mut child, deadline) {
        return None;
    }
    if bytes.len() > 1024 * 1024 {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_reads_yield_until_the_worker_replies() {
        let state = crate::editor::EditorState::new("/workspace".into());
        let version = state.window_version();
        let invocation =
            CommandInvocation::new("edit.put", crate::editor::InvocationSource::Test).unwrap();
        let (sender, receiver) = mpsc::channel();
        let mut session = ClipboardSession {
            reading: Some(receiver),
            pastes: VecDeque::from([(invocation.clone(), version.clone())]),
            ..Default::default()
        };
        let mut registers = RegisterStore::default();
        assert!(
            session
                .finish_pull(&mut registers, true, &version)
                .is_none()
        );
        sender.send(Some("text".into())).unwrap();
        assert_eq!(
            session.finish_pull(&mut registers, true, &version).unwrap(),
            invocation
        );
        assert_eq!(registers.get('+').unwrap().text, "text");
    }

    #[test]
    fn primary_selection_reads_populate_the_requested_star_register() {
        for tool in [Tool::Wayland, Tool::Xclip, Tool::Xsel] {
            assert_ne!(tool.paste_command(true), tool.paste_command(false));
        }
        assert_eq!(Tool::Wayland.paste_command(true).1, &["--primary", "--no-newline"]);
        assert_eq!(Tool::Xclip.paste_command(true).1, &["-o", "-selection", "primary"]);
        assert_eq!(Tool::Xsel.paste_command(true).1, &["-o", "-p"]);
        let state = crate::editor::EditorState::new("/workspace".into());
        let version = state.window_version();
        let mut invocation = CommandInvocation::new("edit.put", crate::editor::InvocationSource::Test).unwrap();
        invocation.register = Some('*');
        let (sender, receiver) = mpsc::channel();
        let mut session = ClipboardSession {
            reading: Some(receiver),
            pastes: VecDeque::from([(invocation.clone(), version.clone())]),
            ..Default::default()
        };
        let mut registers = RegisterStore::default();
        session.import(&mut registers, true, "clipboard".into(), false);
        sender.send(Some("primary".into())).unwrap();
        assert_eq!(session.finish_pull(&mut registers, true, &version), Some(invocation));
        assert_eq!(registers.get('*').unwrap().text, "primary");
        assert_eq!(registers.get('+').unwrap().text, "clipboard");
        assert_eq!(registers.get('"').unwrap().text, "clipboard");
    }

    #[test]
    fn cache_hits_populate_plus_and_empty_reads_clear_old_text() {
        let mut session = ClipboardSession::default();
        let mut registers = RegisterStore::default();
        registers.set(
            '"',
            RegisterValue {
                text: "line\n".into(),
                shape: RegisterShape::Line,
            },
            true,
            false,
        );
        session.import(&mut registers, true, "line\n".into(), false);
        assert_eq!(registers.get('+'), registers.get('"'));
        assert_eq!(registers.get('+').unwrap().shape, RegisterShape::Line);
        session.import(&mut registers, true, String::new(), false);
        assert_eq!(registers.get('+').unwrap().text, "");
        assert_eq!(registers.get('"').unwrap().text, "");
    }

    #[test]
    fn closed_stdout_does_not_bypass_the_process_deadline() {
        let start = Instant::now();
        assert!(read_command("sh", &["-c", "exec 1>&-; sleep 5"]).is_none());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn copy_queue_keeps_the_latest_yank_and_retries_failures() {
        let mut session = ClipboardSession {
            enabled: true,
            ..Default::default()
        };
        let (sender, receiver) = mpsc::channel();
        session.copying = Some((receiver, "A".into()));
        let mut registers = RegisterStore::default();
        for text in ["B", "C"] {
            registers.set(
                '"',
                RegisterValue {
                    text: text.into(),
                    shape: RegisterShape::Character,
                },
                true,
                false,
            );
            assert_eq!(session.push(&mut registers), None);
        }
        assert_eq!(session.queued.as_deref(), Some("C"));
        sender.send(false).unwrap();
        assert_eq!(session.push(&mut registers).as_deref(), Some("C"));
        assert_eq!(session.synced_text.as_deref(), Some("C"));

        let (sender, receiver) = mpsc::channel();
        session.copying = Some((receiver, "D".into()));
        registers.set(
            '"',
            RegisterValue {
                text: "C".into(),
                shape: RegisterShape::Character,
            },
            true,
            false,
        );
        assert_eq!(session.push(&mut registers), None);
        assert_eq!(session.queued.as_deref(), Some("C"));
        sender.send(true).unwrap();
        assert_eq!(session.push(&mut registers).as_deref(), Some("C"));
    }
}
