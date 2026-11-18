//! Dusty core: text editing, files, processes, persistence and workspace state shared by
//! the editor and Dusty Git.

#![forbid(unsafe_code)]
#![cfg_attr(all(test, not(unix)), allow(dead_code))]

/// Injectable production and deterministic clock/ID providers.
pub mod clock;
/// Editing state, text, commands, input and transactions.
pub mod editor;
/// Bounded filesystem effects, checked atomic persistence, and recovery journals.
pub mod files;
/// Configuration and state directories.
pub mod paths;
/// Versioned bounded session, navigation, and history persistence.
pub mod persistence;
/// Bounded process supervision and protocol framing shared by built-in features.
pub mod process;
/// Structured bounded redacted logs and health snapshots.
pub mod telemetry;
/// Helpers for tests that run programs.
pub mod testing;
/// Window topology and contracts.
pub mod windows;
/// Workspace roots, trust, diagnostics, pickers and revision-safe edit proposals.
pub mod workspace;
