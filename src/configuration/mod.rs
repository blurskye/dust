#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    hash::{Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThemeColor {
    Reset,
    Black,
    DarkGrey,
    Red,
    DarkRed,
    Green,
    DarkGreen,
    Yellow,
    DarkYellow,
    Blue,
    DarkBlue,
    Magenta,
    DarkMagenta,
    Cyan,
    DarkCyan,
    White,
    Grey,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl ThemeColor {
    fn parse(value: &str) -> Result<Self, String> {
        let named = match value {
            "reset" => Self::Reset,
            "black" => Self::Black,
            "dark_grey" => Self::DarkGrey,
            "red" => Self::Red,
            "dark_red" => Self::DarkRed,
            "green" => Self::Green,
            "dark_green" => Self::DarkGreen,
            "yellow" => Self::Yellow,
            "dark_yellow" => Self::DarkYellow,
            "blue" => Self::Blue,
            "dark_blue" => Self::DarkBlue,
            "magenta" => Self::Magenta,
            "dark_magenta" => Self::DarkMagenta,
            "cyan" => Self::Cyan,
            "dark_cyan" => Self::DarkCyan,
            "white" => Self::White,
            "grey" => Self::Grey,
            _ if value.starts_with("indexed:") => {
                let index = value[8..]
                    .parse::<u8>()
                    .map_err(|_| format!("invalid indexed color `{value}`"))?;
                Self::Indexed(index)
            }
            _ if value.starts_with('#') && value.len() == 7 => {
                let red = u8::from_str_radix(&value[1..3], 16)
                    .map_err(|_| format!("invalid RGB color `{value}`"))?;
                let green = u8::from_str_radix(&value[3..5], 16)
                    .map_err(|_| format!("invalid RGB color `{value}`"))?;
                let blue = u8::from_str_radix(&value[5..7], 16)
                    .map_err(|_| format!("invalid RGB color `{value}`"))?;
                Self::Rgb(red, green, blue)
            }
            _ => return Err(format!("unknown theme color `{value}`")),
        };
        Ok(named)
    }

    fn notation(self) -> String {
        match self {
            Self::Reset => "reset".into(),
            Self::Black => "black".into(),
            Self::DarkGrey => "dark_grey".into(),
            Self::Red => "red".into(),
            Self::DarkRed => "dark_red".into(),
            Self::Green => "green".into(),
            Self::DarkGreen => "dark_green".into(),
            Self::Yellow => "yellow".into(),
            Self::DarkYellow => "dark_yellow".into(),
            Self::Blue => "blue".into(),
            Self::DarkBlue => "dark_blue".into(),
            Self::Magenta => "magenta".into(),
            Self::DarkMagenta => "dark_magenta".into(),
            Self::Cyan => "cyan".into(),
            Self::DarkCyan => "dark_cyan".into(),
            Self::White => "white".into(),
            Self::Grey => "grey".into(),
            Self::Indexed(index) => format!("indexed:{index}"),
            Self::Rgb(red, green, blue) => format!("#{red:02x}{green:02x}{blue:02x}"),
        }
    }
}

impl<'de> Deserialize<'de> for ThemeColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

