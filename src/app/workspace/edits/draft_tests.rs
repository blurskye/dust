//! Tests for checked workspace edit drafts.

use crate::editor::DocumentSnapshot as FileSnapshot;
use crate::editor::EditorOperation;
use crate::editor::EditorState;
use crate::editor::EditorTransaction;
use crate::editor::TransactionId;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::TextBuffer;
use crate::editor::text::TextEdit;
use crate::editor::text::TextEditSet;
use crate::editor::text::diff::changed_spans;
use crate::workspace::ProposalId;
use crate::workspace::WorkspaceEditDraft;

use crate::workspace::WorkspaceId;
use crate::workspace::WorkspaceState;

#[test]
fn workspace_edits_preserve_preview_bytes_and_undo_mixed_endings() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("mixed.txt");
    let scope = WorkspaceState::new(
        WorkspaceId(1),
        [(
            "root".into(),
            crate::files::RootIdentity::capture(root.path()).unwrap(),
        )],
    )
    .unwrap();
    let original = b"\xef\xbb\xbfa\r\nb\n";
    let mut editor = EditorState::new(root.path().into());
    let (buffer, _) = editor
        .open_text(
            TextBuffer::from_bytes(original).unwrap(),
            Some(path.clone()),
        )
        .unwrap();
    let revision = editor.buffers[buffer].text.revision();
    let proposal = WorkspaceEditProposal::build(
        ProposalId(1),
        "replace".into(),
        &scope,
        vec![WorkspaceEditDraft {
            canonical_path: path.clone(),
            expected_revision: Some(revision),
            edits: vec![TextEdit::new(
                ByteRange::new(ByteOffset(0), ByteOffset(4)).unwrap(),
                "A\nb\n",
            )],
        }],
        vec![FileSnapshot {
            canonical_path: path,
            revision: Some(revision),
            bytes: original.to_vec(),
        }],
    )
    .unwrap();
    let sources = editor.workspace_edit_sources().unwrap();
    let draft = crate::app::workspace::edits::prepare_workspace_edit(
        &editor,
        &sources,
        &proposal,
        TransactionId(1),
    )
    .unwrap();
    crate::app::workspace::edits::workspace_commit_check(&draft, &proposal).unwrap();
    editor = draft.accept(&editor.version()).unwrap();
    assert_eq!(
        editor.buffers[buffer].text.to_bytes(),
        proposal.files[0].proposed_bytes
    );
    assert_eq!(
        editor.buffers[buffer].text.to_bytes(),
        b"\xef\xbb\xbfA\nb\n"
    );
    editor
        .apply_transaction(
            &EditorTransaction::new(TransactionId(2))
                .with_operation(EditorOperation::Undo { buffer }),
        )
        .unwrap();
    assert_eq!(editor.buffers[buffer].text.to_bytes(), original);

    // A draft with the same normalized text but different separators is not the preview.
    let mut wrong = editor.draft();
    assert!(wrong.reload_document(
        buffer,
        TextBuffer::from_bytes(b"\xef\xbb\xbfA\r\nb\n").unwrap(),
        editor.buffers[buffer].path.clone().unwrap()
    ));
    assert!(crate::app::workspace::edits::workspace_commit_check(&wrong, &proposal).is_err());
}

#[test]
fn workspace_edits_use_preview_separators_and_undo_separator_only_changes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("endings.txt");
    let scope = WorkspaceState::new(
        WorkspaceId(1),
        [(
            "root".into(),
            crate::files::RootIdentity::capture(root.path()).unwrap(),
        )],
    )
    .unwrap();
    for separator_only in [false, true] {
        let mut editor = EditorState::new(root.path().into());
        let original: &[u8] = if separator_only {
            b"a\r\nb\n"
        } else {
            b"a\r\nb\r\nc\n"
        };
        let (buffer, _) = editor
            .open_text(
                TextBuffer::from_bytes(original).unwrap(),
                Some(path.clone()),
            )
            .unwrap();
        if !separator_only {
            editor
                .apply_transaction(&EditorTransaction::new(TransactionId(1)).with_operation(
                    EditorOperation::ApplyEdits {
                        buffer,
                        edits: TextEditSet::new(
                            editor.buffers[buffer].text.revision(),
                            vec![TextEdit::new(
                                ByteRange::new(ByteOffset(0), ByteOffset(4)).unwrap(),
                                "",
                            )],
                        ),
                    },
                ))
                .unwrap();
        }
        let before = editor.buffers[buffer].text.to_bytes();
        let preference = editor.buffers[buffer].text.format().preferred_line_ending;
        let revision = editor.buffers[buffer].text.revision();
        let replacement = if separator_only {
            "a\nb\n"
        } else {
            "C\nmore\n"
        };
        let proposal = WorkspaceEditProposal::build(
            ProposalId(1),
            "replace".into(),
            &scope,
            vec![WorkspaceEditDraft {
                canonical_path: path.clone(),
                expected_revision: Some(revision),
                edits: vec![TextEdit::new(
                    ByteRange::new(
                        ByteOffset(0),
                        ByteOffset(editor.buffers[buffer].text.len_bytes()),
                    )
                    .unwrap(),
                    replacement,
                )],
            }],
            vec![FileSnapshot {
                canonical_path: path.clone(),
                revision: Some(revision),
                bytes: before.clone(),
            }],
        )
        .unwrap();
        let draft = crate::app::workspace::edits::prepare_workspace_edit(
            &editor,
            &editor.workspace_edit_sources().unwrap(),
            &proposal,
            TransactionId(2),
        )
        .unwrap();
        crate::app::workspace::edits::workspace_commit_check(&draft, &proposal).unwrap();
        editor = draft.accept(&editor.version()).unwrap();
        assert_eq!(
            editor.buffers[buffer].text.to_bytes(),
            proposal.files[0].proposed_bytes
        );
        assert_eq!(
            editor.buffers[buffer].text.format().preferred_line_ending,
            preference
        );
        editor
            .apply_transaction(
                &EditorTransaction::new(TransactionId(3))
                    .with_operation(EditorOperation::Undo { buffer }),
            )
            .unwrap();
        assert_eq!(editor.buffers[buffer].text.to_bytes(), before);
        editor
            .apply_transaction(
                &EditorTransaction::new(TransactionId(4))
                    .with_operation(EditorOperation::Redo { buffer }),
            )
            .unwrap();
        assert_eq!(
            editor.buffers[buffer].text.to_bytes(),
            proposal.files[0].proposed_bytes
        );
    }
}

