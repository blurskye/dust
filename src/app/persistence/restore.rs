//! Interpretation of saved editor records for restoration.

use crate::editor::{RestoredLayout, RestoredPane, RestoredTab};
fn layout(record: &crate::persistence::PersistedLayout) -> RestoredLayout {
    match record {
        crate::persistence::PersistedLayout::Pane { pane } => RestoredLayout::Pane {
            pane: RestoredPane {
                path: pane.path.clone(),
                cursor: pane.cursor,
                scroll_line: pane.scroll_line,
                scroll_column: pane.scroll_column,
            },
        },
        crate::persistence::PersistedLayout::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let axis = match axis.as_str() {
                "horizontal" => crate::windows::TileAxis::Horizontal,
                "vertical" => crate::windows::TileAxis::Vertical,
                _ => return RestoredLayout::Invalid,
            };
            RestoredLayout::Split {
                axis,
                ratio: *ratio,
                first: Box::new(layout(first)),
                second: Box::new(layout(second)),
            }
        }
    }
}
pub(in crate::app) fn restore_layout(
    draft: &mut crate::editor::EditorDraft,
    session: &crate::persistence::PersistedSession,
    loaded: &std::collections::HashMap<
        std::path::PathBuf,
        (std::path::PathBuf, crate::editor::text::TextBuffer),
    >,
) -> Result<(), String> {
    let tabs = session
        .tabs
        .iter()
        .map(|tab| RestoredTab {
            layout: layout(&tab.layout),
            focused_pane: tab.focused_pane,
            focused_path: tab.focused_path.clone(),
        })
        .collect::<Vec<_>>();
    draft.restore_tabs(&tabs, session.active_tab, loaded)
}
#[cfg(test)]
pub(in crate::app) fn build_restored_layout(
    state: &mut crate::editor::EditorState,
    record: &crate::persistence::PersistedLayout,
    loaded: &std::collections::HashMap<
        std::path::PathBuf,
        (std::path::PathBuf, crate::editor::text::TextBuffer),
    >,
) -> Option<(
    crate::windows::TiledLayoutNode,
    Vec<(std::path::PathBuf, crate::windows::WindowId)>,
)> {
    crate::editor::build_layout_for_test(state, &layout(record), loaded)
}
