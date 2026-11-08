//! Input configuration records and validation.

use std::collections::BTreeMap;

use serde::Deserialize;

use serde::Serialize;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartupConfig {
    pub show_dashboard: bool,
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            show_dashboard: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeymapPolicyConfig {
    pub leader: String,
    pub local_leader: String,
}

impl Default for KeymapPolicyConfig {
    fn default() -> Self {
        Self {
            leader: "space".into(),
            local_leader: "\\".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    pub mapping_timeout_ms: u64,
    pub keymaps: KeymapPolicyConfig,
    pub text_commands: BTreeMap<String, String>,
    pub paste_commands: BTreeMap<String, String>,
    pub character_fallback_commands: BTreeMap<String, String>,
    pub unmatched_commands: BTreeMap<String, String>,
    pub command_aliases: BTreeMap<String, String>,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            mapping_timeout_ms: 500,
            keymaps: KeymapPolicyConfig::default(),
            text_commands: BTreeMap::new(),
            paste_commands: BTreeMap::new(),
            character_fallback_commands: BTreeMap::new(),
            unmatched_commands: BTreeMap::new(),
            command_aliases: BTreeMap::new(),
        }
    }
}
