use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub event_poll_ms: u64,
    pub vfs_queue_capacity: usize,
    pub macro_recursion_limit: usize,
    pub macro_step_limit: usize,
    pub repeat_step_limit: usize,
    pub completion_limit: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            event_poll_ms: 16,
            vfs_queue_capacity: 8,
            macro_recursion_limit: 16,
            macro_step_limit: 10_000,
            repeat_step_limit: 10_000,
            completion_limit: 1000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryStreamConfig {
    pub enabled: bool,
    pub maximum_entries: usize,
}

impl Default for HistoryStreamConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            maximum_entries: 100,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryConfig {
    pub command: HistoryStreamConfig,
    pub search: HistoryStreamConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    pub smart_case: bool,
    pub regex: bool,
    pub wrap: bool,
    pub highlight: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            smart_case: true,
            regex: false,
            wrap: true,
            highlight: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryConfig {
    pub enabled: bool,
    pub maximum_discovered_records: usize,
    pub write_debounce_ms: u64,
    pub delete_clean_journals: bool,
    pub cleanup_enabled: bool,
    pub maximum_retained_records: usize,
    /// Zero disables age-based cleanup.
    pub maximum_age_days: u64,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            maximum_discovered_records: 100,
            write_debounce_ms: 250,
            delete_clean_journals: true,
            cleanup_enabled: true,
            maximum_retained_records: 100,
            maximum_age_days: 30,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigWatchConfig {
    pub enabled: bool,
    pub poll_interval_ms: u64,
    pub debounce_ms: u64,
}

impl Default for ConfigWatchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_ms: 1000,
            debounce_ms: 100,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    pub maximum_diagnostics: usize,
    pub maximum_diagnostics_per_owner_uri: usize,
    pub maximum_picker_items: usize,
    pub picker_wrap: bool,
    pub trust_state_queue_capacity: usize,
    pub search: WorkspaceSearchConfig,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            maximum_diagnostics: 10_000,
            maximum_diagnostics_per_owner_uri: 2_000,
            maximum_picker_items: 10_000,
            picker_wrap: true,
            trust_state_queue_capacity: 8,
            search: WorkspaceSearchConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSearchConfig {
    pub regex: bool,
    pub smart_case: bool,
    pub whole_word: bool,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub include_hidden: bool,
    pub include_ignored: bool,
    pub maximum_files: usize,
    pub maximum_results: usize,
    pub maximum_file_bytes: u64,
    pub maximum_total_bytes: u64,
    pub maximum_preview_bytes: usize,
    pub batch_size: usize,
    pub event_queue_capacity: usize,
}

impl Default for WorkspaceSearchConfig {
    fn default() -> Self {
        Self {
            regex: false,
            smart_case: true,
            whole_word: false,
            include_globs: Vec::new(),
            exclude_globs: vec!["target/**".into(), ".git/**".into()],
            include_hidden: false,
            include_ignored: false,
            maximum_files: 100_000,
            maximum_results: 10_000,
            maximum_file_bytes: 8 * 1024 * 1024,
            maximum_total_bytes: 512 * 1024 * 1024,
            maximum_preview_bytes: 4_096,
            batch_size: 128,
            event_queue_capacity: 8,
        }
    }
}
