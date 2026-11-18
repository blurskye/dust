//! Named editing modes and their key handling policy.

use crate::editor::CommandInvocation;
use crate::editor::CommandValue;
use crate::editor::EditorMode;
use crate::editor::input::KeyEvent;
use crate::editor::input::Keymap;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModeCursor {
    Default,
    Block,
    Bar,
    Underline,
    BlinkingBlock,
    BlinkingBar,
    BlinkingUnderline,
}

#[derive(Clone, Debug)]
pub enum ModeKeys {
    Base(EditorMode),
    /// Unmapped keys invoke this command with their canonical notation in `key`.
    Capture(CommandInvocation),
}

#[derive(Clone, Debug)]
pub struct NamedMode {
    name: String,
    label: String,
    cursor: ModeCursor,
    layer: Keymap,
    keys: ModeKeys,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ModeError {
    #[error("mode name must contain only letters, digits, dots, underscores or hyphens")]
    InvalidName,
    #[error("mode label must be nonempty and contain no controls")]
    InvalidLabel,
}

impl NamedMode {
    pub fn new(
        name: String,
        label: String,
        cursor: ModeCursor,
        layer: Keymap,
        keys: ModeKeys,
    ) -> Result<Self, ModeError> {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ModeError::InvalidName);
        }
        if label.trim().is_empty() || label.len() > 256 || label.chars().any(char::is_control) {
            return Err(ModeError::InvalidLabel);
        }
        Ok(Self {
            name,
            label,
            cursor,
            layer,
            keys,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn cursor(&self) -> ModeCursor {
        self.cursor
    }

    pub fn layer(&self) -> &Keymap {
        &self.layer
    }

    pub fn base(&self) -> Option<EditorMode> {
        match self.keys {
            ModeKeys::Base(mode) => Some(mode),
            ModeKeys::Capture(_) => None,
        }
    }

    pub fn capture(&self, key: &KeyEvent) -> Option<CommandInvocation> {
        match &self.keys {
            ModeKeys::Base(_) => None,
            ModeKeys::Capture(command) => Some(
                command
                    .clone()
                    .with_argument("key", CommandValue::String(key.to_string())),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ModeCursor, ModeError, ModeKeys, NamedMode};
    use crate::editor::input::{KeyEvent, Keymap};
    use crate::editor::{CommandInvocation, CommandValue, EditorMode, InvocationSource};

    #[test]
    fn named_mode_keeps_its_label_cursor_layer_and_base_policy() {
        let mut layer = Keymap::default();
        let command = CommandInvocation::new("view.cursor.move", InvocationSource::Keymap).unwrap();
        layer.insert(&[KeyEvent::character('n')], command);
        let mode = NamedMode::new(
            "review".into(),
            "REVIEW".into(),
            ModeCursor::Underline,
            layer,
            ModeKeys::Base(EditorMode::Normal),
        )
        .unwrap();
        assert_eq!(mode.name(), "review");
        assert_eq!(mode.label(), "REVIEW");
        assert_eq!(mode.cursor(), ModeCursor::Underline);
        assert_eq!(mode.base(), Some(EditorMode::Normal));
        assert!(mode.capture(&KeyEvent::character('x')).is_none());
    }

    #[test]
    fn capture_preserves_non_text_keys_and_modifiers() {
        let command = CommandInvocation::new("plugin.demo.key", InvocationSource::Keymap).unwrap();
        let mode = NamedMode::new(
            "demo".into(),
            "DEMO".into(),
            ModeCursor::Bar,
            Keymap::default(),
            ModeKeys::Capture(command),
        )
        .unwrap();
        assert_eq!(mode.base(), None);
        let key = "ctrl-left".parse().unwrap();
        let command = mode.capture(&key).unwrap();
        assert_eq!(
            command.arguments["key"],
            CommandValue::String("ctrl-left".into())
        );
    }

    #[test]
    fn mode_metadata_rejects_controls_and_empty_names() {
        assert_eq!(
            NamedMode::new(
                "".into(),
                "label".into(),
                ModeCursor::Block,
                Keymap::default(),
                ModeKeys::Base(EditorMode::Normal)
            )
            .unwrap_err(),
            ModeError::InvalidName
        );
        assert_eq!(
            NamedMode::new(
                "demo".into(),
                "\x1b[0m".into(),
                ModeCursor::Block,
                Keymap::default(),
                ModeKeys::Base(EditorMode::Normal)
            )
            .unwrap_err(),
            ModeError::InvalidLabel
        );
    }
}
