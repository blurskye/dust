//! Tests for workspace edit preparation and approval.

use crate::app::workspace::edits::*;
use crate::editor::EditorState;
use crate::editor::TransactionId;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::TextBuffer;
use crate::editor::text::TextEdit;
#[test]
fn workspace_approval_restores_unopened_file_history_on_a_worker() {
    use crate::editor::EditorOperation;
    use crate::editor::EditorTransaction;
    use crate::editor::text::TextEditSet;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.txt");
    let history = root.path().join("undo");
    let maximum = 1_000_000;
    let mut recorded =
        crate::editor::Buffer::new(TextBuffer::from_text("before"), Some(path.clone()));
    let mut recording_editor = EditorState::new(root.path().into());
    let (buffer, _) = recording_editor
        .open_text(recorded.text.clone(), Some(path.clone()))
        .unwrap();
    recording_editor
        .apply_transaction(&EditorTransaction::new(TransactionId(1)).with_operation(
            EditorOperation::ApplyEdits {
                buffer,
                edits: TextEditSet::new(
                    recorded.text.revision(),
                    vec![TextEdit::new(
                        ByteRange::new(ByteOffset(0), ByteOffset(6)).unwrap(),
                        "old",
                    )],
                ),
            },
        ))
        .unwrap();
    recorded = recording_editor.buffers[buffer].clone();
    crate::editor::save_undo_history(
        history.clone(),
        path.clone(),
        recorded.text.clone(),
        recorded.undo,
        maximum,
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while crate::editor::load_undo_history(&history, &path, &recorded.text, maximum).is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "history save did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let scope = WorkspaceState::new(
        crate::workspace::WorkspaceId(1),
        [(
            "root".into(),
            crate::files::RootIdentity::capture(root.path()).unwrap(),
        )],
    )
    .unwrap();
    let workspace = crate::app::workspace::Workspace::new(
        scope.clone(),
        root.path().join("trust.json"),
        1,
        false,
        false,
        1,
    );
    let version = workspace.version();
    let mut editor = EditorState::new(root.path().into());
    editor.undo_directory = Some(history.clone());
    editor.undo_maximum_bytes = maximum;
    let mut owner = WorkspaceEdits::default();
    let ticket = owner
        .begin(version.clone(), 1, editor.workspace_edit_sources().unwrap())
        .unwrap();
    owner
        .build(
            PreparedEdits {
                ticket,
                label: "replace".into(),
                drafts: vec![WorkspaceEditDraft {
                    canonical_path: path.clone(),
                    expected_revision: None,
                    edits: vec![TextEdit::new(
                        ByteRange::new(ByteOffset(0), ByteOffset(3)).unwrap(),
                        "NEW",
                    )],
                }],
                snapshots: vec![FileSnapshot {
                    canonical_path: path.clone(),
                    revision: None,
                    bytes: b"old".to_vec(),
                }],
                resources: Vec::new(),
            },
            &scope,
            &version,
            1,
        )
        .unwrap();
    assert!(
        owner
            .prepare_approval(&editor, TransactionId(2), Vec::new(), &version, 1)
            .unwrap()
            .is_none()
    );
    assert!(editor.buffers.is_empty());
    assert!(owner.toggle(0).is_err());
    assert!(
        owner
            .prepare_approval(&editor, TransactionId(3), Vec::new(), &version, 1)
            .is_err()
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let (draft, changes) = loop {
        if let Some(result) = owner.poll_approval(&version, 1) {
            break result.unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "workspace preparation did not finish"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    // Publication needs no further access to the history directory.
    std::fs::remove_dir_all(history).unwrap();
    owner.stage_approval(&draft, changes).unwrap();
    editor = draft.accept(&editor.version()).unwrap();
    owner.accepted(editor.publication()).unwrap();
    let buffer = editor.buffers.keys().next().unwrap();
    assert_eq!(editor.buffers[buffer].text.to_string(), "NEW");
    for (id, expected) in [(4, "old"), (5, "before")] {
        editor
            .apply_transaction(
                &EditorTransaction::new(TransactionId(id))
                    .with_operation(EditorOperation::Undo { buffer }),
            )
            .unwrap();
        assert_eq!(editor.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn workspace_preview_owns_selection_and_rejects_stale_or_replaced_sources_before_clone() {
    let root = tempfile::tempdir().unwrap();
    let scope = WorkspaceState::new(
        crate::workspace::WorkspaceId(1),
        [(
            "root".into(),
            crate::files::RootIdentity::capture(root.path()).unwrap(),
        )],
    )
    .unwrap();
    let workspace = crate::app::workspace::Workspace::new(
        scope.clone(),
        root.path().join("trust.json"),
        1,
        false,
        false,
        1,
    );
    let version = workspace.version();
    let path = root.path().join("open");
    let hidden = root.path().join("hidden");
    let mut editor = EditorState::new(root.path().into());
    let (buffer, _) = editor
        .open_text(TextBuffer::from_text("old"), Some(path.clone()))
        .unwrap();
    let prepared = |ticket| PreparedEdits {
        ticket,
        label: "preview".into(),
        drafts: vec![
            WorkspaceEditDraft {
                canonical_path: hidden.clone(),
                expected_revision: None,
                edits: vec![TextEdit::new(
                    ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
                    "H",
                )],
            },
            WorkspaceEditDraft {
                canonical_path: path.clone(),
                expected_revision: Some(crate::editor::text::BufferRevision(0)),
                edits: vec![TextEdit::new(
                    ByteRange::new(ByteOffset(0), ByteOffset(3)).unwrap(),
                    "NEW",
                )],
            },
        ],
        snapshots: vec![
            FileSnapshot {
                canonical_path: hidden.clone(),
                revision: None,
                bytes: b"h".to_vec(),
            },
            FileSnapshot {
                canonical_path: path.clone(),
                revision: Some(crate::editor::text::BufferRevision(0)),
                bytes: b"old".to_vec(),
            },
        ],
        resources: Vec::new(),
    };
    let mut owner = WorkspaceEdits::default();
    let invalidated = owner
        .begin(version.clone(), 1, editor.workspace_edit_sources().unwrap())
        .unwrap();
    owner.reject();
    let id = owner
        .begin(version.clone(), 1, editor.workspace_edit_sources().unwrap())
        .unwrap();
    assert!(id.id() > invalidated.id());
    let before = TextBuffer::test_materializations();
    assert!(
        owner
            .build(prepared(invalidated), &scope, &version, 1)
            .is_err()
    );
    let mut foreign_owner = WorkspaceEdits::default();
    let foreign_ticket = foreign_owner
        .begin(version.clone(), 1, editor.workspace_edit_sources().unwrap())
        .unwrap();
    assert!(
        owner
            .build(prepared(foreign_ticket), &scope, &version, 1)
            .is_err()
    );
    assert_eq!(TextBuffer::test_materializations(), before);
    owner.build(prepared(id), &scope, &version, 1).unwrap();
    owner.toggle(0).unwrap();
    assert!(!owner.proposal().unwrap().files[0].selected);
    owner.toggle(0).unwrap();
    assert!(owner.toggle(99).is_err());
    assert!(owner.approval(&version, 2).is_err());
    editor.buffers[buffer].read_only = true;
    let (sources, preview) = owner.approval(&version, 1).unwrap();
    let copies = EditorState::test_clone_count();
    let materializations = TextBuffer::test_materializations();
    assert!(
        crate::app::workspace::edits::prepare_workspace_edit(
            &editor,
            sources,
            preview,
            TransactionId(1)
        )
        .is_err()
    );
    assert_eq!(editor.buffers.len(), 1); // earlier unopened target never leaks into live state
    assert_eq!(EditorState::test_clone_count(), copies);
    assert_eq!(TextBuffer::test_materializations(), materializations);
    editor.buffers[buffer].read_only = false;
    let change = editor.capture_path_change(&path).unwrap();
    editor.buffers[buffer].text.restore_text("dirty").unwrap();
    editor.set_buffer_dirty(buffer, true);
    assert!(
        editor
            .validate_resource_change(&change, b"old", None)
            .is_err()
    );
    assert!(
        crate::app::workspace::edits::prepare_workspace_edit(
            &editor,
            sources,
            preview,
            TransactionId(2)
        )
        .is_err()
    );
    editor.replace_buffer_from_disk(buffer, TextBuffer::from_text("old"), path.clone());
    assert!(
        crate::app::workspace::edits::prepare_workspace_edit(
            &editor,
            sources,
            preview,
            TransactionId(3)
        )
        .is_err()
    );
    assert!(editor.workspace_resource_change(sources, &path).is_err());
    assert_eq!(EditorState::test_clone_count(), copies);
    owner.reject();
    assert!(owner.approval(&version, 1).is_err());
    let id = owner
        .begin(version.clone(), 1, editor.workspace_edit_sources().unwrap())
        .unwrap();
    owner.build(prepared(id), &scope, &version, 1).unwrap();
    let (sources, preview) = owner.approval(&version, 1).unwrap();
    let foreign = EditorState::new(root.path().into());
    assert!(
        crate::app::workspace::edits::prepare_workspace_edit(
            &foreign,
            sources,
            preview,
            TransactionId(4)
        )
        .is_err()
    );
    let draft = crate::app::workspace::edits::prepare_workspace_edit(
        &editor,
        sources,
        preview,
        TransactionId(5),
    )
    .unwrap();
    owner.stage_approval(&draft, Vec::new()).unwrap();
    assert!(owner.accepted(editor.publication()).is_err());
    editor = draft.accept(&editor.version()).unwrap();
    owner.accepted(editor.publication()).unwrap();
    assert!(owner.proposal().is_none());
    assert_eq!(editor.buffers[buffer].text.to_string(), "NEW");
    assert!(editor.buffers[buffer].dirty);
    assert_eq!(editor.buffers.len(), 2);
    assert!(
        editor
            .buffers
            .iter()
            .any(|(_, b)| b.path.as_ref() == Some(&hidden) && b.text.to_string() == "H" && b.dirty)
    );
    assert!(owner.accepted(editor.publication()).is_err());
    // A single real bounded document lane proves read/create submission, queue
    // refusal, cancellation, and a late terminal without reviving old preparation.
    use crate::app::documents::DocumentEvent;
    use crate::app::documents::Documents;
    use crate::app::startup::StartupTarget;
    use crate::files::VfsWorker;
    let disk = root.path().join("on-disk");
    std::fs::write(&disk, b"abc").unwrap();
    let created = root.path().join("new-file");
    let (vfs, release) = VfsWorker::start_gated_for_test(1);
    let mut documents = Documents::new(
        vfs,
        &StartupTarget::Dashboard {
            root: root.path().into(),
        },
        editor.window_version(),
        1,
        1,
    );
    let mut io = WorkspaceEdits::default();
    let ticket = io
        .begin(version.clone(), 1, editor.workspace_edit_sources().unwrap())
        .unwrap();
    io.start_files(
        ticket,
        "old".into(),
        None,
        false,
        vec![
            InputFile::Read(disk.clone()),
            InputFile::Create(created.clone()),
        ],
        (vec![root.path().into()], 1024),
    )
    .unwrap();
    let prepared = io.prepare_io(0, &workspace, &1, &editor).unwrap();
    documents.submit_workspace_io(prepared).unwrap();
    let prepared = io.prepare_io(1, &workspace, &1, &editor).unwrap();
    assert!(!documents.submit_workspace_io(prepared).unwrap());
    assert!(matches!(io.next_file(), Some((1, InputFile::Create(path))) if path == &created));
    assert_eq!(documents.pending_count(), 1);
    let invalidated = io.reject();
    assert_eq!(invalidated.requests.len(), 1);
    assert!(documents.cancel_request(invalidated.requests[0]));
    let ticket = io
        .begin(version.clone(), 1, editor.workspace_edit_sources().unwrap())
        .unwrap();
    io.start_files(
        ticket,
        "new".into(),
        None,
        false,
        vec![InputFile::Read(disk.clone())],
        (vec![root.path().into()], 1024),
    )
    .unwrap();
    release.send(()).unwrap();
    let wait = |documents: &mut Documents| {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if let Some(event) = documents.poll(&editor.window_version(), 1).unwrap() {
                break event;
            }
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    };
    assert!(matches!(wait(&mut documents), DocumentEvent::Discarded));
    assert!(io.preparing());
    let prepared = io.prepare_io(0, &workspace, &1, &editor).unwrap();
    documents.submit_workspace_io(prepared).unwrap();
    let mut foreign_documents = Documents::new(
        VfsWorker::start(2),
        &StartupTarget::Dashboard {
            root: root.path().into(),
        },
        editor.window_version(),
        1,
        2,
    );
    foreign_documents.read(disk.clone()).unwrap();
    assert!(matches!(
        wait(&mut foreign_documents),
        DocumentEvent::Legacy(_)
    ));
    let mut foreign_io = WorkspaceEdits::default();
    let ticket = foreign_io
        .begin(version.clone(), 1, editor.workspace_edit_sources().unwrap())
        .unwrap();
    foreign_io
        .start_files(
            ticket,
            "foreign worker".into(),
            None,
            false,
            vec![InputFile::Read(disk.clone())],
            (vec![root.path().into()], 1024),
        )
        .unwrap();
    let prepared = foreign_io.prepare_io(0, &workspace, &1, &editor).unwrap();
    foreign_documents.submit_workspace_io(prepared).unwrap();
    let DocumentEvent::WorkspaceIo(foreign_reply) = wait(&mut foreign_documents) else {
        panic!("foreign read");
    };
    assert_eq!(foreign_reply.id().0, invalidated.requests[0].0 + 1);
    assert!(
        !io.owns_preparation_reply(&foreign_reply),
        "equal VFS IDs do not substitute another worker"
    );
    let before = TextBuffer::test_materializations();
    assert!(
        io.accept_preparation_reply(foreign_reply, &version, 1)
            .is_err()
    );
    assert!(
        foreign
            .workspace_snapshot(io.preparing_sources().unwrap(), &path, 1024)
            .is_err()
    );
    assert_eq!(TextBuffer::test_materializations(), before);
    foreign_io.reject();
    foreign_documents.stop_worker();
    let DocumentEvent::WorkspaceIo(reply) = wait(&mut documents) else {
        panic!("owned read");
    };
    assert_eq!(
        reply.id().0,
        invalidated.requests[0].0 + 1,
        "full queue did not allocate another VFS ID"
    );
    assert!(io.owns_preparation_reply(&reply));
    assert!(!owner.owns_preparation_reply(&reply));
    io.accept_preparation_reply(reply, &version, 1).unwrap();
    assert!(io.files_ready());
    assert_eq!(
        io.prepared_files().unwrap().snapshot(&disk).unwrap().bytes,
        b"abc"
    );
    io.reject();
    let ticket = io
        .begin(version.clone(), 1, editor.workspace_edit_sources().unwrap())
        .unwrap();
    io.start_files(
        ticket,
        "create".into(),
        None,
        false,
        vec![InputFile::Create(created.clone())],
        (vec![root.path().into()], 1024),
    )
    .unwrap();
    let prepared = io.prepare_io(0, &workspace, &1, &editor).unwrap();
    documents.submit_workspace_io(prepared).unwrap();
    let DocumentEvent::WorkspaceIo(reply) = wait(&mut documents) else {
        panic!("owned create inspection");
    };
    io.accept_preparation_reply(reply, &version, 1).unwrap();
    assert!(io.files_ready());
    assert!(
        io.prepared_files()
            .unwrap()
            .snapshot(&created)
            .unwrap()
            .bytes
            .is_empty()
    );
    assert!(
        !created.exists(),
        "create inspection is not a disk mutation"
    );
    io.reject();
    documents.stop_accepting();
    let ticket = io
        .begin(version.clone(), 1, editor.workspace_edit_sources().unwrap())
        .unwrap();
    io.start_files(
        ticket,
        "closed".into(),
        None,
        false,
        vec![InputFile::Read(disk)],
        (vec![root.path().into()], 1024),
    )
    .unwrap();
    let prepared = io.prepare_io(0, &workspace, &1, &editor).unwrap();
    assert!(documents.submit_workspace_io(prepared).is_err());
    io.reject();
    documents.stop_worker();
    let resource = root.path().join("resource");
    let target = root.path().join("resource-renamed");
    std::fs::write(&resource, b"resource bytes").unwrap();
    let mut resources = WorkspaceEdits::default();
    let mut inspections = Documents::new(
        VfsWorker::start(2),
        &StartupTarget::Dashboard {
            root: root.path().into(),
        },
        editor.window_version(),
        1,
        2,
    );
    stage_resource(
        &mut resources,
        &mut inspections,
        &editor,
        &workspace,
        &scope,
        &resource,
        &target,
    );
    {
        // An unused loan neither consumes the stage nor submits work.
        let _prepared = resources
            .prepare_resource(&workspace, &1, &editor)
            .unwrap()
            .unwrap();
    }
    let (notices, requests) = resources.invalidate_resources("configuration changed");
    assert_eq!(notices.len(), 1);
    assert!(requests.is_empty());
    assert!(
        resources
            .prepare_resource(&workspace, &1, &editor)
            .unwrap()
            .is_none()
    );
    assert!(
        resources
            .invalidate_resources("configuration changed")
            .0
            .is_empty()
    );

    stage_resource(
        &mut resources,
        &mut inspections,
        &editor,
        &workspace,
        &scope,
        &resource,
        &target,
    );
    let (vfs, release) = VfsWorker::start_gated_for_test(2);
    let mut writes = Documents::new(
        vfs,
        &StartupTarget::Dashboard {
            root: root.path().into(),
        },
        editor.window_version(),
        1,
        2,
    );
    writes
        .submit_workspace_resource(
            resources
                .prepare_resource(&workspace, &1, &editor)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    let (_, requests) = resources.invalidate_resources("configuration changed");
    assert_eq!(requests.len(), 1);
    let cancelled = writes
        .cancel_workspace_queued(requests[0])
        .expect("queued cancellation");
    assert!(
        resources
            .resource_cancelled(cancelled)
            .unwrap()
            .contains("before execution")
    );
    assert!(writes.cancel_workspace_queued(requests[0]).is_none());
    release.send(()).unwrap();
    assert!(matches!(
        wait_workspace_document(&mut writes, &editor),
        DocumentEvent::Discarded
    ));
    assert!(resource.exists() && !target.exists());

    stage_resource(
        &mut resources,
        &mut inspections,
        &editor,
        &workspace,
        &scope,
        &resource,
        &target,
    );
    inspections
        .submit_workspace_resource(
            resources
                .prepare_resource(&workspace, &1, &editor)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    let DocumentEvent::WorkspaceIo(reply) = wait_workspace_document(&mut inspections, &editor)
    else {
        panic!("disk terminal");
    };
    assert!(!resource.exists() && target.exists());
    let (_, requests) = resources.invalidate_resources("configuration changed");
    assert_eq!(requests.len(), 1);
    assert!(inspections.cancel_workspace_queued(requests[0]).is_none());
    assert!(
        resources
            .resource_finished(reply, &version, 2)
            .unwrap()
            .is_ok()
    );
    assert!(resources.finish_resources("duplicate shutdown").is_empty());

    // Stop a gated worker with an accepted resource: report an unknown outcome
    // exactly once, not a rollback or a falsely acknowledged cancellation.
    stage_resource(
        &mut resources,
        &mut inspections,
        &editor,
        &workspace,
        &scope,
        &target,
        &resource,
    );
    let (vfs, release) = VfsWorker::start_gated_for_test(2);
    let mut stopped = Documents::new(
        vfs,
        &StartupTarget::Dashboard {
            root: root.path().into(),
        },
        editor.window_version(),
        1,
        2,
    );
    stopped
        .submit_workspace_resource(
            resources
                .prepare_resource(&workspace, &1, &editor)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    stopped.stop_worker();
    release.send(()).unwrap();
    assert!(matches!(
        wait_workspace_document(&mut stopped, &editor),
        DocumentEvent::Stopped { .. }
    ));
    let notices = resources.finish_resources("worker stopped");
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("outcome is unknown"));
    assert!(resources.finish_resources("deadline").is_empty());
    assert!(!resource.exists() && target.exists());
    inspections.stop_worker();
    writes.stop_worker();
    owner.next = u64::MAX;
    assert!(
        owner
            .begin(version, 1, editor.workspace_edit_sources().unwrap())
            .is_err()
    );
}

fn wait_workspace_document(
    documents: &mut crate::app::documents::Documents,
    editor: &EditorState,
) -> crate::app::documents::DocumentEvent {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if let Some(event) = documents.poll(&editor.window_version(), 1).unwrap() {
            return event;
        }
        assert!(std::time::Instant::now() < end);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
fn stage_resource(
    owner: &mut WorkspaceEdits,
    documents: &mut crate::app::documents::Documents,
    editor: &EditorState,
    workspace: &crate::app::workspace::Workspace,
    scope: &WorkspaceState,
    source: &std::path::Path,
    target: &std::path::Path,
) {
    let version = workspace.version();
    let ticket = owner
        .begin(version.clone(), 1, editor.workspace_edit_sources().unwrap())
        .unwrap();
    owner
        .start_files(
            ticket,
            "resource lifetime".into(),
            None,
            false,
            vec![InputFile::Resource {
                source: source.into(),
                target: Some(target.into()),
            }],
            (vec![source.parent().unwrap().into()], 1024),
        )
        .unwrap();
    let prepared = owner.prepare_io(0, workspace, &1, editor).unwrap();
    documents.submit_workspace_io(prepared).unwrap();
    let crate::app::documents::DocumentEvent::WorkspaceIo(reply) =
        wait_workspace_document(documents, editor)
    else {
        panic!("resource inspection");
    };
    owner.accept_preparation_reply(reply, &version, 1).unwrap();
    owner
        .finish_preparation(Vec::new(), scope, &version, 1)
        .unwrap();
    let (sources, proposal) = owner.approval(&version, 1).unwrap();
    let change = editor.workspace_resource_change(sources, source).unwrap();
    let draft = crate::app::workspace::edits::prepare_workspace_edit(
        editor,
        sources,
        proposal,
        TransactionId(1),
    )
    .unwrap();
    owner.stage_approval(&draft, vec![change]).unwrap();
    let published = draft.accept(&editor.version()).unwrap();
    owner.accepted(published.publication()).unwrap();
}

#[test]
fn new_file_text_uses_one_creation_snapshot_and_one_draft() {
    let root = tempfile::tempdir().unwrap();
    let scope = WorkspaceState::new(
        crate::workspace::WorkspaceId(1),
        [(
            "root".into(),
            crate::files::RootIdentity::capture(root.path()).unwrap(),
        )],
    )
    .unwrap();
    let workspace = crate::app::workspace::Workspace::new(
        scope.clone(),
        root.path().join("trust"),
        1,
        false,
        false,
        1,
    );
    let editor = EditorState::new(root.path().into());
    let path = root.path().join("new.rs");
    let mut documents = crate::app::documents::Documents::new(
        crate::files::VfsWorker::start(2),
        &crate::app::startup::StartupTarget::Dashboard {
            root: root.path().into(),
        },
        editor.window_version(),
        1,
        2,
    );
    let mut owner = WorkspaceEdits::default();
    let ticket = owner
        .begin(
            workspace.version(),
            1,
            editor.workspace_edit_sources().unwrap(),
        )
        .unwrap();
    owner
        .start_files(
            ticket,
            "create".into(),
            None,
            false,
            vec![
                InputFile::Read(path.clone()),
                InputFile::Create(path.clone()),
            ],
            (vec![root.path().into()], 1024),
        )
        .unwrap();
    let (index, file) = owner.next_file().unwrap();
    assert!(matches!(file, InputFile::Create(_)));
    let prepared = owner.prepare_io(index, &workspace, &1, &editor).unwrap();
    documents.submit_workspace_io(prepared).unwrap();
    let crate::app::documents::DocumentEvent::WorkspaceIo(reply) =
        wait_workspace_document(&mut documents, &editor)
    else {
        panic!("creation reply");
    };
    owner
        .accept_preparation_reply(reply, &workspace.version(), 1)
        .unwrap();
    owner
        .finish_preparation(
            vec![WorkspaceEditDraft {
                canonical_path: path.clone(),
                expected_revision: None,
                edits: vec![TextEdit::new(
                    ByteRange::empty(ByteOffset(0)),
                    "fn main() {}\n",
                )],
            }],
            &scope,
            &workspace.version(),
            1,
        )
        .unwrap();
    let (sources, proposal) = owner.approval(&workspace.version(), 1).unwrap();
    assert_eq!(proposal.files.len(), 1);
    assert_eq!(proposal.files[0].proposed_bytes, b"fn main() {}\n");
    let draft = crate::app::workspace::edits::prepare_workspace_edit(
        &editor,
        sources,
        proposal,
        TransactionId(1),
    )
    .unwrap();
    let published = draft.accept(&editor.version()).unwrap();
    let buffer = published
        .buffers
        .iter()
        .find_map(|(id, buffer)| (buffer.path.as_ref() == Some(&path)).then_some(id))
        .unwrap();
    assert_eq!(published.buffers[buffer].text.to_string(), "fn main() {}\n");
    assert!(published.buffers[buffer].dirty);
}

#[test]
fn combined_text_and_rename_or_delete_is_rejected_before_publication() {
    let root = tempfile::tempdir().unwrap();
    let scope = WorkspaceState::new(
        crate::workspace::WorkspaceId(1),
        [(
            "root".into(),
            crate::files::RootIdentity::capture(root.path()).unwrap(),
        )],
    )
    .unwrap();
    let workspace = crate::app::workspace::Workspace::new(
        scope.clone(),
        root.path().join("trust"),
        1,
        false,
        false,
        1,
    );
    let mut editor = EditorState::new(root.path().into());
    let path = root.path().join("old");
    let (buffer, _) = editor
        .open_text(TextBuffer::from_text("old"), Some(path.clone()))
        .unwrap();
    for kind in [
        ResourceIntentKind::Rename {
            target: root.path().join("new"),
            parent_device: 1,
            parent_inode: 1,
        },
        ResourceIntentKind::Delete,
    ] {
        let mut owner = WorkspaceEdits::default();
        let ticket = owner
            .begin(
                workspace.version(),
                1,
                editor.workspace_edit_sources().unwrap(),
            )
            .unwrap();
        let prepared = PreparedEdits {
            ticket,
            label: "combined".into(),
            drafts: vec![WorkspaceEditDraft {
                canonical_path: path.clone(),
                expected_revision: Some(editor.buffers[buffer].text.revision()),
                edits: vec![TextEdit::new(
                    ByteRange::new(ByteOffset(0), ByteOffset(3)).unwrap(),
                    "new",
                )],
            }],
            snapshots: vec![FileSnapshot {
                canonical_path: path.clone(),
                revision: Some(editor.buffers[buffer].text.revision()),
                bytes: b"old".to_vec(),
            }],
            resources: vec![CheckedResourceIntent {
                source: path.clone(),
                source_device: 1,
                source_inode: 1,
                source_bytes: b"old".to_vec(),
                canonical_roots: vec![root.path().into()],
                maximum_bytes: 1024,
                kind,
            }],
        };
        assert!(
            owner
                .build(prepared, &scope, &workspace.version(), 1)
                .is_err()
        );
        assert_eq!(editor.buffers[buffer].text.to_string(), "old");
        assert!(owner.proposal().is_none());
    }
}

#[cfg(unix)]
#[test]
fn combined_text_and_rename_is_rejected_before_publication() {
    let root = tempfile::tempdir().unwrap();
    let scope = WorkspaceState::new(
        crate::workspace::WorkspaceId(1),
        [(
            "root".into(),
            crate::files::RootIdentity::capture(root.path()).unwrap(),
        )],
    )
    .unwrap();
    let workspace = crate::app::workspace::Workspace::new(
        scope.clone(),
        root.path().join("trust"),
        1,
        false,
        false,
        1,
    );
    let mut editor = EditorState::new(root.path().into());
    let path = root.path().join("old");
    let (buffer, _) = editor
        .open_text(TextBuffer::from_text("old"), Some(path.clone()))
        .unwrap();
    let mut owner = WorkspaceEdits::default();
    let ticket = owner
        .begin(
            workspace.version(),
            1,
            editor.workspace_edit_sources().unwrap(),
        )
        .unwrap();
    let prepared = PreparedEdits {
        ticket,
        label: "combined".into(),
        drafts: vec![WorkspaceEditDraft {
            canonical_path: path.clone(),
            expected_revision: Some(editor.buffers[buffer].text.revision()),
            edits: vec![TextEdit::new(
                ByteRange::new(ByteOffset(0), ByteOffset(3)).unwrap(),
                "new",
            )],
        }],
        snapshots: vec![FileSnapshot {
            canonical_path: path.clone(),
            revision: Some(editor.buffers[buffer].text.revision()),
            bytes: b"old".to_vec(),
        }],
        resources: vec![CheckedResourceIntent {
            source: path,
            source_device: 1,
            source_inode: 1,
            source_bytes: b"old".to_vec(),
            canonical_roots: vec![root.path().into()],
            maximum_bytes: 1024,
            kind: ResourceIntentKind::Rename {
                target: root.path().join("new"),
                parent_device: 1,
                parent_inode: 1,
            },
        }],
    };
    assert!(
        owner
            .build(prepared, &scope, &workspace.version(), 1)
            .is_err()
    );
    assert_eq!(editor.buffers[buffer].text.to_string(), "old");
    assert!(owner.proposal().is_none());
}
