//! Live editor composition and feature-private owners.
mod clipboard;
mod configuration;
mod debugger;
mod diagnostics;
mod dispatch;
mod documents;
mod explorer;
#[path = "extensions/session.rs"]
mod extensions;
mod flash;
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

#[derive(Clone, Debug)]
pub(in crate::editor::session) enum TrustedAction {
    StartDebug(debugger::DebugIntent),
    InstallRegistry {
        publisher: String,
        name: String,
        version: String,
    },
    PrepareRegistryArtifact,
    InstallPackage {
        package_manifest: std::path::PathBuf,
        plugin_manifest: std::path::PathBuf,
        component: std::path::PathBuf,
        update: bool,
    },
    RunTask(String),
    OpenTerminal(terminal::TerminalIntent),
    OpenDapTerminal {
        intent: terminal::TerminalIntent,
        reply: crate::debugger::DapTerminalReply,
    },
    RunTest {
        name: String,
        matcher: crate::config::ProblemMatcherConfig,
    },
    RunLinter {
        name: String,
        matcher: crate::config::ProblemMatcherConfig,
    },
    RunFormatter {
        name: String,
        source: tasks::FormatterSource,
    },
}

pub use configuration::{
    ConfigurationSources, KeymapInspection, ResolvedConfiguration, ResolvedKeymaps, ViewToggle,
    compile_keymaps, compile_keymaps_with_plugin_catalog, compile_keymaps_with_plugin_index,
    reload_keymaps, validate_loaded_configuration,
};
pub use dispatch::agent::commands::AgentCommand;
pub use dispatch::commands::{
    AppEffect, LayoutAction, PickerKind, PromptMove, register_app_commands,
};
pub use dispatch::{InteractiveError, run_interactive};
