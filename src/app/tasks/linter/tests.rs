//! Tests for linter execution and diagnostic parsing.

use crate::app::documents::DocumentEvent;
use crate::app::documents::Documents;
use crate::app::startup::StartupTarget;
#[cfg(unix)]
use crate::app::tasks::TaskUpdate;
use crate::app::tasks::linter::*;
use crate::config::Config;
use crate::config::ProblemMatcherConfig;
use crate::config::TaskConfig;
use crate::config::TaskKind;
use crate::files::RootIdentity;
use crate::files::VfsWorker;
use crate::workspace::WorkspaceId;
use crate::workspace::WorkspaceState;
use std::thread;
use std::time::Duration;
use std::time::Instant;
fn fixture() -> (
    tempfile::TempDir,
    EditorState,
    Workspace,
    Config,
    Tasks,
    Diagnostics,
) {
    let root = tempfile::tempdir().unwrap();
    let scope = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(scope, root.path().join("trust"), 1, false, false, 1);
    let editor = EditorState::new(root.path().into());
    let matcher = ProblemMatcherConfig {
        pattern: r"(?m)^(?P<path>[^:]+):(?P<line>\d+):(?P<column>\d+):(?P<message>[^\n]+)$".into(),
        path_group: "path".into(),
        line_group: "line".into(),
        column_group: Some("column".into()),
        column_encoding: None,
        severity_group: None,
        message_group: "message".into(),
        name_group: None,
        status_group: None,
        duration_group: None,
    };
    let mut config = Config::default();
    for name in ["first", "second"] {
        config.tasks.insert(
            name.into(),
            TaskConfig {
                kind: TaskKind::Linter,
                executable: "/bin/sh".into(),
                arguments: vec![],
                cwd: None,
                environment: BTreeMap::new(),
                dependencies: vec![],
                timeout_ms: None,
                stdin: false,
                matcher: Some(matcher.clone()),
            },
        );
    }
    let tasks = Tasks::new(&config);
    (
        root,
        editor,
        workspace,
        config,
        tasks,
        Diagnostics::new(100, 100),
    )
}
// Simulated completion metadata inside the private owner; the connected dispatch test
// separately exercises actual authorization, process output and worker submission.
fn completed(
    tasks: &mut Tasks,
    config: &Config,
    workspace: &Workspace,
    editor: &EditorState,
    diagnostics: &mut Diagnostics,
    name: &str,
    bytes: &[u8],
) -> RunningTask {
    let adapter = AdapterContext::Linter {
        matcher: config.tasks[name].matcher.clone().unwrap(),
        lint: true,
    };
    tasks
        .plan_checked(name, &adapter, config, workspace, 1)
        .unwrap();
    let (step, _) = tasks.step(name, workspace, 1).unwrap();
    tasks.statuses.insert(name.into(), "process exited".into());
    let source = TaskDiagnosticRun::capture(name, editor, workspace, 1).unwrap();
    diagnostics.activate(source.producer().clone());
    RunningTask {
        output: bytes.to_vec(),
        cancelled: false,
        key: step.key,
        requested: name.into(),
        name: name.into(),
        adapter,
        root: editor.workspace_root.clone(),
        cwd: editor.workspace_root.clone(),
        diagnostic_source: Some(source),
    }
}
fn documents(vfs: VfsWorker, editor: &EditorState, workspace: &Workspace) -> Documents {
    Documents::new(
        vfs,
        &StartupTarget::Dashboard {
            root: editor.workspace_root.clone(),
        },
        editor.window_version(),
        workspace.root_revision(),
        16,
    )
}
fn reply(documents: &mut Documents, editor: &EditorState, workspace: &Workspace) -> TaskReadReply {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(DocumentEvent::TaskRead(reply)) = documents
            .poll(&editor.window_version(), workspace.root_revision())
            .unwrap()
        {
            return reply;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn linter_owner_validates_whole_sources_and_invalidates_original_publications() {
    let (root, mut editor, workspace, config, mut tasks, mut diagnostics) = fixture();
    let (id, _) = editor
        .open_text(
            TextBuffer::from_text("abc\n"),
            Some(root.path().join("open")),
        )
        .unwrap();
    for name in ["first", "second"] {
        let task = completed(
            &mut tasks,
            &config,
            &workspace,
            &editor,
            &mut diagnostics,
            name,
            b"open:1:1:ok\n",
        );
        tasks
            .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
            .unwrap();
        let Some(LinterUpdate::Publish { producer, batches }) =
            tasks.next_linter_update(&editor, &workspace, 1, &diagnostics)
        else {
            panic!("missing publication")
        };
        diagnostics
            .replace_source(&producer, batches, &editor)
            .unwrap();
        assert!(
            tasks
                .next_linter_update(&editor, &workspace, 1, &diagnostics)
                .is_none()
        );
    }
    assert_eq!(diagnostics.choices().count(), 2);
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"open:1:1:good\nopen:0:1:bad\n",
    );
    assert!(
        tasks
            .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
            .is_err()
    );
    assert_eq!(diagnostics.choices().count(), 1); // independent second producer survives
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"open:1:1:old\n",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    editor
        .open_text(
            TextBuffer::from_text("unrelated"),
            Some(root.path().join("other")),
        )
        .unwrap();
    editor.buffers[id].text = TextBuffer::from_text("abc\n");
    assert!(matches!(
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics),
        Some(LinterUpdate::Failed(_))
    ));
    assert!(
        tasks
            .next_linter_update(&editor, &workspace, 1, &diagnostics)
            .is_none()
    );
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"\xff",
    );
    let foreign = EditorState::new(root.path().into());
    assert!(
        tasks
            .begin_linter(task, &foreign, &workspace, 1, &diagnostics, (100, 1024))
            .unwrap_err()
            .contains("source is stale")
    );
    // Supersession discards the old build, never the new producer's set.
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"unopened:1:1:old",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"open:1:1:new",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    assert!(matches!(
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics),
        Some(LinterUpdate::Failed(_))
    ));
    let Some(LinterUpdate::Publish { producer, batches }) =
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics)
    else {
        panic!("replacement missing")
    };
    diagnostics
        .replace_source(&producer, batches, &editor)
        .unwrap();
    assert_eq!(diagnostics.choices().count(), 2);
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"unopened:1:1:stopped",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    let stopped = tasks.stop("first").unwrap().unwrap();
    diagnostics.clear_source(&stopped);
    assert!(matches!(
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics),
        Some(LinterUpdate::Failed(_))
    ));
    assert_eq!(diagnostics.choices().count(), 1);
    tasks.linters.next = u64::MAX;
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"open:1:1:x",
    );
    assert!(
        tasks
            .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
            .unwrap_err()
            .contains("identity exhausted")
    );
}
#[cfg(unix)]
#[test]
fn linter_symlinks_share_canonical_reads_and_respect_open_buffers() {
    let (root, mut editor, workspace, config, mut tasks, mut diagnostics) = fixture();
    let path = root.path().join("real");
    std::fs::write(&path, "abc\n").unwrap();
    std::os::unix::fs::symlink(&path, root.path().join("link")).unwrap();
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"link:1:1:alias\nreal:1:2:real\n",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    assert_eq!(
        tasks.linters.builds.values().next().unwrap().unread.len(),
        1
    );
    let mut docs = documents(VfsWorker::start(1), &editor, &workspace);
    docs.submit_linter_read(
        tasks
            .linter_read(&editor, &workspace, &1, &diagnostics)
            .unwrap(),
    )
    .unwrap();
    tasks
        .accept_linter_read(
            reply(&mut docs, &editor, &workspace),
            &editor,
            &workspace,
            1,
            &diagnostics,
        )
        .unwrap();
    let Some(LinterUpdate::Publish { batches, .. }) =
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics)
    else {
        panic!("symlink read was not published");
    };
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].items.len(), 2);
    assert_eq!(batches[0].uri, crate::language::lsp::file_uri(&path));

    let (id, _) = editor
        .open_text(
            TextBuffer::from_text("abc\n"),
            Some(root.path().join("link")),
        )
        .unwrap();
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"real:1:1:open\n",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    let Some(LinterUpdate::Publish { producer, batches }) =
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics)
    else {
        panic!("open symlink diagnostics were not published");
    };
    let uri = crate::language::lsp::file_uri(&root.path().join("link"));
    assert_eq!(batches[0].uri, uri);
    assert_eq!(batches[0].items[0].uri, uri);
    diagnostics
        .replace_source(&producer, batches, &editor)
        .unwrap();
    assert_eq!(diagnostics.choices().count(), 1);
    editor.buffers[id].dirty = true;
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"real:1:1:dirty\n",
    );
    assert!(
        tasks
            .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
            .is_err()
    );

    std::fs::create_dir(root.path().join("sources")).unwrap();
    std::os::unix::fs::symlink(root.path().join("sources"), root.path().join("alias")).unwrap();
    let new_path = root.path().join("alias/new");
    editor
        .open_text(TextBuffer::from_text("new text"), Some(new_path.clone()))
        .unwrap();
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"sources/new:1:1:new buffer\n",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    let Some(LinterUpdate::Publish { producer, batches }) =
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics)
    else {
        panic!("new buffer under a symlinked directory was not published");
    };
    assert_eq!(batches[0].uri, crate::language::lsp::file_uri(&new_path));
    diagnostics
        .replace_source(&producer, batches, &editor)
        .unwrap();

    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("source"), "text").unwrap();
    std::os::unix::fs::symlink(outside.path().join("source"), root.path().join("escape")).unwrap();
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"escape:1:1:outside\n",
    );
    assert!(
        tasks
            .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
            .unwrap_err()
            .contains("outside")
    );
}

