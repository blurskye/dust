use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
    pub load_bundled_defaults: bool,
    pub leader: String,
    pub local_leader: String,
}

impl Default for KeymapPolicyConfig {
    fn default() -> Self {
        Self {
            load_bundled_defaults: true,
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
    pub count_limit: u64,
    pub completion_wrap: bool,
    pub completion_select_first: bool,
    pub completion_accept_on_submit: bool,
    pub text_commands: BTreeMap<String, String>,
    pub paste_commands: BTreeMap<String, String>,
    pub character_fallback_commands: BTreeMap<String, String>,
    pub unmatched_commands: BTreeMap<String, String>,
    pub command_aliases: BTreeMap<String, String>,
    pub disabled_command_aliases: Vec<String>,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            mapping_timeout_ms: 500,
            keymaps: KeymapPolicyConfig::default(),
            count_limit: 1_000_000,
            completion_wrap: true,
            completion_select_first: false,
            completion_accept_on_submit: true,
            text_commands: BTreeMap::new(),
            paste_commands: BTreeMap::new(),
            character_fallback_commands: BTreeMap::new(),
            unmatched_commands: BTreeMap::new(),
            command_aliases: BTreeMap::new(),
            disabled_command_aliases: Vec::new(),
        }
    }
}
