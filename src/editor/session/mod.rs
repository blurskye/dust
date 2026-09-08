//! Live editor composition and feature-private owners.
mod configuration;
mod debugger;
mod diagnostics;
mod dispatch;
mod documents;
mod explorer;
#[path = "extensions/session.rs"]
mod extensions;
mod git;
mod input;
#[path = "lsp/session.rs"]
mod lsp;
mod lsp_edits;
mod notifications;
mod persistence;
mod plugin_edits;
mod recovery;
mod search;
mod syntax;
mod tasks;
mod terminal;
#[path = "tooling/session.rs"]
mod tooling;
mod windows;
mod workspace;
mod workspace_edits;

pub use configuration::{
    ConfigurationSources, KeymapInspection, ResolvedConfiguration, ResolvedKeymaps,
    compile_keymaps, compile_keymaps_with_plugin_catalog, compile_keymaps_with_plugin_index,
    reload_keymaps, validate_loaded_configuration,
};
pub use dispatch::commands::{
    AppEffect, LayoutAction, PickerKind, PromptMove, register_app_commands,
};
pub use dispatch::{InteractiveError, run_interactive};
