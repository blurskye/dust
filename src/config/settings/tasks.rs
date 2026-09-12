use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    #[default]
    Task,
    Formatter,
    Linter,
    Test,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProblemMatcherConfig {
    pub pattern: String,
    pub path_group: String,
    pub line_group: String,
    pub column_group: Option<String>,
    pub severity_group: Option<String>,
    pub message_group: String,
    #[serde(default)]
    pub name_group: Option<String>,
    #[serde(default)]
    pub status_group: Option<String>,
    #[serde(default)]
    pub duration_group: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskConfig {
    #[serde(default)]
    pub kind: TaskKind,
    pub executable: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub stdin: bool,
    pub matcher: Option<ProblemMatcherConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservabilityConfig {
    pub enabled: bool,
    pub file_name: String,
    pub maximum_records: usize,
    pub maximum_field_bytes: usize,
    pub maximum_file_bytes: u64,
    pub rotations: usize,
    pub queue_capacity: usize,
    pub sensitive_keys: Vec<String>,
}
impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            file_name: "dust.jsonl".into(),
            maximum_records: 2000,
            maximum_field_bytes: 4096,
            maximum_file_bytes: 8 * 1024 * 1024,
            rotations: 3,
            queue_capacity: 256,
            sensitive_keys: vec![
                "token".into(),
                "password".into(),
                "secret".into(),
                "authorization".into(),
                "source".into(),
                "text".into(),
            ],
        }
    }
}