#[test]
fn workspace_edits_keep_cursors_between_disjoint_replacements() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("words.txt");
    let scope = WorkspaceState::new(
        WorkspaceId(1),
        [(
            "root".into(),
            crate::files::RootIdentity::capture(root.path()).unwrap(),
        )],
    )
    .unwrap();
    let mut editor = EditorState::new(root.path().into());
    let (buffer, view) = editor
        .open_text(TextBuffer::from_text("x keep x\n"), Some(path.clone()))
        .unwrap();
    let sibling = editor
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .unwrap();
    editor
        .apply_transaction(
            &EditorTransaction::new(TransactionId(1))
                .with_operation(EditorOperation::SetCursor {
                    view,
                    position: ByteOffset(3),
                })
                .with_operation(EditorOperation::SetCursor {
                    view: sibling,
                    position: ByteOffset(5),
                }),
        )
        .unwrap();
    let revision = editor.buffers[buffer].text.revision();
    let proposal = WorkspaceEditProposal::build(
        ProposalId(1),
        "replace".into(),
        &scope,
        vec![WorkspaceEditDraft {
            canonical_path: path.clone(),
            expected_revision: Some(revision),
            edits: vec![
                TextEdit::new(
                    ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
                    "name",
                ),
                TextEdit::new(
                    ByteRange::new(ByteOffset(7), ByteOffset(8)).unwrap(),
                    "name",
                ),
            ],
        }],
        vec![FileSnapshot {
            canonical_path: path,
            revision: Some(revision),
            bytes: b"x keep x\n".to_vec(),
        }],
    )
    .unwrap();
    let draft = crate::app::workspace::edits::prepare_workspace_edit(
        &editor,
        &editor.workspace_edit_sources().unwrap(),
        &proposal,
        TransactionId(2),
    )
    .unwrap();
    editor = draft.accept(&editor.version()).unwrap();
    assert_eq!(editor.buffers[buffer].text.to_string(), "name keep name\n");
    assert_eq!(editor.views[view].cursor, ByteOffset(6));
    assert_eq!(editor.views[sibling].cursor, ByteOffset(8));
}

#[test]
fn only_what_differs_is_replaced() {
    let before = "let old = 1;\nkeep\nuse(old);\n";
    let after = "let name = 1;\nkeep\nuse(name);\n";
    assert_eq!(
        changed_spans(before, after),
        vec![(4..7, "name".to_owned()), (22..25, "name".to_owned())]
    );
    assert_eq!(
        changed_spans("a\nb\n", "new\na\nb\n"),
        vec![(0..0, "new\n".to_owned())]
    );
    assert_eq!(
        changed_spans("a\nb", "a\nb\nc\n"),
        vec![(3..3, "\nc\n".to_owned())]
    );
    assert_eq!(changed_spans("é\n", "è\n"), vec![(0..2, "è".to_owned())]);
}

#[test]
fn identical_text_needs_no_edit() {
    assert!(changed_spans("a\nb\n", "a\nb\n").is_empty());
}

#[test]
fn text_a_line_diff_cannot_describe_is_replaced_whole() {
    assert_eq!(
        changed_spans("a\0b\n", "a\0c\n"),
        vec![(0..4, "a\0c\n".to_owned())]
    );
}

use crate::workspace::WorkspaceEditProposal;
