//! Agents configuration records and validation.

use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One ACP agent: a program that speaks the Agent Client Protocol on stdin/stdout.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentProfileConfig {
    /// Looked up on `PATH` when it is not absolute.
    pub program: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    /// Set for both `program` and `cli`.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// Set for `program` only, on top of `environment`: settings for when Dust drives the
    /// agent, such as turning its own subagents off because Dust's `agent` tool gives them.
    #[serde(default)]
    pub acp_environment: BTreeMap<String, String>,
    /// Session options to choose once the agent starts, by option id or category, such as
    /// `model`, `mode` or `thought_level`. Values the agent does not offer are reported.
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    /// Sent as `_meta` when a session starts, for adapter settings such as which tools
    /// Claude Code may use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
    /// The command that adds Dust's tools to an agent that reads MCP servers only from its own
    /// config, such as `pi mcp add`; `{dust}` stands for this Dust program. Dust runs it before
    /// the agent starts when `agents.add_tools` is on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add_tools: Vec<String>,
    /// The agent's own command-line program and its arguments, run in a terminal on the
    /// right when `agents.connection` is `terminal`.
    #[serde(default)]
    pub cli: Vec<String>,
}

/// How `<leader>aa` talks to the default agent.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentConnection {
    /// Through the Agent Client Protocol, in Dust's agent panel.
    #[default]
    Acp,
    /// The agent's own program in a terminal tab.
    Terminal,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsConfig {
    /// The agent `<leader>aa` starts when none was picked.
    #[serde(default)]
    pub default: String,
    #[serde(default)]
    pub connection: AgentConnection,
    /// How much of the editor width the agent panel takes when it opens, in percent.
    #[serde(default = "default_panel_width")]
    pub panel_width: u16,
    /// The side the agent panel, or a terminal agent, opens on.
    #[serde(default = "default_agent_side")]
    pub side: crate::config::settings::ui::PanelSide,
    /// Run each profile's `add_tools` command, which changes the agent's own config.
    #[serde(default = "default_add_tools")]
    pub add_tools: bool,
    #[serde(default)]
    pub profiles: BTreeMap<String, AgentProfileConfig>,
}

const fn default_panel_width() -> u16 {
    35
}

const fn default_add_tools() -> bool {
    true
}

const fn default_agent_side() -> crate::config::settings::ui::PanelSide {
    crate::config::settings::ui::PanelSide::Right
}

impl Default for AgentsConfig {
    fn default() -> Self {
        Self {
            default: String::new(),
            connection: AgentConnection::default(),
            panel_width: default_panel_width(),
            side: default_agent_side(),
            add_tools: default_add_tools(),
            profiles: BTreeMap::new(),
        }
    }
}
