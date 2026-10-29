use std::{fmt, str::FromStr};

use thiserror::Error;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TerminalMouseInput {
    LeftDown,
    LeftUp,
    RightDown,
    RightUp,
    MiddleDown,
    MiddleUp,
    LeftDrag,
    RightDrag,
    MiddleDrag,
    Move,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TerminalNavigationKey {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum KeyCode {
    Char(char),
    Enter,
    Escape,
    Backspace,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Delete,
    FocusGained,
    FocusLost,
    MouseLeftDown,
    MouseLeftUp,
    MouseRightDown,
    MouseRightUp,
    MouseMiddleDown,
    MouseMiddleUp,
    MouseLeftDrag,
    MouseRightDrag,
    MouseMiddleDrag,
    MouseMove,
    MouseScrollUp,
    MouseScrollDown,
    MouseScrollLeft,
    MouseScrollRight,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Modifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct KeyEvent {
    pub code: KeyCode,
    pub modifiers: Modifiers,
}

impl KeyEvent {
    #[must_use]
    pub const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: Modifiers {
                control: false,
                alt: false,
                shift: false,
            },
        }
    }

    #[must_use]
    pub const fn character(character: char) -> Self {
        Self::plain(KeyCode::Char(character))
    }

    #[must_use]
    pub const fn focus_gained() -> Self {
        Self::plain(KeyCode::FocusGained)
    }

    #[must_use]
    pub const fn focus_lost() -> Self {
        Self::plain(KeyCode::FocusLost)
    }

    #[must_use]
    pub const fn mouse(code: KeyCode, modifiers: Modifiers) -> Self {
        Self { code, modifiers }
    }

    #[must_use]
    pub const fn terminal_mouse(&self) -> Option<TerminalMouseInput> {
        match &self.code {
            KeyCode::MouseLeftDown => Some(TerminalMouseInput::LeftDown),
            KeyCode::MouseLeftUp => Some(TerminalMouseInput::LeftUp),
            KeyCode::MouseRightDown => Some(TerminalMouseInput::RightDown),
            KeyCode::MouseRightUp => Some(TerminalMouseInput::RightUp),
            KeyCode::MouseMiddleDown => Some(TerminalMouseInput::MiddleDown),
            KeyCode::MouseMiddleUp => Some(TerminalMouseInput::MiddleUp),
            KeyCode::MouseLeftDrag => Some(TerminalMouseInput::LeftDrag),
            KeyCode::MouseRightDrag => Some(TerminalMouseInput::RightDrag),
            KeyCode::MouseMiddleDrag => Some(TerminalMouseInput::MiddleDrag),
            KeyCode::MouseMove => Some(TerminalMouseInput::Move),
            KeyCode::MouseScrollUp => Some(TerminalMouseInput::ScrollUp),
            KeyCode::MouseScrollDown => Some(TerminalMouseInput::ScrollDown),
            KeyCode::MouseScrollLeft => Some(TerminalMouseInput::ScrollLeft),
            KeyCode::MouseScrollRight => Some(TerminalMouseInput::ScrollRight),
            _ => None,
        }
    }

    pub const fn terminal_navigation(&self) -> Option<TerminalNavigationKey> {
        if self.modifiers.control || self.modifiers.alt || self.modifiers.shift {
            return None;
        }
        match &self.code {
            KeyCode::Up => Some(TerminalNavigationKey::Up),
            KeyCode::Down => Some(TerminalNavigationKey::Down),
            KeyCode::Left => Some(TerminalNavigationKey::Left),
            KeyCode::Right => Some(TerminalNavigationKey::Right),
            KeyCode::Home => Some(TerminalNavigationKey::Home),
            KeyCode::End => Some(TerminalNavigationKey::End),
            _ => None,
        }
    }

    pub const fn text_character(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(character) if !self.modifiers.control && !self.modifiers.alt => {
                Some(character)
            }
            _ => None,
        }
    }
}

impl fmt::Display for KeyEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut prefixes = Vec::new();
        if self.modifiers.control {
            prefixes.push("ctrl");
        }
        if self.modifiers.alt {
            prefixes.push("alt");
        }
        if self.modifiers.shift {
            prefixes.push("shift");
        }
        let key = match self.code {
            KeyCode::Char(' ') => "space".into(),
            KeyCode::Char(character) => character.to_string(),
            KeyCode::Enter => "enter".into(),
            KeyCode::Escape => "escape".into(),
            KeyCode::Backspace => "backspace".into(),
            KeyCode::Tab => "tab".into(),
            KeyCode::Left => "left".into(),
            KeyCode::Right => "right".into(),
            KeyCode::Up => "up".into(),
            KeyCode::Down => "down".into(),
            KeyCode::Home => "home".into(),
            KeyCode::End => "end".into(),
            KeyCode::Delete => "delete".into(),
            KeyCode::FocusGained => "focus-gained".into(),
            KeyCode::FocusLost => "focus-lost".into(),
            KeyCode::MouseLeftDown => "mouse-left-down".into(),
            KeyCode::MouseLeftUp => "mouse-left-up".into(),
            KeyCode::MouseRightDown => "mouse-right-down".into(),
            KeyCode::MouseRightUp => "mouse-right-up".into(),
            KeyCode::MouseMiddleDown => "mouse-middle-down".into(),
            KeyCode::MouseMiddleUp => "mouse-middle-up".into(),
            KeyCode::MouseLeftDrag => "mouse-left-drag".into(),
            KeyCode::MouseRightDrag => "mouse-right-drag".into(),
            KeyCode::MouseMiddleDrag => "mouse-middle-drag".into(),
            KeyCode::MouseMove => "mouse-move".into(),
            KeyCode::MouseScrollUp => "mouse-scroll-up".into(),
            KeyCode::MouseScrollDown => "mouse-scroll-down".into(),
            KeyCode::MouseScrollLeft => "mouse-scroll-left".into(),
            KeyCode::MouseScrollRight => "mouse-scroll-right".into(),
        };
        if prefixes.is_empty() {
            formatter.write_str(&key)
        } else {
            write!(formatter, "{}-{key}", prefixes.join("-"))
        }
    }
}

