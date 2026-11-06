//! Keys, mappings and Vim input grammar.

#![forbid(unsafe_code)]

mod command_line;
mod context;
mod grammar;
mod key;
mod keymap;
mod modes;
mod search;

pub use command_line::ExParseError;
pub use command_line::parse_ex;
pub use command_line::substitute_pattern;
pub(crate) use command_line::substitution_pattern;
pub use command_line::typed_substitution;
pub use context::InputContextDescriptor;
pub use context::InputContextError;
pub use context::InputContextLayer;
pub use context::InputContextPolicy;
pub use context::InputContexts;
pub use grammar::ABSOLUTE_MAX_COUNT;
pub use grammar::GrammarInput;
pub use grammar::InputAction;
pub use grammar::InputError;
pub use grammar::ModalResolver;
pub use grammar::PendingInputContext;
pub use key::KeyCode;
pub use key::KeyEvent;
pub use key::KeyParseError;
pub use key::Modifiers;
pub use key::TerminalMouseInput;
pub use key::TerminalNavigationKey;
pub use keymap::Keymap;
pub(crate) use keymap::KeymapBindings;
pub use keymap::KeymapLayer;
pub use keymap::KeymapResolver;
pub(crate) use keymap::KeymapSequence;
pub use keymap::MappingResolution;
pub use modes::{ModeCursor, ModeError, ModeKeys, NamedMode};
pub use search::SearchPatternError;
pub use search::search_match;
pub use search::search_matches;
pub use search::substitution_regex;
