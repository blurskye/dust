pub(crate) mod terminal;
use terminal::{active_compositor, dock_terminal_panel, render_key_hints};
mod debug;
pub(crate) mod explorer;
use debug::{debug_layout_in, render_debug_panes};
pub use debug::{DebugPane, DebugPanesUiState, debug_pane_insets, debug_pane_tab_at};
use explorer::{place_workspace_frame, render_explorer_window};
pub(crate) use explorer::explorer_segment_style;
pub(crate) mod overlays;
use super::geometry::text_width;
use overlays::{
    render_dashboard, render_documentation_popup, render_language_completion, render_lsp_loading,
    render_lsp_progress,
};
mod workspace;
use workspace::{
    render_non_tiled_editor_window, render_non_tiled_editor_windows, render_workspace,
};
mod agent;
pub(crate) mod editor;
mod settings;
pub(crate) mod surfaces;
mod visual_rows;
#[cfg(test)]
use editor::render_editor_view;
pub(crate) use editor::workspace_title;
#[cfg(test)]
use overlays::{content_tab_width, render_content_tab_line};
use surfaces::{
    render_picker, render_plugin_surface, render_private_surface, render_prompt, render_tool_menu,
};
#[cfg(test)]
use visual_rows::{VisualRowSlice, editor_line_number_digits, git_sign_column};

pub use agent::{AgentListRow, AgentPanelUi, transcript_lines};
pub use settings::{
    SettingsEdit, SettingsHit, SettingsLine, SettingsPageState, SettingsRow, SettingsValue,
    settings_hit, settings_layout, settings_line_height,
};
pub use editor::{snap_display_column, snap_display_column_forward};
pub use explorer::explorer_panel_width;
pub use overlays::content_tab_at_column;
pub use surfaces::picker_geometry;
pub(crate) use terminal::terminal_window_geometry;
pub use terminal::terminal_tab_at_column;
pub use visual_rows::{
    VisualRowAnchor, VisualRowModel, editor_content_width, editor_gutter_width,
    editor_state_content_width, editor_state_gutter_width,
};
pub use workspace::{
    PaneViewport, view_pane_viewport, active_editor_pane_viewport, active_pane_viewport, editor_pane_viewport_at,
    pane_viewports,
};

use std::time::Instant;

use crate::config::{AmbiguousWidth, Config, DashboardConfig, PromptTextConfig};
use crate::editor::EditorState;
use crate::editor::text::{ByteOffset, ByteRange};
use crate::ui::window::{
    CompositorProjection, ContentAdapterKind, ContentId, TiledLayoutSnapshot, WindowId, WindowRect,
};
use crossterm::cursor::SetCursorStyle;
use unicode_segmentation::UnicodeSegmentation;

use crate::ui::{CellGrid, Style};

