#![forbid(unsafe_code)]

mod command_line;
mod context;
mod grammar;
mod key;
mod keymap;
mod search;

pub(crate) use command_line::substitution_pattern;
pub use command_line::{ExParseError, parse_ex, substitute_pattern, typed_substitution};
pub use context::{
    InputContextDescriptor, InputContextError, InputContextLayer, InputContextPolicy,
    InputContextRegistry,
};
pub use grammar::{
    ABSOLUTE_MAX_COUNT, GrammarInput, InputAction, InputError, ModalResolver, PendingInputContext,
};
pub use key::{
    KeyCode, KeyEvent, KeyParseError, Modifiers, TerminalMouseInput, TerminalNavigationKey,
};
pub use keymap::{Keymap, KeymapLayer, KeymapResolver, MappingResolution};
pub(crate) use keymap::{KeymapBindings, KeymapSequence};
pub use search::{SearchPatternError, search_match, search_matches, substitution_regex};
