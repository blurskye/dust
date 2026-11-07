//! Mouse targets, gestures and document positions.

use crate::app::input::InputState;
use crate::editor::BufferId;
use crate::editor::EditorMode;
use crate::editor::EditorOperation;
use crate::editor::EditorTransaction;
use crate::editor::EditorWindowVersion;
use crate::editor::Precondition;
use crate::editor::TransactionId;
use crate::editor::UndoPolicy;
use crate::editor::View;
use crate::editor::ViewId;
use crate::editor::text::BufferRevision;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use crate::ui::VisualRowAnchor;
use crate::ui::VisualRowModel;
use crate::windows::WindowId;

pub(super) struct MouseState {
    position: Option<(u16, u16)>,
    version: Option<u64>,
    anchor: Option<MouseAnchor>,
}
impl Default for MouseState {
    fn default() -> Self {
        Self {
            position: None,
            version: Some(0),
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
pub(in crate::app) struct MouseDocument<'a> {
    pub editor: &'a crate::editor::EditorVersion,
    pub view_id: ViewId,
    pub pane_id: WindowId,
    pub view: &'a View,
    pub text: &'a TextBuffer,
}
pub(in crate::app) struct MouseGeometry<'a> {
    pub column: u16,
    pub row: u16,
    pub body_height: u16,
    pub gutter: u16,
    pub rows: &'a VisualRowModel<'a>,
}
pub(in crate::app) struct MousePlan {
    transaction: EditorTransaction,
    version: u64,
    before: crate::editor::EditorVersion,
    anchor: Option<MouseAnchor>,
}
impl InputState {
    pub(in crate::app) fn mouse_transaction<'a>(
        &self,
        plan: &'a MousePlan,
        current: &crate::editor::EditorVersion,
    ) -> Result<&'a EditorTransaction, &'static str> {
        if self.mouse.version != Some(plan.version) || &plan.before != current {
            return Err("mouse plan source changed before acceptance");
        }
        Ok(&plan.transaction)
    }

