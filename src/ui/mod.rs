#![forbid(unsafe_code)]

pub mod geometry;
mod diff;
mod grid;
pub mod markdown;
mod pty_screen;
mod render;
mod terminal;
pub mod window;

pub use grid::{Cell, CellGrid, Style};
pub use pty_screen::{
    PtyCell, PtyColor, PtyInputKey, PtyMouseEvent, PtyMouseMode, PtyScreen, PtyStyle,
    TerminalMouseEncoding, encode_focus, encode_key, encode_paste, encode_sgr_mouse,
    encode_terminal_mouse_event,
};
pub use render::{
    OperationBarState,
    FlashOverlay,
    CompletionItem, CompositorLayoutState, ConfirmationChoice, ConfirmationState,
    ContentTabDiagnostic, ContentTabDiagnosticSeverity, ContentTabEntry, ContentTabIdentity,
    ContentTabKind, CellAlign, DashboardAction, DiffHunk, DiffLayout, DiffLine, DiffLineKind,
    DiffPreview,
    DiffSideCell, DiffSideRow,
    DiffSpan, DocumentationMarkupKind, DocumentationPopupKind,
    DocumentationPopupState, DocumentationSection, EditorWindowBinding, EditorWindowLayoutState,
    ExpandedDiagnosticState, ExplorerSegmentRole, ExplorerUiRow, ExplorerUiSegment,
    ExplorerUiState, ExplorerWindowUiState, Frame, KeyHintUiRow, KeyHintUiState,
    LanguageCompletionState, NativeSurfaceWindowBinding, PaneViewport, PickerCell, PickerPreview,
    PickerPreviewBody, PickerRow, PickerRowKind, PickerSurfaceKind, PickerTextPreview,
    PickerUiState,
    PluginSurfaceRow, PluginSurfaceUiState, PrivateSurfaceCell,
    PrivateSurfaceUiState, PromptGeometry, PromptKind, PromptState, TerminalPanelGeometry,
    TerminalPanelMode, TerminalPanelState, TerminalPhase, TerminalTabState, ToolMenuRow,
    ToolMenuState, ToolRuntimeOption, ToolRuntimePickerState, UiDiagnostic, UiFold, UiGitSign,
    UiGitSignKind, UiInlayHint, UiLspProgress, UiState, UiSyntaxSpan, VisualRowAnchor,
    VisualRowModel, view_pane_viewport, active_editor_pane_viewport, active_pane_viewport, confirmation_fully_visible,
    content_tab_at_column, editor_content_width, editor_gutter_width, editor_pane_viewport_at,
    editor_state_content_width, editor_state_gutter_width, editor_workspace_insets,
    explorer_panel_width, pane_viewports, picker_geometry, prompt_geometry,
    prompt_geometry_for_state, prompt_surface_dimensions, render, snap_display_column,
    snap_display_column_forward, terminal_panel_geometry, terminal_tab_at_column};
pub use render::{DebugPane, DebugPanesUiState};
pub use diff::{emphasise_changed_spans, parse_unified_patch};
pub use terminal::{
    KeyInputKind, TerminalError, TerminalOptions, TerminalSession, UiEvent,
    install_panic_restore_hook, normalize_event,
};

pub(crate) use render::terminal_geometry_from_rect;