fn named_hyphenated_key(input: &str) -> Option<KeyCode> {
    match input.to_ascii_lowercase().as_str() {
        "-" => Some(KeyCode::Char('-')),
        "focus-gained" => Some(KeyCode::FocusGained),
        "focus-lost" => Some(KeyCode::FocusLost),
        "mouse-left-down" => Some(KeyCode::MouseLeftDown),
        "mouse-left-up" => Some(KeyCode::MouseLeftUp),
        "mouse-right-down" => Some(KeyCode::MouseRightDown),
        "mouse-right-up" => Some(KeyCode::MouseRightUp),
        "mouse-middle-down" => Some(KeyCode::MouseMiddleDown),
        "mouse-middle-up" => Some(KeyCode::MouseMiddleUp),
        "mouse-left-drag" => Some(KeyCode::MouseLeftDrag),
        "mouse-right-drag" => Some(KeyCode::MouseRightDrag),
        "mouse-middle-drag" => Some(KeyCode::MouseMiddleDrag),
        "mouse-move" => Some(KeyCode::MouseMove),
        "mouse-scroll-up" => Some(KeyCode::MouseScrollUp),
        "mouse-scroll-down" => Some(KeyCode::MouseScrollDown),
        "mouse-scroll-left" => Some(KeyCode::MouseScrollLeft),
        "mouse-scroll-right" => Some(KeyCode::MouseScrollRight),
        _ => None,
    }
}

impl FromStr for KeyEvent {
    type Err = KeyParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.is_empty() {
            return Err(KeyParseError::Empty);
        }
        let mut named_input = input;
        let mut named_modifiers = Modifiers::default();
        loop {
            if let Some(rest) = named_input.strip_prefix("ctrl-") {
                named_modifiers.control = true;
                named_input = rest;
            } else if let Some(rest) = named_input.strip_prefix("alt-") {
                named_modifiers.alt = true;
                named_input = rest;
            } else if let Some(rest) = named_input.strip_prefix("shift-") {
                named_modifiers.shift = true;
                named_input = rest;
            } else {
                break;
            }
        }
        if let Some(code) = named_hyphenated_key(named_input) {
            return Ok(Self {
                code,
                modifiers: named_modifiers,
            });
        }
        let parts: Vec<_> = input.split('-').collect();
        let (key_name, modifier_names) = parts.split_last().ok_or(KeyParseError::Empty)?;
        let mut modifiers = Modifiers::default();
        for modifier in modifier_names {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => modifiers.control = true,
                "alt" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                unknown => return Err(KeyParseError::UnknownModifier(unknown.into())),
            }
        }
        let normalized = key_name.to_ascii_lowercase();
        let code = match normalized.as_str() {
            "space" => KeyCode::Char(' '),
            "enter" | "return" => KeyCode::Enter,
            "escape" | "esc" => KeyCode::Escape,
            "backspace" => KeyCode::Backspace,
            "tab" => KeyCode::Tab,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "delete" => KeyCode::Delete,
            "focus-gained" => KeyCode::FocusGained,
            "focus-lost" => KeyCode::FocusLost,
            _ => {
                let mut characters = key_name.chars();
                let character = characters
                    .next()
                    .ok_or_else(|| KeyParseError::UnknownKey((*key_name).into()))?;
                if characters.next().is_some() {
                    return Err(KeyParseError::UnknownKey((*key_name).into()));
                }
                KeyCode::Char(character)
            }
        };
        Ok(Self { code, modifiers })
    }
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum KeyParseError {
    #[error("key notation is empty")]
    Empty,
    #[error("unknown key modifier `{0}`")]
    UnknownModifier(String),
    #[error("unknown key `{0}`")]
    UnknownKey(String),
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::KeyEvent;

    #[test]
    fn notation_round_trips() {
        for notation in [
            "x",
            "space",
            "ctrl-w",
            "alt-enter",
            "shift-tab",
            "mouse-left-down",
            "ctrl-mouse-scroll-down",
        ] {
            let event = KeyEvent::from_str(notation).expect("valid notation");
            assert_eq!(event.to_string(), notation);
        }
    }
}