pub(crate) const CONTROL_CHARACTER_REPLACEMENT: &str = "�";
pub(crate) const ZERO_WIDTH_REPLACEMENT: &str = "◌";
pub(crate) const SCRATCH_LABEL: &str = "[scratch]";
pub(crate) const TERMINAL_PANEL_MINIMUM_HEIGHT: u16 = 4;
pub(crate) const TERMINAL_PANEL_MAXIMUM_HEIGHT: u16 = 40;
pub(crate) const TERMINAL_PANEL_MINIMUM_WORKSPACE_HEIGHT: u16 = 3;
pub(crate) const TERMINAL_PANEL_RESIZE_STEP: u16 = 2;
pub(crate) const COMPLETION_MAX_ROWS: u16 = 12;
pub(crate) const COMPLETION_DETAIL_GAP: u16 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptKind {
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
    pub role: crate::language::syntax::Highlight,
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
    pub path: std::path::PathBuf,
    pub range: ByteRange,
    pub severity: crate::workspace::DiagnosticSeverity,
    pub source: String,
    pub code: Option<String>,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpandedDiagnosticState {
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
    pub spans: Vec<Vec<(std::ops::Range<usize>, crate::language::syntax::Highlight)>>,
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
    fn at(
        &self,
        mut index: usize,
    ) -> Option<(&str, &[(std::ops::Range<usize>, crate::language::syntax::Highlight)])> {
        for section in &self.sections {
            if let Some(heading) = &section.heading {
                if index == 0 {
                    return Some((heading, &[]));
                }
                index = index.saturating_sub(1);
            }
            if let Some(line) = section.lines.get(index) {
                return Some((line, section.spans.get(index).map_or(&[][..], Vec::as_slice)));
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
    pub lines: Vec<String>,
    pub first_line: usize,
    pub highlight_line: Option<usize>,
    /// The language the lines are coloured as, when the previewed file has a grammar.
    pub language: Option<crate::language::syntax::SyntaxLanguage>,
}

/// How a diff line relates to the comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

/// A byte range inside a diff line's text that changed, for intra-line emphasis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiffSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// The line's number on the old side, absent for a line the old side does not have.
    pub old_number: Option<u32>,
    /// The line's number on the new side, absent for a line the new side does not have.
    pub new_number: Option<u32>,
    pub text: String,
    pub spans: Vec<DiffSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffHunk {
    /// The `@@` header, including the enclosing section where the language provides one.
    pub header: String,
    pub lines: Vec<DiffLine>,
    /// Unchanged lines omitted immediately before this hunk, with the exact count.
    pub collapsed_before: usize,
}

/// How a diff is laid out.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiffLayout {
    /// One column, as a patch is written. This is the default.
    #[default]
    Unified,
    /// Old and new side by side, paired line for line.
    SideBySide,
}

/// One unified diff row, ready to paint: its text and the spans inside that text that
/// changed, already in the text's own coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffRow {
    pub kind: DiffRowKind,
    pub text: String,
    pub spans: Vec<DiffSpan>,
}

/// One half of a side-by-side row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffSideCell {
    pub number: Option<u32>,
    pub text: String,
    pub spans: Vec<DiffSpan>,
}

/// One side-by-side row: what the old side shows and what the new side shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffSideRow {
    pub kind: DiffRowKind,
    /// Text for a row that spans both sides, such as the provenance or a hunk header.
    pub spanning: Option<String>,
    pub old: Option<DiffSideCell>,
    pub new: Option<DiffSideCell>,
}

/// A diff carried as structure: hunks of classified lines with both line numbers and the
/// spans that changed inside a line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffPreview {
    pub hunks: Vec<DiffHunk>,
    /// The comparison this diff is relative to, e.g. `staged = HEAD -> index`.
    pub provenance: String,
    /// True when the patch was bounded and what is shown is a window onto a larger total.
    pub truncated: bool,
    /// The true number of changed lines, so a bounded view can state what it is not showing.
    pub total_lines: usize,
    pub layout: DiffLayout,
}

/// What a rendered diff row is, so painting and measurement read the same row set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffRowKind {
    Provenance,
    Collapsed,
    Header,
    Context,
    Added,
    Removed,
    Truncated,
}

impl DiffPreview {
    /// The widest line number, so both numbers align in their own columns.
    #[must_use]
    pub fn number_width(&self) -> usize {
        self.hunks
            .iter()
            .flat_map(|hunk| hunk.lines.iter())
            .flat_map(|line| [line.old_number, line.new_number])
            .flatten()
            .map(|number| number.to_string().len())
            .max()
            .unwrap_or(1)
    }

    /// The row that states how many unchanged lines a hunk skipped.
    #[must_use]
    pub fn collapsed_text(&self, count: usize) -> String {
        format!(
            "{:>number_width$}   {count} unchanged lines",
            "",
            number_width = self.number_width()
        )
    }

    /// The row that states a bounded patch is not the whole patch.
    #[must_use]
    pub fn truncated_text(&self) -> String {
        format!(
            "patch truncated; {} changed lines in total",
            self.total_lines
        )
    }

    /// The widest content this diff draws, so a surface sizes its pane from the same rows the
    /// painter uses rather than from a re-derivation that could drift.
    #[must_use]
    pub fn content_width(&self, ambiguous_width: crate::config::AmbiguousWidth) -> usize {
        let measure = |text: &str| {
            crate::ui::geometry::text_width(text, ambiguous_width)
        };
        match self.layout {
            DiffLayout::Unified => self
                .rows()
                .iter()
                .take(64)
                .map(|row| measure(&row.text))
                .max()
                .unwrap_or(28),
            DiffLayout::SideBySide => {
                let rows = self.side_by_side_rows();
                let spanning = rows
                    .iter()
                    .filter_map(|row| row.spanning.as_deref())
                    .take(64)
                    .map(measure)
                    .max()
                    .unwrap_or(0);
                let (left, right) = rows.iter().fold((0usize, 0usize), |(left, right), row| {
                    let half = |cell: &DiffSideCell| measure(&self.side_cell_text(cell).0);
                    (
                        left.max(row.old.as_ref().map_or(0, half)),
                        right.max(row.new.as_ref().map_or(0, half)),
                    )
                });
                // The two halves and the divider between them share one row.
                spanning.max(left.saturating_add(right).saturating_add(1))
            }
        }
    }

