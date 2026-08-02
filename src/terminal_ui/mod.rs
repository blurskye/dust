#![forbid(unsafe_code)]

mod grid;
mod pty_screen;
mod render;
mod terminal;

pub use grid::{Cell, CellGrid, Style};
pub use pty_screen::{
    PtyCell, PtyColor, PtyInputKey, PtyMouseEvent, PtyMouseMode, PtyScreen, PtyStyle, encode_focus,
    encode_key, encode_paste, encode_sgr_mouse, encode_terminal_mouse_event,
};
pub use render::{
    CompletionItem, CompositorLayoutState, ContentTabDiagnostic, ContentTabDiagnosticSeverity,
    ContentTabEntry, ContentTabIdentity, ContentTabKind, DashboardAction, DocumentationMarkupKind,
    DocumentationPopupKind, DocumentationPopupState, DocumentationSection, EditorWindowBinding,
    EditorWindowLayoutState, ExplorerSegmentRole, ExplorerUiRow, ExplorerUiSegment,
    ExplorerUiState, Frame, KeyHintUiRow, KeyHintUiState, LanguageCompletionState,
    NativeSurfaceWindowBinding, PaneViewport, PickerRow, PickerSurfaceKind, PickerUiState,
    PluginSurfaceRow, PluginSurfaceUiState, PrivateSurfaceCell, PrivateSurfaceUiState,
    PromptGeometry, PromptKind, PromptState, TerminalPanelGeometry, TerminalPanelState,
    ToolMenuRow, ToolMenuState, UiDiagnostic, UiFold, UiGhostText, UiGitSign, UiGitSignKind,
    UiInlayHint, UiState, UiSyntaxSpan, VisualRowAnchor, VisualRowModel, active_pane_viewport,
    content_tab_at_column, editor_content_width, editor_gutter_width, editor_workspace_insets,
    explorer_panel_width, pane_viewports, picker_geometry, prompt_geometry, render,
    snap_display_column, snap_display_column_forward, terminal_panel_geometry,
};
pub use terminal::{
    KeyInputKind, TerminalError, TerminalOptions, TerminalSession, UiEvent,
    install_panic_restore_hook, normalize_event,
};
