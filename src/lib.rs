//! Dusty library modules.

#![forbid(unsafe_code)]
// Some test helpers serve only Unix-only tests, so Windows test builds leave them unused.
#![cfg_attr(all(test, not(unix)), allow(dead_code))]

pub use dusty_core::{clock, editor, files, persistence, process, telemetry, windows, workspace};

/// Agent Client Protocol client for AI coding agents.
pub mod agent;
/// Public command, configuration, persistence, tool, and WIT API versions.
pub mod api;
/// Application composition and ordered event loop.
pub mod app;
/// Bundled/user TOML settings, keymaps, dashboard declarations, and platform paths.
pub use dusty_ui::config;
/// Built-in Debug Adapter Protocol transport and session control.
pub mod debugger;
/// Forge providers and the forge worker, from the `dusty-forge` crate.
pub use dusty_forge as forge;
/// Language identification, syntax execution, and built-in Language Server Protocol support.
pub mod language;
/// Bounded asynchronous WebAssembly Component Model extension ownership.
pub mod plugins;
/// Opening files in a running Dusty from programs it starts.
pub mod remote;
/// Headless fixtures exercising production editing and input paths.
#[cfg(test)]
pub mod testing;
/// Typed tool catalog, executable discovery, registry, and explicit installer worker.
pub mod tools;
/// Unicode cell-grid presentation and terminal lifecycle.
pub use dusty_ui as ui;

/// Git operations.
pub use dusty_git as git;
