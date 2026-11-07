//! Editor surfaces and frame composition.

pub(crate) mod terminal;

use terminal::active_compositor;

use terminal::dock_terminal_panel;

use terminal::render_key_hints;

mod debug;

pub(crate) mod explorer;

pub use debug::DebugPane;

pub use debug::DebugPanesUiState;

pub use debug::debug_pane_insets;

pub use debug::debug_pane_tab_at;

use debug::debug_layout_in;

use debug::render_debug_panes;

pub(crate) use explorer::explorer_segment_style;

use explorer::place_workspace_frame;

use explorer::render_explorer_window;

pub mod chrome;
pub(crate) mod overlays;

use crate::ui::geometry::text_width;

use overlays::render_dashboard;

use documentation::render_documentation_popup;

use overlays::render_language_completion;

use overlays::render_lsp_loading;

use overlays::render_lsp_progress;

mod workspace;

use workspace::render_non_tiled_editor_window;

use workspace::render_non_tiled_editor_windows;

use workspace::render_workspace;

mod agent;

mod decorations;
pub(crate) mod editor;

mod markdown;

mod settings;

mod setup;

pub(crate) mod documentation;
pub(crate) mod picker;
pub(crate) mod plugins;
pub(crate) mod prompt;
pub(crate) mod tools;

mod visual_rows;

#[cfg(test)]
use editor::render_editor_view;

pub(crate) use editor::workspace_title;

#[cfg(test)]
use overlays::content_tab_width;

use picker::render_picker;

use plugins::render_plugin_surface;

use plugins::render_private_surface;

use prompt::render_prompt;

use tools::render_tool_menu;

#[cfg(test)]
use visual_rows::VisualRowSlice;

pub use agent::AgentListRow;

pub use agent::AgentPanelUi;

pub use agent::transcript_lines;

pub use editor::snap_display_column;

pub use editor::snap_display_column_forward;

pub use explorer::explorer_panel_width;

pub use overlays::content_tab_at_column;

pub use settings::SettingsEdit;

pub use settings::SettingsHit;

pub use settings::SettingsLine;

pub use settings::SettingsPageState;

pub use settings::SettingsRow;

pub use settings::SettingsValue;

pub use settings::SetupChoice;

pub use settings::SetupPage;

pub use settings::SetupState;

pub use settings::settings_hit;

pub use settings::settings_layout;

pub use settings::settings_lines_height;

pub use picker::picker_geometry;

pub use terminal::terminal_tab_at_column;

pub(crate) use terminal::terminal_window_geometry;

pub use visual_rows::VisualRowAnchor;

pub use visual_rows::VisualRowModel;

pub use visual_rows::editor_content_width;

pub use visual_rows::editor_gutter_width;

pub use visual_rows::editor_state_content_width;

pub use visual_rows::editor_state_gutter_width;

pub use workspace::PaneViewport;

pub use workspace::active_editor_pane_viewport;

pub use workspace::active_pane_viewport;

pub use workspace::editor_pane_viewport_at;

pub use workspace::pane_viewports;

pub use workspace::view_pane_viewport;

use std::time::Instant;

use crate::config::AmbiguousWidth;

use crate::config::Config;

use crate::config::DashboardConfig;

use crate::config::PromptTextConfig;

use crate::editor::EditorState;

use crate::editor::text::ByteOffset;

use crate::editor::text::ByteRange;

use crate::windows::CompositorProjection;

use crate::windows::ContentAdapterKind;

use crate::windows::ContentId;

use crate::windows::TiledLayoutSnapshot;

use crate::windows::WindowId;

use crate::windows::WindowRect;

use unicode_segmentation::UnicodeSegmentation;

use crate::ui::CellGrid;

use crate::ui::Style;

pub(crate) const SCRATCH_LABEL: &str = "[scratch]";

pub(crate) const TERMINAL_PANEL_MINIMUM_HEIGHT: u16 = 4;

pub(crate) const TERMINAL_PANEL_MAXIMUM_HEIGHT: u16 = 40;

pub(crate) const TERMINAL_PANEL_MINIMUM_WORKSPACE_HEIGHT: u16 = 3;

pub(crate) const TERMINAL_PANEL_RESIZE_STEP: u16 = 2;

pub(crate) const COMPLETION_MAX_ROWS: u16 = 12;

pub(crate) const COMPLETION_DETAIL_GAP: u16 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptKind {
    Text,
    Command,
    SearchForward,
    SearchBackward,
    WorkspaceTrust,
    GitReset,
    ExplorerDelete,
    ExplorerRename,
    PluginConfirmation,
    TerminalClose,
    DirtyClose,
    GitStageUnsaved,
    GitDestructive,
    AgentPermission,
    ReviewComment,
    BreakpointCondition,
    ReplaceFind,
    ReplaceWith,
    SubstituteConfirm,
}

impl PromptKind {
    #[must_use]
    pub fn title(self, text: &PromptTextConfig) -> &str {
        match self {
            Self::Text => "Input",
            Self::Command => &text.command_title,
            Self::SearchForward => &text.search_forward_title,
            Self::SearchBackward => &text.search_backward_title,
            Self::WorkspaceTrust => &text.workspace_trust_title,
            Self::GitReset => &text.git_reset_title,
            Self::ExplorerDelete => &text.explorer_delete_title,
            Self::ExplorerRename => &text.explorer_rename_title,
            Self::PluginConfirmation => "Plugin confirmation",
            Self::TerminalClose => "Close terminal",
            Self::DirtyClose => "Close document",
            Self::GitStageUnsaved => "Stage unsaved changes",
            Self::GitDestructive => "Confirm Git change",
            Self::AgentPermission => "Agent permission",
            Self::ReviewComment => "Review comment",
            Self::BreakpointCondition => "Breakpoint condition",
            Self::ReplaceFind => "Replace in project: find",
            Self::ReplaceWith => "Replace with",
            Self::SubstituteConfirm => "Replace",
        }
    }

