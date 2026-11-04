use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterConfig {
    pub clipboard: ClipboardSync,
}

/// `unnamedplus` shares the unnamed register with the system clipboard, as LazyVim does.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardSync {
    Unnamedplus,
    Off,
}

impl Default for RegisterConfig {
    fn default() -> Self {
        Self {
            clipboard: ClipboardSync::Unnamedplus,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LargeFileConfig {
    pub enabled: bool,
}
impl Default for LargeFileConfig {
    fn default() -> Self {
        Self { enabled: true }
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
}

impl Default for GitSignsConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Markdown drawn in place, as render-markdown.nvim does: the cursor's line shows its source.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownConfig {
    pub render: bool,
    pub glyphs: MarkdownGlyphs,
}

impl Default for MarkdownConfig {
    fn default() -> Self {
        Self {
            render: true,
            glyphs: MarkdownGlyphs::default(),
        }
    }
}

/// What markdown draws in place of its markup.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownGlyphs {
    /// Drawn in place of `#` to `######` and the space after them.
    pub headings: Vec<String>,
    /// Drawn in place of a list's `-`, `+` or `*`, one per nesting depth, repeating.
    pub bullets: Vec<String>,
    pub unchecked: String,
    pub checked: String,
    pub quote: String,
    pub link: String,
    pub image: String,
    /// Repeated across the window for a `---` break.
    pub rule: String,
    /// A table's `│`, `─`, `├`, `┼` and `┤`.
    pub table: Vec<String>,
    /// Icons for links whose address contains the key; the longest key wins, `link` otherwise.
    pub links: BTreeMap<String, String>,
    /// Callouts such as `> [!NOTE]`, by their lowercase name.
    pub callouts: BTreeMap<String, MarkdownCallout>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MarkdownCallout {
    /// Drawn in place of `[!NOTE]`.
    pub text: String,
    /// The highlight group that colours the title and the quote's bar.
    pub group: String,
}

impl Default for MarkdownGlyphs {
    fn default() -> Self {
        Self {
            headings: ["󰲡 ", "󰲣 ", "󰲥 ", "󰲧 ", "󰲩 ", "󰲫 "]
                .map(String::from)
                .into(),
            bullets: ["●", "○", "◆", "◇"].map(String::from).into(),
            unchecked: "󰄱".into(),
            checked: "󰱒".into(),
            quote: "▋".into(),
            link: "󰌹 ".into(),
            image: "󰥶 ".into(),
            rule: "─".into(),
            table: ["│", "─", "├", "┼", "┤"].map(String::from).into(),
            links: BTreeMap::new(),
            callouts: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IndentGuideConfig {
    pub enabled: bool,
}

impl Default for IndentGuideConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EditorConfig {
    pub line_numbers: LineNumbers,
    pub tab_width: usize,
    pub indent_width: usize,
    pub expand_tabs: bool,
    pub wrap: bool,
    pub line_endings: LineEndingPolicy,
    pub final_newline: FinalNewlinePolicy,
    pub ambiguous_width: AmbiguousWidth,
    /// Insert the closing bracket or quote as the opening one is typed.
    pub autopairs: bool,
    pub viewport: EditorViewportConfig,
    pub git_signs: GitSignsConfig,
    pub indent_guides: IndentGuideConfig,
    pub markdown: MarkdownConfig,
    pub registers: RegisterConfig,
    pub large_file: LargeFileConfig,
    /// Line comment string per file extension, used by `gc`.
    pub comments: BTreeMap<String, String>,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            line_numbers: LineNumbers::Relative,
            tab_width: 4,
            indent_width: 4,
            expand_tabs: true,
            wrap: false,
            line_endings: LineEndingPolicy::Preserve,
            final_newline: FinalNewlinePolicy::Preserve,
            ambiguous_width: AmbiguousWidth::Narrow,
            autopairs: true,
            viewport: EditorViewportConfig::default(),
            git_signs: GitSignsConfig::default(),
            indent_guides: IndentGuideConfig::default(),
            markdown: MarkdownConfig::default(),
            registers: RegisterConfig::default(),
            large_file: LargeFileConfig::default(),
            comments: BTreeMap::new(),
        }
    }
}

impl EditorConfig {
    /// The editing rules the buffer commands follow.
    #[must_use]
    pub fn editing_policy(&self) -> crate::editor::EditingPolicy {
        crate::editor::EditingPolicy {
            tab_width: self.tab_width,
            indent_width: self.indent_width,
            expand_tabs: self.expand_tabs,
            ambiguous_width_wide: self.ambiguous_width == AmbiguousWidth::Wide,
            autopairs: self.autopairs,
            ..crate::editor::EditingPolicy::default()
        }
    }
}