#[test]
fn linter_reads_reserve_real_capacity_keep_worker_identity_and_refuse_stale_decode() {
    let (root, mut editor, workspace, config, mut tasks, mut diagnostics) = fixture();
    for path in ["a", "b"] {
        std::fs::write(root.path().join(path), b"abc\n").unwrap();
    }
    let (vfs, release) = VfsWorker::start_gated_for_test(1);
    let mut docs = documents(vfs, &editor, &workspace);
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"a:1:1:a\nb:1:1:b\n",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    {
        let _unused_loan = tasks
            .linter_read(&editor, &workspace, &1, &diagnostics)
            .unwrap();
    }
    assert_eq!(
        tasks.linters.builds.values().next().unwrap().unread.len(),
        2
    );
    docs.submit_linter_read(
        tasks
            .linter_read(&editor, &workspace, &1, &diagnostics)
            .unwrap(),
    )
    .unwrap();
    assert!(
        !docs
            .submit_linter_read(
                tasks
                    .linter_read(&editor, &workspace, &1, &diagnostics)
                    .unwrap()
            )
            .unwrap()
    );
    assert_eq!(
        tasks.linters.builds.values().next().unwrap().unread.len(),
        1
    );
    tasks.cancel_workspace_tasks();
    assert_eq!(
        tasks.linters.builds.values().next().unwrap().pending.len(),
        1
    );
    let key = tasks.linter_cancellations().pop().unwrap();
    tasks.linter_cancelled(docs.cancel_linter_read(&key).unwrap());
    assert!(docs.cancel_linter_read(&key).is_none());
    assert!(matches!(
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics),
        Some(LinterUpdate::Failed(_))
    ));
    release.send(()).unwrap();
    // Full capacity is backpressure, not a failed build: release and resume the exact unread path.
    let (vfs, release) = VfsWorker::start_gated_for_test(1);
    let mut resumed = documents(vfs, &editor, &workspace);
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"a:1:1:a\nb:1:1:b\n",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    assert!(
        resumed
            .submit_linter_read(
                tasks
                    .linter_read(&editor, &workspace, &1, &diagnostics)
                    .unwrap()
            )
            .unwrap()
    );
    assert!(
        !resumed
            .submit_linter_read(
                tasks
                    .linter_read(&editor, &workspace, &1, &diagnostics)
                    .unwrap()
            )
            .unwrap()
    );
    release.send(()).unwrap();
    tasks
        .accept_linter_read(
            reply(&mut resumed, &editor, &workspace),
            &editor,
            &workspace,
            1,
            &diagnostics,
        )
        .unwrap();
    assert!(
        resumed
            .submit_linter_read(
                tasks
                    .linter_read(&editor, &workspace, &1, &diagnostics)
                    .unwrap()
            )
            .unwrap()
    );
    tasks
        .accept_linter_read(
            reply(&mut resumed, &editor, &workspace),
            &editor,
            &workspace,
            1,
            &diagnostics,
        )
        .unwrap();
    assert!(
        matches!(tasks.next_linter_update(&editor,&workspace,1,&diagnostics),Some(LinterUpdate::Publish{batches,..}) if batches.len()==2)
    );
    // Two real worker namespaces may allocate the same numeric ID, never the same key.
    let (vfs, release) = VfsWorker::start_gated_for_test(1);
    let mut first = documents(vfs, &editor, &workspace);
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"a:1:1:a\n",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    first
        .submit_linter_read(
            tasks
                .linter_read(&editor, &workspace, &1, &diagnostics)
                .unwrap(),
        )
        .unwrap();
    let mut other = Tasks::new(&config);
    let mut second = documents(VfsWorker::start(1), &editor, &workspace);
    let task = completed(
        &mut other,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "second",
        b"a:1:1:b\n",
    );
    other
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    second
        .submit_linter_read(
            other
                .linter_read(&editor, &workspace, &1, &diagnostics)
                .unwrap(),
        )
        .unwrap();
    let foreign = reply(&mut second, &editor, &workspace);
    assert_eq!(
        foreign.id(),
        tasks
            .linters
            .builds
            .values()
            .next()
            .unwrap()
            .pending
            .keys()
            .copied()
            .next()
            .unwrap()
    );
    let before = TextBuffer::test_materializations();
    assert!(
        tasks
            .accept_linter_read(foreign, &editor, &workspace, 1, &diagnostics)
            .unwrap_err()
            .contains("unowned")
    );
    assert_eq!(TextBuffer::test_materializations(), before);
    editor
        .open_text(TextBuffer::from_text("new"), Some(root.path().join("a")))
        .unwrap();
    let after_open = TextBuffer::test_materializations();
    release.send(()).unwrap();
    tasks
        .accept_linter_read(
            reply(&mut first, &editor, &workspace),
            &editor,
            &workspace,
            1,
            &diagnostics,
        )
        .unwrap();
    assert_eq!(TextBuffer::test_materializations(), after_open);
    assert!(matches!(
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics),
        Some(LinterUpdate::Failed(_))
    ));
    other.finish_linter_reads("fixture consumed foreign reply");
    // Actual stopped worker produces one sealed terminal and no resurrected source.
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"b:1:1:b\n",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    let (vfs, release) = VfsWorker::start_gated_for_test(1);
    let mut stopped = documents(vfs, &editor, &workspace);
    stopped
        .submit_linter_read(
            tasks
                .linter_read(&editor, &workspace, &1, &diagnostics)
                .unwrap(),
        )
        .unwrap();
    stopped.stop_worker();
    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(DocumentEvent::Stopped { task_reads, .. }) = stopped
            .poll(&editor.window_version(), workspace.root_revision())
            .unwrap()
        {
            tasks.linter_worker_stopped(task_reads);
            break;
        }
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(matches!(
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics),
        Some(LinterUpdate::Failed(_))
    ));
    assert!(
        tasks
            .next_linter_update(&editor, &workspace, 1, &diagnostics)
            .is_none()
    );
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"b:1:1:b",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 2))
        .unwrap();
    let mut bounded = documents(VfsWorker::start(1), &editor, &workspace);
    bounded
        .submit_linter_read(
            tasks
                .linter_read(&editor, &workspace, &1, &diagnostics)
                .unwrap(),
        )
        .unwrap();
    let before = TextBuffer::test_materializations();
    tasks
        .accept_linter_read(
            reply(&mut bounded, &editor, &workspace),
            &editor,
            &workspace,
            1,
            &diagnostics,
        )
        .unwrap();
    assert_eq!(TextBuffer::test_materializations(), before);
    assert!(matches!(
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics),
        Some(LinterUpdate::Failed(_))
    ));
    // Keep the old root in an owned temp directory, preventing inode reuse.
    let task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"b:1:1:b",
    );
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    let (vfs, release) = VfsWorker::start_gated_for_test(1);
    let mut replaced = documents(vfs, &editor, &workspace);
    replaced
        .submit_linter_read(
            tasks
                .linter_read(&editor, &workspace, &1, &diagnostics)
                .unwrap(),
        )
        .unwrap();
    let retained = tempfile::tempdir().unwrap();
    std::fs::rename(root.path(), retained.path().join("old-root")).unwrap();
    std::fs::create_dir(root.path()).unwrap();
    std::fs::write(root.path().join("b"), b"abc\n").unwrap();
    release.send(()).unwrap();
    let before = TextBuffer::test_materializations();
    tasks
        .accept_linter_read(
            reply(&mut replaced, &editor, &workspace),
            &editor,
            &workspace,
            1,
            &diagnostics,
        )
        .unwrap();
    assert_eq!(TextBuffer::test_materializations(), before);
    assert!(
        matches!(tasks.next_linter_update(&editor,&workspace,1,&diagnostics),Some(LinterUpdate::Failed(error)) if error.contains("filesystem identity changed"))
    );
}

