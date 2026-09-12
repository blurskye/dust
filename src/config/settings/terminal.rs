use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalProfileConfig {
    pub program: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    pub columns: u16,
    pub rows: u16,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalsConfig {
    pub queue_capacity: usize,
    pub maximum_output_bytes: usize,
    pub maximum_input_bytes: usize,
    pub scrollback_bytes: usize,
    pub default_profile: String,
    #[serde(default)]
    pub profiles: BTreeMap<String, TerminalProfileConfig>,
}
impl Default for TerminalsConfig {
    fn default() -> Self {
        Self {
            queue_capacity: 256,
            maximum_output_bytes: 32 * 1024 * 1024,
            maximum_input_bytes: 1024 * 1024,
            scrollback_bytes: 8 * 1024 * 1024,
            default_profile: "shell".into(),
            profiles: BTreeMap::from([(
                "shell".into(),
                TerminalProfileConfig {
                    program: PathBuf::from("/bin/sh"),
                    arguments: Vec::new(),
                    cwd: None,
                    environment: BTreeMap::new(),
                    columns: 80,
                    rows: 24,
                },
            )]),
        }
    }
}
