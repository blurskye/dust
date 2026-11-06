//! Snippet fields, navigation and mirrored edits.

use crate::app::input::InputState;
use crate::editor::BufferId;
use crate::editor::EditorOperation;
use crate::editor::EditorTransaction;
use crate::editor::EditorWindowVersion;
use crate::editor::Selection;
use crate::editor::SelectionKind;
use crate::editor::ViewId;
use crate::editor::text::BufferRevision;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::TextBuffer;
use crate::editor::text::TextEdit;
use crate::editor::text::TextEditSet;
use crate::language::lsp::SnippetTabstop;
use crate::language::lsp::SnippetTransform;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(in crate::app) struct SnippetDocument<'a> {
    pub editor: &'a EditorWindowVersion,
    pub buffer: BufferId,
    pub text: &'a TextBuffer,
    pub view: Option<ViewId>,
}
#[derive(Clone)]
pub(in crate::app) struct SnippetExpansion {
    identity: Arc<()>,
    editor: EditorWindowVersion,
    buffer: BufferId,
    revision: BufferRevision,
    groups: Vec<Vec<SnippetTabstop>>,
    transforms: Vec<SnippetTransform>,
    current: usize,
}
impl SnippetExpansion {
    pub(in crate::app) fn matches(&self, document: &SnippetDocument<'_>) -> bool {
        self.editor.same_editor(document.editor)
            && self.buffer == document.buffer
            && self.revision == document.text.revision()
    }
}
pub(in crate::app) struct SnippetUpdate {
    identity: Arc<()>,
    buffer: BufferId,
    before: BufferRevision,
    groups: Vec<Vec<SnippetTabstop>>,
    transforms: Vec<SnippetTransform>,
    current: usize,
}

/// An edit with the key that orders it against insertions at the same position: the source
/// order of the tabstop or copy it belongs to, then copies before tabstops.
type OrderedEdit = (TextEdit, (usize, u8));

/// Builds one edit set from edits ordered by position and then by their key, joining
/// insertions at the same position in that order.
pub(in crate::app) fn merge_edits(
    revision: BufferRevision,
    mut edits: Vec<OrderedEdit>,
) -> TextEditSet {
    edits.sort_by_key(|(edit, key)| (edit.range().start, edit.range().end, *key));
    let mut merged: Vec<TextEdit> = Vec::new();
    for (edit, _) in edits {
        if edit.range().is_empty()
            && let Some(previous) = merged
                .last_mut()
                .filter(|previous| previous.range() == edit.range())
        {
            *previous = TextEdit::new(
                edit.range(),
                format!("{}{}", previous.replacement(), edit.replacement()),
            );
        } else {
            merged.push(edit);
        }
    }
    TextEditSet::new(revision, merged)
}