    pub(in crate::app) fn observe_mouse(&mut self, column: u16, row: u16) {
        self.mouse.position = Some((column, row));
        self.mouse.version = self.mouse.version.and_then(|n| n.checked_add(1));
    }
    pub(in crate::app) fn mouse_position(&self) -> Option<(u16, u16)> {
        self.mouse.position
    }
    pub(in crate::app) fn remember_search(&mut self, forward: bool) {
        self.last_search_forward = forward;
        self.whole_word_search = false;
    }
    pub(in crate::app) fn remember_word_search(&mut self) {
        self.whole_word_search = true;
    }
    pub(in crate::app) fn whole_word_search(&self) -> bool {
        self.whole_word_search
    }
    pub(in crate::app) fn search_direction(&self, reverse: bool) -> bool {
        self.last_search_forward != reverse
    }
    pub(in crate::app) fn accept_mouse_plan(
        &mut self,
        plan: MousePlan,
        editor: &EditorWindowVersion,
        current: Option<(ViewId, BufferId, BufferRevision)>,
    ) {
        if self.mouse.version != Some(plan.version) {
            return;
        }
        if let Some(anchor) = plan.anchor
            && anchor.editor.same_editor(editor)
            && current == Some((anchor.view, anchor.buffer, anchor.revision))
        {
            self.mouse.anchor = Some(anchor);
        }
    }
    pub(in crate::app) fn mouse_plan(
        &self,
        document: MouseDocument<'_>,
        geometry: MouseGeometry<'_>,
        transaction_id: TransactionId,
        extend_selection: bool,
    ) -> Option<MousePlan> {
        let version = self.mouse.version?;
        let MouseGeometry {
            column,
            row,
            body_height,
            gutter,
            rows: visual_rows,
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
        let target_anchor = visual_rows.painted_row(start_anchor, usize::from(row))?;
        let line_index = target_anchor.line;
        let line_start = text.line_to_byte(line_index).unwrap_or(ByteOffset(0));
        let byte = visual_rows.byte_at_scrolled_cell(
            target_anchor,
            usize::from(column.saturating_sub(gutter)),
            view.scroll_column,
        );
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
            let mut anchor = self
                .mouse
                .anchor
                .as_ref()
                .filter(|anchor| {
                    anchor.editor.same_editor(document.editor.window())
                        && anchor.view == view_id
                        && anchor.buffer == buffer_id
                        && anchor.revision == revision
                })
                .map_or(original_cursor, |anchor| anchor.position);
            let mut head = position;
            if document.view.mode == EditorMode::Insert {
                // Insert selections (including snippet placeholders) are half-open.
                let end = if anchor <= head {
                    &mut head
                } else {
                    &mut anchor
                };
                *end = crate::editor::text::next_grapheme_boundary(text, *end);
            }
            transaction.operations.push(EditorOperation::SetSelection {
                view: view_id,
                selection: Some(crate::editor::Selection {
                    anchor,
                    head,
                    kind: crate::editor::SelectionKind::Character,
                }),
            });
            // Dragging while typing keeps typing, so the next key replaces the selection.
            if document.view.mode != EditorMode::Insert {
                transaction.operations.push(EditorOperation::SetMode {
                    view: view_id,
                    mode: EditorMode::VisualChar,
                });
            }
        } else {
            transaction.operations.push(EditorOperation::SetSelection {
                view: view_id,
                selection: None,
            });
            if matches!(
                document.view.mode,
                EditorMode::VisualChar | EditorMode::VisualLine | EditorMode::VisualBlock
            ) {
                transaction.operations.push(EditorOperation::SetMode {
                    view: view_id,
                    mode: EditorMode::Normal,
                });
            }
        }
        Some(MousePlan {
            transaction,
            version,
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
    use crate::app::input::mouse::*;
    use crate::editor::EditorState;
    pub(in crate::app) fn plan(input: &InputState, state: &EditorState) -> MousePlan {
        plan_at(input, state, &crate::ui::UiState::default(), 1, 0, false)
    }
    fn plan_at(
        input: &InputState,
        state: &EditorState,
        ui: &crate::ui::UiState,
        column: u16,
        row: u16,
        extend: bool,
    ) -> MousePlan {
        let config = crate::config::load_bundled_config().config;
        let view_id = state.active_view().unwrap();
        let view = &state.views[view_id];
        let text = &state.buffers[view.buffer].text;
        let rows = VisualRowModel::new_for_state(state, text, view_id, ui, &config, 80, false);
        input
            .mouse_plan(
                MouseDocument {
                    editor: &state.version(),
                    view_id,
                    pane_id: state
                        .editor_windows()
                        .map(|window| window.id)
                        .next()
                        .unwrap(),
                    view,
                    text,
                },
                MouseGeometry {
                    column,
                    row,
                    body_height: 5,
                    gutter: 0,
                    rows: &rows,
                },
                TransactionId(1),
                extend,
            )
            .unwrap()
    }
    #[test]
    fn mouse_plans_reject_new_input_or_editor_changes_and_publish_anchors_only_after_commit() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, view) = state.open_text(TextBuffer::from_text("éab"), None).unwrap();
        let mut input = InputState::new(100);
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
    #[test]
    fn scrolled_mouse_hit_testing_ignores_inlays_beyond_the_logical_line_limit() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, view) = state
            .open_text(TextBuffer::from_text(&"a".repeat(50)), None)
            .unwrap();
        state.views[view].scroll_column = 33;
        let mut ui = crate::ui::UiState::default();
        ui.lsp_inlay_hints = (0..40)
            .map(|position| crate::ui::UiInlayHint {
                buffer,
                revision: state.buffers[buffer].text.revision(),
                position: ByteOffset(position),
                label: ":".into(),
            })
            .collect();
        let input = InputState::new(100);
        let pending = plan_at(&input, &state, &ui, 1, 0, false);
        state
            .apply_transaction(input.mouse_transaction(&pending, &state.version()).unwrap())
            .unwrap();
        assert_eq!(state.views[view].cursor, ByteOffset(34));
    }

