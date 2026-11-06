//! Canonical restoration of editor stores through a consuming draft.
pub(crate) struct RestoredPane {
    pub(crate) path: PathBuf,
    pub(crate) cursor: usize,
    pub(crate) scroll_line: usize,
    pub(crate) scroll_column: usize,
}
pub(crate) enum RestoredLayout {
    Pane {
        pane: RestoredPane,
    },
    Split {
        axis: crate::windows::TileAxis,
        ratio: u16,
        first: Box<Self>,
        second: Box<Self>,
    },
    Invalid,
}
pub(crate) struct RestoredTab {
    pub(crate) layout: RestoredLayout,
    pub(crate) focused_pane: Option<usize>,
    pub(crate) focused_path: Option<PathBuf>,
}
use crate::editor::EditorState;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use crate::editor::transaction::EditorDraft;

use std::collections::HashMap;
use std::path::PathBuf;
impl EditorDraft {
    pub(crate) fn restore_tabs(
        &mut self,
        tabs: &[RestoredTab],
        active_tab: usize,
        loaded: &HashMap<PathBuf, (PathBuf, TextBuffer)>,
    ) -> Result<(), String> {
        if self.state.buffers.values().any(|buffer| buffer.dirty) {
            return Err("session restore would discard unsaved buffers".into());
        }
        self.state.buffers.clear();
        self.state.views.clear();
        self.state
            .graph
            .clear()
            .map_err(|error| error.to_string())?;
        self.state.buffer_contents.clear();
        self.state.graph_views.clear();
        let mut built = Vec::new();
        for tab in tabs {
            let id = self
                .state
                .graph
                .create_workspace_tab("builtin.editor", 1)
                .map_err(|error| error.to_string())?;
            let (layout, panes) = build_layout(&mut self.state, id, &tab.layout, loaded)
                .ok_or("session layout is incomplete")?;
            let focused = tab
                .focused_pane
                .and_then(|index| panes.get(index).map(|(_, pane)| *pane))
                .or_else(|| {
                    tab.focused_path.as_ref().and_then(|path| {
                        panes
                            .iter()
                            .find(|(candidate, _)| candidate == path)
                            .map(|(_, pane)| *pane)
                    })
                })
                .or_else(|| panes.first().map(|(_, pane)| *pane))
                .ok_or("session tab is empty")?;
            self.state
                .graph
                .replace_layout(id, Some(layout))
                .map_err(|error| error.to_string())?;
            self.state
                .graph
                .select_window(focused)
                .map_err(|error| error.to_string())?;
            built.push(id);
        }
        let tabs = built;
        if tabs.is_empty() {
            return Err("session has no restorable tabs".into());
        }
        let active = active_tab.min(tabs.len() - 1);
        self.state
            .activate_tab(tabs[active])
            .map_err(str::to_owned)?;
        self.state.mark_window_changed();
        self.state.validate_layout().map_err(str::to_owned)
    }
}
fn build_layout(
    state: &mut EditorState,
    tab: crate::windows::WorkspaceTabId,
    layout: &RestoredLayout,
    loaded: &HashMap<PathBuf, (PathBuf, TextBuffer)>,
) -> Option<(
    crate::windows::TiledLayoutNode,
    Vec<(PathBuf, crate::windows::WindowId)>,
)> {
    match layout {
        RestoredLayout::Invalid => None,
        RestoredLayout::Pane { pane } => {
            let (canonical, text) = loaded.get(&pane.path)?;
            let buffer = state
                .load_hidden_text(text.clone(), canonical.clone())
                .ok()?;
            let mut view = crate::editor::View::new(buffer);
            let length = state.buffers[buffer].text.len_bytes();
            let mut offset = pane.cursor.min(length);
            while !state.buffers[buffer]
                .text
                .is_char_boundary(ByteOffset(offset))
            {
                offset -= 1;
            }
            view.cursor = ByteOffset(offset);
            view.scroll_line = pane
                .scroll_line
                .min(state.buffers[buffer].text.len_lines().saturating_sub(1));
            view.scroll_wrap_row = 0;
            view.scroll_column = pane.scroll_column;
            let view = state.views.insert(view);
            let graph_view = state.register_view(view).ok()?;
            let pane_id = state
                .graph
                .open_window(
                    tab,
                    graph_view,
                    crate::windows::WindowPlacement::Tiled,
                    true,
                    "builtin.editor",
                    1,
                )
                .ok()?;
            Some((
                crate::windows::TiledLayoutNode::Window(pane_id),
                vec![(pane.path.clone(), pane_id)],
            ))
        }
        RestoredLayout::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            if !(100..=900).contains(ratio) {
                return None;
            }
            let (first, mut panes) = build_layout(state, tab, first, loaded)?;
            let (second, second_panes) = build_layout(state, tab, second, loaded)?;
            panes.extend(second_panes);
            let axis = match axis {
                crate::windows::TileAxis::Horizontal => crate::windows::TileAxis::Horizontal,
                crate::windows::TileAxis::Vertical => crate::windows::TileAxis::Vertical,
            };
            Some((
                crate::windows::TiledLayoutNode::Split {
                    axis,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                },
                panes,
            ))
        }
    }
}

#[cfg(test)]
pub(crate) fn build_layout_for_test(
    state: &mut EditorState,
    layout: &RestoredLayout,
    loaded: &HashMap<PathBuf, (PathBuf, TextBuffer)>,
) -> Option<(
    crate::windows::TiledLayoutNode,
    Vec<(PathBuf, crate::windows::WindowId)>,
)> {
    let tab = state.graph.create_workspace_tab("builtin.editor", 1).ok()?;
    build_layout(state, tab, layout, loaded)
}