/// Recomputes transformed copies such as `${1/(.*)/${1:/upcase}/}` from the edited tabstops.
/// `edits` turn `text` into the text with the new tabstop values, whose positions `groups`
/// holds. Returns the moved copies and the edits that rewrite them, keyed for `merge_edits`,
/// and moves `groups` past the rewritten copies.
pub(in crate::app) fn follow_transforms(
    session: &SnippetExpansion,
    text: &TextBuffer,
    copies: &[(TextEdit, (usize, usize))],
    edits: &TextEditSet,
    groups: &mut [Vec<SnippetTabstop>],
) -> Option<(Vec<SnippetTransform>, Vec<OrderedEdit>)> {
    if session.transforms.is_empty() {
        return Some((Vec::new(), Vec::new()));
    }
    let mut edited = text.clone();
    edited.apply_edits(edits).ok()?;
    // Each copy with its position in `edited`, its old text and its new text.
    let mut moved = Vec::new();
    for transform in &session.transforms {
        let old = transform.range();
        let replaced = copies.iter().any(|(edit, _)| {
            let range = edit.range();
            !range.is_empty() && range.start < old.end && old.start < range.end
        });
        let value = groups
            .iter()
            .find(|group| group[0].index == transform.index)
            .and_then(|group| edited.slice(group[0].range()).ok());
        let (false, Some(value), Ok(current)) = (replaced, value, text.slice(old)) else {
            continue;
        };
        let shift = |position: ByteOffset, end: bool| {
            let mut value = position.get() as i128;
            for (edit, owner) in copies {
                let range = edit.range();
                if range.is_empty() {
                    // Typing at the edge of a copy's text goes outside it.
                    let before = if old.is_empty() {
                        session.groups[owner.0][owner.1].order < transform.order
                    } else {
                        !end
                    };
                    if range.start < position || (range.start == position && before) {
                        value += edit.replacement().len() as i128;
                    }
                } else if range.end <= position {
                    value += edit.replacement().len() as i128 - range.len() as i128;
                }
            }
            value.max(0) as usize
        };
        // A rule that fails on the new value leaves the copy as it was.
        let output = transform.apply(&value).unwrap_or_else(|_| current.clone());
        let mut transform = transform.clone();
        transform.start = shift(old.start, false);
        transform.end = shift(old.end, true);
        moved.push((transform, old, current, output));
    }
    let encloses = |stop: &SnippetTabstop, transform: &SnippetTransform| {
        stop.depth < transform.depth
            && stop.order < transform.order
            && stop.start <= transform.start
            && transform.end <= stop.end
            && !session.groups.iter().flatten().any(|other| {
                stop.order < other.order
                    && other.order < transform.order
                    && other.depth <= stop.depth
            })
    };
    for stop in groups.iter_mut().flatten() {
        let shift = |position: usize, end: bool| {
            let mut value = position as i128;
            for (transform, _, current, output) in &moved {
                if transform.end < position
                    || (transform.end == position
                        && (stop.order >= transform.order || (end && encloses(stop, transform))))
                {
                    value += output.len() as i128 - current.len() as i128;
                }
            }
            value.max(0) as usize
        };
        let (start, end) = (shift(stop.start, false), shift(stop.end, true));
        stop.start = start;
        stop.end = end;
    }
    let mut transforms = Vec::with_capacity(moved.len());
    let mut rewrites = Vec::new();
    for (index, (transform, old, current, output)) in moved.iter().enumerate() {
        let mut start = transform.start as i128;
        for (other_index, (other, _, other_current, other_output)) in moved.iter().enumerate() {
            if other.end < transform.start || (other.end == transform.start && other_index < index)
            {
                start += other_output.len() as i128 - other_current.len() as i128;
            }
        }
        let mut transform = transform.clone();
        transform.start = start.max(0) as usize;
        transform.end = transform.start + output.len();
        if output != current {
            rewrites.push((TextEdit::new(*old, output.clone()), (transform.order, 0)));
        }
        transforms.push(transform);
    }
    Some((transforms, rewrites))
}