#[test]
fn relative_linter_and_test_paths_use_the_task_working_directory() {
    let (root, mut editor, workspace, config, mut tasks, mut diagnostics) = fixture();
    std::fs::create_dir_all(root.path().join("sub/src")).unwrap();
    std::fs::create_dir(root.path().join("sub/build")).unwrap();
    let path = root.path().join("sub/src/main.rs");
    editor
        .open_text(TextBuffer::from_text("text"), Some(path.clone()))
        .unwrap();
    let mut task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"src/main.rs:1:1:problem\n",
    );
    task.cwd = root.path().join("sub");
    let matcher = config.tasks["first"].matcher.clone().unwrap();
    task.adapter = AdapterContext::Test {
        matcher: matcher.clone(),
    };
    tasks.apply_test_result(&task);
    assert_eq!(tasks.tests["first"][0].source.as_ref().unwrap().path, path);
    task.adapter = AdapterContext::Linter {
        matcher,
        lint: true,
    };
    task.cwd = root.path().join("sub/build");
    task.output = b"../src/main.rs:1:1:problem\n".to_vec();
    tasks
        .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    let Some(LinterUpdate::Publish { batches, .. }) =
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics)
    else {
        panic!("publication missing")
    };
    assert_eq!(batches[0].uri, crate::language::lsp::file_uri(&path));
    let mut task = completed(
        &mut tasks,
        &config,
        &workspace,
        &editor,
        &mut diagnostics,
        "first",
        b"../../../outside:1:1:problem\n",
    );
    task.cwd = root.path().join("sub/build");
    assert!(
        tasks
            .begin_linter(task, &editor, &workspace, 1, &diagnostics, (100, 1024))
            .unwrap_err()
            .contains("outside")
    );
}

