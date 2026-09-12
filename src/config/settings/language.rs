use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InlayHintLevel {
    #[default]
    Standard,
    Max,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LspConfig {
    pub enabled: bool,
    pub auto_start: bool,
    pub inlay_hints: bool,
    pub inlay_hint_level: InlayHintLevel,
    pub nix_workspace_fallback: bool,
    pub request_timeout_ms: u64,
    pub restart_limit: u8,
    pub restart_backoff_ms: u64,
    pub maximum_log_lines: usize,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_start: true,
            inlay_hints: true,
            inlay_hint_level: InlayHintLevel::Standard,
            nix_workspace_fallback: true,
            request_timeout_ms: 10_000,
            restart_limit: 3,
            restart_backoff_ms: 250,
            maximum_log_lines: 200,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyntaxConfig {
    pub enabled: bool,
    pub queue_capacity: usize,
    pub debounce_ms: u64,
    pub maximum_source_bytes: usize,
    pub maximum_spans: usize,
    pub maximum_folds: usize,
    pub maximum_indent_rows: usize,
}
impl Default for SyntaxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            queue_capacity: 8,
            debounce_ms: 75,
            maximum_source_bytes: 8 * 1024 * 1024,
            maximum_spans: 100_000,
            maximum_folds: 10_000,
            maximum_indent_rows: 100_000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PersistenceConfig {
    pub enabled: bool,
    pub restore_on_startup: bool,
    pub save_on_exit: bool,
    pub state_file: String,
    pub maximum_file_bytes: usize,
    pub maximum_entries: usize,
    pub recent_files: usize,
    pub marks: usize,
    pub jumps: usize,
    pub changes: usize,
    pub persist_command_history: bool,
    pub persist_search_history: bool,
    pub queue_capacity: usize,
}
impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            restore_on_startup: true,
            save_on_exit: true,
            state_file: "session.json".into(),
            maximum_file_bytes: 4 * 1024 * 1024,
            maximum_entries: 10_000,
            recent_files: 256,
            marks: 256,
            jumps: 1024,
            changes: 1024,
            persist_command_history: false,
            persist_search_history: false,
            queue_capacity: 4,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessConfig {
    pub event_capacity: usize,
    pub maximum_output_bytes: usize,
    pub maximum_line_bytes: usize,
    pub maximum_stdin_bytes: usize,
    pub maximum_timeout_ms: u64,
    pub maximum_running: usize,
}
impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            event_capacity: 256,
            maximum_output_bytes: 16 * 1024 * 1024,
            maximum_line_bytes: 1024 * 1024,
            maximum_stdin_bytes: 16 * 1024 * 1024,
            maximum_timeout_ms: 300_000,
            maximum_running: 16,
        }
    }
}
