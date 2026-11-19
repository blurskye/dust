//! Language configuration records and validation.

use serde::Deserialize;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InlayHintLevel {
    #[default]
    Standard,
    Max,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LspConfig {
    pub enabled: bool,
    pub auto_start: bool,
    /// Install a server for a file's language when none is installed, as mason-lspconfig does.
    pub auto_install: bool,
    pub inlay_hints: bool,
    pub inlay_hint_level: InlayHintLevel,
    /// Open the completion menu while typing, not only on ctrl-space.
    pub auto_completion: bool,
    pub format_on_save: bool,
    pub nix_workspace_fallback: bool,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_start: true,
            auto_install: true,
            inlay_hints: true,
            inlay_hint_level: InlayHintLevel::Standard,
            auto_completion: true,
            format_on_save: true,
            nix_workspace_fallback: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyntaxConfig {
    pub enabled: bool,
}
impl Default for SyntaxConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PersistenceConfig {
    pub enabled: bool,
    pub restore_on_startup: bool,
    /// Keep each file's undo history across restarts, like Vim's `undofile`.
    pub undo_history: bool,
}
impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            restore_on_startup: true,
            undo_history: true,
        }
    }
}
