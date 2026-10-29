use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

/// One ACP agent: a program that speaks the Agent Client Protocol on stdin/stdout.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfileConfig {
    /// Looked up on `PATH` when it is not absolute.
    pub program: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    /// The agent `<leader>aa` starts when none was picked.
    #[serde(default)]
    pub default: String,
    #[serde(default)]
    pub profiles: BTreeMap<String, AgentProfileConfig>,
}
