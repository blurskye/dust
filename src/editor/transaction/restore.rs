//! Canonical restoration of editor stores through a consuming draft.
use super::EditorDraft;
use crate::{
    editor::{
        EditorState,
        text::{ByteOffset, TextBuffer},
    },
    persistence::{PersistedLayout, PersistedSession},
};
use std::{collections::HashMap, path::PathBuf};
impl EditorDraft {
    pub(crate) fn restore_layout(
        &mut self,
        session: &PersistedSession,
        loaded: &HashMap<PathBuf, (PathBuf, TextBuffer)>,
    ) -> Result<(), String> {
        if self.state.buffers.values().any(|buffer| buffer.dirty) {
            return Err("session restore would discard unsaved buffers".into());
        }
        self.state.buffers.clear();
        self.state.views.clear();
        self.state.panes.clear();
        self.state.tabs.clear();
        self.state.active_tab = None;
        self.state.active_view = None;
        let mut built = Vec::new();
        for tab in &session.tabs {
            let (layout, panes) = build_layout(&mut self.state, &tab.layout, loaded)
                .ok_or("session layout is incomplete")?;
            let focused = tab
                .focused_path
                .as_ref()
                .and_then(|path| {
                    panes
                        .iter()
                        .find(|(candidate, _)| candidate == path)
                        .map(|(_, pane)| *pane)
                })
                .or_else(|| panes.first().map(|(_, pane)| *pane))
                .ok_or("session tab is empty")?;
            built.push(crate::editor::Tab {
                root: layout,
                focused,
            });
        }
        // Cleared slots are reused last-freed first, so take the keys in iteration order
        // before filling them; the tab bar and saved tab indices both follow that order.
        for tab in &built {
            self.state.tabs.insert(tab.clone());
        }
        let keys = self.state.tabs.keys().collect::<Vec<_>>();
        let mut tabs = Vec::new();
        for (id, tab) in keys.into_iter().zip(built) {
            tabs.push((id, tab.focused));
            self.state.tabs[id] = tab;
        }
        if tabs.is_empty() {
            return Err("session has no restorable tabs".into());
        }
        let active = session.active_tab.min(tabs.len() - 1);
        self.state.active_tab = Some(tabs[active].0);
        self.state.active_view = self.state.panes.get(tabs[active].1).map(|pane| pane.view);
        self.state.mark_window_changed();
        self.state.validate_layout().map_err(str::to_owned)
    }
}
fn build_layout(
    state: &mut EditorState,
    layout: &PersistedLayout,
    loaded: &HashMap<PathBuf, (PathBuf, TextBuffer)>,
) -> Option<(
    crate::editor::LayoutNode,
    Vec<(PathBuf, crate::editor::PaneId)>,
)> {
    match layout {
        PersistedLayout::Pane { pane } => {
            let (canonical, text) = loaded.get(&pane.path)?;
            let buffer = state.load_hidden_text(text.clone(), canonical.clone());
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
            let pane_id = state.panes.insert(crate::editor::Pane { view });
            Some((
                crate::editor::LayoutNode::Pane(pane_id),
                vec![(pane.path.clone(), pane_id)],
            ))
        }
        PersistedLayout::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            if !(100..=900).contains(ratio) {
                return None;
            }
            let (first, mut panes) = build_layout(state, first, loaded)?;
            let (second, second_panes) = build_layout(state, second, loaded)?;
            panes.extend(second_panes);
            let axis = match axis.as_str() {
                "horizontal" => crate::editor::Axis::Horizontal,
                "vertical" => crate::editor::Axis::Vertical,
                _ => return None,
            };
            Some((
                crate::editor::LayoutNode::Split {
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
pub(crate) fn build_restored_layout(
    state: &mut EditorState,
    layout: &PersistedLayout,
    loaded: &HashMap<PathBuf, (PathBuf, TextBuffer)>,
) -> Option<(
    crate::editor::LayoutNode,
    Vec<(PathBuf, crate::editor::PaneId)>,
)> {
    build_layout(state, layout, loaded)
}
