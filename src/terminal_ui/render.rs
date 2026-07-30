use std::path::Path;

use crate::configuration::{
    AmbiguousWidth, BorderStyle, CommandLineConfig, Config, CursorShape, DashboardConfig,
    LineNumbers, PromptTextConfig,
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
    pub diagnostics: Vec<UiDiagnostic>,
    pub picker: Option<PickerUiState>,
    pub explorer: Option<PickerUiState>,
    pub explorer_focused: bool,
    pub terminal_panel: Option<PickerUiState>,
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
pub fn render(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    dashboard: &DashboardConfig,
    width: u16,
    height: u16,
) -> Frame {
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
            height,
        )
    } else {
        render_workspace(state, ui, config, content_width, height)
    };
    let mut frame = if let (Some(panel_width), Some(explorer)) = (explorer_width, &ui.explorer) {
        dock_explorer(
            content,
            explorer,
            state,
            ui,
            config,
            panel_width,
            width,
            height,
        )
    } else {
        content
    };
    if let Some(tool_menu) = &ui.tool_menu {
        render_tool_menu(&mut frame, tool_menu, config, width, height);
    }
    if let Some(picker) = &ui.picker {
        render_picker(&mut frame, picker, config, width, height);
    }
    if let Some(terminal) = &ui.terminal_panel {
        render_picker(&mut frame, terminal, config, width, height);
    }
    if let Some(completion) = &ui.language_completion {
        render_language_completion(&mut frame, completion, config, width, height);
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
        let y = height.saturating_sub(config.ui.notifications.rows_from_bottom);
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
        render_prompt(&mut frame, prompt, config, width, height);
    }
    frame
}

/// Width of the persistent explorer dock, leaving a usable editor pane.
#[must_use]
pub fn explorer_panel_width(config: &Config, terminal_width: u16) -> Option<u16> {
    let separator = 1;
    let minimum_editor = config.ui.panes.minimum_width.max(20);
    let maximum_panel = terminal_width.saturating_sub(minimum_editor.saturating_add(separator));
    if terminal_width < 2 {
        return None;
    }
    if maximum_panel < 12 {
        return Some(terminal_width.saturating_sub(separator));
    }
    let proportional =
        terminal_width.saturating_mul(u16::from(config.ui.explorer.width_percent)) / 100;
    Some(
        proportional
            .max(config.ui.explorer.minimum_width.min(maximum_panel))
            .min(maximum_panel),
    )
}

#[allow(clippy::too_many_arguments)]
fn dock_explorer(
    content: Frame,
    explorer: &PickerUiState,
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
    let cursor = content
        .cursor
        .map(|(x, y, shape)| (content_left.saturating_add(x), y, shape));
    Frame { grid, cursor }
}

