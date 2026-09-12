use super::InputSession;
use crate::editor::session::configuration::{
    configured_byte_offset_at_display_column, configured_grapheme_width, configured_text_width,
};
use crate::editor::text::{BufferRevision, ByteOffset, TextBuffer};
use crate::editor::{
    BufferId, EditorMode, EditorOperation, EditorTransaction, EditorWindowVersion, PaneId,
    Precondition, TransactionId, UndoPolicy, View, ViewId,
};
use crate::ui::{UiInlayHint, VisualRowAnchor, VisualRowModel, snap_display_column};
use unicode_segmentation::UnicodeSegmentation;

pub(super) struct MouseState {
    position: Option<(u16, u16)>,
    generation: Option<u64>,
    anchor: Option<MouseAnchor>,
}
impl Default for MouseState {
    fn default() -> Self {
        Self {
            position: None,
            generation: Some(0),
            anchor: None,
        }
    }
}
struct MouseAnchor {
    editor: EditorWindowVersion,
    view: ViewId,
    buffer: BufferId,
    revision: BufferRevision,
    position: ByteOffset,
}
pub(in crate::editor::session) struct MouseDocument<'a> {
    pub editor: &'a crate::editor::EditorVersion,
    pub view_id: ViewId,
    pub pane_id: PaneId,
    pub view: &'a View,
    pub text: &'a TextBuffer,
}
pub(in crate::editor::session) struct MouseGeometry<'a> {
    pub column: u16,
    pub row: u16,
    pub body_height: u16,
    pub gutter: u16,
    pub wrap: bool,
    pub rows: &'a VisualRowModel<'a>,
    pub inlays: &'a [UiInlayHint],
}
pub(in crate::editor::session) struct MousePlan {
    transaction: EditorTransaction,
    generation: u64,
    before: crate::editor::EditorVersion,
    anchor: Option<MouseAnchor>,
}
impl InputSession {
    pub(in crate::editor::session) fn mouse_transaction<'a>(
        &self,
        plan: &'a MousePlan,
        current: &crate::editor::EditorVersion,
    ) -> Result<&'a EditorTransaction, &'static str> {
        if self.mouse.generation != Some(plan.generation) || &plan.before != current {
            return Err("mouse plan source changed before acceptance");
        }
        Ok(&plan.transaction)
    }

    pub(in crate::editor::session) fn observe_mouse(&mut self, column: u16, row: u16) {
        self.mouse.position = Some((column, row));
        self.mouse.generation = self.mouse.generation.and_then(|n| n.checked_add(1));
    }
    pub(in crate::editor::session) fn mouse_position(&self) -> Option<(u16, u16)> {
        self.mouse.position
    }
    pub(in crate::editor::session) fn remember_search(&mut self, forward: bool) {
        self.last_search_forward = forward;
    }
    pub(in crate::editor::session) fn search_direction(&self, reverse: bool) -> bool {
        self.last_search_forward != reverse
    }
    pub(in crate::editor::session) fn accept_mouse_plan(
        &mut self,
        plan: MousePlan,
        editor: &EditorWindowVersion,
        current: Option<(ViewId, BufferId, BufferRevision)>,
    ) {
        if self.mouse.generation != Some(plan.generation) {
            return;
        }
        if let Some(anchor) = plan.anchor {
            if anchor.editor.same_editor(editor)
                && current == Some((anchor.view, anchor.buffer, anchor.revision))
            {
                self.mouse.anchor = Some(anchor);
            }
        }
    }
    pub(in crate::editor::session) fn mouse_plan(
        &self,
        document: MouseDocument<'_>,
        geometry: MouseGeometry<'_>,
        policy: &crate::config::EditorConfig,
        transaction_id: TransactionId,
        extend_selection: bool,
    ) -> Option<MousePlan> {
        let generation = self.mouse.generation?;
        let MouseGeometry {
            column,
            row,
            body_height,
            gutter,
            wrap: wrap_enabled,
            rows: visual_rows,
            ..
        } = geometry;
        if row >= body_height {
            return None;
        }
        let MouseDocument {
            view_id,
            pane_id,
            view,
            text,
            ..
        } = document;
        let original_cursor = view.cursor;
        let buffer_id = view.buffer;
        let revision = text.revision();
        let start_anchor = visual_rows.normalize(VisualRowAnchor {
            line: view.scroll_line,
            wrap_row: view.scroll_wrap_row,
        });
        let target_anchor = visual_rows.forward(start_anchor, usize::from(row));
        if visual_rows.screen_row(start_anchor, target_anchor, usize::from(body_height))
            != Some(usize::from(row))
        {
            return None;
        }
        let line_index = target_anchor.line;
        let line_start = text.line_to_byte(line_index).unwrap_or(ByteOffset(0));
        let byte = if wrap_enabled {
            visual_rows.byte_at_cell(target_anchor, usize::from(column.saturating_sub(gutter)))
        } else {
            let line = text.line(line_index).unwrap_or_default();
            let content = line.strip_suffix('\n').unwrap_or(&line);
            let snapped_scroll = snap_display_column(
                content,
                view.scroll_column,
                policy.tab_width,
                policy.ambiguous_width,
            );
            let target_column =
                usize::from(column.saturating_sub(gutter)).saturating_add(snapped_scroll);
            let skipped_bytes = configured_byte_offset_at_display_column(
                content,
                view.scroll_column,
                policy.tab_width,
                policy.ambiguous_width,
            );
            let visible_start = ByteOffset(line_start.get().saturating_add(skipped_bytes));
            let line_end = ByteOffset(line_start.get().saturating_add(content.len()));
            let mut hints = geometry
                .inlays
                .iter()
                .filter(|hint| {
                    hint.buffer == buffer_id
                        && hint.revision == revision
                        && hint.position >= visible_start
                        && hint.position <= line_end
                })
                .collect::<Vec<_>>();
            hints.sort_unstable_by_key(|hint| hint.position);
            hints.truncate(32);
            let mut hint_cursor = 0usize;
            let mut display_column = 0_usize;
            let mut virtual_cells = 0usize;
            'target: {
                for (relative, grapheme) in content.grapheme_indices(true) {
                    let global = ByteOffset(line_start.get().saturating_add(relative));
                    while let Some(hint) = hints
                        .get(hint_cursor)
                        .filter(|hint| hint.position == global)
                    {
                        let width = configured_text_width(
                            &hint.label,
                            policy.tab_width,
                            policy.ambiguous_width,
                        );
                        let start = display_column.saturating_add(virtual_cells);
                        if start.saturating_add(width) > target_column {
                            break 'target relative;
                        }
                        virtual_cells = virtual_cells.saturating_add(width);
                        hint_cursor = hint_cursor.saturating_add(1);
                    }
                    let width = if grapheme == "\t" {
                        policy.tab_width - (display_column % policy.tab_width)
                    } else {
                        configured_grapheme_width(grapheme, policy.ambiguous_width)
                    };
                    if display_column
                        .saturating_add(virtual_cells)
                        .saturating_add(width)
                        > target_column
                    {
                        break 'target relative;
                    }
                    display_column = display_column.saturating_add(width);
                }
                content.len()
            }
        };
        let position = ByteOffset(line_start.get().saturating_add(byte));
        let mut transaction = EditorTransaction::new(transaction_id)
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_operation(EditorOperation::FocusPaneId { pane: pane_id })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if extend_selection {
            transaction.operations.push(EditorOperation::SetSelection {
                view: view_id,
                selection: Some(crate::editor::Selection {
                    anchor: self
                        .mouse
                        .anchor
                        .as_ref()
                        .filter(|anchor| {
                            anchor.editor.same_editor(document.editor.window())
                                && anchor.view == view_id
                                && anchor.buffer == buffer_id
                                && anchor.revision == revision
                        })
                        .map_or(original_cursor, |anchor| anchor.position),
                    head: position,
                    kind: crate::editor::SelectionKind::Character,
                }),
            });
            transaction.operations.push(EditorOperation::SetMode {
                view: view_id,
                mode: EditorMode::VisualChar,
            });
        } else {
            transaction.operations.push(EditorOperation::SetSelection {
                view: view_id,
                selection: None,
            });
        }
        Some(MousePlan {
            transaction,
            generation,
            before: document.editor.clone(),
            anchor: (!extend_selection).then(|| MouseAnchor {
                editor: document.editor.window().clone(),
                view: view_id,
                buffer: buffer_id,
                revision,
                position,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::EditorState;
    fn plan(input: &InputSession, state: &EditorState) -> MousePlan {
        let config = crate::config::load_bundled_config().config;
        let view_id = state.active_view.unwrap();
        let view = &state.views[view_id];
        let text = &state.buffers[view.buffer].text;
        let ui = crate::ui::UiState::default();
        let rows = VisualRowModel::new_for_state(state, text, view_id, &ui, &config, 80, false);
        input
            .mouse_plan(
                MouseDocument {
                    editor: &state.version(),
                    view_id,
                    pane_id: state.panes.keys().next().unwrap(),
                    view,
                    text,
                },
                MouseGeometry {
                    column: 1,
                    row: 0,
                    body_height: 5,
                    gutter: 0,
                    wrap: false,
                    rows: &rows,
                    inlays: &[],
                },
                &config.editor,
                TransactionId(1),
                false,
            )
            .unwrap()
    }
    #[test]
    fn mouse_plans_reject_new_input_or_editor_changes_and_publish_anchors_only_after_commit() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, view) = state.open_text(TextBuffer::from_text("éab"), None);
        let mut input = InputSession::new(100);
        input.observe_mouse(1, 0);
        let pending = plan(&input, &state);
        let window = state.window_version();
        state
            .apply_transaction(&EditorTransaction::new(TransactionId(2)).with_operation(
                EditorOperation::SetCursor {
                    view,
                    position: ByteOffset(3),
                },
            ))
            .unwrap();
        assert_eq!(state.window_version(), window);
        assert!(input.mouse_transaction(&pending, &state.version()).is_err());
        let pending = plan(&input, &state);
        input.observe_mouse(1, 0);
        assert!(input.mouse_transaction(&pending, &state.version()).is_err());
        let pending = plan(&input, &state);
        assert!(input.mouse.anchor.is_none());
        state
            .apply_transaction(input.mouse_transaction(&pending, &state.version()).unwrap())
            .unwrap();
        input.accept_mouse_plan(
            pending,
            &state.window_version(),
            Some((view, buffer, state.buffers[buffer].text.revision())),
        );
        assert_eq!(input.mouse.anchor.as_ref().unwrap().position, ByteOffset(2));
        assert_eq!(state.views[view].cursor, ByteOffset(2));
    }
}
