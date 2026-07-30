#![forbid(unsafe_code)]

mod grid;
mod pty_screen;
mod render;
mod terminal;

pub use grid::{Cell, CellGrid, Style};
pub use pty_screen::{PtyCell, PtyScreen, PtyStyle};
pub use render::{
    CompletionItem, DashboardAction, Frame, LanguageCompletionState, PickerRow, PickerUiState,
    PromptGeometry, PromptKind, PromptState, ToolMenuRow, ToolMenuState, UiDiagnostic, UiFold,
    UiInlayHint, UiState, UiSyntaxSpan, explorer_panel_width, prompt_geometry, render,
};
pub use terminal::{
    KeyInputKind, TerminalError, TerminalOptions, TerminalSession, UiEvent,
    install_panic_restore_hook, normalize_event,
};
