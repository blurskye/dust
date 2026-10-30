use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::editor::{ColorMode, CursorShape};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BorderStyle {
    #[default]
    Single,
    Double,
    Rounded,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandLineConfig {
    pub width: u16,
    pub minimum_width: u16,
    pub narrow_width_threshold: u16,
    pub outer_margin: u16,
    pub top_rows: u16,
    pub border: BorderStyle,
    pub show_title: bool,
    pub completion_max_rows: u16,
    pub input_padding: u16,
    pub attachment_gap: u16,
    pub completion_detail_gap: u16,
    pub title_min_width: u16,
    pub text: PromptTextConfig,
}

impl Default for CommandLineConfig {
    fn default() -> Self {
        Self {
            width: 60,
            minimum_width: 36,
            narrow_width_threshold: 40,
            outer_margin: 1,
            top_rows: 2,
            border: BorderStyle::Single,
            show_title: true,
            completion_max_rows: 12,
            input_padding: 2,
            attachment_gap: 0,
            completion_detail_gap: 2,
            title_min_width: 10,
            text: PromptTextConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CursorConfig {
    pub normal: CursorShape,
    pub insert: CursorShape,
    pub replace: CursorShape,
    pub visual: CursorShape,
    pub operator_pending: CursorShape,
    pub command: CursorShape,
    pub prompt: CursorShape,
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self {
            normal: CursorShape::Block,
            insert: CursorShape::Bar,
            replace: CursorShape::Underline,
            visual: CursorShape::Block,
            operator_pending: CursorShape::Block,
            command: CursorShape::Bar,
            prompt: CursorShape::Bar,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusLineConfig {
    pub enabled: bool,
    pub components: Vec<String>,
    pub mode_labels: BTreeMap<String, String>,
    pub scratch_label: String,
    pub dirty_marker: String,
    pub mode_padding: String,
    pub component_separator: String,
    pub position_format: String,
    pub lsp_format: String,
    pub diagnostic_format: String,
    pub git_format: String,
    pub task_format: String,
    pub root_format: String,
}

impl Default for StatusLineConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            components: vec!["mode".into(), "file".into(), "position".into()],
            mode_labels: BTreeMap::new(),
            scratch_label: "[scratch]".into(),
            dirty_marker: " [+]".into(),
            mode_padding: " ".into(),
            component_separator: " ".into(),
            position_format: "{line}:{column} ".into(),
            lsp_format: "LSP {server}".into(),
            diagnostic_format: "E{errors} W{warnings}".into(),
            git_format: "Git {count} files".into(),
            task_format: "Tasks {count}".into(),
            root_format: "{root}".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationConfig {
    pub max_width: u16,
    pub timeout_ms: u64,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            max_width: 120,
            timeout_ms: 4000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LspProgressConfig {
    pub enabled: bool,
    pub max_rows: u16,
    pub max_width: u16,
    pub right_margin: u16,
    pub rows_from_bottom: u16,
    pub spinner_frames: Vec<String>,
    pub spinner_interval_ms: u64,
    pub completed_symbol: String,
    pub completed_timeout_ms: u64,
}

impl Default for LspProgressConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_rows: 8,
            max_width: 72,
            right_margin: 1,
            rows_from_bottom: 1,
            spinner_frames: ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            spinner_interval_ms: 80,
            completed_symbol: "✓".into(),
            completed_timeout_ms: 3_000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaneConfig {
    pub default_ratio: u16,
    pub resize_step: u16,
    pub minimum_width: u16,
    pub minimum_height: u16,
    pub border: BorderStyle,
    pub focus_wrap: bool,
}

impl Default for PaneConfig {
    fn default() -> Self {
        Self {
            default_ratio: 500,
            resize_step: 50,
            minimum_width: 8,
            minimum_height: 3,
            border: BorderStyle::Single,
            focus_wrap: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BufferLineConfig {
    pub enabled: bool,
    pub show_icons: bool,
    pub show_diagnostics: bool,
    pub show_pins: bool,
    pub show_close: bool,
    pub pin_marker: String,
    pub close_marker: String,
    pub diagnostic_error_marker: String,
    pub diagnostic_warning_marker: String,
    pub scratch_icon: String,
    pub virtual_icon: String,
    pub terminal_icon: String,
    pub custom_icon: String,
    /// The icon of agent sessions in the agent panel's tab line.
    pub agent_icon: String,
    pub extension_icons: BTreeMap<String, String>,
    pub hide_when_single: bool,
    pub active_format: String,
    pub inactive_format: String,
    pub separator: String,
    pub dirty_marker: String,
    pub scratch_label: String,
    pub overflow_left: String,
    pub overflow_right: String,
    pub maximum_name_width: u16,
}

impl Default for BufferLineConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            show_icons: true,
            show_diagnostics: true,
            show_pins: true,
            show_close: true,
            pin_marker: "P".into(),
            close_marker: "x".into(),
            diagnostic_error_marker: "E".into(),
            diagnostic_warning_marker: "W".into(),
            scratch_icon: "󰎚".into(),
            virtual_icon: "󰈙".into(),
            terminal_icon: "".into(),
            custom_icon: "󰈔".into(),
            agent_icon: "󰚩".into(),
            extension_icons: BTreeMap::new(),
            hide_when_single: true,
            active_format: " {title}{dirty}  ".into(),
            inactive_format: " {title}{dirty}  ".into(),
            separator: "│".into(),
            dirty_marker: " ●".into(),
            scratch_label: "[scratch]".into(),
            overflow_left: "‹ ".into(),
            overflow_right: " ›".into(),
            maximum_name_width: 32,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TabLineConfig {
    pub enabled: bool,
    pub hide_when_single: bool,
    pub active_format: String,
    pub inactive_format: String,
    pub separator: String,
    pub scratch_label: String,
    pub focus_wrap: bool,
}

impl Default for TabLineConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            hide_when_single: true,
            active_format: "[{title}]".into(),
            inactive_format: " {title} ".into(),
            separator: "│".into(),
            scratch_label: "[scratch]".into(),
            focus_wrap: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolManagerConfig {
    pub title: String,
    pub help: String,
    pub categories: BTreeMap<String, String>,
    pub category_order: Vec<String>,
    pub width_percent: u8,
    pub height_percent: u8,
    pub minimum_width: u16,
    pub minimum_height: u16,
    pub border: BorderStyle,
    pub row_marker: String,
    pub selected_marker: String,
    pub language_separator: String,
}

impl Default for ToolManagerConfig {
    fn default() -> Self {
        Self {
            title: "Tools".into(),
            help: "j/k move · Tab category · Enter install or start · r refresh · Esc close".into(),
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
            category_order: vec![
                "all".into(),
                "lsp".into(),
                "dap".into(),
                "linter".into(),
                "formatter".into(),
                "agent".into(),
            ],
            width_percent: 82,
            height_percent: 84,
            minimum_width: 48,
            minimum_height: 14,
            border: BorderStyle::Single,
            row_marker: "○".into(),
            selected_marker: "●".into(),
            language_separator: ", ".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PickerConfig {
    pub width_percent: u8,
    pub height_percent: u8,
    pub minimum_width: u16,
    pub minimum_height: u16,
    pub border: BorderStyle,
    pub selected_marker: String,
    pub row_marker: String,
    pub source_separator: String,
    pub loading_label: String,
    pub maximum_preview_bytes: usize,
}

impl Default for PickerConfig {
    fn default() -> Self {
        Self {
            width_percent: 88,
            height_percent: 82,
            minimum_width: 40,
            minimum_height: 10,
            border: BorderStyle::Single,
            selected_marker: ">".into(),
            row_marker: " ".into(),
            source_separator: String::new(),
            loading_label: "loading…".into(),
            maximum_preview_bytes: 256 * 1024,
        }
    }
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
    /// Before a folder's icon, closed and open.
    pub expander: String,
    pub expander_open: String,
    pub directory: String,
    pub directory_open: String,
    pub file: String,
    pub symlink: String,
    pub loading: String,
    pub panel_vertical: String,
    pub panel_horizontal: String,
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
pub struct ExplorerWatchConfig {
    pub enabled: bool,
    pub poll_interval_ms: u64,
    pub debounce_ms: u64,
    pub maximum_directories: usize,
    pub maximum_entries_per_directory: usize,
    pub maximum_events: usize,
    pub maximum_path_bytes: usize,
    pub queue_capacity: usize,
}
impl Default for ExplorerWatchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_ms: 500,
            debounce_ms: 100,
            maximum_directories: 128,
            maximum_entries_per_directory: 10_000,
            maximum_events: 128,
            maximum_path_bytes: 65_536,
            queue_capacity: 8,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorerConfig {
    pub bundled_enabled: bool,
    pub watch: ExplorerWatchConfig,
    pub width: u16,
    pub minimum_width: u16,
    pub maximum_width: u16,
    pub maximum_visible_rows: usize,
    pub preview_enabled: bool,
    pub maximum_preview_bytes: usize,
    pub maximum_preview_lines: usize,
    pub loading_label: String,
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
            tree_middle: "│ ".into(),
            tree_last: "└ ".into(),
            tree_space: "  ".into(),
            expander: "▸ ".into(),
            expander_open: "▾ ".into(),
            directory: "▪".into(),
            directory_open: "▫".into(),
            file: "·".into(),
            symlink: "↗".into(),
            loading: "…".into(),
            panel_vertical: "⎸".into(),
            panel_horizontal: "⎯".into(),
            icon_spacing: " ".into(),
        };
        Self {
            bundled_enabled: true,
            watch: ExplorerWatchConfig::default(),
            width: 40,
            minimum_width: 28,
            maximum_width: 60,
            maximum_visible_rows: 200,
            preview_enabled: false,
            maximum_preview_bytes: 256 * 1024,
            maximum_preview_lines: 20,
            loading_label: "loading…".into(),
            icon_mode: ExplorerIconMode::NerdFont,
            glyphs: ExplorerGlyphs {
                nerd_font: ExplorerGlyphSet {
                    expander: "\u{f460} ".into(),
                    expander_open: "\u{f47c} ".into(),
                    directory: "\u{e5ff}".into(),
                    directory_open: "\u{e5fe}".into(),
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
                    expander: "+ ".into(),
                    expander_open: "- ".into(),
                    directory: "/".into(),
                    directory_open: "/".into(),
                    file: ".".into(),
                    symlink: "@".into(),
                    loading: "~".into(),
                    panel_vertical: "|".into(),
                    panel_horizontal: "-".into(),
                    icon_spacing: " ".into(),
                },
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyHintAnchor {
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeyHintConfig {
    pub enabled: bool,
    pub delay_ms: u64,
    pub anchor: KeyHintAnchor,
    pub max_rows: u16,
    pub max_columns: u16,
    pub max_width: u16,
    pub horizontal_padding: u16,
    pub vertical_padding: u16,
    pub row_gap: u16,
    pub column_gap: u16,
    pub show_group_icons: bool,
    pub show_command_titles: bool,
    pub fallback_label: String,
    pub border: BorderStyle,
}

impl Default for KeyHintConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            delay_ms: 250,
            anchor: KeyHintAnchor::BottomRight,
            max_rows: 8,
            max_columns: 3,
            max_width: 96,
            horizontal_padding: 1,
            vertical_padding: 0,
            row_gap: 0,
            column_gap: 2,
            show_group_icons: true,
            show_command_titles: true,
            fallback_label: "Action".into(),
            border: BorderStyle::Rounded,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentationPopupConfig {
    pub enabled: bool,
    pub max_width: u16,
    pub max_height: u16,
    pub horizontal_padding: u16,
    pub scroll_step: usize,
    pub maximum_sections: usize,
    pub maximum_lines: usize,
    pub maximum_bytes: usize,
    pub preferred_below: bool,
    pub border: BorderStyle,
}

impl Default for DocumentationPopupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_width: 72,
            max_height: 18,
            horizontal_padding: 1,
            scroll_step: 4,
            maximum_sections: 16,
            maximum_lines: 512,
            maximum_bytes: 64 * 1024,
            preferred_below: true,
            border: BorderStyle::Rounded,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalPanelConfig {
    pub height: u16,
    pub minimum_height: u16,
    pub maximum_height: u16,
    pub minimum_workspace_height: u16,
    pub resize_step: u16,
    pub divider: String,
    pub title_format: String,
    pub tab_separator: String,
    pub input_mode_label: String,
    pub normal_mode_label: String,
    pub visual_mode_label: String,
}

impl Default for TerminalPanelConfig {
    fn default() -> Self {
        Self {
            height: 12,
            minimum_height: 4,
            maximum_height: 40,
            minimum_workspace_height: 3,
            resize_step: 2,
            divider: "─".into(),
            title_format: "{id}: {profile}".into(),
            tab_separator: " │ ".into(),
            input_mode_label: "TERMINAL".into(),
            normal_mode_label: "NORMAL".into(),
            visual_mode_label: "VISUAL".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UiConfig {
    pub theme: String,
    pub color_mode: ColorMode,
    pub mouse: bool,
    pub alternate_screen: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub keep_cursor_visible_on_resize: bool,
    pub command_line: CommandLineConfig,
    pub cursor: CursorConfig,
    pub status_line: StatusLineConfig,
    pub buffer_line: BufferLineConfig,
    pub tab_line: TabLineConfig,
    pub panes: PaneConfig,
    pub notifications: NotificationConfig,
    pub lsp_progress: LspProgressConfig,
    pub key_hints: KeyHintConfig,
    pub documentation_popup: DocumentationPopupConfig,
    pub terminal_panel: TerminalPanelConfig,
    pub tool_manager: ToolManagerConfig,
    pub picker: PickerConfig,
    pub explorer: ExplorerConfig,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "dust".into(),
            color_mode: ColorMode::Auto,
            mouse: true,
            alternate_screen: true,
            bracketed_paste: true,
            focus_events: true,
            keep_cursor_visible_on_resize: true,
            command_line: CommandLineConfig::default(),
            cursor: CursorConfig::default(),
            status_line: StatusLineConfig::default(),
            buffer_line: BufferLineConfig::default(),
            tab_line: TabLineConfig::default(),
            panes: PaneConfig::default(),
            notifications: NotificationConfig::default(),
            lsp_progress: LspProgressConfig::default(),
            key_hints: KeyHintConfig::default(),
            documentation_popup: DocumentationPopupConfig::default(),
            terminal_panel: TerminalPanelConfig::default(),
            tool_manager: ToolManagerConfig::default(),
            picker: PickerConfig::default(),
            explorer: ExplorerConfig::default(),
        }
    }
}