    /// The text of one unified diff line, with the changed spans moved into its coordinates.
    #[must_use]
    pub fn line_row(&self, line: &DiffLine) -> (String, Vec<DiffSpan>) {
        let number_width = self.number_width();
        let old = line.old_number.map_or_else(String::new, |n| n.to_string());
        let new = line.new_number.map_or_else(String::new, |n| n.to_string());
        let glyph = match line.kind {
            DiffLineKind::Added => '+',
            DiffLineKind::Removed => '-',
            DiffLineKind::Context => ' ',
        };
        let prefix = format!("{old:>number_width$} {new:>number_width$} {glyph} ");
        let offset = prefix.len();
        let spans = line
            .spans
            .iter()
            .map(|span| DiffSpan {
                start: span.start.saturating_add(offset),
                end: span.end.saturating_add(offset),
            })
            .collect();
        (format!("{prefix}{}", line.text), spans)
    }

    /// The text of one side-by-side half, with its number column and its changed spans in the
    /// returned text's coordinates.
    #[must_use]
    pub fn side_cell_text(&self, cell: &DiffSideCell) -> (String, Vec<DiffSpan>) {
        let number = cell.number.map_or_else(String::new, |n| n.to_string());
        let prefix = format!("{number:>width$} ", width = self.number_width());
        let offset = prefix.len();
        let spans = cell
            .spans
            .iter()
            .map(|span| DiffSpan {
                start: span.start.saturating_add(offset),
                end: span.end.saturating_add(offset),
            })
            .collect();
        (format!("{prefix}{}", cell.text), spans)
    }

    /// The rows that stay on top of the box while the rest scroll: the comparison's name.
    #[must_use]
    pub fn pinned_rows(&self) -> usize {
        usize::from(!self.provenance.is_empty())
    }

    /// The furthest a box of `height` rows can scroll.
    #[must_use]
    pub fn last_scroll(&self, height: usize) -> usize {
        let pinned = self.pinned_rows();
        self.rows()
            .len()
            .saturating_sub(pinned)
            .saturating_sub(height.saturating_sub(pinned))
    }

    /// Where a box of `height` rows opens: at the top when the first change fits, otherwise
    /// on that change with one row above it.
    #[must_use]
    pub fn opening_scroll(&self, height: usize) -> usize {
        let pinned = self.pinned_rows();
        let first_change = self
            .rows()
            .iter()
            .skip(pinned)
            .position(|row| matches!(row.kind, DiffRowKind::Added | DiffRowKind::Removed))
            .unwrap_or(0);
        if first_change < height.saturating_sub(pinned) {
            0
        } else {
            first_change.saturating_sub(1)
        }
    }

    /// Every row this diff draws in the unified layout, in order.
    ///
    /// Measurement and painting both read this, so a preview's width cannot be computed from
    /// one row set while a different one is drawn, and the spans a row carries are already in
    /// that row's own coordinates so the painter cannot shift them wrongly.
    #[must_use]
    pub fn rows(&self) -> Vec<DiffRow> {
        let mut rows = Vec::new();
        let plain = |kind: DiffRowKind, text: String| DiffRow {
            kind,
            text,
            spans: Vec::new(),
        };
        if !self.provenance.is_empty() {
            rows.push(plain(DiffRowKind::Provenance, self.provenance.clone()));
        }
        for hunk in &self.hunks {
            if hunk.collapsed_before > 0 {
                rows.push(plain(
                    DiffRowKind::Collapsed,
                    self.collapsed_text(hunk.collapsed_before),
                ));
            }
            rows.push(plain(DiffRowKind::Header, hunk.header.clone()));
            for line in &hunk.lines {
                let kind = match line.kind {
                    DiffLineKind::Added => DiffRowKind::Added,
                    DiffLineKind::Removed => DiffRowKind::Removed,
                    DiffLineKind::Context => DiffRowKind::Context,
                };
                let (text, spans) = self.line_row(line);
                rows.push(DiffRow { kind, text, spans });
            }
        }
        if self.truncated {
            rows.push(plain(DiffRowKind::Truncated, self.truncated_text()));
        }
        rows
    }

