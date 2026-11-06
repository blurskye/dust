//! Dust library modules.

#![forbid(unsafe_code)]

/// Agent Client Protocol client for AI coding agents.
pub mod agent;
/// Public command, configuration, persistence, tool, and WIT API versions.
pub mod api;
/// Application composition and ordered event loop.
pub mod app;
/// Injectable production and deterministic clock/ID providers.
pub mod clock;
/// Bundled/user TOML settings, keymaps, dashboard declarations, and platform paths.
pub mod config;
/// Built-in Debug Adapter Protocol transport and session control.
pub mod debugger;
/// Editing state, text, commands, input and transactions.
pub mod editor;
/// Bounded filesystem effects, checked atomic persistence, and recovery journals.
pub mod files;
/// Pull requests through the GitHub, Gitea and Forgejo CLIs.
pub mod forge;
/// Language identification, syntax execution, and built-in Language Server Protocol support.
pub mod language;
/// Versioned bounded session, navigation, and history persistence.
pub mod persistence;
/// Bounded asynchronous WebAssembly Component Model extension ownership.
pub mod plugins;
/// Bounded process supervision and protocol framing shared by built-in features.
pub mod process;
/// Opening files in a running Dust from programs it starts.
pub mod remote;
/// Structured bounded redacted logs and health snapshots.
pub mod telemetry;
/// Headless fixtures exercising production editing and input paths.
#[cfg(test)]
pub mod testing;
/// Typed tool catalog, executable discovery, registry, and explicit installer worker.
pub mod tools;
/// Unicode cell-grid presentation and terminal lifecycle.
pub mod ui;
/// Workspace roots, trust, diagnostics, pickers and revision-safe edit proposals.
pub mod workspace;

/// Git operations.
pub mod git;
/// Window topology and contracts.
pub mod windows;
