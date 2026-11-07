//! Vim grammar for counts, registers, motions and operators.

use std::num::NonZeroU64;

use crate::editor::CommandError;

use crate::editor::CommandInvocation;

use crate::editor::CommandValue;

use crate::editor::EditorMode;

use crate::editor::InvocationSource;
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
    Character,
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
    Motion {
        motion: String,
        linewise: bool,
    },
    PrefixBegin,
    ReplaceBegin,
    MarkBegin(String),
    ReplaceText(String),
    /// `f`, `F`, `t` or `T`: the motion waits for the character to find.
    FindBegin(String),
    /// `;` repeats the last find, `,` repeats it in the other direction.
    FindRepeat {
        reverse: bool,
    },
    /// `gsd` and `gsr`: wait for the surrounding character to delete or replace.
    SurroundBegin(String),
    Character(String),
    MacroRecordToggle,
    MacroRecordSelect(String),
    MacroReplayBegin,
    MacroReplaySelect(String),
    VisualOperator {
        operator: String,
        linewise: bool,
    },
}

#[derive(Clone, Debug)]
pub struct ModalResolver {
    count: Option<u64>,
    operator_count: Option<u64>,
    operator: Option<String>,
    register: Option<char>,
    pending: Option<PendingInputContext>,
    awaiting: Option<CharacterTarget>,
    last_find: Option<(String, char)>,
    count_limit: u64,
}

/// What a typed character completes while the `character` context is pending.
#[derive(Clone, Debug)]
enum CharacterTarget {
    Find(String),
    SurroundAdd(CommandInvocation),
    SurroundDelete,
    SurroundReplace(Option<char>),
    Mark(String),
}