    /// Every row a side-by-side diff draws, paired line for line.
    ///
    /// The side-by-side layout reads this for both measurement and painting the same way the
    /// unified layout reads `rows`, so neither layout can be measured from one row set while
    /// a different one is drawn.
    #[must_use]
    pub fn side_by_side_rows(&self) -> Vec<DiffSideRow> {
        let spanning = |kind: DiffRowKind, text: String| DiffSideRow {
            kind,
            spanning: Some(text),
            old: None,
            new: None,
        };
        let cell = |number: Option<u32>, line: &DiffLine| DiffSideCell {
            number,
            text: line.text.clone(),
            spans: line.spans.clone(),
        };
        let mut rows = Vec::new();
        if !self.provenance.is_empty() {
            rows.push(spanning(DiffRowKind::Provenance, self.provenance.clone()));
        }
        for hunk in &self.hunks {
            if hunk.collapsed_before > 0 {
                rows.push(spanning(
                    DiffRowKind::Collapsed,
                    self.collapsed_text(hunk.collapsed_before),
                ));
            }
            rows.push(spanning(DiffRowKind::Header, hunk.header.clone()));
            let mut index = 0usize;
            while index < hunk.lines.len() {
                let line = &hunk.lines[index];
                match line.kind {
                    DiffLineKind::Context => {
                        rows.push(DiffSideRow {
                            kind: DiffRowKind::Context,
                            spanning: None,
                            old: Some(cell(line.old_number, line)),
                            new: Some(cell(line.new_number, line)),
                        });
                        index += 1;
                    }
                    DiffLineKind::Removed => {
                        let removed_start = index;
                        while index < hunk.lines.len()
                            && hunk.lines[index].kind == DiffLineKind::Removed
                        {
                            index += 1;
                        }
                        let added_start = index;
                        while index < hunk.lines.len()
                            && hunk.lines[index].kind == DiffLineKind::Added
                        {
                            index += 1;
                        }
                        let removed = &hunk.lines[removed_start..added_start];
                        let added = &hunk.lines[added_start..index];
                        // A change block pairs its removed and added lines so the two sides
                        // stay in step; an unpaired line leaves the other side blank.
                        for pair in 0..removed.len().max(added.len()) {
                            rows.push(DiffSideRow {
                                kind: DiffRowKind::Removed,
                                spanning: None,
                                old: removed.get(pair).map(|l| cell(l.old_number, l)),
                                new: added.get(pair).map(|l| cell(l.new_number, l)),
                            });
                        }
                    }
                    DiffLineKind::Added => {
                        rows.push(DiffSideRow {
                            kind: DiffRowKind::Added,
                            spanning: None,
                            old: None,
                            new: Some(cell(line.new_number, line)),
                        });
                        index += 1;
                    }
                }
            }
        }
        if self.truncated {
            rows.push(spanning(DiffRowKind::Truncated, self.truncated_text()));
        }
        rows
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExplorerSegmentRole {
    Tree,
    Icon,
    IconAzure,
    IconBlue,
    IconCyan,
    IconGreen,
    IconGrey,
    IconOrange,
    IconPurple,
    IconRed,
    IconYellow,
    Directory,
    File,
    Symlink,
    Loading,
    Git,
    Diagnostic,
}

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
    pub columns: u16,
    pub rows: u16,
    pub cells: Vec<Vec<super::pty_screen::PtyCell>>,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EditorWindowBinding {
    pub window: WindowId,
    pub pane: crate::editor::PaneId,
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
    pub repository_id: crate::workspace::git::GitRepositoryId,
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
    pub fn row(&self, width: u16) -> String {
        let width = usize::from(width);
        let actions = self.actions.join("/");
        // Cancellation is the one thing that must survive every width, so it is reserved before
        // anything else is allowed to occupy the row.
        let tail = if self.actions.iter().any(|action| action == "abort") {
            "esc cancel"
        } else {
            ""
        };
        let mut parts = vec![self.operation.clone(), self.phase.clone()];
        if !self.blockers.is_empty() {
            // Blockers are named by PATH, which is what tells the user what to go and fix.
            parts.push(self.blockers.join(", "));
        }
        parts.push(actions);
        let full = parts.join(" · ");
        if full.chars().count() <= width {
            return full;
        }
        // Drop the blockers first, then shorten the phase, and always keep the cancellation.
        let brief = format!("{} · {}", self.operation, self.phase);
        let tail_width = tail.chars().count();
        let separator = usize::from(!tail.is_empty());
        if brief.chars().count() + separator + tail_width <= width {
            return if tail.is_empty() {
                brief
            } else {
                format!("{brief} {tail}")
            };
        }
        let budget = width
            .saturating_sub(tail_width)
            .saturating_sub(separator);
        let mut head: String = brief.chars().take(budget).collect();
        while head.ends_with(' ') {
            head.pop();
        }
        if tail.is_empty() {
            head
        } else {
            format!("{head} {tail}")
        }
    }
}

#[derive(Clone, Debug)]
pub struct FlashOverlay {
    pub view: crate::editor::ViewId,
    pub matches: Vec<crate::editor::text::ByteRange>,
    pub labels: Vec<(crate::editor::text::ByteOffset, char)>,
}

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub surface_content: crate::ui::geometry::SurfaceContentGeometry,
    pub dashboard: bool,
    pub prompt: Option<PromptState>,
    pub notification: Option<String>,
    pub notification_error: bool,
    pub notification_warning: bool,
    pub search_query: Option<String>,
    /// The `s` jump's matches and labels while it is active.
    pub flash: Option<FlashOverlay>,
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
    /// Renderer-pure content-switcher projection supplied by the application-owned window graph.
    pub content_tabs: Vec<ContentTabEntry>,
    /// Active generic tiled topology and editor adapter bindings.
    pub editor_window_layout: Option<EditorWindowLayoutState>,
    /// Current source-owned graph revision, independent from the projection.
    pub window_graph_revision: Option<u64>,
    /// Final application-supplied universal compositor projection.
    pub compositor_layout: Option<CompositorLayoutState>,
    pub syntax_spans: Vec<UiSyntaxSpan>,
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

#[derive(Clone, Debug)]
pub struct Frame {
    pub grid: CellGrid,
    pub cursor: Option<(u16, u16, SetCursorStyle)>,
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
        &CONTROL_CHARACTER_REPLACEMENT,
        &ZERO_WIDTH_REPLACEMENT,
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
    confirmation_layout(terminal_width, terminal_height, config, prompt).geometry
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
    if !config.ui.status_line.enabled || ui.dashboard || state.active_view.is_none() || height < 2 {
        return 0;
    }
    1 + u16::from(ui.operation_bar.is_some() && height > 2)
}

#[must_use]
pub fn workspace_top_rows(state: &EditorState, ui: &UiState, config: &Config) -> u16 {
    let tab_line = state.tabs.len() > 1;
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
    let Some(mut rect) = super::geometry::error_notification_geometry(width, height) else {
        return;
    };
    // A long message wraps instead of losing its end, which is usually the part that says why.
    let ambiguous = config.editor.ambiguous_width;
    let inner = usize::from(rect.width.saturating_sub(4)).max(1);
    let mut lines: Vec<String> = if text_width(message, ambiguous) <= inner {
        vec![message.to_owned()]
    } else {
        wrap_card_text(message, inner, ambiguous)
            .into_iter()
            .map(|runs| runs.into_iter().map(|(piece, _)| piece).collect())
            .collect()
    };
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
        frame
            .grid
            .put_text_clipped(rect.x.saturating_add(2), row, line, style, right.saturating_sub(1));
    }
}

/// Words of `text` wrapped to `width`, each line a list of (piece, is a key) runs.
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
            let word_width: usize = pieces.iter().map(|(piece, _)| text_width(piece, ambiguous)).sum();
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
                for grapheme in unicode_segmentation::UnicodeSegmentation::graphemes(piece.as_str(), true) {
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
fn render_tutor_card(frame: &mut Frame, card: &TutorCard, config: &Config, width: u16, bottom: u16, place: (bool, Option<u16>)) {
    let (hints_open, panel) = place;
    let beside = panel.map(|x| x.saturating_sub(2)).filter(|room| *room >= 30);
    let card_width = width.saturating_sub(2).min(64).min(beside.unwrap_or(u16::MAX));
    if card_width < 24 || bottom < 8 {
        return;
    }
    let ambiguous = config.editor.ambiguous_width;
    let styled = |text: &str, room: u16, plain: Style, key: Style| -> Vec<Vec<(String, Style)>> {
        wrap_card_text(text, usize::from(room), ambiguous)
            .into_iter()
            .map(|line| line.into_iter().map(|(piece, is_key)| (piece, if is_key { key } else { plain })).collect())
            .collect()
    };
    let mut rows = styled(&card.text, card_width - 4, Style::Default, Style::KeyHintKey);
    rows.push(Vec::new());
    if !card.place.is_empty() {
        let (style, lead) = if card.away { (Style::Warning, "! ") } else { (Style::Accent, "▶ ") };
        for (index, mut line) in styled(&card.place, card_width - 6, style, Style::KeyHintKey).into_iter().enumerate() {
            line.insert(0, ((if index == 0 { lead } else { "  " }).to_owned(), style));
            rows.push(line);
        }
    }
    // The instruction sits after a moving pointer; once it is done it turns into a check mark.
    let (lead, plain, key) = if card.praised {
        ("✓   ".to_owned(), Style::GitSignAdded, Style::GitSignAdded)
    } else {
        (format!("{} ", card.pointer), Style::Accent, Style::KeyHintKey)
    };
    for (index, mut line) in styled(&card.press, card_width - 8, plain, key).into_iter().enumerate() {
        let lead = if index == 0 { lead.clone() } else { "    ".to_owned() };
        line.insert(0, (lead, if card.praised { Style::GitSignAdded } else { Style::Accent }));
        rows.push(line);
    }
    rows.push(Vec::new());
    rows.push(vec![(card.footer.clone(), Style::Muted)]);
    // Long steps lose their last rows rather than cover the whole screen.
    let card_height = u16::try_from(rows.len()).unwrap_or(u16::MAX).saturating_add(2).min(bottom - 2);
    let top = if hints_open || panel.is_some() && beside.is_none() { 1 } else { bottom - card_height };
    let left = if beside.is_some() { 1 } else { width - card_width - 1 };
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
    let x = frame.grid.put_text_clipped(left + 2, top, &format!(" {} ", card.title), Style::Accent, right);
    if card.praised {
        frame.grid.put_text_clipped(x, top, " ✓ Nice! ", Style::GitSignAdded, right);
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
    let compositor = active_compositor(state, ui, width, height);
    if compositor.is_none() {
        let mut frame = Frame {
            grid: CellGrid::new_with_policy(
                width,
                height,
                &CONTROL_CHARACTER_REPLACEMENT,
                &ZERO_WIDTH_REPLACEMENT,
                config.editor.ambiguous_width == AmbiguousWidth::Wide,
            ),
            cursor: None,
        };
        // Protected host prompts remain reachable even while ordinary
        // compositor state fails closed.
        if let Some(prompt) = &ui.prompt {
            render_prompt(&mut frame, prompt, config, width, height);
        }
        return frame;
    }
    let terminal_geometry = compositor.and_then(|compositor| {
        let (_, window) = compositor.surfaces.terminal?;
        let projected = compositor.projection.window(window)?;
        (projected.layer != crate::ui::window::CompositorLayer::Floating)
            .then(|| terminal_window_geometry(projected))?
    });
    let workspace = compositor.map(|compositor| compositor.projection.workspace_bounds);
    let workspace_height = workspace.map_or(height, |bounds| bounds.height);
    let explorer_rect = compositor.and_then(|compositor| {
        let window = compositor.surfaces.explorer?;
        compositor
            .projection
            .window(window)
            .filter(|window| window.layer != crate::ui::window::CompositorLayer::Floating)
            .map(|window| window.rect)
    });
    let terminal_rect = compositor.and_then(|compositor| {
        let (_, window) = compositor.surfaces.terminal?;
        compositor
            .projection
            .window(window)
            .filter(|window| window.layer != crate::ui::window::CompositorLayer::Floating)
            .map(|window| window.rect)
    });
    let dashboard_rect = compositor.and_then(|compositor| {
        let window = compositor.surfaces.dashboard?;
        compositor
            .projection
            .window(window)
            .map(|window| window.rect)
    });
    // ONE decision, so the regions cannot disagree: the source is whatever the inspectors leave,
    // and the workspace is rendered at that size rather than being squeezed into it.
    // The terminal panel is planned WITH the arrangement rather than after it: the panel's top row
    // is what the arrangement has, so the two docks cannot collide.
    let debug_height = ui
        .terminal_panel
        .as_ref()
        .and(terminal_geometry.as_ref())
        .map_or(height, |geometry| geometry.top);
    let debug = ui
        .debug_panes
        .as_ref()
        .filter(|_| !ui.dashboard && state.active_view.is_some())
        .and_then(|_| {
            let area = workspace.unwrap_or(WindowRect {
                x: 0,
                y: 0,
                width,
                height,
            });
            let top = workspace_top_rows(state, ui, config);
            let end = area.y.saturating_add(area.height).min(debug_height);
            debug_layout_in(WindowRect {
                y: area.y.saturating_add(top),
                height: end.saturating_sub(area.y).saturating_sub(top),
                ..area
            })
        });
    let content_bounds = if ui.dashboard || state.active_view.is_none() {
        // The dashboard centres itself in what a docked explorer, or a terminal docked at the
        // side such as an agent's, leaves free.
        let beside = |bounds: WindowRect, dock: Option<WindowRect>| match dock {
            Some(dock) if dock.x <= bounds.x && dock.x + dock.width > bounds.x => {
                let cut = (dock.x + dock.width - bounds.x).min(bounds.width);
                WindowRect {
                    x: bounds.x + cut,
                    width: bounds.width - cut,
                    ..bounds
                }
            }
            Some(dock) if dock.x > bounds.x && dock.x < bounds.x + bounds.width => WindowRect {
                width: dock.x - bounds.x,
                ..bounds
            },
            _ => bounds,
        };
        let side_terminal =
            terminal_rect.filter(|dock| dock.y <= workspace.map_or(0, |bounds| bounds.y));
        dashboard_rect
            .or(workspace)
            .map(|bounds| beside(beside(bounds, explorer_rect), side_terminal))
    } else {
        workspace
    };
    let content_width = content_bounds.map_or(width, |bounds| bounds.width);
    let content_height = content_bounds.map_or(workspace_height, |bounds| bounds.height);
    let content = if ui.dashboard || state.active_view.is_none() {
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
                .filter(|window| window.layer != crate::ui::window::CompositorLayer::Floating)
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
    let fullscreen = compositor.is_some_and(|compositor| {
        compositor
            .projection
            .windows
            .iter()
            .any(|window| window.layer == crate::ui::window::CompositorLayer::Fullscreen)
    });
    let foot_rows = screen_foot_rows(state, ui, config, height);
    if !fullscreen && foot_rows > 0 {
        if let Some(view) = state.active_view {
            editor::render_status_line(&mut frame.grid, state, view, ui, config, width, height - 1);
        }
        if ui.operation_bar.is_some() && height > 2 {
            editor::render_operation_bar(&mut frame.grid, ui, width, height - 2);
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
                        projected.layer != crate::ui::window::CompositorLayer::Floating
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
                        projected.layer != crate::ui::window::CompositorLayer::Floating
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
            .filter(|window| window.layer == crate::ui::window::CompositorLayer::Floating)
        {
            if render_non_tiled_editor_window(&mut frame, state, ui, config, projected) {
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
                if let (Some(terminal), Some(geometry)) = (
                    &ui.terminal_panel,
                    terminal_window_geometry(*projected),
                ) {
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
    if let Some(picker) = &ui.picker {
        if let Some(rect) = compositor.and_then(|compositor| {
            let window = compositor.surfaces.picker?;
            compositor
                .projection
                .window(window)
                .map(|window| window.rect)
        }) {
            render_picker(&mut frame, picker, config, rect, ui.surface_content.picker);
        }
    }
    let completion_menu = ui.language_completion.as_ref().and_then(|completion| {
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
    if let (Some(text), None) = (&ui.lsp_loading, completion_menu) {
        render_lsp_loading(&mut frame, text, config);
    }
    if let Some(documentation) = &ui.documentation_popup {
        render_documentation_popup(
            &mut frame,
            documentation,
            config,
            workspace.map_or(0, |bounds| bounds.x),
            content_width,
            workspace_height,
            completion_menu,
        );
    }
    if let Some(key_hints) = &ui.key_hints {
        render_key_hints(
            &mut frame,
            key_hints,
            workspace.map_or(0, |bounds| bounds.x),
            content_width,
            workspace_height,
        );
    }
    let modal_open = ui.picker.is_some() || ui.tool_menu.is_some() || ui.settings.is_some();
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
                .is_some_and(|window| window.layer == crate::ui::window::CompositorLayer::Floating)
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
        render_error_notification(&mut frame, message, style, config, width, notice_height, modal_open);
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
    frame
}

#[cfg(test)]
mod tests;

