use super::InputSession;
use crate::editor::text::{
    BufferRevision, ByteOffset, ByteRange, TextBuffer, TextEdit, TextEditSet,
};
use crate::editor::{
    BufferId, EditorOperation, EditorTransaction, EditorWindowVersion, Selection, SelectionKind,
    ViewId,
};
use std::{collections::BTreeMap, sync::Arc};

pub(in crate::editor::session) struct SnippetDocument<'a> {
    pub editor: &'a EditorWindowVersion,
    pub buffer: BufferId,
    pub text: &'a TextBuffer,
    pub view: Option<ViewId>,
}
#[derive(Clone)]
pub(super) struct SnippetSession {
    identity: Arc<()>,
    editor: EditorWindowVersion,
    buffer: BufferId,
    revision: BufferRevision,
    groups: Vec<Vec<ByteRange>>,
    current: usize,
}
impl SnippetSession {
    fn matches(&self, document: &SnippetDocument<'_>) -> bool {
        self.editor.same_editor(document.editor)
            && self.buffer == document.buffer
            && self.revision == document.text.revision()
    }
}
pub(in crate::editor::session) struct SnippetUpdate {
    identity: Arc<()>,
    buffer: BufferId,
    before: BufferRevision,
    groups: Vec<Vec<ByteRange>>,
}
impl InputSession {
    pub(in crate::editor::session) fn snippet_buffer(&self) -> Option<BufferId> {
        self.snippet.as_ref().map(|s| s.buffer)
    }
    pub(in crate::editor::session) fn clear_snippet(&mut self) {
        self.snippet = None;
    }
    pub(in crate::editor::session) fn install_snippet(
        &mut self,
        document: SnippetDocument<'_>,
        insertion: ByteOffset,
        stops: impl IntoIterator<Item = (u32, usize, usize)>,
    ) {
        let mut grouped = BTreeMap::<u32, Vec<ByteRange>>::new();
        for (index, start, end) in stops {
            let start = ByteOffset(insertion.get().saturating_add(start));
            let end = ByteOffset(insertion.get().saturating_add(end));
            let Ok(range) = ByteRange::new(start, end) else {
                self.clear_snippet();
                return;
            };
            if end.get() > document.text.len_bytes()
                || !document.text.is_char_boundary(start)
                || !document.text.is_char_boundary(end)
            {
                self.clear_snippet();
                return;
            }
            grouped.entry(index).or_default().push(range);
        }
        let mut groups = grouped.into_iter().collect::<Vec<_>>();
        groups.sort_by_key(|(index, _)| (*index == 0, *index));
        self.snippet = (!groups.is_empty()).then(|| SnippetSession {
            identity: Arc::new(()),
            editor: document.editor.clone(),
            buffer: document.buffer,
            revision: document.text.revision(),
            groups: groups.into_iter().map(|(_, ranges)| ranges).collect(),
            current: 0,
        });
    }
    pub(in crate::editor::session) fn accept_snippet_edit(
        &mut self,
        update: SnippetUpdate,
        document: Option<SnippetDocument<'_>>,
    ) -> bool {
        let Some(session) = &mut self.snippet else {
            return false;
        };
        let Some(document) = document else {
            self.clear_snippet();
            return false;
        };
        if !Arc::ptr_eq(&update.identity, &session.identity) {
            return false;
        }
        if !session.editor.same_editor(document.editor)
            || document.buffer != update.buffer
            || session.buffer != update.buffer
            || session.revision != update.before
            || update.before.0.checked_add(1) != Some(document.text.revision().0)
        {
            self.clear_snippet();
            return false;
        }
        session.groups = update.groups;
        session.revision = document.text.revision();
        true
    }
    pub(in crate::editor::session) fn snippet_selection(
        &mut self,
        document: Option<SnippetDocument<'_>>,
    ) -> Option<[EditorOperation; 2]> {
        let session = self.snippet.as_ref()?;
        let Some(document) = document else {
            self.clear_snippet();
            return None;
        };
        if !session.matches(&document) {
            self.clear_snippet();
            return None;
        }
        let view = document.view?;
        let range = *session.groups.get(session.current)?.first()?;
        Some([
            EditorOperation::SetCursor {
                view,
                position: range.end,
            },
            EditorOperation::SetSelection {
                view,
                selection: Some(Selection {
                    anchor: range.start,
                    head: range.end,
                    kind: SelectionKind::Character,
                }),
            },
        ])
    }
    pub(in crate::editor::session) fn navigate_snippet(&mut self, direction: i8) {
        let Some(session) = &mut self.snippet else {
            return;
        };
        if direction > 0 {
            if session.current + 1 >= session.groups.len() {
                self.clear_snippet();
                return;
            }
            session.current += 1;
        } else if direction < 0 {
            session.current = session.current.saturating_sub(1);
        }
    }
    pub(in crate::editor::session) fn prepare_snippet_mirror(
        &mut self,
        document: Option<SnippetDocument<'_>>,
        transaction: &mut EditorTransaction,
    ) -> Option<SnippetUpdate> {
        let session = self.snippet.as_ref()?;
        let Some(document) = document else {
            self.snippet = None;
            return None;
        };
        if !session.matches(&document) {
            self.snippet = None;
            return None;
        }
        let Some(primary) = session
            .groups
            .get(session.current)
            .and_then(|group| group.first())
            .copied()
        else {
            self.snippet = None;
            return None;
        };
        let mut target = None;
        for operation in &mut transaction.operations {
            if let EditorOperation::ApplyEdits { buffer, edits } = operation {
                if *buffer != session.buffer || target.is_some() {
                    self.snippet = None;
                    return None;
                }
                target = Some(edits.clone());
            }
        }
        let edits = target?;
        if edits.base_revision() != session.revision
            || edits.edits().is_empty()
            || edits
                .edits()
                .iter()
                .any(|edit| edit.range().start < primary.start || edit.range().end > primary.end)
        {
            self.snippet = None;
            return None;
        }
        let Ok(mut replacement) = document.text.slice(primary) else {
            self.snippet = None;
            return None;
        };
        let mut local = edits
            .edits()
            .iter()
            .map(|edit| {
                (
                    edit.range().start.get() - primary.start.get(),
                    edit.range().end.get() - primary.start.get(),
                    edit.replacement().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        local.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
        for (start, end, text) in local {
            if !replacement.is_char_boundary(start) || !replacement.is_char_boundary(end) {
                self.snippet = None;
                return None;
            }
            replacement.replace_range(start..end, &text);
        }
        let mut mirrors = session.groups[session.current].clone();
        mirrors.sort_by_key(|range| range.start);
        let replacement_edits = mirrors
            .iter()
            .map(|range| TextEdit::new(*range, replacement.clone()))
            .collect::<Vec<_>>();
        for operation in &mut transaction.operations {
            if let EditorOperation::ApplyEdits { buffer, edits } = operation
                && *buffer == session.buffer
            {
                *edits = TextEditSet::new(session.revision, replacement_edits.clone());
            }
        }
        let shift = |position: ByteOffset| {
            let mut value = position.get() as i128;
            for range in &mirrors {
                if range.end <= position {
                    value += replacement.len() as i128 - range.len() as i128;
                }
            }
            ByteOffset(value.max(0) as usize)
        };
        let mut groups = Vec::with_capacity(session.groups.len());
        for group in &session.groups {
            let mut updated = Vec::with_capacity(group.len());
            for range in group {
                if mirrors.contains(range) {
                    let start = shift(range.start);
                    updated.push(
                        ByteRange::new(start, ByteOffset(start.get() + replacement.len())).ok()?,
                    );
                } else {
                    updated.push(ByteRange::new(shift(range.start), shift(range.end)).ok()?);
                }
            }
            groups.push(updated);
        }
        Some(SnippetUpdate {
            identity: session.identity.clone(),
            buffer: session.buffer,
            before: session.revision,
            groups,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{EditorState, TransactionId};
    fn fixture() -> (EditorState, BufferId, InputSession) {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state.open_text(TextBuffer::from_text("x x"), None);
        (state, buffer, InputSession::new(100))
    }
    fn document<'a>(
        state: &'a EditorState,
        buffer: BufferId,
        source: &'a EditorWindowVersion,
    ) -> SnippetDocument<'a> {
        SnippetDocument {
            editor: source,
            buffer,
            text: &state.buffers[buffer].text,
            view: state.active_view,
        }
    }
    #[test]
    fn mirrors_are_input_owned_and_only_the_matching_post_commit_reply_advances_ranges() {
        let (mut state, buffer, mut input) = fixture();
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            [(1, 0, 1), (1, 2, 3)],
        );
        let mut transaction =
            EditorTransaction::new(TransactionId(1)).with_operation(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    state.buffers[buffer].text.revision(),
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
                        "é",
                    )],
                ),
            });
        let update = input
            .prepare_snippet_mirror(Some(document(&state, buffer, &source)), &mut transaction)
            .unwrap();
        assert_eq!(state.buffers[buffer].text.to_string(), "x x");
        state.apply_transaction(&transaction).unwrap();
        assert_eq!(state.buffers[buffer].text.to_string(), "é é");
        let source = state.window_version();
        assert!(input.accept_snippet_edit(update, Some(document(&state, buffer, &source))));
        assert_eq!(
            input.snippet.as_ref().unwrap().groups[0][1],
            ByteRange::new(ByteOffset(3), ByteOffset(5)).unwrap()
        );
        assert!(
            input
                .snippet_selection(Some(document(&state, buffer, &source)))
                .is_some()
        );
    }
    #[test]
    fn foreign_editor_reused_ids_and_reopened_snippets_reject_old_replies() {
        let (mut state, buffer, mut input) = fixture();
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            [(1, 0, 1)],
        );
        let mut transaction =
            EditorTransaction::new(TransactionId(1)).with_operation(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    state.buffers[buffer].text.revision(),
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
                        "z",
                    )],
                ),
            });
        let update = input
            .prepare_snippet_mirror(Some(document(&state, buffer, &source)), &mut transaction)
            .unwrap();
        state.apply_transaction(&transaction).unwrap();
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            [(1, 0, 1)],
        );
        assert!(!input.accept_snippet_edit(update, Some(document(&state, buffer, &source))));
        assert!(input.snippet_buffer().is_some());
        let (foreign, foreign_buffer, _) = fixture();
        assert_eq!(foreign_buffer, buffer);
        assert!(
            input
                .snippet_selection(Some(document(
                    &foreign,
                    foreign_buffer,
                    &foreign.window_version()
                )))
                .is_none()
        );
        assert!(input.snippet_buffer().is_none());
    }
}
