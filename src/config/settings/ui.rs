//! Ui configuration records and validation.

use std::collections::BTreeMap;

use serde::Deserialize;

use serde::Serialize;

use crate::config::settings::editor::ColorMode;

use crate::config::settings::editor::CursorShape;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorderStyle {
    #[default]
    Single,
    Double,
    Rounded,
}

impl BorderStyle {
    /// Horizontal, vertical, then the top-left, top-right, bottom-left and bottom-right corners.
    #[must_use]
    pub fn glyphs(self) -> (char, char, char, char, char, char) {
        match self {
            Self::Double => ('═', '║', '╔', '╗', '╚', '╝'),
            Self::Rounded => ('─', '│', '╭', '╮', '╰', '╯'),
            Self::Single => ('─', '│', '┌', '┐', '└', '┘'),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PromptTextConfig {
    pub command_title: String,
    pub command_marker: String,
    pub search_forward_title: String,
    pub search_forward_marker: String,
    pub search_backward_title: String,
    pub search_backward_marker: String,
    pub workspace_trust_title: String,
    pub workspace_trust_marker: String,
    pub git_reset_title: String,
    pub git_reset_marker: String,
    pub explorer_delete_title: String,
    pub explorer_delete_marker: String,
    #[serde(default = "rename_prompt_title")]
    pub explorer_rename_title: String,
    #[serde(default = "rename_prompt_marker")]
    pub explorer_rename_marker: String,
}
fn rename_prompt_title() -> String {
    "Rename path".into()
}
fn rename_prompt_marker() -> String {
    "> ".into()
}

impl Default for PromptTextConfig {
    fn default() -> Self {
        Self {
            command_title: "Command".into(),
            command_marker: ": ".into(),
            search_forward_title: "Search Forward".into(),
            search_forward_marker: "/ ".into(),
            search_backward_title: "Search Backward".into(),
            search_backward_marker: "? ".into(),
            workspace_trust_title: "Do you want to trust this workspace?".into(),
            workspace_trust_marker: "> ".into(),
            git_reset_title: "Reset this Git hunk?".into(),
            git_reset_marker: "> ".into(),
            explorer_delete_title: "Delete this path?".into(),
            explorer_delete_marker: "> ".into(),
            explorer_rename_title: rename_prompt_title(),
            explorer_rename_marker: rename_prompt_marker(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CommandLineConfig {
    /// Glyphs by language-server completion kind, drawn before a completion's label.
    pub completion_kind_icons: std::collections::BTreeMap<String, String>,
    pub text: PromptTextConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CursorConfig {
    pub normal: CursorShape,
    pub insert: CursorShape,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            normal: CursorShape::Block,
            insert: CursorShape::Bar,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusLineConfig {
    pub enabled: bool,
    pub components: Vec<String>,
    pub mode_labels: BTreeMap<String, String>,
}

impl Default for StatusLineConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            components: vec!["mode".into(), "file".into(), "position".into()],
            mode_labels: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LspProgressConfig {
    pub enabled: bool,
}

impl Default for LspProgressConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BufferLineConfig {
    pub enabled: bool,
    pub extension_icons: BTreeMap<String, String>,
    pub hide_when_single: bool,
}

impl Default for BufferLineConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            extension_icons: BTreeMap::new(),
            hide_when_single: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolManagerConfig {
    pub categories: BTreeMap<String, String>,
}

impl Default for ToolManagerConfig {
    fn default() -> Self {
        Self {
            categories: [
                ("all".into(), "All".into()),
                ("lsp".into(), "LSP".into()),
                ("dap".into(), "DAP".into()),
                ("linter".into(), "Linter".into()),
                ("formatter".into(), "Formatter".into()),
                ("agent".into(), "Agent".into()),
            ]
            .into_iter()
            .collect(),
        }
    }
}

/// Which edge of the screen a side panel docks on.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelSide {
    #[default]
    Left,
    Right,
}

/// Where the terminal panel opens: docked at the bottom, as a centered float, or over the
/// whole screen.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalPlacement {
    #[default]
    Bottom,
    Float,
    Fullscreen,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorerIconMode {
    NerdFont,
    Unicode,
    Ascii,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorerGlyphSet {
    pub tree_vertical: String,
    pub tree_middle: String,
    pub tree_last: String,
    pub tree_space: String,
    pub directory: String,
    pub directory_open: String,
    pub file: String,
    pub symlink: String,
    pub loading: String,
    pub icon_spacing: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorerGlyphs {
    pub nerd_font: ExplorerGlyphSet,
    pub unicode: ExplorerGlyphSet,
    pub ascii: ExplorerGlyphSet,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorerConfig {
    pub width: u16,
    #[serde(default)]
    pub side: PanelSide,
    pub icon_mode: ExplorerIconMode,
    pub glyphs: ExplorerGlyphs,
}

impl ExplorerConfig {
    #[must_use]
    pub fn active_glyphs(&self) -> &ExplorerGlyphSet {
        match self.icon_mode {
            ExplorerIconMode::NerdFont => &self.glyphs.nerd_font,
            ExplorerIconMode::Unicode => &self.glyphs.unicode,
            ExplorerIconMode::Ascii => &self.glyphs.ascii,
        }
    }
}

impl Default for ExplorerConfig {
    fn default() -> Self {
        let unicode = ExplorerGlyphSet {
            tree_vertical: "│ ".into(),
            tree_middle: "├─".into(),
            tree_last: "└─".into(),
            tree_space: "  ".into(),
            directory: "▸".into(),
            directory_open: "▾".into(),
            file: "·".into(),
            symlink: "↗".into(),
            loading: "…".into(),
            icon_spacing: " ".into(),
        };
        Self {
            width: 40,
            side: PanelSide::Left,
            icon_mode: ExplorerIconMode::NerdFont,
            glyphs: ExplorerGlyphs {
                nerd_font: ExplorerGlyphSet {
                    directory: "\u{f024b}".into(),
                    directory_open: "\u{f0770}".into(),
                    file: "󰈔".into(),
                    symlink: "".into(),
                    ..unicode.clone()
                },
                unicode,
                ascii: ExplorerGlyphSet {
                    tree_vertical: "| ".into(),
                    tree_middle: "|-".into(),
                    tree_last: "\\-".into(),
                    tree_space: "  ".into(),
                    directory: "+".into(),
                    directory_open: "-".into(),
                    file: ".".into(),
                    symlink: "@".into(),
                    loading: "~".into(),
                    icon_spacing: " ".into(),
                },
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeyHintConfig {
    pub enabled: bool,
    pub delay_ms: u64,
}

impl Default for KeyHintConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            delay_ms: 250,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentationPopupConfig {
    pub enabled: bool,
}

impl Default for DocumentationPopupConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalPanelConfig {
    pub height: u16,
    #[serde(default)]
    pub placement: TerminalPlacement,
}

impl Default for TerminalPanelConfig {
    fn default() -> Self {
        Self {
            height: 16,
            placement: TerminalPlacement::Bottom,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    pub theme: String,
    pub color_mode: ColorMode,
    pub mouse: bool,
    pub command_line: CommandLineConfig,
    pub cursor: CursorConfig,
    pub status_line: StatusLineConfig,
    pub buffer_line: BufferLineConfig,
    pub lsp_progress: LspProgressConfig,
    pub key_hints: KeyHintConfig,
    pub documentation_popup: DocumentationPopupConfig,
    pub terminal_panel: TerminalPanelConfig,
    pub tool_manager: ToolManagerConfig,
    pub explorer: ExplorerConfig,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dust".into(),
            color_mode: ColorMode::Auto,
            mouse: true,
            command_line: CommandLineConfig::default(),
            cursor: CursorConfig::default(),
            status_line: StatusLineConfig::default(),
            buffer_line: BufferLineConfig::default(),
            lsp_progress: LspProgressConfig::default(),
            key_hints: KeyHintConfig::default(),
            documentation_popup: DocumentationPopupConfig::default(),
            terminal_panel: TerminalPanelConfig::default(),
            tool_manager: ToolManagerConfig::default(),
            explorer: ExplorerConfig::default(),
        }
    }
}
