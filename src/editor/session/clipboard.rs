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
    time::Duration,
};

use crate::editor::{RegisterShape, RegisterStore, RegisterValue};

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

    fn paste_command(self) -> (&'static str, &'static [&'static str]) {
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
    pub(super) fn push(&mut self, registers: &RegisterStore) -> Option<String> {
        if !self.enabled || registers.generation() == self.synced_generation {
            return None;
        }
        self.synced_generation = registers.generation();
        let text = registers.get('"')?.text.clone();
        if self.synced_text.as_deref() == Some(text.as_str()) {
            return None;
        }
        self.synced_text = Some(text.clone());
        let Some(tool) = self.tool else {
            return Some(text);
        };
        let (program, arguments) = tool.copy_command();
        let child = Command::new(program)
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            // The copy programs fork into the background; write and reap off the UI thread.
            thread::spawn(move || {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
            });
        }
        None
    }

    /// Before a paste, take what another program copied into the unnamed and `+` registers.
    pub(super) fn pull(&mut self, registers: &mut RegisterStore, enabled: bool) {
        let Some(tool) = self.tool.filter(|_| self.enabled) else {
            return;
        };
        let Some(text) = read(tool) else {
            return;
        };
        if text.is_empty()
            || self.synced_text.as_deref() == Some(text.as_str())
            || registers.get('"').is_some_and(|value| value.text == text)
        {
            return;
        }
        // Text copied elsewhere carries no shape; a trailing newline means whole lines, as in
        // Neovim.
        let shape = if text.ends_with('\n') {
            RegisterShape::Line
        } else {
            RegisterShape::Character
        };
        let value = RegisterValue {
            text: text.clone(),
            shape,
        };
        registers.set('+', value.clone(), enabled, false);
        registers.set('"', value, enabled, false);
        self.synced_text = Some(text);
        self.synced_generation = registers.generation();
    }
}

fn read(tool: Tool) -> Option<String> {
    let (program, arguments) = tool.paste_command();
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
        let _ = stdout.read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    let Ok(bytes) = receiver.recv_timeout(READ_TIMEOUT) else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    let status = child.wait().ok()?;
    if !status.success() {
        return None;
    }
    String::from_utf8(bytes).ok()
}
