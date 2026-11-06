//! Tasks configuration records and validation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use serde::Serialize;

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
    #[serde(default)]
    pub column_encoding: Option<crate::editor::text::PositionEncoding>,
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
