//! Theme styles and the current inline completion used by decoration drawing.

use crate::config::Highlight;
use crate::editor::{EditorMode, EditorState, ViewId};
use crate::ui::Style;
use crate::ui::render::{UiInlayHint, UiState};

pub(super) fn style(name: &str) -> Style {
    Highlight::named(name).map_or(Style::Muted, Style::Syntax)
}

pub(super) fn ghost<'a>(
    state: &EditorState,
    view_id: ViewId,
    ui: &'a UiState,
) -> Option<&'a UiInlayHint> {
    let view = state.views.get(view_id)?;
    let buffer = &state.buffers[view.buffer];
    let (owner, ghost) = ui.inline_completion.as_ref()?;
    (*owner == view_id
        && view.mode == EditorMode::Insert
        && ui.language_completion.is_some()
        && ghost.buffer == view.buffer
        && ghost.revision == buffer.text.revision()
        && ghost.position == view.cursor)
        .then_some(ghost)
}
