use std::num::NonZeroU64;

use crate::editor::{CommandError, CommandInvocation, CommandValue, EditorMode, InvocationSource};
use thiserror::Error;

pub const ABSOLUTE_MAX_COUNT: u64 = 1_000_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputAction {
    Pending,
    Cancelled,
    Unmapped,
    Text(String),
    Invoke(CommandInvocation),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum PendingInputContext {
    Register,
    Prefix,
    Replace,
    MacroRecord,
    MacroReplay,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GrammarInput {
    Cancel,
    Count(u8),
    Zero,
    RegisterBegin,
    RegisterSelect(String),
    OperatorBegin(String),
    OperatorRepeat(String),
    Motion { motion: String, linewise: bool },
    PrefixBegin,
    ReplaceBegin,
    ReplaceText(String),
    MacroRecordToggle,
    MacroRecordSelect(String),
    MacroReplayBegin,
    MacroReplaySelect(String),
    VisualOperator { operator: String, linewise: bool },
}

#[derive(Clone, Debug)]
pub struct ModalResolver {
    count: Option<u64>,
    operator_count: Option<u64>,
    operator: Option<String>,
    register: Option<char>,
    pending: Option<PendingInputContext>,
    macro_recording: bool,
    count_limit: u64,
}

impl Default for ModalResolver {
    fn default() -> Self {
        Self {
            count: None,
            operator_count: None,
            operator: None,
            register: None,
            pending: None,
            macro_recording: false,
            count_limit: ABSOLUTE_MAX_COUNT,
        }
    }
}

impl ModalResolver {
    #[must_use]
    pub fn with_count_limit(count_limit: u64) -> Self {
        Self {
            count_limit: count_limit.clamp(1, ABSOLUTE_MAX_COUNT),
            ..Self::default()
        }
    }

    pub fn set_count_limit(&mut self, count_limit: u64) {
        self.count_limit = count_limit.clamp(1, ABSOLUTE_MAX_COUNT);
        self.reset_pending();
    }

    pub fn cancel_pending(&mut self) {
        let count_limit = self.count_limit;
        *self = Self::with_count_limit(count_limit);
    }

    #[must_use]
    pub fn effective_mode(&self, primary: EditorMode) -> EditorMode {
        if self.operator.is_some() {
            EditorMode::OperatorPending
        } else {
            primary
        }
    }

    #[must_use]
    pub const fn pending_context(&self) -> Option<PendingInputContext> {
        self.pending
    }

    #[must_use]
    pub const fn macro_recording(&self) -> bool {
        self.macro_recording
    }

    pub fn decorate_invocation(&mut self, invocation: &mut CommandInvocation) {
        if invocation.count.is_none() {
            invocation.count = count_nonzero(self.take_count());
        }
        if invocation.register.is_none() {
            invocation.register = self.register.take();
        }
    }

    pub fn feed(
        &mut self,
        mode: EditorMode,
        input: GrammarInput,
    ) -> Result<InputAction, InputError> {
        match input {
            GrammarInput::Cancel => {
                self.reset_pending();
                if mode == EditorMode::Normal {
                    Ok(InputAction::Cancelled)
                } else {
                    Ok(InputAction::Invoke(command("mode.normal")?))
                }
            }
            GrammarInput::Count(digit) => {
                self.push_count(digit)?;
                Ok(InputAction::Pending)
            }
            GrammarInput::Zero => {
                if self.count.is_some() || self.operator.is_some() {
                    self.push_count(0)?;
                    Ok(InputAction::Pending)
                } else {
                    self.motion("line_start", false)
                }
            }
            GrammarInput::RegisterBegin => {
                self.pending = Some(PendingInputContext::Register);
                Ok(InputAction::Pending)
            }
            GrammarInput::RegisterSelect(value) => {
                let name = one_character(&value)?;
                self.pending = None;
                self.register = Some(name);
                Ok(InputAction::Pending)
            }
            GrammarInput::OperatorBegin(operator) => {
                self.operator = Some(operator);
                self.operator_count = self.count.take();
                Ok(InputAction::Pending)
            }
            GrammarInput::OperatorRepeat(operator) => {
                if self.operator.as_deref() != Some(operator.as_str()) {
                    self.reset_pending();
                    return Ok(InputAction::Cancelled);
                }
                let count = self.combined_count()?;
                let invocation = self.operator_invocation("line", true, count)?;
                self.reset_pending();
                Ok(InputAction::Invoke(invocation))
            }
            GrammarInput::Motion { motion, linewise } => self.motion(&motion, linewise),
            GrammarInput::PrefixBegin => {
                self.pending = Some(PendingInputContext::Prefix);
                Ok(InputAction::Pending)
            }
            GrammarInput::ReplaceBegin => {
                self.pending = Some(PendingInputContext::Replace);
                Ok(InputAction::Pending)
            }
            GrammarInput::ReplaceText(text) => {
                self.pending = None;
                let mut invocation =
                    command("edit.replace")?.with_argument("text", CommandValue::String(text));
                invocation.count = count_nonzero(self.take_count());
                Ok(InputAction::Invoke(invocation))
            }
            GrammarInput::MacroRecordToggle => {
                if self.macro_recording {
                    self.macro_recording = false;
                    Ok(InputAction::Invoke(command("macro.record.stop")?))
                } else {
                    self.pending = Some(PendingInputContext::MacroRecord);
                    Ok(InputAction::Pending)
                }
            }
            GrammarInput::MacroRecordSelect(value) => {
                let name = one_character(&value)?;
                self.pending = None;
                self.macro_recording = true;
                Ok(InputAction::Invoke(
                    command("macro.record.start")?
                        .with_argument("register", CommandValue::String(name.to_string())),
                ))
            }
            GrammarInput::MacroReplayBegin => {
                self.pending = Some(PendingInputContext::MacroReplay);
                Ok(InputAction::Pending)
            }
            GrammarInput::MacroReplaySelect(value) => {
                let name = one_character(&value)?;
                self.pending = None;
                let mut invocation = command("macro.replay")?
                    .with_argument("register", CommandValue::String(name.to_string()));
                invocation.count = count_nonzero(self.take_count());
                Ok(InputAction::Invoke(invocation))
            }
            GrammarInput::VisualOperator { operator, linewise } => {
                let mut invocation = command("edit.operator")?
                    .with_argument("operator", CommandValue::String(operator))
                    .with_argument("motion", CommandValue::String("visual".into()))
                    .with_argument("linewise", CommandValue::Bool(linewise));
                invocation.register = self.register.take();
                self.reset_pending();
                Ok(InputAction::Invoke(invocation))
            }
        }
    }

    fn motion(&mut self, motion: &str, linewise: bool) -> Result<InputAction, InputError> {
        self.pending = None;
        if self.operator.is_some() {
            let count = self.combined_count()?;
            let invocation = self.operator_invocation(motion, linewise, count)?;
            self.reset_pending();
            return Ok(InputAction::Invoke(invocation));
        }
        let mut invocation = command("view.cursor.move")?
            .with_argument("motion", CommandValue::String(motion.into()));
        invocation.count = count_nonzero(self.take_count());
        Ok(InputAction::Invoke(invocation))
    }

    fn operator_invocation(
        &self,
        motion: &str,
        linewise: bool,
        count: u64,
    ) -> Result<CommandInvocation, InputError> {
        let mut invocation = command("edit.operator")?
            .with_argument(
                "operator",
                CommandValue::String(self.operator.as_ref().expect("operator exists").clone()),
            )
            .with_argument("motion", CommandValue::String(motion.into()))
            .with_argument("linewise", CommandValue::Bool(linewise));
        invocation.count = count_nonzero(count);
        invocation.register = self.register;
        Ok(invocation)
    }

    fn push_count(&mut self, digit: u8) -> Result<(), InputError> {
        if digit > 9 {
            return Err(InputError::InvalidDigit(digit));
        }
        let next = self
            .count
            .unwrap_or(0)
            .checked_mul(10)
            .and_then(|value| value.checked_add(u64::from(digit)))
            .filter(|value| *value <= self.count_limit)
            .ok_or(InputError::CountOverflow(self.count_limit))?;
        self.count = Some(next);
        Ok(())
    }

    fn combined_count(&self) -> Result<u64, InputError> {
        self.operator_count
            .unwrap_or(1)
            .checked_mul(self.count.unwrap_or(1))
            .filter(|value| *value <= self.count_limit)
            .ok_or(InputError::CountOverflow(self.count_limit))
    }

    fn take_count(&mut self) -> u64 {
        self.count.take().unwrap_or(1)
    }

    fn reset_pending(&mut self) {
        self.count = None;
        self.operator_count = None;
        self.operator = None;
        self.register = None;
        self.pending = None;
    }
}

fn one_character(value: &str) -> Result<char, InputError> {
    let mut characters = value.chars();
    let character = characters.next().ok_or(InputError::ExpectedCharacter)?;
    if characters.next().is_some() {
        return Err(InputError::ExpectedCharacter);
    }
    Ok(character)
}

fn count_nonzero(value: u64) -> Option<NonZeroU64> {
    NonZeroU64::new(value).filter(|value| value.get() != 1)
}

fn command(id: &str) -> Result<CommandInvocation, CommandError> {
    CommandInvocation::new(id, InvocationSource::Keymap)
}

#[derive(Debug, Error)]
pub enum InputError {
    #[error("count exceeds configured limit {0}")]
    CountOverflow(u64),
    #[error("invalid count digit {0}")]
    InvalidDigit(u8),
    #[error("input requires exactly one character")]
    ExpectedCharacter,
    #[error(transparent)]
    Command(#[from] CommandError),
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use crate::editor::{CommandValue, EditorMode};

    use super::{GrammarInput, InputAction, ModalResolver, PendingInputContext};

    #[test]
    fn configured_count_limit_is_enforced_below_the_absolute_ceiling() {
        let mut resolver = ModalResolver::with_count_limit(2);
        assert!(matches!(
            resolver.feed(EditorMode::Normal, GrammarInput::Count(3)),
            Err(super::InputError::CountOverflow(2))
        ));
    }

    #[test]
    fn combines_operator_and_motion_counts_without_knowing_keys() {
        let mut resolver = ModalResolver::default();
        resolver
            .feed(EditorMode::Normal, GrammarInput::Count(2))
            .expect("count");
        resolver
            .feed(
                EditorMode::Normal,
                GrammarInput::OperatorBegin("delete".into()),
            )
            .expect("operator");
        resolver
            .feed(EditorMode::OperatorPending, GrammarInput::Count(3))
            .expect("motion count");
        let InputAction::Invoke(invocation) = resolver
            .feed(
                EditorMode::OperatorPending,
                GrammarInput::Motion {
                    motion: "word_forward".into(),
                    linewise: false,
                },
            )
            .expect("motion")
        else {
            panic!("expected command")
        };
        assert_eq!(invocation.count, NonZeroU64::new(6));
        assert_eq!(
            invocation.arguments.get("operator"),
            Some(&CommandValue::String("delete".into()))
        );
    }

    #[test]
    fn register_prefix_and_doubled_operator_are_semantic() {
        let mut resolver = ModalResolver::default();
        resolver
            .feed(EditorMode::Normal, GrammarInput::RegisterBegin)
            .expect("register begin");
        assert_eq!(
            resolver.pending_context(),
            Some(PendingInputContext::Register)
        );
        resolver
            .feed(EditorMode::Normal, GrammarInput::RegisterSelect("a".into()))
            .expect("register");
        resolver
            .feed(
                EditorMode::Normal,
                GrammarInput::OperatorBegin("delete".into()),
            )
            .expect("operator");
        let InputAction::Invoke(invocation) = resolver
            .feed(
                EditorMode::OperatorPending,
                GrammarInput::OperatorRepeat("delete".into()),
            )
            .expect("repeat")
        else {
            panic!("expected command")
        };
        assert_eq!(invocation.register, Some('a'));
        assert_eq!(
            invocation.arguments.get("linewise"),
            Some(&CommandValue::Bool(true))
        );
    }

    #[test]
    fn replace_and_macro_pending_contexts_accept_configured_text() {
        let mut resolver = ModalResolver::default();
        resolver
            .feed(EditorMode::Normal, GrammarInput::ReplaceBegin)
            .expect("replace");
        assert_eq!(
            resolver.pending_context(),
            Some(PendingInputContext::Replace)
        );
        let InputAction::Invoke(replace) = resolver
            .feed(EditorMode::Normal, GrammarInput::ReplaceText("é".into()))
            .expect("replacement")
        else {
            panic!("replace command")
        };
        assert_eq!(replace.id.as_str(), "edit.replace");

        resolver
            .feed(EditorMode::Normal, GrammarInput::MacroRecordToggle)
            .expect("record begin");
        assert_eq!(
            resolver.pending_context(),
            Some(PendingInputContext::MacroRecord)
        );
    }
}