// The task fixtures are POSIX shell scripts.
#[cfg(unix)]
#[test]
fn a_failing_dependency_publishes_its_matched_diagnostics() {
    let (root, mut editor, workspace, mut config, mut tasks, mut diagnostics) = fixture();
    editor
        .open_text(
            TextBuffer::from_text("text"),
            Some(root.path().join("source")),
        )
        .unwrap();
    let mut dependency = config.tasks["first"].clone();
    dependency.matcher = None;
    dependency.arguments = vec![
        "-c".into(),
        "printf 'source:1:1:compile failed\n'; exit 1".into(),
    ];
    config.tasks.insert("dependency".into(), dependency);
    config
        .tasks
        .get_mut("first")
        .unwrap()
        .dependencies
        .push("dependency".into());
    tasks.configure(&config);
    tasks
        .plan_checked(
            "first",
            &AdapterContext::Linter {
                matcher: config.tasks["first"].matcher.clone().unwrap(),
                lint: false,
            },
            &config,
            &workspace,
            1,
        )
        .unwrap();
    let (step, _) = tasks.step("first", &workspace, 1).unwrap();
    let producer = tasks
        .launch_inner(
            step,
            crate::process::ExecutionPreflightResult {
                id: crate::process::PreflightId(1),
                executable: "/bin/sh".into(),
                cwd: root.path().into(),
                root: root.path().into(),
                root_revision: workspace.root_revision(),
            },
            &editor,
            &workspace,
            1,
        )
        .unwrap()
        .unwrap();
    diagnostics.activate(producer);
    let deadline = Instant::now() + Duration::from_secs(3);
    let task = loop {
        if let Some(TaskUpdate::Finished {
            task, code, next, ..
        }) = tasks.next_update(&workspace, 1)
        {
            assert_eq!(code, Some(1));
            assert!(!next);
            break task;
        }
        assert!(Instant::now() < deadline);
        thread::yield_now();
    };
    tasks
        .begin_linter(*task, &editor, &workspace, 1, &diagnostics, (100, 1024))
        .unwrap();
    let Some(LinterUpdate::Publish { producer, batches }) =
        tasks.next_linter_update(&editor, &workspace, 1, &diagnostics)
    else {
        panic!("missing dependency diagnostic publication")
    };
    diagnostics
        .replace_source(&producer, batches, &editor)
        .unwrap();
    let rows = diagnostics
        .choices()
        .map(|(_, row, _)| row)
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].uri,
        crate::language::lsp::file_uri(&root.path().join("source"))
    );
    assert_eq!(rows[0].range.start.get(), 0);
    assert_eq!(rows[0].message, "compile failed");
}