impl InputState {
    pub(in crate::app) fn snippet_buffer(&self) -> Option<BufferId> {
        self.snippet.as_ref().map(|s| s.buffer)
    }
    pub(in crate::app) fn clear_snippet(&mut self) {
        self.snippet = None;
    }
    pub(in crate::app) fn install_snippet(
        &mut self,
        document: SnippetDocument<'_>,
        insertion: ByteOffset,
        stops: impl IntoIterator<Item = SnippetTabstop>,
    ) {
        let mut grouped = BTreeMap::<u32, Vec<SnippetTabstop>>::new();
        for mut stop in stops {
            let start = ByteOffset(insertion.get().saturating_add(stop.start));
            let end = ByteOffset(insertion.get().saturating_add(stop.end));
            let Ok(_) = ByteRange::new(start, end) else {
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
            stop.start = start.get();
            stop.end = end.get();
            grouped.entry(stop.index).or_default().push(stop);
        }
        let mut groups = grouped.into_iter().collect::<Vec<_>>();
        groups.sort_by_key(|(index, _)| (*index == 0, *index));
        self.snippet = (!groups.is_empty()).then(|| SnippetExpansion {
            identity: Arc::new(()),
            editor: document.editor.clone(),
            buffer: document.buffer,
            revision: document.text.revision(),
            groups: groups.into_iter().map(|(_, ranges)| ranges).collect(),
            transforms: Vec::new(),
            current: 0,
        });
    }
    /// Adds the transformed copies of the snippet just installed at `insertion`.
    pub(in crate::app) fn install_snippet_transforms(
        &mut self,
        insertion: ByteOffset,
        transforms: Vec<SnippetTransform>,
    ) {
        if let Some(session) = &mut self.snippet {
            session.transforms = transforms
                .into_iter()
                .map(|mut transform| {
                    transform.start = transform.start.saturating_add(insertion.get());
                    transform.end = transform.end.saturating_add(insertion.get());
                    transform
                })
                .collect();
        }
    }
    pub(in crate::app) fn accept_snippet_edit(
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
        session.transforms = update.transforms;
        session.current = update.current;
        session.revision = document.text.revision();
        true
    }
    pub(in crate::app) fn snippet_selection(
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
        let range = session.groups.get(session.current)?.first()?.range();
        Some([
            EditorOperation::SetCursor {
                view,
                position: range.end,
            },
            EditorOperation::SetSelection {
                view,
                // An empty placeholder selects nothing, or typing there would grow a selection.
                selection: (!range.is_empty()).then_some(Selection {
                    anchor: range.start,
                    head: range.end,
                    kind: SelectionKind::Character,
                }),
            },
        ])
    }
    pub(in crate::app) fn navigate_snippet(&mut self, direction: i8) -> bool {
        let Some(session) = &mut self.snippet else {
            return false;
        };
        if direction > 0 {
            if session.current + 1 >= session.groups.len() {
                self.clear_snippet();
                return true;
            }
            session.current += 1;
        } else if direction < 0 {
            session.current = session.current.saturating_sub(1);
        }
        false
    }
    pub(in crate::app) fn prepare_snippet_mirror(
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
            .cloned()
        else {
            self.snippet = None;
            return None;
        };
        let primary = primary.range();
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
        // Keep the primary edits intact: replacing the entire placeholder would map an
        // interior cursor to its end. Copy those edits to linked occurrences instead.
        let contains = |outer: ByteRange, inner: ByteRange| {
            outer.start <= inner.start && inner.end <= outer.end
        };
        let is_ancestor = |outer: &SnippetTabstop, inner: &SnippetTabstop| {
            outer.depth < inner.depth
                && outer.order < inner.order
                && contains(outer.range(), inner.range())
                && !session.groups.iter().flatten().any(|stop| {
                    outer.order < stop.order
                        && stop.order <= inner.order
                        && stop.depth <= outer.depth
                })
        };
        let mut copies = Vec::new();
        let mut affected = Vec::new();
        for (occurrence, stop) in session.groups[session.current].iter().enumerate() {
            let range = stop.range();
            affected.push((session.current, occurrence));
            for edit in edits.edits() {
                let start = range.start.get() + edit.range().start.get() - primary.start.get();
                let end = range.start.get() + edit.range().end.get() - primary.start.get();
                copies.push((
                    TextEdit::new(
                        ByteRange::new(ByteOffset(start), ByteOffset(end)).ok()?,
                        edit.replacement(),
                    ),
                    (session.current, occurrence),
                ));
            }
        }
        // Work outwards so a parent's mirrors include changes to all child mirrors
        // inside its defining occurrence, not just the edited child.
        let mut parents = session
            .groups
            .iter()
            .enumerate()
            .filter_map(|(index, group)| {
                if index == session.current {
                    return None;
                }
                group
                    .iter()
                    .enumerate()
                    .find(|(_, stop)| {
                        session.groups[session.current]
                            .iter()
                            .any(|child| is_ancestor(stop, child))
                    })
                    .map(|(occurrence, stop)| (index, occurrence, stop.clone()))
            })
            .collect::<Vec<_>>();
        parents.sort_by_key(|(_, _, stop)| std::cmp::Reverse(stop.depth));
        for (index, defining, stop) in parents {
            let parent = stop.range();
            let local = copies
                .iter()
                .filter(|(edit, owner)| {
                    contains(parent, edit.range())
                        && is_ancestor(&stop, &session.groups[owner.0][owner.1])
                })
                .map(|(edit, _)| edit.clone())
                .collect::<Vec<_>>();
            for (occurrence, stop) in session.groups[index].iter().enumerate() {
                let range = stop.range();
                affected.push((index, occurrence));
                if occurrence == defining {
                    continue;
                }
                for edit in &local {
                    let start = range.start.get() + edit.range().start.get() - parent.start.get();
                    let end = range.start.get() + edit.range().end.get() - parent.start.get();
                    copies.push((
                        TextEdit::new(
                            ByteRange::new(ByteOffset(start), ByteOffset(end)).ok()?,
                            edit.replacement(),
                        ),
                        (index, occurrence),
                    ));
                }
            }
        }
        let ordered = copies
            .iter()
            .map(|(edit, (group, occurrence))| {
                (edit.clone(), (session.groups[*group][*occurrence].order, 1))
            })
            .collect::<Vec<_>>();
        let replacement_edits = merge_edits(session.revision, ordered.clone());
        if document.text.validate_edits(&replacement_edits).is_err() {
            self.clear_snippet();
            return None;
        }
        let mut groups = Vec::with_capacity(session.groups.len());
        let mut current = 0;
        for (index, group) in session.groups.iter().enumerate() {
            let mut updated = Vec::with_capacity(group.len());
            for (occurrence, stop) in group.iter().enumerate() {
                let range = stop.range();
                let key = (index, occurrence);
                let grows = affected.contains(&key);
                // Stops overwritten by an edit no longer describe placeholders.
                if !grows
                    && copies.iter().any(|(edit, owner)| {
                        !edit.range().is_empty()
                            && if range.is_empty() {
                                edit.range().start <= range.start
                                    && (range.end < edit.range().end
                                        || (range.end == edit.range().end
                                            && is_ancestor(
                                                &session.groups[owner.0][owner.1],
                                                stop,
                                            )))
                            } else {
                                edit.range().start < range.end && range.start < edit.range().end
                            }
                    })
                {
                    continue;
                }
                let shift = |position: ByteOffset, end: bool| {
                    let mut value = position.get() as i128;
                    for (edit, owner) in &copies {
                        let edited = edit.range();
                        let owner_stop = &session.groups[owner.0][owner.1];
                        let after = if key == *owner || is_ancestor(stop, owner_stop) {
                            end
                        } else {
                            stop.order > owner_stop.order
                        };
                        if edited.is_empty() {
                            if edited.start < position || (edited.start == position && after) {
                                value += edit.replacement().len() as i128;
                            }
                        } else if edited.end <= position {
                            value += edit.replacement().len() as i128 - edited.len() as i128;
                        }
                    }
                    ByteOffset(value.max(0) as usize)
                };
                let range =
                    ByteRange::new(shift(range.start, false), shift(range.end, true)).ok()?;
                let mut stop = stop.clone();
                stop.start = range.start.get();
                stop.end = range.end.get();
                updated.push(stop);
            }
            if !updated.is_empty() {
                if index == session.current {
                    current = groups.len();
                }
                groups.push(updated);
            }
        }
        let Some((transforms, transform_edits)) = follow_transforms(
            session,
            document.text,
            &copies,
            &replacement_edits,
            &mut groups,
        ) else {
            self.clear_snippet();
            return None;
        };
        let replacement_edits = if transform_edits.is_empty() {
            replacement_edits
        } else {
            let edits = merge_edits(session.revision, [ordered, transform_edits].concat());
            if document.text.validate_edits(&edits).is_err() {
                self.clear_snippet();
                return None;
            }
            edits
        };
        let cursor_shift = groups[current][0].start as i128 - primary.start.get() as i128;
        let mut applied = false;
        for operation in &mut transaction.operations {
            match operation {
                EditorOperation::ApplyEdits { buffer, edits } if *buffer == session.buffer => {
                    *edits = replacement_edits.clone();
                    applied = true;
                }
                EditorOperation::SetCursor { position, .. } if applied => {
                    // Commands position the cursor after their primary edits, not after
                    // extra mirrors inserted earlier in the document.
                    *position = ByteOffset((position.get() as i128 + cursor_shift).max(0) as usize);
                }
                _ => {}
            }
        }
        Some(SnippetUpdate {
            identity: session.identity.clone(),
            buffer: session.buffer,
            before: session.revision,
            groups,
            transforms,
            current,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::app::language::snippets::*;
    use crate::editor::EditorState;
    use crate::editor::TransactionId;
    pub(in crate::app) fn fixture() -> (EditorState, BufferId, InputState) {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state.open_text(TextBuffer::from_text("x x"), None).unwrap();
        (state, buffer, InputState::new(100))
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
            view: state.active_view(),
        }
    }
    fn stops(source: &str) -> Vec<SnippetTabstop> {
        crate::language::lsp::parse_snippet(source, 1024)
            .unwrap()
            .tabstops
    }
    fn ranges(input: &InputState, group: usize) -> Vec<ByteRange> {
        input.snippet.as_ref().unwrap().groups[group]
            .iter()
            .map(SnippetTabstop::range)
            .collect()
    }
    fn edit_snippet(
        state: &mut EditorState,
        buffer: BufferId,
        input: &mut InputState,
        start: usize,
        end: usize,
        text: &str,
    ) {
        let source = state.window_version();
        let mut transaction =
            EditorTransaction::new(TransactionId(1)).with_operation(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    state.buffers[buffer].text.revision(),
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(start), ByteOffset(end)).unwrap(),
                        text,
                    )],
                ),
            });
        let update = input
            .prepare_snippet_mirror(Some(document(state, buffer, &source)), &mut transaction)
            .unwrap();
        state.apply_transaction(&transaction).unwrap();
        let source = state.window_version();
        assert!(input.accept_snippet_edit(update, Some(document(state, buffer, &source))));
    }
    pub(in crate::app) fn install(source: &str) -> (EditorState, BufferId, InputState) {
        let parsed = crate::language::lsp::parse_snippet(source, 1024).unwrap();
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state
            .open_text(TextBuffer::from_text(&parsed.text), None)
            .unwrap();
        let mut input = InputState::new(100);
        let version = state.window_version();
        input.install_snippet(
            document(&state, buffer, &version),
            ByteOffset(0),
            parsed.tabstops,
        );
        input.install_snippet_transforms(ByteOffset(0), parsed.transforms);
        (state, buffer, input)
    }
    fn transform_ranges(input: &InputState) -> Vec<ByteRange> {
        input
            .snippet
            .as_ref()
            .unwrap()
            .transforms
            .iter()
            .map(SnippetTransform::range)
            .collect()
    }
    #[test]
    fn transformed_copies_follow_typing_in_their_tabstop() {
        let (mut state, buffer, mut input) =
            install("const [${1:state}, set${1/(.*)/${1:/capitalize}/}] = useState($2)");
        assert_eq!(
            state.buffers[buffer].text.to_string(),
            "const [state, setState] = useState()"
        );
        edit_snippet(&mut state, buffer, &mut input, 7, 12, "c");
        edit_snippet(&mut state, buffer, &mut input, 8, 8, "ount");
        assert_eq!(
            state.buffers[buffer].text.to_string(),
            "const [count, setCount] = useState()"
        );
        assert_eq!(
            transform_ranges(&input),
            vec![ByteRange::new(ByteOffset(17), ByteOffset(22)).unwrap()]
        );
        input.navigate_snippet(1);
        assert_eq!(ranges(&input, 1)[0], ByteRange::empty(ByteOffset(35)));
        edit_snippet(&mut state, buffer, &mut input, 35, 35, "0");
        assert_eq!(
            state.buffers[buffer].text.to_string(),
            "const [count, setCount] = useState(0)"
        );
    }
    #[test]
    fn transformed_copies_before_their_tabstop_move_it_and_the_cursor() {
        let mut state = EditorState::new("/workspace".into());
        let parsed =
            crate::language::lsp::parse_snippet("${1/(.*)/${1:/upcase}/}: ${1:x}", 1024).unwrap();
        let (buffer, view) = state
            .open_text(TextBuffer::from_text(&parsed.text), None)
            .unwrap();
        state.views[view].mode = crate::editor::EditorMode::Insert;
        let mut input = InputState::new(100);
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            parsed.tabstops,
        );
        input.install_snippet_transforms(ByteOffset(0), parsed.transforms);
        let mut transaction = EditorTransaction::new(TransactionId(1))
            .with_operation(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    state.buffers[buffer].text.revision(),
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(3), ByteOffset(4)).unwrap(),
                        "abc",
                    )],
                ),
            })
            .with_operation(EditorOperation::SetCursor {
                view,
                position: ByteOffset(6),
            });
        let update = input
            .prepare_snippet_mirror(Some(document(&state, buffer, &source)), &mut transaction)
            .unwrap();
        state.apply_transaction(&transaction).unwrap();
        let source = state.window_version();
        assert!(input.accept_snippet_edit(update, Some(document(&state, buffer, &source))));
        assert_eq!(state.buffers[buffer].text.to_string(), "ABC: abc");
        assert_eq!(state.views[view].cursor, ByteOffset(8));
        assert_eq!(
            ranges(&input, 0)[0],
            ByteRange::new(ByteOffset(5), ByteOffset(8)).unwrap()
        );
    }
    #[test]
    fn empty_transformed_copies_stay_after_an_empty_tabstop() {
        let (mut state, buffer, mut input) = install("$1${1/(.+)/[$1]/}$0");
        edit_snippet(&mut state, buffer, &mut input, 0, 0, "x");
        assert_eq!(state.buffers[buffer].text.to_string(), "x[x]");
        assert_eq!(
            ranges(&input, 0)[0],
            ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap()
        );
        edit_snippet(&mut state, buffer, &mut input, 1, 1, "y");
        assert_eq!(state.buffers[buffer].text.to_string(), "xy[xy]");
        assert_eq!(ranges(&input, 1)[0], ByteRange::empty(ByteOffset(6)));
        edit_snippet(&mut state, buffer, &mut input, 0, 2, "");
        assert_eq!(state.buffers[buffer].text.to_string(), "");
        assert_eq!(
            transform_ranges(&input),
            vec![ByteRange::empty(ByteOffset(0))]
        );
    }
    #[test]
    fn replacing_a_placeholder_that_holds_a_transformed_copy_stops_tracking_it() {
        let (mut state, buffer, mut input) = install("${1:a} ${2:<${1/(.*)/$1/}>}");
        assert_eq!(state.buffers[buffer].text.to_string(), "a <a>");
        input.navigate_snippet(1);
        edit_snippet(&mut state, buffer, &mut input, 2, 5, "z");
        assert_eq!(state.buffers[buffer].text.to_string(), "a z");
        assert!(transform_ranges(&input).is_empty());
        input.navigate_snippet(-1);
        edit_snippet(&mut state, buffer, &mut input, 0, 1, "b");
        assert_eq!(state.buffers[buffer].text.to_string(), "b z");
    }
    #[test]
    fn typing_at_the_end_of_a_placeholder_holding_a_transformed_copy_goes_after_it() {
        let (mut state, buffer, mut input) = install("${1:a} ${2:${1/(.*)/$1/}}");
        assert_eq!(state.buffers[buffer].text.to_string(), "a a");
        input.navigate_snippet(1);
        edit_snippet(&mut state, buffer, &mut input, 3, 3, "!");
        assert_eq!(state.buffers[buffer].text.to_string(), "a a!");
        input.navigate_snippet(-1);
        edit_snippet(&mut state, buffer, &mut input, 0, 1, "bc");
        assert_eq!(state.buffers[buffer].text.to_string(), "bc bc!");
        assert_eq!(
            ranges(&input, 1)[0],
            ByteRange::new(ByteOffset(3), ByteOffset(6)).unwrap()
        );
    }
    #[test]
    fn adjacent_nonempty_mirrors_keep_separate_ranges_at_insertion_boundaries() {
        for at in [0, 1] {
            let mut state = EditorState::new("/workspace".into());
            let (buffer, _) = state.open_text(TextBuffer::from_text("aa"), None).unwrap();
            let mut input = InputState::new(100);
            let source = state.window_version();
            input.install_snippet(
                document(&state, buffer, &source),
                ByteOffset(0),
                stops("${1:a}$1"),
            );
            edit_snippet(&mut state, buffer, &mut input, at, at, "x");
            assert_eq!(
                ranges(&input, 0),
                vec![
                    ByteRange::new(ByteOffset(0), ByteOffset(2)).unwrap(),
                    ByteRange::new(ByteOffset(2), ByteOffset(4)).unwrap(),
                ]
            );
            edit_snippet(&mut state, buffer, &mut input, 0, 2, "z");
            assert_eq!(state.buffers[buffer].text.to_string(), "zz");
        }
    }
    #[test]
    fn coincident_unrelated_placeholders_are_not_treated_as_parents() {
        let snippet = "${1:}${2:$3} / $1 / $2";
        let parsed = crate::language::lsp::parse_snippet(snippet, 1024).unwrap();
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state
            .open_text(TextBuffer::from_text(&parsed.text), None)
            .unwrap();
        let mut input = InputState::new(100);
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            parsed.tabstops,
        );
        input.navigate_snippet(1);
        input.navigate_snippet(1);
        edit_snippet(&mut state, buffer, &mut input, 0, 0, "x");
        assert_eq!(state.buffers[buffer].text.to_string(), "x /  / x");
        assert_eq!(ranges(&input, 0)[0], ByteRange::empty(ByteOffset(0)));
    }
    #[test]
    fn parent_mirrors_only_copy_child_occurrences_inside_the_parent() {
        for (snippet, expected) in [("${1:$2}$2 $1", "xx x"), ("$2 ${1:${2:foo}} $1", "x x x")] {
            let parsed = crate::language::lsp::parse_snippet(snippet, 1024).unwrap();
            let mut state = EditorState::new("/workspace".into());
            let (buffer, _) = state
                .open_text(TextBuffer::from_text(&parsed.text), None)
                .unwrap();
            let mut input = InputState::new(100);
            let source = state.window_version();
            input.install_snippet(
                document(&state, buffer, &source),
                ByteOffset(0),
                parsed.tabstops,
            );
            input.navigate_snippet(1);
            let end = input.snippet.as_ref().unwrap().groups[1][0].end;
            edit_snippet(&mut state, buffer, &mut input, 0, end, "x");
            assert_eq!(state.buffers[buffer].text.to_string(), expected);
        }
    }
    #[test]
    fn nested_edits_shift_explicit_cursors_past_earlier_parent_mirrors() {
        let parsed = crate::language::lsp::parse_snippet("$1 = ${1:${2:foo}}", 1024).unwrap();
        let mut state = EditorState::new("/workspace".into());
        let (buffer, view) = state
            .open_text(TextBuffer::from_text(&parsed.text), None)
            .unwrap();
        state.views[view].mode = crate::editor::EditorMode::Insert;
        let mut input = InputState::new(100);
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            parsed.tabstops,
        );
        input.navigate_snippet(1);
        let mut transaction = EditorTransaction::new(TransactionId(1))
            .with_operation(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    state.buffers[buffer].text.revision(),
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(6), ByteOffset(9)).unwrap(),
                        "x",
                    )],
                ),
            })
            .with_operation(EditorOperation::SetCursor {
                view,
                position: ByteOffset(7),
            });
        let update = input
            .prepare_snippet_mirror(Some(document(&state, buffer, &source)), &mut transaction)
            .unwrap();
        state.apply_transaction(&transaction).unwrap();
        let source = state.window_version();
        assert!(input.accept_snippet_edit(update, Some(document(&state, buffer, &source))));
        assert_eq!(state.buffers[buffer].text.to_string(), "x = x");
        assert_eq!(state.views[view].cursor, ByteOffset(5));
    }
    #[test]
    fn replacing_a_parent_removes_its_empty_stop_at_the_end() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state.open_text(TextBuffer::from_text("abc"), None).unwrap();
        let mut input = InputState::new(100);
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            stops("${1:abc$2}$0"),
        );
        edit_snippet(&mut state, buffer, &mut input, 0, 3, "x");
        assert_eq!(input.snippet.as_ref().unwrap().groups.len(), 2);
        assert_eq!(input.snippet.as_ref().unwrap().groups[1][0].index, 0);
    }
    #[test]
    fn nested_snippet_edits_update_enclosing_mirrors() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state
            .open_text(TextBuffer::from_text("foo = foo"), None)
            .unwrap();
        let mut input = InputState::new(100);
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            stops("${1:${2:foo}} = $1$0"),
        );
        input.navigate_snippet(1);
        edit_snippet(&mut state, buffer, &mut input, 0, 3, "x");
        assert_eq!(state.buffers[buffer].text.to_string(), "x = x");
        assert_eq!(
            ranges(&input, 0),
            vec![
                ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
                ByteRange::new(ByteOffset(4), ByteOffset(5)).unwrap(),
            ]
        );
        input.navigate_snippet(-1);
        edit_snippet(&mut state, buffer, &mut input, 0, 1, "long");
        assert_eq!(state.buffers[buffer].text.to_string(), "long = long");
    }
    #[test]
    fn enclosing_snippet_mirrors_follow_nesting_not_tabstop_numbers() {
        for (snippet, initial) in [
            ("${2:${1:foo}} = $2", "foo = foo"),
            ("${2:${1:}} = $2", " = "),
        ] {
            let mut state = EditorState::new("/workspace".into());
            let (buffer, _) = state
                .open_text(TextBuffer::from_text(initial), None)
                .unwrap();
            let mut input = InputState::new(100);
            let source = state.window_version();
            input.install_snippet(
                document(&state, buffer, &source),
                ByteOffset(0),
                stops(snippet),
            );
            let end = input.snippet.as_ref().unwrap().groups[0][0].end;
            edit_snippet(&mut state, buffer, &mut input, 0, end, "x");
            assert_eq!(state.buffers[buffer].text.to_string(), "x = x");
            assert_eq!(
                ranges(&input, 1)[0],
                ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap()
            );
        }
    }
    #[test]
    fn coincident_snippet_stops_follow_source_order_not_tabstop_numbers() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state.open_text(TextBuffer::from_text(""), None).unwrap();
        let mut input = InputState::new(100);
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            stops("$2$1"),
        );
        edit_snippet(&mut state, buffer, &mut input, 0, 0, "x");
        assert_eq!(ranges(&input, 1)[0], ByteRange::empty(ByteOffset(0)));
    }
    #[test]
    fn snippet_mirroring_preserves_an_interior_deletion_cursor() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, view) = state
            .open_text(TextBuffer::from_text("foo foo"), None)
            .unwrap();
        state.views[view].mode = crate::editor::EditorMode::Insert;
        let mut input = InputState::new(100);
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            stops("${1:foo} $1"),
        );
        edit_snippet(&mut state, buffer, &mut input, 0, 3, "abcd");
        state.views[view].cursor = ByteOffset(2);
        edit_snippet(&mut state, buffer, &mut input, 2, 3, "");
        assert_eq!(state.buffers[buffer].text.to_string(), "abd abd");
        assert_eq!(state.views[view].cursor, ByteOffset(2));
    }
    #[test]
    fn replacing_a_parent_snippet_removes_overwritten_nested_stops() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state
            .open_text(TextBuffer::from_text("f(abc)"), None)
            .unwrap();
        let mut input = InputState::new(100);
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            stops("f(${1:a${2:b}c})$0"),
        );
        edit_snippet(&mut state, buffer, &mut input, 2, 5, "x");
        assert_eq!(state.buffers[buffer].text.to_string(), "f(x)");
        input.navigate_snippet(1);
        let source = state.window_version();
        let operations = input
            .snippet_selection(Some(document(&state, buffer, &source)))
            .unwrap();
        assert!(matches!(
            operations[0],
            EditorOperation::SetCursor {
                position: ByteOffset(4),
                ..
            }
        ));
        assert!(matches!(
            operations[1],
            EditorOperation::SetSelection {
                selection: None,
                ..
            }
        ));
    }
    #[test]
    fn editing_a_later_empty_snippet_stop_keeps_the_earlier_one_before_it() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state.open_text(TextBuffer::from_text(""), None).unwrap();
        let mut input = InputState::new(100);
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            stops("$1$2"),
        );
        input.navigate_snippet(1);
        edit_snippet(&mut state, buffer, &mut input, 0, 0, "x");
        input.navigate_snippet(-1);
        let source = state.window_version();
        let operations = input
            .snippet_selection(Some(document(&state, buffer, &source)))
            .unwrap();
        assert!(matches!(
            operations[0],
            EditorOperation::SetCursor {
                position: ByteOffset(0),
                ..
            }
        ));
        assert_eq!(state.buffers[buffer].text.to_string(), "x");
    }
    #[test]
    fn typing_into_an_empty_placeholder_keeps_it_around_the_typed_text() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state
            .open_text(TextBuffer::from_text("fn () {}"), None)
            .unwrap();
        let mut input = InputState::new(100);
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            stops("fn $1($2) {}"),
        );
        for (at, typed) in [(3, "q"), (4, "a")] {
            let source = state.window_version();
            let position = ByteOffset(at);
            let mut transaction = EditorTransaction::new(TransactionId(at as u64)).with_operation(
                EditorOperation::ApplyEdits {
                    buffer,
                    edits: TextEditSet::new(
                        state.buffers[buffer].text.revision(),
                        vec![TextEdit::new(
                            ByteRange::new(position, position).unwrap(),
                            typed,
                        )],
                    ),
                },
            );
            let update = input
                .prepare_snippet_mirror(Some(document(&state, buffer, &source)), &mut transaction)
                .unwrap();
            state.apply_transaction(&transaction).unwrap();
            let source = state.window_version();
            assert!(input.accept_snippet_edit(update, Some(document(&state, buffer, &source))));
        }
        assert_eq!(state.buffers[buffer].text.to_string(), "fn qa() {}");
        let session = input.snippet.as_ref().unwrap();
        assert_eq!(
            session.groups[0][0].range(),
            ByteRange::new(ByteOffset(3), ByteOffset(5)).unwrap(),
            "going back selects exactly the name"
        );
        assert_eq!(
            session.groups[1][0].range(),
            ByteRange::new(ByteOffset(6), ByteOffset(6)).unwrap(),
            "the next placeholder stays inside the parentheses"
        );
    }
    #[test]
    fn mirrors_are_input_owned_and_only_the_matching_post_commit_reply_advances_ranges() {
        let (mut state, buffer, mut input) = fixture();
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            stops("${1:x} $1"),
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
            input.snippet.as_ref().unwrap().groups[0][1].range(),
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
            stops("${1:x} x"),
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
            stops("${1:z} x"),
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
    #[test]
    fn adjacent_empty_placeholder_does_not_expand_the_previous_one() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state.open_text(TextBuffer::from_text("a"), None).unwrap();
        let mut input = InputState::new(100);
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            stops("${1:a}$2$0"),
        );
        input.navigate_snippet(1);
        let mut transaction =
            EditorTransaction::new(TransactionId(1)).with_operation(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    state.buffers[buffer].text.revision(),
                    vec![TextEdit::new(ByteRange::empty(ByteOffset(1)), "b")],
                ),
            });
        let update = input
            .prepare_snippet_mirror(Some(document(&state, buffer, &source)), &mut transaction)
            .unwrap();
        state.apply_transaction(&transaction).unwrap();
        let source = state.window_version();
        assert!(input.accept_snippet_edit(update, Some(document(&state, buffer, &source))));
        assert_eq!(
            input.snippet.as_ref().unwrap().groups[0][0].range(),
            ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap()
        );
        assert_eq!(state.buffers[buffer].text.to_string(), "ab");
    }

    #[test]
    fn coincident_empty_mirrors_insert_once_per_occurrence() {
        let mut state = EditorState::new("/workspace".into());
        let (buffer, _) = state.open_text(TextBuffer::from_text("[]"), None).unwrap();
        let mut input = InputState::new(100);
        let source = state.window_version();
        input.install_snippet(
            document(&state, buffer, &source),
            ByteOffset(0),
            stops("[$1$1]"),
        );
        let mut transaction =
            EditorTransaction::new(TransactionId(1)).with_operation(EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    state.buffers[buffer].text.revision(),
                    vec![TextEdit::new(ByteRange::empty(ByteOffset(1)), "x")],
                ),
            });
        let update = input
            .prepare_snippet_mirror(Some(document(&state, buffer, &source)), &mut transaction)
            .unwrap();
        state.apply_transaction(&transaction).unwrap();
        let source = state.window_version();
        assert!(input.accept_snippet_edit(update, Some(document(&state, buffer, &source))));
        assert_eq!(state.buffers[buffer].text.to_string(), "[xx]");
        assert_eq!(
            ranges(&input, 0),
            vec![
                ByteRange::new(ByteOffset(1), ByteOffset(2)).unwrap(),
                ByteRange::new(ByteOffset(2), ByteOffset(3)).unwrap()
            ]
        );
    }
}