    #[must_use]
    pub fn marker(self, text: &PromptTextConfig) -> &str {
        match self {
            Self::Text => ">",
            Self::Command => &text.command_marker,
            Self::SearchForward => &text.search_forward_marker,
            Self::SearchBackward => &text.search_backward_marker,
            Self::WorkspaceTrust => &text.workspace_trust_marker,
            Self::GitReset => &text.git_reset_marker,
            Self::ExplorerDelete => &text.explorer_delete_marker,
            Self::ExplorerRename => &text.explorer_rename_marker,
            Self::PluginConfirmation
            | Self::TerminalClose
            | Self::DirtyClose
            | Self::GitStageUnsaved
            | Self::GitDestructive
            | Self::AgentPermission
            | Self::SubstituteConfirm => "",
            Self::ReviewComment => ">",
            Self::BreakpointCondition | Self::ReplaceFind | Self::ReplaceWith => ">",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionItem {
    /// The kind's glyph, drawn before the label.
    pub icon: Option<String>,
    pub label: String,
    pub detail: String,
    /// A swatch drawn after the label, for colour completions such as Tailwind classes.
    pub color: Option<(u8, u8, u8)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmationChoice {
    Yes,
    No,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmationState {
    pub title: String,
    pub message: String,
    pub selected: ConfirmationChoice,
    pub positive_label: String,
    pub negative_label: String,
    pub hint: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptState {
    pub kind: PromptKind,
    pub confirmation: Option<ConfirmationState>,
    pub input: String,
    pub cursor: usize,
    pub help: Option<String>,
    pub error: Option<String>,
    pub completions: Vec<CompletionItem>,
    pub selected_completion: Option<usize>,
}

impl PromptState {
    #[must_use]
    pub fn new(kind: PromptKind) -> Self {
        Self {
            kind,
            confirmation: None,
            input: String::new(),
            cursor: 0,
            help: None,
            error: None,
            completions: Vec::new(),
            selected_completion: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DashboardAction {
    pub command: String,
    pub label: String,
    pub icon: String,
    pub key: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolMenuRow {
    pub id: String,
    pub for_project: bool,
    pub name: String,
    pub kind: String,
    pub languages: String,
    pub version: String,
    pub release: String,
    pub runtime: String,
    /// What the row shows: installed, running, installing 40%, not installed, …
    pub lifecycle: String,
    /// The language-server session state the row was built from, to notice when it changes.
    pub session_state: String,
    pub executable: String,
    pub note: String,
    pub sessions: Vec<String>,
    pub lsp: bool,
    pub installed: bool,
    pub can_start: bool,
    pub can_install: bool,
    pub can_uninstall: bool,
    /// The version this Dust release ships, when the Dust-installed copy is a different one.
    pub update: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolRuntimeOption {
    pub source: String,
    /// Exact managed catalog pin; PATH and the fixed Nix workspace use `None`.
    pub version_pin: Option<String>,
    pub label: String,
    pub version: String,
    pub release: String,
    pub location: String,
    pub status: String,
    pub note: String,
    pub selected: bool,
    pub installed: bool,
    pub installable: bool,
    pub uninstallable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolRuntimePickerState {
    pub server_id: String,
    pub selected: usize,
    pub scroll: usize,
    pub options: Vec<ToolRuntimeOption>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiSyntaxSpan {
    pub buffer: crate::editor::BufferId,
    pub revision: crate::editor::text::BufferRevision,
    pub range: ByteRange,
    pub role: crate::config::Highlight,
}

/// What a markdown buffer draws in place of its markup, for one parse of its text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiMarkdownMark {
    pub buffer: crate::editor::BufferId,
    pub revision: crate::editor::text::BufferRevision,
    pub range: ByteRange,
    pub mark: crate::language::syntax::MarkdownMark,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiInlayHint {
    pub buffer: crate::editor::BufferId,
    pub revision: crate::editor::text::BufferRevision,
    pub position: ByteOffset,
    pub label: String,
}

/// A colour a language server reported, painted as the background of its range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiColorSpan {
    pub server: String,
    pub buffer: crate::editor::BufferId,
    pub revision: crate::editor::text::BufferRevision,
    pub range: ByteRange,
    pub rgb: (u8, u8, u8),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiFold {
    pub view: crate::editor::ViewId,
    pub start_line: usize,
    pub end_line: usize,
    pub collapsed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiDiagnostic {
    pub buffer: Option<crate::editor::BufferId>,
    pub path: std::path::PathBuf,
    pub range: ByteRange,
    pub severity: crate::workspace::DiagnosticSeverity,
    pub source: String,
    pub code: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpandedDiagnosticState {
    pub buffer: Option<crate::editor::BufferId>,
    pub path: std::path::PathBuf,
    pub line: usize,
    pub severities: Vec<crate::workspace::DiagnosticSeverity>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentationPopupKind {
    Hover,
    Signature,
    Completion,
    Git,
    /// A notice opened from `:messages`, whatever the file under it.
    Notice,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentationMarkupKind {
    PlainText,
    Markdown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentationSection {
    pub heading: Option<String>,
    pub markup: DocumentationMarkupKind,
    pub lines: Vec<String>,
    /// Styled runs of rendered Markdown, as byte ranges in their line, by line; a line
    /// without any is drawn in its role's style.
    pub spans: Vec<Vec<(std::ops::Range<usize>, crate::config::Highlight)>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentationPopupState {
    pub kind: DocumentationPopupKind,
    pub title: String,
    pub provider: String,
    pub source_range: Option<ByteRange>,
    pub sections: Vec<DocumentationSection>,
    pub scroll: usize,
}

impl DocumentationPopupState {
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.sections.iter().fold(0usize, |count, section| {
            count
                .saturating_add(usize::from(section.heading.is_some()))
                .saturating_add(section.lines.len())
        })
    }

    /// Whether the line at `index` is a section's heading.
    fn heading_at(&self, mut index: usize) -> bool {
        for section in &self.sections {
            if section.heading.is_some() {
                if index == 0 {
                    return true;
                }
                index = index.saturating_sub(1);
            }
            if index < section.lines.len() {
                return false;
            }
            index = index.saturating_sub(section.lines.len());
        }
        false
    }

    fn line_at(&self, index: usize) -> Option<&str> {
        self.at(index).map(|(line, _)| line)
    }

    /// The line at `index` and its styled runs.
    #[allow(clippy::type_complexity)]
    fn at(
        &self,
        mut index: usize,
    ) -> Option<(&str, &[(std::ops::Range<usize>, crate::config::Highlight)])> {
        for section in &self.sections {
            if let Some(heading) = &section.heading {
                if index == 0 {
                    return Some((heading, &[]));
                }
                index = index.saturating_sub(1);
            }
            if let Some(line) = section.lines.get(index) {
                return Some((
                    line,
                    section.spans.get(index).map_or(&[][..], Vec::as_slice),
                ));
            }
            index = index.saturating_sub(section.lines.len());
        }
        None
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageCompletionState {
    /// Columns already typed of the word being completed, so labels line up with it.
    pub typed: u16,
    pub items: Vec<CompletionItem>,
    pub selected: usize,
    pub scroll: usize,
}

/// What a picker row is. A section header groups the entries under it and is drawn as a
/// header rather than as an entry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PickerRowKind {
    #[default]
    Entry,
    Section,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerRow {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub source: String,
    pub kind: PickerRowKind,
    /// Columns this row draws instead of the flattened label/detail/source text.
    ///
    /// Empty means the legacy rendering: `label` and `detail` concatenated with one style
    /// and `source` right-aligned. A row with cells draws each one at its own declared
    /// width with its own style, which is what lets a status column, a lane or a badge be
    /// coloured independently of its neighbours.
    pub cells: Vec<PickerCell>,
}

impl PickerRow {
    /// A row rendered from its `label`, `detail` and `source` text, as every existing
    /// surface does.
    #[must_use]
    pub fn text(
        id: impl Into<String>,
        label: impl Into<String>,
        detail: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: detail.into(),
            source: source.into(),
            kind: PickerRowKind::Entry,
            cells: Vec::new(),
        }
    }

    /// A section header: a group label that is never an action candidate.
    #[must_use]
    pub fn section(label: impl Into<String>) -> Self {
        Self {
            id: String::new(),
            label: label.into(),
            detail: String::new(),
            source: String::new(),
            kind: PickerRowKind::Section,
            cells: Vec::new(),
        }
    }
}

/// How a cell's text sits inside its declared width.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CellAlign {
    #[default]
    Left,
    Right,
}

/// One column of a row: its own text, its own style and its own declared width.
///
/// Columns line up because the producer declares the width rather than leaving the column
/// boundary to whatever the text happens to measure. A glyph cell is a one-wide cell whose
/// text is a lane mark, a status marker or a breakpoint state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerCell {
    pub text: String,
    pub style: Style,
    pub width: u16,
    pub align: CellAlign,
}

impl PickerCell {
    #[must_use]
    pub fn new(text: impl Into<String>, style: Style, width: u16) -> Self {
        Self {
            text: text.into(),
            style,
            width,
            align: CellAlign::Left,
        }
    }

    /// A one-cell glyph column.
    #[must_use]
    pub fn glyph(text: impl Into<String>, style: Style) -> Self {
        Self::new(text, style, 1)
    }

    /// A column that takes whatever space is left after the fixed columns beside it, so a
    /// narrow surface clips the flexible column rather than the fixed ones.
    #[must_use]
    pub fn flex(text: impl Into<String>, style: Style) -> Self {
        Self::new(text, style, 0)
    }

    #[must_use]
    pub fn right(text: impl Into<String>, style: Style, width: u16) -> Self {
        Self {
            text: text.into(),
            style,
            width,
            align: CellAlign::Right,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PickerSurfaceKind {
    #[default]
    Generic,
    Problems,
    Output,
    Logs,
    Debug,
    Theme,
    /// A few choices, such as the tutor's start: a small box sized to its rows.
    Choice,
    /// The change list: grouped sections whose entries draw status, path and provenance as
    /// their own columns.
    Changes,
    /// Workspace files: each row draws its file's icon, the folder dimmed and the name.
    Files,
    /// Grep matches: each row draws its file's icon, `path:line:col` and the line's text
    /// highlighted as its own columns.
    Grep,
}

/// What a preview pane draws.
///
/// A preview carries the structure of what it previews rather than flattened text, so a diff
/// preview is a diff and a graph preview is a graph. `Text` is the plain-line rendering that
/// existing surfaces use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PickerPreviewBody {
    Text(PickerTextPreview),
    Diff(DiffPreview),
}

/// Plain preview lines with a starting line number and one emphasised line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerTextPreview {
    lines: Vec<String>,
    pub first_line: usize,
    pub highlight_line: Option<usize>,
    /// The language the lines are coloured as, when the previewed file has a grammar.
    language: Option<crate::language::syntax::SyntaxLanguage>,
    spans: Vec<Vec<(std::ops::Range<usize>, crate::config::Highlight)>>,
}

impl PickerTextPreview {
    #[must_use]
    pub fn new(
        lines: Vec<String>,
        first_line: usize,
        highlight_line: Option<usize>,
        language: Option<crate::language::syntax::SyntaxLanguage>,
    ) -> Self {
        let mut preview = Self {
            lines,
            first_line,
            highlight_line,
            language,
            spans: Vec::new(),
        };
        preview.prepare_highlighting();
        preview
    }

    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    #[must_use]
    pub fn language(&self) -> Option<crate::language::syntax::SyntaxLanguage> {
        self.language
    }

    /// Replace text and its cached byte ranges together.
    pub fn set_content(
        &mut self,
        lines: Vec<String>,
        language: Option<crate::language::syntax::SyntaxLanguage>,
    ) {
        self.lines = lines;
        self.language = language;
        self.prepare_highlighting();
    }

    fn prepare_highlighting(&mut self) {
        self.spans.clear();
        let Some(language) = self.language else {
            return;
        };
        if self
            .lines
            .iter()
            .map(|line| line.len().saturating_add(1))
            .sum::<usize>()
            > 512 * 1024
        {
            return;
        }
        let source = self.lines.join("\n");
        let mut spans = crate::language::syntax::highlight(language, &source);
        spans.sort_by_key(|span| (span.start, std::cmp::Reverse(span.end)));
        let mut per_line = vec![Vec::new(); self.lines.len()];
        let mut line = 0;
        let mut line_start = 0;
        let mut covered = 0;
        for span in spans {
            if span.start < covered || span.start >= span.end {
                continue;
            }
            covered = span.end;
            let mut start = span.start;
            while start < span.end && line < self.lines.len() {
                let line_end = line_start + self.lines[line].len();
                if start > line_end {
                    line_start = line_end + 1;
                    line += 1;
                    continue;
                }
                let end = span.end.min(line_end);
                if start < end {
                    per_line[line].push((start - line_start..end - line_start, span.role));
                }
                if span.end <= line_end {
                    break;
                }
                start = line_end + 1;
            }
        }
        self.spans = per_line;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerPreview {
    pub title: String,
    pub body: PickerPreviewBody,
}

impl PickerPreview {
    /// A preview of plain text lines.
    #[must_use]
    pub fn text(title: impl Into<String>, text: PickerTextPreview) -> Self {
        Self {
            title: title.into(),
            body: PickerPreviewBody::Text(text),
        }
    }

    /// The plain text this preview carries, when it carries text rather than structure.
    #[must_use]
    pub fn as_text(&self) -> Option<&PickerTextPreview> {
        match &self.body {
            PickerPreviewBody::Text(text) => Some(text),
            PickerPreviewBody::Diff(_) => None,
        }
    }

    /// The plain text this preview carries, mutably, so a body can be replaced in place.
    pub fn as_text_mut(&mut self) -> Option<&mut PickerTextPreview> {
        match &mut self.body {
            PickerPreviewBody::Text(text) => Some(text),
            PickerPreviewBody::Diff(_) => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerUiState {
    /// Effective interaction mode and hints derived from the accepted keymaps.
    pub input_hint: String,
    /// A surface's own footer legend, which describes the operation in front of the user
    /// rather than the widget's mode. When present it is shown instead of `input_hint`, so a
    /// surface can name what its keys do here.
    pub footer: Option<String>,
    /// How far a diff preview is scrolled, in rows. The unified layout reads the first offset.
    ///
    /// The two sides scroll INDEPENDENTLY: each offset moves its own side's sequence of
    /// cells, so one side can be moved without disturbing the other. Equal offsets keep the
    /// two sides paired.
    pub preview_side_scroll: (usize, usize),
    pub surface: PickerSurfaceKind,
    pub title: String,
    pub query: String,
    pub rows: Vec<PickerRow>,
    /// How many entries there are before the query filters them, for the `shown/total` count.
    pub total: usize,
    pub selected: Option<usize>,
    pub scroll: usize,
    pub loading: bool,
    pub preview: Option<PickerPreview>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSurfaceRow {
    pub node: u64,
    pub depth: u16,
    pub text: String,
    pub style: Style,
    pub actionable: bool,
    pub focused: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginSurfaceUiState {
    pub handle: u64,
    pub window: WindowId,
    pub tree_revision: u64,
    pub scroll: usize,
    pub rows: Vec<PluginSurfaceRow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateSurfaceCell {
    pub x: u16,
    pub y: u16,
    pub grapheme: String,
    pub continuation: bool,
    pub style: Style,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateSurfaceUiState {
    pub handle: u64,
    pub window: WindowId,
    pub dimensions_revision: u64,
    pub surface_revision: u64,
    pub width: u16,
    pub height: u16,
    pub cells: Vec<PrivateSurfaceCell>,
    pub cursor: Option<(u16, u16)>,
    pub focused: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyHintUiRow {
    pub next_key: String,
    pub label: String,
    pub icon: Option<String>,
    pub group: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyHintUiState {
    pub prefix: String,
    pub rows: Vec<KeyHintUiRow>,
}

pub use crate::ui::grid::ExplorerSegmentRole;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerUiSegment {
    pub text: String,
    pub role: ExplorerSegmentRole,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerUiRow {
    pub node_id: u64,
    pub selected: bool,
    pub segments: Vec<ExplorerUiSegment>,
    pub right_segments: Vec<ExplorerUiSegment>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerUiState {
    pub title: String,
    pub rows: Vec<ExplorerUiRow>,
    pub scroll: usize,
    pub loading: bool,
    pub total: usize,
    /// Entries whose name holds the query; the folders above them are only context.
    pub matches: usize,
    /// What `/` filters the tree by; empty shows everything.
    pub query: String,
    /// Whether keys type into the query.
    pub searching: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalPanelMode {
    Input,
    Normal,
    Visual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalPhase {
    Preflight,
    Starting,
    Running,
    Stopping,
    Exited,
    Failed,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalTabState {
    pub phase: TerminalPhase,
    pub id: u64,
    pub profile: String,
    pub running: bool,
    pub exit_code: Option<u32>,
}

pub type TerminalSelection = ((usize, usize), (usize, usize), bool);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalPanelState {
    pub id: u64,
    pub profile: String,
    pub tabs: Vec<TerminalTabState>,
    pub mode: TerminalPanelMode,
    pub running: bool,
    pub exit_code: Option<u32>,
    pub truncated: bool,
    pub focused: bool,
    pub input_hint: Option<String>,
    pub columns: u16,
    pub rows: u16,
    pub cells: Vec<Vec<crate::ui::pty_screen::PtyCell>>,
    pub cells_start: usize,
    pub cursor: Option<(usize, usize)>,
    pub selection: Option<TerminalSelection>,
    pub scrollback_lines: usize,
    pub scroll_offset: usize,
    pub requested_height: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalPanelGeometry {
    pub x: u16,
    pub width: u16,
    /// Drawn in the column left of `x`.
    pub left_border: bool,
    pub top: u16,
    pub height: u16,
    pub body_top: u16,
    pub body_height: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolMenuState {
    pub category: String,
    pub selected: usize,
    pub scroll: usize,
    pub rows: Vec<ToolMenuRow>,
    pub runtime_picker: Option<ToolRuntimePickerState>,
    /// What `/` typed: rows are filtered by name, id and language.
    pub query: String,
    pub searching: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentTabIdentity(pub ContentId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentTabKind {
    Text,
    Terminal,
    VirtualDocument,
    DeclarativeUi,
    PrivateCellSurface,
    ExternalTui,
}

impl From<ContentAdapterKind> for ContentTabKind {
    fn from(adapter: ContentAdapterKind) -> Self {
        match adapter {
            ContentAdapterKind::TextBuffer => Self::Text,
            ContentAdapterKind::TerminalSession => Self::Terminal,
            ContentAdapterKind::VirtualDocument => Self::VirtualDocument,
            ContentAdapterKind::DeclarativeUi => Self::DeclarativeUi,
            ContentAdapterKind::PrivateCellSurface => Self::PrivateCellSurface,
            ContentAdapterKind::ExternalTui => Self::ExternalTui,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContentTabDiagnosticSeverity {
    Error,
    Warning,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentTabDiagnostic {
    pub severity: ContentTabDiagnosticSeverity,
    pub count: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContentTabEntry {
    pub identity: ContentTabIdentity,
    pub kind: ContentTabKind,
    pub title: String,
    pub active: bool,
    pub dirty: bool,
    pub pinned: bool,
    pub diagnostic: Option<ContentTabDiagnostic>,
    pub icon: String,
    pub closeable: bool,
    /// None asks for the built-in projection, as used by independent tab strips.
    pub segments: Option<Vec<chrome::ChromeSegment>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditorWindowBinding {
    pub window: WindowId,
    pub view: crate::editor::ViewId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExplorerWindowUiState {
    pub view: u64,
    pub window: WindowId,
    pub content: ExplorerUiState,
    pub focused: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorWindowLayoutState {
    pub source_revision: u64,
    pub layout: TiledLayoutSnapshot,
    pub bindings: Vec<EditorWindowBinding>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeSurfaceWindowBinding {
    pub explorer: Option<WindowId>,
    pub terminal: Option<(u64, WindowId)>,
    pub dashboard: Option<WindowId>,
    pub picker: Option<WindowId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompositorLayoutState {
    pub source_revision: u64,
    pub graph_revision: u64,
    pub projection: CompositorProjection,
    pub surfaces: NativeSurfaceWindowBinding,
}

enum PluginRenderable<'a> {
    Retained(&'a PluginSurfaceUiState),
    Private(&'a PrivateSurfaceUiState),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DebugSignKind {
    Breakpoint,
    ConditionalBreakpoint,
    DisabledBreakpoint,
    Execution,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DebugSign {
    pub path: std::path::PathBuf,
    pub line: usize,
    pub kind: DebugSignKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiGitSignKind {
    Added,
    Changed,
    Deleted,
}

/// A symbol the status line can name when the cursor is on one of its lines.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiBreadcrumb {
    pub name: String,
    pub range: crate::editor::text::ByteRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiGitSign {
    pub buffer: crate::editor::BufferId,
    pub buffer_revision: u64,
    pub root_revision: u64,
    pub status_revision: u64,
    pub repository_id: crate::git::GitRepositoryId,
    pub line: usize,
    pub kind: UiGitSignKind,
    pub staged: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiLspProgress {
    pub(crate) server_key: String,
    pub(crate) token: String,
    pub server: String,
    pub title: String,
    pub message: String,
    pub percentage: Option<u8>,
    pub done: bool,
    pub(crate) completed_at: Option<Instant>,
}

/// What the operation bar shows. The actions are the ones VALID in the repository's real state,
/// so a bar can never offer an action the repository would refuse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationBarState {
    pub operation: String,
    pub phase: String,
    pub blockers: Vec<String>,
    pub actions: Vec<String>,
    pub abort_binding: Option<String>,
}

impl OperationBarState {
    /// The compact indicator the statusline carries, so the state survives where the bar cannot
    /// be drawn in full.
    #[must_use]
    pub fn indicator(&self) -> String {
        format!("{} · {}", self.operation, self.phase)
    }

    /// The bar's row at the given width. Narrow widths DROP the blockers and then shorten the
    /// phase, because the operation and the actions are what the user must not lose; if even the
    /// actions cannot fit, the cancellation is kept and named.
    #[must_use]
    pub fn row(&self, width: u16, ambiguous: AmbiguousWidth) -> String {
        let width = usize::from(width);
        let abort = self
            .abort_binding
            .as_ref()
            .map_or_else(|| "abort".to_owned(), |binding| format!("{binding} abort"));
        let actions = self
            .actions
            .iter()
            .map(|action| {
                if action == "abort" {
                    abort.as_str()
                } else {
                    action.as_str()
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        let tail = if self.actions.iter().any(|action| action == "abort") {
            abort.as_str()
        } else {
            ""
        };
        let brief = format!("{} · {}", self.operation, self.phase);
        let mut parts = vec![brief.clone()];
        if !self.blockers.is_empty() {
            parts.push(self.blockers.join(", "));
        }
        if !actions.is_empty() {
            parts.push(actions.clone());
        }
        let full = parts.join(" · ");
        if text_width(&full, ambiguous) <= width {
            return full;
        }
        let without_blockers = if actions.is_empty() {
            brief.clone()
        } else {
            format!("{brief} · {actions}")
        };
        if text_width(&without_blockers, ambiguous) <= width {
            return without_blockers;
        }
        // Reserve the real abort binding, then shorten the phase on grapheme boundaries.
        let tail_width = text_width(tail, ambiguous);
        let separator = usize::from(!tail.is_empty());
        let budget = width.saturating_sub(tail_width + separator);
        let mut used = 0;
        let head: String = brief
            .graphemes(true)
            .take_while(|grapheme| {
                used += text_width(grapheme, ambiguous);
                used <= budget
            })
            .collect();
        let compact = if tail.is_empty() {
            head
        } else if head.trim_end().is_empty() {
            tail.to_owned()
        } else {
            format!("{} {tail}", head.trim_end())
        };
        let mut used = 0;
        compact
            .graphemes(true)
            .take_while(|grapheme| {
                used += text_width(grapheme, ambiguous);
                used <= width
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct FlashOverlay {
    pub view: crate::editor::ViewId,
    pub matches: Vec<crate::editor::text::ByteRange>,
    pub labels: Vec<(crate::editor::text::ByteOffset, char)>,
}

/// Text just yanked, drawn in the search colour until `until`, as LazyVim's highlight on yank.
#[derive(Clone, Debug)]
pub struct YankHighlight {
    pub view: crate::editor::ViewId,
    /// The buffer revision the ranges belong to; an edit ends the highlight.
    pub revision: u64,
    pub ranges: Vec<crate::editor::text::ByteRange>,
    pub until: std::time::Instant,
}

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub(crate) inline_completion: Option<(crate::editor::ViewId, UiInlayHint)>,
    pub chrome: chrome::ChromeLayout,
    pub surface_content: crate::ui::render::SurfaceContentGeometry,
    pub dashboard: bool,
    pub prompt: Option<PromptState>,
    pub notification: Option<String>,
    pub notification_error: bool,
    pub notification_warning: bool,
    pub search_query: Option<String>,
    pub search_whole_word: bool,
    /// The `s` jump's matches and labels while it is active.
    pub flash: Option<FlashOverlay>,
    pub yanked: Option<YankHighlight>,
    pub config_diagnostic: Option<String>,
    pub dashboard_actions: Vec<DashboardAction>,
    /// The dashboard item under the cursor.
    pub dashboard_selected: usize,
    pub tool_menu: Option<ToolMenuState>,
    /// The Settings page, while it is open.
    pub settings: Option<SettingsPageState>,
    pub language_completion: Option<LanguageCompletionState>,
    /// Shown at the cursor while a language server is too busy loading to answer.
    pub lsp_loading: Option<String>,
    pub documentation_popup: Option<DocumentationPopupState>,
    pub diagnostics: Vec<UiDiagnostic>,
    pub expanded_diagnostic: Option<ExpandedDiagnosticState>,
    pub picker: Option<PickerUiState>,
    /// The operation bar: present only while an operation with a lifecycle is paused, so its
    /// presence is itself the statement that something is in progress.
    pub operation_bar: Option<OperationBarState>,
    /// The coordinated debug arrangement's content, when a debug session is active. Absent means
    /// no session, which is what makes the panes appear and disappear as ONE decision.
    pub debug_panes: Option<DebugPanesUiState>,
    /// Breakpoints and where the program is paused, known with or without a session.
    pub debug_signs: Vec<DebugSign>,
    pub plugin_surfaces: Vec<PluginSurfaceUiState>,
    pub private_surfaces: Vec<PrivateSurfaceUiState>,
    pub key_hints: Option<KeyHintUiState>,
    pub explorer: Option<ExplorerUiState>,
    pub explorer_views: Vec<ExplorerWindowUiState>,
    pub explorer_focused: bool,
    pub terminal_panel: Option<TerminalPanelState>,
    /// Terminals shown in their own windows beside the active one, such as a docked agent.
    pub other_terminals: Vec<(WindowId, TerminalPanelState)>,
    /// Renderer-pure content-switcher projection supplied by the application-owned window graph.
    pub content_tabs: Vec<ContentTabEntry>,
    /// Active generic tiled topology and editor adapter bindings.
    pub editor_window_layout: Option<EditorWindowLayoutState>,
    /// Current source-owned graph revision, independent from the projection.
    pub window_graph_revision: Option<u64>,
    /// Final application-supplied universal compositor projection.
    pub compositor_layout: Option<CompositorLayoutState>,
    pub syntax_spans: Vec<UiSyntaxSpan>,
    /// Sorted by start.
    pub markdown_marks: Vec<UiMarkdownMark>,
    pub lsp_semantic_spans: Vec<UiSyntaxSpan>,
    pub lsp_highlight_spans: Vec<UiSyntaxSpan>,
    pub lsp_inlay_hints: Vec<UiInlayHint>,
    /// What a `:s` being typed would put after each match.
    pub substitute_preview: Vec<UiInlayHint>,
    pub lsp_colors: Vec<UiColorSpan>,
    pub git_signs: Vec<UiGitSign>,
    pub lsp_folds: Vec<UiFold>,
    pub lsp_server: Option<String>,
    pub git_branch: Option<String>,
    pub breadcrumbs: Option<(crate::editor::BufferId, Vec<UiBreadcrumb>)>,
    pub running_tasks: usize,
    pub lsp_progress: Vec<UiLspProgress>,
    pub lsp_progress_frame: usize,
    pub syntax_folds: Vec<UiFold>,
    pub large_file_buffers: Vec<crate::editor::BufferId>,
    /// The agent panel, drawn in place of the pane that shows its view.
    pub agent_panel: Option<AgentPanelUi>,
    /// The tutor's current step, while the user is in its practice project.
    pub tutor: Option<TutorCard>,
}

/// One tutor step on a card at the bottom right, above the status line. Text in `backticks` is
/// drawn as keys.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TutorCard {
    pub title: String,
    pub text: String,
    /// The one thing to do now, after the moving pointer.
    pub press: String,
    pub pointer: String,
    /// The step was just done; the card says so before the next one comes.
    pub praised: bool,
    pub footer: String,
    /// The text the step is about, highlighted in its file.
    pub target: Option<UiSyntaxSpan>,
    /// Where the step happens, such as "greet.rs, line 12", or how to get back there.
    pub place: String,
    /// The step's file isn't the one in front.
    pub away: bool,
    /// The line the step is about, marked in the gutter.
    pub line: Option<(crate::editor::BufferId, usize)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PromptGeometry {
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
}

#[must_use]
pub fn prompt_geometry(
    terminal_width: u16,
    terminal_height: u16,
    has_error: bool,
) -> PromptGeometry {
    const WIDTH: u16 = 60;
    const MINIMUM_WIDTH: u16 = 36;
    const NARROW_TERMINAL: u16 = 40;
    const TOP_ROWS: u16 = 2;
    let margins = 2;
    let width = if terminal_width < NARROW_TERMINAL {
        terminal_width.saturating_sub(margins).max(1)
    } else {
        WIDTH
            .min(terminal_width.saturating_sub(margins))
            .max(MINIMUM_WIDTH.min(terminal_width.max(1)))
    };
    let height = if has_error { 4 } else { 3 }.min(terminal_height.max(1));
    let left = terminal_width.saturating_sub(width) / 2;
    let top = TOP_ROWS.min(terminal_height.saturating_sub(height));
    PromptGeometry {
        left,
        top,
        width,
        height,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ConfirmationLine {
    pub units: Vec<(String, u16)>,
}

#[derive(Clone, Debug)]
pub(crate) struct ConfirmationLayout {
    pub geometry: PromptGeometry,
    pub lines: Vec<ConfirmationLine>,
    pub fully_visible: bool,
}

fn confirmation_lines(text: &str, width: u16, config: &Config) -> (Vec<ConfirmationLine>, bool) {
    let policy = CellGrid::new_with_policy(
        0,
        0,
        CONTROL_CHARACTER_REPLACEMENT,
        ZERO_WIDTH_REPLACEMENT,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    let mut lines = Vec::new();
    let mut units = Vec::new();
    let mut used = 0u16;
    let mut oversized = false;
    for grapheme in text.graphemes(true) {
        if grapheme == "\n" {
            lines.push(ConfirmationLine {
                units: std::mem::take(&mut units),
            });
            used = 0;
            continue;
        }
        let projected = policy.confirmation_grapheme(grapheme);
        let unit_width = u16::try_from(policy.display_width(&projected).clamp(1, 2)).unwrap_or(1);
        if unit_width > width {
            oversized = true;
        }
        if !units.is_empty() && used.saturating_add(unit_width) > width {
            // Break after the last word that fits; a path or command with no space breaks anywhere.
            let carry = match units.iter().rposition(|(unit, _)| unit == " ") {
                Some(space) => {
                    let carry = units.split_off(space + 1);
                    units.pop();
                    carry
                }
                None => Vec::new(),
            };
            lines.push(ConfirmationLine {
                units: std::mem::replace(&mut units, carry),
            });
            used = units.iter().map(|(_, width)| *width).sum();
            if grapheme == " " && units.is_empty() {
                continue;
            }
        }
        units.push((projected, unit_width));
        used = used.saturating_add(unit_width);
    }
    lines.push(ConfirmationLine { units });
    (lines, oversized)
}

#[must_use]
pub(crate) fn confirmation_layout(
    terminal_width: u16,
    terminal_height: u16,
    config: &Config,
    prompt: &PromptState,
) -> ConfirmationLayout {
    let mut geometry = prompt_geometry(
        terminal_width,
        terminal_height,
        prompt.error.is_some() || prompt.help.is_some(),
    );
    let Some(confirmation) = &prompt.confirmation else {
        return ConfirmationLayout {
            geometry,
            lines: Vec::new(),
            fully_visible: true,
        };
    };
    let content_width = geometry.width.saturating_sub(4);
    let (lines, oversized) = confirmation_lines(&confirmation.message, content_width, config);
    let desired_height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(3);
    geometry.height = desired_height.min(terminal_height.max(1));
    // `:s///c` asks about text in the file, so its question sits above the status line,
    // clear of the match; the session scrolls the match into the rows above it.
    geometry.top = if prompt.kind == PromptKind::SubstituteConfirm {
        terminal_height.saturating_sub(geometry.height.saturating_add(1))
    } else {
        2.min(terminal_height.saturating_sub(geometry.height))
    };
    let buttons = format!(
        "[ {} ] [ {} ]",
        confirmation.positive_label, confirmation.negative_label
    );
    let buttons_width =
        u16::try_from(text_width(&buttons, config.editor.ambiguous_width)).unwrap_or(u16::MAX);
    let fully_visible = !oversized
        && geometry.width >= buttons_width.saturating_add(4)
        && desired_height <= terminal_height
        && geometry.height >= 4;
    ConfirmationLayout {
        geometry,
        lines,
        fully_visible,
    }
}

#[must_use]
pub fn prompt_geometry_for_state(
    terminal_width: u16,
    terminal_height: u16,
    config: &Config,
    prompt: &PromptState,
) -> PromptGeometry {
    let mut geometry =
        confirmation_layout(terminal_width, terminal_height, config, prompt).geometry;
    if prompt.confirmation.is_none()
        && prompt.error.is_none()
        && let Some(help) = &prompt.help
    {
        // Short prompts retain their usual box. Longer instructions first use the available
        // width, then wrap above the input instead of losing their ending at the border.
        let message_width = help
            .lines()
            .map(|line| text_width(line, config.editor.ambiguous_width))
            .max()
            .unwrap_or(0)
            .saturating_add(4);
        geometry.width = geometry.width.max(
            u16::try_from(message_width)
                .unwrap_or(u16::MAX)
                .min(terminal_width.saturating_sub(2).max(1)),
        );
        geometry.left = terminal_width.saturating_sub(geometry.width) / 2;
        let rows = wrap_plain_text(
            help,
            usize::from(geometry.width.saturating_sub(4)).max(1),
            config.editor.ambiguous_width,
        )
        .len();
        geometry.height = u16::try_from(rows)
            .unwrap_or(u16::MAX)
            .saturating_add(3)
            .min(terminal_height.max(1));
        geometry.top = geometry
            .top
            .min(terminal_height.saturating_sub(geometry.height));
    }
    geometry
}

#[must_use]
pub fn confirmation_fully_visible(
    terminal_width: u16,
    terminal_height: u16,
    config: &Config,
    prompt: &PromptState,
) -> bool {
    confirmation_layout(terminal_width, terminal_height, config, prompt).fully_visible
}

#[must_use]
pub fn prompt_surface_dimensions(
    state: &EditorState,
    ui: &UiState,
    terminal_width: u16,
    terminal_height: u16,
) -> (u16, u16) {
    let height = active_compositor(state, ui, terminal_width, terminal_height)
        .map_or(terminal_height, |compositor| {
            compositor.projection.tiled_bounds.height
        });
    (terminal_width, height)
}

/// Rows at the foot of the screen, under every dock: the status line, and above it the bar of a
/// paused operation. The dashboard has neither.
#[must_use]
pub fn screen_foot_rows(state: &EditorState, ui: &UiState, config: &Config, height: u16) -> u16 {
    if !config.ui.status_line.enabled || ui.dashboard || state.active_view().is_none() || height < 2
    {
        return 0;
    }
    1 + u16::from(ui.operation_bar.is_some() && height > 2)
}

#[must_use]
pub fn workspace_top_rows(state: &EditorState, ui: &UiState, config: &Config) -> u16 {
    let tab_line = state.editor_tabs().count() > 1;
    let buffer_line = config.ui.buffer_line.enabled
        && ui.content_tabs.len() > usize::from(config.ui.buffer_line.hide_when_single);
    u16::from(tab_line).saturating_add(u16::from(buffer_line))
}

#[must_use]
pub fn terminal_panel_geometry(
    terminal_height: u16,
    requested_height: u16,
) -> Option<TerminalPanelGeometry> {
    if terminal_height < 3 {
        return None;
    }
    let workspace_minimum =
        TERMINAL_PANEL_MINIMUM_WORKSPACE_HEIGHT.min(terminal_height.saturating_sub(3));
    let maximum_panel = terminal_height.saturating_sub(workspace_minimum);
    let height = requested_height
        .clamp(TERMINAL_PANEL_MINIMUM_HEIGHT, TERMINAL_PANEL_MAXIMUM_HEIGHT)
        .min(maximum_panel)
        .max(3);
    let top = terminal_height.saturating_sub(height);
    Some(TerminalPanelGeometry {
        x: 0,
        width: u16::MAX,
        left_border: false,
        top,
        height,
        body_top: top.saturating_add(1),
        body_height: height.saturating_sub(1),
    })
}

const NOTIFICATION_MAX_LINES: usize = 5;

fn render_error_notification(
    frame: &mut Frame,
    message: &str,
    style: Style,
    config: &Config,
    width: u16,
    height: u16,
    at_bottom: bool,
) {
    let Some(mut rect) = crate::ui::render::overlays::error_notification_geometry(width, height)
    else {
        return;
    };
    // A long message wraps instead of losing its end, which is usually the part that says why.
    let ambiguous = config.editor.ambiguous_width;
    let inner = usize::from(rect.width.saturating_sub(4)).max(1);
    let mut lines = wrap_plain_text(message, inner, ambiguous);
    // Under a picker only the rows below it are free, so the whole text waits in :messages.
    let shown = if at_bottom {
        1
    } else {
        usize::from(height.saturating_sub(rect.y + 2)).clamp(1, NOTIFICATION_MAX_LINES)
    };
    if lines.len() > shown {
        lines.truncate(shown);
        const MORE: &str = "… :messages";
        if let Some(last) = lines.last_mut() {
            while !last.is_empty() && text_width(last, ambiguous) + MORE.len() + 1 > inner {
                last.pop();
            }
            last.push(' ');
            last.push_str(MORE);
        }
    }
    let content_width = lines
        .iter()
        .map(|line| text_width(line, ambiguous))
        .max()
        .and_then(|width| u16::try_from(width).ok())
        .unwrap_or(rect.width)
        .saturating_add(4)
        .clamp(4, rect.width);
    rect.height = lines.len() as u16 + 2;
    if at_bottom {
        rect.y = height.saturating_sub(rect.height);
    }
    rect.x = rect
        .x
        .saturating_add(rect.width.saturating_sub(content_width));
    rect.width = content_width;
    let right = rect.x.saturating_add(rect.width).saturating_sub(1);
    let bottom = rect.y.saturating_add(rect.height).saturating_sub(1);
    for y in rect.y..=bottom {
        for x in rect.x..=right {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
    }
    for x in rect.x.saturating_add(1)..right {
        frame.grid.set_cell(x, rect.y, "─", style);
        frame.grid.set_cell(x, bottom, "─", style);
    }
    frame.grid.set_cell(rect.x, rect.y, "╭", style);
    frame.grid.set_cell(right, rect.y, "╮", style);
    frame.grid.set_cell(rect.x, bottom, "╰", style);
    frame.grid.set_cell(right, bottom, "╯", style);
    for (row, line) in (rect.y + 1..).zip(&lines) {
        frame.grid.set_cell(rect.x, row, "│", style);
        frame.grid.set_cell(right, row, "│", style);
        frame.grid.put_text_clipped(
            rect.x.saturating_add(2),
            row,
            line,
            style,
            right.saturating_sub(1),
        );
    }
}

fn wrap_plain_text(text: &str, width: usize, ambiguous: AmbiguousWidth) -> Vec<String> {
    text.split('\n')
        .flat_map(|line| {
            documentation::wrap_ranges(line, width, ambiguous)
                .into_iter()
                .map(|range| line[range].to_owned())
        })
        .collect()
}

/// Tutor text wrapped to `width`, each line a list of (piece, is a key) runs.
fn wrap_card_text(text: &str, width: usize, ambiguous: AmbiguousWidth) -> Vec<Vec<(String, bool)>> {
    let mut lines = Vec::new();
    let mut key = false;
    for paragraph in text.split('\n') {
        let mut line: Vec<(String, bool)> = Vec::new();
        let mut used = 0;
        for word in paragraph.split(' ').filter(|word| !word.is_empty()) {
            let space_is_key = key;
            let mut pieces = Vec::new();
            for (index, piece) in word.split('`').enumerate() {
                if index > 0 {
                    key = !key;
                }
                if !piece.is_empty() {
                    pieces.push((piece.to_owned(), key));
                }
            }
            let word_width: usize = pieces
                .iter()
                .map(|(piece, _)| text_width(piece, ambiguous))
                .sum();
            if used > 0 && used + 1 + word_width > width {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            if used > 0 {
                line.push((" ".into(), space_is_key));
                used += 1;
            }
            if word_width <= width {
                line.extend(pieces);
                used += word_width;
                continue;
            }
            // A long path breaks where it has to, so its end, the name that matters, still shows.
            for (piece, is_key) in pieces {
                let mut run = String::new();
                for grapheme in
                    unicode_segmentation::UnicodeSegmentation::graphemes(piece.as_str(), true)
                {
                    let grapheme_width = text_width(grapheme, ambiguous).max(1);
                    if used > 0 && used + grapheme_width > width {
                        if !run.is_empty() {
                            line.push((std::mem::take(&mut run), is_key));
                        }
                        lines.push(std::mem::take(&mut line));
                        used = 0;
                    }
                    run.push_str(grapheme);
                    used += grapheme_width;
                }
                if !run.is_empty() {
                    line.push((run, is_key));
                }
            }
        }
        lines.push(line);
    }
    lines
}

/// The tutor card at the bottom right, above the status line: the step's title in its border,
/// the instruction and the tutor's commands. It moves to the top while the key hints fill the
/// bottom.
/// `place` is whether the key hints are open and where an agent panel starts. The card goes to
/// the top while the hints are open, and left of the panel, or above its input when there is
/// no room beside it.
fn render_tutor_card(
    frame: &mut Frame,
    card: &TutorCard,
    config: &Config,
    width: u16,
    bottom: u16,
    place: (bool, Option<u16>),
) {
    let (hints_open, panel) = place;
    let beside = panel
        .map(|x| x.saturating_sub(2))
        .filter(|room| *room >= 30);
    let card_width = width
        .saturating_sub(2)
        .min(64)
        .min(beside.unwrap_or(u16::MAX));
    if card_width < 24 || bottom < 8 {
        return;
    }
    let ambiguous = config.editor.ambiguous_width;
    let styled = |text: &str, room: u16, plain: Style, key: Style| -> Vec<Vec<(String, Style)>> {
        wrap_card_text(text, usize::from(room), ambiguous)
            .into_iter()
            .map(|line| {
                line.into_iter()
                    .map(|(piece, is_key)| (piece, if is_key { key } else { plain }))
                    .collect()
            })
            .collect()
    };
    let mut rows = styled(
        &card.text,
        card_width - 4,
        Style::Default,
        Style::KeyHintKey,
    );
    rows.push(Vec::new());
    if !card.place.is_empty() {
        let (style, lead) = if card.away {
            (Style::Warning, "! ")
        } else {
            (Style::Accent, "▶ ")
        };
        for (index, mut line) in styled(&card.place, card_width - 6, style, Style::KeyHintKey)
            .into_iter()
            .enumerate()
        {
            line.insert(
                0,
                ((if index == 0 { lead } else { "  " }).to_owned(), style),
            );
            rows.push(line);
        }
    }
    // The instruction sits after a moving pointer; once it is done it turns into a check mark.
    let (lead, plain, key) = if card.praised {
        ("✓   ".to_owned(), Style::GitSignAdded, Style::GitSignAdded)
    } else {
        (
            format!("{} ", card.pointer),
            Style::Accent,
            Style::KeyHintKey,
        )
    };
    for (index, mut line) in styled(&card.press, card_width - 8, plain, key)
        .into_iter()
        .enumerate()
    {
        let lead = if index == 0 {
            lead.clone()
        } else {
            "    ".to_owned()
        };
        line.insert(
            0,
            (
                lead,
                if card.praised {
                    Style::GitSignAdded
                } else {
                    Style::Accent
                },
            ),
        );
        rows.push(line);
    }
    rows.push(Vec::new());
    rows.push(vec![(card.footer.clone(), Style::Muted)]);
    // Long steps lose their last rows rather than cover the whole screen.
    let card_height = u16::try_from(rows.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .min(bottom - 2);
    let top = if hints_open || panel.is_some() && beside.is_none() {
        1
    } else {
        bottom - card_height
    };
    let left = if beside.is_some() {
        1
    } else {
        width - card_width - 1
    };
    let right = left + card_width - 1;
    let last = top + card_height - 1;
    for y in top..=last {
        for x in left..=right {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
        frame.grid.set_cell(left, y, "│", Style::Accent);
        frame.grid.set_cell(right, y, "│", Style::Accent);
    }
    for x in left + 1..right {
        frame.grid.set_cell(x, top, "─", Style::Accent);
        frame.grid.set_cell(x, last, "─", Style::Accent);
    }
    frame.grid.set_cell(left, top, "╭", Style::Accent);
    frame.grid.set_cell(right, top, "╮", Style::Accent);
    frame.grid.set_cell(left, last, "╰", Style::Accent);
    frame.grid.set_cell(right, last, "╯", Style::Accent);
    let x = frame.grid.put_text_clipped(
        left + 2,
        top,
        &format!(" {} ", card.title),
        Style::Accent,
        right,
    );
    if card.praised {
        frame
            .grid
            .put_text_clipped(x, top, " ✓ Nice! ", Style::GitSignAdded, right);
    }
    for (row, y) in rows.iter().zip(top + 1..last) {
        let mut x = left + 2;
        for (piece, style) in row {
            x = frame.grid.put_text_clipped(x, y, piece, *style, right - 1);
        }
    }
}

#[must_use]
pub fn render(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    dashboard: &DashboardConfig,
    width: u16,
    height: u16,
) -> Frame {
    render_with_documentation_geometry(state, ui, config, dashboard, width, height).0
}

/// Also returns the documentation's visible and wrapped row counts for scrolling.
pub(crate) fn render_with_documentation_geometry(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    dashboard: &DashboardConfig,
    width: u16,
    height: u16,
) -> (Frame, Option<(usize, usize)>) {
    let compositor = active_compositor(state, ui, width, height);
    if compositor.is_none() {
        let mut frame = Frame {
            grid: CellGrid::new_with_policy(
                width,
                height,
                CONTROL_CHARACTER_REPLACEMENT,
                ZERO_WIDTH_REPLACEMENT,
                config.editor.ambiguous_width == AmbiguousWidth::Wide,
            ),
            cursor: None,
        };
        // Protected host prompts remain reachable even while ordinary
        // compositor state fails closed.
        if let Some(prompt) = &ui.prompt {
            render_prompt(&mut frame, prompt, config, width, height);
        }
        return (frame, None);
    }
    let terminal_geometry = compositor.and_then(|compositor| {
        let (_, window) = compositor.surfaces.terminal?;
        let projected = compositor.projection.window(window)?;
        (projected.layer != crate::windows::CompositorLayer::Floating)
            .then(|| terminal_window_geometry(projected))?
    });
    let workspace = compositor.map(|compositor| compositor.projection.workspace_bounds);
    let workspace_height = workspace.map_or(height, |bounds| bounds.height);
    let explorer_rect = compositor.and_then(|compositor| {
        let window = compositor.surfaces.explorer?;
        compositor
            .projection
            .window(window)
            .filter(|window| window.layer != crate::windows::CompositorLayer::Floating)
            .map(|window| window.rect)
    });
    let dashboard_window = compositor
        .and_then(|compositor| compositor.projection.window(compositor.surfaces.dashboard?));
    let dashboard_floating = dashboard_window
        .is_some_and(|window| window.layer == crate::windows::CompositorLayer::Floating);
    // ONE decision, so the regions cannot disagree: the source is whatever the inspectors leave,
    // and the workspace is rendered at that size rather than being squeezed into it.
    // The terminal panel is planned WITH the arrangement rather than after it: the panel's top row
    // is what the arrangement has, so the two docks cannot collide.
    let debug = ui
        .debug_panes
        .as_ref()
        .filter(|_| !ui.dashboard && state.active_view().is_some())
        .and_then(|_| {
            let area = workspace.unwrap_or(WindowRect {
                x: 0,
                y: 0,
                width,
                height,
            });
            let top = workspace_top_rows(state, ui, config);
            let end = area.y.saturating_add(area.height);
            debug_layout_in(WindowRect {
                y: area.y.saturating_add(top),
                height: end.saturating_sub(area.y).saturating_sub(top),
                ..area
            })
        });
    let content_bounds = if (ui.dashboard || state.active_view().is_none()) && !dashboard_floating {
        // The compositor already subtracts every dock, including top and bottom docks.
        workspace.map(|bounds| {
            if let Some(window) = dashboard_window
                && window.layer == crate::windows::CompositorLayer::Docked
            {
                return window.rect;
            }
            let rect = dashboard_window.map_or(bounds, |window| window.rect);
            let x = rect.x.max(bounds.x);
            let y = rect.y.max(bounds.y);
            WindowRect {
                x,
                y,
                width: rect
                    .x
                    .saturating_add(rect.width)
                    .min(bounds.x.saturating_add(bounds.width))
                    .saturating_sub(x),
                height: rect
                    .y
                    .saturating_add(rect.height)
                    .min(bounds.y.saturating_add(bounds.height))
                    .saturating_sub(y),
            }
        })
    } else {
        workspace
    };
    let content_width = content_bounds.map_or(width, |bounds| bounds.width);
    let content_height = content_bounds.map_or(workspace_height, |bounds| bounds.height);
    let content = if dashboard_floating && (ui.dashboard || state.active_view().is_none()) {
        Frame {
            grid: CellGrid::new_with_policy(
                content_width,
                content_height,
                CONTROL_CHARACTER_REPLACEMENT,
                ZERO_WIDTH_REPLACEMENT,
                config.editor.ambiguous_width == AmbiguousWidth::Wide,
            ),
            cursor: None,
        }
    } else if ui.dashboard || state.active_view().is_none() {
        render_dashboard(
            &state.workspace_root,
            &ui.dashboard_actions,
            ui.dashboard_selected,
            dashboard,
            config,
            content_width,
            content_height,
        )
    } else {
        render_workspace(state, ui, config, content_width, content_height)
    };
    let mut workspace_frame = place_workspace_frame(
        content,
        content_bounds.unwrap_or(WindowRect {
            x: 0,
            y: 0,
            width,
            height: workspace_height,
        }),
        config,
        width,
        height,
    );
    if let (Some(layout), Some(panes)) = (debug, &ui.debug_panes) {
        render_debug_panes(&mut workspace_frame, layout, panes);
    }
    render_non_tiled_editor_windows(&mut workspace_frame, state, ui, config);
    let cloned_explorer_focused = ui.explorer_views.iter().any(|view| view.focused);
    if let (Some(rect), Some(explorer)) = (explorer_rect, &ui.explorer) {
        render_explorer_window(
            &mut workspace_frame,
            explorer,
            config,
            rect,
            ui.explorer_focused && !cloned_explorer_focused,
        );
    }
    if let Some(compositor) = compositor {
        let mut explorer_views = ui.explorer_views.iter().collect::<Vec<_>>();
        explorer_views.sort_by_key(|view| {
            compositor
                .projection
                .window(view.window)
                .map_or((u16::MAX, view.view), |window| (window.z_index, view.view))
        });
        for view in explorer_views {
            if let Some(window) = compositor
                .projection
                .window(view.window)
                .filter(|window| window.layer != crate::windows::CompositorLayer::Floating)
            {
                render_explorer_window(
                    &mut workspace_frame,
                    &view.content,
                    config,
                    window.rect,
                    view.focused,
                );
            }
        }
    }
    let mut frame =
        if let (Some(terminal), Some(geometry)) = (&ui.terminal_panel, terminal_geometry) {
            dock_terminal_panel(workspace_frame, terminal, geometry, config, width, height)
        } else {
            workspace_frame
        };
    for (window, terminal) in &ui.other_terminals {
        let Some(projected) =
            compositor.and_then(|compositor| compositor.projection.window(*window))
        else {
            continue;
        };
        if projected.layer == crate::windows::CompositorLayer::Floating {
            continue;
        }
        if let Some(geometry) = terminal_window_geometry(projected) {
            frame = dock_terminal_panel(frame, terminal, geometry, config, width, height);
        }
    }
    let fullscreen = compositor.is_some_and(|compositor| {
        compositor
            .projection
            .windows
            .iter()
            .any(|window| window.layer == crate::windows::CompositorLayer::Fullscreen)
    });
    let foot_rows = screen_foot_rows(state, ui, config, height);
    if !fullscreen && foot_rows > 0 {
        if let Some(view) = state.active_view() {
            editor::render_status_line(&mut frame.grid, state, view, ui, config, width, height - 1);
        }
        if ui.operation_bar.is_some() && height > 2 {
            editor::render_operation_bar(&mut frame.grid, ui, config, width, height - 2);
        }
    }
    if let Some(compositor) = compositor {
        let mut plugin_surfaces = ui
            .plugin_surfaces
            .iter()
            .filter_map(|surface| {
                compositor
                    .projection
                    .window(surface.window)
                    .filter(|projected| {
                        projected.layer != crate::windows::CompositorLayer::Floating
                    })
                    .map(|projected| {
                        (
                            projected.z_index,
                            surface.handle,
                            PluginRenderable::Retained(surface),
                            projected.rect,
                        )
                    })
            })
            .chain(ui.private_surfaces.iter().filter_map(|surface| {
                compositor
                    .projection
                    .window(surface.window)
                    .filter(|projected| {
                        projected.layer != crate::windows::CompositorLayer::Floating
                    })
                    .map(|projected| {
                        (
                            projected.z_index,
                            surface.handle,
                            PluginRenderable::Private(surface),
                            projected.rect,
                        )
                    })
            }))
            .collect::<Vec<_>>();
        plugin_surfaces.sort_by_key(|(z_index, handle, _, _)| (*z_index, *handle));
        for (_, _, surface, rect) in plugin_surfaces {
            match surface {
                PluginRenderable::Retained(surface) => {
                    render_plugin_surface(&mut frame, surface, rect);
                }
                PluginRenderable::Private(surface) => {
                    render_private_surface(&mut frame, surface, rect);
                }
            }
        }

        // Floating ordinary windows can overlap across content kinds. Paint
        // that layer in the compositor's exact z-order rather than allowing
        // adapter-specific render passes to decide stacking.
        for projected in compositor
            .projection
            .windows
            .iter()
            .filter(|window| window.layer == crate::windows::CompositorLayer::Floating)
        {
            if render_non_tiled_editor_window(&mut frame, state, ui, config, projected) {
                continue;
            }
            if compositor.surfaces.dashboard == Some(projected.window) {
                let rect = projected.rect;
                let dashboard = render_dashboard(
                    &state.workspace_root,
                    &ui.dashboard_actions,
                    ui.dashboard_selected,
                    dashboard,
                    config,
                    rect.width,
                    rect.height,
                );
                for y in 0..rect.height {
                    for x in 0..rect.width {
                        if let Some(cell) = dashboard.grid.get(x, y)
                            && !cell.continuation
                        {
                            frame
                                .grid
                                .set_cell(rect.x + x, rect.y + y, &cell.symbol, cell.style);
                        }
                    }
                }
                if projected.focused {
                    frame.cursor = dashboard
                        .cursor
                        .map(|(x, y, shape)| (rect.x + x, rect.y + y, shape));
                }
                continue;
            }
            if compositor.surfaces.explorer == Some(projected.window) {
                if let Some(explorer) = &ui.explorer {
                    render_explorer_window(
                        &mut frame,
                        explorer,
                        config,
                        projected.rect,
                        ui.explorer_focused && !cloned_explorer_focused,
                    );
                }
                continue;
            }
            if let Some(view) = ui
                .explorer_views
                .iter()
                .find(|view| view.window == projected.window)
            {
                render_explorer_window(
                    &mut frame,
                    &view.content,
                    config,
                    projected.rect,
                    view.focused,
                );
                continue;
            }
            if compositor
                .surfaces
                .terminal
                .is_some_and(|(_, window)| window == projected.window)
            {
                if let (Some(terminal), Some(geometry)) =
                    (&ui.terminal_panel, terminal_window_geometry(*projected))
                {
                    frame = dock_terminal_panel(frame, terminal, geometry, config, width, height);
                }
                continue;
            }
            if let Some((_, terminal)) = ui
                .other_terminals
                .iter()
                .find(|(window, _)| *window == projected.window)
            {
                if let Some(geometry) = terminal_window_geometry(*projected) {
                    frame = dock_terminal_panel(frame, terminal, geometry, config, width, height);
                }
                continue;
            }
            if let Some(surface) = ui
                .plugin_surfaces
                .iter()
                .find(|surface| surface.window == projected.window)
            {
                render_plugin_surface(&mut frame, surface, projected.rect);
                continue;
            }
            if let Some(surface) = ui
                .private_surfaces
                .iter()
                .find(|surface| surface.window == projected.window)
            {
                render_private_surface(&mut frame, surface, projected.rect);
            }
        }
    }
    if let Some(tool_menu) = &ui.tool_menu {
        render_tool_menu(&mut frame, tool_menu, config, width, workspace_height);
    }
    if let Some(page) = &ui.settings {
        settings::render_settings(&mut frame, page, config, width, workspace_height);
    }
    if let Some(picker) = &ui.picker
        && let Some(rect) = compositor.and_then(|compositor| {
            let window = compositor.surfaces.picker?;
            compositor
                .projection
                .window(window)
                .map(|window| window.rect)
        })
    {
        render_picker(&mut frame, picker, config, rect, ui.surface_content.picker);
    }
    let modal_open = ui.picker.is_some() || ui.tool_menu.is_some() || ui.settings.is_some();
    let completion_menu = ui
        .language_completion
        .as_ref()
        .filter(|_| !modal_open)
        .and_then(|completion| {
            let pane = active_editor_pane_viewport(state, ui)?;
            render_language_completion(
                &mut frame,
                completion,
                config,
                WindowRect {
                    x: pane.x,
                    y: pane.y,
                    width: pane.width,
                    height: pane.height,
                },
            )
        });
    if let (Some(text), None) = (&ui.lsp_loading, completion_menu)
        && !modal_open
    {
        render_lsp_loading(&mut frame, text, config);
    }
    let overlay_bounds = workspace.unwrap_or(WindowRect {
        x: 0,
        y: 0,
        width,
        height,
    });
    let documentation_geometry = ui
        .documentation_popup
        .as_ref()
        .filter(|_| !modal_open)
        .and_then(|documentation| {
            render_documentation_popup(
                &mut frame,
                documentation,
                config,
                overlay_bounds,
                completion_menu,
            )
        });
    if let Some(key_hints) = &ui.key_hints {
        render_key_hints(
            &mut frame,
            key_hints,
            overlay_bounds,
            config.editor.ambiguous_width,
        );
    }
    // A modal's footer is at the bottom right too, and its keys matter more.
    if config.ui.lsp_progress.enabled && !ui.lsp_progress.is_empty() && !modal_open {
        render_lsp_progress(
            &mut frame,
            &ui.lsp_progress,
            ui.lsp_progress_frame,
            config,
            workspace.unwrap_or(WindowRect {
                x: 0,
                y: 0,
                width,
                height: workspace_height,
            }),
        );
    }
    // The tutor card stays on top of popups and server progress so its instruction is readable
    // while they are open, and keeps clear of an agent panel's input.
    if let Some(card) = &ui.tutor {
        let panel = ui
            .agent_panel
            .as_ref()
            .and_then(|panel| view_pane_viewport(state, ui, panel.view))
            .map(|viewport| viewport.x);
        let place = (ui.key_hints.is_some(), panel);
        render_tutor_card(&mut frame, card, config, width, height - foot_rows, place);
    }
    // Pickers use the whole screen, so a notice below one clears the terminal dock too.
    let notice_height = if modal_open { height } else { workspace_height };
    let terminal_floats = compositor.is_some_and(|compositor| {
        compositor.surfaces.terminal.is_some_and(|(_, window)| {
            compositor
                .projection
                .window(window)
                .is_some_and(|window| window.layer == crate::windows::CompositorLayer::Floating)
        })
    });
    let information = !ui.notification_error && !ui.notification_warning;
    if ui.prompt.is_some() {
        // A prompt sits where notices do, so one waits rather than being cut through.
    } else if information
        && ui.notification.is_some()
        && (terminal_floats || ui.documentation_popup.is_some())
    {
        // What the user just opened comes first; the notice stays in `:messages`.
    } else if let Some(message) = ui.notification.as_ref() {
        // Messages show in a box at the top right, like errors, and fade after the
        // configured timeout.
        let style = if ui.notification_error {
            Style::Error
        } else if ui.notification_warning {
            Style::Warning
        } else {
            Style::Accent
        };
        render_error_notification(
            &mut frame,
            message,
            style,
            config,
            width,
            notice_height,
            modal_open,
        );
    } else if let Some(message) = ui.config_diagnostic.as_ref() {
        render_error_notification(
            &mut frame,
            message,
            Style::Error,
            config,
            width,
            notice_height,
            modal_open,
        );
    }
    if let Some(prompt) = &ui.prompt {
        let prompt_height = compositor.map_or(height, |compositor| {
            compositor.projection.tiled_bounds.height
        });
        render_prompt(&mut frame, prompt, config, width, prompt_height);
    }
    (frame, documentation_geometry)
}

#[cfg(test)]
mod completion_tests;
#[cfg(test)]
mod composition_tests;
#[cfg(test)]
mod dashboard_tests;
#[cfg(test)]
mod debug_tests;
#[cfg(test)]
mod decorations_tests;
#[cfg(test)]
mod documentation_tests;
#[cfg(test)]
mod editor_tests;
#[cfg(test)]
mod explorer_tests;
#[cfg(test)]
mod picker_tests;
#[cfg(test)]
mod plugin_tests;
#[cfg(test)]
mod prompt_tests;
#[cfg(test)]
mod tab_tests;
#[cfg(test)]
mod terminal_tests;
#[cfg(test)]
mod window_tests;
use crate::ui::grid::Frame;

use crate::ui::grid::{CONTROL_CHARACTER_REPLACEMENT, ZERO_WIDTH_REPLACEMENT};

#[derive(Clone, Debug, Default)]
pub struct SurfaceContentGeometry {
    pub picker: Option<PickerContentGeometry>,
    pub explorer: Option<WindowRect>,
    pub terminal: Option<WindowRect>,
    pub documentation_rows: usize,
}

use crate::ui::render::picker::PickerContentGeometry;

#[cfg(test)]
mod tools_tests;

use crate::ui::diff::DiffPreview;