    #[test]
    fn unwrapped_mouse_hit_testing_uses_rendered_markdown_after_source_scrolling() {
        for (scroll, column) in [(0, 3), (3, 2)] {
            let mut state = EditorState::new("/workspace".into());
            let (buffer, view) = state
                .open_text(TextBuffer::from_text("**abc**def\ncursor"), None)
                .unwrap();
            state.views[view].cursor = ByteOffset(11);
            state.views[view].scroll_column = scroll;
            let mut ui = crate::ui::UiState::default();
            ui.markdown_marks = [(0, 2), (5, 7)]
                .into_iter()
                .map(|(start, end)| crate::ui::UiMarkdownMark {
                    buffer,
                    revision: state.buffers[buffer].text.revision(),
                    range: crate::editor::text::ByteRange::new(ByteOffset(start), ByteOffset(end))
                        .unwrap(),
                    mark: crate::language::syntax::MarkdownMark::Hide,
                })
                .collect();
            let input = InputState::new(100);
            let pending = plan_at(&input, &state, &ui, column, 0, false);
            state
                .apply_transaction(input.mouse_transaction(&pending, &state.version()).unwrap())
                .unwrap();
            assert_eq!(state.views[view].cursor, ByteOffset(7));
        }
    }

    #[test]
    fn insert_mouse_drags_replace_both_endpoint_graphemes_in_either_direction() {
        for (start, end) in [(0, 2), (2, 0)] {
            let mut state = EditorState::new("/workspace".into());
            let (buffer, view) = state
                .open_text(TextBuffer::from_text("abé\u{301}d"), None)
                .unwrap();
            state.views[view].mode = EditorMode::Insert;
            let ui = crate::ui::UiState::default();
            let mut input = InputState::new(100);
            let pending = plan_at(&input, &state, &ui, start, 0, false);
            state
                .apply_transaction(input.mouse_transaction(&pending, &state.version()).unwrap())
                .unwrap();
            input.accept_mouse_plan(
                pending,
                &state.window_version(),
                Some((view, buffer, state.buffers[buffer].text.revision())),
            );
            let pending = plan_at(&input, &state, &ui, end, 0, true);
            state
                .apply_transaction(input.mouse_transaction(&pending, &state.version()).unwrap())
                .unwrap();
            assert_eq!(state.views[view].mode, EditorMode::Insert);
            let selection = state.views[view].selection.unwrap();
            assert_eq!(selection.anchor.min(selection.head), ByteOffset(0));
            assert_eq!(
                selection.anchor.max(selection.head),
                ByteOffset("abé\u{301}".len())
            );
            let mut registry = crate::editor::CommandRegistry::<()>::default();
            crate::editor::register_builtin_commands(&mut registry).unwrap();
            let invocation = crate::editor::CommandInvocation::new(
                "edit.insert_text",
                crate::editor::InvocationSource::Test,
            )
            .unwrap()
            .with_argument("text", crate::editor::CommandValue::String("X".into()));
            let outcome = registry.execute(&state, &invocation).unwrap();
            state
                .apply_transaction(outcome.transaction.as_ref().unwrap())
                .unwrap();
            assert_eq!(state.buffers[buffer].text.to_string(), "Xd");
        }
    }

    #[test]
    fn plain_click_leaves_visual_mode_with_no_selection() {
        let mut state = EditorState::new("/workspace".into());
        let (_, view) = state.open_text(TextBuffer::from_text("abc"), None).unwrap();
        state.views[view].mode = EditorMode::VisualChar;
        state.views[view].selection = Some(crate::editor::Selection {
            anchor: ByteOffset(0),
            head: ByteOffset(2),
            kind: crate::editor::SelectionKind::Character,
        });
        let mut input = InputState::new(100);
        input.observe_mouse(1, 0);
        let pending = plan(&input, &state);
        state
            .apply_transaction(input.mouse_transaction(&pending, &state.version()).unwrap())
            .unwrap();
        assert_eq!(state.views[view].mode, EditorMode::Normal);
        assert!(state.views[view].selection.is_none());
    }
}
