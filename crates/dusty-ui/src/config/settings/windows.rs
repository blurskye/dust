//! Windows configuration records and validation.

use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WindowPlacementConfig {
    Automatic,
    Docked {
        edge: String,
        size: u16,
    },
    Floating {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
    },
    Fullscreen,
    Hidden,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WindowRuleConfig {
    pub content_type: String,
    pub placement: WindowPlacementConfig,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WindowsConfig {
    #[serde(default, rename = "rule")]
    pub rules: Vec<WindowRuleConfig>,
}
