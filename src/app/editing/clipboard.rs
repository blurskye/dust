//! System clipboard sync for `clipboard = "unnamedplus"`, as LazyVim sets it: yanks reach the
//! system clipboard and `p` pastes what another program copied.
//!
//! It uses wl-copy/wl-paste, xclip or xsel like Neovim's clipboard provider. Without any of
//! them, yanks are sent to the terminal with OSC 52 and pastes use the internal register.

use std::io::Read;

use std::io::Write;

use std::process::Command;

use std::process::Stdio;

use std::sync::mpsc;

use std::thread;

use std::time::Duration;

use std::time::Instant;

use crate::editor::CommandInvocation;

use crate::editor::EditorWindowVersion;

use crate::editor::RegisterShape;

use crate::editor::RegisterStore;

use crate::editor::RegisterValue;
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
        match (self, primary) {
            (Self::Wayland, false) => ("wl-paste", &["--no-newline"]),
            (Self::Wayland, true) => ("wl-paste", &["--no-newline", "--primary"]),
            (Self::Xclip, false) => ("xclip", &["-o", "-selection", "clipboard"]),
            (Self::Xclip, true) => ("xclip", &["-o", "-selection", "primary"]),
            (Self::Xsel, false) => ("xsel", &["-o", "-b"]),
            (Self::Xsel, true) => ("xsel", &["-o", "-p"]),
        }
    }
}

#[derive(Debug)]
pub(in crate::app) enum DeferredInput {
    Event(crate::ui::UiEvent),
    Invocation(CommandInvocation),
    MacroReplay(char, u64),
}

#[derive(Debug, Default)]
pub(in crate::app) struct Clipboard {
    enabled: bool,
    tool: Option<Tool>,
    /// Register revision last queued for copying or imported from a read.
    synced_revision: u64,
    copying: Option<(mpsc::Receiver<bool>, String)>,
    queued: Option<String>,
    reading: Option<mpsc::Receiver<Option<String>>>,
    paste: Option<(CommandInvocation, EditorWindowVersion, u64)>,
    deferred: VecDeque<DeferredInput>,
    paste_ready: bool,
}

impl crate::app::App {
    pub(in crate::app) fn process_clipboard(&mut self) -> Option<String> {
        self.process_clipboard_paste();
        let revision = self.state.registers.revision();
        let output = if self.clipboard_shared() {
            self.clipboard.push(&mut self.state.value.registers)
        } else {
            None
        };
        if self.state.registers.revision() != revision {
            self.state.revision = self.state.revision.wrapping_add(1);
        }
        output
    }
}

impl Clipboard {
    /// Turn sync on for a real terminal session. Tests never call this, so they never touch
    /// the developer's clipboard.
    pub(in crate::app) fn enable(&mut self, registers: &RegisterStore) {
        self.enabled = true;
        self.tool = Tool::detect();
        self.synced_revision = registers.revision();
    }

    /// Send a changed unnamed register to the system clipboard. Returns the text when it must
    /// go through the terminal (OSC 52) because no clipboard program exists.
    pub(in crate::app) fn push(&mut self, registers: &mut RegisterStore) -> Option<String> {
        if !self.enabled {
            return None;
        }
        self.queue_copy(registers);
        let mut fallback = None;
        if let Some((receiver, _)) = &self.copying
            && let Ok(success) = receiver.try_recv()
        {
            let (_, text) = self.copying.take().expect("completed copy");
            if !success && self.queued.is_none() {
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
                fallback = Some(text);
            }
        }
        fallback
    }

    fn queue_copy(&mut self, registers: &mut RegisterStore) {
        if registers.revision() != self.synced_revision {
            if let Some(value) = registers.get('"').cloned() {
                registers.set('+', value.clone(), true, false);
                // A new yank must replace any clipboard changes made by other applications,
                // even when its text matches our last copy.
                self.queued = Some(value.text);
            }
            self.synced_revision = registers.revision();
        }
    }

    pub(in crate::app) fn waiting(&self) -> bool {
        self.paste.is_some()
    }

    pub(in crate::app) fn defer(&mut self, input: DeferredInput) {
        self.deferred.push_back(input);
    }

    pub(in crate::app) fn take_deferred(&mut self) -> VecDeque<DeferredInput> {
        std::mem::take(&mut self.deferred)
    }

    pub(in crate::app) fn append_deferred(&mut self, mut inputs: VecDeque<DeferredInput>) {
        self.deferred.append(&mut inputs);
    }

    pub(in crate::app) fn next_input(&mut self) -> Option<DeferredInput> {
        if self.waiting() {
            None
        } else {
            self.deferred.pop_front()
        }
    }

