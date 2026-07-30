#![forbid(unsafe_code)]

/// Public command, configuration, persistence, package, and WIT API versions.
pub mod api;
/// Startup composition, foreground coordination, command effects, and policy.
pub mod application;
/// Source-owned architecture and ambient-authority audit.
pub mod architecture;
/// Injectable production and deterministic clock/ID providers.
pub mod clock;
/// Bundled/user TOML settings, keymaps, dashboard declarations, and platform paths.
pub mod configuration;
/// Built-in Debug Adapter Protocol transport and session control.
pub mod debugging;
/// UTF-8 text storage, coordinates, graphemes, edits, and encoding preservation.
pub mod document;
/// Deterministic editor state, commands, transactions, undo, views, panes, and tabs.
pub mod editing;
/// Bounded filesystem effects, checked atomic persistence, and recovery journals.
pub mod filesystem;
/// Normalized logical input, configurable mappings, modal grammar, and Ex parsing.
pub mod input;
/// Built-in Language Server Protocol transport, sessions, and document synchronization.
pub mod languages;
/// Structured bounded redacted logs and health snapshots.
pub mod observability;
/// Typed tool catalog, executable discovery, and explicit installer worker.
pub mod packages;
/// Versioned bounded session, navigation, and history persistence.
pub mod persistence;
/// Bounded asynchronous WebAssembly Component Model plugin ownership.
pub mod plugins;
/// Bounded process supervision and protocol framing shared by built-in services.
pub mod processes;
/// Canonical Linux release manifest generation.
pub mod release;
/// Unicode cell-grid presentation and guarded Crossterm lifecycle.
pub mod syntax;
pub mod terminal_ui;
/// Headless fixture exercising production editing and input paths.
pub mod test_support;
/// Workspace roots, trust, diagnostics, pickers, and revision-safe edit proposals.
pub mod workspace;