fn render_explorer_dock(
    grid: &mut CellGrid,
    explorer: &PickerUiState,
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
    for y in 0..panel_height {
        grid.set_cell(panel_width, y, "│", Style::Border);
    }

    let title = format!(" {} ", explorer.title);
    let title_width = u16::try_from(text_width(&title, config.editor.ambiguous_width))
        .unwrap_or(panel_width)
        .min(panel_width);
    let title_left = panel_width.saturating_sub(title_width) / 2;
    for x in 0..panel_width {
        grid.set_cell(x, 0, "─", Style::Border);
    }
    grid.put_text(title_left, 0, &title, Style::Accent);

    if panel_height > 1 {
        let total = explorer.rows.len();
        let count = format!("{total}/{total}");
        let count_width =
            u16::try_from(text_width(&count, config.editor.ambiguous_width)).unwrap_or(panel_width);
        grid.put_text(
            panel_width.saturating_sub(count_width).saturating_sub(1),
            1,
            &count,
            Style::Muted,
        );
        if explorer.loading {
            grid.put_text(1, 1, &config.ui.picker.loading_label, Style::Muted);
        } else if ui.explorer_focused {
            grid.put_text(1, 1, &config.ui.explorer.selected_marker, Style::Accent);
        }
    }
    if panel_height > 2 {
        for x in 0..panel_width {
            grid.set_cell(x, 2, "─", Style::Border);
        }
    }

    let body_top = 3;
    let available_rows = usize::from(panel_height.saturating_sub(body_top));
    for (screen_row, (index, row)) in explorer
        .rows
        .iter()
        .enumerate()
        .skip(explorer.scroll)
        .take(available_rows)
        .enumerate()
    {
        let y = body_top.saturating_add(u16::try_from(screen_row).unwrap_or(0));
        let selected = explorer.selected == Some(index);
        let style = if selected {
            Style::Selection
        } else {
            Style::Default
        };
        if selected {
            for x in 0..panel_width {
                grid.set_cell(x, y, " ", style);
            }
        }
        grid.put_text(1, y, &row.label, style);
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

#[derive(Clone, Copy)]
struct PaneRect {
    pane: crate::editing::PaneId,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
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
    let tab_line_visible = config.ui.tab_line.enabled
        && (!config.ui.tab_line.hide_when_single || state.tabs.len() > 1);
    let top = u16::from(tab_line_visible);
    let pane_count = layout_pane_count(&tab.root);
    if pane_count == 1 && !tab_line_visible {
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
    let status_rows = u16::from(config.ui.status_line.enabled && height > top);
    let layout_height = height.saturating_sub(top).saturating_sub(status_rows);

    let mut grid = CellGrid::new_with_policy(
        width,
        height,
        &config.editor.control_character_replacement,
        &config.editor.zero_width_replacement,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    if tab_line_visible {
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
    let mut panes = Vec::new();
    let mut borders = Vec::new();
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
    collect_pane_rects(
        layout,
        0,
        top,
        width,
        layout_height,
        &mut panes,
        &mut borders,
    );
    let mut cursor = None;
    let mut pane_config = config.clone();
    pane_config.ui.status_line.enabled = false;
    for rect in panes {
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
    for (x, y, length, axis) in borders {
        for offset in 0..length {
            let (cell_x, cell_y, symbol) = match axis {
                crate::editing::Axis::Horizontal => (x, y.saturating_add(offset), vertical),
                crate::editing::Axis::Vertical => (x.saturating_add(offset), y, horizontal),
            };
            grid.set_cell(cell_x, cell_y, &symbol.to_string(), Style::Border);
        }
    }
    if status_rows > 0
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
    let gutter_width = if config.editor.line_numbers == LineNumbers::Off {
        0
    } else {
        u16::try_from(
            digits
                .saturating_add(text_width(
                    &config.editor.line_number_separator,
                    config.editor.ambiguous_width,
                ))
                .saturating_add(usize::from(config.editor.gutter_padding)),
        )
        .unwrap_or(u16::MAX)
    };
    let first_wrap_skip = if config.editor.wrap
        && current_line == view.scroll_line
        && width > gutter_width
        && body_height > 0
    {
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
        ) / usize::from(width - gutter_width);
        cursor_wrap_row.saturating_sub(usize::from(body_height.saturating_sub(1)))
    } else {
        0
    };
    let large_file = ui.large_file_buffers.contains(&view.buffer);
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
            grid.put_text(
                0,
                screen_row,
                &label,
                if line_index == current_line {
                    Style::Accent
                } else {
                    Style::Muted
                },
            );
        }
        let line_start = buffer
            .text
            .line_to_byte(line_index)
            .unwrap_or(ByteOffset(0));
        let line = buffer.text.line(line_index).unwrap_or_default();
        let content = line.strip_suffix('\n').unwrap_or(&line);
        let skipped_bytes = if line_index == view.scroll_line && first_wrap_skip > 0 {
            byte_offset_at_display_column(
                content,
                first_wrap_skip.saturating_mul(usize::from(width - gutter_width)),
                config.editor.tab_width,
                config.editor.ambiguous_width,
            )
        } else {
            0
        };
        let content = &content[skipped_bytes..];
        let mut x = gutter_width;
        let can_wrap = config.editor.wrap
            && !(large_file && config.editor.large_file.disable_wrap)
            && width > gutter_width;
        for (relative, grapheme) in content.grapheme_indices(true) {
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
                config.editor.tab_width
                    - (usize::from(x.saturating_sub(gutter_width)) % config.editor.tab_width)
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
        let line_end = ByteOffset(line_start.get().saturating_add(content.len()));
        for hint in inlay_hints
            .iter()
            .filter(|hint| hint.position >= line_start && hint.position <= line_end)
            .take(32)
        {
            let relative = hint
                .position
                .get()
                .saturating_sub(line_start.get())
                .min(content.len());
            if content.is_char_boundary(relative) {
                let column = display_column(
                    &content[..relative],
                    config.editor.tab_width,
                    config.editor.ambiguous_width,
                );
                let hint_x = gutter_width.saturating_add(u16::try_from(column).unwrap_or(u16::MAX));
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

    let cursor = editor_cursor(state, view_id, config, width, body_height).map(|(x, y)| {
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
) -> Option<(u16, u16)> {
    let view = state.views.get(view_id)?;
    let buffer = state.buffers.get(view.buffer)?;
    let point = PositionCodec::new(&buffer.text)
        .byte_to_point(view.cursor)
        .ok()?;
    if point.line < view.scroll_line {
        return None;
    }
    let digits = buffer.text.len_lines().max(1).to_string().len();
    let gutter = if config.editor.line_numbers == LineNumbers::Off {
        0
    } else {
        u16::try_from(
            digits
                .saturating_add(text_width(
                    &config.editor.line_number_separator,
                    config.editor.ambiguous_width,
                ))
                .saturating_add(usize::from(config.editor.gutter_padding)),
        )
        .ok()?
    };
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
    if config.editor.wrap && width > gutter {
        let available = usize::from(width - gutter);
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
        let x = gutter.saturating_add(u16::try_from(column).ok()?);
        let y = u16::try_from(point.line - view.scroll_line).ok()?;
        (x < width && y < body_height).then_some((x, y))
    }
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

    use super::{CompletionItem, PromptKind, PromptState, UiState, prompt_geometry, render};

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
    fn explorer_is_a_persistent_left_dock_that_reflows_the_editor() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state.open_text(
            crate::document::TextBuffer::from_text("editor content"),
            Some(PathBuf::from("/workspace/main.rs")),
        );
        let ui = UiState {
            explorer: Some(super::PickerUiState {
                title: "Explorer".into(),
                query: String::new(),
                rows: vec![
                    super::PickerRow {
                        id: "1".into(),
                        label: "├─▾ src".into(),
                        detail: String::new(),
                        source: String::new(),
                    },
                    super::PickerRow {
                        id: "2".into(),
                        label: "│ └─· main.rs".into(),
                        detail: String::new(),
                        source: String::new(),
                    },
                ],
                selected: Some(1),
                scroll: 0,
                loading: false,
            }),
            explorer_focused: true,
            ..UiState::default()
        };
        let loaded = crate::configuration::load_bundled_config();
        let panel = super::explorer_panel_width(&loaded.config, 100).expect("dock width");
        let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 20);
        assert!(frame.grid.row_text(0).contains("Explorer"));
        assert!(frame.grid.row_text(3).contains("src"));
        assert!(frame.grid.row_text(4).contains("main.rs"));
        assert_eq!(
            frame.grid.get(panel, 3).expect("dock separator").symbol,
            "│"
        );
        assert!(frame.cursor.is_some_and(|(x, _, _)| x > panel));
        assert!(frame.grid.row_text(19).contains("NORMAL"));
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