    pub(in crate::app) fn pull(
        &mut self,
        invocation: &CommandInvocation,
        source: EditorWindowVersion,
        registers: &mut RegisterStore,
        shared: bool,
    ) -> bool {
        if std::mem::take(&mut self.paste_ready) {
            return false;
        }
        let Some(tool) = self.tool.filter(|_| self.enabled) else {
            return false;
        };
        if shared {
            self.queue_copy(registers);
        }
        let primary = invocation.register == Some('*');
        let pending_copy = (!primary)
            .then(|| {
                self.queued
                    .as_ref()
                    .or_else(|| self.copying.as_ref().map(|(_, text)| text))
                    .cloned()
            })
            .flatten();
        if self.reading.is_none() {
            let (sender, receiver) = mpsc::channel();
            if let Some(text) = pending_copy {
                // A paste immediately after a yank must not race the clipboard writer.
                let _ = sender.send(Some(text));
            } else {
                thread::spawn(move || {
                    let _ = sender.send(read(tool, primary));
                });
            }
            self.reading = Some(receiver);
        }
        self.paste = Some((invocation.clone(), source, registers.revision()));
        true
    }

    pub(in crate::app) fn finish_pull(
        &mut self,
        registers: &mut RegisterStore,
        enabled: bool,
        current: &EditorWindowVersion,
    ) -> Result<Option<CommandInvocation>, &'static str> {
        let Some(receiver) = &self.reading else {
            return Ok(None);
        };
        let text = match receiver.try_recv() {
            Ok(text) => text,
            Err(mpsc::TryRecvError::Empty) => return Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.reading = None;
        let (invocation, source, revision) = self.paste.take().expect("pending clipboard paste");
        if &source != current || revision != registers.revision() {
            return Ok(None);
        }
        let text = text.ok_or("Could not read the system clipboard; paste cancelled")?;
        let register = if invocation.register == Some('*') {
            '*'
        } else {
            '+'
        };
        self.import(registers, enabled, register, text);
        self.paste_ready = true;
        Ok(Some(invocation))
    }

