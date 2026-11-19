//! Workspace configuration records and validation.

use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    pub smart_case: bool,
    pub wrap: bool,
    pub highlight: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            smart_case: true,
            wrap: true,
            highlight: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryConfig {
    pub enabled: bool,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    pub search: WorkspaceSearchConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSearchConfig {
    pub exclude_globs: Vec<String>,
    pub include_hidden: bool,
    pub include_ignored: bool,
}

impl Default for WorkspaceSearchConfig {
    fn default() -> Self {
        Self {
            exclude_globs: vec!["target/**".into(), ".git/**".into()],
            include_hidden: false,
            include_ignored: false,
        }
    }
}