#[test]
fn linter_columns_match_the_codec_for_sorted_repeated_unicode_positions() {
    use crate::editor::text::PositionCodec;
    use crate::editor::text::PositionEncoding;
    let text = TextBuffer::from_text(&format!("{}\nlast", "a😀é".repeat(100_000)));
    let codec = PositionCodec::new(&text);
    for encoding in [
        PositionEncoding::Utf8,
        PositionEncoding::Utf16,
        PositionEncoding::Utf32,
    ] {
        let end_column = codec
            .byte_to_lsp(crate::editor::text::ByteOffset(699_993), encoding)
            .unwrap()
            .1;
        let raw = (0..10_000)
            .map(|_| Raw {
                line: 0,
                column: end_column,
                encoding,
                severity: DiagnosticSeverity::Error,
                message: "end".into(),
            })
            .collect();
        let items = convert(Path::new("/source"), raw, &text, None, "test").unwrap();
        assert_eq!(items.len(), 10_000);
        assert!(items.iter().all(|item| item.range.start.get() == 699_993));
        let invalid = Raw {
            line: 0,
            column: 2,
            encoding,
            severity: DiagnosticSeverity::Error,
            message: "split".into(),
        };
        assert_eq!(
            convert(Path::new("/source"), vec![invalid], &text, None, "test").is_err(),
            codec.lsp_to_byte(0, 2, encoding).is_err()
        );
    }
}

#[test]
fn character_columns_use_the_existing_position_codec() {
    let text = TextBuffer::from_text("éx");
    let diagnostics = convert(
        std::path::Path::new("/tmp/file"),
        vec![Raw {
            line: 0,
            column: 1,
            encoding: crate::editor::text::PositionEncoding::Utf32,
            severity: DiagnosticSeverity::Error,
            message: "bad x".into(),
        }],
        &text,
        None,
        "test",
    )
    .unwrap();
    assert_eq!(diagnostics[0].range.start.get(), 2);
    for line in [text.len_lines(), 999, usize::MAX] {
        let result = convert(
            std::path::Path::new("/tmp/file"),
            vec![Raw {
                line,
                column: 0,
                encoding: crate::editor::text::PositionEncoding::Utf32,
                severity: DiagnosticSeverity::Error,
                message: "invalid line".into(),
            }],
            &text,
            None,
            "test",
        );
        assert_eq!(result.unwrap_err(), "linter line is outside its source");
    }
}