    fn import(
        &mut self,
        registers: &mut RegisterStore,
        enabled: bool,
        register: char,
        text: String,
    ) {
        let cached = registers
            .get(register)
            .filter(|value| value.text == text)
            .or_else(|| registers.get('"').filter(|value| value.text == text));
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
        let value = RegisterValue { text, shape };
        registers.set(register, value.clone(), enabled, false);
        if register != '*' {
            registers.set('"', value, enabled, false);
            self.synced_revision = registers.revision();
        }
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
impl Clipboard {
    pub(in crate::app) fn fixture_read(&mut self) -> mpsc::Sender<Option<String>> {
        let (sender, receiver) = mpsc::channel();
        self.enabled = true;
        self.tool = Some(Tool::Wayland);
        self.reading = Some(receiver);
        sender
    }
}

#[cfg(test)]
mod tests {
    use crate::app::editing::clipboard::*;

    impl Clipboard {
        pub(in crate::app) fn pending_copy_fixture(revision: u64, text: &str) -> Self {
            let (_, receiver) = mpsc::channel();
            Self {
                enabled: true,
                tool: Some(Tool::Xclip),
                synced_revision: revision,
                copying: Some((receiver, text.into())),
                ..Default::default()
            }
        }

        pub(in crate::app) fn pending_read_fixture(
            invocation: CommandInvocation,
            source: EditorWindowVersion,
            revision: u64,
        ) -> (Self, mpsc::Sender<Option<String>>) {
            let (sender, receiver) = mpsc::channel();
            (
                Self {
                    enabled: true,
                    tool: Some(Tool::Xclip),
                    synced_revision: revision,
                    reading: Some(receiver),
                    paste: Some((invocation, source, revision)),
                    ..Default::default()
                },
                sender,
            )
        }
    }

    #[test]
    fn paste_reads_yield_until_the_worker_replies() {
        let state = crate::editor::EditorState::new("/workspace".into());
        let version = state.window_version();
        let invocation =
            CommandInvocation::new("edit.put", crate::editor::InvocationSource::Test).unwrap();
        let (sender, receiver) = mpsc::channel();
        let mut session = Clipboard {
            reading: Some(receiver),
            paste: Some((invocation.clone(), version.clone(), 0)),
            ..Default::default()
        };
        let mut registers = RegisterStore::default();
        assert!(
            session
                .finish_pull(&mut registers, true, &version)
                .unwrap()
                .is_none()
        );
        sender.send(Some("text".into())).unwrap();
        assert_eq!(
            session
                .finish_pull(&mut registers, true, &version)
                .unwrap()
                .unwrap(),
            invocation
        );
        assert_eq!(registers.get('+').unwrap().text, "text");
    }

    #[test]
    fn primary_selection_reads_populate_the_requested_star_register() {
        for tool in [Tool::Wayland, Tool::Xclip, Tool::Xsel] {
            assert_ne!(tool.paste_command(true), tool.paste_command(false));
        }
        assert_eq!(
            Tool::Wayland.paste_command(true).1,
            &["--no-newline", "--primary"]
        );
        assert_eq!(
            Tool::Xclip.paste_command(true).1,
            &["-o", "-selection", "primary"]
        );
        assert_eq!(Tool::Xsel.paste_command(true).1, &["-o", "-p"]);
        let state = crate::editor::EditorState::new("/workspace".into());
        let version = state.window_version();
        let mut invocation =
            CommandInvocation::new("edit.put", crate::editor::InvocationSource::Test).unwrap();
        invocation.register = Some('*');
        let (sender, receiver) = mpsc::channel();
        let mut session = Clipboard {
            reading: Some(receiver),
            paste: None,
            ..Default::default()
        };
        let mut registers = RegisterStore::default();
        session.import(&mut registers, true, '+', "clipboard".into());
        session.paste = Some((invocation.clone(), version.clone(), registers.revision()));
        sender.send(Some("primary".into())).unwrap();
        assert_eq!(
            session.finish_pull(&mut registers, true, &version).unwrap(),
            Some(invocation)
        );
        assert_eq!(registers.get('*').unwrap().text, "primary");
        assert_eq!(registers.get('+').unwrap().text, "clipboard");
        assert_eq!(registers.get('"').unwrap().text, "clipboard");
    }

    #[test]
    fn primary_reads_do_not_suppress_a_pending_clipboard_yank() {
        let mut session = Clipboard {
            enabled: true,
            ..Default::default()
        };
        let mut registers = RegisterStore::default();
        registers.set(
            '"',
            RegisterValue {
                text: "new yank".into(),
                shape: RegisterShape::Character,
            },
            true,
            false,
        );
        session.import(&mut registers, true, '*', "primary".into());
        assert_eq!(session.push(&mut registers).as_deref(), Some("new yank"));
        assert_eq!(registers.get('+').unwrap().text, "new yank");
    }

    #[test]
    fn clipboard_read_failures_do_not_paste_stale_registers() {
        let state = crate::editor::EditorState::new("/workspace".into());
        let version = state.window_version();
        let mut registers = RegisterStore::default();
        registers.set('"', value("old"), true, false);
        let invocation =
            CommandInvocation::new("edit.put", crate::editor::InvocationSource::Test).unwrap();
        let (mut session, sender) =
            Clipboard::pending_read_fixture(invocation, version.clone(), registers.revision());
        sender.send(None).unwrap();
        assert!(session.finish_pull(&mut registers, true, &version).is_err());
        assert_eq!(registers.get('"').unwrap().text, "old");
        assert!(!session.waiting());
        assert!(!session.paste_ready);
    }

    #[test]
    fn clipboard_read_worker_disconnect_is_a_failure() {
        let state = crate::editor::EditorState::new("/workspace".into());
        let version = state.window_version();
        let mut registers = RegisterStore::default();
        let invocation =
            CommandInvocation::new("edit.put", crate::editor::InvocationSource::Test).unwrap();
        let (mut session, sender) =
            Clipboard::pending_read_fixture(invocation, version.clone(), registers.revision());
        drop(sender);
        assert!(session.finish_pull(&mut registers, true, &version).is_err());
        assert!(!session.waiting());
    }

    #[test]
    fn clipboard_reads_do_not_overwrite_new_yanks_or_import_from_stale_sources() {
        for changed_register in [false, true] {
            let state = crate::editor::EditorState::new("/workspace".into());
            let version = state.window_version();
            let mut registers = RegisterStore::default();
            registers.set('"', value("before"), true, false);
            let invocation =
                CommandInvocation::new("edit.put", crate::editor::InvocationSource::Test).unwrap();
            let (mut session, sender) =
                Clipboard::pending_read_fixture(invocation, version.clone(), registers.revision());
            let current = if changed_register {
                registers.set('"', value("new yank"), true, false);
                version
            } else {
                crate::editor::EditorState::new("/workspace".into()).window_version()
            };
            let expected = registers.get('"').cloned();
            sender.send(Some("stale read".into())).unwrap();
            assert!(
                session
                    .finish_pull(&mut registers, true, &current)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(registers.get('"'), expected.as_ref());
            assert!(registers.get('+').is_none());
            assert!(!session.paste_ready);
        }
    }

    pub(in crate::app) fn value(text: &str) -> RegisterValue {
        RegisterValue {
            text: text.into(),
            shape: RegisterShape::Character,
        }
    }

    #[test]
    fn clipboard_pastes_use_the_latest_pending_copy_without_reading_the_provider() {
        for queued in [false, true] {
            let state = crate::editor::EditorState::new("/workspace".into());
            let version = state.window_version();
            let mut registers = RegisterStore::default();
            let (_sender, receiver) = mpsc::channel();
            let mut session = Clipboard {
                enabled: true,
                tool: Some(Tool::Xclip),
                copying: Some((receiver, "in flight".into())),
                ..Default::default()
            };
            if queued {
                registers.set('"', value("latest yank"), true, false);
            }
            let invocation =
                CommandInvocation::new("edit.put", crate::editor::InvocationSource::Test).unwrap();
            assert!(session.pull(&invocation, version.clone(), &mut registers, true));
            assert_eq!(
                session.finish_pull(&mut registers, true, &version).unwrap(),
                Some(invocation)
            );
            assert_eq!(
                registers.get('"').unwrap().text,
                if queued { "latest yank" } else { "in flight" }
            );
            assert_eq!(session.queued.as_deref(), queued.then_some("latest yank"));
            assert!(
                session.copying.is_some(),
                "the paste must not discard the pending write"
            );
        }
    }

    #[test]
    fn primary_selection_reads_populate_star_without_replacing_plus() {
        for tool in [Tool::Wayland, Tool::Xclip, Tool::Xsel] {
            assert_ne!(tool.paste_command(true), tool.paste_command(false));
            assert!(tool.paste_command(true).1.contains(&if tool == Tool::Xsel {
                "-p"
            } else if tool == Tool::Xclip {
                "primary"
            } else {
                "--primary"
            }));
        }
        let state = crate::editor::EditorState::new("/workspace".into());
        let version = state.window_version();
        let mut registers = RegisterStore::default();
        registers.set('+', value("clipboard"), true, false);
        let mut invocation =
            CommandInvocation::new("edit.put", crate::editor::InvocationSource::Test).unwrap();
        invocation.register = Some('*');
        let (mut session, sender) = Clipboard::pending_read_fixture(
            invocation.clone(),
            version.clone(),
            registers.revision(),
        );
        sender.send(Some("primary".into())).unwrap();
        assert_eq!(
            session.finish_pull(&mut registers, true, &version).unwrap(),
            Some(invocation)
        );
        assert_eq!(registers.get('*').unwrap().text, "primary");
        assert!(
            registers.get('"').is_none(),
            "primary reads leave the unnamed clipboard register alone"
        );
        assert_eq!(registers.get('+').unwrap().text, "clipboard");
    }

    #[test]
    fn cache_hits_populate_plus_and_empty_reads_clear_old_text() {
        let mut session = Clipboard::default();
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
        session.import(&mut registers, true, '+', "line\n".into());
        assert_eq!(registers.get('+'), registers.get('"'));
        assert_eq!(registers.get('+').unwrap().shape, RegisterShape::Line);
        session.import(&mut registers, true, '+', String::new());
        assert_eq!(registers.get('+').unwrap().text, "");
        assert_eq!(registers.get('"').unwrap().text, "");
    }

    #[test]
    fn a_stale_selection_cache_does_not_hide_the_unnamed_register_shape() {
        let mut session = Clipboard::default();
        let mut registers = RegisterStore::default();
        registers.set('+', value("old clipboard"), true, false);
        registers.set(
            '"',
            RegisterValue {
                text: "block".into(),
                shape: RegisterShape::Block,
            },
            true,
            false,
        );
        session.import(&mut registers, true, '+', "block".into());
        assert_eq!(registers.get('+').unwrap().shape, RegisterShape::Block);
    }

    #[test]
    fn closed_stdout_does_not_bypass_the_process_deadline() {
        let start = Instant::now();
        assert!(read_command("sh", &["-c", "exec 1>&-; sleep 5"]).is_none());
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn identical_yanks_each_replace_the_external_clipboard() {
        let mut session = Clipboard {
            enabled: true,
            ..Default::default()
        };
        let mut registers = RegisterStore::default();
        for _ in 0..2 {
            registers.set('"', value("A"), true, false);
            assert_eq!(session.push(&mut registers).as_deref(), Some("A"));
            assert_eq!(
                session.push(&mut registers),
                None,
                "polling alone must not recopy"
            );
        }
        let (sender, receiver) = mpsc::channel();
        session.copying = Some((receiver, "A".into()));
        registers.set('"', value("A"), true, false);
        assert_eq!(session.push(&mut registers), None);
        assert_eq!(session.queued.as_deref(), Some("A"));
        sender.send(true).unwrap();
        assert_eq!(session.push(&mut registers).as_deref(), Some("A"));
    }

    #[test]
    fn copy_queue_keeps_the_latest_yank_and_retries_failures() {
        let mut session = Clipboard {
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
        assert!(session.queued.is_none());

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