impl Serialize for ThemeColor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.notation())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeAttribute {
    Bold,
    Dim,
    Italic,
    Underlined,
    Reverse,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeStyle {
    pub foreground: ThemeColor,
    pub background: ThemeColor,
    #[serde(default)]
    pub foreground_256: Option<ThemeColor>,
    #[serde(default)]
    pub background_256: Option<ThemeColor>,
    #[serde(default)]
    pub foreground_16: Option<ThemeColor>,
    #[serde(default)]
    pub background_16: Option<ThemeColor>,
    #[serde(default)]
    pub attributes: Vec<ThemeAttribute>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeStyles {
    pub default: ThemeStyle,
    pub muted: ThemeStyle,
    pub accent: ThemeStyle,
    pub status: ThemeStyle,
    pub status_accent: ThemeStyle,
    pub selection: ThemeStyle,
    pub search: ThemeStyle,
    pub error: ThemeStyle,
    pub border: ThemeStyle,
    pub syntax_comment: ThemeStyle,
    pub syntax_string: ThemeStyle,
    pub syntax_number: ThemeStyle,
    pub syntax_keyword: ThemeStyle,
    pub syntax_type: ThemeStyle,
    pub syntax_variable: ThemeStyle,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    pub name: String,
    pub styles: ThemeStyles,
}

impl ThemeConfig {
    fn validate(&self) -> Result<(), String> {
        let styles = [
            ("default", &self.styles.default),
            ("muted", &self.styles.muted),
            ("accent", &self.styles.accent),
            ("status", &self.styles.status),
            ("status_accent", &self.styles.status_accent),
            ("selection", &self.styles.selection),
            ("search", &self.styles.search),
            ("error", &self.styles.error),
            ("border", &self.styles.border),
            ("syntax_comment", &self.styles.syntax_comment),
            ("syntax_string", &self.styles.syntax_string),
            ("syntax_number", &self.styles.syntax_number),
            ("syntax_keyword", &self.styles.syntax_keyword),
            ("syntax_type", &self.styles.syntax_type),
            ("syntax_variable", &self.styles.syntax_variable),
        ];
        for (name, style) in styles {
            if style.foreground == style.background && style.foreground != ThemeColor::Reset {
                return Err(format!(
                    "theme style `{name}` has identical foreground and background"
                ));
            }
            for (channel, primary, fallback_256, fallback_16) in [
                (
                    "foreground",
                    style.foreground,
                    style.foreground_256,
                    style.foreground_16,
                ),
                (
                    "background",
                    style.background,
                    style.background_256,
                    style.background_16,
                ),
            ] {
                if matches!(primary, ThemeColor::Rgb(..)) && fallback_256.is_none() {
                    return Err(format!(
                        "theme style `{name}` {channel} requires a 256-color fallback"
                    ));
                }
                if matches!(primary, ThemeColor::Rgb(..) | ThemeColor::Indexed(_))
                    && fallback_16.is_none()
                {
                    return Err(format!(
                        "theme style `{name}` {channel} requires a 16-color fallback"
                    ));
                }
                if fallback_256.is_some_and(|color| matches!(color, ThemeColor::Rgb(..))) {
                    return Err(format!(
                        "theme style `{name}` {channel}_256 cannot be truecolor"
                    ));
                }
                if fallback_16.is_some_and(|color| {
                    matches!(color, ThemeColor::Rgb(..) | ThemeColor::Indexed(_))
                }) {
                    return Err(format!(
                        "theme style `{name}` {channel}_16 must use a named ANSI color"
                    ));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorMode {
    Auto,
    Ansi16,
    Ansi256,
    TrueColor,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    Default,
    Block,
    Bar,
    Underline,
    BlinkingBlock,
    BlinkingBar,
    BlinkingUnderline,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguousWidth {
    #[default]
    Narrow,
    Wide,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineNumbers {
    Absolute,
    #[default]
    Relative,
    Hybrid,
    Off,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineEndingPolicy {
    #[default]
    Preserve,
    Lf,
    CrLf,
    Cr,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalNewlinePolicy {
    #[default]
    Preserve,
    Insert,
    Remove,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BomPolicy {
    #[default]
    Preserve,
    Always,
    Never,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegisterStorage {
    Internal,
    Disabled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterConfig {
    pub storage: RegisterStorage,
    pub mirror_named_to_unnamed: bool,
}

impl Default for RegisterConfig {
    fn default() -> Self {
        Self {
            storage: RegisterStorage::Internal,
            mirror_named_to_unnamed: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LargeFileConfig {
    pub enabled: bool,
    pub byte_threshold: usize,
    pub line_threshold: usize,
    pub disable_syntax: bool,
    pub disable_diagnostics: bool,
    pub disable_wrap: bool,
    pub status_label: String,
}
impl Default for LargeFileConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            byte_threshold: 16 * 1024 * 1024,
            line_threshold: 250_000,
            disable_syntax: true,
            disable_diagnostics: true,
            disable_wrap: true,
            status_label: "LARGE".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditorConfig {
    pub line_numbers: LineNumbers,
    pub tab_width: usize,
    pub indent_width: usize,
    pub expand_tabs: bool,
    pub undo_break_on_text_commit: bool,
    pub wrap: bool,
    pub buffer_navigation_wrap: bool,
    pub end_of_buffer_marker: String,
    pub half_page_percent: u8,
    pub full_page_percent: u8,
    pub line_endings: LineEndingPolicy,
    pub final_newline: FinalNewlinePolicy,
    pub utf8_bom: BomPolicy,
    pub line_number_separator: String,
    pub gutter_padding: u16,
    pub control_character_replacement: String,
    pub zero_width_replacement: String,
    pub ambiguous_width: AmbiguousWidth,
    pub registers: RegisterConfig,
    pub large_file: LargeFileConfig,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            line_numbers: LineNumbers::Relative,
            tab_width: 4,
            indent_width: 4,
            expand_tabs: true,
            undo_break_on_text_commit: false,
            wrap: false,
            buffer_navigation_wrap: true,
            end_of_buffer_marker: "~".into(),
            half_page_percent: 50,
            full_page_percent: 100,
            line_endings: LineEndingPolicy::Preserve,
            final_newline: FinalNewlinePolicy::Preserve,
            utf8_bom: BomPolicy::Preserve,
            line_number_separator: " ".into(),
            gutter_padding: 1,
            control_character_replacement: "�".into(),
            zero_width_replacement: "◌".into(),
            ambiguous_width: AmbiguousWidth::Narrow,
            registers: RegisterConfig::default(),
            large_file: LargeFileConfig::default(),
        }
    }
}

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
            workspace_trust_title: "Confirm workspace trust".into(),
            workspace_trust_marker: "> ".into(),
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
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NotificationConfig {
    pub left: u16,
    pub rows_from_bottom: u16,
    pub max_width: u16,
    pub timeout_ms: u64,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        Self {
            left: 1,
            rows_from_bottom: 2,
            max_width: 120,
            timeout_ms: 0,
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
    pub installed_heading: String,
    pub available_heading: String,
    pub installing_label: String,
    pub installed_label: String,
    pub available_label: String,
    pub unavailable_installer_label: String,
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
            title: "Dust tools".into(),
            help: "j/k move  Tab category  Enter install/start  r refresh  Esc close".into(),
            installed_heading: "Installed".into(),
            available_heading: "Available".into(),
            installing_label: "installing".into(),
            installed_label: "installed".into(),
            available_label: "available".into(),
            unavailable_installer_label: "manual install required".into(),
            categories: [
                ("all".into(), "All".into()),
                ("lsp".into(), "LSP".into()),
                ("dap".into(), "DAP".into()),
                ("linter".into(), "Linter".into()),
                ("formatter".into(), "Formatter".into()),
            ]
            .into_iter()
            .collect(),
            category_order: vec![
                "all".into(),
                "lsp".into(),
                "dap".into(),
                "linter".into(),
                "formatter".into(),
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
}

impl Default for PickerConfig {
    fn default() -> Self {
        Self {
            width_percent: 76,
            height_percent: 70,
            minimum_width: 40,
            minimum_height: 10,
            border: BorderStyle::Single,
            selected_marker: ">".into(),
            row_marker: " ".into(),
            source_separator: " · ".into(),
            loading_label: "loading…".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExplorerConfig {
    pub width_percent: u8,
    pub height_percent: u8,
    pub minimum_width: u16,
    pub minimum_height: u16,
    pub indent_width: usize,
    pub maximum_visible_rows: usize,
    pub preview_enabled: bool,
    pub maximum_preview_bytes: usize,
    pub maximum_preview_lines: usize,
    pub selected_marker: String,
    pub row_marker: String,
    pub border: BorderStyle,
}

impl Default for ExplorerConfig {
    fn default() -> Self {
        Self {
            width_percent: 22,
            height_percent: 100,
            minimum_width: 28,
            minimum_height: 10,
            indent_width: 2,
            maximum_visible_rows: 200,
            preview_enabled: false,
            maximum_preview_bytes: 256 * 1024,
            maximum_preview_lines: 20,
            selected_marker: ">".into(),
            row_marker: " ".into(),
            border: BorderStyle::Single,
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
    pub tab_line: TabLineConfig,
    pub panes: PaneConfig,
    pub notifications: NotificationConfig,
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
            tab_line: TabLineConfig::default(),
            panes: PaneConfig::default(),
            notifications: NotificationConfig::default(),
            tool_manager: ToolManagerConfig::default(),
            picker: PickerConfig::default(),
            explorer: ExplorerConfig::default(),
        }
    }
}

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
pub struct InputConfig {
    pub mapping_timeout_ms: u64,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub event_poll_ms: u64,
    pub vfs_queue_capacity: usize,
    pub macro_recursion_limit: usize,
    pub macro_step_limit: usize,
    pub repeat_step_limit: usize,
    pub completion_limit: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            event_poll_ms: 16,
            vfs_queue_capacity: 8,
            macro_recursion_limit: 16,
            macro_step_limit: 10_000,
            repeat_step_limit: 10_000,
            completion_limit: 100,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryStreamConfig {
    pub enabled: bool,
    pub maximum_entries: usize,
}

impl Default for HistoryStreamConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            maximum_entries: 100,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryConfig {
    pub command: HistoryStreamConfig,
    pub search: HistoryStreamConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchConfig {
    pub smart_case: bool,
    pub regex: bool,
    pub wrap: bool,
    pub highlight: bool,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            smart_case: true,
            regex: false,
            wrap: true,
            highlight: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryConfig {
    pub enabled: bool,
    pub maximum_discovered_records: usize,
    pub write_debounce_ms: u64,
    pub delete_clean_journals: bool,
    pub cleanup_enabled: bool,
    pub maximum_retained_records: usize,
    /// Zero disables age-based cleanup.
    pub maximum_age_days: u64,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            maximum_discovered_records: 100,
            write_debounce_ms: 250,
            delete_clean_journals: true,
            cleanup_enabled: true,
            maximum_retained_records: 100,
            maximum_age_days: 30,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigWatchConfig {
    pub enabled: bool,
    pub poll_interval_ms: u64,
    pub debounce_ms: u64,
}

impl Default for ConfigWatchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval_ms: 250,
            debounce_ms: 100,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    pub maximum_diagnostics: usize,
    pub maximum_diagnostics_per_owner_uri: usize,
    pub maximum_picker_items: usize,
    pub picker_wrap: bool,
    pub trust_confirmation_phrase: String,
    pub trust_state_queue_capacity: usize,
    pub search: WorkspaceSearchConfig,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            maximum_diagnostics: 10_000,
            maximum_diagnostics_per_owner_uri: 2_000,
            maximum_picker_items: 10_000,
            picker_wrap: true,
            trust_confirmation_phrase: "trust this workspace".into(),
            trust_state_queue_capacity: 8,
            search: WorkspaceSearchConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSearchConfig {
    pub regex: bool,
    pub smart_case: bool,
    pub whole_word: bool,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub include_hidden: bool,
    pub include_ignored: bool,
    pub maximum_files: usize,
    pub maximum_results: usize,
    pub maximum_file_bytes: u64,
    pub maximum_total_bytes: u64,
    pub maximum_preview_bytes: usize,
    pub batch_size: usize,
    pub event_queue_capacity: usize,
}

impl Default for WorkspaceSearchConfig {
    fn default() -> Self {
        Self {
            regex: false,
            smart_case: true,
            whole_word: false,
            include_globs: Vec::new(),
            exclude_globs: vec!["target/**".into(), ".git/**".into()],
            include_hidden: false,
            include_ignored: false,
            maximum_files: 100_000,
            maximum_results: 10_000,
            maximum_file_bytes: 8 * 1024 * 1024,
            maximum_total_bytes: 512 * 1024 * 1024,
            maximum_preview_bytes: 4_096,
            batch_size: 128,
            event_queue_capacity: 8,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LspConfig {
    pub enabled: bool,
    pub request_timeout_ms: u64,
    pub restart_limit: u8,
    pub restart_backoff_ms: u64,
    pub maximum_log_lines: usize,
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            request_timeout_ms: 10_000,
            restart_limit: 3,
            restart_backoff_ms: 250,
            maximum_log_lines: 200,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SyntaxConfig {
    pub enabled: bool,
    pub queue_capacity: usize,
    pub debounce_ms: u64,
    pub maximum_source_bytes: usize,
    pub maximum_spans: usize,
    pub maximum_folds: usize,
    pub maximum_indent_rows: usize,
}
impl Default for SyntaxConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            queue_capacity: 8,
            debounce_ms: 75,
            maximum_source_bytes: 8 * 1024 * 1024,
            maximum_spans: 100_000,
            maximum_folds: 10_000,
            maximum_indent_rows: 100_000,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PersistenceConfig {
    pub enabled: bool,
    pub restore_on_startup: bool,
    pub save_on_exit: bool,
    pub state_file: String,
    pub maximum_file_bytes: usize,
    pub maximum_entries: usize,
    pub recent_files: usize,
    pub marks: usize,
    pub jumps: usize,
    pub changes: usize,
    pub persist_command_history: bool,
    pub persist_search_history: bool,
    pub queue_capacity: usize,
}
impl Default for PersistenceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            restore_on_startup: true,
            save_on_exit: true,
            state_file: "session.json".into(),
            maximum_file_bytes: 4 * 1024 * 1024,
            maximum_entries: 10_000,
            recent_files: 256,
            marks: 256,
            jumps: 1024,
            changes: 1024,
            persist_command_history: false,
            persist_search_history: false,
            queue_capacity: 4,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessConfig {
    pub event_capacity: usize,
    pub maximum_output_bytes: usize,
    pub maximum_line_bytes: usize,
    pub maximum_stdin_bytes: usize,
    pub maximum_timeout_ms: u64,
    pub maximum_running: usize,
}
impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            event_capacity: 256,
            maximum_output_bytes: 16 * 1024 * 1024,
            maximum_line_bytes: 1024 * 1024,
            maximum_stdin_bytes: 16 * 1024 * 1024,
            maximum_timeout_ms: 300_000,
            maximum_running: 16,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    #[default]
    Task,
    Formatter,
    Linter,
    Test,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProblemMatcherConfig {
    pub pattern: String,
    pub path_group: String,
    pub line_group: String,
    pub column_group: Option<String>,
    pub severity_group: Option<String>,
    pub message_group: String,
    #[serde(default)]
    pub name_group: Option<String>,
    #[serde(default)]
    pub status_group: Option<String>,
    #[serde(default)]
    pub duration_group: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TaskConfig {
    #[serde(default)]
    pub kind: TaskKind,
    pub executable: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub stdin: bool,
    pub matcher: Option<ProblemMatcherConfig>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservabilityConfig {
    pub enabled: bool,
    pub file_name: String,
    pub maximum_records: usize,
    pub maximum_field_bytes: usize,
    pub maximum_file_bytes: u64,
    pub rotations: usize,
    pub queue_capacity: usize,
    pub sensitive_keys: Vec<String>,
}
impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            file_name: "dust.jsonl".into(),
            maximum_records: 2000,
            maximum_field_bytes: 4096,
            maximum_file_bytes: 8 * 1024 * 1024,
            rotations: 3,
            queue_capacity: 256,
            sensitive_keys: vec![
                "token".into(),
                "password".into(),
                "secret".into(),
                "authorization".into(),
                "source".into(),
                "text".into(),
            ],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRegistryConfig {
    pub base_url: String,
    pub offline: bool,
    pub redirect_limit: u8,
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub maximum_metadata_bytes: usize,
    pub maximum_artifact_bytes: usize,
    pub maximum_cache_bytes: u64,
    pub maximum_cache_entries: usize,
    pub maximum_search_results: usize,
    pub queue_capacity: usize,
}
impl Default for PackageRegistryConfig {
    fn default() -> Self {
        Self {
            base_url: "https://dust.blurskye.dev/registry/v1".into(),
            offline: false,
            redirect_limit: 0,
            connect_timeout_ms: 5_000,
            request_timeout_ms: 30_000,
            maximum_metadata_bytes: 1024 * 1024,
            maximum_artifact_bytes: 64 * 1024 * 1024,
            maximum_cache_bytes: 512 * 1024 * 1024,
            maximum_cache_entries: 4096,
            maximum_search_results: 100,
            queue_capacity: 8,
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackagesConfig {
    pub registry: PackageRegistryConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEntryConfig {
    pub manifest: PathBuf,
    pub component: PathBuf,
    pub enabled: bool,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginsConfig {
    pub store: PathBuf,
    pub queue_capacity: usize,
    pub maximum_component_bytes: usize,
    pub maximum_commands: usize,
    pub fuel: u64,
    pub epoch_deadline_ticks: u64,
    pub maximum_memory_bytes: usize,
    pub maximum_table_elements: u32,
    pub maximum_instances: u32,
    #[serde(default)]
    pub entries: BTreeMap<String, PluginEntryConfig>,
}
impl Default for PluginsConfig {
    fn default() -> Self {
        Self {
            store: PathBuf::from("packages/store"),
            queue_capacity: 8,
            maximum_component_bytes: 64 * 1024 * 1024,
            maximum_commands: 256,
            fuel: 10_000_000,
            epoch_deadline_ticks: 10,
            maximum_memory_bytes: 64 * 1024 * 1024,
            maximum_table_elements: 10_000,
            maximum_instances: 16,
            entries: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DebugAdapterConfig {
    pub program: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugRequestKind {
    Launch,
    Attach,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DebugSessionConfig {
    pub adapter: String,
    pub request: DebugRequestKind,
    pub cwd: Option<PathBuf>,
    pub program: Option<PathBuf>,
    #[serde(default)]
    pub run_in_terminal_profile: Option<String>,
    #[serde(default)]
    pub arguments: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub path_maps: BTreeMap<PathBuf, PathBuf>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DebugConfig {
    pub maximum_arguments: usize,
    pub maximum_environment: usize,
    pub maximum_breakpoints: usize,
    pub maximum_watches: usize,
    #[serde(default)]
    pub adapters: BTreeMap<String, DebugAdapterConfig>,
    #[serde(default)]
    pub sessions: BTreeMap<String, DebugSessionConfig>,
}
impl Default for DebugConfig {
    fn default() -> Self {
        Self {
            maximum_arguments: 4096,
            maximum_environment: 4096,
            maximum_breakpoints: 10000,
            maximum_watches: 1000,
            adapters: BTreeMap::new(),
            sessions: BTreeMap::new(),
        }
    }
}

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
            profiles: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "current_config_schema")]
    pub schema_version: u32,
    pub editor: EditorConfig,
    pub ui: UiConfig,
    pub startup: StartupConfig,
    pub input: InputConfig,
    pub search: SearchConfig,
    pub history: HistoryConfig,
    pub recovery: RecoveryConfig,
    pub config_watch: ConfigWatchConfig,
    pub runtime: RuntimeConfig,
    pub lsp: LspConfig,
    pub observability: ObservabilityConfig,
    pub persistence: PersistenceConfig,
    pub processes: ProcessConfig,
    #[serde(default)]
    pub packages: PackagesConfig,
    pub plugins: PluginsConfig,
    pub debug: DebugConfig,
    #[serde(default)]
    pub tasks: BTreeMap<String, TaskConfig>,
    pub syntax: SyntaxConfig,
    pub terminals: TerminalsConfig,
    pub workspace: WorkspaceConfig,
    pub messages: BTreeMap<String, String>,
    pub command_labels: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KeymapEntry {
    #[serde(default = "default_editor_context")]
    pub context: String,
    #[serde(default)]
    pub mode: String,
    pub keys: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub arguments: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub unmap: bool,
    #[serde(default)]
    pub disable: bool,
    #[serde(default)]
    pub pass_through: bool,
}

fn task_graph_has_cycle(tasks: &BTreeMap<String, TaskConfig>) -> bool {
    fn visit(
        name: &str,
        tasks: &BTreeMap<String, TaskConfig>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> bool {
        if visited.contains(name) {
            return false;
        }
        if !visiting.insert(name.to_owned()) {
            return true;
        }
        if tasks.get(name).is_some_and(|task| {
            task.dependencies
                .iter()
                .any(|dependency| visit(dependency, tasks, visiting, visited))
        }) {
            return true;
        }
        visiting.remove(name);
        visited.insert(name.to_owned());
        false
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    tasks
        .keys()
        .any(|name| visit(name, tasks, &mut visiting, &mut visited))
}

const fn current_config_schema() -> u32 {
    crate::api::CONFIG_SCHEMA_VERSION
}
pub fn migrate_config_source(source: &str) -> Result<(String, bool), String> {
    let mut document = source
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| error.to_string())?;
    let version = document
        .get("schema_version")
        .and_then(toml_edit::Item::as_integer)
        .unwrap_or(0);
    if version < 0
        || u32::try_from(version)
            .ok()
            .is_none_or(|version| version > crate::api::CONFIG_SCHEMA_VERSION)
    {
        return Err(format!("unsupported future configuration schema {version}"));
    }
    let changed = version != i64::from(crate::api::CONFIG_SCHEMA_VERSION);
    if changed {
        document["schema_version"] = toml_edit::value(i64::from(crate::api::CONFIG_SCHEMA_VERSION));
    }
    Ok((document.to_string(), changed))
}

fn default_editor_context() -> String {
    "editor".into()
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardActionConfig {
    pub command: String,
    pub label: String,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default = "default_true")]
    pub show_binding: bool,
    #[serde(default)]
    pub arguments: BTreeMap<String, toml::Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardConfig {
    pub show_identity: bool,
    pub show_version: bool,
    pub show_path: bool,
    pub identity: String,
    pub version_format: String,
    pub resize_message: String,
    pub unbound_label: String,
    pub key_format: String,
    pub content_width: u16,
    pub minimum_width: u16,
    pub minimum_height: u16,
    pub block_height: u16,
    pub identity_row: u16,
    pub version_row: u16,
    pub path_row: u16,
    pub actions_row: u16,
    pub action_key_width: u16,
    pub action_left_offset: u16,
    pub compact_left: u16,
    pub compact_top: u16,
    #[serde(default)]
    pub action: Vec<DashboardActionConfig>,
}

impl DashboardConfig {
    fn validate(&self) -> Result<(), String> {
        if self.content_width == 0
            || self.minimum_width == 0
            || self.minimum_height == 0
            || self.block_height == 0
            || self.action_key_width == 0
        {
            return Err("dashboard dimensions must be positive".into());
        }
        if !self.version_format.contains("{version}") {
            return Err("dashboard version_format must contain `{version}`".into());
        }
        if !self.key_format.contains("{key}") {
            return Err("dashboard key_format must contain `{key}`".into());
        }
        if self.content_width > 1_000
            || self.block_height > 1_000
            || self.action.len() > 1_000
            || self
                .identity
                .chars()
                .chain(self.resize_message.chars())
                .any(char::is_control)
        {
            return Err("dashboard presentation exceeds safe bounds".into());
        }
        Ok(())
    }
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            show_identity: true,
            show_version: true,
            show_path: true,
            identity: "D U S T".into(),
            version_format: "v{version}".into(),
            resize_message: "Dust — resize terminal".into(),
            unbound_label: "unbound".into(),
            key_format: "[{key}]".into(),
            content_width: 70,
            minimum_width: 20,
            minimum_height: 7,
            block_height: 9,
            identity_row: 0,
            version_row: 1,
            path_row: 3,
            actions_row: 5,
            action_key_width: 12,
            action_left_offset: 2,
            compact_left: 1,
            compact_top: 1,
            action: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadedKeymaps {
    pub entries: Vec<KeymapEntry>,
    pub user_spans: Vec<std::ops::Range<usize>>,
    pub bundled_count: usize,
    pub path: PathBuf,
    pub diagnostic: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadedConfig {
    pub user_configuration: bool,
    pub user_setting_keys: BTreeSet<String>,
    pub user_setting_spans: BTreeMap<String, std::ops::Range<usize>>,
    pub config: Config,
    pub path: PathBuf,
    pub diagnostic: Option<String>,
    pub keymaps: LoadedKeymaps,
    pub dashboard: DashboardConfig,
    pub dashboard_path: PathBuf,
    pub dashboard_spans: BTreeMap<String, std::ops::Range<usize>>,
    pub theme: ThemeConfig,
    pub theme_path: PathBuf,
    pub theme_spans: BTreeMap<String, std::ops::Range<usize>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfigExplanation {
    pub key: String,
    pub value: String,
    pub source: String,
    pub contributors: Vec<String>,
}

pub(crate) fn config_key_segments(key: &str) -> Option<Vec<String>> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in key.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if quoted && character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == '.' && !quoted {
            if current.is_empty() {
                return None;
            }
            segments.push(std::mem::take(&mut current));
        } else {
            current.push(character);
        }
    }
    if quoted || escaped || current.is_empty() {
        return None;
    }
    segments.push(current);
    Some(segments)
}

impl LoadedConfig {
    #[must_use]
    pub fn explain(&self, key: &str) -> Option<ConfigExplanation> {
        let segments = config_key_segments(key)?;
        let canonical = segments.join(".");
        let mut value = toml::Value::try_from(&self.config).ok()?;
        for segment in &segments {
            value = value.get(segment)?.clone();
        }
        let bundled = !self.user_setting_keys.contains(&canonical);
        let user_source = self.user_setting_spans.get(&canonical).map_or_else(
            || self.path.display().to_string(),
            |span| format!("{}:bytes {}..{}", self.path.display(), span.start, span.end),
        );
        Some(ConfigExplanation {
            key: key.into(),
            value: value.to_string(),
            source: if bundled {
                "bundled defaults".into()
            } else {
                user_source.clone()
            },
            contributors: if bundled {
                vec!["bundled defaults".into()]
            } else {
                vec!["bundled defaults".into(), user_source]
            },
        })
    }
}

impl Config {
    fn validate(&self) -> Result<(), &'static str> {
        if !(1..=32).contains(&self.editor.tab_width) {
            return Err("editor.tab_width must be between 1 and 32");
        }
        if !(1..=32).contains(&self.editor.indent_width) {
            return Err("editor.indent_width must be between 1 and 32");
        }
        if self
            .input
            .disabled_command_aliases
            .iter()
            .any(|alias| alias.is_empty() || alias.chars().any(char::is_whitespace))
        {
            return Err("input.disabled_command_aliases contains an invalid alias");
        }
        // This ceiling is a correctness/resource invariant, not product policy.
        if !(1..=1_000_000).contains(&self.input.count_limit) {
            return Err("input.count_limit must be between 1 and 1000000");
        }
        if !(10..=10_000).contains(&self.input.mapping_timeout_ms) {
            return Err("input.mapping_timeout_ms must be between 10 and 10000");
        }
        if !(20..=240).contains(&self.ui.command_line.width) {
            return Err("ui.command_line.width must be between 20 and 240");
        }
        if self.ui.command_line.top_rows > 100 {
            return Err("ui.command_line.top_rows must not exceed 100");
        }
        if self.ui.command_line.minimum_width > self.ui.command_line.width {
            return Err("ui.command_line.minimum_width must not exceed width");
        }
        if self.ui.command_line.outer_margin > 20 {
            return Err("ui.command_line.outer_margin must not exceed 20");
        }
        if self.ui.command_line.completion_max_rows > 1_000
            || self.ui.command_line.input_padding > 20
            || self.ui.command_line.attachment_gap > 20
            || self.ui.command_line.completion_detail_gap > 40
        {
            return Err("ui.command_line layout values exceed safe bounds");
        }
        if !(1..=400).contains(&self.ui.panes.resize_step) {
            return Err("ui.panes.resize_step must be between 1 and 400");
        }
        if !(30..=100).contains(&self.ui.tool_manager.width_percent)
            || !(30..=100).contains(&self.ui.tool_manager.height_percent)
            || self.ui.tool_manager.minimum_width < 20
            || self.ui.tool_manager.minimum_height < 8
            || self.ui.tool_manager.category_order.is_empty()
            || self
                .ui
                .tool_manager
                .category_order
                .iter()
                .any(|category| !self.ui.tool_manager.categories.contains_key(category))
        {
            return Err("ui.tool_manager layout or category policy is invalid");
        }
        if !(15..=100).contains(&self.ui.explorer.width_percent)
            || !(50..=100).contains(&self.ui.explorer.height_percent)
            || self.ui.explorer.minimum_width < 20
            || self.ui.explorer.minimum_height < 6
            || !(1..=8).contains(&self.ui.explorer.indent_width)
            || !(1..=10_000).contains(&self.ui.explorer.maximum_visible_rows)
            || !(1..=16 * 1024 * 1024).contains(&self.ui.explorer.maximum_preview_bytes)
            || !(1..=1000).contains(&self.ui.explorer.maximum_preview_lines)
        {
            return Err("ui.explorer layout policy is invalid");
        }
        if !(30..=100).contains(&self.ui.picker.width_percent)
            || !(30..=100).contains(&self.ui.picker.height_percent)
            || self.ui.picker.minimum_width < 20
            || self.ui.picker.minimum_height < 6
            || self.ui.picker.selected_marker.chars().any(char::is_control)
            || self.ui.picker.row_marker.chars().any(char::is_control)
        {
            return Err("ui.picker layout or presentation policy is invalid");
        }
        if !(1..=100_000).contains(&self.workspace.maximum_diagnostics)
            || !(1..=100_000).contains(&self.workspace.maximum_diagnostics_per_owner_uri)
            || !(1..=100_000).contains(&self.workspace.maximum_picker_items)
            || !(1..=1_024).contains(&self.workspace.trust_state_queue_capacity)
            || self.workspace.trust_confirmation_phrase.trim().is_empty()
            || self.workspace.trust_confirmation_phrase.len() > 128
            || self
                .workspace
                .trust_confirmation_phrase
                .chars()
                .any(char::is_control)
        {
            return Err("workspace service limits are outside safe bounds");
        }
        if self.schema_version > crate::api::CONFIG_SCHEMA_VERSION {
            return Err("configuration schema is newer than this Dust build");
        }
        if !(1..=4096).contains(&self.debug.maximum_arguments)
            || !(1..=4096).contains(&self.debug.maximum_environment)
            || !(1..=100_000).contains(&self.debug.maximum_breakpoints)
            || !(1..=10_000).contains(&self.debug.maximum_watches)
            || self.debug.adapters.iter().any(|(id, adapter)| {
                id.is_empty()
                    || !adapter.program.is_absolute()
                    || adapter.arguments.len() > self.debug.maximum_arguments
                    || adapter.environment.len() > self.debug.maximum_environment
            })
            || self.debug.sessions.iter().any(|(id, session)| {
                id.is_empty()
                    || !self.debug.adapters.contains_key(&session.adapter)
                    || session.cwd.as_ref().is_some_and(|path| {
                        path.components()
                            .any(|part| matches!(part, std::path::Component::ParentDir))
                    })
                    || session.program.as_ref().is_some_and(|path| {
                        path.components()
                            .any(|part| matches!(part, std::path::Component::ParentDir))
                    })
                    || session
                        .run_in_terminal_profile
                        .as_ref()
                        .is_some_and(|profile| !self.terminals.profiles.contains_key(profile))
                    || session.arguments.len() > self.debug.maximum_arguments
                    || session.path_maps.len() > 1024
            })
        {
            return Err("debug policy is invalid");
        }
        if !(1..=4096).contains(&self.terminals.queue_capacity)
            || !(1..=256 * 1024 * 1024).contains(&self.terminals.maximum_output_bytes)
            || !(1..=16 * 1024 * 1024).contains(&self.terminals.maximum_input_bytes)
            || self.terminals.scrollback_bytes > self.terminals.maximum_output_bytes
            || self.terminals.profiles.iter().any(|(name, profile)| {
                name.is_empty()
                    || !profile.program.is_absolute()
                    || profile.arguments.len() > 4096
                    || profile.columns == 0
                    || profile.rows == 0
            })
        {
            return Err("terminal policy is invalid");
        }
        if self
            .plugins
            .store
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
            || !(1..=64).contains(&self.plugins.queue_capacity)
            || !(1024..=256 * 1024 * 1024).contains(&self.plugins.maximum_component_bytes)
            || !(1..=4096).contains(&self.plugins.maximum_commands)
            || self.plugins.fuel == 0
            || self.plugins.entries.iter().any(|(id, entry)| {
                id.is_empty()
                    || entry.manifest.is_absolute()
                    || entry.component.is_absolute()
                    || entry
                        .manifest
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir))
                    || entry
                        .component
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir))
            })
        {
            return Err("plugin policy is invalid");
        }
        if self.observability.file_name.is_empty()
            || self.observability.file_name.contains('/')
            || !(1..=100_000).contains(&self.observability.maximum_records)
            || !(1..=64 * 1024).contains(&self.observability.maximum_field_bytes)
            || !(1024..=1024 * 1024 * 1024).contains(&self.observability.maximum_file_bytes)
            || !(1..=32).contains(&self.observability.rotations)
            || !(1..=4096).contains(&self.observability.queue_capacity)
        {
            return Err("observability policy is invalid");
        }
        if !(1..=4096).contains(&self.processes.event_capacity)
            || !(1..=256 * 1024 * 1024).contains(&self.processes.maximum_output_bytes)
            || !(1..=16 * 1024 * 1024).contains(&self.processes.maximum_line_bytes)
            || !(1..=256 * 1024 * 1024).contains(&self.processes.maximum_stdin_bytes)
            || !(1..=86_400_000).contains(&self.processes.maximum_timeout_ms)
            || !(1..=256).contains(&self.processes.maximum_running)
            || self.tasks.iter().any(|(name, task)| {
                name.is_empty()
                    || !task.executable.is_absolute()
                    || task.arguments.len() > 4096
                    || task.environment.len() > 4096
                    || task
                        .dependencies
                        .iter()
                        .any(|dependency| !self.tasks.contains_key(dependency))
                    || task
                        .matcher
                        .as_ref()
                        .is_some_and(|matcher| regex::Regex::new(&matcher.pattern).is_err())
                    || matches!(task.kind, TaskKind::Formatter) && !task.stdin
                    || task.timeout_ms.is_some_and(|timeout| {
                        timeout == 0 || timeout > self.processes.maximum_timeout_ms
                    })
            })
            || task_graph_has_cycle(&self.tasks)
        {
            return Err("process or task policy is invalid");
        }
        if self.persistence.state_file.is_empty()
            || self.persistence.state_file.contains('/')
            || self.persistence.state_file.contains('\\')
            || !(1..=64 * 1024 * 1024).contains(&self.persistence.maximum_file_bytes)
            || !(1..=100_000).contains(&self.persistence.maximum_entries)
            || !(1..=64).contains(&self.persistence.queue_capacity)
            || [
                self.persistence.recent_files,
                self.persistence.marks,
                self.persistence.jumps,
                self.persistence.changes,
            ]
            .iter()
            .any(|limit| *limit > self.persistence.maximum_entries)
        {
            return Err("persistence policy is invalid or outside safe bounds");
        }
        if !(1..=1_073_741_824).contains(&self.editor.large_file.byte_threshold)
            || !(1..=10_000_000).contains(&self.editor.large_file.line_threshold)
            || self.editor.large_file.status_label.is_empty()
        {
            return Err("editor.large_file policy is invalid or outside safe bounds");
        }
        if !(1..=1024).contains(&self.syntax.queue_capacity)
            || self.syntax.debounce_ms > 60_000
            || !(1..=268_435_456).contains(&self.syntax.maximum_source_bytes)
            || !(1..=1_000_000).contains(&self.syntax.maximum_spans)
            || !(1..=100_000).contains(&self.syntax.maximum_folds)
            || !(1..=1_000_000).contains(&self.syntax.maximum_indent_rows)
        {
            return Err("syntax policy is invalid or outside safe bounds");
        }
        let workspace_search = &self.workspace.search;
        if !(1..=1_000_000).contains(&workspace_search.maximum_files)
            || !(1..=100_000).contains(&workspace_search.maximum_results)
            || !(1..=268_435_456).contains(&workspace_search.maximum_file_bytes)
            || !(1..=4_294_967_296).contains(&workspace_search.maximum_total_bytes)
            || !(1..=65_536).contains(&workspace_search.maximum_preview_bytes)
            || !(1..=4_096).contains(&workspace_search.batch_size)
            || !(1..=1_024).contains(&workspace_search.event_queue_capacity)
            || workspace_search
                .include_globs
                .iter()
                .chain(&workspace_search.exclude_globs)
                .any(|pattern| globset::Glob::new(pattern).is_err())
        {
            return Err("workspace.search policy is invalid or outside safe bounds");
        }
        if !(100..=900).contains(&self.ui.panes.default_ratio) {
            return Err("ui.panes.default_ratio must be between 100 and 900");
        }
        if self.ui.panes.minimum_width < 3 || self.ui.panes.minimum_height < 2 {
            return Err("ui.panes minimum dimensions are too small");
        }
        if !self.ui.tab_line.active_format.contains("{title}")
            || !self.ui.tab_line.inactive_format.contains("{title}")
        {
            return Err("ui.tab_line formats must contain {title}");
        }
        if self.ui.notifications.timeout_ms > 86_400_000 {
            return Err("ui.notifications.timeout_ms exceeds 24 hours");
        }
        if self.ui.theme.is_empty()
            || !self.ui.theme.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err("ui.theme must be a safe lowercase theme identifier");
        }
        if self
            .editor
            .line_number_separator
            .chars()
            .any(char::is_control)
            || self.editor.line_number_separator.len() > 16
            || self.editor.gutter_padding > 20
            || self.editor.control_character_replacement.chars().count() != 1
            || self.editor.zero_width_replacement.chars().count() != 1
            || self
                .editor
                .control_character_replacement
                .chars()
                .any(char::is_control)
            || self
                .editor
                .zero_width_replacement
                .chars()
                .any(char::is_control)
        {
            return Err("editor gutter presentation is invalid");
        }
        if !(1..=100).contains(&self.editor.half_page_percent)
            || !(1..=200).contains(&self.editor.full_page_percent)
        {
            return Err("editor scroll percentages are outside safe bounds");
        }
        if !(100..=60_000).contains(&self.lsp.request_timeout_ms)
            || self.lsp.restart_limit > 10
            || !(10..=60_000).contains(&self.lsp.restart_backoff_ms)
            || !(10..=10_000).contains(&self.lsp.maximum_log_lines)
        {
            return Err("lsp policy is outside supported safety bounds");
        }
        if !(1..=1_000).contains(&self.runtime.event_poll_ms) {
            return Err("runtime.event_poll_ms must be between 1 and 1000");
        }
        if !(1..=1024).contains(&self.runtime.vfs_queue_capacity) {
            return Err("runtime.vfs_queue_capacity must be between 1 and 1024");
        }
        if !(1..=64).contains(&self.runtime.macro_recursion_limit)
            || !(1..=100_000).contains(&self.runtime.macro_step_limit)
            || !(1..=100_000).contains(&self.runtime.repeat_step_limit)
        {
            return Err("runtime macro limits exceed safe ceilings");
        }
        if !(25..=60_000).contains(&self.config_watch.poll_interval_ms) {
            return Err("config_watch.poll_interval_ms must be between 25 and 60000");
        }
        if self.config_watch.debounce_ms > 60_000 {
            return Err("config_watch.debounce_ms exceeds 60 seconds");
        }
        if self.recovery.maximum_discovered_records > 10_000 {
            return Err("recovery.maximum_discovered_records exceeds safe ceiling");
        }
        if self.recovery.maximum_retained_records > 10_000 {
            return Err("recovery.maximum_retained_records exceeds safe ceiling");
        }
        if self.recovery.maximum_age_days > 36_500 {
            return Err("recovery.maximum_age_days exceeds 100 years");
        }
        if self.recovery.write_debounce_ms > 60_000 {
            return Err("recovery.write_debounce_ms exceeds 60 seconds");
        }
        if self.history.command.maximum_entries > 100_000
            || self.history.search.maximum_entries > 100_000
        {
            return Err("history maximum_entries exceeds safe ceiling");
        }
        let registry = &self.packages.registry;
        if registry.base_url != "https://dust.blurskye.dev/registry/v1"
            || registry.redirect_limit != 0
            || !(100..=30_000).contains(&registry.connect_timeout_ms)
            || !(100..=120_000).contains(&registry.request_timeout_ms)
            || !(1024..=8 * 1024 * 1024).contains(&registry.maximum_metadata_bytes)
            || !(1024..=256 * 1024 * 1024).contains(&registry.maximum_artifact_bytes)
            || registry.maximum_cache_bytes < registry.maximum_artifact_bytes as u64
            || registry.maximum_cache_bytes > 16 * 1024 * 1024 * 1024
            || !(1..=100_000).contains(&registry.maximum_cache_entries)
            || !(1..=1000).contains(&registry.maximum_search_results)
            || !(1..=64).contains(&registry.queue_capacity)
        {
            return Err("official package registry policy is outside source-owned safety bounds");
        }
        if !(1..=100_000).contains(&self.runtime.completion_limit) {
            return Err("runtime.completion_limit is outside safe bounds");
        }
        const REQUIRED_MESSAGES: &[&str] = &[
            "unsaved_quit",
            "new_scratch",
            "macro_recording",
            "macro_recorded",
            "no_macro_recording",
            "config_reload_queued",
            "unknown_config_key",
            "no_keymap_binding",
            "no_keymap_conflicts",
            "healthy",
            "no_previous_change",
            "no_previous_macro",
            "repeat_step_limit",
            "empty_macro",
            "macro_recursion_limit",
            "macro_step_limit",
            "cursor_not_word",
            "opening_file",
            "no_active_reload",
            "dirty_reload",
            "scratch_reload",
            "reloading",
            "dirty_close",
            "returned_dashboard",
            "buffer_closed",
            "no_active_save",
            "scratch_no_name",
            "saving",
            "reload_rejected",
            "auto_reload_rejected",
            "auto_reloaded",
            "reloaded_restart",
            "reloaded",
            "opened",
            "reloaded_file",
            "reload_closed",
            "unexpected_read",
            "saved_dirty",
            "saved",
            "saved_others_dirty",
            "unexpected_completion",
            "search_empty",
            "search_no_buffer",
            "no_previous_search",
            "pattern_not_found",
            "config_explanation",
            "keymap_inspection",
            "keymap_context_summary",
            "startup_fallback",
            "recovery_summary",
            "recovery_opened",
            "recovery_none",
            "recovery_discarded",
            "recovery_pruned",
            "tool_install_started",
            "tool_install_succeeded",
            "tool_install_failed",
            "tool_no_installer",
            "lsp_started",
            "lsp_running",
            "lsp_stopped",
            "lsp_failed",
            "lsp_status",
            "lsp_no_server",
            "lsp_no_buffer",
            "lsp_disabled",
            "lsp_restart_disabled",
            "lsp_diagnostics",
            "lsp_hover",
            "lsp_progress",
            "lsp_health",
            "lsp_unsupported",
            "lsp_request_failed",
            "lsp_completion",
            "lsp_symbols",
            "lsp_code_actions",
            "lsp_no_result",
            "lsp_aux_result",
            "lsp_invalid_uri",
            "lsp_invalid_position",
            "lsp_edit_applied",
            "lsp_logs",
            "diagnostic_none",
            "diagnostic_current",
            "workspace_trusted",
            "workspace_untrusted",
            "workspace_trust_trusted",
            "workspace_trust_untrusted",
            "workspace_trust_status",
            "workspace_trust_confirmation",
            "workspace_trust_confirmation_wrong",
            "workspace_trust_not_ready",
            "workspace_trust_saving",
            "workspace_trust_state_failed",
            "workspace_execution_validating",
            "workspace_execution_validation_busy",
            "workspace_execution_validation_failed",
            "workspace_root_operation_busy",
            "workspace_root_loading",
            "workspace_root_stale",
            "workspace_root_added",
            "workspace_root_add_failed",
            "workspace_root_removed",
            "workspace_root_activated",
            "workspace_root_dirty_blocker",
            "workspace_root_service_blocker",
            "workspace_root_picker_title",
            "workspace_root_active",
            "workspace_root_inactive",
            "workspace_root_row",
            "workspace_root_status",
            "session_save_queued",
            "session_load_queued",
            "history_cleared",
            "large_file_invalid",
            "large_file_changed",
            "large_file_status",
            "task_not_running",
            "task_already_running",
            "task_unknown",
            "task_no_root",
            "task_cwd_outside_root",
            "task_exited",
            "task_failed",
            "task_output",
            "plugin_status",
            "plugin_disabled",
            "previous_crash",
            "crash_location",
            "crash_cleared",
            "terminal_unknown",
            "terminal_outside_root",
            "terminal_title",
            "terminal_list",
            "terminal_missing",
            "formatter_invalid_utf8",
            "formatter_stale",
            "formatter_applied",
            "formatter_truncated",
            "linter_matcher_missing",
            "test_matcher_missing",
            "picker_tasks_title",
            "picker_tests_title",
            "picker_output_title",
            "picker_code_lens_title",
            "picker_lsp_sessions_title",
            "explorer_title",
            "explorer_loading_marker",
            "explorer_expanded_marker",
            "explorer_collapsed_marker",
            "explorer_file_marker",
            "explorer_symlink_marker",
            "explorer_row",
            "workspace_search_started",
            "workspace_search_complete",
            "workspace_search_failed",
            "workspace_search_empty",
            "workspace_search_cancelled",
            "workspace_search_stale_result",
            "workspace_search_title",
            "workspace_search_row",
            "workspace_replace_label",
            "picker_references_title",
            "picker_symbols_title",
            "picker_symbol_kind",
            "picker_code_actions_title",
            "picker_diagnostics_title",
            "picker_buffers_title",
            "picker_commands_title",
            "workspace_edit_lsp_label",
            "workspace_edit_server_label",
            "workspace_edit_busy",
            "workspace_resource_unsupported",
            "workspace_edit_loading",
            "workspace_edit_failed",
            "workspace_edit_included",
            "workspace_edit_excluded",
            "workspace_edit_rejected",
            "workspace_edit_applied",
            "dap_started",
            "dap_running",
            "dap_stopped",
            "dap_output",
            "dap_failed",
            "dap_status",
            "dap_no_adapter",
            "dap_no_buffer",
        ];
        if REQUIRED_MESSAGES
            .iter()
            .any(|key| !self.messages.contains_key(*key))
        {
            return Err("messages table is missing a required current-surface message");
        }
        if self
            .messages
            .values()
            .any(|message| message.contains('\u{1b}') || message.len() > 4_096)
        {
            return Err("messages contain unsafe or oversized text");
        }
        if [
            "normal",
            "insert",
            "replace",
            "visual_char",
            "visual_line",
            "visual_block",
            "operator_pending",
            "command_line",
            "prompt",
        ]
        .iter()
        .any(|mode| !self.ui.status_line.mode_labels.contains_key(*mode))
        {
            return Err("ui.status_line.mode_labels is incomplete");
        }
        if self.ui.status_line.components.iter().any(|component| {
            !matches!(
                component.as_str(),
                "mode" | "file" | "large_file" | "lsp" | "position"
            )
        }) {
            return Err("ui.status_line.components contains an unknown component");
        }
        Ok(())
    }
}

#[must_use]
pub fn user_state_directory() -> PathBuf {
    if let Some(root) = env::var_os("DUST_STATE_HOME") {
        return PathBuf::from(root);
    }
    #[cfg(target_os = "windows")]
    {
        return env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Dust");
    }
    #[cfg(target_os = "macos")]
    {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/Application Support/Dust");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("dust")
    }
}

#[must_use]
pub fn user_config_path() -> PathBuf {
    if let Some(root) = env::var_os("DUST_CONFIG_HOME") {
        return PathBuf::from(root).join("config.toml");
    }
    #[cfg(target_os = "windows")]
    {
        return env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Dust")
            .join("config.toml");
    }
    #[cfg(target_os = "macos")]
    {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Library/Application Support/Dust/config.toml");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("dust/config.toml")
    }
}

#[must_use]
pub fn user_keymaps_path() -> PathBuf {
    user_config_path().with_file_name("keymaps.toml")
}

#[must_use]
pub fn user_dashboard_path() -> PathBuf {
    user_config_path().with_file_name("dashboard.toml")
}

#[must_use]
pub fn user_theme_path(name: &str) -> PathBuf {
    user_config_path()
        .with_file_name("themes")
        .join(format!("{name}.toml"))
}

const BUNDLED_CONFIG: &str = include_str!("../../assets/defaults/config.toml");
const BUNDLED_KEYMAPS: &str = include_str!("../../assets/defaults/keymaps.toml");
const BUNDLED_DASHBOARD: &str = include_str!("../../assets/defaults/dashboard.toml");
const BUNDLED_THEME: &str = include_str!("../../assets/themes/dust.toml");

#[must_use]
pub fn load_bundled_config() -> LoadedConfig {
    let config =
        toml::from_str::<Config>(BUNDLED_CONFIG).expect("bundled config must be covered by tests");
    config
        .validate()
        .expect("bundled config policy must be covered by tests");
    let mut keymaps = parse_keymaps(BUNDLED_KEYMAPS, PathBuf::from("<bundled:keymaps>"));
    keymaps.bundled_count = keymaps.entries.len();
    let dashboard = toml::from_str::<DashboardConfig>(BUNDLED_DASHBOARD)
        .expect("bundled dashboard must be covered by tests");
    dashboard
        .validate()
        .expect("bundled dashboard dimensions must be covered by tests");
    let theme = toml::from_str::<ThemeConfig>(BUNDLED_THEME)
        .expect("bundled theme must be covered by tests");
    theme
        .validate()
        .expect("bundled theme fallbacks must be covered by tests");
    LoadedConfig {
        user_configuration: false,
        user_setting_keys: BTreeSet::new(),
        user_setting_spans: BTreeMap::new(),
        config,
        path: PathBuf::from("<bundled:config>"),
        diagnostic: None,
        keymaps,
        dashboard,
        dashboard_path: PathBuf::from("<bundled:dashboard>"),
        dashboard_spans: BTreeMap::new(),
        theme,
        theme_path: PathBuf::from("<bundled:theme:dust>"),
        theme_spans: BTreeMap::new(),
    }
}

#[derive(Clone, Debug)]
enum CapturedText {
    Missing,
    Text(String),
    Error(String),
}

#[derive(Clone, Debug)]
struct CapturedSource {
    path: PathBuf,
    text: CapturedText,
    identity: SourceFingerprint,
}

#[derive(Clone, Debug)]
struct ConfigurationSourceBundle {
    config: CapturedSource,
    keymaps: CapturedSource,
    dashboard: CapturedSource,
    theme: CapturedSource,
    selected_theme: String,
}

impl ConfigurationSourceBundle {
    fn identity(&self) -> Vec<SourceFingerprint> {
        [&self.config, &self.keymaps, &self.dashboard, &self.theme]
            .into_iter()
            .map(|source| source.identity.clone())
            .collect()
    }

    fn into_loaded(self) -> LoadedConfig {
        let bundled = load_bundled_config();
        let mut loaded = load_config_capture(&self.config);
        loaded.user_configuration = true;
        let user_keymaps = match &self.keymaps.text {
            CapturedText::Missing => LoadedKeymaps {
                path: self.keymaps.path.clone(),
                ..LoadedKeymaps::default()
            },
            CapturedText::Text(source) => parse_keymaps(source, self.keymaps.path.clone()),
            CapturedText::Error(error) => LoadedKeymaps {
                path: self.keymaps.path.clone(),
                diagnostic: Some(format!(
                    "cannot read {}: {error}",
                    self.keymaps.path.display()
                )),
                ..LoadedKeymaps::default()
            },
        };
        let bundled_count = bundled.keymaps.entries.len();
        loaded.keymaps = LoadedKeymaps {
            entries: bundled
                .keymaps
                .entries
                .into_iter()
                .chain(user_keymaps.entries)
                .collect(),
            bundled_count,
            path: user_keymaps.path,
            diagnostic: user_keymaps.diagnostic,
            user_spans: user_keymaps.user_spans,
        };
        match parse_dashboard_capture(&self.dashboard) {
            Ok(Some(dashboard)) => {
                loaded.dashboard_spans = match &self.dashboard.text {
                    CapturedText::Text(source) => setting_spans(source),
                    CapturedText::Missing | CapturedText::Error(_) => BTreeMap::new(),
                };
                loaded.dashboard = dashboard;
                loaded.dashboard_path = self.dashboard.path.clone();
            }
            Ok(None) => {
                loaded.dashboard = bundled.dashboard.clone();
                loaded.dashboard_path = bundled.dashboard_path.clone();
            }
            Err(diagnostic) => {
                loaded.dashboard = bundled.dashboard.clone();
                loaded.dashboard_path = bundled.dashboard_path.clone();
                append_diagnostic(&mut loaded.diagnostic, diagnostic);
            }
        }
        if self.selected_theme == bundled.theme.name {
            loaded.theme = bundled.theme;
            loaded.theme_path = bundled.theme_path;
        } else {
            match parse_theme_capture(&self.theme) {
                Ok(theme) if theme.name == self.selected_theme => {
                    loaded.theme_spans = match &self.theme.text {
                        CapturedText::Text(source) => setting_spans(source),
                        CapturedText::Missing | CapturedText::Error(_) => BTreeMap::new(),
                    };
                    loaded.theme = theme;
                    loaded.theme_path = self.theme.path.clone();
                }
                Ok(theme) => {
                    loaded.theme = bundled.theme;
                    loaded.theme_path = bundled.theme_path;
                    append_diagnostic(
                        &mut loaded.diagnostic,
                        format!(
                            "theme {} declares name `{}` instead of `{}`; using bundled theme",
                            self.theme.path.display(),
                            theme.name,
                            self.selected_theme
                        ),
                    );
                }
                Err(diagnostic) => {
                    loaded.theme = bundled.theme;
                    loaded.theme_path = bundled.theme_path;
                    append_diagnostic(&mut loaded.diagnostic, diagnostic);
                }
            }
        }
        loaded
    }
}

fn capture_configuration_bundle_with(
    theme_hint: &str,
    mut capture: impl FnMut(PathBuf) -> CapturedSource,
) -> ConfigurationSourceBundle {
    let config = capture(user_config_path());
    let selected_theme = load_config_capture(&config).config.ui.theme;
    let keymaps = capture(user_keymaps_path());
    let dashboard = capture(user_dashboard_path());
    let theme_name = if selected_theme.is_empty() {
        theme_hint
    } else {
        &selected_theme
    };
    let theme = capture(user_theme_path(theme_name));
    ConfigurationSourceBundle {
        config,
        keymaps,
        dashboard,
        theme,
        selected_theme,
    }
}

fn capture_configuration_bundle(theme_hint: &str) -> ConfigurationSourceBundle {
    capture_configuration_bundle_with(theme_hint, capture_source)
}

#[must_use]
pub fn load_user_config() -> LoadedConfig {
    match capture_consistent_configuration_bundle("dust") {
        Ok((bundle, _)) => bundle.into_loaded(),
        Err(diagnostic) => {
            let mut loaded = fallback_config(Some(diagnostic));
            loaded.user_configuration = true;
            loaded
        }
    }
}

fn hash_source_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn capture_source(path: PathBuf) -> CapturedSource {
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CapturedSource {
                identity: SourceFingerprint {
                    path: path.clone(),
                    modified: None,
                    length: None,
                    content_hash: None,
                },
                path,
                text: CapturedText::Missing,
            };
        }
        Err(error) => {
            return CapturedSource {
                identity: SourceFingerprint {
                    path: path.clone(),
                    modified: None,
                    length: None,
                    content_hash: None,
                },
                path,
                text: CapturedText::Error(error.to_string()),
            };
        }
    };
    let modified = metadata.modified().ok();
    let length = metadata.len();
    let bytes = fs::File::open(&path).and_then(|file| {
        let mut reader = file.take(WATCH_FINGERPRINT_BYTE_CEILING.saturating_add(1));
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    });
    let (text, content_hash) = match bytes {
        Ok(bytes) => {
            let hash_length = bytes.len().min(WATCH_FINGERPRINT_BYTE_CEILING as usize);
            let hash = Some(hash_source_bytes(&bytes[..hash_length]));
            if length > WATCH_FINGERPRINT_BYTE_CEILING
                || bytes.len() as u64 > WATCH_FINGERPRINT_BYTE_CEILING
            {
                (
                    CapturedText::Error(format!(
                        "configuration source exceeds {} byte ceiling",
                        WATCH_FINGERPRINT_BYTE_CEILING
                    )),
                    hash,
                )
            } else {
                match String::from_utf8(bytes) {
                    Ok(source) => (CapturedText::Text(source), hash),
                    Err(error) => (CapturedText::Error(error.to_string()), hash),
                }
            }
        }
        Err(error) => (CapturedText::Error(error.to_string()), None),
    };
    CapturedSource {
        identity: SourceFingerprint {
            path: path.clone(),
            modified,
            length: Some(length),
            content_hash,
        },
        path,
        text,
    }
}

fn read_configuration_source(path: &Path) -> std::io::Result<String> {
    match capture_source(path.to_path_buf()).text {
        CapturedText::Text(source) => Ok(source),
        CapturedText::Missing => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        CapturedText::Error(error) => {
            Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        }
    }
}

#[must_use]
pub fn load_keymaps(path: &Path) -> LoadedKeymaps {
    let source = match read_configuration_source(path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LoadedKeymaps {
                path: path.to_path_buf(),
                ..LoadedKeymaps::default()
            };
        }
        Err(error) => {
            return LoadedKeymaps {
                path: path.to_path_buf(),
                diagnostic: Some(format!("cannot read {}: {error}", path.display())),
                ..LoadedKeymaps::default()
            };
        }
    };
    parse_keymaps(&source, path.to_path_buf())
}

fn parse_keymaps(source: &str, path: PathBuf) -> LoadedKeymaps {
    #[derive(Deserialize)]
    struct KeymapFile {
        #[serde(default)]
        keymap: Vec<toml::Spanned<KeymapEntry>>,
    }
    match toml::from_str::<KeymapFile>(source) {
        Ok(file) => LoadedKeymaps {
            user_spans: file.keymap.iter().map(toml::Spanned::span).collect(),
            entries: file
                .keymap
                .into_iter()
                .map(toml::Spanned::into_inner)
                .collect(),
            bundled_count: 0,
            path,
            diagnostic: None,
        },
        Err(error) => LoadedKeymaps {
            diagnostic: Some(format!(
                "invalid {}: {error}; ignoring keymaps",
                path.display()
            )),
            path,
            ..LoadedKeymaps::default()
        },
    }
}

fn append_diagnostic(target: &mut Option<String>, diagnostic: String) {
    *target = Some(match target.take() {
        Some(existing) => format!("{existing}; {diagnostic}"),
        None => diagnostic,
    });
}

fn bundled_theme() -> ThemeConfig {
    toml::from_str(BUNDLED_THEME).expect("bundled theme must remain valid")
}

fn parse_theme_capture(captured: &CapturedSource) -> Result<ThemeConfig, String> {
    let source = match &captured.text {
        CapturedText::Text(source) => source,
        CapturedText::Missing => {
            return Err(format!(
                "cannot read theme {}: not found",
                captured.path.display()
            ));
        }
        CapturedText::Error(error) => {
            return Err(format!(
                "cannot read theme {}: {error}",
                captured.path.display()
            ));
        }
    };
    let path = &captured.path;
    let theme = toml::from_str(source).map_err(|error| {
        format!(
            "invalid theme {}: {error}; using bundled theme",
            path.display()
        )
    })?;
    ThemeConfig::validate(&theme).map_err(|error| {
        format!(
            "invalid theme {}: {error}; using bundled theme",
            path.display()
        )
    })?;
    Ok(theme)
}

#[cfg(test)]
fn load_theme(path: &Path) -> Result<ThemeConfig, String> {
    parse_theme_capture(&capture_source(path.to_path_buf()))
}

fn parse_dashboard_capture(captured: &CapturedSource) -> Result<Option<DashboardConfig>, String> {
    let source = match &captured.text {
        CapturedText::Text(source) => source,
        CapturedText::Missing => return Ok(None),
        CapturedText::Error(error) => {
            return Err(format!("cannot read {}: {error}", captured.path.display()));
        }
    };
    let path = &captured.path;
    let dashboard: DashboardConfig = toml::from_str(source).map_err(|error| {
        format!(
            "invalid {}: {error}; using bundled dashboard",
            path.display()
        )
    })?;
    dashboard.validate().map_err(|error| {
        format!(
            "invalid {}: {error}; using bundled dashboard",
            path.display()
        )
    })?;
    Ok(Some(dashboard))
}

fn fallback_config(diagnostic: Option<String>) -> LoadedConfig {
    LoadedConfig {
        user_configuration: false,
        user_setting_keys: BTreeSet::new(),
        user_setting_spans: BTreeMap::new(),
        config: toml::from_str::<Config>(BUNDLED_CONFIG).expect("bundled config must remain valid"),
        path: PathBuf::from("<bundled:config>"),
        diagnostic,
        keymaps: LoadedKeymaps::default(),
        dashboard: DashboardConfig::default(),
        dashboard_path: PathBuf::from("<bundled:dashboard>"),
        dashboard_spans: BTreeMap::new(),
        theme: bundled_theme(),
        theme_path: PathBuf::from("<bundled:theme:dust>"),
        theme_spans: BTreeMap::new(),
    }
}

fn load_config_capture(captured: &CapturedSource) -> LoadedConfig {
    match &captured.text {
        CapturedText::Missing => fallback_config(None),
        CapturedText::Error(error) => fallback_config(Some(format!(
            "cannot read {}: {error}",
            captured.path.display()
        ))),
        CapturedText::Text(source) => parse_config_source(&captured.path, source),
    }
}

#[must_use]
pub fn load_config(path: &Path) -> LoadedConfig {
    load_config_capture(&capture_source(path.to_path_buf()))
}

fn parse_config_source(path: &Path, source: &str) -> LoadedConfig {
    let migrated = match migrate_config_source(source) {
        Ok((source, _)) => source,
        Err(error) => {
            return fallback_config(Some(format!(
                "invalid {}: {error}; using bundled defaults",
                path.display()
            )));
        }
    };
    let source = migrated.as_str();
    let defaults =
        toml::from_str::<Config>(BUNDLED_CONFIG).expect("bundled config must remain valid");
    let mut merged =
        toml::from_str::<toml::Value>(BUNDLED_CONFIG).expect("bundled config must remain valid");
    let user = match toml::from_str::<toml::Value>(source) {
        Ok(user) => user,
        Err(error) => {
            return LoadedConfig {
                user_configuration: false,
                user_setting_keys: BTreeSet::new(),
                user_setting_spans: BTreeMap::new(),
                config: defaults,
                path: PathBuf::from("<bundled:config>"),
                diagnostic: Some(format!(
                    "invalid {}: {error}; using bundled defaults",
                    path.display()
                )),
                keymaps: LoadedKeymaps::default(),
                dashboard: DashboardConfig::default(),
                dashboard_path: PathBuf::from("<bundled:dashboard>"),
                dashboard_spans: BTreeMap::new(),
                theme: bundled_theme(),
                theme_path: PathBuf::from("<bundled:theme:dust>"),
                theme_spans: BTreeMap::new(),
            };
        }
    };
    let user_setting_keys = setting_keys(&user);
    merge_toml(&mut merged, user);
    match merged.try_into::<Config>() {
        Ok(config) => match config.validate() {
            Ok(()) => LoadedConfig {
                user_configuration: false,
                user_setting_keys,
                user_setting_spans: setting_spans(source),
                config,
                path: path.to_path_buf(),
                diagnostic: unknown_key_warning(source),
                keymaps: LoadedKeymaps::default(),
                dashboard: DashboardConfig::default(),
                dashboard_path: PathBuf::from("<bundled:dashboard>"),
                dashboard_spans: BTreeMap::new(),
                theme: bundled_theme(),
                theme_path: PathBuf::from("<bundled:theme:dust>"),
                theme_spans: BTreeMap::new(),
            },
            Err(error) => LoadedConfig {
                user_configuration: false,
                user_setting_keys: BTreeSet::new(),
                user_setting_spans: BTreeMap::new(),
                config: defaults,
                path: PathBuf::from("<bundled:config>"),
                diagnostic: Some(format!(
                    "invalid {}: {error}; using defaults",
                    path.display()
                )),
                keymaps: LoadedKeymaps::default(),
                dashboard: DashboardConfig::default(),
                dashboard_path: PathBuf::from("<bundled:dashboard>"),
                dashboard_spans: BTreeMap::new(),
                theme: bundled_theme(),
                theme_path: PathBuf::from("<bundled:theme:dust>"),
                theme_spans: BTreeMap::new(),
            },
        },
        Err(error) => LoadedConfig {
            user_configuration: false,
            user_setting_keys: BTreeSet::new(),
            user_setting_spans: BTreeMap::new(),
            config: defaults,
            path: PathBuf::from("<bundled:config>"),
            diagnostic: Some(format!(
                "invalid {}: {error}; using bundled defaults",
                path.display()
            )),
            keymaps: LoadedKeymaps::default(),
            dashboard: DashboardConfig::default(),
            dashboard_path: PathBuf::from("<bundled:dashboard>"),
            dashboard_spans: BTreeMap::new(),
            theme: bundled_theme(),
            theme_path: PathBuf::from("<bundled:theme:dust>"),
            theme_spans: BTreeMap::new(),
        },
    }
}

pub(crate) fn setting_spans(source: &str) -> BTreeMap<String, std::ops::Range<usize>> {
    fn visit(
        table: &dyn toml_edit::TableLike,
        prefix: &str,
        spans: &mut BTreeMap<String, std::ops::Range<usize>>,
    ) {
        for (key, item) in table.iter() {
            let path = if prefix.is_empty() {
                key.into()
            } else {
                format!("{prefix}.{key}")
            };
            if let Some(nested) = item.as_table_like() {
                visit(nested, &path, spans);
            } else if let Some(array) = item.as_array_of_tables() {
                for (index, table) in array.iter().enumerate() {
                    visit(table, &format!("{path}.{index}"), spans);
                }
            } else if let Some(span) = item.span() {
                spans.insert(path, span);
            }
        }
    }

    let Ok(document) = source.parse::<toml_edit::Document<String>>() else {
        return BTreeMap::new();
    };
    let mut spans = BTreeMap::new();
    visit(document.as_table(), "", &mut spans);
    spans
}

fn setting_keys(value: &toml::Value) -> BTreeSet<String> {
    fn visit(value: &toml::Value, prefix: &str, output: &mut BTreeSet<String>) {
        if let Some(table) = value.as_table() {
            for (key, value) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                if value.is_table() {
                    visit(value, &path, output);
                } else {
                    output.insert(path);
                }
            }
        }
    }
    let mut output = BTreeSet::new();
    visit(value, "", &mut output);
    output
}

fn merge_toml(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = base.get_mut(&key) {
                    merge_toml(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

pub struct ConfigurationWorker {
    sender: SyncSender<ConfigurationRequest>,
    receiver: Receiver<ConfigurationWorkerResult>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug)]
pub struct ConfigurationWorkerResult {
    pub generation: u64,
    pub automatic: bool,
    pub candidate: Result<LoadedConfig, String>,
}

enum ConfigurationRequest {
    Reload,
    ConfigureWatch {
        watch: ConfigWatchConfig,
        theme: String,
    },
    Shutdown,
}

impl ConfigurationWorker {
    #[must_use]
    pub fn start(
        use_user_configuration: bool,
        mut watch: ConfigWatchConfig,
        mut theme: String,
    ) -> Self {
        if !use_user_configuration {
            watch.enabled = false;
        }
        let (sender, requests) = mpsc::sync_channel(4);
        let (results, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("dust-configuration".into())
            .spawn(move || {
                let mut generation = 0_u64;
                let mut fingerprint = if use_user_configuration {
                    configuration_fingerprint(&theme)
                } else {
                    Vec::new()
                };
                let mut changed_at: Option<Instant> = None;
                loop {
                    let request = if watch.enabled {
                        requests.recv_timeout(Duration::from_millis(watch.poll_interval_ms))
                    } else {
                        requests.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    };
                    match request {
                        Ok(ConfigurationRequest::Reload) => {
                            generation = generation.saturating_add(1);
                            let candidate =
                                match load_consistent_configuration(use_user_configuration, &theme)
                                {
                                    Ok(consistent) => {
                                        if consistent.theme == theme
                                            && let Some(stable) = consistent.fingerprint
                                        {
                                            fingerprint = stable;
                                        }
                                        changed_at = None;
                                        consistent.candidate
                                    }
                                    Err(diagnostic) => {
                                        if use_user_configuration {
                                            fingerprint = configuration_fingerprint(&theme);
                                            changed_at = Some(Instant::now());
                                        }
                                        Err(diagnostic)
                                    }
                                };
                            if results
                                .send(ConfigurationWorkerResult {
                                    generation,
                                    automatic: false,
                                    candidate,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(ConfigurationRequest::ConfigureWatch {
                            watch: configured,
                            theme: configured_theme,
                        }) => {
                            watch = configured;
                            if !use_user_configuration {
                                watch.enabled = false;
                                theme = configured_theme;
                                fingerprint.clear();
                                changed_at = None;
                                continue;
                            }
                            let theme_changed = theme != configured_theme;
                            theme = configured_theme;
                            let observed = configuration_fingerprint(&theme);
                            let verify_candidate = theme_changed || observed != fingerprint;
                            fingerprint = observed;
                            changed_at = verify_candidate.then(Instant::now);
                        }
                        Ok(ConfigurationRequest::Shutdown)
                        | Err(RecvTimeoutError::Disconnected) => {
                            break;
                        }
                        Err(RecvTimeoutError::Timeout) => {
                            let observed = configuration_fingerprint(&theme);
                            if observed != fingerprint {
                                fingerprint = observed;
                                changed_at = Some(Instant::now());
                            }
                            if changed_at.is_some_and(|changed| {
                                changed.elapsed() >= Duration::from_millis(watch.debounce_ms)
                            }) {
                                match load_consistent_configuration(use_user_configuration, &theme)
                                {
                                    Ok(consistent) => {
                                        if consistent.theme == theme
                                            && let Some(stable) = consistent.fingerprint
                                        {
                                            fingerprint = stable;
                                        }
                                        changed_at = None;
                                        generation = generation.saturating_add(1);
                                        if results
                                            .send(ConfigurationWorkerResult {
                                                generation,
                                                automatic: true,
                                                candidate: consistent.candidate,
                                            })
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    Err(_) => {
                                        fingerprint = configuration_fingerprint(&theme);
                                        changed_at = Some(Instant::now());
                                    }
                                }
                            }
                        }
                    }
                }
            })
            .expect("configuration worker thread must start");
        Self {
            sender,
            receiver,
            thread: Some(thread),
        }
    }

    pub fn request_reload(&self) -> Result<(), ConfigurationWorkerError> {
        self.sender
            .try_send(ConfigurationRequest::Reload)
            .map_err(map_configuration_send_error)
    }

    pub fn configure_watch(
        &self,
        watch: ConfigWatchConfig,
        theme: String,
    ) -> Result<(), ConfigurationWorkerError> {
        self.sender
            .try_send(ConfigurationRequest::ConfigureWatch { watch, theme })
            .map_err(map_configuration_send_error)
    }

    pub fn try_result(
        &self,
    ) -> Result<Option<ConfigurationWorkerResult>, ConfigurationWorkerError> {
        match self.receiver.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ConfigurationWorkerError::Stopped),
        }
    }
}

fn map_configuration_send_error(
    error: TrySendError<ConfigurationRequest>,
) -> ConfigurationWorkerError {
    match error {
        TrySendError::Full(_) => ConfigurationWorkerError::Busy,
        TrySendError::Disconnected(_) => ConfigurationWorkerError::Stopped,
    }
}

struct CandidateAttempt {
    candidate: Result<LoadedConfig, String>,
    theme: String,
}

fn candidate_attempt_from_loaded(loaded: LoadedConfig) -> CandidateAttempt {
    let theme = loaded.config.ui.theme.clone();
    let mut diagnostics = Vec::new();
    diagnostics.extend(loaded.diagnostic.iter().cloned());
    diagnostics.extend(loaded.keymaps.diagnostic.iter().cloned());
    CandidateAttempt {
        candidate: if diagnostics.is_empty() {
            Ok(loaded)
        } else {
            Err(diagnostics.join("; "))
        },
        theme,
    }
}

struct ConsistentCandidate<F> {
    candidate: Result<LoadedConfig, String>,
    theme: String,
    fingerprint: Option<F>,
}

const CONFIGURATION_CONSISTENCY_ATTEMPTS: usize = 3;

#[cfg(test)]
fn load_consistent_candidate_with<F, L, P>(
    use_user_configuration: bool,
    initial_theme: &str,
    mut load: L,
    mut fingerprint: P,
) -> Result<ConsistentCandidate<F>, String>
where
    F: Eq,
    L: FnMut() -> CandidateAttempt,
    P: FnMut(&str) -> F,
{
    if !use_user_configuration {
        let attempt = load();
        return Ok(ConsistentCandidate {
            candidate: attempt.candidate,
            theme: attempt.theme,
            fingerprint: None,
        });
    }
    let mut theme = initial_theme.to_owned();
    for _ in 0..CONFIGURATION_CONSISTENCY_ATTEMPTS {
        let before = fingerprint(&theme);
        let attempt = load();
        if attempt.theme != theme {
            theme = attempt.theme;
            continue;
        }
        let after = fingerprint(&theme);
        if before == after {
            return Ok(ConsistentCandidate {
                candidate: attempt.candidate,
                theme,
                fingerprint: Some(after),
            });
        }
    }
    Err("configuration sources changed while a complete candidate was loading; no snapshot was applied"
        .into())
}

fn captured_bundle_is_stable<F: Eq>(before: &F, captured: &F, after: &F) -> bool {
    before == captured && captured == after
}

fn capture_consistent_configuration_bundle(
    initial_theme: &str,
) -> Result<(ConfigurationSourceBundle, Vec<SourceFingerprint>), String> {
    let mut theme = initial_theme.to_owned();
    for _ in 0..CONFIGURATION_CONSISTENCY_ATTEMPTS {
        let before = configuration_fingerprint(&theme);
        let bundle = capture_configuration_bundle(&theme);
        if bundle.selected_theme != theme {
            theme = bundle.selected_theme.clone();
            continue;
        }
        let captured = bundle.identity();
        let after = configuration_fingerprint(&theme);
        if captured_bundle_is_stable(&before, &captured, &after) {
            return Ok((bundle, captured));
        }
    }
    Err("configuration sources changed while a complete candidate was loading; no snapshot was applied"
        .into())
}

fn load_consistent_configuration(
    use_user_configuration: bool,
    initial_theme: &str,
) -> Result<ConsistentCandidate<Vec<SourceFingerprint>>, String> {
    if !use_user_configuration {
        let attempt = candidate_attempt_from_loaded(load_bundled_config());
        return Ok(ConsistentCandidate {
            candidate: attempt.candidate,
            theme: attempt.theme,
            fingerprint: None,
        });
    }
    let (bundle, captured) = capture_consistent_configuration_bundle(initial_theme)?;
    let theme = bundle.selected_theme.clone();
    let attempt = candidate_attempt_from_loaded(bundle.into_loaded());
    Ok(ConsistentCandidate {
        candidate: attempt.candidate,
        theme,
        fingerprint: Some(captured),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceFingerprint {
    path: PathBuf,
    modified: Option<SystemTime>,
    length: Option<u64>,
    /// Hashes at most this process-owned ceiling, so polling cannot allocate or
    /// read an unbounded file merely to detect a same-metadata replacement.
    content_hash: Option<u64>,
}

const WATCH_FINGERPRINT_BYTE_CEILING: u64 = 4 * 1024 * 1024;

fn source_fingerprint(path: PathBuf) -> SourceFingerprint {
    let metadata = fs::metadata(&path).ok();
    let content_hash = fs::File::open(&path).ok().and_then(|file| {
        let mut reader = file.take(WATCH_FINGERPRINT_BYTE_CEILING);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).ok()?;
        Some(hash_source_bytes(&bytes))
    });
    SourceFingerprint {
        path,
        modified: metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok()),
        length: metadata.map(|metadata| metadata.len()),
        content_hash,
    }
}

fn configuration_fingerprint(theme: &str) -> Vec<SourceFingerprint> {
    [
        user_config_path(),
        user_keymaps_path(),
        user_dashboard_path(),
        user_theme_path(theme),
    ]
    .into_iter()
    .map(source_fingerprint)
    .collect()
}

impl Drop for ConfigurationWorker {
    fn drop(&mut self) {
        let _ = self.sender.send(ConfigurationRequest::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ConfigurationWorkerError {
    #[error("configuration reload is already pending")]
    Busy,
    #[error("configuration worker stopped")]
    Stopped,
}

fn unknown_key_warning(source: &str) -> Option<String> {
    let value = toml::from_str::<toml::Value>(source).ok()?;
    let table = value.as_table()?;
    let unknown: Vec<_> = table
        .keys()
        .filter(|key| {
            !matches!(
                key.as_str(),
                "schema_version"
                    | "editor"
                    | "ui"
                    | "startup"
                    | "input"
                    | "search"
                    | "history"
                    | "recovery"
                    | "config_watch"
                    | "runtime"
                    | "lsp"
                    | "observability"
                    | "persistence"
                    | "processes"
                    | "packages"
                    | "plugins"
                    | "debug"
                    | "tasks"
                    | "syntax"
                    | "terminals"
                    | "workspace"
                    | "messages"
                    | "command_labels"
            )
        })
        .cloned()
        .collect();
    (!unknown.is_empty()).then(|| format!("unknown configuration sections: {}", unknown.join(", ")))
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, fs, path::PathBuf};

    use tempfile::tempdir;

    use super::{
        CandidateAttempt, CapturedSource, CapturedText, LineNumbers, SourceFingerprint,
        WATCH_FINGERPRINT_BYTE_CEILING, capture_configuration_bundle_with,
        captured_bundle_is_stable, hash_source_bytes, load_bundled_config, load_config,
        load_consistent_candidate_with, load_keymaps, load_theme, source_fingerprint,
    };

    #[test]
    fn absent_file_uses_defaults_without_error() {
        let root = tempdir().expect("tempdir");
        let loaded = load_config(&root.path().join("missing.toml"));
        assert_eq!(loaded.config, load_bundled_config().config);
        assert!(loaded.diagnostic.is_none());
    }

    #[test]
    fn valid_partial_config_merges_with_defaults() {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("config.toml");
        fs::write(
            &path,
            "[editor]\nline_numbers = 'absolute'\ntab_width = 2\n",
        )
        .expect("write");
        let loaded = load_config(&path);
        assert_eq!(loaded.config.editor.line_numbers, LineNumbers::Absolute);
        assert_eq!(loaded.config.editor.tab_width, 2);
        assert_eq!(loaded.config.ui.command_line.width, 60);
        assert_eq!(
            loaded.config.input.text_commands.get("editor.insert"),
            Some(&"edit.insert_text".to_owned())
        );
        assert!(loaded.diagnostic.is_none());
        let source = loaded
            .explain("editor.tab_width")
            .expect("user value")
            .source;
        assert!(source.starts_with(&path.display().to_string()));
        assert!(source.contains(":bytes "));
        assert_eq!(
            loaded
                .explain("ui.command_line.width")
                .expect("inherited value")
                .source,
            "bundled defaults"
        );
    }

    #[test]
    fn parser_spans_cover_inline_tables_and_quoted_dotted_keys() {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("config.toml");
        fs::write(
            &path,
            "editor = { tab_width = 3 }\n[command_labels]\n\"view.cursor.move\" = \"MOVE\"\n",
        )
        .expect("write");
        let loaded = load_config(&path);
        assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
        assert_eq!(loaded.config.editor.tab_width, 3);
        let editor = loaded.explain("editor.tab_width").expect("inline explain");
        assert!(editor.source.contains(":bytes "));
        let quoted = loaded
            .explain("command_labels.\"view.cursor.move\"")
            .expect("quoted key explain");
        assert_eq!(quoted.value, "\"MOVE\"");
        assert!(quoted.source.contains(":bytes "));
    }

    #[test]
    fn parser_spans_cover_theme_leaves_and_dashboard_action_tables() {
        let theme = "[styles.accent]\nforeground='#010203'\n";
        let theme_spans = super::setting_spans(theme);
        let span = &theme_spans["styles.accent.foreground"];
        assert_eq!(&theme[span.clone()], "'#010203'");

        let dashboard = "[[action]]\ncommand='file.new'\nlabel='New'\n";
        let dashboard_spans = super::setting_spans(dashboard);
        assert!(dashboard_spans.contains_key("action.0.command"));
        let span = &dashboard_spans["action.0.label"];
        assert_eq!(&dashboard[span.clone()], "'New'");
    }

    #[test]
    fn malformed_or_invalid_config_is_nonfatal() {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("config.toml");
        fs::write(&path, "[editor]\ntab_width = 0\n").expect("write");
        let loaded = load_config(&path);
        assert_eq!(loaded.config, load_bundled_config().config);
        assert!(loaded.diagnostic.is_some());

        fs::write(&path, "[ui]\nmade_up = true\n").expect("unknown nested key");
        let loaded = load_config(&path);
        assert_eq!(loaded.config, load_bundled_config().config);
        assert!(
            loaded
                .diagnostic
                .as_deref()
                .is_some_and(|message| message.contains("unknown field `made_up`"))
        );
    }

    fn captured(path: PathBuf, text: CapturedText) -> CapturedSource {
        let (length, content_hash) = match &text {
            CapturedText::Text(source) => (
                Some(source.len() as u64),
                Some(hash_source_bytes(source.as_bytes())),
            ),
            CapturedText::Missing | CapturedText::Error(_) => (None, None),
        };
        CapturedSource {
            identity: SourceFingerprint {
                path: path.clone(),
                modified: None,
                length,
                content_hash,
            },
            path,
            text,
        }
    }

    fn candidate_attempt(theme: &str, valid: bool) -> CandidateAttempt {
        let mut loaded = load_bundled_config();
        loaded.config.ui.theme = theme.into();
        CandidateAttempt {
            candidate: if valid {
                Ok(loaded)
            } else {
                Err("synthetic malformed candidate".into())
            },
            theme: theme.into(),
        }
    }

    #[test]
    fn captured_bundle_identity_exposes_aba_and_uses_the_same_text_for_spans() {
        let config = "[ui]\ntheme='night'\n";
        let keymaps_b = "[[keymap]]\ncontext='dashboard'\nkeys='b'\ncommand='file.new'\n";
        let dashboard = include_str!("../../assets/defaults/dashboard.toml")
            .replace("D U S T", "CAPTURED DASH");
        let theme = include_str!("../../assets/themes/dust.toml")
            .replace("name = \"dust\"", "name = \"night\"");
        let reads = Cell::new(0_usize);
        let bundle = capture_configuration_bundle_with("dust", |path| {
            reads.set(reads.get() + 1);
            let text = match path.file_name().and_then(|name| name.to_str()) {
                Some("config.toml") => config.to_owned(),
                Some("keymaps.toml") => keymaps_b.to_owned(),
                Some("dashboard.toml") => dashboard.clone(),
                Some("night.toml") => theme.clone(),
                other => panic!("unexpected captured path {other:?}"),
            };
            captured(path, CapturedText::Text(text))
        });
        assert_eq!(reads.get(), 4, "every represented source is captured once");
        assert_eq!(bundle.selected_theme, "night");
        assert!(bundle.theme.path.ends_with("night.toml"));

        // External pre/post observations can both be A, but a transient B read
        // remains visible in the captured identity and cannot be emitted as A.
        let mut observed_a = bundle.identity();
        let keymap_path = observed_a[1].path.clone();
        observed_a[1] = captured(
            keymap_path,
            CapturedText::Text(
                "[[keymap]]\ncontext='dashboard'\nkeys='a'\ncommand='file.new'\n".into(),
            ),
        )
        .identity;
        assert_ne!(observed_a, bundle.identity());
        assert!(
            !captured_bundle_is_stable(&observed_a, &bundle.identity(), &observed_a),
            "A→B→A must reject the transient B bundle"
        );

        let loaded = bundle.into_loaded();
        assert_eq!(loaded.dashboard.identity, "CAPTURED DASH");
        let dashboard_span = &loaded.dashboard_spans["identity"];
        assert_eq!(&dashboard[dashboard_span.clone()], "\"CAPTURED DASH\"");
        let theme_span = &loaded.theme_spans["name"];
        assert_eq!(&theme[theme_span.clone()], "\"night\"");
        assert_eq!(
            loaded.keymaps.entries.last().expect("user keymap").keys,
            "b"
        );
    }

    #[test]
    fn captured_bundle_identity_detects_missing_created_and_deleted_sources() {
        let config = captured(
            super::user_config_path(),
            CapturedText::Text("[ui]\ntheme='dust'\n".into()),
        );
        let missing = captured(super::user_keymaps_path(), CapturedText::Missing);
        let created = captured(
            super::user_keymaps_path(),
            CapturedText::Text(
                "[[keymap]]\ncontext='dashboard'\nkeys='x'\ncommand='file.new'\n".into(),
            ),
        );
        assert_ne!(missing.identity, created.identity);
        assert_ne!(config.identity, missing.identity);
        assert!(!captured_bundle_is_stable(
            &missing.identity,
            &created.identity,
            &missing.identity
        ));
    }

    #[test]
    fn consistent_loader_retries_same_theme_changes_and_theme_source_switches() {
        let revision = Cell::new(0_u8);
        let loads = Cell::new(0_usize);
        let stable = load_consistent_candidate_with(
            true,
            "dust",
            || {
                loads.set(loads.get() + 1);
                if loads.get() == 1 {
                    revision.set(1);
                }
                candidate_attempt("dust", true)
            },
            |_| revision.get(),
        )
        .expect("eventually stable");
        assert_eq!(loads.get(), 2);
        assert_eq!(stable.fingerprint, Some(1));
        assert!(stable.candidate.is_ok());

        let theme_revision = Cell::new(0_u8);
        let loads = Cell::new(0_usize);
        let switched = load_consistent_candidate_with(
            true,
            "dust",
            || {
                loads.set(loads.get() + 1);
                if loads.get() == 2 {
                    theme_revision.set(1);
                }
                candidate_attempt("night", true)
            },
            |theme| (theme.to_owned(), theme_revision.get()),
        )
        .expect("new theme stabilizes");
        assert_eq!(loads.get(), 3);
        assert_eq!(switched.theme, "night");
        assert_eq!(switched.fingerprint, Some(("night".into(), 1)));
    }

    #[test]
    fn consistent_loader_never_emits_discarded_malformed_or_stale_attempts() {
        for final_valid in [true, false] {
            let revision = Cell::new(0_u8);
            let loads = Cell::new(0_usize);
            let loaded = load_consistent_candidate_with(
                true,
                "dust",
                || {
                    loads.set(loads.get() + 1);
                    if loads.get() == 1 {
                        revision.set(1);
                        candidate_attempt("dust", !final_valid)
                    } else {
                        candidate_attempt("dust", final_valid)
                    }
                },
                |_| vec![revision.get(); 4],
            )
            .expect("second complete source set is stable");
            assert_eq!(loads.get(), 2);
            assert_eq!(loaded.candidate.is_ok(), final_valid);
            assert_eq!(loaded.fingerprint, Some(vec![1; 4]));
        }
    }

    #[test]
    fn accepted_candidate_fingerprint_matches_all_four_loaded_surfaces() {
        let revision = Cell::new(0_u8);
        let loads = Cell::new(0_usize);
        let loaded = load_consistent_candidate_with(
            true,
            "dust",
            || {
                loads.set(loads.get() + 1);
                let snapshot = revision.get();
                let mut attempt = candidate_attempt("dust", true);
                let candidate = attempt.candidate.as_mut().expect("candidate");
                candidate.config.editor.tab_width = usize::from(snapshot) + 2;
                candidate.keymaps.path = format!("keymaps-{snapshot}").into();
                candidate.dashboard.identity = format!("dashboard-{snapshot}");
                candidate.theme_path = format!("theme-{snapshot}").into();
                if loads.get() == 1 {
                    revision.set(1);
                }
                attempt
            },
            |_| vec![revision.get(); 4],
        )
        .expect("stable complete snapshot");
        let candidate = loaded.candidate.expect("accepted candidate");
        assert_eq!(loaded.fingerprint, Some(vec![1; 4]));
        assert_eq!(candidate.config.editor.tab_width, 3);
        assert_eq!(candidate.keymaps.path, PathBuf::from("keymaps-1"));
        assert_eq!(candidate.dashboard.identity, "dashboard-1");
        assert_eq!(candidate.theme_path, PathBuf::from("theme-1"));
    }

    #[test]
    fn clean_consistent_load_never_fingerprints_user_sources() {
        let loads = Cell::new(0_usize);
        let loaded = load_consistent_candidate_with(
            false,
            "dust",
            || {
                loads.set(loads.get() + 1);
                candidate_attempt("dust", true)
            },
            |_| -> Vec<u8> { panic!("--clean must not fingerprint user sources") },
        )
        .expect("bundled candidate");
        assert_eq!(loads.get(), 1);
        assert!(loaded.fingerprint.is_none());
        assert!(loaded.candidate.is_ok());
    }

    #[test]
    fn watcher_fingerprint_detects_same_length_content_and_sources_are_size_bounded() {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("config.toml");
        fs::write(&path, "[editor]\ntab_width=2\n").expect("first");
        let first = source_fingerprint(path.clone());
        fs::write(&path, "[editor]\ntab_width=3\n").expect("same-length replacement");
        let second = source_fingerprint(path.clone());
        assert_eq!(first.length, second.length);
        assert_ne!(first.content_hash, second.content_hash);

        let oversized = vec![b' '; WATCH_FINGERPRINT_BYTE_CEILING as usize + 1];
        fs::write(&path, oversized).expect("oversized source");
        let loaded = load_config(&path);
        assert!(
            loaded
                .diagnostic
                .as_deref()
                .is_some_and(|diagnostic| { diagnostic.contains("configuration source exceeds") })
        );
        assert_eq!(loaded.config, load_bundled_config().config);
    }

    #[test]
    fn recovery_retention_and_watcher_limits_reject_unsafe_candidates() {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("config.toml");
        fs::write(
            &path,
            "[recovery]\nmaximum_retained_records=10001\nmaximum_age_days=36501\n",
        )
        .expect("unsafe recovery policy");
        let loaded = load_config(&path);
        assert_eq!(loaded.config, load_bundled_config().config);
        assert!(loaded.diagnostic.as_deref().is_some_and(|diagnostic| {
            diagnostic.contains("maximum_retained_records exceeds safe ceiling")
        }));

        fs::write(
            &path,
            "[config_watch]\npoll_interval_ms=24\ndebounce_ms=60001\n",
        )
        .expect("unsafe watcher policy");
        let loaded = load_config(&path);
        assert_eq!(loaded.config, load_bundled_config().config);
        assert!(
            loaded.diagnostic.as_deref().is_some_and(|diagnostic| {
                diagnostic.contains("poll_interval_ms must be between")
            })
        );
    }

    #[test]
    fn bundled_assets_share_the_typed_configuration_loaders() {
        let loaded = load_bundled_config();
        let declared =
            toml::from_str::<toml::Value>(super::BUNDLED_CONFIG).expect("bundled source value");
        let resolved = toml::Value::try_from(&loaded.config).expect("resolved value");
        assert_eq!(
            super::setting_keys(&declared),
            super::setting_keys(&resolved),
            "every typed bundled setting must be explicit in config.toml"
        );
        assert!(loaded.keymaps.entries.len() > 100);
        assert_eq!(loaded.keymaps.bundled_count, loaded.keymaps.entries.len());
        assert_eq!(loaded.dashboard.action.len(), 5);
        let explained = loaded.explain("ui.command_line.width").expect("explain");
        assert_eq!(explained.value, "60");
        assert_eq!(explained.source, "bundled defaults");
        assert_eq!(
            loaded.config.input.text_commands.get("command_prompt"),
            Some(&"command_line.insert".to_owned())
        );
    }

    #[test]
    fn themes_are_typed_and_require_capability_fallbacks() {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("theme.toml");
        let source =
            super::BUNDLED_THEME.replace("foreground = \"cyan\"", "foreground = \"#010203\"");
        fs::write(&path, source).expect("theme");
        let error = load_theme(&path).expect_err("missing fallbacks");
        assert!(error.contains("256-color fallback"));

        let source = super::BUNDLED_THEME.replace(
            "foreground = \"cyan\"",
            "foreground = \"indexed:201\"\nforeground_16 = \"cyan\"",
        );
        fs::write(&path, source).expect("theme");
        let theme = load_theme(&path).expect("valid typed theme");
        assert_eq!(
            theme.styles.accent.foreground,
            super::ThemeColor::Indexed(201)
        );
    }

    #[test]
    fn keymaps_parse_typed_arguments_and_fail_transactionally() {
        let root = tempdir().expect("tempdir");
        let path = root.path().join("keymaps.toml");
        fs::write(
            &path,
            "[[keymap]]\nmode='normal'\nkeys='space q'\ncommand='editor.quit'\narguments={force=true}\n",
        )
        .expect("write");
        let loaded = load_keymaps(&path);
        assert!(loaded.diagnostic.is_none());
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.user_spans.len(), 1);
        assert!(
            loaded.user_spans[0].end > loaded.user_spans[0].start,
            "span was {:?}",
            loaded.user_spans[0]
        );
        assert_eq!(loaded.entries[0].arguments["force"].as_bool(), Some(true));

        fs::write(&path, "[[keymap]\n").expect("malformed");
        let loaded = load_keymaps(&path);
        assert!(loaded.entries.is_empty());
        assert!(loaded.diagnostic.is_some());

        fs::write(
            &path,
            "[[keymap]]\nmode='normal'\nkeys='x'\nreplacement_keys='dd'\n",
        )
        .expect("unsupported recursive mapping");
        let loaded = load_keymaps(&path);
        assert!(loaded.entries.is_empty());
        assert!(
            loaded
                .diagnostic
                .as_deref()
                .is_some_and(|message| message.contains("unknown field `replacement_keys`"))
        );
    }
}