impl Default for ModalResolver {
    fn default() -> Self {
        Self {
            count: None,
            operator_count: None,
            operator: None,
            register: None,
            pending: None,
            awaiting: None,
            last_find: None,
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
        let last_find = self.last_find.take();
        *self = Self::with_count_limit(count_limit);
        self.last_find = last_find;
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
    pub const fn has_pending_grammar(&self) -> bool {
        self.count.is_some()
            || self.operator_count.is_some()
            || self.operator.is_some()
            || self.register.is_some()
            || self.pending.is_some()
    }

    pub fn decorate_invocation(&mut self, invocation: &mut CommandInvocation) {
        if invocation.count.is_none() {
            invocation.count = self.count.take().and_then(NonZeroU64::new);
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
        let result = self.feed_inner(mode, input);
        if result.is_err() {
            self.reset_pending();
        }
        result
    }

    fn feed_inner(
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
                if self.count.is_some() {
                    self.push_count(0)?;
                    Ok(InputAction::Pending)
                } else {
                    self.motion("line_start", false, None)
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
            GrammarInput::Motion { motion, linewise } => self.motion(&motion, linewise, None),
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
            GrammarInput::MarkBegin(command_id) => {
                self.await_character(CharacterTarget::Mark(command_id));
                Ok(InputAction::Pending)
            }
            GrammarInput::FindBegin(motion) => {
                self.await_character(CharacterTarget::Find(motion));
                Ok(InputAction::Pending)
            }
            GrammarInput::FindRepeat { reverse } => {
                let Some((motion, character)) = self.last_find.clone() else {
                    self.reset_pending();
                    return Ok(InputAction::Cancelled);
                };
                let motion = if reverse {
                    reversed_find(&motion)
                } else {
                    motion.as_str()
                };
                // Repeating `t`/`T` must not stop in front of the character it already reached.
                let motion = match motion {
                    "till_forward" => "till_forward_again",
                    "till_backward" => "till_backward_again",
                    other => other,
                };
                self.motion(motion, false, Some(character))
            }
            GrammarInput::SurroundBegin(action) => {
                self.await_character(match action.as_str() {
                    "replace" => CharacterTarget::SurroundReplace(None),
                    _ => CharacterTarget::SurroundDelete,
                });
                Ok(InputAction::Pending)
            }
            GrammarInput::Character(value) => {
                let character = one_character(&value)?;
                let Some(target) = self.awaiting.take() else {
                    self.reset_pending();
                    return Ok(InputAction::Cancelled);
                };
                self.pending = None;
                match target {
                    CharacterTarget::Find(motion) => {
                        self.last_find = Some((motion.clone(), character));
                        self.motion(&motion, false, Some(character))
                    }
                    CharacterTarget::SurroundAdd(invocation) => {
                        self.reset_pending();
                        Ok(InputAction::Invoke(invocation.with_argument(
                            "character",
                            CommandValue::String(character.to_string()),
                        )))
                    }
                    CharacterTarget::Mark(command_id) => {
                        self.reset_pending();
                        Ok(InputAction::Invoke(command(&command_id)?.with_argument(
                            "name",
                            CommandValue::String(character.to_string()),
                        )))
                    }
                    CharacterTarget::SurroundDelete => {
                        self.reset_pending();
                        Ok(InputAction::Invoke(
                            command("edit.surround")?
                                .with_argument("action", CommandValue::String("delete".into()))
                                .with_argument(
                                    "character",
                                    CommandValue::String(character.to_string()),
                                ),
                        ))
                    }
                    CharacterTarget::SurroundReplace(None) => {
                        self.await_character(CharacterTarget::SurroundReplace(Some(character)));
                        Ok(InputAction::Pending)
                    }
                    CharacterTarget::SurroundReplace(Some(old)) => {
                        self.reset_pending();
                        Ok(InputAction::Invoke(
                            command("edit.surround")?
                                .with_argument("action", CommandValue::String("replace".into()))
                                .with_argument("character", CommandValue::String(old.to_string()))
                                .with_argument(
                                    "replacement",
                                    CommandValue::String(character.to_string()),
                                ),
                        ))
                    }
                }
            }
            GrammarInput::MacroRecordToggle => {
                self.pending = Some(PendingInputContext::MacroRecord);
                Ok(InputAction::Pending)
            }
            GrammarInput::MacroRecordSelect(value) => {
                let name = one_character(&value)?;
                self.pending = None;
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
                let surround = operator == "surround_add";
                let mut invocation = command("edit.operator")?
                    .with_argument("operator", CommandValue::String(operator))
                    .with_argument("motion", CommandValue::String("visual".into()))
                    .with_argument("linewise", CommandValue::Bool(linewise));
                invocation.register = self.register.take();
                invocation.count = count_nonzero(self.take_count());
                self.reset_pending();
                if surround {
                    self.await_character(CharacterTarget::SurroundAdd(invocation));
                    return Ok(InputAction::Pending);
                }
                Ok(InputAction::Invoke(invocation))
            }
        }
    }

    fn motion(
        &mut self,
        motion: &str,
        linewise: bool,
        character: Option<char>,
    ) -> Result<InputAction, InputError> {
        self.pending = None;
        if self.operator.is_some() {
            let count = self.combined_count()?;
            let mut invocation = self.operator_invocation(motion, linewise, count)?;
            // A typed 1 still counts: `d1G` deletes to line 1, not to the end.
            if self.count.is_some() || self.operator_count.is_some() {
                invocation.count = NonZeroU64::new(count);
            }
            let surround = self.operator.as_deref() == Some("surround_add");
            if let Some(character) = character {
                invocation = invocation.with_argument(
                    if surround {
                        "find_character"
                    } else {
                        "character"
                    },
                    CommandValue::String(character.to_string()),
                );
            }
            self.reset_pending();
            if surround {
                self.await_character(CharacterTarget::SurroundAdd(invocation));
                return Ok(InputAction::Pending);
            }
            return Ok(InputAction::Invoke(invocation));
        }
        let mut invocation = command("view.cursor.move")?
            .with_argument("motion", CommandValue::String(motion.into()));
        if let Some(character) = character {
            invocation =
                invocation.with_argument("character", CommandValue::String(character.to_string()));
        }
        // A typed 1 still counts: `1G` goes to line 1, not to the end.
        invocation.count = self.count.take().and_then(NonZeroU64::new);
        Ok(InputAction::Invoke(invocation))
    }

    fn await_character(&mut self, target: CharacterTarget) {
        self.awaiting = Some(target);
        self.pending = Some(PendingInputContext::Character);
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
        self.awaiting = None;
    }
}

fn reversed_find(motion: &str) -> &str {
    match motion {
        "find_forward" => "find_backward",
        "find_backward" => "find_forward",
        "till_forward" => "till_backward",
        "till_backward" => "till_forward",
        other => other,
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

    use crate::editor::CommandValue;

    use crate::editor::EditorMode;

    use crate::editor::input::grammar::GrammarInput;

    use crate::editor::input::grammar::InputAction;

    use crate::editor::input::grammar::ModalResolver;

    use crate::editor::input::grammar::PendingInputContext;

    #[test]
    fn configured_count_limit_is_enforced_below_the_absolute_limit() {
        let mut resolver = ModalResolver::with_count_limit(2);
        assert!(matches!(
            resolver.feed(EditorMode::Normal, GrammarInput::Count(3)),
            Err(crate::editor::input::grammar::InputError::CountOverflow(2))
        ));
    }

    #[test]
    fn a_typed_count_of_one_is_not_the_same_as_no_count() {
        let motion = || GrammarInput::Motion {
            motion: "document_end".into(),
            linewise: false,
        };
        let count = |resolver: &mut ModalResolver, mode| {
            let InputAction::Invoke(invocation) = resolver.feed(mode, motion()).expect("motion")
            else {
                panic!("expected command")
            };
            invocation.count
        };
        let mut resolver = ModalResolver::default();
        assert_eq!(count(&mut resolver, EditorMode::Normal), None);
        resolver
            .feed(EditorMode::Normal, GrammarInput::Count(1))
            .expect("count");
        assert_eq!(count(&mut resolver, EditorMode::Normal), NonZeroU64::new(1));
        for typed_first in [false, true] {
            if typed_first {
                resolver
                    .feed(EditorMode::Normal, GrammarInput::Count(1))
                    .expect("count");
            }
            resolver
                .feed(
                    EditorMode::Normal,
                    GrammarInput::OperatorBegin("delete".into()),
                )
                .expect("operator");
            if !typed_first {
                resolver
                    .feed(EditorMode::OperatorPending, GrammarInput::Count(1))
                    .expect("motion count");
            }
            assert_eq!(
                count(&mut resolver, EditorMode::OperatorPending),
                NonZeroU64::new(1)
            );
        }
    }

    #[test]
    fn decorated_invocations_preserve_an_explicit_count_of_one() {
        let mut resolver = ModalResolver::default();
        let mut invocation = crate::editor::input::grammar::command("view.cursor.move").unwrap();
        resolver.decorate_invocation(&mut invocation);
        assert_eq!(invocation.count, None);
        resolver
            .feed(EditorMode::Normal, GrammarInput::Count(1))
            .unwrap();
        resolver.decorate_invocation(&mut invocation);
        assert_eq!(invocation.count, NonZeroU64::new(1));
        assert!(!resolver.has_pending_grammar());
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
    #[test]
    fn operator_zero_is_a_motion_and_count_failures_clear_pending_grammar() {
        let mut resolver = ModalResolver::with_count_limit(10);
        resolver
            .feed(
                EditorMode::Normal,
                GrammarInput::OperatorBegin("delete".into()),
            )
            .unwrap();
        let InputAction::Invoke(delete) = resolver
            .feed(EditorMode::OperatorPending, GrammarInput::Zero)
            .unwrap()
        else {
            panic!("delete to start")
        };
        assert_eq!(
            delete.arguments["motion"],
            CommandValue::String("line_start".into())
        );
        resolver
            .feed(EditorMode::Normal, GrammarInput::Count(9))
            .unwrap();
        assert!(
            resolver
                .feed(EditorMode::Normal, GrammarInput::Count(9))
                .is_err()
        );
        assert!(!resolver.has_pending_grammar());
        resolver
            .feed(EditorMode::Normal, GrammarInput::Count(9))
            .unwrap();
        resolver
            .feed(
                EditorMode::Normal,
                GrammarInput::OperatorBegin("delete".into()),
            )
            .unwrap();
        resolver
            .feed(EditorMode::OperatorPending, GrammarInput::Count(9))
            .unwrap();
        assert!(
            resolver
                .feed(
                    EditorMode::OperatorPending,
                    GrammarInput::Motion {
                        motion: "word_forward".into(),
                        linewise: false
                    }
                )
                .is_err()
        );
        assert!(!resolver.has_pending_grammar());
    }
}
