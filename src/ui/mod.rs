#![forbid(unsafe_code)]

mod diff;
pub mod geometry;
mod grid;
pub mod markdown;
mod pty_screen;
pub(crate) mod render;
mod terminal;
pub mod window;

pub use diff::{emphasise_changed_spans, parse_unified_patch};
pub use grid::{Cell, CellGrid, Style};
pub use pty_screen::{
    PtyCell, PtyColor, PtyInputKey, PtyMouseEvent, PtyMouseMode, PtyScreen, PtyStyle,
    TerminalMouseEncoding, encode_focus, encode_key, encode_paste, encode_sgr_mouse,
    encode_terminal_mouse_event,
};
pub use render::{
    AgentListRow, AgentPanelUi, DebugPane, DebugPanesUiState, DebugSign, DebugSignKind,
    SettingsEdit, SettingsHit, SettingsLine, SettingsPageState, SettingsRow, SettingsValue,
    TutorCard, debug_pane_insets, debug_pane_tab_at, settings_hit, settings_layout,
    settings_line_height, transcript_lines,
};
pub use render::{
    CellAlign, CompletionItem, CompositorLayoutState, ConfirmationChoice, ConfirmationState,
    ContentTabDiagnostic, ContentTabDiagnosticSeverity, ContentTabEntry, ContentTabIdentity,
    ContentTabKind, DashboardAction, DiffHunk, DiffLayout, DiffLine, DiffLineKind, DiffPreview,
    DiffSideCell, DiffSideRow, DiffSpan, DocumentationMarkupKind, DocumentationPopupKind,
    DocumentationPopupState, DocumentationSection, EditorWindowBinding, EditorWindowLayoutState,
    ExpandedDiagnosticState, ExplorerSegmentRole, ExplorerUiRow, ExplorerUiSegment,
    ExplorerUiState, ExplorerWindowUiState, FlashOverlay, Frame, KeyHintUiRow, KeyHintUiState,
    LanguageCompletionState, NativeSurfaceWindowBinding, OperationBarState, PaneViewport,
    PickerCell, PickerPreview, PickerPreviewBody, PickerRow, PickerRowKind, PickerSurfaceKind,
    PickerTextPreview, PickerUiState, PluginSurfaceRow, PluginSurfaceUiState, PrivateSurfaceCell,
    PrivateSurfaceUiState, PromptGeometry, PromptKind, PromptState, TerminalPanelGeometry,
    TerminalPanelMode, TerminalPanelState, TerminalPhase, TerminalTabState, ToolMenuRow,
    ToolMenuState, ToolRuntimeOption, ToolRuntimePickerState, UiBreadcrumb, UiColorSpan,
    UiDiagnostic, UiFold, UiGitSign, UiGitSignKind, UiInlayHint, UiLspProgress, UiMarkdownMark,
    UiState, UiSyntaxSpan, VisualRowAnchor, VisualRowModel, active_editor_pane_viewport,
    active_pane_viewport, confirmation_fully_visible, content_tab_at_column, editor_content_width,
    editor_gutter_width, editor_pane_viewport_at, editor_state_content_width,
    editor_state_gutter_width, explorer_panel_width, pane_viewports, picker_geometry,
    prompt_geometry, prompt_geometry_for_state, prompt_surface_dimensions, render,
    screen_foot_rows, snap_display_column, snap_display_column_forward, terminal_panel_geometry,
    terminal_tab_at_column, view_pane_viewport, workspace_top_rows,
};
pub use terminal::{
    KeyInputKind, TerminalError, TerminalOptions, TerminalSession, UiEvent,
    install_panic_restore_hook, normalize_event,
};

pub(crate) use render::{explorer_segment_style, terminal_window_geometry, workspace_title};
