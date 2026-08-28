use super::super::*;

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
pub struct EditorViewportConfig {
    /// Non-wrapped cursor context retained at the left and right viewport edges.
    pub horizontal_margin: u16,
    /// Cursor context retained above and below the viewport.
    pub vertical_margin: u16,
}

impl Default for EditorViewportConfig {
    fn default() -> Self {
        Self {
            horizontal_margin: 4,
            vertical_margin: 4,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitSignsConfig {
    pub enabled: bool,
    pub unchanged: String,
    pub added: String,
    pub changed: String,
    pub deleted: String,
}

impl Default for GitSignsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            unchanged: "│".into(),
            added: "│".into(),
            changed: "│".into(),
            deleted: "_".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IndentGuideConfig {
    pub enabled: bool,
    pub active_scope: bool,
    pub character: String,
}

impl Default for IndentGuideConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            active_scope: true,
            character: "│".into(),
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
    pub viewport: EditorViewportConfig,
    pub git_signs: GitSignsConfig,
    pub indent_guides: IndentGuideConfig,
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
            viewport: EditorViewportConfig::default(),
            git_signs: GitSignsConfig::default(),
            indent_guides: IndentGuideConfig::default(),
            registers: RegisterConfig::default(),
            large_file: LargeFileConfig::default(),
        }
    }
}
