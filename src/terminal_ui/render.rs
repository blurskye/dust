use std::path::Path;

use crate::configuration::{
    AmbiguousWidth, BorderStyle, CommandLineConfig, Config, CursorShape, DashboardConfig,
    EditorConfig, KeyHintAnchor, LineNumbers, PromptTextConfig,
};
use crate::document::{
    ByteOffset, ByteRange, DisplayWidthPolicy, PositionCodec, byte_range_for_display_columns,
    display_column_at,
};
use crate::editing::{EditorMode, EditorState, SelectionKind};
use crossterm::cursor::SetCursorStyle;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::terminal_ui::{CellGrid, Style};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PromptKind {
    Command,
    SearchForward,
    SearchBackward,
    WorkspaceTrust,
}

impl PromptKind {
    #[must_use]
    pub fn title(self, text: &PromptTextConfig) -> &str {
        match self {
            Self::Command => &text.command_title,
            Self::SearchForward => &text.search_forward_title,
            Self::SearchBackward => &text.search_backward_title,
            Self::WorkspaceTrust => &text.workspace_trust_title,
        }
    }

    #[must_use]
    pub fn marker(self, text: &PromptTextConfig) -> &str {
        match self {
            Self::Command => &text.command_marker,
            Self::SearchForward => &text.search_forward_marker,
            Self::SearchBackward => &text.search_backward_marker,
            Self::WorkspaceTrust => &text.workspace_trust_marker,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionItem {
    pub label: String,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptState {
    pub kind: PromptKind,
    pub input: String,
    pub cursor: usize,
    pub error: Option<String>,
    pub completions: Vec<CompletionItem>,
    pub selected_completion: Option<usize>,
}

impl PromptState {
    #[must_use]
    pub fn new(kind: PromptKind) -> Self {
        Self {
            kind,
            input: String::new(),
            cursor: 0,
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
    pub key: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolMenuRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub languages: String,
    pub status: String,
    pub installed: bool,
    pub can_install: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiSyntaxSpan {
    pub buffer: crate::editing::BufferId,
    pub revision: crate::document::BufferRevision,
    pub range: ByteRange,
    pub role: crate::syntax::SyntaxRole,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiInlayHint {
    pub buffer: crate::editing::BufferId,
    pub revision: crate::document::BufferRevision,
    pub position: ByteOffset,
    pub label: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiFold {
    pub view: crate::editing::ViewId,
    pub start_line: usize,
    pub end_line: usize,
    pub collapsed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiDiagnostic {
    pub path: std::path::PathBuf,
    pub range: ByteRange,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentationPopupKind {
    Hover,
    Signature,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentationPopupState {
    pub kind: DocumentationPopupKind,
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageCompletionState {
    pub items: Vec<CompletionItem>,
    pub selected: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerRow {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub source: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerUiState {
    pub title: String,
    pub query: String,
    pub rows: Vec<PickerRow>,
    pub selected: Option<usize>,
    pub scroll: usize,
    pub loading: bool,
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
    pub preview_lines: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalPanelState {
    pub id: u64,
    pub profile: String,
    pub running: bool,
    pub exit_code: Option<u32>,
    pub truncated: bool,
    pub unsupported_sequences: u64,
    pub focused: bool,
    pub columns: u16,
    pub rows: u16,
    pub cells: Vec<Vec<super::pty_screen::PtyCell>>,
    pub cursor: Option<(u16, u16)>,
    pub scrollback_lines: usize,
    pub scroll_offset: usize,
    pub requested_height: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalPanelGeometry {
    pub top: u16,
    pub height: u16,
    pub body_top: u16,
    pub body_height: u16,
    pub status_y: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolMenuState {
    pub category: String,
    pub selected: usize,
    pub scroll: usize,
    pub rows: Vec<ToolMenuRow>,
}

#[derive(Clone, Debug, Default)]
pub struct UiState {
    pub dashboard: bool,
    pub prompt: Option<PromptState>,
    pub notification: Option<String>,
    pub notification_error: bool,
    pub search_query: Option<String>,
    pub config_diagnostic: Option<String>,
    pub dashboard_actions: Vec<DashboardAction>,
    pub tool_menu: Option<ToolMenuState>,
    pub language_completion: Option<LanguageCompletionState>,
    pub documentation_popup: Option<DocumentationPopupState>,
    pub diagnostics: Vec<UiDiagnostic>,
    pub picker: Option<PickerUiState>,
    pub key_hints: Option<KeyHintUiState>,
    pub explorer: Option<ExplorerUiState>,
    pub explorer_focused: bool,
    pub terminal_panel: Option<TerminalPanelState>,
    pub syntax_spans: Vec<UiSyntaxSpan>,
    pub lsp_semantic_spans: Vec<UiSyntaxSpan>,
    pub lsp_highlight_spans: Vec<UiSyntaxSpan>,
    pub lsp_inlay_hints: Vec<UiInlayHint>,
    pub lsp_folds: Vec<UiFold>,
    pub lsp_server: Option<String>,
    pub syntax_folds: Vec<UiFold>,
    pub large_file_buffers: Vec<crate::editing::BufferId>,
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
    configured: &CommandLineConfig,
    has_error: bool,
) -> PromptGeometry {
    let margins = configured.outer_margin.saturating_mul(2);
    let width = if terminal_width < configured.narrow_width_threshold {
        terminal_width.saturating_sub(margins).max(1)
    } else {
        configured
            .width
            .min(terminal_width.saturating_sub(margins))
            .max(configured.minimum_width.min(terminal_width.max(1)))
    };
    let height = if has_error { 4 } else { 3 }.min(terminal_height.max(1));
    let left = terminal_width.saturating_sub(width) / 2;
    let top = configured
        .top_rows
        .min(terminal_height.saturating_sub(height));
    PromptGeometry {
        left,
        top,
        width,
        height,
    }
}

#[must_use]
pub fn terminal_panel_geometry(
    config: &Config,
    terminal_height: u16,
    requested_height: u16,
) -> Option<TerminalPanelGeometry> {
    if terminal_height < 3 {
        return None;
    }
    let policy = &config.ui.terminal_panel;
    let workspace_minimum = policy
        .minimum_workspace_height
        .min(terminal_height.saturating_sub(3));
    let maximum_panel = terminal_height.saturating_sub(workspace_minimum);
    let height = requested_height
        .clamp(policy.minimum_height, policy.maximum_height)
        .min(maximum_panel)
        .max(3);
    let top = terminal_height.saturating_sub(height);
    Some(TerminalPanelGeometry {
        top,
        height,
        body_top: top.saturating_add(1),
        body_height: height.saturating_sub(2),
        status_y: terminal_height.saturating_sub(1),
    })
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
    let terminal_geometry = ui
        .terminal_panel
        .as_ref()
        .and_then(|terminal| terminal_panel_geometry(config, height, terminal.requested_height));
    let workspace_height = terminal_geometry.map_or(height, |geometry| geometry.top);
    let explorer_width = ui
        .explorer
        .as_ref()
        .and_then(|_| explorer_panel_width(config, width));
    let content_width =
        explorer_width.map_or(width, |panel| width.saturating_sub(panel).saturating_sub(1));
    let content = if ui.dashboard || state.active_view.is_none() {
        render_dashboard(
            &state.workspace_root,
            &ui.dashboard_actions,
            dashboard,
            config,
            content_width,
            workspace_height,
        )
    } else {
        render_workspace(state, ui, config, content_width, workspace_height)
    };
    let workspace = if let (Some(panel_width), Some(explorer)) = (explorer_width, &ui.explorer) {
        dock_explorer(
            content,
            explorer,
            state,
            ui,
            config,
            panel_width,
            width,
            workspace_height,
        )
    } else {
        content
    };
    let mut frame =
        if let (Some(terminal), Some(geometry)) = (&ui.terminal_panel, terminal_geometry) {
            dock_terminal_panel(workspace, terminal, geometry, config, width, height)
        } else {
            workspace
        };
    if let Some(tool_menu) = &ui.tool_menu {
        render_tool_menu(&mut frame, tool_menu, config, width, workspace_height);
    }
    if let Some(picker) = &ui.picker {
        render_picker(&mut frame, picker, config, width, workspace_height);
    }
    if let Some(completion) = &ui.language_completion {
        render_language_completion(&mut frame, completion, config, width, workspace_height);
    }
    if let Some(documentation) = &ui.documentation_popup {
        render_documentation_popup(
            &mut frame,
            documentation,
            config,
            explorer_width.map_or(0, |panel| panel.saturating_add(1)),
            content_width,
            workspace_height,
        );
    }
    if let Some(key_hints) = &ui.key_hints {
        render_key_hints(
            &mut frame,
            key_hints,
            config,
            explorer_width.map_or(0, |panel| panel.saturating_add(1)),
            content_width,
            workspace_height,
        );
    }
    let message = ui
        .notification
        .as_ref()
        .map(|message| {
            (
                message,
                if ui.notification_error {
                    Style::Error
                } else {
                    Style::Accent
                },
            )
        })
        .or_else(|| {
            ui.config_diagnostic
                .as_ref()
                .map(|message| (message, Style::Error))
        });
    if let Some((message, style)) = message {
        let y = workspace_height.saturating_sub(config.ui.notifications.rows_from_bottom);
        let available = usize::from(
            config
                .ui
                .notifications
                .max_width
                .min(width.saturating_sub(config.ui.notifications.left)),
        );
        let message = message.chars().take(available).collect::<String>();
        frame
            .grid
            .put_text(config.ui.notifications.left, y, &message, style);
    }
    if let Some(prompt) = &ui.prompt {
        render_prompt(&mut frame, prompt, config, width, workspace_height);
    }
    frame
}

fn dock_terminal_panel(
    workspace: Frame,
    terminal: &TerminalPanelState,
    geometry: TerminalPanelGeometry,
    config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &config.editor.control_character_replacement,
        &config.editor.zero_width_replacement,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    for y in 0..workspace.grid.height().min(geometry.top) {
        for x in 0..workspace.grid.width().min(width) {
            let Some(cell) = workspace.grid.get(x, y) else {
                continue;
            };
            if !cell.continuation {
                grid.set_cell(x, y, &cell.symbol, cell.style);
            }
        }
    }
    for y in geometry.top..height {
        for x in 0..width {
            grid.set_cell(x, y, " ", Style::TerminalBackground);
        }
    }
    for x in 0..width {
        grid.set_cell(
            x,
            geometry.top,
            &config.ui.terminal_panel.divider,
            Style::TerminalBorder,
        );
    }
    let title = config
        .ui
        .terminal_panel
        .title_format
        .replace("{id}", &terminal.id.to_string())
        .replace("{profile}", &terminal.profile);
    grid.put_text_clipped(1, geometry.top, &title, Style::TerminalTitle, width);

    let source_rows = terminal.cells.len().min(usize::from(terminal.rows));
    let visible_rows = usize::from(geometry.body_height).min(source_rows);
    let source_start = source_rows.saturating_sub(visible_rows);
    for (screen_row, row) in terminal
        .cells
        .iter()
        .skip(source_start)
        .take(visible_rows)
        .enumerate()
    {
        let y = geometry
            .body_top
            .saturating_add(u16::try_from(screen_row).unwrap_or(u16::MAX));
        for (column, cell) in row
            .iter()
            .take(usize::from(width.min(terminal.columns)))
            .enumerate()
        {
            if cell.continuation {
                continue;
            }
            grid.set_cell(
                u16::try_from(column).unwrap_or(u16::MAX),
                y,
                &cell.text,
                Style::Terminal(cell.style),
            );
        }
    }

    for x in 0..width {
        grid.set_cell(x, geometry.status_y, " ", Style::TerminalStatus);
    }
    let status = if terminal.running {
        config.ui.terminal_panel.running_label.clone()
    } else {
        config.ui.terminal_panel.exited_format.replace(
            "{code}",
            &terminal
                .exit_code
                .map_or_else(|| "signal".into(), |code| code.to_string()),
        )
    };
    let focus = if terminal.focused {
        &config.ui.terminal_panel.focused_label
    } else {
        &config.ui.terminal_panel.unfocused_label
    };
    let status_line = config
        .ui
        .terminal_panel
        .status_format
        .replace("{focus}", focus)
        .replace("{status}", &status)
        .replace("{unsupported}", &terminal.unsupported_sequences.to_string())
        .replace(
            "{truncated}",
            if terminal.truncated {
                &config.ui.terminal_panel.truncated_label
            } else {
                ""
            },
        );
    grid.put_text_clipped(
        0,
        geometry.status_y,
        &status_line,
        if terminal.focused {
            Style::TerminalStatusAccent
        } else {
            Style::TerminalStatus
        },
        width,
    );

    let cursor = if terminal.focused {
        terminal.cursor.and_then(|(column, row)| {
            let source_row = usize::from(row);
            let projected_row = source_row.checked_sub(source_start)?;
            let y = geometry
                .body_top
                .saturating_add(u16::try_from(projected_row).ok()?);
            (column < width && y < geometry.status_y).then_some((
                column,
                y,
                crossterm::cursor::SetCursorStyle::SteadyBlock,
            ))
        })
    } else {
        workspace.cursor
    };
    Frame { grid, cursor }
}

fn render_key_hints(
    frame: &mut Frame,
    hints: &KeyHintUiState,
    config: &Config,
    editor_left: u16,
    editor_width: u16,
    terminal_height: u16,
) {
    let policy = &config.ui.key_hints;
    let available_height = terminal_height.saturating_sub(1);
    if editor_width < 12 || available_height < 4 || hints.rows.is_empty() {
        return;
    }
    let row_count = hints
        .rows
        .len()
        .min(usize::from(policy.max_rows).saturating_mul(usize::from(policy.max_columns)));
    let columns = usize::from(policy.max_columns)
        .min(row_count.div_ceil(usize::from(policy.max_rows)))
        .max(1);
    let rows_per_column = row_count
        .div_ceil(columns)
        .min(usize::from(policy.max_rows));
    let row_stride = usize::from(policy.row_gap) + 1;
    let body_height = rows_per_column
        .saturating_mul(row_stride)
        .saturating_sub(usize::from(policy.row_gap));
    let desired_height = 3usize
        .saturating_add(usize::from(policy.vertical_padding).saturating_mul(2))
        .saturating_add(body_height);
    let box_height = u16::try_from(desired_height)
        .unwrap_or(available_height)
        .min(available_height);
    if box_height < 4 {
        return;
    }
    let widest = hints
        .rows
        .iter()
        .take(row_count)
        .map(|row| {
            UnicodeWidthStr::width(row.next_key.as_str())
                + UnicodeWidthStr::width(row.label.as_str())
                + row
                    .icon
                    .as_deref()
                    .map_or(0, |icon| UnicodeWidthStr::width(icon) + 1)
                + 3
        })
        .max()
        .unwrap_or(1);
    let desired_width = widest
        .saturating_mul(columns)
        .saturating_add(usize::from(policy.column_gap).saturating_mul(columns.saturating_sub(1)))
        .saturating_add(usize::from(policy.horizontal_padding).saturating_mul(2))
        .saturating_add(2);
    let box_width = u16::try_from(desired_width)
        .unwrap_or(policy.max_width)
        .min(policy.max_width)
        .min(editor_width);
    if box_width < 12 {
        return;
    }
    let left = match policy.anchor {
        KeyHintAnchor::BottomLeft => editor_left,
        KeyHintAnchor::BottomRight => {
            editor_left.saturating_add(editor_width.saturating_sub(box_width))
        }
    };
    let top = available_height.saturating_sub(box_height);
    let right = left.saturating_add(box_width);
    let bottom = top.saturating_add(box_height).saturating_sub(1);
    for y in top..=bottom {
        for x in left..right {
            frame.grid.set_cell(x, y, " ", Style::KeyHintBackground);
        }
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) = match policy.border
    {
        BorderStyle::Double => ('═', '║', '╔', '╗', '╚', '╝'),
        BorderStyle::Rounded => ('─', '│', '╭', '╮', '╰', '╯'),
        BorderStyle::Single => ('─', '│', '┌', '┐', '└', '┘'),
    };
    for x in left.saturating_add(1)..right.saturating_sub(1) {
        frame
            .grid
            .set_cell(x, top, &horizontal.to_string(), Style::KeyHintBorder);
        frame
            .grid
            .set_cell(x, bottom, &horizontal.to_string(), Style::KeyHintBorder);
    }
    for y in top.saturating_add(1)..bottom {
        frame
            .grid
            .set_cell(left, y, &vertical.to_string(), Style::KeyHintBorder);
        frame.grid.set_cell(
            right.saturating_sub(1),
            y,
            &vertical.to_string(),
            Style::KeyHintBorder,
        );
    }
    frame
        .grid
        .set_cell(left, top, &top_left.to_string(), Style::KeyHintBorder);
    frame.grid.set_cell(
        right.saturating_sub(1),
        top,
        &top_right.to_string(),
        Style::KeyHintBorder,
    );
    frame
        .grid
        .set_cell(left, bottom, &bottom_left.to_string(), Style::KeyHintBorder);
    frame.grid.set_cell(
        right.saturating_sub(1),
        bottom,
        &bottom_right.to_string(),
        Style::KeyHintBorder,
    );
    let inner_left = left
        .saturating_add(1)
        .saturating_add(policy.horizontal_padding);
    let inner_right = right.saturating_sub(1 + policy.horizontal_padding);
    if inner_left >= inner_right {
        return;
    }
    frame.grid.put_text_clipped(
        inner_left,
        top.saturating_add(1 + policy.vertical_padding),
        &hints.prefix,
        Style::KeyHintPrefix,
        inner_right,
    );
    let body_top = top.saturating_add(2 + policy.vertical_padding);
    let gap_total = policy
        .column_gap
        .saturating_mul(u16::try_from(columns.saturating_sub(1)).unwrap_or(0));
    let column_width = inner_right
        .saturating_sub(inner_left)
        .saturating_sub(gap_total)
        / u16::try_from(columns).unwrap_or(1);
    if column_width < 3 {
        return;
    }
    for (index, row) in hints.rows.iter().take(row_count).enumerate() {
        let column = index / rows_per_column;
        let row_index = index % rows_per_column;
        let x = inner_left.saturating_add(
            u16::try_from(column)
                .unwrap_or(0)
                .saturating_mul(column_width.saturating_add(policy.column_gap)),
        );
        let y = body_top.saturating_add(
            u16::try_from(row_index.saturating_mul(row_stride)).unwrap_or(u16::MAX),
        );
        if y >= bottom {
            continue;
        }
        let column_right = x.saturating_add(column_width).min(inner_right);
        let mut cursor = frame.grid.put_text_clipped(
            x,
            y,
            &format!("[{}]", row.next_key),
            Style::KeyHintKey,
            column_right,
        );
        if let Some(icon) = &row.icon {
            cursor = frame.grid.put_text_clipped(
                cursor.saturating_add(1),
                y,
                icon,
                Style::KeyHintGroup,
                column_right,
            );
        }
        frame.grid.put_text_clipped(
            cursor.saturating_add(1),
            y,
            &row.label,
            if row.group {
                Style::KeyHintGroup
            } else {
                Style::KeyHintCommand
            },
            column_right,
        );
    }
}

/// Width of the persistent explorer dock, leaving a usable editor pane.
#[must_use]
pub fn explorer_panel_width(config: &Config, terminal_width: u16) -> Option<u16> {
    let separator = 1;
    let minimum_editor = config.ui.panes.minimum_width.max(20);
    if terminal_width < 3 {
        return None;
    }
    let maximum_panel = terminal_width.saturating_sub(minimum_editor.saturating_add(separator));
    if maximum_panel < 12 {
        return Some(terminal_width.saturating_sub(separator) / 2);
    }
    Some(
        config
            .ui
            .explorer
            .width
            .clamp(
                config.ui.explorer.minimum_width,
                config.ui.explorer.maximum_width,
            )
            .min(maximum_panel),
    )
}

#[allow(clippy::too_many_arguments)]
fn dock_explorer(
    content: Frame,
    explorer: &ExplorerUiState,
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    panel_width: u16,
    width: u16,
    height: u16,
) -> Frame {
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &config.editor.control_character_replacement,
        &config.editor.zero_width_replacement,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    let content_left = panel_width.saturating_add(1);
    for y in 0..content.grid.height() {
        for x in 0..content.grid.width() {
            if let Some(cell) = content.grid.get(x, y)
                && !cell.continuation
            {
                grid.set_cell(content_left.saturating_add(x), y, &cell.symbol, cell.style);
            }
        }
    }
    render_explorer_dock(&mut grid, explorer, ui, config, panel_width, height);
    if config.ui.status_line.enabled
        && height > 0
        && let Some(view) = state.active_view
    {
        render_status_line(&mut grid, state, view, ui, config, width, height - 1);
    }
    let cursor = if ui.explorer_focused {
        explorer
            .rows
            .iter()
            .position(|row| row.selected)
            .and_then(|row| u16::try_from(row).ok())
            .map(|row| {
                (
                    1.min(panel_width.saturating_sub(1)),
                    3_u16.saturating_add(row),
                    cursor_style(config.ui.cursor.normal),
                )
            })
            .filter(|(_, y, _)| {
                *y < height.saturating_sub(u16::from(config.ui.status_line.enabled))
            })
    } else {
        content
            .cursor
            .map(|(x, y, shape)| (content_left.saturating_add(x), y, shape))
    };
    Frame { grid, cursor }
}

fn render_explorer_dock(
    grid: &mut CellGrid,
    explorer: &ExplorerUiState,
    ui: &UiState,
    config: &Config,
    panel_width: u16,
    height: u16,
) {
    if panel_width == 0 || height == 0 {
        return;
    }
    let status_rows = u16::from(config.ui.status_line.enabled);
    let panel_height = height.saturating_sub(status_rows);
    if panel_height == 0 {
        return;
    }
    let glyphs = config.ui.explorer.active_glyphs();
    for y in 0..panel_height {
        for x in 0..panel_width {
            grid.set_cell(x, y, " ", Style::ExplorerBackground);
        }
        grid.set_cell(
            panel_width,
            y,
            &glyphs.panel_vertical,
            Style::ExplorerBorder,
        );
    }

    let title = format!(" {} ", explorer.title);
    let title_width = u16::try_from(text_width(&title, config.editor.ambiguous_width))
        .unwrap_or(panel_width)
        .min(panel_width);
    let title_left = panel_width.saturating_sub(title_width) / 2;
    for x in 0..panel_width {
        grid.set_cell(x, 0, &glyphs.panel_horizontal, Style::ExplorerBorder);
    }
    grid.put_text_clipped(title_left, 0, &title, Style::ExplorerHeader, panel_width);

    if panel_height > 1 {
        for x in 0..panel_width {
            grid.set_cell(x, 1, " ", Style::ExplorerHeader);
        }
        let count = format!("{0}/{0}", explorer.total);
        let count_width =
            u16::try_from(text_width(&count, config.editor.ambiguous_width)).unwrap_or(panel_width);
        grid.put_text_clipped(
            panel_width.saturating_sub(count_width).saturating_sub(1),
            1,
            &count,
            Style::ExplorerCount,
            panel_width,
        );
        if explorer.loading {
            grid.put_text_clipped(
                1,
                1,
                &config.ui.explorer.loading_label,
                Style::ExplorerCount,
                panel_width,
            );
        } else if ui.explorer_focused {
            grid.put_text_clipped(
                1,
                1,
                &config.ui.explorer.focus_marker,
                Style::ExplorerHeader,
                panel_width,
            );
        }
    }
    if panel_height > 2 {
        for x in 0..panel_width {
            grid.set_cell(x, 2, &glyphs.panel_horizontal, Style::ExplorerBorder);
        }
    }

    let body_top = 3;
    let available_rows = usize::from(panel_height.saturating_sub(body_top));
    let mut rendered_rows = 0usize;
    for (screen_row, row) in explorer.rows.iter().take(available_rows).enumerate() {
        rendered_rows = rendered_rows.saturating_add(1);
        let y = body_top.saturating_add(u16::try_from(screen_row).unwrap_or(0));
        if row.selected {
            for x in 0..panel_width {
                grid.set_cell(x, y, " ", Style::ExplorerSelection);
            }
        }
        let right_width = row.right_segments.iter().fold(0usize, |width, segment| {
            width.saturating_add(text_width(&segment.text, config.editor.ambiguous_width))
        });
        let content_right = panel_width.saturating_sub(1);
        let right_x =
            content_right.saturating_sub(u16::try_from(right_width).unwrap_or(content_right));
        let main_right = if row.right_segments.is_empty() {
            content_right
        } else {
            right_x
        };
        let mut x = 1;
        for segment in &row.segments {
            x = grid.put_text_clipped(
                x,
                y,
                &segment.text,
                if row.selected {
                    Style::ExplorerSelection
                } else {
                    explorer_segment_style(segment.role)
                },
                main_right,
            );
        }
        let mut right_x = right_x;
        for segment in &row.right_segments {
            right_x = grid.put_text_clipped(
                right_x,
                y,
                &segment.text,
                if row.selected {
                    Style::ExplorerSelection
                } else {
                    explorer_segment_style(segment.role)
                },
                content_right,
            );
        }
    }

    if !explorer.preview_lines.is_empty() && rendered_rows < available_rows {
        let divider_y = body_top.saturating_add(u16::try_from(rendered_rows).unwrap_or(0));
        for x in 0..panel_width {
            grid.set_cell(
                x,
                divider_y,
                &glyphs.panel_horizontal,
                Style::ExplorerBorder,
            );
        }
        for (index, line) in explorer
            .preview_lines
            .iter()
            .take(
                available_rows
                    .saturating_sub(rendered_rows)
                    .saturating_sub(1),
            )
            .enumerate()
        {
            grid.put_text_clipped(
                1,
                divider_y
                    .saturating_add(1)
                    .saturating_add(u16::try_from(index).unwrap_or(0)),
                line,
                Style::ExplorerDecoration,
                panel_width.saturating_sub(1),
            );
        }
    }
}

fn explorer_segment_style(role: ExplorerSegmentRole) -> Style {
    match role {
        ExplorerSegmentRole::Tree => Style::ExplorerTree,
        ExplorerSegmentRole::Icon => Style::ExplorerIcon,
        ExplorerSegmentRole::Directory => Style::ExplorerDirectory,
        ExplorerSegmentRole::File => Style::ExplorerFile,
        ExplorerSegmentRole::Symlink => Style::ExplorerSymlink,
        ExplorerSegmentRole::Loading => Style::ExplorerDecoration,
        ExplorerSegmentRole::Git => Style::ExplorerGit,
        ExplorerSegmentRole::Diagnostic => Style::ExplorerDiagnostic,
    }
}

fn render_documentation_popup(
    frame: &mut Frame,
    popup: &DocumentationPopupState,
    config: &Config,
    editor_left: u16,
    editor_width: u16,
    height: u16,
) {
    let policy = &config.ui.documentation_popup;
    let editor_right = editor_left.saturating_add(editor_width);
    let Some((cursor_x, cursor_y, _)) = frame.cursor else {
        return;
    };
    let available_height = height.saturating_sub(u16::from(config.ui.status_line.enabled));
    if !policy.enabled || popup.lines.is_empty() || editor_width < 4 || available_height < 3 {
        return;
    }
    let widest = popup
        .lines
        .iter()
        .map(|line| UnicodeWidthStr::width(line.as_str()))
        .chain(std::iter::once(UnicodeWidthStr::width(
            popup.title.as_str(),
        )))
        .max()
        .unwrap_or(1)
        .saturating_add(usize::from(policy.horizontal_padding).saturating_mul(2))
        .saturating_add(2);
    let box_width = u16::try_from(widest)
        .unwrap_or(policy.max_width)
        .min(policy.max_width)
        .min(editor_width);
    let visible_rows = popup
        .lines
        .len()
        .saturating_sub(popup.scroll)
        .min(usize::from(policy.max_height.saturating_sub(2)))
        .min(usize::from(available_height.saturating_sub(2)));
    let box_height = u16::try_from(visible_rows.saturating_add(2))
        .unwrap_or(policy.max_height)
        .min(policy.max_height)
        .min(available_height);
    if box_width < 4 || box_height < 3 {
        return;
    }
    let below = cursor_y.saturating_add(1);
    let room_below = available_height.saturating_sub(below);
    let top = if (policy.preferred_below && room_below >= box_height) || cursor_y < box_height {
        below.min(available_height.saturating_sub(box_height))
    } else {
        cursor_y.saturating_sub(box_height)
    };
    let left = if cursor_x.saturating_add(box_width) <= editor_right {
        cursor_x.max(editor_left)
    } else {
        cursor_x
            .saturating_add(1)
            .saturating_sub(box_width)
            .max(editor_left)
    };
    let right = left.saturating_add(box_width);
    let bottom = top.saturating_add(box_height).saturating_sub(1);
    for y in top..=bottom {
        for x in left..right {
            frame
                .grid
                .set_cell(x, y, " ", Style::DocumentationBackground);
        }
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) = match policy.border
    {
        BorderStyle::Double => ('═', '║', '╔', '╗', '╚', '╝'),
        BorderStyle::Rounded => ('─', '│', '╭', '╮', '╰', '╯'),
        BorderStyle::Single => ('─', '│', '┌', '┐', '└', '┘'),
    };
    for x in left.saturating_add(1)..right.saturating_sub(1) {
        frame
            .grid
            .set_cell(x, top, &horizontal.to_string(), Style::DocumentationBorder);
        frame.grid.set_cell(
            x,
            bottom,
            &horizontal.to_string(),
            Style::DocumentationBorder,
        );
    }
    for y in top.saturating_add(1)..bottom {
        frame
            .grid
            .set_cell(left, y, &vertical.to_string(), Style::DocumentationBorder);
        frame.grid.set_cell(
            right.saturating_sub(1),
            y,
            &vertical.to_string(),
            Style::DocumentationBorder,
        );
    }
    for (x, y, symbol) in [
        (left, top, top_left),
        (right.saturating_sub(1), top, top_right),
        (left, bottom, bottom_left),
        (right.saturating_sub(1), bottom, bottom_right),
    ] {
        frame
            .grid
            .set_cell(x, y, &symbol.to_string(), Style::DocumentationBorder);
    }
    frame.grid.put_text_clipped(
        left.saturating_add(1),
        top,
        &popup.title,
        Style::DocumentationTitle,
        right.saturating_sub(1),
    );
    let text_left = left
        .saturating_add(1)
        .saturating_add(policy.horizontal_padding);
    for (row, line) in popup
        .lines
        .iter()
        .skip(popup.scroll)
        .take(visible_rows)
        .enumerate()
    {
        frame.grid.put_text_clipped(
            text_left,
            top.saturating_add(1)
                .saturating_add(u16::try_from(row).unwrap_or(u16::MAX)),
            line,
            Style::DocumentationText,
            right
                .saturating_sub(1)
                .saturating_sub(policy.horizontal_padding),
        );
    }
}

fn render_language_completion(
    frame: &mut Frame,
    completion: &LanguageCompletionState,
    config: &Config,
    width: u16,
    height: u16,
) {
    if completion.items.is_empty() || width < 4 || height < 3 {
        return;
    }
    let (cursor_x, cursor_y, _) =
        frame
            .cursor
            .unwrap_or((1, 1, cursor_style(config.ui.cursor.normal)));
    let rows = completion
        .items
        .len()
        .min(usize::from(config.ui.command_line.completion_max_rows))
        .min(usize::from(height.saturating_sub(1)));
    let content_width = completion
        .items
        .iter()
        .take(rows)
        .map(|item| {
            text_width(&item.label, config.editor.ambiguous_width)
                .saturating_add(config.ui.command_line.completion_detail_gap.into())
                .saturating_add(text_width(&item.detail, config.editor.ambiguous_width))
        })
        .max()
        .unwrap_or(1)
        .min(usize::from(width.saturating_sub(2)));
    let panel_width = u16::try_from(content_width)
        .unwrap_or(width)
        .saturating_add(2)
        .min(width);
    let left = cursor_x.min(width.saturating_sub(panel_width));
    let below = cursor_y.saturating_add(1);
    let top = if below.saturating_add(u16::try_from(rows).unwrap_or(0)) < height {
        below
    } else {
        cursor_y.saturating_sub(u16::try_from(rows).unwrap_or(0))
    };
    for (row, item) in completion.items.iter().take(rows).enumerate() {
        let y = top.saturating_add(u16::try_from(row).unwrap_or(0));
        let style = if row == completion.selected {
            Style::Selection
        } else {
            Style::Status
        };
        for x in left..left.saturating_add(panel_width) {
            frame.grid.set_cell(x, y, " ", style);
        }
        frame
            .grid
            .put_text(left.saturating_add(1), y, &item.label, style);
        let detail_x = left
            .saturating_add(1)
            .saturating_add(
                u16::try_from(text_width(&item.label, config.editor.ambiguous_width)).unwrap_or(0),
            )
            .saturating_add(config.ui.command_line.completion_detail_gap);
        frame.grid.put_text(detail_x, y, &item.detail, Style::Muted);
    }
}

fn render_dashboard(
    root: &Path,
    configured_actions: &[DashboardAction],
    configured: &DashboardConfig,
    editor_config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &editor_config.editor.control_character_replacement,
        &editor_config.editor.zero_width_replacement,
        editor_config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    if width < configured.minimum_width || height < configured.minimum_height {
        grid.put_text(
            configured.compact_left.min(width.saturating_sub(1)),
            configured.compact_top.min(height.saturating_sub(1)),
            &configured.resize_message,
            Style::Accent,
        );
        return Frame { grid, cursor: None };
    }
    let content_width = width.min(configured.content_width);
    let left = width.saturating_sub(content_width) / 2;
    let top = height.saturating_sub(configured.block_height) / 2;
    if configured.show_identity {
        center_text(
            &mut grid,
            top.saturating_add(configured.identity_row),
            &configured.identity,
            Style::Accent,
            editor_config.editor.ambiguous_width,
        );
    }
    if configured.show_version {
        center_text(
            &mut grid,
            top.saturating_add(configured.version_row),
            &configured
                .version_format
                .replace("{version}", env!("CARGO_PKG_VERSION")),
            Style::Muted,
            editor_config.editor.ambiguous_width,
        );
    }
    if configured.show_path {
        center_text(
            &mut grid,
            top.saturating_add(configured.path_row),
            &root.display().to_string(),
            Style::Default,
            editor_config.editor.ambiguous_width,
        );
    }
    for (index, action) in configured_actions.iter().enumerate() {
        let y = top
            .saturating_add(configured.actions_row)
            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
        if y >= height {
            break;
        }
        let x = left.saturating_add(content_width / 2).saturating_sub(
            configured
                .action_key_width
                .saturating_add(configured.action_left_offset),
        );
        let key = action.key.as_deref().unwrap_or(&configured.unbound_label);
        let key = configured.key_format.replace("{key}", key);
        grid.put_text(x, y, &key, Style::Accent);
        grid.put_text(
            x.saturating_add(configured.action_key_width),
            y,
            &action.label,
            Style::Default,
        );
    }
    Frame { grid, cursor: None }
}

fn text_width(text: &str, ambiguous_width: AmbiguousWidth) -> usize {
    if ambiguous_width == AmbiguousWidth::Wide {
        UnicodeWidthStr::width_cjk(text)
    } else {
        UnicodeWidthStr::width(text)
    }
}

fn center_text(
    grid: &mut CellGrid,
    y: u16,
    text: &str,
    style: Style,
    ambiguous_width: AmbiguousWidth,
) {
    if y >= grid.height() {
        return;
    }
    let text_width = u16::try_from(text_width(text, ambiguous_width)).unwrap_or(u16::MAX);
    let x = grid.width().saturating_sub(text_width) / 2;
    grid.put_text(x, y, text, style);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneViewport {
    pub pane: crate::editing::PaneId,
    pub view: crate::editing::ViewId,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

#[derive(Clone, Copy)]
struct PaneRect {
    pane: crate::editing::PaneId,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

struct WorkspaceProjection {
    tab_line_visible: bool,
    status_rows: u16,
    panes: Vec<PaneRect>,
    borders: Vec<(u16, u16, u16, crate::editing::Axis)>,
}

fn workspace_projection(
    state: &EditorState,
    config: &Config,
    width: u16,
    height: u16,
) -> Option<WorkspaceProjection> {
    let tab = state.tabs.get(state.active_tab?)?;
    let tab_line_visible = config.ui.tab_line.enabled
        && (!config.ui.tab_line.hide_when_single || state.tabs.len() > 1);
    let top = u16::from(tab_line_visible);
    let status_rows = u16::from(config.ui.status_line.enabled && height > top);
    let layout_height = height.saturating_sub(top).saturating_sub(status_rows);
    let pane_count = layout_pane_count(&tab.root);
    let focused_layout;
    let layout = if width
        < config
            .ui
            .panes
            .minimum_width
            .saturating_mul(u16::try_from(pane_count).unwrap_or(u16::MAX))
        || layout_height < config.ui.panes.minimum_height
    {
        focused_layout = crate::editing::LayoutNode::Pane(tab.focused);
        &focused_layout
    } else {
        &tab.root
    };
    let mut panes = Vec::new();
    let mut borders = Vec::new();
    collect_pane_rects(
        layout,
        0,
        top,
        width,
        layout_height,
        &mut panes,
        &mut borders,
    );
    Some(WorkspaceProjection {
        tab_line_visible,
        status_rows,
        panes,
        borders,
    })
}

/// Exact renderer-owned pane rectangles for the active tab.
#[must_use]
pub fn pane_viewports(
    state: &EditorState,
    config: &Config,
    width: u16,
    height: u16,
) -> Vec<PaneViewport> {
    workspace_projection(state, config, width, height).map_or_else(Vec::new, |projection| {
        projection
            .panes
            .into_iter()
            .filter_map(|rect| {
                Some(PaneViewport {
                    pane: rect.pane,
                    view: state.panes.get(rect.pane)?.view,
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                })
            })
            .collect()
    })
}

#[must_use]
pub fn active_pane_viewport(
    state: &EditorState,
    config: &Config,
    width: u16,
    height: u16,
) -> Option<PaneViewport> {
    let tab = state.tabs.get(state.active_tab?)?;
    pane_viewports(state, config, width, height)
        .into_iter()
        .find(|viewport| viewport.pane == tab.focused)
}

fn render_workspace(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    let Some(tab_id) = state.active_tab else {
        let mut grid = CellGrid::new_with_policy(
            width,
            height,
            &config.editor.control_character_replacement,
            &config.editor.zero_width_replacement,
            config.editor.ambiguous_width == AmbiguousWidth::Wide,
        );
        grid.put_text(0, 0, &config.editor.end_of_buffer_marker, Style::Muted);
        return Frame { grid, cursor: None };
    };
    let Some(tab) = state.tabs.get(tab_id) else {
        return Frame {
            grid: CellGrid::new(width, height),
            cursor: None,
        };
    };
    let pane_count = layout_pane_count(&tab.root);
    let Some(projection) = workspace_projection(state, config, width, height) else {
        return Frame {
            grid: CellGrid::new(width, height),
            cursor: None,
        };
    };
    if pane_count == 1 && !projection.tab_line_visible {
        let pane = match &tab.root {
            crate::editing::LayoutNode::Pane(pane) => *pane,
            _ => unreachable!("pane count and layout disagree"),
        };
        let Some(view) = state.panes.get(pane).map(|pane| pane.view) else {
            return Frame {
                grid: CellGrid::new(width, height),
                cursor: None,
            };
        };
        return render_editor_view(state, view, ui, config, width, height);
    }
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &config.editor.control_character_replacement,
        &config.editor.zero_width_replacement,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    if projection.tab_line_visible {
        grid.fill_row(0, Style::Status);
        let mut x = 0;
        for (candidate, candidate_tab) in &state.tabs {
            let title = state
                .panes
                .get(candidate_tab.focused)
                .and_then(|pane| state.views.get(pane.view))
                .and_then(|view| state.buffers.get(view.buffer))
                .and_then(|buffer| buffer.path.as_ref())
                .and_then(|path| path.file_name())
                .map_or_else(
                    || config.ui.tab_line.scratch_label.clone(),
                    |name| name.to_string_lossy().into_owned(),
                );
            let format = if candidate == tab_id {
                &config.ui.tab_line.active_format
            } else {
                &config.ui.tab_line.inactive_format
            };
            x = grid.put_text(
                x,
                0,
                &format.replace("{title}", &title),
                if candidate == tab_id {
                    Style::StatusAccent
                } else {
                    Style::Status
                },
            );
            x = grid.put_text(x, 0, &config.ui.tab_line.separator, Style::Status);
        }
    }
    let mut cursor = None;
    let mut pane_config = config.clone();
    pane_config.ui.status_line.enabled = false;
    for rect in projection.panes {
        let Some(view) = state.panes.get(rect.pane).map(|pane| pane.view) else {
            continue;
        };
        let frame = render_editor_view(state, view, ui, &pane_config, rect.width, rect.height);
        for y in 0..rect.height {
            for x in 0..rect.width {
                let Some(cell) = frame.grid.get(x, y) else {
                    continue;
                };
                if !cell.continuation {
                    grid.set_cell(
                        rect.x.saturating_add(x),
                        rect.y.saturating_add(y),
                        &cell.symbol,
                        cell.style,
                    );
                }
            }
        }
        if rect.pane == tab.focused
            && let Some((x, y, shape)) = frame.cursor
        {
            cursor = Some((rect.x.saturating_add(x), rect.y.saturating_add(y), shape));
        }
    }
    let (horizontal, vertical) = match config.ui.panes.border {
        BorderStyle::Double => ('═', '║'),
        BorderStyle::Single | BorderStyle::Rounded => ('─', '│'),
    };
    for (x, y, length, axis) in projection.borders {
        for offset in 0..length {
            let (cell_x, cell_y, symbol) = match axis {
                crate::editing::Axis::Horizontal => (x, y.saturating_add(offset), vertical),
                crate::editing::Axis::Vertical => (x.saturating_add(offset), y, horizontal),
            };
            grid.set_cell(cell_x, cell_y, &symbol.to_string(), Style::Border);
        }
    }
    if projection.status_rows > 0
        && let Some(view) = state.active_view
    {
        render_status_line(&mut grid, state, view, ui, config, width, height - 1);
    }
    Frame { grid, cursor }
}

fn layout_pane_count(node: &crate::editing::LayoutNode) -> usize {
    match node {
        crate::editing::LayoutNode::Pane(_) => 1,
        crate::editing::LayoutNode::Split { first, second, .. } => {
            layout_pane_count(first).saturating_add(layout_pane_count(second))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_pane_rects(
    node: &crate::editing::LayoutNode,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    panes: &mut Vec<PaneRect>,
    borders: &mut Vec<(u16, u16, u16, crate::editing::Axis)>,
) {
    match node {
        crate::editing::LayoutNode::Pane(pane) => panes.push(PaneRect {
            pane: *pane,
            x,
            y,
            width,
            height,
        }),
        crate::editing::LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => match axis {
            crate::editing::Axis::Horizontal => {
                let available = width.saturating_sub(1);
                let first_width = available
                    .saturating_mul(*ratio)
                    .checked_div(1000)
                    .unwrap_or(0)
                    .min(available);
                let second_width = available.saturating_sub(first_width);
                collect_pane_rects(first, x, y, first_width, height, panes, borders);
                let border_x = x.saturating_add(first_width);
                borders.push((border_x, y, height, *axis));
                collect_pane_rects(
                    second,
                    border_x.saturating_add(1),
                    y,
                    second_width,
                    height,
                    panes,
                    borders,
                );
            }
            crate::editing::Axis::Vertical => {
                let available = height.saturating_sub(1);
                let first_height = available
                    .saturating_mul(*ratio)
                    .checked_div(1000)
                    .unwrap_or(0)
                    .min(available);
                let second_height = available.saturating_sub(first_height);
                collect_pane_rects(first, x, y, width, first_height, panes, borders);
                let border_y = y.saturating_add(first_height);
                borders.push((x, border_y, width, *axis));
                collect_pane_rects(
                    second,
                    x,
                    border_y.saturating_add(1),
                    width,
                    second_height,
                    panes,
                    borders,
                );
            }
        },
    }
}

fn syntax_role_style(role: crate::syntax::SyntaxRole) -> Style {
    match role {
        crate::syntax::SyntaxRole::Comment => Style::SyntaxComment,
        crate::syntax::SyntaxRole::String => Style::SyntaxString,
        crate::syntax::SyntaxRole::Number => Style::SyntaxNumber,
        crate::syntax::SyntaxRole::Keyword => Style::SyntaxKeyword,
        crate::syntax::SyntaxRole::Type => Style::SyntaxType,
        crate::syntax::SyntaxRole::Variable => Style::SyntaxVariable,
    }
}

fn configured_editor_gutter_width(
    text: &crate::document::TextBuffer,
    config: &EditorConfig,
) -> usize {
    if config.line_numbers == LineNumbers::Off {
        return 0;
    }
    text.len_lines()
        .max(1)
        .to_string()
        .len()
        .saturating_add(text_width(
            &config.line_number_separator,
            config.ambiguous_width,
        ))
        .saturating_add(usize::from(config.gutter_padding))
}

/// Effective display-cell gutter for an exact pane width. Every non-empty
/// pane reserves at least one cell for text/cursor projection.
#[must_use]
pub fn editor_gutter_width(
    text: &crate::document::TextBuffer,
    config: &EditorConfig,
    pane_width: u16,
) -> u16 {
    let maximum = pane_width.saturating_sub(1);
    u16::try_from(configured_editor_gutter_width(text, config))
        .unwrap_or(u16::MAX)
        .min(maximum)
}

/// Display cells available to buffer content in an exact pane viewport.
/// A zero-width pane has no synthetic content cell.
#[must_use]
pub fn editor_content_width(
    text: &crate::document::TextBuffer,
    config: &EditorConfig,
    pane_width: u16,
) -> usize {
    usize::from(pane_width.saturating_sub(editor_gutter_width(text, config, pane_width)))
}

fn render_editor_view(
    state: &EditorState,
    view_id: crate::editing::ViewId,
    ui: &UiState,
    config: &Config,
    width: u16,
    height: u16,
) -> Frame {
    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &config.editor.control_character_replacement,
        &config.editor.zero_width_replacement,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    let Some(view) = state.views.get(view_id) else {
        return Frame { grid, cursor: None };
    };
    let buffer = &state.buffers[view.buffer];
    let body_height = height.saturating_sub(u16::from(config.ui.status_line.enabled));
    let current_line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
    let digits = buffer.text.len_lines().max(1).to_string().len();
    let gutter_width = editor_gutter_width(&buffer.text, &config.editor, width);
    let content_width = editor_content_width(&buffer.text, &config.editor, width);
    let large_file = ui.large_file_buffers.contains(&view.buffer);
    let wrap_enabled = content_width > 0
        && config.editor.wrap
        && !(large_file && config.editor.large_file.disable_wrap);
    let first_wrap_skip = if wrap_enabled && current_line == view.scroll_line && body_height > 0 {
        let line_start = buffer
            .text
            .line_to_byte(current_line)
            .unwrap_or(ByteOffset(0));
        let prefix = buffer
            .text
            .slice(ByteRange::new(line_start, view.cursor).expect("cursor range"))
            .unwrap_or_default();
        let cursor_wrap_row = display_column(
            &prefix,
            config.editor.tab_width,
            config.editor.ambiguous_width,
        ) / content_width;
        cursor_wrap_row.saturating_sub(usize::from(body_height.saturating_sub(1)))
    } else {
        0
    };
    let search_ranges = if config.search.highlight && !large_file {
        ui.search_query
            .as_deref()
            .filter(|query| !query.is_empty())
            .map_or_else(Vec::new, |query| {
                let source = buffer.text.to_string();
                crate::input::search_matches(
                    &source,
                    query,
                    config.search.smart_case,
                    config.search.regex,
                )
                .unwrap_or_default()
                .into_iter()
                .filter_map(|range| {
                    ByteRange::new(ByteOffset(range.start), ByteOffset(range.end)).ok()
                })
                .collect()
            })
    } else {
        Vec::new()
    };

    let mut syntax_spans = ui
        .syntax_spans
        .iter()
        .filter(|span| span.buffer == view.buffer && span.revision == buffer.text.revision())
        .collect::<Vec<_>>();
    syntax_spans.sort_unstable_by_key(|span| (span.range.start, span.range.end));
    let mut syntax_cursor = 0_usize;
    let mut semantic_spans = ui
        .lsp_semantic_spans
        .iter()
        .filter(|span| span.buffer == view.buffer && span.revision == buffer.text.revision())
        .collect::<Vec<_>>();
    semantic_spans.sort_unstable_by_key(|span| (span.range.start, span.range.end));
    let mut semantic_cursor = 0usize;
    let mut highlight_spans = ui
        .lsp_highlight_spans
        .iter()
        .filter(|span| span.buffer == view.buffer && span.revision == buffer.text.revision())
        .collect::<Vec<_>>();
    highlight_spans.sort_unstable_by_key(|span| (span.range.start, span.range.end));
    let mut highlight_cursor = 0usize;
    let inlay_hints = ui
        .lsp_inlay_hints
        .iter()
        .filter(|hint| hint.buffer == view.buffer && hint.revision == buffer.text.revision())
        .collect::<Vec<_>>();
    let mut screen_row = 0_u16;
    let mut line_index = view.scroll_line;
    while screen_row < body_height && line_index < buffer.text.len_lines() {
        if let Some(fold) = ui
            .lsp_folds
            .iter()
            .chain(ui.syntax_folds.iter())
            .find(|fold| {
                fold.view == view_id
                    && fold.collapsed
                    && line_index > fold.start_line
                    && line_index <= fold.end_line
            })
        {
            line_index = fold.end_line.saturating_add(1);
            continue;
        }
        if gutter_width > 0 {
            let number = match config.editor.line_numbers {
                LineNumbers::Absolute => line_index + 1,
                LineNumbers::Relative => current_line.abs_diff(line_index),
                LineNumbers::Hybrid => {
                    if line_index == current_line {
                        line_index + 1
                    } else {
                        current_line.abs_diff(line_index)
                    }
                }
                LineNumbers::Off => 0,
            };
            let number = if config.editor.line_numbers == LineNumbers::Relative
                && line_index == current_line
            {
                line_index + 1
            } else {
                number
            };
            let label = format!("{number:>digits$}{}", config.editor.line_number_separator);
            grid.put_text_clipped(
                0,
                screen_row,
                &label,
                if line_index == current_line {
                    Style::Accent
                } else {
                    Style::Muted
                },
                gutter_width,
            );
        }
        let line_start = buffer
            .text
            .line_to_byte(line_index)
            .unwrap_or(ByteOffset(0));
        let line = buffer.text.line(line_index).unwrap_or_default();
        let full_content = line.strip_suffix('\n').unwrap_or(&line);
        let can_wrap = wrap_enabled;
        let skipped_bytes = if line_index == view.scroll_line && first_wrap_skip > 0 {
            byte_offset_at_display_column(
                full_content,
                first_wrap_skip.saturating_mul(content_width),
                config.editor.tab_width,
                config.editor.ambiguous_width,
            )
        } else if !can_wrap {
            byte_offset_at_display_column(
                full_content,
                view.scroll_column,
                config.editor.tab_width,
                config.editor.ambiguous_width,
            )
        } else {
            0
        };
        let skipped_column = display_column(
            &full_content[..skipped_bytes],
            config.editor.tab_width,
            config.editor.ambiguous_width,
        );
        let visible_content = &full_content[skipped_bytes..];
        let mut x = gutter_width;
        let mut source_column = skipped_column;
        for (relative, grapheme) in visible_content.grapheme_indices(true) {
            let global = ByteOffset(line_start.get() + skipped_bytes + relative);
            let style = if is_selected(
                view.selection,
                global,
                &buffer.text,
                DisplayWidthPolicy {
                    tab_width: config.editor.tab_width,
                    ambiguous_wide: config.editor.ambiguous_width == AmbiguousWidth::Wide,
                },
            ) {
                Style::Selection
            } else if search_ranges.iter().any(|range| range.contains(global)) {
                Style::Search
            } else if !(large_file && config.editor.large_file.disable_diagnostics)
                && buffer.path.as_ref().is_some_and(|path| {
                    ui.diagnostics.iter().any(|diagnostic| {
                        &diagnostic.path == path
                            && (diagnostic.range.contains(global)
                                || (diagnostic.range.is_empty()
                                    && diagnostic.range.start == global))
                    })
                })
            {
                Style::Error
            } else if let Some(span) = {
                while highlight_spans
                    .get(highlight_cursor)
                    .is_some_and(|span| span.range.end <= global)
                {
                    highlight_cursor = highlight_cursor.saturating_add(1);
                }
                highlight_spans
                    .get(highlight_cursor)
                    .copied()
                    .filter(|span| span.range.contains(global))
            } {
                let _ = span;
                Style::Accent
            } else if let Some(span) = {
                while semantic_spans
                    .get(semantic_cursor)
                    .is_some_and(|span| span.range.end <= global)
                {
                    semantic_cursor = semantic_cursor.saturating_add(1);
                }
                semantic_spans
                    .get(semantic_cursor)
                    .copied()
                    .filter(|span| span.range.contains(global))
            } {
                syntax_role_style(span.role)
            } else if let Some(span) = {
                while syntax_spans
                    .get(syntax_cursor)
                    .is_some_and(|span| span.range.end <= global)
                {
                    syntax_cursor = syntax_cursor.saturating_add(1);
                }
                syntax_spans
                    .get(syntax_cursor)
                    .copied()
                    .filter(|span| span.range.contains(global))
            } {
                syntax_role_style(span.role)
            } else {
                Style::Default
            };
            let cells = if grapheme == "\t" {
                config.editor.tab_width - (source_column % config.editor.tab_width)
            } else {
                text_width(grapheme, config.editor.ambiguous_width).max(1)
            };
            let advance = u16::try_from(cells).unwrap_or(u16::MAX);
            if x.saturating_add(advance) > width {
                if !can_wrap {
                    break;
                }
                screen_row = screen_row.saturating_add(1);
                if screen_row >= body_height {
                    break;
                }
                x = gutter_width;
            }
            source_column = source_column.saturating_add(cells);
            if grapheme == "\t" {
                for _ in 0..cells {
                    if x >= width {
                        break;
                    }
                    grid.set_cell(x, screen_row, " ", style);
                    x = x.saturating_add(1);
                }
            } else if x < width {
                grid.set_cell(x, screen_row, grapheme, style);
                x = x.saturating_add(advance);
            }
        }
        let line_end = ByteOffset(
            line_start
                .get()
                .saturating_add(skipped_bytes)
                .saturating_add(visible_content.len()),
        );
        let visible_start = ByteOffset(line_start.get().saturating_add(skipped_bytes));
        for hint in inlay_hints
            .iter()
            .filter(|hint| hint.position >= visible_start && hint.position <= line_end)
            .take(32)
        {
            let source_relative = hint
                .position
                .get()
                .saturating_sub(line_start.get())
                .min(full_content.len());
            if full_content.is_char_boundary(source_relative) {
                let source_hint_column = display_column(
                    &full_content[..source_relative],
                    config.editor.tab_width,
                    config.editor.ambiguous_width,
                );
                let visible_hint_column = source_hint_column.saturating_sub(skipped_column);
                let hint_x = gutter_width
                    .saturating_add(u16::try_from(visible_hint_column).unwrap_or(u16::MAX));
                if hint_x < width {
                    grid.put_text(hint_x, screen_row, &hint.label, Style::Muted);
                }
            }
        }
        screen_row = screen_row.saturating_add(1);
        line_index = line_index.saturating_add(1);
    }
    while screen_row < body_height {
        grid.put_text(
            0,
            screen_row,
            &config.editor.end_of_buffer_marker,
            Style::Muted,
        );
        screen_row = screen_row.saturating_add(1);
    }

    if config.ui.status_line.enabled && height > 0 {
        render_status_line(&mut grid, state, view_id, ui, config, width, height - 1);
    }

    let cursor = editor_cursor(
        state,
        view_id,
        config,
        width,
        body_height,
        gutter_width,
        content_width,
        wrap_enabled,
    )
    .map(|(x, y)| {
        let configured = match view.mode {
            EditorMode::Insert => config.ui.cursor.insert,
            EditorMode::Replace => config.ui.cursor.replace,
            EditorMode::VisualChar | EditorMode::VisualLine | EditorMode::VisualBlock => {
                config.ui.cursor.visual
            }
            EditorMode::OperatorPending => config.ui.cursor.operator_pending,
            EditorMode::CommandLine => config.ui.cursor.command,
            EditorMode::Prompt => config.ui.cursor.prompt,
            EditorMode::Normal => config.ui.cursor.normal,
        };
        (x, y, cursor_style(configured))
    });
    Frame { grid, cursor }
}

fn render_status_line(
    grid: &mut CellGrid,
    state: &EditorState,
    view_id: crate::editing::ViewId,
    ui: &UiState,
    config: &Config,
    width: u16,
    status_y: u16,
) {
    let Some(view) = state.views.get(view_id) else {
        return;
    };
    let Some(buffer) = state.buffers.get(view.buffer) else {
        return;
    };
    grid.fill_row(status_y, Style::Status);
    let mode_label = config
        .ui
        .status_line
        .mode_labels
        .get(mode_name(view.mode))
        .expect("resolved configuration validates mode labels");
    let mode = format!(
        "{}{}{}",
        config.ui.status_line.mode_padding, mode_label, config.ui.status_line.mode_padding
    );
    let name = buffer
        .path
        .as_ref()
        .and_then(|path| path.file_name())
        .map_or_else(
            || config.ui.status_line.scratch_label.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
    let dirty = if buffer.dirty {
        config.ui.status_line.dirty_marker.as_str()
    } else {
        ""
    };
    let current_line = buffer.text.byte_to_line(view.cursor).unwrap_or(0);
    let position = config
        .ui
        .status_line
        .position_format
        .replace("{line}", &(current_line + 1).to_string())
        .replace(
            "{column}",
            &(byte_column(&buffer.text, view.cursor) + 1).to_string(),
        );
    let lsp_status = ui
        .lsp_server
        .as_ref()
        .map(|server| config.ui.status_line.lsp_format.replace("{server}", server))
        .unwrap_or_default();
    let mut x = 0;
    for component in &config.ui.status_line.components {
        let (text, style, right_aligned) = match component.as_str() {
            "mode" => (mode.as_str(), Style::StatusAccent, false),
            "file" => (
                if buffer.dirty { "" } else { name.as_str() },
                Style::Status,
                false,
            ),
            "large_file" => (
                if ui.large_file_buffers.contains(&view.buffer) {
                    config.editor.large_file.status_label.as_str()
                } else {
                    ""
                },
                Style::StatusAccent,
                false,
            ),
            "lsp" => (lsp_status.as_str(), Style::StatusAccent, false),
            "position" => (position.as_str(), Style::Status, true),
            _ => continue,
        };
        let owned_file;
        let text = if component == "file" && buffer.dirty {
            owned_file = format!("{name}{dirty}");
            owned_file.as_str()
        } else {
            text
        };
        if right_aligned {
            let component_width =
                u16::try_from(text_width(text, config.editor.ambiguous_width)).unwrap_or(0);
            if width > component_width {
                grid.put_text(width - component_width, status_y, text, style);
            }
        } else {
            x = grid.put_text(x, status_y, text, style);
            x = grid.put_text(
                x,
                status_y,
                &config.ui.status_line.component_separator,
                Style::Status,
            );
        }
    }
}

fn editor_cursor(
    state: &EditorState,
    view_id: crate::editing::ViewId,
    config: &Config,
    width: u16,
    body_height: u16,
    gutter: u16,
    available: usize,
    wrap_enabled: bool,
) -> Option<(u16, u16)> {
    let view = state.views.get(view_id)?;
    let buffer = state.buffers.get(view.buffer)?;
    let point = PositionCodec::new(&buffer.text)
        .byte_to_point(view.cursor)
        .ok()?;
    if point.line < view.scroll_line {
        return None;
    }
    let line_start = buffer.text.line_to_byte(point.line).ok()?;
    let prefix = buffer
        .text
        .slice(ByteRange::new(line_start, view.cursor).ok()?)
        .ok()?;
    let column = display_column(
        &prefix,
        config.editor.tab_width,
        config.editor.ambiguous_width,
    );
    if wrap_enabled {
        let mut row = 0_usize;
        for line_index in view.scroll_line..point.line {
            let line = buffer.text.line(line_index).ok()?;
            let content = line.strip_suffix('\n').unwrap_or(&line);
            let columns = display_column(
                content,
                config.editor.tab_width,
                config.editor.ambiguous_width,
            );
            row = row.saturating_add(columns.max(1).div_ceil(available));
        }
        row = row.saturating_add(column / available);
        if point.line == view.scroll_line {
            let skip =
                (column / available).saturating_sub(usize::from(body_height.saturating_sub(1)));
            row = row.saturating_sub(skip);
        }
        let y = u16::try_from(row).ok()?;
        let x = gutter.saturating_add(u16::try_from(column % available).ok()?);
        (x < width && y < body_height).then_some((x, y))
    } else {
        let line = buffer.text.line(point.line).ok()?;
        let content = line.strip_suffix('\n').unwrap_or(&line);
        let snapped_scroll = snap_display_column(
            content,
            view.scroll_column,
            config.editor.tab_width,
            config.editor.ambiguous_width,
        );
        let visible_column = column.checked_sub(snapped_scroll)?;
        let x = gutter.saturating_add(u16::try_from(visible_column).ok()?);
        let y = u16::try_from(point.line - view.scroll_line).ok()?;
        (x < width && y < body_height).then_some((x, y))
    }
}

#[must_use]
pub fn snap_display_column(
    text: &str,
    target: usize,
    tab_width: usize,
    ambiguous_width: AmbiguousWidth,
) -> usize {
    let byte = byte_offset_at_display_column(text, target, tab_width, ambiguous_width);
    display_column(&text[..byte], tab_width, ambiguous_width)
}

/// Snap forward to the first complete display boundary at or after `target`.
#[must_use]
pub fn snap_display_column_forward(
    text: &str,
    target: usize,
    tab_width: usize,
    ambiguous_width: AmbiguousWidth,
) -> usize {
    let mut column = 0_usize;
    for grapheme in text.graphemes(true) {
        if column >= target {
            return column;
        }
        column = column.saturating_add(if grapheme == "\t" {
            tab_width - (column % tab_width)
        } else {
            text_width(grapheme, ambiguous_width).max(1)
        });
    }
    column
}

fn byte_offset_at_display_column(
    text: &str,
    target: usize,
    tab_width: usize,
    ambiguous_width: AmbiguousWidth,
) -> usize {
    let mut column = 0_usize;
    let mut byte = 0_usize;
    for (relative, grapheme) in text.grapheme_indices(true) {
        let width = if grapheme == "\t" {
            tab_width - (column % tab_width)
        } else {
            text_width(grapheme, ambiguous_width).max(1)
        };
        if column.saturating_add(width) > target {
            return relative;
        }
        column = column.saturating_add(width);
        byte = relative.saturating_add(grapheme.len());
    }
    byte
}

fn display_column(text: &str, tab_width: usize, ambiguous_width: AmbiguousWidth) -> usize {
    let mut column = 0;
    for grapheme in text.graphemes(true) {
        if grapheme == "\t" {
            column += tab_width - (column % tab_width);
        } else {
            column += text_width(grapheme, ambiguous_width).max(1);
        }
    }
    column
}

fn byte_column(text: &crate::document::TextBuffer, offset: ByteOffset) -> usize {
    PositionCodec::new(text)
        .byte_to_point(offset)
        .map_or(0, |point| point.byte_column)
}

fn is_selected(
    selection: Option<crate::editing::Selection>,
    offset: ByteOffset,
    text: &crate::document::TextBuffer,
    width_policy: DisplayWidthPolicy,
) -> bool {
    let Some(selection) = selection else {
        return false;
    };
    match selection.kind {
        SelectionKind::Character => {
            let start = selection.anchor.min(selection.head);
            let end = selection.anchor.max(selection.head);
            offset >= start && offset <= end
        }
        SelectionKind::Line => {
            let line = text.byte_to_line(offset).unwrap_or(usize::MAX);
            let anchor = text.byte_to_line(selection.anchor).unwrap_or(0);
            let head = text.byte_to_line(selection.head).unwrap_or(0);
            (anchor.min(head)..=anchor.max(head)).contains(&line)
        }
        SelectionKind::Block => {
            let point_line = text.byte_to_line(offset).unwrap_or(usize::MAX);
            let anchor_line = text.byte_to_line(selection.anchor).unwrap_or(0);
            let head_line = text.byte_to_line(selection.head).unwrap_or(0);
            if !(anchor_line.min(head_line)..=anchor_line.max(head_line)).contains(&point_line) {
                return false;
            }
            let Ok(anchor_start) = text.line_to_byte(anchor_line) else {
                return false;
            };
            let Ok(head_start) = text.line_to_byte(head_line) else {
                return false;
            };
            let Ok(anchor_text) = text.line(anchor_line) else {
                return false;
            };
            let Ok(head_text) = text.line(head_line) else {
                return false;
            };
            let anchor_content = anchor_text.strip_suffix('\n').unwrap_or(&anchor_text);
            let head_content = head_text.strip_suffix('\n').unwrap_or(&head_text);
            let anchor_column = display_column_at(
                anchor_content,
                selection.anchor.get().saturating_sub(anchor_start.get()),
                width_policy,
            );
            let head_column = display_column_at(
                head_content,
                selection.head.get().saturating_sub(head_start.get()),
                width_policy,
            );
            let Ok(point_start) = text.line_to_byte(point_line) else {
                return false;
            };
            let Ok(point_text) = text.line(point_line) else {
                return false;
            };
            let point_content = point_text.strip_suffix('\n').unwrap_or(&point_text);
            byte_range_for_display_columns(point_content, anchor_column, head_column, width_policy)
                .is_some_and(|(start, end)| {
                    let relative = offset.get().saturating_sub(point_start.get());
                    (start..end).contains(&relative)
                })
        }
    }
}

fn mode_name(mode: EditorMode) -> &'static str {
    match mode {
        EditorMode::Normal => "normal",
        EditorMode::Insert => "insert",
        EditorMode::Replace => "replace",
        EditorMode::VisualChar => "visual_char",
        EditorMode::VisualLine => "visual_line",
        EditorMode::VisualBlock => "visual_block",
        EditorMode::OperatorPending => "operator_pending",
        EditorMode::CommandLine => "command_line",
        EditorMode::Prompt => "prompt",
    }
}

fn cursor_style(shape: CursorShape) -> SetCursorStyle {
    match shape {
        CursorShape::Default => SetCursorStyle::DefaultUserShape,
        CursorShape::Block => SetCursorStyle::SteadyBlock,
        CursorShape::Bar => SetCursorStyle::SteadyBar,
        CursorShape::Underline => SetCursorStyle::SteadyUnderScore,
        CursorShape::BlinkingBlock => SetCursorStyle::BlinkingBlock,
        CursorShape::BlinkingBar => SetCursorStyle::BlinkingBar,
        CursorShape::BlinkingUnderline => SetCursorStyle::BlinkingUnderScore,
    }
}

fn render_picker(
    frame: &mut Frame,
    picker: &PickerUiState,
    config: &Config,
    terminal_width: u16,
    terminal_height: u16,
) {
    let configured = &config.ui.picker;
    let width =
        u16::try_from(u32::from(terminal_width) * u32::from(configured.width_percent) / 100)
            .unwrap_or(terminal_width)
            .max(configured.minimum_width.min(terminal_width))
            .min(terminal_width);
    let height =
        u16::try_from(u32::from(terminal_height) * u32::from(configured.height_percent) / 100)
            .unwrap_or(terminal_height)
            .max(configured.minimum_height.min(terminal_height))
            .min(terminal_height);
    if width < 4 || height < 6 {
        return;
    }
    let left = terminal_width.saturating_sub(width) / 2;
    let top = terminal_height.saturating_sub(height) / 2;
    for y in top..top.saturating_add(height) {
        for x in left..left.saturating_add(width) {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
        match configured.border {
            BorderStyle::Double => ('═', '║', '╔', '╗', '╚', '╝'),
            BorderStyle::Rounded => ('─', '│', '╭', '╮', '╰', '╯'),
            BorderStyle::Single => ('─', '│', '┌', '┐', '└', '┘'),
        };
    for x in left..left + width {
        frame
            .grid
            .set_cell(x, top, &horizontal.to_string(), Style::Border);
        frame
            .grid
            .set_cell(x, top + height - 1, &horizontal.to_string(), Style::Border);
    }
    for y in top..top + height {
        frame
            .grid
            .set_cell(left, y, &vertical.to_string(), Style::Border);
        frame
            .grid
            .set_cell(left + width - 1, y, &vertical.to_string(), Style::Border);
    }
    for (x, y, character) in [
        (left, top, top_left),
        (left + width - 1, top, top_right),
        (left, top + height - 1, bottom_left),
        (left + width - 1, top + height - 1, bottom_right),
    ] {
        frame
            .grid
            .set_cell(x, y, &character.to_string(), Style::Border);
    }
    frame
        .grid
        .put_text(left + 2, top, &format!(" {} ", picker.title), Style::Accent);
    let content_left = left + 2;
    let query_y = top + 1;
    frame
        .grid
        .put_text(content_left, query_y, &picker.query, Style::Default);
    if picker.loading {
        let loading_x = left + width
            - 2
            - u16::try_from(text_width(
                &configured.loading_label,
                config.editor.ambiguous_width,
            ))
            .unwrap_or(0);
        frame.grid.put_text(
            loading_x.max(content_left),
            query_y,
            &configured.loading_label,
            Style::Muted,
        );
    }
    let first_row = top + 3;
    let visible_rows = usize::from(height.saturating_sub(4));
    for (display_row, (index, row)) in picker
        .rows
        .iter()
        .enumerate()
        .skip(picker.scroll)
        .take(visible_rows)
        .enumerate()
    {
        let y = first_row + u16::try_from(display_row).unwrap_or(0);
        let selected = picker.selected == Some(index);
        let marker = if selected {
            &configured.selected_marker
        } else {
            &configured.row_marker
        };
        let source = if row.source.is_empty() {
            String::new()
        } else {
            format!("{}{}", configured.source_separator, row.source)
        };
        let text = format!("{} {} {}{}", marker, row.label, row.detail, source);
        frame.grid.put_text(
            content_left,
            y,
            &text
                .chars()
                .take(usize::from(width.saturating_sub(4)))
                .collect::<String>(),
            if selected {
                Style::Selection
            } else {
                Style::Default
            },
        );
    }
    let cursor_x = content_left.saturating_add(
        u16::try_from(text_width(&picker.query, config.editor.ambiguous_width)).unwrap_or(0),
    );
    frame.cursor = Some((
        cursor_x.min(left + width - 2),
        query_y,
        cursor_style(config.ui.cursor.prompt),
    ));
}

fn render_tool_menu(
    frame: &mut Frame,
    menu: &ToolMenuState,
    config: &Config,
    terminal_width: u16,
    terminal_height: u16,
) {
    let configured = &config.ui.tool_manager;
    let width = (u32::from(terminal_width) * u32::from(configured.width_percent) / 100)
        .try_into()
        .unwrap_or(terminal_width);
    let height = (u32::from(terminal_height) * u32::from(configured.height_percent) / 100)
        .try_into()
        .unwrap_or(terminal_height);
    let width = width
        .max(configured.minimum_width.min(terminal_width))
        .min(terminal_width);
    let height = height
        .max(configured.minimum_height.min(terminal_height))
        .min(terminal_height);
    if width < 4 || height < 6 {
        return;
    }
    let left = terminal_width.saturating_sub(width) / 2;
    let top = terminal_height.saturating_sub(height) / 2;
    for y in top..top.saturating_add(height) {
        for x in left..left.saturating_add(width) {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
        match configured.border {
            BorderStyle::Double => ('═', '║', '╔', '╗', '╚', '╝'),
            BorderStyle::Rounded => ('─', '│', '╭', '╮', '╰', '╯'),
            BorderStyle::Single => ('─', '│', '┌', '┐', '└', '┘'),
        };
    for x in left..left + width {
        frame
            .grid
            .set_cell(x, top, &horizontal.to_string(), Style::Border);
        frame
            .grid
            .set_cell(x, top + height - 1, &horizontal.to_string(), Style::Border);
    }
    for y in top..top + height {
        frame
            .grid
            .set_cell(left, y, &vertical.to_string(), Style::Border);
        frame
            .grid
            .set_cell(left + width - 1, y, &vertical.to_string(), Style::Border);
    }
    for (x, y, character) in [
        (left, top, top_left),
        (left + width - 1, top, top_right),
        (left, top + height - 1, bottom_left),
        (left + width - 1, top + height - 1, bottom_right),
    ] {
        frame
            .grid
            .set_cell(x, y, &character.to_string(), Style::Border);
    }
    let title = format!(" {} ", configured.title);
    frame.grid.put_text(left + 2, top, &title, Style::Accent);
    let content_left = left + 2;
    let content_width = width.saturating_sub(4);
    let mut y = top + 1;
    frame
        .grid
        .put_text(content_left, y, &configured.help, Style::Muted);
    y += 2;
    let mut category_x = content_left;
    for category in &configured.category_order {
        let label = configured
            .categories
            .get(category)
            .map(String::as_str)
            .unwrap_or(category);
        let text = format!(" {} ", label);
        let style = if category == &menu.category {
            Style::StatusAccent
        } else {
            Style::Status
        };
        category_x = frame
            .grid
            .put_text(category_x, y, &text, style)
            .saturating_add(1);
        if category_x >= left + width - 2 {
            break;
        }
    }
    y += 2;
    let visible_rows = usize::from(height.saturating_sub(y - top).saturating_sub(2));
    let rows = menu
        .rows
        .iter()
        .enumerate()
        .skip(menu.scroll)
        .take(visible_rows);
    let mut installed_heading_drawn = false;
    let mut available_heading_drawn = false;
    for (index, row) in rows {
        if row.installed && !installed_heading_drawn {
            frame.grid.put_text(
                content_left,
                y,
                &configured.installed_heading,
                Style::Accent,
            );
            installed_heading_drawn = true;
            y += 1;
        } else if !row.installed && !available_heading_drawn {
            frame.grid.put_text(
                content_left,
                y,
                &configured.available_heading,
                Style::Accent,
            );
            available_heading_drawn = true;
            y += 1;
        }
        if y >= top + height - 1 {
            break;
        }
        let selected = index == menu.selected;
        let marker = if selected {
            &configured.selected_marker
        } else {
            &configured.row_marker
        };
        let row_text = format!(
            "{} {:<28} {:<10} {}",
            marker, row.name, row.kind, row.languages
        );
        frame.grid.put_text(
            content_left,
            y,
            &row_text
                .chars()
                .take(usize::from(content_width))
                .collect::<String>(),
            if selected {
                Style::Selection
            } else if row.installed {
                Style::Default
            } else {
                Style::Muted
            },
        );
        let status_width = text_width(&row.status, config.editor.ambiguous_width);
        let status_x = left + width - 2 - u16::try_from(status_width).unwrap_or(0);
        frame.grid.put_text(
            status_x.max(content_left),
            y,
            &row.status,
            if row.installed {
                Style::Accent
            } else {
                Style::Muted
            },
        );
        y += 1;
    }
    frame.cursor = None;
}

fn render_prompt(
    frame: &mut Frame,
    prompt: &PromptState,
    config: &Config,
    width: u16,
    height: u16,
) {
    let geometry = prompt_geometry(
        width,
        height,
        &config.ui.command_line,
        prompt.error.is_some(),
    );
    if geometry.width == 0 || geometry.height == 0 {
        return;
    }
    if geometry.height < 3 {
        let marker_end = frame.grid.put_text(
            geometry.left,
            geometry.top,
            prompt.kind.marker(&config.ui.command_line.text),
            Style::Accent,
        );
        frame
            .grid
            .put_text(marker_end, geometry.top, &prompt.input, Style::Default);
        let prefix = prompt.input.get(..prompt.cursor).unwrap_or(&prompt.input);
        frame.cursor = Some((
            marker_end
                .saturating_add(
                    u16::try_from(text_width(prefix, config.editor.ambiguous_width)).unwrap_or(0),
                )
                .min(width.saturating_sub(1)),
            geometry.top,
            cursor_style(config.ui.cursor.prompt),
        ));
        return;
    }
    let (horizontal, vertical, top_left, top_right, bottom_left, bottom_right) =
        match config.ui.command_line.border {
            BorderStyle::Double => ('═', '║', '╔', '╗', '╚', '╝'),
            BorderStyle::Rounded => ('─', '│', '╭', '╮', '╰', '╯'),
            BorderStyle::Single => ('─', '│', '┌', '┐', '└', '┘'),
        };
    for x in geometry.left..geometry.left.saturating_add(geometry.width) {
        frame
            .grid
            .set_cell(x, geometry.top, &horizontal.to_string(), Style::Border);
        if geometry.height >= 3 {
            frame
                .grid
                .set_cell(x, geometry.top + 2, &horizontal.to_string(), Style::Border);
        }
    }
    frame.grid.set_cell(
        geometry.left,
        geometry.top,
        &top_left.to_string(),
        Style::Border,
    );
    frame.grid.set_cell(
        geometry.left + geometry.width - 1,
        geometry.top,
        &top_right.to_string(),
        Style::Border,
    );
    if geometry.height >= 3 {
        frame.grid.set_cell(
            geometry.left,
            geometry.top + 2,
            &bottom_left.to_string(),
            Style::Border,
        );
        frame.grid.set_cell(
            geometry.left + geometry.width - 1,
            geometry.top + 2,
            &bottom_right.to_string(),
            Style::Border,
        );
    }
    frame.grid.set_cell(
        geometry.left,
        geometry.top + 1,
        &vertical.to_string(),
        Style::Border,
    );
    frame.grid.set_cell(
        geometry.left + geometry.width - 1,
        geometry.top + 1,
        &vertical.to_string(),
        Style::Border,
    );
    if config.ui.command_line.show_title && geometry.width > config.ui.command_line.title_min_width
    {
        let title = format!(" {} ", prompt.kind.title(&config.ui.command_line.text));
        let title_width =
            u16::try_from(text_width(&title, config.editor.ambiguous_width)).unwrap_or(0);
        let title_x = geometry.left + geometry.width.saturating_sub(title_width) / 2;
        frame
            .grid
            .put_text(title_x, geometry.top, &title, Style::Accent);
    }
    let input_x = geometry
        .left
        .saturating_add(config.ui.command_line.input_padding);
    let input_y = geometry.top.saturating_add(1);
    let marker_end = frame.grid.put_text(
        input_x,
        input_y,
        prompt.kind.marker(&config.ui.command_line.text),
        Style::Accent,
    );
    frame
        .grid
        .put_text(marker_end, input_y, &prompt.input, Style::Default);
    if let Some(error) = &prompt.error {
        let error_y = geometry
            .top
            .saturating_add(3)
            .saturating_add(config.ui.command_line.attachment_gap);
        frame.grid.put_text(
            geometry
                .left
                .saturating_add(config.ui.command_line.input_padding),
            error_y,
            error,
            Style::Error,
        );
    } else if !prompt.completions.is_empty() {
        let available = height.saturating_sub(geometry.top.saturating_add(3));
        let rows = u16::try_from(prompt.completions.len())
            .unwrap_or(u16::MAX)
            .min(config.ui.command_line.completion_max_rows)
            .min(available);
        for row in 0..rows {
            let index = usize::from(row);
            let y = geometry
                .top
                .saturating_add(3)
                .saturating_add(config.ui.command_line.attachment_gap)
                .saturating_add(row);
            let style = if prompt.selected_completion == Some(index) {
                Style::Selection
            } else {
                Style::Status
            };
            for x in geometry.left..geometry.left.saturating_add(geometry.width) {
                frame.grid.set_cell(x, y, " ", style);
            }
            let item = &prompt.completions[index];
            let label_x = geometry
                .left
                .saturating_add(config.ui.command_line.input_padding);
            let detail_x = label_x
                .saturating_add(
                    u16::try_from(text_width(&item.label, config.editor.ambiguous_width))
                        .unwrap_or(0),
                )
                .saturating_add(config.ui.command_line.completion_detail_gap);
            frame.grid.put_text(label_x, y, &item.label, style);
            frame.grid.put_text(detail_x, y, &item.detail, Style::Muted);
        }
    }
    let prefix = prompt.input.get(..prompt.cursor).unwrap_or(&prompt.input);
    let cursor_x = marker_end.saturating_add(
        u16::try_from(text_width(prefix, config.editor.ambiguous_width)).unwrap_or(0),
    );
    frame.cursor = Some((
        cursor_x.min(geometry.left + geometry.width.saturating_sub(2)),
        input_y,
        cursor_style(config.ui.cursor.prompt),
    ));
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::editing::EditorState;

    use super::{
        CompletionItem, DocumentationPopupKind, DocumentationPopupState, PromptKind, PromptState,
        UiState, prompt_geometry, render,
    };

    #[test]
    fn prompt_is_globally_centered_and_clamped() {
        let configured = crate::configuration::CommandLineConfig::default();
        let geometry = prompt_geometry(120, 40, &configured, false);
        assert_eq!(geometry.left, 30);
        assert_eq!(geometry.top, 2);
        assert_eq!(geometry.width, 60);
        let narrow = prompt_geometry(30, 8, &configured, false);
        assert_eq!(narrow.left, 1);
        assert_eq!(narrow.width, 28);
    }

    #[test]
    fn documentation_popup_preserves_lines_clamps_and_protects_status_row() {
        let mut state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
        state.open_text(
            crate::document::TextBuffer::from_text("cursor\nline\nline\nline\nline\nline"),
            None,
        );
        let ui = UiState {
            documentation_popup: Some(DocumentationPopupState {
                kind: DocumentationPopupKind::Hover,
                title: "Hover".into(),
                lines: vec!["first line".into(), "wide 界界 clipped safely".into()],
                scroll: 0,
            }),
            ..UiState::default()
        };
        let config = crate::configuration::load_bundled_config().config;
        let dashboard = crate::configuration::load_bundled_config().dashboard;
        let frame = render(&state, &ui, &config, &dashboard, 24, 8);
        let screen = (0..8)
            .map(|y| {
                (0..24)
                    .map(|x| frame.grid.get(x, y).map_or("", |cell| cell.symbol.as_str()))
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(screen.iter().any(|line| line.contains("Hover")));
        assert!(screen.iter().any(|line| line.contains("first line")));
        assert!(screen.iter().any(|line| line.contains("wide 界")));
        assert!(screen.last().is_some_and(|line| line.contains("NORMAL")));
    }

    #[test]
    fn completion_attaches_below_globally_centered_prompt() {
        let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
        let mut prompt = PromptState::new(PromptKind::Command);
        prompt.input = "e".into();
        prompt.cursor = 1;
        prompt.completions = vec![
            CompletionItem {
                label: "e".into(),
                detail: "Open file".into(),
            },
            CompletionItem {
                label: "edit.file".into(),
                detail: "Example".into(),
            },
        ];
        prompt.selected_completion = Some(0);
        let ui = UiState {
            dashboard: true,
            prompt: Some(prompt),
            ..UiState::default()
        };
        let loaded = crate::configuration::load_bundled_config();
        let config = loaded.config;
        let dashboard = loaded.dashboard;
        let frame = render(&state, &ui, &config, &dashboard, 100, 20);
        let geometry = prompt_geometry(100, 20, &config.ui.command_line, false);
        assert_eq!(geometry.left, 20);
        assert!(frame.grid.row_text(geometry.top + 3).contains("Open file"));
    }

    #[test]
    fn block_selection_projection_uses_shared_display_cell_ranges() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        let (_, view) = state.open_text(crate::document::TextBuffer::from_text("\tZ\n界ab"), None);
        state.views[view].mode = crate::editing::EditorMode::VisualBlock;
        state.views[view].selection = Some(crate::editing::Selection {
            anchor: crate::document::ByteOffset(0),
            head: crate::document::ByteOffset("\tZ\n界ab".len()),
            kind: crate::editing::SelectionKind::Block,
        });
        state.views[view].cursor = crate::document::ByteOffset("\tZ\n界ab".len());
        let loaded = crate::configuration::load_bundled_config();
        let frame = render(
            &state,
            &UiState::default(),
            &loaded.config,
            &loaded.dashboard,
            20,
            6,
        );
        let gutter = 3;
        assert_eq!(
            frame.grid.get(gutter + 3, 0).expect("tab cell").style,
            crate::terminal_ui::Style::Selection
        );
        assert_eq!(
            frame.grid.get(gutter, 1).expect("wide cell").style,
            crate::terminal_ui::Style::Selection
        );
    }

    #[test]
    fn split_tree_and_tab_line_render_real_independent_views() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state.open_text(crate::document::TextBuffer::from_text("left\nright"), None);
        state
            .split_active(crate::editing::Axis::Horizontal, 500)
            .expect("split");
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.ui.tab_line.hide_when_single = false;
        let frame = render(
            &state,
            &UiState::default(),
            &config,
            &loaded.dashboard,
            20,
            8,
        );
        assert!(frame.grid.row_text(0).contains("[scratch]"));
        assert_eq!(frame.grid.get(9, 2).expect("split border").symbol, "│");
        assert!(frame.cursor.is_some_and(|(x, y, _)| x > 9 && y > 0));
    }

    #[test]
    fn configured_wrapping_uses_visual_rows() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        state.open_text(crate::document::TextBuffer::from_text("abcdefghij"), None);
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.editor.wrap = true;
        config.editor.line_numbers = crate::configuration::LineNumbers::Off;
        config.ui.status_line.enabled = false;
        let frame = render(
            &state,
            &UiState::default(),
            &config,
            &loaded.dashboard,
            5,
            3,
        );
        assert!(frame.grid.row_text(0).starts_with("abcde"));
        assert!(frame.grid.row_text(1).starts_with("fghij"));

        let view = state.active_view.expect("view");
        let buffer = state.views[view].buffer;
        state.buffers[buffer].text =
            crate::document::TextBuffer::from_text("abcdefghijklmnopqrstuvwxyz");
        state.views[view].cursor = crate::document::ByteOffset(25);
        let frame = render(
            &state,
            &UiState::default(),
            &config,
            &loaded.dashboard,
            5,
            3,
        );
        assert!(frame.cursor.is_some_and(|(_, y, _)| y < 3));
        assert!(frame.grid.row_text(2).contains('z'));
    }

    #[test]
    fn non_wrapped_horizontal_projection_snaps_graphemes_and_preserves_source_tabs() {
        let text = "a\t界e\u{301}bcdef";
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (buffer, view) = state.open_text(crate::document::TextBuffer::from_text(text), None);
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.editor.wrap = false;
        config.editor.line_numbers = crate::configuration::LineNumbers::Off;
        config.ui.status_line.enabled = false;

        // Column two is inside the source tab. The viewport snaps to the tab's
        // column-one boundary, so only its three remaining source cells render.
        state.views[view].scroll_column = 2;
        state.views[view].cursor = crate::document::ByteOffset("a\t界e\u{301}".len());
        let ui = UiState {
            syntax_spans: vec![super::UiSyntaxSpan {
                buffer,
                revision: state.buffers[buffer].text.revision(),
                range: crate::document::ByteRange::new(
                    crate::document::ByteOffset("a\t界".len()),
                    crate::document::ByteOffset("a\t界e\u{301}".len()),
                )
                .expect("combining syntax range"),
                role: crate::syntax::SyntaxRole::String,
            }],
            lsp_inlay_hints: vec![super::UiInlayHint {
                buffer,
                revision: state.buffers[buffer].text.revision(),
                position: crate::document::ByteOffset("a\t界e\u{301}b".len()),
                label: "H".into(),
            }],
            ..UiState::default()
        };
        let frame = render(&state, &ui, &config, &loaded.dashboard, 8, 2);
        assert_eq!(frame.grid.get(0, 0).expect("tab remainder").symbol, " ");
        assert_eq!(frame.grid.get(2, 0).expect("last tab cell").symbol, " ");
        assert_eq!(frame.grid.get(3, 0).expect("wide grapheme").symbol, "界");
        assert!(
            frame
                .grid
                .get(4, 0)
                .expect("wide continuation")
                .continuation
        );
        assert_eq!(
            frame.grid.get(5, 0).expect("combining grapheme").symbol,
            "e\u{301}"
        );
        assert_eq!(
            frame.grid.get(5, 0).expect("syntax after clipping").style,
            crate::terminal_ui::Style::SyntaxString
        );
        assert_eq!(
            frame.grid.get(8 - 1, 0).expect("projected inlay").symbol,
            "H"
        );
        assert!(frame.cursor.is_some_and(|(x, y, _)| x == 6 && y == 0));

        // Column five is inside the wide glyph and snaps to its column-four start.
        state.views[view].scroll_column = 5;
        let frame = render(
            &state,
            &UiState::default(),
            &config,
            &loaded.dashboard,
            8,
            2,
        );
        assert_eq!(
            frame.grid.get(0, 0).expect("snapped wide glyph").symbol,
            "界"
        );
        assert!(
            frame
                .grid
                .get(1, 0)
                .expect("wide continuation")
                .continuation
        );
        assert!(frame.cursor.is_some_and(|(x, _, _)| x == 3));

        config.editor.ambiguous_width = crate::configuration::AmbiguousWidth::Narrow;
        assert_eq!(
            super::snap_display_column("a·b", 2, 4, config.editor.ambiguous_width),
            2
        );
        config.editor.ambiguous_width = crate::configuration::AmbiguousWidth::Wide;
        assert_eq!(
            super::snap_display_column("a·b", 2, 4, config.editor.ambiguous_width),
            1
        );
    }

    #[test]
    fn concrete_gutter_reserves_content_at_zero_one_and_two_cell_widths() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (buffer, view) = state.open_text(crate::document::TextBuffer::from_text("a\tX"), None);
        state.views[view].cursor = crate::document::ByteOffset("a\t".len());
        state.views[view].scroll_column = 4;
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.editor.wrap = false;
        config.editor.line_numbers = crate::configuration::LineNumbers::Absolute;
        config.editor.line_number_separator = "1234567890123456".into();
        config.editor.gutter_padding = 20;
        config.ui.status_line.enabled = false;

        assert_eq!(
            super::editor_gutter_width(&state.buffers[buffer].text, &config.editor, 0),
            0
        );
        assert_eq!(
            super::editor_content_width(&state.buffers[buffer].text, &config.editor, 0),
            0
        );
        let zero = render(
            &state,
            &UiState::default(),
            &config,
            &loaded.dashboard,
            0,
            2,
        );
        assert_eq!(zero.grid.width(), 0);
        assert!(zero.cursor.is_none());

        let mut split_state = EditorState::new(PathBuf::from("/tmp"));
        split_state.open_text(crate::document::TextBuffer::from_text("x"), None);
        split_state
            .split_active(crate::editing::Axis::Horizontal, 100)
            .expect("minimum-ratio split");
        split_state.focus_pane(-1, false).expect("zero-width leaf");
        config.ui.panes.minimum_width = 3;
        config.ui.panes.minimum_height = 2;
        let zero_leaf = super::active_pane_viewport(&split_state, &config, 6, 2)
            .expect("reachable zero-width pane");
        assert_eq!(zero_leaf.width, 0);
        let split_frame = render(
            &split_state,
            &UiState::default(),
            &config,
            &loaded.dashboard,
            6,
            2,
        );
        assert!(split_frame.cursor.is_none());

        for (width, expected_gutter) in [(1, 0), (2, 1)] {
            assert_eq!(
                super::editor_gutter_width(&state.buffers[buffer].text, &config.editor, width,),
                expected_gutter
            );
            assert_eq!(
                super::editor_content_width(&state.buffers[buffer].text, &config.editor, width,),
                1
            );
            let frame = render(
                &state,
                &UiState::default(),
                &config,
                &loaded.dashboard,
                width,
                2,
            );
            assert_eq!(
                frame
                    .grid
                    .get(expected_gutter, 0)
                    .expect("reserved content cell")
                    .symbol,
                "X"
            );
            assert!(
                frame
                    .cursor
                    .is_some_and(|(x, y, _)| { x == expected_gutter && x < width && y == 0 })
            );
        }

        config.editor.line_numbers = crate::configuration::LineNumbers::Off;
        assert_eq!(
            super::editor_gutter_width(&state.buffers[buffer].text, &config.editor, 2),
            0
        );
        assert_eq!(
            super::editor_content_width(&state.buffers[buffer].text, &config.editor, 2),
            2
        );
    }

    #[test]
    fn wrapping_ignores_horizontal_offset() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (_, view) = state.open_text(crate::document::TextBuffer::from_text("abcdefghij"), None);
        state.views[view].scroll_column = 7;
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.editor.wrap = true;
        config.editor.line_numbers = crate::configuration::LineNumbers::Off;
        config.ui.status_line.enabled = false;
        let frame = render(
            &state,
            &UiState::default(),
            &config,
            &loaded.dashboard,
            5,
            3,
        );
        assert!(frame.grid.row_text(0).starts_with("abcde"));
        assert!(frame.grid.row_text(1).starts_with("fghij"));
        assert!(frame.cursor.is_some_and(|(x, y, _)| x == 0 && y == 0));
    }

    #[test]
    fn generic_picker_is_centered_filtered_presentation_with_configured_markers() {
        let state = EditorState::new(PathBuf::from("/tmp"));
        let ui = UiState {
            dashboard: true,
            picker: Some(super::PickerUiState {
                title: "Commands".into(),
                query: "health".into(),
                rows: vec![super::PickerRow {
                    id: "health".into(),
                    label: "health.open".into(),
                    detail: "Show health".into(),
                    source: "core".into(),
                }],
                selected: Some(0),
                scroll: 0,
                loading: false,
            }),
            ..UiState::default()
        };
        let loaded = crate::configuration::load_bundled_config();
        let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 30);
        assert!((0..30).any(|row| frame.grid.row_text(row).contains("health.open")));
        assert!(frame.cursor.is_some());
    }

    #[test]
    fn active_pane_viewport_uses_exact_focused_split_rectangle() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state.open_text(crate::document::TextBuffer::from_text("one\ntwo"), None);
        state
            .split_active(crate::editing::Axis::Horizontal, 400)
            .expect("horizontal split");
        state
            .split_active(crate::editing::Axis::Vertical, 600)
            .expect("vertical split");
        let loaded = crate::configuration::load_bundled_config();
        let viewport =
            super::active_pane_viewport(&state, &loaded.config, 79, 24).expect("focused viewport");
        assert_eq!(Some(viewport.view), state.active_view);
        assert!(viewport.x > 0 && viewport.y > 0);
        assert!(viewport.width < 79 && viewport.height < 23);
        assert!(viewport.y.saturating_add(viewport.height) <= 23);
    }

    #[test]
    fn explorer_is_a_persistent_left_dock_that_reflows_the_editor() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state.open_text(
            crate::document::TextBuffer::from_text("editor content"),
            Some(PathBuf::from("/workspace/main.rs")),
        );
        let ui = UiState {
            explorer: Some(super::ExplorerUiState {
                title: "Explorer".into(),
                rows: vec![
                    super::ExplorerUiRow {
                        node_id: 1,
                        selected: false,
                        segments: vec![
                            super::ExplorerUiSegment {
                                text: "├─".into(),
                                role: super::ExplorerSegmentRole::Tree,
                            },
                            super::ExplorerUiSegment {
                                text: "▾".into(),
                                role: super::ExplorerSegmentRole::Icon,
                            },
                            super::ExplorerUiSegment {
                                text: " ".into(),
                                role: super::ExplorerSegmentRole::Tree,
                            },
                            super::ExplorerUiSegment {
                                text: "src".into(),
                                role: super::ExplorerSegmentRole::Directory,
                            },
                        ],
                        right_segments: vec![
                            super::ExplorerUiSegment {
                                text: "M".into(),
                                role: super::ExplorerSegmentRole::Git,
                            },
                            super::ExplorerUiSegment {
                                text: "E".into(),
                                role: super::ExplorerSegmentRole::Diagnostic,
                            },
                        ],
                    },
                    super::ExplorerUiRow {
                        node_id: 2,
                        selected: true,
                        segments: vec![
                            super::ExplorerUiSegment {
                                text: "│ └─".into(),
                                role: super::ExplorerSegmentRole::Tree,
                            },
                            super::ExplorerUiSegment {
                                text: "·".into(),
                                role: super::ExplorerSegmentRole::Icon,
                            },
                            super::ExplorerUiSegment {
                                text: " main.rs-with-an-intentionally-overlong-name-that-must-not-spill".into(),
                                role: super::ExplorerSegmentRole::File,
                            },
                        ],
                        right_segments: vec![super::ExplorerUiSegment {
                            text: "?".into(),
                            role: super::ExplorerSegmentRole::Diagnostic,
                        }],
                    },
                ],
                scroll: 0,
                loading: false,
                total: 2,
                preview_lines: Vec::new(),
            }),
            explorer_focused: true,
            ..UiState::default()
        };
        let loaded = crate::configuration::load_bundled_config();
        let panel = super::explorer_panel_width(&loaded.config, 100).expect("dock width");
        assert_eq!(panel, 40);
        assert_eq!(super::explorer_panel_width(&loaded.config, 30), Some(14));
        assert_eq!(super::explorer_panel_width(&loaded.config, 2), None);
        let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 20);
        assert!(frame.grid.row_text(0).contains("Explorer"));
        assert!(frame.grid.row_text(3).contains("src"));
        assert!(frame.grid.row_text(4).contains("main.rs"));
        assert_eq!(
            frame.grid.get(1, 3).expect("tree segment").style,
            crate::terminal_ui::Style::ExplorerTree
        );
        assert_eq!(
            frame.grid.get(3, 3).expect("icon segment").style,
            crate::terminal_ui::Style::ExplorerIcon
        );
        assert_eq!(
            frame.grid.get(5, 3).expect("directory name").style,
            crate::terminal_ui::Style::ExplorerDirectory
        );
        assert_eq!(
            frame.grid.get(37, 3).expect("git decoration").style,
            crate::terminal_ui::Style::ExplorerGit
        );
        assert_eq!(
            frame.grid.get(38, 3).expect("diagnostic decoration").style,
            crate::terminal_ui::Style::ExplorerDiagnostic
        );
        assert_eq!(
            frame.grid.get(1, 4).expect("selected row").style,
            crate::terminal_ui::Style::ExplorerSelection
        );
        assert_eq!(
            frame.grid.get(panel, 3).expect("dock separator").symbol,
            "⎸"
        );
        assert_eq!(
            frame
                .grid
                .get(panel, 4)
                .expect("preserved separator")
                .symbol,
            "⎸"
        );
        assert_eq!(
            frame
                .grid
                .get(panel + 1, 4)
                .expect("editor-side cell")
                .style,
            crate::terminal_ui::Style::Muted
        );
        assert!(frame.cursor.is_some_and(|(x, y, _)| x == 1 && y == 4));
        assert!(frame.grid.row_text(19).contains("NORMAL"));
        let mut editor_ui = ui.clone();
        editor_ui.explorer_focused = false;
        let editor_frame = render(
            &state,
            &editor_ui,
            &loaded.config,
            &loaded.dashboard,
            100,
            20,
        );
        assert!(editor_frame.cursor.is_some_and(|(x, _, _)| x > panel));

        let mut ascii = loaded.config.clone();
        ascii.ui.explorer.icon_mode = crate::configuration::ExplorerIconMode::Ascii;
        let ascii_frame = render(&state, &ui, &ascii, &loaded.dashboard, 100, 20);
        assert_eq!(
            ascii_frame
                .grid
                .get(panel, 3)
                .expect("ASCII separator")
                .symbol,
            "|"
        );
        let mut unicode = loaded.config.clone();
        unicode.ui.explorer.icon_mode = crate::configuration::ExplorerIconMode::Unicode;
        let unicode_frame = render(&state, &ui, &unicode, &loaded.dashboard, 100, 20);
        assert_eq!(
            unicode_frame
                .grid
                .get(panel, 3)
                .expect("Unicode separator")
                .symbol,
            "⎸"
        );
    }

    #[test]
    fn language_diagnostics_and_completion_are_projected_without_service_calls() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let path = PathBuf::from("/tmp/main.rs");
        state.open_text(
            crate::document::TextBuffer::from_text("bad"),
            Some(path.clone()),
        );
        let ui = UiState {
            diagnostics: vec![super::UiDiagnostic {
                path,
                range: crate::document::ByteRange::new(
                    crate::document::ByteOffset(0),
                    crate::document::ByteOffset(3),
                )
                .expect("range"),
                message: "problem".into(),
            }],
            language_completion: Some(super::LanguageCompletionState {
                items: vec![CompletionItem {
                    label: "better".into(),
                    detail: "fake LSP".into(),
                }],
                selected: 0,
            }),
            ..UiState::default()
        };
        let loaded = crate::configuration::load_bundled_config();
        let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 40, 10);
        assert_eq!(
            frame.grid.get(3, 0).expect("diagnostic text").style,
            crate::terminal_ui::Style::Error
        );
        assert!((0..10).any(|row| frame.grid.row_text(row).contains("better")));
    }

    #[test]
    fn terminal_is_a_full_width_bottom_dock_with_direct_cells_and_focus_cursor() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state.open_text(
            crate::document::TextBuffer::from_text("editor content\nsecond line"),
            Some(PathBuf::from("/workspace/main.rs")),
        );
        state
            .split_active(crate::editing::Axis::Horizontal, 400)
            .expect("nested editor split");
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.ui.terminal_panel.height = 6;
        config.ui.terminal_panel.minimum_height = 4;
        let mut screen = crate::terminal_ui::PtyScreen::new(80, 4, 100);
        screen.feed("\u{1b}[31;1mRED\u{1b}[0m 界");
        let (cursor_column, cursor_row) = screen.cursor();
        let terminal = super::TerminalPanelState {
            id: 7,
            profile: "shell".into(),
            running: true,
            exit_code: None,
            truncated: false,
            unsupported_sequences: screen.unsupported_sequences(),
            focused: true,
            columns: 80,
            rows: 4,
            cells: screen.cells().to_vec(),
            cursor: Some((
                u16::try_from(cursor_column).expect("cursor column"),
                u16::try_from(cursor_row).expect("cursor row"),
            )),
            scrollback_lines: screen.scrollback_len(),
            scroll_offset: 0,
            requested_height: 6,
        };
        let ui = UiState {
            explorer: Some(super::ExplorerUiState {
                title: "Explorer".into(),
                rows: Vec::new(),
                scroll: 0,
                loading: false,
                total: 0,
                preview_lines: Vec::new(),
            }),
            terminal_panel: Some(terminal.clone()),
            ..UiState::default()
        };
        let geometry = super::terminal_panel_geometry(&config, 20, 6).expect("terminal geometry");
        assert_eq!(geometry.top, 14);
        assert_eq!(geometry.body_height, 4);
        let panel_width = super::explorer_panel_width(&config, 80).expect("explorer width");
        let editor_width = 80_u16.saturating_sub(panel_width).saturating_sub(1);
        let viewport = super::active_pane_viewport(&state, &config, editor_width, geometry.top)
            .expect("reduced editor viewport");
        assert!(viewport.height < 14);

        let frame = render(&state, &ui, &config, &loaded.dashboard, 80, 20);
        assert_eq!(
            frame.grid.get(0, geometry.top).expect("full divider").style,
            crate::terminal_ui::Style::TerminalBorder
        );
        assert_eq!(
            frame
                .grid
                .get(79, geometry.top)
                .expect("right divider")
                .style,
            crate::terminal_ui::Style::TerminalBorder
        );
        assert_eq!(
            frame
                .grid
                .get(0, geometry.body_top)
                .expect("red terminal cell")
                .style,
            crate::terminal_ui::Style::Terminal(crate::terminal_ui::PtyStyle {
                foreground: Some(1),
                background: None,
                bold: true,
            })
        );
        assert_eq!(
            frame
                .grid
                .get(4, geometry.body_top)
                .expect("wide terminal cell")
                .symbol,
            "界"
        );
        assert!(
            frame
                .grid
                .get(5, geometry.body_top)
                .expect("wide continuation")
                .continuation
        );
        assert_ne!(
            frame
                .grid
                .get(panel_width, geometry.body_top)
                .expect("below explorer")
                .style,
            crate::terminal_ui::Style::ExplorerBorder
        );
        assert!(frame.cursor.is_some_and(|(x, y, _)| {
            x == u16::try_from(cursor_column).unwrap() && y == geometry.body_top
        }));
        assert!(frame.grid.row_text(geometry.status_y).contains("RUNNING"));

        let mut editor_ui = ui;
        editor_ui.terminal_panel.as_mut().expect("terminal").focused = false;
        let editor_frame = render(&state, &editor_ui, &config, &loaded.dashboard, 80, 20);
        assert!(
            editor_frame
                .cursor
                .is_some_and(|(_, y, _)| y < geometry.top)
        );
        assert!(
            editor_frame
                .grid
                .row_text(geometry.top)
                .contains("TERMINAL #7")
        );
    }

    #[test]
    fn key_hints_anchor_clip_and_preserve_protected_rows() {
        let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.ui.key_hints.max_rows = 2;
        config.ui.key_hints.max_columns = 2;
        config.ui.key_hints.max_width = 44;
        let mut ui = UiState {
            dashboard: true,
            key_hints: Some(super::KeyHintUiState {
                prefix: "space f".into(),
                rows: (0..8)
                    .map(|index| super::KeyHintUiRow {
                        next_key: char::from(b'a' + index).to_string(),
                        label: format!("Action {index}"),
                        icon: (index == 0).then(|| "F".into()),
                        group: index == 0,
                    })
                    .collect(),
            }),
            ..UiState::default()
        };
        let right = render(&state, &ui, &config, &loaded.dashboard, 80, 20);
        let right_prefix_x = (0..80)
            .find(|x| {
                (0..19).any(|y| {
                    right
                        .grid
                        .get(*x, y)
                        .is_some_and(|cell| cell.style == crate::terminal_ui::Style::KeyHintPrefix)
                })
            })
            .expect("right anchored prefix");
        assert!(right_prefix_x > 30);
        assert!(!(0..80).any(|x| {
            right.grid.get(x, 19).is_some_and(|cell| {
                matches!(
                    cell.style,
                    crate::terminal_ui::Style::KeyHintBackground
                        | crate::terminal_ui::Style::KeyHintBorder
                        | crate::terminal_ui::Style::KeyHintKey
                        | crate::terminal_ui::Style::KeyHintGroup
                        | crate::terminal_ui::Style::KeyHintCommand
                        | crate::terminal_ui::Style::KeyHintPrefix
                )
            })
        }));
        assert!(!(0..20).any(|row| right.grid.row_text(row).contains("Action 4")));

        config.ui.key_hints.anchor = crate::configuration::KeyHintAnchor::BottomLeft;
        ui.explorer = Some(super::ExplorerUiState {
            title: "Files".into(),
            rows: Vec::new(),
            scroll: 0,
            loading: false,
            total: 0,
            preview_lines: Vec::new(),
        });
        let left = render(&state, &ui, &config, &loaded.dashboard, 80, 20);
        let panel = super::explorer_panel_width(&config, 80).expect("panel");
        let leftmost_hint = (0..80)
            .find(|x| {
                (0..19).any(|y| {
                    left.grid
                        .get(*x, y)
                        .is_some_and(|cell| cell.style == crate::terminal_ui::Style::KeyHintBorder)
                })
            })
            .expect("left anchored hint");
        assert_eq!(leftmost_hint, panel + 1);
        assert!(leftmost_hint < right_prefix_x);

        let tiny = render(&state, &ui, &config, &loaded.dashboard, 10, 3);
        assert!(!(0..3).any(|row| tiny.grid.row_text(row).contains("space f")));
    }

    #[test]
    fn dashboard_and_prompt_render_at_multiple_sizes() {
        let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
        for (width, height) in [(120, 40), (42, 10), (18, 5), (18, 1)] {
            let mut ui = UiState {
                dashboard: true,
                ..UiState::default()
            };
            let loaded = crate::configuration::load_bundled_config();
            let config = loaded.config;
            let dashboard_config = loaded.dashboard;
            let dashboard = render(&state, &ui, &config, &dashboard_config, width, height);
            assert_eq!(dashboard.grid.width(), width);
            assert!(
                dashboard.grid.row_text(height.saturating_sub(1)).len()
                    >= usize::from(width.saturating_sub(2))
                    || width < 3
            );
            ui.prompt = Some(PromptState::new(PromptKind::Command));
            let prompt = render(&state, &ui, &config, &dashboard_config, width, height);
            let geometry = prompt_geometry(width, height, &config.ui.command_line, false);
            if height > 2 && width > 2 {
                assert!(prompt.grid.row_text(geometry.top).contains("Command") || width < 20);
            }
        }
    }
}
