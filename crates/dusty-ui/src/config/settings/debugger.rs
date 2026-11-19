//! Debugger configuration records and validation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use serde::Serialize;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DebugAdapterConfig {
    pub program: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugRequestKind {
    Launch,
    Attach,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DebugSessionConfig {
    pub adapter: String,
    pub request: DebugRequestKind,
    pub cwd: Option<PathBuf>,
    pub program: Option<PathBuf>,
    #[serde(default)]
    pub run_in_terminal_profile: Option<String>,
    #[serde(default)]
    pub arguments: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub path_maps: BTreeMap<PathBuf, PathBuf>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct DebugConfig {
    #[serde(default)]
    pub adapters: BTreeMap<String, DebugAdapterConfig>,
    #[serde(default)]
    pub sessions: BTreeMap<String, DebugSessionConfig>,
}
