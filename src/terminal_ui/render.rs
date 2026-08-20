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
use crate::windows::{
    CompositorProjection, ContentAdapterKind, ContentId, TileAxis, TiledLayoutSnapshot, WindowId,
    WindowRect,
};
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
    GitReset,
    AiChat,
    AiComposer,
}

impl PromptKind {
    #[must_use]
    pub fn title(self, text: &PromptTextConfig) -> &str {
        match self {
            Self::Command => &text.command_title,
            Self::SearchForward => &text.search_forward_title,
            Self::SearchBackward => &text.search_backward_title,
            Self::WorkspaceTrust | Self::GitReset => &text.workspace_trust_title,
            Self::AiChat => &text.ai_chat_title,
            Self::AiComposer => &text.ai_composer_title,
        }
    }

    #[must_use]
    pub fn marker(self, text: &PromptTextConfig) -> &str {
        match self {
            Self::Command => &text.command_marker,
            Self::SearchForward => &text.search_forward_marker,
            Self::SearchBackward => &text.search_backward_marker,
            Self::WorkspaceTrust | Self::GitReset => &text.workspace_trust_marker,
            Self::AiChat => &text.ai_chat_marker,
            Self::AiComposer => &text.ai_composer_marker,
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
pub struct UiGhostText {
    pub workspace_root_revision: u64,
    pub buffer: crate::editing::BufferId,
    pub revision: crate::document::BufferRevision,
    pub view: crate::editing::ViewId,
    pub cursor: ByteOffset,
    pub mode: crate::editing::EditorMode,
    pub text: String,
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
    pub severity: crate::workspace::DiagnosticSeverity,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentationPopupKind {
    Hover,
    Signature,
    Completion,
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

    fn line_at(&self, mut index: usize) -> Option<&str> {
        for section in &self.sections {
            if let Some(heading) = &section.heading {
                if index == 0 {
                    return Some(heading);
                }
                index = index.saturating_sub(1);
            }
            if let Some(line) = section.lines.get(index) {
                return Some(line);
            }
            index = index.saturating_sub(section.lines.len());
        }
        None
    }
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

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PickerSurfaceKind {
    #[default]
    Generic,
    Problems,
    Output,
    Logs,
    Debug,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PickerUiState {
    pub surface: PickerSurfaceKind,
    pub title: String,
    pub query: String,
    pub rows: Vec<PickerRow>,
    pub selected: Option<usize>,
    pub scroll: usize,
    pub loading: bool,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalPanelMode {
    Input,
    Normal,
    Visual,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalTabState {
    pub id: u64,
    pub profile: String,
    pub running: bool,
    pub exit_code: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalPanelState {
    pub id: u64,
    pub profile: String,
    pub tabs: Vec<TerminalTabState>,
    pub mode: TerminalPanelMode,
    pub running: bool,
    pub exit_code: Option<u32>,
    pub truncated: bool,
    pub unsupported_sequences: u64,
    pub focused: bool,
    pub columns: u16,
    pub rows: u16,
    pub cells: Vec<Vec<super::pty_screen::PtyCell>>,
    pub cells_start: usize,
    pub cursor: Option<(usize, usize)>,
    pub selection: Option<((usize, usize), (usize, usize), bool)>,
    pub scrollback_lines: usize,
    pub scroll_offset: usize,
    pub requested_height: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalPanelGeometry {
    pub x: u16,
    pub width: u16,
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
    pub pane: crate::editing::PaneId,
    pub view: crate::editing::ViewId,
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
pub enum UiGitSignKind {
    Added,
    Changed,
    Deleted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UiGitSign {
    pub buffer: crate::editing::BufferId,
    pub buffer_revision: u64,
    pub root_revision: u64,
    pub status_revision: u64,
    pub repository_id: crate::workspace::git::GitRepositoryId,
    pub line: usize,
    pub kind: UiGitSignKind,
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
    pub plugin_surfaces: Vec<PluginSurfaceUiState>,
    pub private_surfaces: Vec<PrivateSurfaceUiState>,
    pub key_hints: Option<KeyHintUiState>,
    pub explorer: Option<ExplorerUiState>,
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
    pub ai_ghost_text: Option<UiGhostText>,
    pub git_signs: Vec<UiGitSign>,
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
pub fn editor_workspace_insets(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    available_height: u16,
) -> (u16, u16) {
    let tab_line = config.ui.tab_line.enabled
        && (!config.ui.tab_line.hide_when_single || state.tabs.len() > 1);
    let buffer_line = config.ui.buffer_line.enabled
        && (!config.ui.buffer_line.hide_when_single || ui.content_tabs.len() > 1);
    let top = u16::from(tab_line).saturating_add(u16::from(buffer_line));
    let bottom = u16::from(config.ui.status_line.enabled && available_height > top);
    (top, bottom)
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
        x: 0,
        width: u16::MAX,
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
    let compositor = active_compositor(state, ui, width, height);
    if compositor.is_none() {
        let mut frame = Frame {
            grid: CellGrid::new_with_policy(
                width,
                height,
                &config.editor.control_character_replacement,
                &config.editor.zero_width_replacement,
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
        terminal_geometry_from_rect(compositor.projection.window(window)?.rect)
    });
    let workspace = compositor.map(|compositor| compositor.projection.workspace_bounds);
    let workspace_height = workspace.map_or(height, |bounds| bounds.height);
    let explorer_rect = compositor.and_then(|compositor| {
        let window = compositor.surfaces.explorer?;
        compositor
            .projection
            .window(window)
            .map(|window| window.rect)
    });
    let dashboard_rect = compositor.and_then(|compositor| {
        let window = compositor.surfaces.dashboard?;
        compositor
            .projection
            .window(window)
            .map(|window| window.rect)
    });
    let content_bounds = if ui.dashboard || state.active_view.is_none() {
        dashboard_rect.or(workspace)
    } else {
        workspace
    };
    let content_width = content_bounds.map_or(width, |bounds| bounds.width);
    let content_height = content_bounds.map_or(workspace_height, |bounds| bounds.height);
    let content = if ui.dashboard || state.active_view.is_none() {
        render_dashboard(
            &state.workspace_root,
            &ui.dashboard_actions,
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
    if let (Some(rect), Some(explorer)) = (explorer_rect, &ui.explorer) {
        render_explorer_window(&mut workspace_frame, explorer, ui, config, rect);
    }
    let mut frame =
        if let (Some(terminal), Some(geometry)) = (&ui.terminal_panel, terminal_geometry) {
            dock_terminal_panel(workspace_frame, terminal, geometry, config, width, height)
        } else {
            workspace_frame
        };
    if let Some(tool_menu) = &ui.tool_menu {
        render_tool_menu(&mut frame, tool_menu, config, width, workspace_height);
    }
    if let Some(compositor) = compositor {
        let mut plugin_surfaces = ui
            .plugin_surfaces
            .iter()
            .filter_map(|surface| {
                compositor
                    .projection
                    .window(surface.window)
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
        render_picker(&mut frame, picker, config, rect);
    }
    if let Some(completion) = &ui.language_completion {
        render_language_completion(&mut frame, completion, config, width, workspace_height);
    }
    if let Some(documentation) = &ui.documentation_popup {
        render_documentation_popup(
            &mut frame,
            documentation,
            config,
            workspace.map_or(0, |bounds| bounds.x),
            content_width,
            workspace_height,
        );
    }
    if let Some(key_hints) = &ui.key_hints {
        render_key_hints(
            &mut frame,
            key_hints,
            config,
            workspace.map_or(0, |bounds| bounds.x),
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

fn active_compositor<'a>(
    state: &EditorState,
    ui: &'a UiState,
    width: u16,
    height: u16,
) -> Option<&'a CompositorLayoutState> {
    let revision = state.window_revision()?;
    ui.compositor_layout.as_ref().filter(|compositor| {
        compositor.source_revision == revision
            && Some(compositor.graph_revision) == ui.window_graph_revision
            && compositor.projection.bounds.width == width
            && compositor.projection.bounds.height == height
    })
}

fn terminal_geometry_from_rect(rect: WindowRect) -> Option<TerminalPanelGeometry> {
    (rect.width > 0 && rect.height >= 3).then_some(TerminalPanelGeometry {
        x: rect.x,
        width: rect.width,
        top: rect.y,
        height: rect.height,
        body_top: rect.y.saturating_add(1),
        body_height: rect.height.saturating_sub(2),
        status_y: rect.y.saturating_add(rect.height).saturating_sub(1),
    })
}

fn terminal_tab_label(tab: &TerminalTabState, config: &Config) -> String {
    let mut title = config
        .ui
        .terminal_panel
        .title_format
        .replace("{id}", &tab.id.to_string())
        .replace("{profile}", &tab.profile);
    if !tab.running {
        title.push_str(
            &tab.exit_code
                .map_or_else(|| " [signal]".into(), |code| format!(" [exit {code}]")),
        );
    }
    title
}

#[must_use]
pub fn terminal_tab_at_column(
    terminal: &TerminalPanelState,
    config: &Config,
    width: u16,
    column: u16,
) -> Option<u64> {
    let mut x = 1_u16;
    for (index, tab) in terminal.tabs.iter().enumerate() {
        if index > 0 {
            x = x.saturating_add(
                u16::try_from(text_width(
                    &config.ui.terminal_panel.tab_separator,
                    config.editor.ambiguous_width,
                ))
                .unwrap_or(u16::MAX),
            );
        }
        let label = terminal_tab_label(tab, config);
        let end = x
            .saturating_add(
                u16::try_from(text_width(&label, config.editor.ambiguous_width))
                    .unwrap_or(u16::MAX),
            )
            .min(width);
        if column >= x && column < end {
            return Some(tab.id);
        }
        x = end;
        if x >= width {
            break;
        }
    }
    None
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
    for y in 0..workspace.grid.height().min(height) {
        for x in 0..workspace.grid.width().min(width) {
            let Some(cell) = workspace.grid.get(x, y) else {
                continue;
            };
            if !cell.continuation {
                grid.set_cell(x, y, &cell.symbol, cell.style);
            }
        }
    }
    let right = geometry.x.saturating_add(geometry.width).min(width);
    let bottom = geometry.top.saturating_add(geometry.height).min(height);
    for y in geometry.top..bottom {
        for x in geometry.x..right {
            grid.set_cell(x, y, " ", Style::TerminalBackground);
        }
    }
    for x in geometry.x..right {
        grid.set_cell(
            x,
            geometry.top,
            &config.ui.terminal_panel.divider,
            Style::TerminalBorder,
        );
    }
    let mut title_x = geometry.x.saturating_add(1);
    for (index, tab) in terminal.tabs.iter().enumerate() {
        if index > 0 {
            title_x = grid.put_text_clipped(
                title_x,
                geometry.top,
                &config.ui.terminal_panel.tab_separator,
                Style::TerminalBorder,
                right,
            );
        }
        let title = terminal_tab_label(tab, config);
        title_x = grid.put_text_clipped(
            title_x,
            geometry.top,
            &title,
            if tab.id == terminal.id {
                Style::TerminalTitle
            } else {
                Style::TerminalBorder
            },
            right,
        );
    }

    let visible_rows = usize::from(geometry.body_height).min(terminal.cells.len());
    let source_start = terminal.cells_start;
    for (screen_row, row) in terminal.cells.iter().take(visible_rows).enumerate() {
        let source_row = source_start.saturating_add(screen_row);
        let y = geometry
            .body_top
            .saturating_add(u16::try_from(screen_row).unwrap_or(u16::MAX));
        for (column, cell) in row
            .iter()
            .take(usize::from(geometry.width.min(terminal.columns)))
            .enumerate()
        {
            if cell.continuation {
                continue;
            }
            let selected = terminal.selection.is_some_and(|(first, second, linewise)| {
                let (start, end) = if first <= second {
                    (first, second)
                } else {
                    (second, first)
                };
                if linewise {
                    (start.0..=end.0).contains(&source_row)
                } else {
                    (source_row, column) >= start && (source_row, column) <= end
                }
            });
            grid.set_cell(
                geometry
                    .x
                    .saturating_add(u16::try_from(column).unwrap_or(u16::MAX)),
                y,
                &cell.text,
                if selected {
                    Style::Selection
                } else {
                    Style::Terminal(cell.style)
                },
            );
        }
    }

    for x in geometry.x..right {
        grid.set_cell(x, geometry.status_y, " ", Style::TerminalStatus);
    }
    let mode = match terminal.mode {
        TerminalPanelMode::Input => &config.ui.terminal_panel.input_mode_label,
        TerminalPanelMode::Normal => &config.ui.terminal_panel.normal_mode_label,
        TerminalPanelMode::Visual => &config.ui.terminal_panel.visual_mode_label,
    };
    let mut status_line = if terminal.focused {
        format!(" {mode}  {}: {}", terminal.id, terminal.profile)
    } else {
        format!(" {}: {}", terminal.id, terminal.profile)
    };
    if terminal.truncated {
        status_line.push_str(" · output truncated");
    }
    grid.put_text_clipped(
        geometry.x,
        geometry.status_y,
        &status_line,
        if terminal.focused {
            Style::TerminalStatusAccent
        } else {
            Style::TerminalStatus
        },
        right,
    );

    let cursor = if terminal.focused {
        terminal.cursor.and_then(|(column, source_row)| {
            let projected_row = source_row.checked_sub(source_start)?;
            let y = geometry
                .body_top
                .saturating_add(u16::try_from(projected_row).ok()?);
            (column < usize::from(geometry.width) && y < geometry.status_y).then_some((
                geometry.x.saturating_add(u16::try_from(column).ok()?),
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

fn place_workspace_frame(
    content: Frame,
    bounds: WindowRect,
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
    for y in 0..content.grid.height().min(bounds.height) {
        for x in 0..content.grid.width().min(bounds.width) {
            if let Some(cell) = content.grid.get(x, y)
                && !cell.continuation
            {
                grid.set_cell(
                    bounds.x.saturating_add(x),
                    bounds.y.saturating_add(y),
                    &cell.symbol,
                    cell.style,
                );
            }
        }
    }
    let cursor = content.cursor.and_then(|(x, y, shape)| {
        (x < bounds.width && y < bounds.height).then_some((
            bounds.x.saturating_add(x),
            bounds.y.saturating_add(y),
            shape,
        ))
    });
    Frame { grid, cursor }
}

fn render_explorer_window(
    frame: &mut Frame,
    explorer: &ExplorerUiState,
    ui: &UiState,
    config: &Config,
    rect: WindowRect,
) {
    if rect.width < 2 || rect.height == 0 {
        return;
    }
    let mut local = CellGrid::new_with_policy(
        rect.width,
        rect.height,
        &config.editor.control_character_replacement,
        &config.editor.zero_width_replacement,
        config.editor.ambiguous_width == AmbiguousWidth::Wide,
    );
    let panel_width = rect.width.saturating_sub(1);
    render_explorer_dock(&mut local, explorer, ui, config, panel_width, rect.height);
    for y in 0..rect.height {
        for x in 0..rect.width {
            if let Some(cell) = local.get(x, y)
                && !cell.continuation
            {
                frame.grid.set_cell(
                    rect.x.saturating_add(x),
                    rect.y.saturating_add(y),
                    &cell.symbol,
                    cell.style,
                );
            }
        }
    }
    if ui.explorer_focused {
        frame.cursor = explorer
            .rows
            .iter()
            .position(|row| row.selected)
            .and_then(|row| u16::try_from(row).ok())
            .map(|row| {
                (
                    rect.x.saturating_add(1.min(panel_width.saturating_sub(1))),
                    rect.y.saturating_add(3).saturating_add(row),
                    cursor_style(config.ui.cursor.normal),
                )
            })
            .filter(|(_, y, _)| *y < rect.y.saturating_add(rect.height));
    }
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
    let line_count = popup.line_count();
    if !policy.enabled || line_count == 0 || editor_width < 4 || available_height < 3 {
        return;
    }
    let widest = (0..line_count)
        .filter_map(|index| popup.line_at(index))
        .map(UnicodeWidthStr::width)
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
    let visible_rows = line_count
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
    for (row, line) in (popup.scroll..popup.scroll.saturating_add(visible_rows))
        .filter_map(|index| popup.line_at(index))
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

fn truncate_content_title(
    title: &str,
    maximum_width: usize,
    ambiguous_width: AmbiguousWidth,
) -> String {
    if text_width(title, ambiguous_width) <= maximum_width {
        return title.to_owned();
    }
    let ellipsis = "…";
    let available = maximum_width.saturating_sub(text_width(ellipsis, ambiguous_width));
    let mut suffix = Vec::new();
    let mut width = 0usize;
    for grapheme in title.graphemes(true).rev() {
        let grapheme_width = text_width(grapheme, ambiguous_width).max(1);
        if width.saturating_add(grapheme_width) > available {
            break;
        }
        suffix.push(grapheme);
        width = width.saturating_add(grapheme_width);
    }
    suffix.reverse();
    format!("{ellipsis}{}", suffix.concat())
}

fn formatted_content_tab(
    entry: &ContentTabEntry,
    config: &Config,
) -> (String, String, String, Style) {
    let format = if entry.active {
        &config.ui.buffer_line.active_format
    } else {
        &config.ui.buffer_line.inactive_format
    };
    let title = truncate_content_title(
        &entry.title,
        usize::from(config.ui.buffer_line.maximum_name_width),
        config.editor.ambiguous_width,
    );
    let format = format.replace("{title}", &title);
    let (before, after) = format
        .split_once("{dirty}")
        .expect("validated buffer-line format");
    (
        before.to_owned(),
        if entry.dirty {
            config.ui.buffer_line.dirty_marker.clone()
        } else {
            String::new()
        },
        after.to_owned(),
        if entry.active {
            Style::BufferLineActive
        } else {
            Style::BufferLineInactive
        },
    )
}

fn content_tab_segments(entry: &ContentTabEntry, config: &Config) -> Vec<(String, Style)> {
    let (before, dirty, after, style) = formatted_content_tab(entry, config);
    let mut segments = Vec::with_capacity(8);
    if config.ui.buffer_line.show_pins && entry.pinned {
        segments.push((
            format!("{} ", config.ui.buffer_line.pin_marker),
            Style::BufferLinePin,
        ));
    }
    if config.ui.buffer_line.show_icons {
        segments.push((format!("{} ", entry.icon), Style::BufferLineIcon));
    }
    segments.push((before, style));
    if !dirty.is_empty() {
        segments.push((dirty, Style::BufferLineDirty));
    }
    if config.ui.buffer_line.show_diagnostics
        && let Some(diagnostic) = entry.diagnostic
    {
        let marker = match diagnostic.severity {
            ContentTabDiagnosticSeverity::Error => &config.ui.buffer_line.diagnostic_error_marker,
            ContentTabDiagnosticSeverity::Warning => {
                &config.ui.buffer_line.diagnostic_warning_marker
            }
        };
        segments.push((
            format!(" {marker}{}", diagnostic.count),
            Style::BufferLineDiagnostic,
        ));
    }
    if config.ui.buffer_line.show_close && entry.closeable {
        segments.push((
            format!(" {}", config.ui.buffer_line.close_marker),
            Style::BufferLineClose,
        ));
    }
    segments.push((after, style));
    segments.push((
        config.ui.buffer_line.separator.clone(),
        Style::BufferLineSeparator,
    ));
    segments
}
fn content_tab_width(entry: &ContentTabEntry, config: &Config) -> usize {
    content_tab_segments(entry, config)
        .iter()
        .map(|(text, _)| text_width(text, config.editor.ambiguous_width))
        .sum()
}

#[must_use]
pub fn content_tab_at_column(
    entries: &[ContentTabEntry],
    config: &Config,
    width: u16,
    column: u16,
) -> Option<ContentTabIdentity> {
    if entries.is_empty() || column >= width {
        return None;
    }
    let active = entries.iter().position(|e| e.active).unwrap_or(0);
    let left = text_width(
        &config.ui.buffer_line.overflow_left,
        config.editor.ambiguous_width,
    );
    let right = text_width(
        &config.ui.buffer_line.overflow_right,
        config.editor.ambiguous_width,
    );
    let available = usize::from(width);
    let mut start = 0;
    while start < active {
        let required = entries[start..=active]
            .iter()
            .map(|e| content_tab_width(e, config))
            .sum::<usize>()
            .saturating_add(if start > 0 { left } else { 0 })
            .saturating_add(if active + 1 < entries.len() { right } else { 0 });
        if required <= available {
            break;
        }
        start += 1
    }
    let mut x = if start > 0 {
        u16::try_from(left).unwrap_or(width)
    } else {
        0
    };
    for (index, entry) in entries.iter().enumerate().skip(start) {
        let item = content_tab_width(entry, config);
        let remaining = usize::from(width.saturating_sub(x));
        let reserve = if index + 1 < entries.len() { right } else { 0 };
        if item.saturating_add(reserve) > remaining && index > active {
            break;
        }
        let end = x
            .saturating_add(u16::try_from(item).unwrap_or(u16::MAX))
            .min(width);
        if column >= x && column < end {
            return Some(entry.identity.clone());
        }
        x = end;
        if x >= width {
            break;
        }
    }
    None
}

fn render_content_tab_line(
    grid: &mut CellGrid,
    entries: &[ContentTabEntry],
    config: &Config,
    row: u16,
) {
    let width = grid.width();
    if width == 0 || row >= grid.height() {
        return;
    }
    grid.fill_row(row, Style::BufferLineBackground);
    if entries.is_empty() {
        return;
    }
    let active = entries.iter().position(|entry| entry.active).unwrap_or(0);
    let left_width = text_width(
        &config.ui.buffer_line.overflow_left,
        config.editor.ambiguous_width,
    );
    let right_width = text_width(
        &config.ui.buffer_line.overflow_right,
        config.editor.ambiguous_width,
    );
    let available = usize::from(width);
    let mut start = 0usize;
    while start < active {
        let required = entries[start..=active]
            .iter()
            .map(|entry| content_tab_width(entry, config))
            .sum::<usize>()
            .saturating_add(if start > 0 { left_width } else { 0 })
            .saturating_add(if active + 1 < entries.len() {
                right_width
            } else {
                0
            });
        if required <= available {
            break;
        }
        start += 1;
    }

    let mut x = 0u16;
    if start > 0 {
        x = grid.put_text_clipped(
            x,
            row,
            &config.ui.buffer_line.overflow_left,
            Style::BufferLineSeparator,
            width,
        );
    }
    let mut end = start;
    for (index, entry) in entries.iter().enumerate().skip(start) {
        let item_width = content_tab_width(entry, config);
        let remaining = usize::from(width.saturating_sub(x));
        let reserve_right = if index + 1 < entries.len() {
            right_width
        } else {
            0
        };
        if item_width.saturating_add(reserve_right) > remaining && index > active {
            break;
        }
        for (text, style) in content_tab_segments(entry, config) {
            x = grid.put_text_clipped(x, row, &text, style, width);
        }
        end = index + 1;
        if x >= width {
            break;
        }
    }
    if end < entries.len() && right_width <= available {
        let marker_x = width.saturating_sub(u16::try_from(right_width).unwrap_or(width));
        grid.put_text_clipped(
            marker_x,
            row,
            &config.ui.buffer_line.overflow_right,
            Style::BufferLineSeparator,
            width,
        );
    }
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
    view: crate::editing::ViewId,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

struct WorkspaceProjection {
    tab_line_visible: bool,
    buffer_line_visible: bool,
    status_rows: u16,
    panes: Vec<PaneRect>,
    borders: Vec<crate::windows::ProjectedBorder>,
}

fn workspace_projection(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    width: u16,
    height: u16,
) -> Option<WorkspaceProjection> {
    state.tabs.get(state.active_tab?)?;
    let source_revision = state.window_revision()?;
    let layout = ui.editor_window_layout.as_ref()?;
    let compositor = ui.compositor_layout.as_ref()?;
    if layout.source_revision != source_revision
        || compositor.source_revision != source_revision
        || compositor.projection.workspace_bounds.width != width
        || compositor.projection.workspace_bounds.height != height
    {
        return None;
    }
    let tab_line_visible = config.ui.tab_line.enabled
        && (!config.ui.tab_line.hide_when_single || state.tabs.len() > 1);
    let buffer_line_visible = config.ui.buffer_line.enabled
        && (!config.ui.buffer_line.hide_when_single || ui.content_tabs.len() > 1);
    let top = u16::from(tab_line_visible).saturating_add(u16::from(buffer_line_visible));
    let status_rows = u16::from(config.ui.status_line.enabled && height > top);
    let origin = compositor.projection.workspace_bounds;
    let panes = compositor
        .projection
        .windows
        .iter()
        .filter(|projected| projected.layer == crate::windows::CompositorLayer::Tiled)
        .filter_map(|projected| {
            let binding = layout
                .bindings
                .iter()
                .find(|binding| binding.window == projected.window)?;
            Some(PaneRect {
                pane: binding.pane,
                view: binding.view,
                x: projected.rect.x.saturating_sub(origin.x),
                y: projected.rect.y.saturating_sub(origin.y),
                width: projected.rect.width,
                height: projected.rect.height,
            })
        })
        .collect();
    let borders = compositor
        .projection
        .borders
        .iter()
        .map(|border| crate::windows::ProjectedBorder {
            x: border.x.saturating_sub(origin.x),
            y: border.y.saturating_sub(origin.y),
            ..*border
        })
        .collect();
    Some(WorkspaceProjection {
        tab_line_visible,
        buffer_line_visible,
        status_rows,
        panes,
        borders,
    })
}

/// Exact generic-window rectangles used by rendering, cursor following, and mouse projection.
#[must_use]
pub fn pane_viewports(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    width: u16,
    height: u16,
) -> Vec<PaneViewport> {
    workspace_projection(state, ui, config, width, height).map_or_else(Vec::new, |projection| {
        projection
            .panes
            .into_iter()
            .map(|rect| PaneViewport {
                pane: rect.pane,
                view: rect.view,
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            })
            .collect()
    })
}

#[must_use]
pub fn active_pane_viewport(
    state: &EditorState,
    ui: &UiState,
    config: &Config,
    width: u16,
    height: u16,
) -> Option<PaneViewport> {
    let tab = state.tabs.get(state.active_tab?)?;
    pane_viewports(state, ui, config, width, height)
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
    let pane_count = ui
        .editor_window_layout
        .as_ref()
        .map_or(0, |layout| layout.bindings.len());
    let Some(projection) = workspace_projection(state, ui, config, width, height) else {
        return Frame {
            grid: CellGrid::new(width, height),
            cursor: None,
        };
    };
    if pane_count == 1 && !projection.tab_line_visible && !projection.buffer_line_visible {
        let Some(view) = projection.panes.first().map(|pane| pane.view) else {
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
    if projection.buffer_line_visible {
        render_content_tab_line(
            &mut grid,
            &ui.content_tabs,
            config,
            u16::from(projection.tab_line_visible),
        );
    }
    let mut cursor = None;
    let mut pane_config = config.clone();
    pane_config.ui.status_line.enabled = false;
    for rect in projection.panes {
        let frame = render_editor_view(state, rect.view, ui, &pane_config, rect.width, rect.height);
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
    for border in projection.borders {
        for offset in 0..border.length {
            let (cell_x, cell_y, symbol) = match border.axis {
                TileAxis::Horizontal => (border.x, border.y.saturating_add(offset), vertical),
                TileAxis::Vertical => (border.x.saturating_add(offset), border.y, horizontal),
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

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct VisualRowAnchor {
    pub line: usize,
    pub wrap_row: usize,
}

/// Pure visual-row ownership shared by viewport follow, rendering, paging,
/// cursor projection, and mouse hit testing. It reads immutable editor/UI
/// snapshots only and performs no parsing or service work.
pub struct VisualRowModel<'a> {
    text: &'a crate::document::TextBuffer,
    folds: Vec<(usize, usize)>,
    wrap: bool,
    content_width: usize,
    tab_width: usize,
    ambiguous_width: AmbiguousWidth,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VisualRowSlice {
    pub byte_start: usize,
    pub byte_end: usize,
    pub source_column_start: usize,
    pub used_cells: usize,
}

impl<'a> VisualRowModel<'a> {
    #[must_use]
    pub fn new(
        text: &'a crate::document::TextBuffer,
        view: crate::editing::ViewId,
        ui: &UiState,
        config: &Config,
        pane_width: u16,
        wrap: bool,
    ) -> Self {
        Self::new_with_minimum_digits(
            text,
            view,
            ui,
            config,
            pane_width,
            wrap,
            line_number_digits(text),
        )
    }

    #[must_use]
    pub fn new_for_state(
        state: &EditorState,
        text: &'a crate::document::TextBuffer,
        view: crate::editing::ViewId,
        ui: &UiState,
        config: &Config,
        pane_width: u16,
        wrap: bool,
    ) -> Self {
        Self::new_with_minimum_digits(
            text,
            view,
            ui,
            config,
            pane_width,
            wrap,
            editor_line_number_digits(state),
        )
    }

    fn new_with_minimum_digits(
        text: &'a crate::document::TextBuffer,
        view: crate::editing::ViewId,
        ui: &UiState,
        config: &Config,
        pane_width: u16,
        wrap: bool,
        minimum_digits: usize,
    ) -> Self {
        let maximum = text.len_lines().saturating_sub(1);
        let mut folds = ui
            .lsp_folds
            .iter()
            .chain(ui.syntax_folds.iter())
            .filter(|fold| fold.view == view && fold.collapsed)
            .filter_map(|fold| {
                let start = fold.start_line.min(maximum);
                let end = fold.end_line.min(maximum);
                (start < end).then_some((start, end))
            })
            .collect::<Vec<_>>();
        folds.sort_unstable_by_key(|&(start, end)| (start, std::cmp::Reverse(end)));
        let mut normalized_folds: Vec<(usize, usize)> = Vec::with_capacity(folds.len());
        for (start, end) in folds {
            if let Some(last) = normalized_folds.last_mut()
                && start <= last.1
            {
                last.1 = last.1.max(end);
            } else {
                normalized_folds.push((start, end));
            }
        }
        Self {
            text,
            folds: normalized_folds,
            wrap,
            content_width: usize::from(pane_width.saturating_sub(editor_gutter_width_with_digits(
                text,
                &config.editor,
                pane_width,
                minimum_digits,
            )))
            .max(1),
            tab_width: config.editor.tab_width.max(1),
            ambiguous_width: config.editor.ambiguous_width,
        }
    }

    fn containing_fold(&self, line: usize) -> Option<(usize, usize)> {
        self.folds
            .iter()
            .copied()
            .find(|&(start, end)| line > start && line <= end)
    }

    fn grapheme_width(&self, grapheme: &str, source_column: usize) -> usize {
        if grapheme == "\t" {
            self.tab_width - (source_column % self.tab_width)
        } else {
            text_width(grapheme, self.ambiguous_width).max(1)
        }
    }

    /// Return exact grapheme-atomic slices for one logical line. A grapheme is
    /// never split across rows. If it is wider than a tiny content body, it
    /// alone consumes one row and its visible width is clipped to that body.
    #[must_use]
    pub fn line_rows(&self, line: usize) -> Vec<VisualRowSlice> {
        let text = self.text.line(line).unwrap_or_default();
        let content = text.strip_suffix('\n').unwrap_or(&text);
        if !self.wrap {
            return vec![VisualRowSlice {
                byte_start: 0,
                byte_end: content.len(),
                source_column_start: 0,
                used_cells: display_column(content, self.tab_width, self.ambiguous_width),
            }];
        }
        if content.is_empty() {
            return vec![VisualRowSlice {
                byte_start: 0,
                byte_end: 0,
                source_column_start: 0,
                used_cells: 0,
            }];
        }
        let mut rows = Vec::new();
        let mut row_start = 0usize;
        let mut row_source_column = 0usize;
        let mut row_used = 0usize;
        let mut source_column = 0usize;
        for (relative, grapheme) in content.grapheme_indices(true) {
            let source_width = self.grapheme_width(grapheme, source_column);
            let visible_width = source_width.min(self.content_width);
            if row_used > 0 && row_used.saturating_add(visible_width) > self.content_width {
                rows.push(VisualRowSlice {
                    byte_start: row_start,
                    byte_end: relative,
                    source_column_start: row_source_column,
                    used_cells: row_used,
                });
                row_start = relative;
                row_source_column = source_column;
                row_used = 0;
            }
            row_used = row_used.saturating_add(visible_width);
            source_column = source_column.saturating_add(source_width);
        }
        rows.push(VisualRowSlice {
            byte_start: row_start,
            byte_end: content.len(),
            source_column_start: row_source_column,
            used_cells: row_used,
        });
        rows
    }

    #[must_use]
    pub fn row_slice(&self, anchor: VisualRowAnchor) -> VisualRowSlice {
        let anchor = self.normalize(anchor);
        self.line_rows(anchor.line)
            .get(anchor.wrap_row)
            .copied()
            .unwrap_or(VisualRowSlice {
                byte_start: 0,
                byte_end: 0,
                source_column_start: 0,
                used_cells: 0,
            })
    }

    fn row_cell_for_relative_byte(&self, line: usize, row: VisualRowSlice, byte: usize) -> usize {
        let text = self.text.line(line).unwrap_or_default();
        let content = text.strip_suffix('\n').unwrap_or(&text);
        let end = row.byte_end.min(content.len());
        let mut source_column = row.source_column_start;
        let mut cell = 0usize;
        for (relative, grapheme) in content[row.byte_start..end].grapheme_indices(true) {
            let relative = row.byte_start.saturating_add(relative);
            if byte <= relative {
                return cell.min(self.content_width.saturating_sub(1));
            }
            let source_width = self.grapheme_width(grapheme, source_column);
            let visible_width = source_width.min(self.content_width);
            if byte < relative.saturating_add(grapheme.len()) {
                return cell.min(self.content_width.saturating_sub(1));
            }
            cell = cell.saturating_add(visible_width);
            source_column = source_column.saturating_add(source_width);
        }
        // Exact-width line endings stay on the final owned cell; no synthetic
        // empty row is introduced between adjacent logical lines.
        cell.min(self.content_width.saturating_sub(1))
    }

    #[must_use]
    pub fn cell_for_byte(&self, anchor: VisualRowAnchor, byte: usize) -> usize {
        let anchor = self.normalize(anchor);
        self.row_cell_for_relative_byte(anchor.line, self.row_slice(anchor), byte)
    }

    #[must_use]
    pub fn byte_at_cell(&self, anchor: VisualRowAnchor, cell: usize) -> usize {
        let anchor = self.normalize(anchor);
        let row = self.row_slice(anchor);
        let text = self.text.line(anchor.line).unwrap_or_default();
        let content = text.strip_suffix('\n').unwrap_or(&text);
        let end = row.byte_end.min(content.len());
        let mut source_column = row.source_column_start;
        let mut used = 0usize;
        for (relative, grapheme) in content[row.byte_start..end].grapheme_indices(true) {
            let relative = row.byte_start.saturating_add(relative);
            let source_width = self.grapheme_width(grapheme, source_column);
            let visible_width = source_width.min(self.content_width);
            if cell < used.saturating_add(visible_width) {
                return relative;
            }
            used = used.saturating_add(visible_width);
            source_column = source_column.saturating_add(source_width);
        }
        row.byte_end
    }

    #[must_use]
    pub fn normalize(&self, anchor: VisualRowAnchor) -> VisualRowAnchor {
        let maximum = self.text.len_lines().saturating_sub(1);
        let mut line = anchor.line.min(maximum);
        if let Some((start, _)) = self.containing_fold(line) {
            line = start;
        }
        VisualRowAnchor {
            line,
            wrap_row: anchor
                .wrap_row
                .min(self.rows_for_line(line).saturating_sub(1)),
        }
    }

    #[must_use]
    pub fn rows_for_line(&self, line: usize) -> usize {
        self.line_rows(line).len()
    }

    fn next_line(&self, line: usize) -> usize {
        let folded_end = self
            .folds
            .iter()
            .find_map(|&(start, end)| (start == line).then_some(end))
            .unwrap_or(line);
        folded_end
            .saturating_add(1)
            .min(self.text.len_lines().saturating_sub(1))
    }

    fn previous_line(&self, line: usize) -> usize {
        let previous = line.saturating_sub(1);
        self.containing_fold(previous)
            .map_or(previous, |(start, _)| start)
    }

    #[must_use]
    pub fn next(&self, anchor: VisualRowAnchor) -> VisualRowAnchor {
        let anchor = self.normalize(anchor);
        if anchor.wrap_row + 1 < self.rows_for_line(anchor.line) {
            return VisualRowAnchor {
                line: anchor.line,
                wrap_row: anchor.wrap_row + 1,
            };
        }
        let line = self.next_line(anchor.line);
        if line == anchor.line {
            anchor
        } else {
            VisualRowAnchor { line, wrap_row: 0 }
        }
    }

    #[must_use]
    pub fn previous(&self, anchor: VisualRowAnchor) -> VisualRowAnchor {
        let anchor = self.normalize(anchor);
        if anchor.wrap_row > 0 {
            return VisualRowAnchor {
                line: anchor.line,
                wrap_row: anchor.wrap_row - 1,
            };
        }
        let line = self.previous_line(anchor.line);
        if line == anchor.line {
            anchor
        } else {
            VisualRowAnchor {
                line,
                wrap_row: self.rows_for_line(line).saturating_sub(1),
            }
        }
    }

    #[must_use]
    pub fn forward(&self, mut anchor: VisualRowAnchor, count: usize) -> VisualRowAnchor {
        anchor = self.normalize(anchor);
        for _ in 0..count {
            let next = self.next(anchor);
            if next == anchor {
                break;
            }
            anchor = next;
        }
        anchor
    }

    #[must_use]
    pub fn backward(&self, mut anchor: VisualRowAnchor, count: usize) -> VisualRowAnchor {
        anchor = self.normalize(anchor);
        for _ in 0..count {
            let previous = self.previous(anchor);
            if previous == anchor {
                break;
            }
            anchor = previous;
        }
        anchor
    }

    #[must_use]
    pub fn cursor_projection(&self, cursor: ByteOffset) -> (VisualRowAnchor, usize) {
        let line = self.text.byte_to_line(cursor).unwrap_or(0);
        if let Some((start, _)) = self.containing_fold(line) {
            return (
                VisualRowAnchor {
                    line: start,
                    wrap_row: 0,
                },
                0,
            );
        }
        if !self.wrap {
            let line_start = self.text.line_to_byte(line).unwrap_or(ByteOffset(0));
            let prefix = self
                .text
                .slice(ByteRange::new(line_start, cursor).expect("cursor line range"))
                .unwrap_or_default();
            return (
                VisualRowAnchor { line, wrap_row: 0 },
                display_column(&prefix, self.tab_width, self.ambiguous_width),
            );
        }
        let line_start = self.text.line_to_byte(line).unwrap_or(ByteOffset(0));
        let relative = cursor.get().saturating_sub(line_start.get());
        let rows = self.line_rows(line);
        let row_index = rows
            .iter()
            .enumerate()
            .find_map(|(index, row)| {
                (relative < row.byte_end || (relative == row.byte_end && index + 1 == rows.len()))
                    .then_some(index)
            })
            .unwrap_or_else(|| rows.len().saturating_sub(1));
        let anchor = VisualRowAnchor {
            line,
            wrap_row: row_index,
        };
        (
            anchor,
            self.row_cell_for_relative_byte(line, rows[row_index], relative),
        )
    }

    #[must_use]
    pub fn cursor_anchor(&self, cursor: ByteOffset) -> VisualRowAnchor {
        self.cursor_projection(cursor).0
    }

    #[must_use]
    pub fn screen_row(
        &self,
        start: VisualRowAnchor,
        target: VisualRowAnchor,
        height: usize,
    ) -> Option<usize> {
        let mut current = self.normalize(start);
        let target = self.normalize(target);
        for row in 0..height {
            if current == target {
                return Some(row);
            }
            let next = self.next(current);
            if next == current {
                break;
            }
            current = next;
        }
        None
    }
}

fn one_cell_git_symbol<'a>(
    symbol: &'a str,
    fallback: &'static str,
    ambiguous_width: AmbiguousWidth,
) -> &'a str {
    if text_width(symbol, ambiguous_width) == 1 {
        symbol
    } else {
        fallback
    }
}

fn line_number_digits(text: &crate::document::TextBuffer) -> usize {
    text.len_lines().max(1).to_string().len()
}

#[must_use]
pub fn editor_line_number_digits(state: &EditorState) -> usize {
    state
        .buffers
        .values()
        .map(|buffer| line_number_digits(&buffer.text))
        .max()
        .unwrap_or(1)
}

fn configured_editor_gutter_width(
    text: &crate::document::TextBuffer,
    config: &EditorConfig,
    minimum_digits: usize,
) -> usize {
    if config.line_numbers == LineNumbers::Off {
        return 0;
    }
    line_number_digits(text)
        .max(minimum_digits)
        .saturating_add(text_width(
            &config.line_number_separator,
            config.ambiguous_width,
        ))
        .saturating_add(
            usize::from(config.gutter_padding).max(usize::from(config.git_signs.enabled)),
        )
}

fn editor_gutter_width_with_digits(
    text: &crate::document::TextBuffer,
    config: &EditorConfig,
    pane_width: u16,
    minimum_digits: usize,
) -> u16 {
    let maximum = pane_width.saturating_sub(1);
    u16::try_from(configured_editor_gutter_width(text, config, minimum_digits))
        .unwrap_or(u16::MAX)
        .min(maximum)
}

/// Effective display-cell gutter for an exact pane width. Every non-empty
/// pane reserves at least one cell for text/cursor projection.
#[must_use]
pub fn editor_gutter_width(
    text: &crate::document::TextBuffer,
    config: &EditorConfig,
    pane_width: u16,
) -> u16 {
    editor_gutter_width_with_digits(text, config, pane_width, line_number_digits(text))
}

#[must_use]
pub fn editor_state_gutter_width(
    state: &EditorState,
    text: &crate::document::TextBuffer,
    config: &EditorConfig,
    pane_width: u16,
) -> u16 {
    editor_gutter_width_with_digits(text, config, pane_width, editor_line_number_digits(state))
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

#[must_use]
pub fn editor_state_content_width(
    state: &EditorState,
    text: &crate::document::TextBuffer,
    config: &EditorConfig,
    pane_width: u16,
) -> usize {
    usize::from(
        pane_width.saturating_sub(editor_state_gutter_width(state, text, config, pane_width)),
    )
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
    let digits = editor_line_number_digits(state);
    let gutter_width = editor_state_gutter_width(state, &buffer.text, &config.editor, width);
    let content_width = editor_state_content_width(state, &buffer.text, &config.editor, width);
    let large_file = ui.large_file_buffers.contains(&view.buffer);
    let wrap_enabled = content_width > 0
        && config.editor.wrap
        && !(large_file && config.editor.large_file.disable_wrap);
    let visual_rows = VisualRowModel::new_for_state(
        state,
        &buffer.text,
        view_id,
        ui,
        config,
        width,
        wrap_enabled,
    );
    let configured_start = visual_rows.normalize(VisualRowAnchor {
        line: view.scroll_line,
        wrap_row: view.scroll_wrap_row,
    });
    let cursor_anchor = visual_rows.cursor_anchor(view.cursor);
    let start_anchor = if configured_start.line == cursor_anchor.line
        && visual_rows
            .screen_row(
                configured_start,
                cursor_anchor,
                usize::from(body_height.max(1)),
            )
            .is_none()
    {
        visual_rows.backward(cursor_anchor, usize::from(body_height.saturating_sub(1)))
    } else {
        configured_start
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
    let mut line_index = start_anchor.line;
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
        let line_rows = visual_rows.line_rows(line_index);
        let first_line_row = if line_index == start_anchor.line {
            start_anchor.wrap_row.min(line_rows.len().saturating_sub(1))
        } else {
            0
        };
        if gutter_width > 0 && first_line_row == 0 {
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
        if config.editor.git_signs.enabled && gutter_width > 0 && first_line_row == 0 {
            grid.set_cell(
                gutter_width - 1,
                screen_row,
                one_cell_git_symbol(
                    &config.editor.git_signs.unchanged,
                    "|",
                    config.editor.ambiguous_width,
                ),
                Style::Muted,
            );
            if let Some(sign) = ui.git_signs.iter().find(|sign| {
                sign.buffer == view.buffer
                    && sign.buffer_revision == buffer.text.revision().0
                    && sign.line == line_index
            }) {
                let (symbol, fallback, style) = match sign.kind {
                    UiGitSignKind::Added => {
                        (&config.editor.git_signs.added, "|", Style::GitSignAdded)
                    }
                    UiGitSignKind::Changed => {
                        (&config.editor.git_signs.changed, "|", Style::GitSignChanged)
                    }
                    UiGitSignKind::Deleted => {
                        (&config.editor.git_signs.deleted, "_", Style::GitSignDeleted)
                    }
                };
                grid.set_cell(
                    gutter_width - 1,
                    screen_row,
                    one_cell_git_symbol(symbol, fallback, config.editor.ambiguous_width),
                    style,
                );
            }
        }
        let line_start = buffer
            .text
            .line_to_byte(line_index)
            .unwrap_or(ByteOffset(0));
        let line = buffer.text.line(line_index).unwrap_or_default();
        let full_content = line.strip_suffix('\n').unwrap_or(&line);
        let can_wrap = wrap_enabled;
        let skipped_bytes = if can_wrap {
            line_rows[first_line_row].byte_start
        } else {
            byte_offset_at_display_column(
                full_content,
                view.scroll_column,
                config.editor.tab_width,
                config.editor.ambiguous_width,
            )
        };
        let skipped_column = if can_wrap {
            line_rows[first_line_row].source_column_start
        } else {
            display_column(
                &full_content[..skipped_bytes],
                config.editor.tab_width,
                config.editor.ambiguous_width,
            )
        };
        let visible_content = &full_content[skipped_bytes..];
        let mut x = gutter_width;
        let mut source_column = skipped_column;
        let mut visual_row_index = first_line_row;
        for (relative, grapheme) in visible_content.grapheme_indices(true) {
            let source_relative = skipped_bytes.saturating_add(relative);
            if can_wrap
                && line_rows
                    .get(visual_row_index.saturating_add(1))
                    .is_some_and(|row| source_relative >= row.byte_start)
            {
                screen_row = screen_row.saturating_add(1);
                if screen_row >= body_height {
                    break;
                }
                visual_row_index = visual_row_index.saturating_add(1);
                x = gutter_width;
            }
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
            let visible_cells = if can_wrap {
                cells.min(content_width)
            } else {
                cells
            };
            let advance = u16::try_from(visible_cells).unwrap_or(u16::MAX);
            if !can_wrap && x.saturating_add(advance) > width {
                break;
            }
            source_column = source_column.saturating_add(cells);
            if grapheme == "\t" {
                for _ in 0..visible_cells {
                    if x >= width {
                        break;
                    }
                    grid.set_cell(x, screen_row, " ", style);
                    x = x.saturating_add(1);
                }
            } else if x < width {
                if can_wrap && cells > content_width {
                    let replacement = if text_width(
                        &config.editor.control_character_replacement,
                        config.editor.ambiguous_width,
                    ) == 1
                    {
                        config.editor.control_character_replacement.as_str()
                    } else {
                        "?"
                    };
                    grid.set_cell(x, screen_row, replacement, style);
                } else {
                    grid.set_cell(x, screen_row, grapheme, style);
                }
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
            let (hint_anchor, absolute_hint_cell) = visual_rows.cursor_projection(hint.position);
            let hint_cell = if can_wrap {
                absolute_hint_cell
            } else {
                absolute_hint_cell.saturating_sub(skipped_column)
            };
            if let Some(hint_row) =
                visual_rows.screen_row(start_anchor, hint_anchor, usize::from(body_height))
            {
                let hint_x =
                    gutter_width.saturating_add(u16::try_from(hint_cell).unwrap_or(u16::MAX));
                let hint_y = u16::try_from(hint_row).unwrap_or(u16::MAX);
                if hint_x < width && hint_y < body_height {
                    grid.put_text(hint_x, hint_y, &hint.label, Style::Muted);
                }
            }
        }
        if let Some(ghost) = ui.ai_ghost_text.as_ref().filter(|ghost| {
            ghost.buffer == view.buffer
                && ghost.revision == buffer.text.revision()
                && ghost.view == view_id
                && ghost.cursor == view.cursor
                && ghost.mode == view.mode
                && ghost.cursor >= visible_start
                && ghost.cursor <= line_end
        }) {
            let (ghost_anchor, absolute_ghost_cell) = visual_rows.cursor_projection(ghost.cursor);
            let ghost_cell = if can_wrap {
                absolute_ghost_cell
            } else {
                absolute_ghost_cell.saturating_sub(skipped_column)
            };
            if let Some(ghost_row) =
                visual_rows.screen_row(start_anchor, ghost_anchor, usize::from(body_height))
            {
                let ghost_row = u16::try_from(ghost_row).unwrap_or(u16::MAX);
                let ghost_x =
                    gutter_width.saturating_add(u16::try_from(ghost_cell).unwrap_or(u16::MAX));
                if ghost_row < body_height && ghost_x < width {
                    grid.put_text_clipped(
                        ghost_x,
                        ghost_row,
                        &ghost.text,
                        Style::AiGhost,
                        width - ghost_x,
                    );
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
        wrap_enabled,
        &visual_rows,
        start_anchor,
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
    wrap_enabled: bool,
    visual_rows: &VisualRowModel<'_>,
    start_anchor: VisualRowAnchor,
) -> Option<(u16, u16)> {
    let view = state.views.get(view_id)?;
    let buffer = state.buffers.get(view.buffer)?;
    let point = PositionCodec::new(&buffer.text)
        .byte_to_point(view.cursor)
        .ok()?;
    let (cursor_anchor, cursor_cell) = visual_rows.cursor_projection(view.cursor);
    if wrap_enabled {
        let row = visual_rows.screen_row(start_anchor, cursor_anchor, usize::from(body_height))?;
        let y = u16::try_from(row).ok()?;
        let x = gutter.saturating_add(u16::try_from(cursor_cell).ok()?);
        (x < width && y < body_height).then_some((x, y))
    } else {
        let column = cursor_cell;
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
        let y = u16::try_from(visual_rows.screen_row(
            start_anchor,
            cursor_anchor,
            usize::from(body_height),
        )?)
        .ok()?;
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

fn render_plugin_surface(frame: &mut Frame, surface: &PluginSurfaceUiState, rect: WindowRect) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let right = rect.x.saturating_add(rect.width).min(frame.grid.width());
    let bottom = rect.y.saturating_add(rect.height).min(frame.grid.height());
    for y in rect.y..bottom {
        for x in rect.x..right {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
    }
    for (offset, row) in surface.rows.iter().skip(surface.scroll).enumerate() {
        let Ok(offset) = u16::try_from(offset) else {
            break;
        };
        let y = rect.y.saturating_add(offset);
        if y >= bottom {
            break;
        }
        let indent = row
            .depth
            .saturating_mul(2)
            .min(rect.width.saturating_sub(1));
        let x = rect.x.saturating_add(indent);
        let prefix = if row.actionable { "› " } else { "" };
        let mut text = String::with_capacity(prefix.len().saturating_add(row.text.len()));
        text.push_str(prefix);
        text.push_str(&row.text);
        frame.grid.put_text_clipped(
            x,
            y,
            &text,
            if row.focused {
                Style::Selection
            } else {
                row.style
            },
            right,
        );
    }
}

fn render_private_surface(frame: &mut Frame, surface: &PrivateSurfaceUiState, rect: WindowRect) {
    if rect.width == 0
        || rect.height == 0
        || surface.width != rect.width
        || surface.height != rect.height
        || surface.dimensions_revision == 0
        || surface.surface_revision == 0
    {
        return;
    }
    let right = rect.x.saturating_add(rect.width).min(frame.grid.width());
    let bottom = rect.y.saturating_add(rect.height).min(frame.grid.height());
    for y in rect.y..bottom {
        for x in rect.x..right {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
    }
    for cell in &surface.cells {
        if cell.continuation || cell.x >= rect.width || cell.y >= rect.height {
            continue;
        }
        let x = rect.x.saturating_add(cell.x);
        let y = rect.y.saturating_add(cell.y);
        if x < right && y < bottom {
            frame
                .grid
                .put_text_clipped(x, y, &cell.grapheme, cell.style, right);
        }
    }
    if surface.focused {
        frame.cursor = surface
            .cursor
            .filter(|(x, y)| *x < rect.width && *y < rect.height)
            .map(|(x, y)| {
                (
                    rect.x.saturating_add(x),
                    rect.y.saturating_add(y),
                    SetCursorStyle::SteadyBlock,
                )
            });
    }
}

#[must_use]
pub fn picker_geometry(config: &Config, bounds: WindowRect) -> Option<WindowRect> {
    let configured = &config.ui.picker;
    let width = u16::try_from(u32::from(bounds.width) * u32::from(configured.width_percent) / 100)
        .unwrap_or(bounds.width)
        .max(configured.minimum_width.min(bounds.width))
        .min(bounds.width);
    let height =
        u16::try_from(u32::from(bounds.height) * u32::from(configured.height_percent) / 100)
            .unwrap_or(bounds.height)
            .max(configured.minimum_height.min(bounds.height))
            .min(bounds.height);
    if width < 4 || height < 6 {
        return None;
    }
    Some(WindowRect {
        x: bounds
            .x
            .saturating_add(bounds.width.saturating_sub(width) / 2),
        y: bounds
            .y
            .saturating_add(bounds.height.saturating_sub(height) / 2),
        width,
        height,
    })
}

fn render_picker(frame: &mut Frame, picker: &PickerUiState, config: &Config, rect: WindowRect) {
    let configured = &config.ui.picker;
    let WindowRect {
        x: left,
        y: top,
        width,
        height,
    } = rect;
    if width < 4 || height < 6 {
        return;
    }
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
    let right = geometry.left.saturating_add(geometry.width);
    for y in geometry.top..geometry.top.saturating_add(geometry.height) {
        for x in geometry.left..right {
            frame.grid.set_cell(x, y, " ", Style::Default);
        }
    }
    if geometry.height < 3 {
        let marker_end = frame.grid.put_text_clipped(
            geometry.left,
            geometry.top,
            prompt.kind.marker(&config.ui.command_line.text),
            Style::Accent,
            right,
        );
        frame.grid.put_text_clipped(
            marker_end,
            geometry.top,
            &prompt.input,
            Style::Default,
            right,
        );
        let prefix = prompt.input.get(..prompt.cursor).unwrap_or(&prompt.input);
        frame.cursor = Some((
            marker_end
                .saturating_add(
                    u16::try_from(text_width(prefix, config.editor.ambiguous_width)).unwrap_or(0),
                )
                .min(right.saturating_sub(1)),
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
    let content_right = right.saturating_sub(1);
    let marker_end = frame.grid.put_text_clipped(
        input_x,
        input_y,
        prompt.kind.marker(&config.ui.command_line.text),
        Style::Accent,
        content_right,
    );
    frame.grid.put_text_clipped(
        marker_end,
        input_y,
        &prompt.input,
        Style::Default,
        content_right,
    );
    if let Some(error) = &prompt.error {
        let error_y = geometry
            .top
            .saturating_add(3)
            .saturating_add(config.ui.command_line.attachment_gap);
        for x in geometry.left..right {
            frame.grid.set_cell(x, error_y, " ", Style::Default);
        }
        frame.grid.put_text_clipped(
            geometry
                .left
                .saturating_add(config.ui.command_line.input_padding),
            error_y,
            error,
            Style::Error,
            right,
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
            frame
                .grid
                .put_text_clipped(label_x, y, &item.label, style, right);
            frame
                .grid
                .put_text_clipped(detail_x, y, &item.detail, Style::Muted, right);
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
        CompletionItem, DocumentationMarkupKind, DocumentationPopupKind, DocumentationPopupState,
        DocumentationSection, EditorWindowBinding, EditorWindowLayoutState, PromptKind,
        PromptState, UiState, prompt_geometry,
    };

    fn editor_layout(state: &EditorState) -> EditorWindowLayoutState {
        fn convert(
            node: &crate::editing::LayoutNode,
            state: &EditorState,
            next: &mut u64,
            bindings: &mut Vec<EditorWindowBinding>,
        ) -> crate::windows::TiledLayoutNode {
            match node {
                crate::editing::LayoutNode::Pane(pane) => {
                    *next += 1;
                    let window = crate::windows::WindowId(*next);
                    bindings.push(EditorWindowBinding {
                        window,
                        pane: *pane,
                        view: state.panes[*pane].view,
                    });
                    crate::windows::TiledLayoutNode::Window(window)
                }
                crate::editing::LayoutNode::Split {
                    axis,
                    ratio,
                    first,
                    second,
                } => crate::windows::TiledLayoutNode::Split {
                    axis: match axis {
                        crate::editing::Axis::Horizontal => crate::windows::TileAxis::Horizontal,
                        crate::editing::Axis::Vertical => crate::windows::TileAxis::Vertical,
                    },
                    ratio: *ratio,
                    first: Box::new(convert(first, state, next, bindings)),
                    second: Box::new(convert(second, state, next, bindings)),
                },
            }
        }

        let tab = &state.tabs[state.active_tab.expect("active tab")];
        let mut bindings = Vec::new();
        let root = convert(&tab.root, state, &mut 0, &mut bindings);
        let focused = bindings
            .iter()
            .find(|binding| binding.pane == tab.focused)
            .map(|binding| binding.window);
        EditorWindowLayoutState {
            source_revision: state.window_revision().expect("test revision"),
            layout: crate::windows::TiledLayoutSnapshot { root, focused },
            bindings,
        }
    }

    fn ui_with_editor_layout(state: &EditorState) -> UiState {
        UiState {
            editor_window_layout: Some(editor_layout(state)),
            ..UiState::default()
        }
    }

    fn attach_test_compositor(
        state: &EditorState,
        ui: &mut UiState,
        config: &crate::configuration::Config,
        width: u16,
        height: u16,
    ) {
        let layout = ui.editor_window_layout.as_ref();
        let mut windows = layout
            .into_iter()
            .flat_map(|layout| &layout.bindings)
            .map(|binding| crate::windows::CompositorWindowSnapshot {
                window: binding.window,
                placement: crate::windows::WindowPlacement::Tiled,
                adapter: crate::windows::ContentAdapterKind::TextBuffer,
                focusable: true,
            })
            .collect::<Vec<_>>();
        let explorer_window = ui
            .explorer
            .as_ref()
            .map(|_| crate::windows::WindowId(10_001));
        if let Some(window) = explorer_window {
            let size = super::explorer_panel_width(config, width)
                .map_or(1, |panel| panel.saturating_add(1));
            windows.push(crate::windows::CompositorWindowSnapshot {
                window,
                placement: crate::windows::WindowPlacement::Docked {
                    edge: crate::windows::DockEdge::Left,
                    size,
                },
                adapter: crate::windows::ContentAdapterKind::DeclarativeUi,
                focusable: true,
            });
        }
        let terminal_window = ui
            .terminal_panel
            .as_ref()
            .map(|terminal| (terminal.id, crate::windows::WindowId(10_002)));
        if let Some((_, window)) = terminal_window {
            let size = super::terminal_panel_geometry(
                config,
                height,
                ui.terminal_panel
                    .as_ref()
                    .expect("terminal")
                    .requested_height,
            )
            .map_or(1, |geometry| geometry.height);
            windows.push(crate::windows::CompositorWindowSnapshot {
                window,
                placement: crate::windows::WindowPlacement::Docked {
                    edge: crate::windows::DockEdge::Bottom,
                    size,
                },
                adapter: crate::windows::ContentAdapterKind::TerminalSession,
                focusable: true,
            });
        }
        let dashboard_window = ui.dashboard.then_some(crate::windows::WindowId(10_003));
        if let Some(window) = dashboard_window {
            windows.push(crate::windows::CompositorWindowSnapshot {
                window,
                placement: crate::windows::WindowPlacement::Overlay(crate::windows::WindowRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                }),
                adapter: crate::windows::ContentAdapterKind::DeclarativeUi,
                focusable: true,
            });
        }
        let picker_window = ui
            .picker
            .as_ref()
            .and_then(|_| {
                super::picker_geometry(
                    config,
                    crate::windows::WindowRect {
                        x: 0,
                        y: 0,
                        width,
                        height,
                    },
                )
            })
            .map(|rect| (crate::windows::WindowId(10_004), rect));
        if let Some((window, rect)) = picker_window {
            windows.push(crate::windows::CompositorWindowSnapshot {
                window,
                placement: crate::windows::WindowPlacement::Overlay(rect),
                adapter: crate::windows::ContentAdapterKind::DeclarativeUi,
                focusable: true,
            });
        }
        let focused = if let Some((window, _)) = picker_window {
            Some(window)
        } else if ui
            .terminal_panel
            .as_ref()
            .is_some_and(|terminal| terminal.focused)
        {
            terminal_window.map(|(_, window)| window)
        } else if ui.explorer_focused {
            explorer_window
        } else {
            layout.and_then(|layout| layout.layout.focused)
        };
        let snapshot = crate::windows::CompositorSnapshot {
            tab: crate::windows::WorkspaceTabId(1),
            tiled: layout.map(|layout| layout.layout.clone()),
            windows,
            focused,
        };
        let bounds = crate::windows::WindowRect {
            x: 0,
            y: 0,
            width,
            height,
        };
        let preliminary = snapshot.project(
            bounds,
            config.ui.panes.minimum_width,
            config.ui.panes.minimum_height,
        );
        let (top, bottom) =
            super::editor_workspace_insets(state, ui, config, preliminary.workspace_bounds.height);
        ui.window_graph_revision = Some(1);
        ui.compositor_layout = Some(super::CompositorLayoutState {
            source_revision: state.window_revision().expect("test revision"),
            graph_revision: 1,
            projection: snapshot.project_with_tiled_insets(
                bounds,
                config.ui.panes.minimum_width,
                config.ui.panes.minimum_height,
                top,
                bottom,
            ),
            surfaces: super::NativeSurfaceWindowBinding {
                explorer: explorer_window,
                terminal: terminal_window,
                dashboard: dashboard_window,
                picker: picker_window.map(|(window, _)| window),
            },
        });
    }

    fn render(
        state: &EditorState,
        ui: &UiState,
        config: &crate::configuration::Config,
        dashboard: &crate::configuration::DashboardConfig,
        width: u16,
        height: u16,
    ) -> super::Frame {
        let mut ui = ui.clone();
        attach_test_compositor(state, &mut ui, config, width, height);
        super::render(state, &ui, config, dashboard, width, height)
    }

    #[test]
    fn workspace_frame_respects_nonzero_compositor_origin_for_side_and_top_docks() {
        let config = crate::configuration::Config::default();
        let mut local = crate::terminal_ui::CellGrid::new(6, 3);
        local.set_cell(0, 0, "X", crate::terminal_ui::Style::Default);
        local.set_cell(5, 2, "Y", crate::terminal_ui::Style::Default);
        let frame = super::place_workspace_frame(
            super::Frame {
                grid: local,
                cursor: Some((1, 1, crossterm::cursor::SetCursorStyle::SteadyBlock)),
            },
            crate::windows::WindowRect {
                x: 7,
                y: 4,
                width: 6,
                height: 3,
            },
            &config,
            20,
            10,
        );
        assert_eq!(frame.grid.get(7, 4).expect("offset first").symbol, "X");
        assert_eq!(frame.grid.get(12, 6).expect("offset last").symbol, "Y");
        assert_eq!(frame.grid.get(0, 0).expect("outside").symbol, " ");
        assert!(frame.cursor.is_some_and(|(x, y, _)| (x, y) == (8, 5)));

        let terminal = super::TerminalPanelState {
            id: 1,
            profile: "side".into(),
            tabs: vec![super::TerminalTabState {
                id: 1,
                profile: "side".into(),
                running: true,
                exit_code: None,
            }],
            mode: super::TerminalPanelMode::Input,
            running: true,
            exit_code: None,
            truncated: false,
            unsupported_sequences: 0,
            focused: false,
            columns: 5,
            rows: 8,
            cells: Vec::new(),
            cells_start: 0,
            cursor: None,
            selection: None,
            scrollback_lines: 0,
            scroll_offset: 0,
            requested_height: 5,
        };
        let composed = super::dock_terminal_panel(
            frame,
            &terminal,
            super::TerminalPanelGeometry {
                x: 0,
                width: 5,
                top: 0,
                height: 10,
                body_top: 1,
                body_height: 8,
                status_y: 9,
            },
            &config,
            20,
            10,
        );
        assert_eq!(
            composed.grid.get(0, 0).expect("side terminal").style,
            crate::terminal_ui::Style::TerminalBorder
        );
        assert_eq!(
            composed.grid.get(7, 4).expect("workspace retained").symbol,
            "X"
        );
    }

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
    fn prompt_clears_and_clips_the_editor_layer_beneath_its_box() {
        let mut state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
        state.open_text(
            crate::document::TextBuffer::from_text(&vec!["X".repeat(100); 10].join("\n")),
            None,
        );
        let loaded = crate::configuration::load_bundled_config();
        let base_ui = UiState {
            editor_window_layout: Some(editor_layout(&state)),
            ..UiState::default()
        };
        let geometry = prompt_geometry(100, 20, &loaded.config.ui.command_line, false);
        let probe_x = geometry.left + geometry.width - 2;
        let probe_y = geometry.top + 1;
        let base = render(&state, &base_ui, &loaded.config, &loaded.dashboard, 100, 20);
        assert_eq!(
            base.grid.get(probe_x, probe_y).expect("editor cell").symbol,
            "X"
        );

        let ui = UiState {
            prompt: Some(PromptState::new(PromptKind::Command)),
            ..base_ui.clone()
        };
        let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 20);
        assert_eq!(
            frame
                .grid
                .get(probe_x, probe_y)
                .expect("cleared cell")
                .symbol,
            " "
        );
        assert_eq!(
            frame
                .grid
                .get(geometry.left + geometry.width - 1, probe_y)
                .expect("right border")
                .style,
            crate::terminal_ui::Style::Border
        );

        let mut long_prompt = PromptState::new(PromptKind::Command);
        long_prompt.input = "x".repeat(200);
        long_prompt.cursor = long_prompt.input.len();
        let clipped = render(
            &state,
            &UiState {
                prompt: Some(long_prompt),
                ..base_ui
            },
            &loaded.config,
            &loaded.dashboard,
            100,
            20,
        );
        let outside_x = geometry.left + geometry.width;
        assert_eq!(
            clipped.grid.get(outside_x, probe_y),
            base.grid.get(outside_x, probe_y)
        );
    }

    #[test]
    fn documentation_popup_preserves_lines_clamps_and_protects_status_row() {
        let mut state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
        state.open_text(
            crate::document::TextBuffer::from_text("cursor\nline\nline\nline\nline\nline"),
            None,
        );
        let ui = UiState {
            editor_window_layout: Some(editor_layout(&state)),
            documentation_popup: Some(DocumentationPopupState {
                kind: DocumentationPopupKind::Hover,
                title: "Hover".into(),
                provider: "test".into(),
                source_range: None,
                sections: vec![DocumentationSection {
                    heading: None,
                    markup: DocumentationMarkupKind::Markdown,
                    lines: vec!["first line".into(), "wide 界界 clipped safely".into()],
                }],
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
        let ui = ui_with_editor_layout(&state);
        let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 20, 6);
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
    fn missing_application_window_snapshot_fails_closed() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state.open_text(crate::document::TextBuffer::from_text("not rendered"), None);
        let loaded = crate::configuration::load_bundled_config();
        let ui = UiState::default();
        assert!(super::pane_viewports(&state, &ui, &loaded.config, 40, 8).is_empty());
        let frame = super::render(&state, &ui, &loaded.config, &loaded.dashboard, 40, 8);
        assert!(frame.cursor.is_none());
        assert!(!(0..8).any(|row| frame.grid.row_text(row).contains("not rendered")));

        let stale = ui_with_editor_layout(&state);
        state
            .split_active(crate::editing::Axis::Horizontal, 500)
            .expect("change source layout");
        assert!(super::pane_viewports(&state, &stale, &loaded.config, 40, 8).is_empty());
        let frame = super::render(&state, &stale, &loaded.config, &loaded.dashboard, 40, 8);
        assert!(frame.cursor.is_none());
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
        let ui = ui_with_editor_layout(&state);
        let frame = render(&state, &ui, &config, &loaded.dashboard, 20, 8);
        assert!(frame.grid.row_text(0).contains("[scratch]"));
        assert_eq!(frame.grid.get(9, 2).expect("split border").symbol, "│");
        assert!(frame.cursor.is_some_and(|(x, y, _)| x > 9 && y > 0));
    }

    #[test]
    fn buffer_line_lists_unique_dirty_content_and_follows_active_overflow() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        let (first, _) = state.open_text(
            crate::document::TextBuffer::from_text("first"),
            Some(PathBuf::from("/workspace/src/main.rs")),
        );
        state.open_text(
            crate::document::TextBuffer::from_text("second"),
            Some(PathBuf::from("/workspace/tests/main.rs")),
        );
        state.open_text(
            crate::document::TextBuffer::from_text("third"),
            Some(PathBuf::from("/workspace/very-long-active-buffer-name.rs")),
        );
        state.buffers[first].dirty = true;
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.ui.tab_line.enabled = false;
        config.ui.buffer_line.maximum_name_width = 18;
        let entries = vec![
            super::ContentTabEntry {
                identity: super::ContentTabIdentity(crate::windows::ContentId(1)),
                kind: super::ContentTabKind::Text,
                title: "src/main.rs ·1".into(),
                active: false,
                dirty: true,
                pinned: false,
                diagnostic: None,
                icon: "R".into(),
                closeable: true,
            },
            super::ContentTabEntry {
                identity: super::ContentTabIdentity(crate::windows::ContentId(2)),
                kind: super::ContentTabKind::Text,
                title: "tests/main.rs ·2".into(),
                active: false,
                dirty: false,
                pinned: false,
                diagnostic: None,
                icon: "R".into(),
                closeable: true,
            },
            super::ContentTabEntry {
                identity: super::ContentTabIdentity(crate::windows::ContentId(3)),
                kind: super::ContentTabKind::Text,
                title: "very-long-active-buffer-name.rs".into(),
                active: true,
                dirty: false,
                pinned: false,
                diagnostic: None,
                icon: "R".into(),
                closeable: true,
            },
        ];
        let ui = UiState {
            editor_window_layout: Some(editor_layout(&state)),
            content_tabs: entries,
            ..UiState::default()
        };

        let frame = render(&state, &ui, &config, &loaded.dashboard, 24, 7);
        let line = frame.grid.row_text(0);
        assert!(line.contains("buffer-name.rs"), "{line:?}");
        assert!(line.contains('‹'));
        assert!(frame.cursor.is_some_and(|(_, y, _)| y >= 1));

        for width in 1..=5 {
            let narrow = render(&state, &ui, &config, &loaded.dashboard, width, 5);
            assert_eq!(narrow.grid.width(), width);
            assert!(narrow.cursor.is_none_or(|(x, y, _)| x < width && y < 5));
        }
    }

    #[test]
    fn content_line_segment_width_and_hit_testing_share_exact_boundaries() {
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.ui.buffer_line.active_format = "{title}{dirty}".into();
        config.ui.buffer_line.inactive_format = "{title}{dirty}".into();
        let entry = super::ContentTabEntry {
            identity: super::ContentTabIdentity(crate::windows::ContentId(9)),
            kind: super::ContentTabKind::Text,
            title: "a.rs".into(),
            active: true,
            dirty: true,
            pinned: true,
            diagnostic: Some(super::ContentTabDiagnostic {
                severity: super::ContentTabDiagnosticSeverity::Error,
                count: 2,
            }),
            icon: "R".into(),
            closeable: true,
        };
        let width = super::content_tab_width(&entry, &config);
        let mut grid = crate::terminal_ui::CellGrid::new(u16::try_from(width).unwrap(), 1);
        super::render_content_tab_line(&mut grid, std::slice::from_ref(&entry), &config, 0);
        let styles = (0..grid.width())
            .filter_map(|x| grid.get(x, 0).map(|cell| cell.style))
            .collect::<Vec<_>>();
        for role in [
            crate::terminal_ui::Style::BufferLinePin,
            crate::terminal_ui::Style::BufferLineIcon,
            crate::terminal_ui::Style::BufferLineDirty,
            crate::terminal_ui::Style::BufferLineDiagnostic,
            crate::terminal_ui::Style::BufferLineClose,
        ] {
            assert!(styles.contains(&role));
        }
        for column in 0..u16::try_from(width).unwrap() {
            assert_eq!(
                super::content_tab_at_column(
                    std::slice::from_ref(&entry),
                    &config,
                    u16::try_from(width).unwrap(),
                    column
                ),
                Some(entry.identity)
            );
        }
        assert_eq!(
            super::content_tab_at_column(
                std::slice::from_ref(&entry),
                &config,
                u16::try_from(width).unwrap(),
                u16::try_from(width).unwrap()
            ),
            None
        );
        let mut entries = vec![entry.clone(), entry.clone(), entry.clone()];
        for (index, entry) in entries.iter_mut().enumerate() {
            entry.identity = super::ContentTabIdentity(crate::windows::ContentId(index as u64 + 1));
            entry.title = char::from(b'a' + index as u8).to_string();
            entry.active = index == 2;
        }
        let left = super::text_width(
            &config.ui.buffer_line.overflow_left,
            config.editor.ambiguous_width,
        );
        for column in 0..u16::try_from(left).unwrap() {
            assert_eq!(
                super::content_tab_at_column(&entries, &config, 8, column),
                None
            )
        }
        let mut right_entries = entries.clone();
        for (index, entry) in right_entries.iter_mut().enumerate() {
            entry.active = index == 0;
            entry.pinned = false;
            entry.diagnostic = None;
            entry.dirty = false;
            entry.closeable = false;
        }
        config.ui.buffer_line.show_icons = false;
        config.ui.buffer_line.show_pins = false;
        config.ui.buffer_line.show_diagnostics = false;
        config.ui.buffer_line.show_close = false;
        let first = super::content_tab_width(&right_entries[0], &config);
        assert_eq!(
            super::content_tab_at_column(&right_entries, &config, 5, u16::try_from(first).unwrap()),
            None
        );
    }

    #[test]
    fn buffer_line_begins_after_the_explorer_dock() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state.open_text(
            crate::document::TextBuffer::from_text("one"),
            Some(PathBuf::from("/workspace/one.rs")),
        );
        state.open_text(
            crate::document::TextBuffer::from_text("two"),
            Some(PathBuf::from("/workspace/two.rs")),
        );
        let ui = UiState {
            editor_window_layout: Some(editor_layout(&state)),
            content_tabs: vec![
                super::ContentTabEntry {
                    identity: super::ContentTabIdentity(crate::windows::ContentId(1)),
                    kind: super::ContentTabKind::Text,
                    title: "one.rs".into(),
                    active: false,
                    dirty: false,
                    pinned: false,
                    diagnostic: None,
                    icon: "R".into(),
                    closeable: true,
                },
                super::ContentTabEntry {
                    identity: super::ContentTabIdentity(crate::windows::ContentId(2)),
                    kind: super::ContentTabKind::Text,
                    title: "two.rs".into(),
                    active: true,
                    dirty: false,
                    pinned: false,
                    diagnostic: None,
                    icon: "R".into(),
                    closeable: true,
                },
            ],
            explorer: Some(super::ExplorerUiState {
                title: "Explorer".into(),
                rows: Vec::new(),
                scroll: 0,
                loading: false,
                total: 0,
                preview_lines: Vec::new(),
            }),
            ..UiState::default()
        };
        let loaded = crate::configuration::load_bundled_config();
        let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 10);
        let panel = super::explorer_panel_width(&loaded.config, 80).expect("explorer width");
        assert!(frame.grid.row_text(0).contains("Explorer"));
        let editor_line = (panel.saturating_add(1)..80)
            .map(|x| frame.grid.get(x, 0).map_or("", |cell| cell.symbol.as_str()))
            .collect::<String>();
        assert!(editor_line.contains("one.rs"));
        assert!(editor_line.contains("two.rs"));
    }

    #[test]
    fn exact_ai_ghost_text_is_cursor_clipped_and_non_mutating() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (buffer, view) = state.open_text(crate::document::TextBuffer::from_text("abc"), None);
        state.views[view].cursor = crate::document::ByteOffset(3);
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.editor.line_numbers = crate::configuration::LineNumbers::Off;
        config.ui.status_line.enabled = false;
        let mut ui = ui_with_editor_layout(&state);
        ui.ai_ghost_text = Some(super::UiGhostText {
            workspace_root_revision: 1,
            buffer,
            revision: state.buffers[buffer].text.revision(),
            view,
            cursor: crate::document::ByteOffset(3),
            mode: crate::editing::EditorMode::Normal,
            text: " suggestion beyond width".into(),
        });
        let before = state.buffers[buffer].text.to_string();
        let frame = render(&state, &ui, &config, &loaded.dashboard, 10, 2);
        assert!(frame.grid.row_text(0).starts_with("abc sug"));
        assert_eq!(state.buffers[buffer].text.to_string(), before);
        ui.ai_ghost_text.as_mut().unwrap().revision = crate::document::BufferRevision(99);
        let frame = render(&state, &ui, &config, &loaded.dashboard, 10, 2);
        assert!(!frame.grid.row_text(0).contains("sugges"));
    }

    #[test]
    fn visual_row_model_is_reversible_fold_aware_and_unicode_width_exact() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (_, view) = state.open_text(
            crate::document::TextBuffer::from_text(
                "a\t界e\u{301}zz\nfold\nhidden one\nhidden two\ntail tail",
            ),
            None,
        );
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.editor.wrap = true;
        config.editor.line_numbers = crate::configuration::LineNumbers::Off;
        let ui = UiState {
            lsp_folds: vec![super::UiFold {
                view,
                start_line: 1,
                end_line: 3,
                collapsed: true,
            }],
            ..ui_with_editor_layout(&state)
        };
        let buffer = state.views[view].buffer;
        let model =
            super::VisualRowModel::new(&state.buffers[buffer].text, view, &ui, &config, 4, true);
        assert_eq!(model.rows_for_line(0), 3);
        assert_eq!(
            model.normalize(super::VisualRowAnchor {
                line: 3,
                wrap_row: 8
            }),
            super::VisualRowAnchor {
                line: 1,
                wrap_row: 0
            }
        );
        let start = super::VisualRowAnchor::default();
        let sequence = (0..7)
            .scan(start, |anchor, _| {
                let current = *anchor;
                *anchor = model.next(*anchor);
                Some(current)
            })
            .collect::<Vec<_>>();
        assert!(!sequence.iter().any(|anchor| matches!(anchor.line, 2 | 3)));
        for pair in sequence.windows(2) {
            assert_eq!(model.previous(pair[1]), pair[0]);
        }
        assert_eq!(model.backward(model.forward(start, 5), 5), start);
    }

    #[test]
    fn grapheme_atomic_rows_match_render_cursor_and_tiny_pane_policy() {
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.editor.wrap = true;
        config.editor.line_numbers = crate::configuration::LineNumbers::Off;
        config.ui.status_line.enabled = false;

        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (buffer, view) =
            state.open_text(crate::document::TextBuffer::from_text("aa界界"), None);
        let ui = ui_with_editor_layout(&state);
        let model =
            super::VisualRowModel::new(&state.buffers[buffer].text, view, &ui, &config, 3, true);
        assert_eq!(model.rows_for_line(0), 3);
        assert_eq!(
            model.line_rows(0),
            vec![
                super::VisualRowSlice {
                    byte_start: 0,
                    byte_end: 2,
                    source_column_start: 0,
                    used_cells: 2,
                },
                super::VisualRowSlice {
                    byte_start: 2,
                    byte_end: 5,
                    source_column_start: 2,
                    used_cells: 2,
                },
                super::VisualRowSlice {
                    byte_start: 5,
                    byte_end: 8,
                    source_column_start: 4,
                    used_cells: 2,
                },
            ]
        );
        let frame = render(&state, &ui, &config, &loaded.dashboard, 3, 3);
        assert_eq!(frame.grid.get(0, 0).unwrap().symbol, "a");
        assert_eq!(frame.grid.get(1, 0).unwrap().symbol, "a");
        assert_eq!(frame.grid.get(0, 1).unwrap().symbol, "界");
        assert!(frame.grid.get(1, 1).unwrap().continuation);
        assert_eq!(frame.grid.get(0, 2).unwrap().symbol, "界");
        assert!(frame.grid.get(1, 2).unwrap().continuation);

        state.views[view].scroll_wrap_row = 1;
        state.views[view].cursor = crate::document::ByteOffset(2);
        let ui = ui_with_editor_layout(&state);
        let scrolled = render(&state, &ui, &config, &loaded.dashboard, 3, 1);
        assert_eq!(scrolled.grid.get(0, 0).unwrap().symbol, "界");
        assert_eq!(scrolled.cursor.map(|(x, y, _)| (x, y)), Some((0, 0)));
        state.views[view].scroll_wrap_row = 0;

        for &(cursor, expected) in &[
            (0, (0, 0)),
            (1, (1, 0)),
            (2, (0, 1)),
            (5, (0, 2)),
            (8, (2, 2)),
        ] {
            state.views[view].cursor = crate::document::ByteOffset(cursor);
            let ui = ui_with_editor_layout(&state);
            let frame = render(&state, &ui, &config, &loaded.dashboard, 3, 3);
            assert_eq!(
                frame.cursor.map(|(x, y, _)| (x, y)),
                Some(expected),
                "cursor byte {cursor}"
            );
        }

        state.buffers[buffer].text = crate::document::TextBuffer::from_text("abc");
        state.views[view].cursor = crate::document::ByteOffset(3);
        let ui = ui_with_editor_layout(&state);
        let exact = render(&state, &ui, &config, &loaded.dashboard, 3, 1);
        assert_eq!(exact.cursor.map(|(x, y, _)| (x, y)), Some((2, 0)));

        state.buffers[buffer].text = crate::document::TextBuffer::from_text("界a");
        state.views[view].cursor = crate::document::ByteOffset(0);
        let ui = ui_with_editor_layout(&state);
        let tiny_model =
            super::VisualRowModel::new(&state.buffers[buffer].text, view, &ui, &config, 1, true);
        assert_eq!(tiny_model.rows_for_line(0), 2);
        let tiny = render(&state, &ui, &config, &loaded.dashboard, 1, 2);
        assert_eq!(
            tiny.grid.get(0, 0).unwrap().symbol,
            config.editor.control_character_replacement
        );
        assert_eq!(tiny.grid.get(0, 1).unwrap().symbol, "a");
    }

    #[test]
    fn mixed_grapheme_row_sequences_are_partitioned_reversible_and_bounded() {
        let loaded = crate::configuration::load_bundled_config();
        let mut config = loaded.config;
        config.editor.wrap = true;
        config.editor.line_numbers = crate::configuration::LineNumbers::Off;
        let samples = [
            "aa界界",
            "aa\t\t",
            "e\u{301}界\tx",
            "界界界",
            "abc",
            "\t界e\u{301}z",
        ];
        for sample in samples {
            for width in 1..=4 {
                let mut state = EditorState::new(PathBuf::from("/tmp"));
                let (buffer, view) =
                    state.open_text(crate::document::TextBuffer::from_text(sample), None);
                let ui = ui_with_editor_layout(&state);
                let model = super::VisualRowModel::new(
                    &state.buffers[buffer].text,
                    view,
                    &ui,
                    &config,
                    width,
                    true,
                );
                let rows = model.line_rows(0);
                assert!(!rows.is_empty());
                assert_eq!(rows[0].byte_start, 0);
                assert_eq!(rows.last().unwrap().byte_end, sample.len());
                for (index, row) in rows.iter().enumerate() {
                    assert!(row.used_cells <= usize::from(width));
                    assert!(sample.is_char_boundary(row.byte_start));
                    assert!(sample.is_char_boundary(row.byte_end));
                    if let Some(next) = rows.get(index + 1) {
                        assert_eq!(row.byte_end, next.byte_start);
                        let current = super::VisualRowAnchor {
                            line: 0,
                            wrap_row: index,
                        };
                        let following = model.next(current);
                        assert_eq!(following.wrap_row, index + 1);
                        assert_eq!(model.previous(following), current);
                    }
                    let anchor = super::VisualRowAnchor {
                        line: 0,
                        wrap_row: index,
                    };
                    let bytes = (0..usize::from(width))
                        .map(|cell| model.byte_at_cell(anchor, cell))
                        .collect::<Vec<_>>();
                    assert!(bytes.windows(2).all(|pair| pair[0] <= pair[1]));
                    assert!(bytes.iter().all(|byte| {
                        *byte >= row.byte_start
                            && *byte <= row.byte_end
                            && sample.is_char_boundary(*byte)
                    }));
                }
                let last = super::VisualRowAnchor {
                    line: 0,
                    wrap_row: rows.len() - 1,
                };
                assert_eq!(
                    model.backward(model.forward(super::VisualRowAnchor::default(), 999), 999),
                    super::VisualRowAnchor::default()
                );
                assert_eq!(model.forward(last, 1), last);
            }
        }
    }

    #[test]
    fn overlapping_collapsed_folds_normalize_to_one_reversible_interval() {
        let mut state = EditorState::new(PathBuf::from("/tmp"));
        let (buffer, view) = state.open_text(
            crate::document::TextBuffer::from_text("zero\none\ntwo\nthree\nfour\nfive"),
            None,
        );
        let loaded = crate::configuration::load_bundled_config();
        let ui = UiState {
            lsp_folds: vec![super::UiFold {
                view,
                start_line: 1,
                end_line: 3,
                collapsed: true,
            }],
            syntax_folds: vec![super::UiFold {
                view,
                start_line: 2,
                end_line: 4,
                collapsed: true,
            }],
            ..ui_with_editor_layout(&state)
        };
        let model = super::VisualRowModel::new(
            &state.buffers[buffer].text,
            view,
            &ui,
            &loaded.config,
            20,
            true,
        );
        let fold_start = super::VisualRowAnchor {
            line: 1,
            wrap_row: 0,
        };
        let after = model.next(fold_start);
        assert_eq!(after.line, 5);
        assert_eq!(model.previous(after), fold_start);
        for hidden in 2..=4 {
            assert_eq!(
                model.normalize(super::VisualRowAnchor {
                    line: hidden,
                    wrap_row: 99,
                }),
                fold_start
            );
        }
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
        let ui = ui_with_editor_layout(&state);
        let frame = render(&state, &ui, &config, &loaded.dashboard, 5, 3);
        assert!(frame.grid.row_text(0).starts_with("abcde"));
        assert!(frame.grid.row_text(1).starts_with("fghij"));

        let view = state.active_view.expect("view");
        let buffer = state.views[view].buffer;
        state.buffers[buffer].text =
            crate::document::TextBuffer::from_text("abcdefghijklmnopqrstuvwxyz");
        state.views[view].cursor = crate::document::ByteOffset(25);
        let ui = ui_with_editor_layout(&state);
        let frame = render(&state, &ui, &config, &loaded.dashboard, 5, 3);
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
            editor_window_layout: Some(editor_layout(&state)),
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
        let ui = ui_with_editor_layout(&state);
        let frame = render(&state, &ui, &config, &loaded.dashboard, 8, 2);
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
    fn immutable_git_signs_use_the_reserved_gutter_and_exact_buffer_revision() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        let (buffer, _) = state.open_text(
            crate::document::TextBuffer::from_text("added\nchanged\ndeleted\n"),
            Some(PathBuf::from("/workspace/file.rs")),
        );
        let loaded = crate::configuration::load_bundled_config();
        let mut ui = ui_with_editor_layout(&state);
        let revision = state.buffers[buffer].text.revision().0;
        ui.git_signs = vec![
            super::UiGitSign {
                buffer,
                buffer_revision: revision,
                root_revision: 1,
                status_revision: 1,
                repository_id: crate::workspace::git::GitRepositoryId(1),
                line: 0,
                kind: super::UiGitSignKind::Added,
            },
            super::UiGitSign {
                buffer,
                buffer_revision: revision,
                root_revision: 1,
                status_revision: 1,
                repository_id: crate::workspace::git::GitRepositoryId(1),
                line: 1,
                kind: super::UiGitSignKind::Changed,
            },
            super::UiGitSign {
                buffer,
                buffer_revision: revision,
                root_revision: 1,
                status_revision: 1,
                repository_id: crate::workspace::git::GitRepositoryId(1),
                line: 2,
                kind: super::UiGitSignKind::Deleted,
            },
        ];
        let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 20, 5);
        let gutter =
            super::editor_gutter_width(&state.buffers[buffer].text, &loaded.config.editor, 20);
        assert_eq!(
            (
                frame.grid.get(gutter - 1, 0).unwrap().symbol.as_str(),
                frame.grid.get(gutter - 1, 0).unwrap().style,
            ),
            ("│", crate::terminal_ui::Style::GitSignAdded)
        );
        assert_eq!(
            (
                frame.grid.get(gutter - 1, 1).unwrap().symbol.as_str(),
                frame.grid.get(gutter - 1, 1).unwrap().style,
            ),
            ("│", crate::terminal_ui::Style::GitSignChanged)
        );
        assert_eq!(
            (
                frame.grid.get(gutter - 1, 2).unwrap().symbol.as_str(),
                frame.grid.get(gutter - 1, 2).unwrap().style,
            ),
            ("_", crate::terminal_ui::Style::GitSignDeleted)
        );
        assert_eq!(
            (
                frame.grid.get(gutter - 1, 3).unwrap().symbol.as_str(),
                frame.grid.get(gutter - 1, 3).unwrap().style,
            ),
            ("│", crate::terminal_ui::Style::Muted)
        );

        let mut wide_config = loaded.config.clone();
        wide_config.editor.ambiguous_width = crate::configuration::AmbiguousWidth::Wide;
        let wide = render(&state, &ui, &wide_config, &loaded.dashboard, 20, 5);
        assert_eq!(wide.grid.get(gutter - 1, 0).unwrap().symbol, "|");
        assert!(!wide.grid.get(gutter - 1, 0).unwrap().continuation);
        assert_eq!(wide.grid.get(gutter - 1, 3).unwrap().symbol, "|");

        ui.git_signs[0].buffer_revision = revision.saturating_add(1);
        let stale = render(&state, &ui, &loaded.config, &loaded.dashboard, 20, 5);
        assert_ne!(
            stale.grid.get(gutter - 1, 0).unwrap().style,
            crate::terminal_ui::Style::GitSignAdded
        );
        let tiny = render(&state, &ui, &loaded.config, &loaded.dashboard, 1, 3);
        assert_eq!(tiny.grid.width(), 1);
    }

    #[test]
    fn line_number_gutter_uses_the_largest_open_buffer_across_tabs() {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        let (short, short_view) = state.open_text(
            crate::document::TextBuffer::from_text("short\n"),
            Some(PathBuf::from("/workspace/short.rs")),
        );
        let first_tab = state.active_tab.unwrap();
        state.new_tab().unwrap();
        let long_text = (0..1_000).map(|_| "long\n").collect::<String>();
        let (long, long_view) = state.open_text(
            crate::document::TextBuffer::from_text(&long_text),
            Some(PathBuf::from("/workspace/long.rs")),
        );
        let loaded = crate::configuration::load_bundled_config();
        assert_eq!(super::editor_line_number_digits(&state), 4);
        let short_gutter = super::editor_state_gutter_width(
            &state,
            &state.buffers[short].text,
            &loaded.config.editor,
            80,
        );
        let long_gutter = super::editor_state_gutter_width(
            &state,
            &state.buffers[long].text,
            &loaded.config.editor,
            80,
        );
        assert_eq!(short_gutter, long_gutter);
        assert_eq!(short_gutter, 6);
        state.focus_tab(-1, true).unwrap();
        assert_eq!(state.active_tab, Some(first_tab));
        let ui = ui_with_editor_layout(&state);
        let frame = super::render_editor_view(&state, short_view, &ui, &loaded.config, 80, 4);
        assert_eq!(frame.grid.get(3, 0).unwrap().symbol, "1");
        assert_eq!(frame.grid.get(short_gutter - 1, 0).unwrap().symbol, "│");
        assert_eq!(
            frame.grid.get(short_gutter - 1, 0).unwrap().style,
            crate::terminal_ui::Style::Muted
        );
        assert_eq!(frame.grid.get(short_gutter, 0).unwrap().symbol, "s");
        let long_frame = super::render_editor_view(&state, long_view, &ui, &loaded.config, 80, 4);
        assert_eq!(long_frame.grid.get(long_gutter - 1, 0).unwrap().symbol, "│");
        assert_eq!(long_frame.grid.get(long_gutter, 0).unwrap().symbol, "l");
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
        let ui = ui_with_editor_layout(&state);
        let zero = render(&state, &ui, &config, &loaded.dashboard, 0, 2);
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
        let mut split_ui = ui_with_editor_layout(&split_state);
        attach_test_compositor(&split_state, &mut split_ui, &config, 6, 2);
        let zero_leaf = super::active_pane_viewport(&split_state, &split_ui, &config, 6, 2)
            .expect("focused tiny-layout fallback");
        assert_eq!(zero_leaf.width, 6);
        let split_frame = render(&split_state, &split_ui, &config, &loaded.dashboard, 6, 2);
        assert!(split_frame.cursor.is_some());

        for (width, expected_gutter) in [(1, 0), (2, 1)] {
            assert_eq!(
                super::editor_gutter_width(&state.buffers[buffer].text, &config.editor, width,),
                expected_gutter
            );
            assert_eq!(
                super::editor_content_width(&state.buffers[buffer].text, &config.editor, width,),
                1
            );
            let ui = ui_with_editor_layout(&state);
            let frame = render(&state, &ui, &config, &loaded.dashboard, width, 2);
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
        let ui = ui_with_editor_layout(&state);
        let frame = render(&state, &ui, &config, &loaded.dashboard, 5, 3);
        assert!(frame.grid.row_text(0).starts_with("abcde"));
        assert!(frame.grid.row_text(1).starts_with("fghij"));
        assert!(frame.cursor.is_some_and(|(x, y, _)| x == 0 && y == 0));
    }

    #[test]
    fn retained_plugin_surface_rows_are_strictly_clipped_to_the_projected_window() {
        let mut frame = super::Frame {
            grid: crate::terminal_ui::CellGrid::new(12, 5),
            cursor: None,
        };
        let surface = super::PluginSurfaceUiState {
            handle: 9,
            window: crate::windows::WindowId(99),
            tree_revision: 1,
            scroll: 0,
            rows: vec![
                super::PluginSurfaceRow {
                    node: 1,
                    depth: 0,
                    text: "abcdefgh".into(),
                    style: crate::terminal_ui::Style::Accent,
                    actionable: false,
                    focused: false,
                },
                super::PluginSurfaceRow {
                    node: 2,
                    depth: 1,
                    text: "button".into(),
                    style: crate::terminal_ui::Style::Selection,
                    actionable: true,
                    focused: true,
                },
            ],
        };
        super::render_plugin_surface(
            &mut frame,
            &surface,
            crate::windows::WindowRect {
                x: 3,
                y: 1,
                width: 4,
                height: 2,
            },
        );
        assert_eq!(&frame.grid.row_text(1)[3..7], "abcd");
        assert_eq!(frame.grid.get(7, 1).expect("outside right").symbol, " ");
        assert_eq!(frame.grid.get(2, 2).expect("outside left").symbol, " ");
        assert_eq!(frame.grid.get(3, 3).expect("outside bottom").symbol, " ");
    }

    #[test]
    fn generic_picker_is_centered_filtered_presentation_with_configured_markers() {
        let state = EditorState::new(PathBuf::from("/tmp"));
        let ui = UiState {
            dashboard: true,
            picker: Some(super::PickerUiState {
                surface: super::PickerSurfaceKind::Generic,
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
        let mut ui = ui_with_editor_layout(&state);
        attach_test_compositor(&state, &mut ui, &loaded.config, 79, 24);
        let viewport = super::active_pane_viewport(&state, &ui, &loaded.config, 79, 24)
            .expect("focused viewport");
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
            editor_window_layout: Some(editor_layout(&state)),
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
    fn private_surface_cells_and_cursor_are_local_and_strictly_clipped() {
        let mut frame = super::Frame {
            grid: crate::terminal_ui::CellGrid::new(10, 4),
            cursor: None,
        };
        let surface = super::PrivateSurfaceUiState {
            handle: 3,
            window: crate::windows::WindowId(7),
            dimensions_revision: 2,
            surface_revision: 1,
            width: 4,
            height: 2,
            cells: vec![
                super::PrivateSurfaceCell {
                    x: 0,
                    y: 0,
                    grapheme: "界".into(),
                    continuation: false,
                    style: crate::terminal_ui::Style::Accent,
                },
                super::PrivateSurfaceCell {
                    x: 1,
                    y: 0,
                    grapheme: String::new(),
                    continuation: true,
                    style: crate::terminal_ui::Style::Accent,
                },
                super::PrivateSurfaceCell {
                    x: 3,
                    y: 1,
                    grapheme: "e\u{301}".into(),
                    continuation: false,
                    style: crate::terminal_ui::Style::Selection,
                },
            ],
            cursor: Some((3, 1)),
            focused: true,
        };
        super::render_private_surface(
            &mut frame,
            &surface,
            crate::windows::WindowRect {
                x: 2,
                y: 1,
                width: 4,
                height: 2,
            },
        );
        assert_eq!(frame.grid.get(2, 1).expect("wide lead").symbol, "界");
        assert!(
            frame
                .grid
                .get(3, 1)
                .expect("wide continuation")
                .continuation
        );
        assert_eq!(frame.grid.get(6, 2).expect("outside right").symbol, " ");
        assert!(frame.cursor.is_some_and(|(x, y, _)| (x, y) == (5, 2)));

        let mut stale = surface.clone();
        stale.width = 5;
        let before = (frame.grid.row_text(1), frame.grid.row_text(2), frame.cursor);
        super::render_private_surface(
            &mut frame,
            &stale,
            crate::windows::WindowRect {
                x: 2,
                y: 1,
                width: 4,
                height: 2,
            },
        );
        assert_eq!(
            (frame.grid.row_text(1), frame.grid.row_text(2), frame.cursor),
            before,
            "stale dimensions draw nothing"
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
            editor_window_layout: Some(editor_layout(&state)),
            diagnostics: vec![super::UiDiagnostic {
                path,
                severity: crate::workspace::DiagnosticSeverity::Error,
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
            tabs: vec![
                super::TerminalTabState {
                    id: 7,
                    profile: "shell".into(),
                    running: true,
                    exit_code: None,
                },
                super::TerminalTabState {
                    id: 8,
                    profile: "logs".into(),
                    running: false,
                    exit_code: Some(2),
                },
            ],
            mode: super::TerminalPanelMode::Input,
            running: true,
            exit_code: None,
            truncated: false,
            unsupported_sequences: screen.unsupported_sequences(),
            focused: true,
            columns: 80,
            rows: 4,
            cells: screen.history_window(0, 4),
            cells_start: 0,
            cursor: Some((cursor_column, cursor_row)),
            selection: None,
            scrollback_lines: screen.scrollback_len(),
            scroll_offset: 0,
            requested_height: 6,
        };
        assert_eq!(
            super::terminal_tab_at_column(&terminal, &config, 80, 2),
            Some(7)
        );
        assert_eq!(
            super::terminal_tab_at_column(&terminal, &config, 80, 13),
            Some(8)
        );
        let mut ui = ui_with_editor_layout(&state);
        ui.explorer = Some(super::ExplorerUiState {
            title: "Explorer".into(),
            rows: Vec::new(),
            scroll: 0,
            loading: false,
            total: 0,
            preview_lines: Vec::new(),
        });
        ui.terminal_panel = Some(terminal.clone());
        let geometry = super::terminal_panel_geometry(&config, 20, 6).expect("terminal geometry");
        assert_eq!(geometry.top, 14);
        assert_eq!(geometry.body_height, 4);
        let panel_width = super::explorer_panel_width(&config, 80).expect("explorer width");
        let editor_width = 80_u16.saturating_sub(panel_width).saturating_sub(1);
        attach_test_compositor(&state, &mut ui, &config, 80, 20);
        let viewport =
            super::active_pane_viewport(&state, &ui, &config, editor_width, geometry.top)
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
                foreground: Some(crate::terminal_ui::PtyColor::Indexed(1)),
                bold: true,
                ..crate::terminal_ui::PtyStyle::default()
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
        assert!(
            frame
                .grid
                .row_text(geometry.status_y)
                .contains("TERMINAL  7: shell")
        );
        assert!(
            !frame
                .grid
                .row_text(geometry.status_y)
                .contains("unsupported")
        );

        let mut editor_ui = ui;
        editor_ui.terminal_panel.as_mut().expect("terminal").focused = false;
        let editor_frame = render(&state, &editor_ui, &config, &loaded.dashboard, 80, 20);
        assert!(
            editor_frame
                .cursor
                .is_some_and(|(_, y, _)| y < geometry.top)
        );
        let tab_row = editor_frame.grid.row_text(geometry.top);
        assert!(tab_row.contains("7: shell"));
        assert!(tab_row.contains("8: logs [exit 2]"));
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
