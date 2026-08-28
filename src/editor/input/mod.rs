#![forbid(unsafe_code)]

mod command_line;
mod context;
mod grammar;
mod key;
mod keymap;
mod search;

pub use command_line::{ExParseError, parse_ex};
pub use context::{
    InputContextDescriptor, InputContextError, InputContextLayer, InputContextPolicy,
    InputContextRegistry,
};
pub use grammar::{GrammarInput, InputAction, InputError, ModalResolver, PendingInputContext};
pub use key::{
    KeyCode, KeyEvent, KeyParseError, Modifiers, TerminalMouseInput, TerminalNavigationKey,
};
pub use keymap::{Keymap, KeymapLayer, KeymapResolver, MappingResolution};
pub use search::{SearchPatternError, search_matches};
