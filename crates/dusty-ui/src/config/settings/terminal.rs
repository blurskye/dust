//! Terminal configuration records and validation.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use serde::Serialize;

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
    /// Open as a centered float instead of the bottom dock (lazygit and similar tools).
    #[serde(default)]
    pub float: bool,
    /// Close the window when the program exits successfully.
    #[serde(default)]
    pub close_on_exit: bool,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalsConfig {
    pub default_profile: String,
    #[serde(default)]
    pub profiles: BTreeMap<String, TerminalProfileConfig>,
}
impl Default for TerminalsConfig {
    fn default() -> Self {
        Self {
            default_profile: "shell".into(),
            profiles: BTreeMap::from([(
                "shell".into(),
                TerminalProfileConfig {
                    program: crate::config::load::bundled_setting(
                        "terminals.profiles.shell.program",
                    )
                    .and_then(|value| value.as_str().map(PathBuf::from))
                    .expect("bundled shell program"),
                    arguments: Vec::new(),
                    cwd: None,
                    environment: BTreeMap::new(),
                    columns: 80,
                    rows: 24,
                    float: false,
                    close_on_exit: false,
                },
            )]),
        }
    }
}
