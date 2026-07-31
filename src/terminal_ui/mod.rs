#![forbid(unsafe_code)]

mod grid;
mod pty_screen;
mod render;
mod terminal;

pub use grid::{Cell, CellGrid, Style};
pub use pty_screen::{PtyCell, PtyScreen, PtyStyle};
pub use render::{
    CompletionItem, DashboardAction, DocumentationPopupKind, DocumentationPopupState,
    ExplorerSegmentRole, ExplorerUiRow, ExplorerUiSegment, ExplorerUiState, Frame, KeyHintUiRow,
    KeyHintUiState, LanguageCompletionState, PaneViewport, PickerRow, PickerUiState,
    PromptGeometry, PromptKind, PromptState, TerminalPanelGeometry, TerminalPanelState,
    ToolMenuRow, ToolMenuState, UiDiagnostic, UiFold, UiInlayHint, UiState, UiSyntaxSpan,
    active_pane_viewport, editor_content_width, editor_gutter_width, explorer_panel_width,
    pane_viewports, prompt_geometry, render, snap_display_column, snap_display_column_forward,
    terminal_panel_geometry,
};
pub use terminal::{
    KeyInputKind, TerminalError, TerminalOptions, TerminalSession, UiEvent,
    install_panic_restore_hook, normalize_event,
};
