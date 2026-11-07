//! Documents behavior and application integration tests.

use crate::app::App;
use crate::app::AppEffect;
use crate::app::PickerKind;
#[cfg(unix)]
use crate::app::agent::commands::AgentCommand;
use crate::app::documents::configured_file_bytes;
use crate::app::git::tests::settle_git_work;
use crate::app::startup::StartupTarget;
#[cfg(unix)]
use crate::app::tasks::tests::check_task_diagnostics;
use crate::app::tests::*;
use crate::debugger::DapControl;
use crate::debugger::DapEvent;
use crate::debugger::DapResponse;
use crate::editor::CommandValue;
use crate::editor::EditorOperation;
use crate::editor::EditorState;
use crate::editor::EditorTransaction;
use crate::editor::InvocationSource;
use crate::editor::TransactionId;
use crate::editor::input::KeyEvent;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::TextBuffer;
use crate::process::PtyId;
use crate::ui::ContentTabDiagnostic;
use crate::ui::ContentTabDiagnosticSeverity;
use crate::ui::ContentTabKind;
use crate::ui::PtyScreen;
use crate::ui::UiEvent;
use crate::ui::active_pane_viewport;
use crate::ui::render;
use crate::ui::render::overlays::CUSTOM_ICON;
use crate::ui::render::overlays::SCRATCH_ICON;
use crate::windows::DockEdge;
use crate::windows::WindowPlacement;
use crate::workspace::DiagnosticSeverity;
#[cfg(unix)]
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::time::Instant;
#[cfg(unix)]
use tempfile::tempdir;

#[test]
fn diagnostic_navigation_filters_severity_wraps_and_expands_the_landed_line() {
    let root = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        crate::config::load_bundled_config(),
        false,
        state_dir.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((80, 24));
    let path = app.state.workspace_root.join("diagnostic.rs");
    let (buffer, view) = app
        .state
        .open_text(
            TextBuffer::from_text("ok\nerror\nwarning\n"),
            Some(path.clone()),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let uri = crate::language::lsp::file_uri(&path);
    let diagnostic = |range, severity, message: &str| crate::workspace::Diagnostic {
        uri: uri.clone(),
        version: None,
        range,
        severity,
        source: "fake".into(),
        code: None,
        message: message.into(),
        related: Vec::new(),
        tags: Vec::new(),
        fixes: Vec::new(),
        data: None,
        timestamp: std::time::SystemTime::UNIX_EPOCH,
    };
    let producer = crate::workspace::DiagnosticProducer::new(
        crate::workspace::DiagnosticOwner::new("lsp.fake").unwrap(),
    );
    app.diagnostics.activate(producer.clone());
    app.diagnostics
        .publish(
            crate::app::diagnostics::DiagnosticBatch {
                producer,
                uri: uri.clone(),
                document: crate::app::diagnostics::DiagnosticDocument::capture(&app.state, buffer),
                validity: crate::app::diagnostics::DiagnosticValidity::UnversionedReport,
                items: vec![
                    diagnostic(
                        ByteRange::new(ByteOffset(3), ByteOffset(8)).unwrap(),
                        DiagnosticSeverity::Error,
                        "error",
                    ),
                    diagnostic(
                        ByteRange::new(ByteOffset(9), ByteOffset(16)).unwrap(),
                        DiagnosticSeverity::Warning,
                        "warning",
                    ),
                ],
            },
            &app.state,
        )
        .unwrap();
    app.sync_ui_diagnostics();

    app.invoke_mapped_command("diagnostic.warning.next");
    assert_eq!(app.state.views[view].cursor, ByteOffset(9));
    assert_eq!(
        app.ui.expanded_diagnostic.as_ref().unwrap().severities,
        vec![DiagnosticSeverity::Warning]
    );

    app.invoke_mapped_command("diagnostic.error.next");
    assert_eq!(app.state.views[view].cursor, ByteOffset(3));
    app.invoke_mapped_command("diagnostic.next");
    assert_eq!(app.state.views[view].cursor, ByteOffset(9));
    assert_eq!(app.state.views[view].buffer, buffer);
    app.open_builtin_picker(PickerKind::Diagnostics);
    let rows = &app.ui.picker.as_ref().unwrap().rows;
    assert!(
        rows.iter()
            .any(|row| row.label.ends_with("diagnostic.rs:2") && row.detail == "error")
    );
    assert!(
        rows.iter()
            .any(|row| row.label.ends_with("diagnostic.rs:3") && row.detail == "warning")
    );
    let before = app.state.views[view].cursor;
    let replacement = crate::workspace::DiagnosticProducer::new(
        crate::workspace::DiagnosticOwner::new("lsp.fake").unwrap(),
    );
    app.diagnostics.activate(replacement);
    app.accept_picker();
    assert_eq!(
        app.state.views[view].cursor, before,
        "old picker key cannot navigate replacement diagnostics"
    );
    assert!(app.ui.expanded_diagnostic.is_none());

    // A simulated task source uses the production pre-run capture, not a late name lookup.
    let configuration = app.configuration.binding_revision();
    let run = crate::app::tasks::TaskDiagnosticRun::capture(
        "lint",
        &app.state,
        &app.workspace,
        configuration,
    )
    .unwrap();
    app.diagnostics.activate(run.producer().clone());
    assert!(run.current(&app.diagnostics, &app.workspace, configuration));
    assert!(run.document(&path, &app.state).unwrap().is_some());
    app.state
        .open_text(
            TextBuffer::from_text("unrelated"),
            Some(root.path().join("other")),
        )
        .unwrap();
    assert!(run.document(&path, &app.state).unwrap().is_some());
    assert!(!run.current(&app.diagnostics, &app.workspace, configuration + 1));
    let next = crate::app::tasks::TaskDiagnosticRun::capture(
        "lint",
        &app.state,
        &app.workspace,
        configuration,
    )
    .unwrap();
    app.diagnostics.activate(next.producer().clone());
    app.diagnostics.clear_source(run.producer());
    assert!(next.current(&app.diagnostics, &app.workspace, configuration));
    assert!(!run.current(&app.diagnostics, &app.workspace, configuration));
    app.state.buffers[buffer].text = TextBuffer::from_text("ok\nerror\nwarning\n");
    assert!(next.document(&path, &app.state).is_err());
}

#[test]
fn accepting_a_disk_diagnostic_opens_its_file_and_validates_the_range() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("unopened.txt");
    fs::write(&path, "first\nerror\n").unwrap();
    let uri = crate::language::lsp::file_uri(&path);
    let producer = crate::workspace::DiagnosticProducer::new(
        crate::workspace::DiagnosticOwner::new("lint.test").unwrap(),
    );
    app.diagnostics.activate(producer.clone());
    app.diagnostics
        .publish(
            crate::app::diagnostics::DiagnosticBatch {
                producer,
                uri: uri.clone(),
                document: None,
                validity: crate::app::diagnostics::DiagnosticValidity::DiskReport,
                items: vec![crate::workspace::Diagnostic {
                    uri,
                    version: None,
                    range: ByteRange::new(ByteOffset(6), ByteOffset(11)).unwrap(),
                    severity: DiagnosticSeverity::Error,
                    source: "test".into(),
                    code: None,
                    message: "error".into(),
                    related: vec![],
                    tags: vec![],
                    fixes: vec![],
                    data: None,
                    timestamp: std::time::SystemTime::UNIX_EPOCH,
                }],
            },
            &app.state,
        )
        .unwrap();
    app.open_builtin_picker(PickerKind::Diagnostics);
    app.accept_picker();
    wait_vfs(&mut app);
    let view = &app.state.views[app.state.active_view().unwrap()];
    assert_eq!(app.state.buffers[view.buffer].path.as_ref(), Some(&path));
    assert_eq!(view.cursor, ByteOffset(6));

    let before = view.buffer;
    let invalid = app.state.workspace_root.join("short.txt");
    fs::write(&invalid, "x").unwrap();
    app.navigate_byte_location(
        &crate::language::lsp::file_uri(&invalid),
        ByteRange::new(ByteOffset(6), ByteOffset(11)).unwrap(),
    );
    wait_vfs(&mut app);
    assert_eq!(app.state.active_buffer_id(), Some(before));
    assert!(app.ui.notification_error);
}

#[test]
fn operation_state_read_failure_is_reported_as_unavailable_not_idle() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let git_dir = repo.path().to_path_buf();
    drop(repo);
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.git.status().is_some());
    assert!(app.ui.operation_bar.is_none());
    fs::write(git_dir.join("index"), b"invalid index").unwrap();
    app.project_operation_bar();
    let bar = app.ui.operation_bar.as_ref().unwrap();
    assert_eq!(bar.phase, "operation state unavailable");
    assert!(bar.actions.is_empty());
    assert!(bar.abort_binding.is_none());
    assert!(app.ui.notification_error);
    assert!(app.ui.notification.is_some());
    fs::remove_file(git_dir.join("index")).unwrap();
    app.project_operation_bar();
    assert!(app.ui.operation_bar.is_none());
}

#[test]
fn soft_reset_refreshes_status_even_when_index_and_head_files_do_not_change() {
    let (_app_dirs, mut app, repo, _) = near_changes_fixture();
    let old = repo.head().unwrap().peel_to_commit().unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("near.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&old])
        .unwrap();
    app.start_git_status_refresh_with_feedback(false);
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    let stamp = |name| {
        fs::metadata(repo.path().join(name))
            .unwrap()
            .modified()
            .unwrap()
    };
    let before = (stamp("index"), stamp("HEAD"));
    repo.reset(old.as_object(), git2::ResetType::Soft, None)
        .unwrap();
    assert_eq!(before, (stamp("index"), stamp("HEAD")));
    app.next_disk_check = None;
    app.process_disk_changes();
    assert!(app.git.status_pending());
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(
        app.git
            .status()
            .unwrap()
            .entries
            .iter()
            .any(|entry| entry.sources.staged)
    );
}

/// A trusted repository with one file whose two changes are close enough to share a hunk.
pub(in crate::app) fn near_changes_fixture() -> (
    (tempfile::TempDir, tempfile::TempDir),
    App,
    git2::Repository,
    std::path::PathBuf,
) {
    let (dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("near.txt");
    fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("near.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "fixture",
        &repo.find_tree(tree_id).unwrap(),
        &[],
    )
    .unwrap();
    fs::write(&path, "one\nTWO\nthree\nFOUR\nfive\n").unwrap();
    prepare_trust_identity(&mut app);
    let root = app.workspace.roots()[0].clone();
    app.workspace.fixture_trust_identity(root.identity.clone());
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    app.start_git_status_refresh_silent();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.git.status().is_none() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    (dirs, app, repo, path)
}

#[test]
fn save_all_retries_documents_beyond_the_request_capacity() {
    let (_app_dirs, mut app) = app();
    let mut saved = Vec::new();
    for index in 0..12 {
        let path = app
            .state
            .workspace_root
            .join(format!("save-all-{index}.txt"));
        fs::write(&path, "old").unwrap();
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("old"), Some(path.clone()))
            .unwrap();
        app.replace_with_recovered(buffer, format!("new {index}"))
            .unwrap();
        saved.push((buffer, path, format!("new {index}")));
    }
    app.process_effect(AppEffect::SaveAll { quit_after: true });
    assert!(!app.pending_saves.is_empty());
    assert!(!app.should_quit);
    wait_vfs(&mut app);
    assert!(app.pending_saves.is_empty());
    for (buffer, path, expected) in saved {
        assert_eq!(fs::read_to_string(path).unwrap(), expected);
        assert!(!app.state.buffers[buffer].dirty);
    }
    assert!(app.should_quit);
}

#[test]
fn save_as_reload_close_and_external_change_protection_work() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.start_save(false, Some(PathBuf::from("missing/new.txt")), false);
    assert_eq!(
        app.ui.notification.as_deref(),
        Some("can't save new.txt: the folder missing doesn't exist")
    );
    app.start_save(false, Some(PathBuf::from("new.txt")), false);
    wait_vfs(&mut app);
    let path = root.join("new.txt");
    assert_eq!(fs::read(&path).expect("saved"), b"abc");
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));

    fs::write(&path, b"external").expect("external edit");
    app.invoke_insert_text("X".into());
    app.start_save(false, None, false);
    wait_vfs(&mut app);
    assert_eq!(fs::read(&path).expect("not overwritten"), b"external");
    assert!(app.ui.notification_error);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("externally modified"))
    );
    // `:e! new.txt` on the current file takes the disk's text and drops the unsaved edit.
    app.process_effect(AppEffect::ForceOpenPath(PathBuf::from("new.txt")));
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "external");
    app.invoke_insert_text("X".into());

    let unsaved = app.state.buffers[buffer].text.to_string();
    assert!(app.state.buffers[buffer].dirty);
    app.start_reload(false);
    assert_eq!(app.documents.pending_count(), 0);
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.to_string(), unsaved);
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(fs::read(&path).unwrap(), b"external");
    app.start_reload(true);
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "external");
    assert!(!app.state.buffers[buffer].dirty);
    app.close_active_buffer(false);
    assert!(app.ui.dashboard);
    assert!(app.state.active_view().is_none());
}

#[test]
fn dirty_quit_is_blocked_but_forced_quit_succeeds() {
    let (_app_dirs, mut app) = app();
    let (_, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("dirty"), None)
        .unwrap();
    let buffer = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[buffer].dirty = true;
    let quit = crate::editor::CommandInvocation::new("editor.quit", InvocationSource::Test)
        .expect("quit")
        .with_argument("force", CommandValue::Bool(false));
    app.invoke(quit);
    assert!(!app.should_quit);
    let force = crate::editor::CommandInvocation::new("editor.quit", InvocationSource::Test)
        .expect("quit")
        .with_argument("force", CommandValue::Bool(true));
    app.invoke(force);
    assert!(app.should_quit);
}

#[cfg(unix)]
#[test]
fn a_failed_test_is_listed_in_problems_and_jumps_to_its_line() {
    let root = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    fs::write(root.path().join("a.txt"), "one\ntwo\néX\n").unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.tasks.insert(
        "unit".into(),
        crate::config::TaskConfig {
            kind: crate::config::TaskKind::Test,
            executable: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "printf 'FAILED adds a.txt:3:2 wrong sum\nok reads a.txt:1:1 fine\n'".into(),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            dependencies: vec![],
            timeout_ms: Some(2000),
            stdin: false,
            matcher: Some(crate::config::ProblemMatcherConfig {
                pattern: r"(?m)^(?P<status>\S+) (?P<name>\S+) (?P<path>[^:]+):(?P<line>\d+):(?P<column>\d+) (?P<message>[^\n]+)$"
                    .into(),
                path_group: "path".into(),
                line_group: "line".into(),
                column_group: Some("column".into()),
                column_encoding: Some(crate::editor::text::PositionEncoding::Utf32),
                severity_group: None,
                message_group: "message".into(),
                name_group: Some("name".into()),
                status_group: Some("status".into()),
                duration_group: None,
            }),
        },
    );
    let mut app = App::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        loaded,
        false,
        state_dir.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((80, 24));
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    app.process_effect(AppEffect::TestRun("unit".into()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.tasks.test_items("unit").is_empty() && Instant::now() < deadline {
        app.process_execution_preflight();
        app.process_job_events();
        thread::sleep(Duration::from_millis(2));
    }
    app.open_builtin_picker(PickerKind::Diagnostics);
    let rows = &app.ui.picker.as_ref().unwrap().rows;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].label, "a.txt:3");
    assert_eq!(rows[0].detail, "adds: wrong sum");
    app.accept_picker();
    wait_vfs(&mut app);
    let view = &app.state.views[app.state.active_view().expect("view")];
    let buffer = &app.state.buffers[view.buffer];
    assert_eq!(
        buffer.path.as_deref(),
        Some(root.path().join("a.txt").as_path())
    );
    assert_eq!(buffer.text.byte_to_line(view.cursor).unwrap(), 2);
    assert_eq!(
        view.cursor.get(),
        buffer.text.line_to_byte(2).unwrap().get() + "é".len()
    );

    // Restarting the finished task runs it again without claiming an error from the stop half.
    app.process_effect(AppEffect::TaskRestart("unit".into()));
    assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
    assert!(app.tasks.test_items("unit").is_empty());
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.tasks.test_items("unit").is_empty() && Instant::now() < deadline {
        app.process_execution_preflight();
        app.process_job_events();
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(app.tasks.test_items("unit").len(), 2);
    app.open_builtin_picker(PickerKind::Diagnostics);
    let rows = &app.ui.picker.as_ref().unwrap().rows;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].label, "a.txt:3");
    assert_eq!(rows[0].detail, "adds: wrong sum");
}

// The task fixtures are POSIX shell scripts.
#[cfg(unix)]
#[test]
fn successful_dependency_diagnostics_keep_separate_stream_records() {
    check_task_diagnostics(true);
}

pub(in crate::app) fn index_bytes(root: &std::path::Path, name: &str) -> String {
    let repo = git2::Repository::open(root).unwrap();
    let index = repo.index().unwrap();
    let entry = index.get_path(std::path::Path::new(name), 0).unwrap();
    String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
}

#[test]
fn named_staging_commands_change_only_the_index() {
    fn index_bytes(root: &std::path::Path, name: &str) -> String {
        let repo = git2::Repository::open(root).unwrap();
        let index = repo.index().unwrap();
        let entry = index.get_path(std::path::Path::new(name), 0).unwrap();
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
    }
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("commands.txt");
    let original = "a\nb\nc\nd\ne\n";
    fs::write(&path, original).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_path(std::path::Path::new("commands.txt"))
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let changed = "a\nB\nc\nNEW\nd\ne\n";
    fs::write(&path, changed).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    assert_eq!(
        app.workspace.trust(),
        crate::workspace::WorkspaceTrust::Trusted
    );
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let invoke = |app: &mut App, id: &str, args: &[(&str, i64)]| {
        let mut invocation =
            crate::editor::CommandInvocation::new(id, InvocationSource::Test).expect("command id");
        for (name, value) in args {
            invocation = invocation.with_argument(name, CommandValue::Integer(*value));
        }
        app.invoke(invocation);
    };
    // A completed mutation clears the session's status snapshot, so the follow-up refresh
    // must also be drained before the next command has a captured state to plan against.
    let wait_mutation = |app: &mut App| {
        let deadline = Instant::now() + Duration::from_secs(4);
        while app.git.mutation_pending() || app.git.status_pending() {
            app.process_git_mutation_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };

    // Whole-file stage moves the index and never the worktree.
    invoke(&mut app, "workspace.git.file.stage", &[]);
    wait_mutation(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "commands.txt"),
        changed
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);

    // Whole-file unstage returns the index entry to HEAD and leaves the worktree alone.
    invoke(&mut app, "workspace.git.file.unstage", &[]);
    wait_mutation(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "commands.txt"),
        original
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);

    // A line range needs loaded hunks; while they load nothing is written.
    let before = index_bytes(&app.state.workspace_root, "commands.txt");
    invoke(
        &mut app,
        "workspace.git.lines.stage",
        &[("start", 0), ("end", 0)],
    );
    assert!(!app.git.mutation_pending());
    assert_eq!(
        index_bytes(&app.state.workspace_root, "commands.txt"),
        before
    );

    app.start_git_hunks();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.hunks_pending() {
        app.process_git_hunk_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let snapshot = app.git.hunks().unwrap().clone();
    assert_eq!(snapshot.hunks.len(), 1, "fixture must produce one hunk");
    assert!(app.git.select(
        snapshot.status_revision,
        snapshot.repository_id,
        snapshot.root_revision,
        &path,
        snapshot.hunks[0].id
    ));

    // A reversed range is refused and writes nothing.
    invoke(
        &mut app,
        "workspace.git.lines.stage",
        &[("start", 3), ("end", 1)],
    );
    assert!(!app.git.mutation_pending());
    assert_eq!(
        index_bytes(&app.state.workspace_root, "commands.txt"),
        original
    );

    // The chosen line replaces the line it changed, and nothing else reaches the index.
    let add = snapshot.hunks[0]
        .lines
        .iter()
        .find(|line| line.kind == crate::git::GitDiffLineKind::Add)
        .and_then(|line| line.new_line)
        .expect("fixture adds a line") as i64;
    invoke(
        &mut app,
        "workspace.git.lines.stage",
        &[("start", add), ("end", add)],
    );
    wait_mutation(&mut app);
    assert_eq!(
        index_bytes(&app.state.workspace_root, "commands.txt"),
        "a\nB\nc\nd\ne\n"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), changed);
}

#[test]
fn staging_an_unsaved_buffer_offers_an_explicit_choice() {
    fn index_bytes(root: &std::path::Path, name: &str) -> String {
        let repo = git2::Repository::open(root).unwrap();
        let index = repo.index().unwrap();
        let entry = index.get_path(std::path::Path::new(name), 0).unwrap();
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
    }
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("choice.txt");
    fs::write(&path, "disk\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("choice.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    // The buffer holds text that is not on disk, at a non-zero revision.
    let id = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[id].text = crate::editor::text::TextBuffer::from_text("buffer\n");
    app.state.buffers[id].dirty = true;
    app.state.buffers[id]
        .text
        .bump_revision()
        .expect("revision bump");
    assert!(app.state.buffers[id].text.revision().0 > 0);

    let wait_for_choice = |app: &mut App| {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            app.process_git_mutation_events();
            if app
                .input
                .prompt()
                .is_some_and(|prompt| prompt.kind == crate::ui::PromptKind::GitStageUnsaved)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no save-and-stage choice appeared"
            );
            thread::yield_now()
        }
    };

    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("stage command"),
    );
    wait_for_choice(&mut app);
    assert_eq!(
        app.input
            .prompt()
            .unwrap()
            .confirmation
            .as_ref()
            .unwrap()
            .selected,
        crate::ui::ConfirmationChoice::No,
        "the choice defaults to No"
    );
    assert_eq!(
        index_bytes(&app.state.workspace_root, "choice.txt"),
        "disk\n"
    );

    // Negative branch: nothing is saved and nothing is staged.
    app.submit_prompt();
    assert_eq!(
        index_bytes(&app.state.workspace_root, "choice.txt"),
        "disk\n"
    );
    assert!(app.state.buffers[id].dirty);
    assert_eq!(fs::read_to_string(&path).unwrap(), "disk\n");

    // Positive branch: save, then stage the saved revision rather than the old disk bytes.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("stage command"),
    );
    wait_for_choice(&mut app);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    wait_vfs(&mut app);
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "buffer\n",
        "the save reached disk"
    );
    assert_eq!(
        index_bytes(&app.state.workspace_root, "choice.txt"),
        "buffer\n",
        "the saved revision was staged, not the previous disk content"
    );
    assert!(!app.state.buffers[id].dirty);
}

#[test]
fn status_reports_a_path_whose_only_change_is_an_unsaved_buffer() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("unsaved-only.txt");
    fs::write(&path, "disk\n").unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_path(std::path::Path::new("unsaved-only.txt"))
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let id = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[id].text = crate::editor::text::TextBuffer::from_text("buffer\n");
    app.state.buffers[id].dirty = true;
    app.state.buffers[id]
        .text
        .bump_revision()
        .expect("revision bump");
    let revision = app.state.buffers[id].text.revision().0;
    assert!(revision > 0);

    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let snapshot = app.git.status().expect("status snapshot").clone();
    // The file matches HEAD on disk, so libgit2 reports no change at all. The entry exists
    // only because the refresh merges unsaved editor content as a third source.
    let entry = snapshot
        .entries
        .iter()
        .find(|entry| entry.relative_path == std::path::Path::new("unsaved-only.txt"))
        .expect("unsaved-only path must appear in status");
    assert_eq!(entry.sources.unsaved_document_revision, Some(revision));
    assert!(!entry.sources.staged);
    assert!(!entry.sources.unstaged);
}

#[test]
fn staging_one_of_two_changed_lines_keeps_the_file_in_order() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("pair.txt");
    let original = (1..=20).map(|n| format!("line {n}\n")).collect::<String>();
    fs::write(&path, &original).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("pair.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    let changed = original
        .replace("line 10\n", "changed 10\n")
        .replace("line 11\n", "changed 11\n");
    fs::write(&path, &changed).unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let settle = |app: &mut App| {
        let deadline = Instant::now() + Duration::from_secs(4);
        while app.git.hunks_pending() || app.git.mutation_pending() || app.git.status_pending() {
            app.process_git_hunk_events();
            app.process_git_mutation_events();
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    app.start_git_status_refresh();
    settle(&mut app);
    let mut invocation =
        crate::editor::CommandInvocation::new("workspace.git.lines.stage", InvocationSource::Test)
            .expect("command id");
    for (name, value) in [("start", 11), ("end", 11)] {
        invocation
            .arguments
            .insert(name.into(), crate::editor::CommandValue::Integer(value));
    }
    app.invoke(invocation);
    settle(&mut app);
    let repo = git2::Repository::open(&app.state.workspace_root).unwrap();
    let index = repo.index().unwrap();
    let entry = index.get_path(std::path::Path::new("pair.txt"), 0).unwrap();
    assert_eq!(
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap(),
        original.replace("line 11\n", "changed 11\n"),
        "{:?}",
        app.ui.notification
    );
}

/// Build a repository with distinct commit subjects and a configured identity.
pub(in crate::app) fn history_fixture(
    root: &std::path::Path,
    subjects: &[&str],
) -> git2::Repository {
    let repo = git2::Repository::init(root).unwrap();
    {
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Dust Fixture").unwrap();
        config
            .set_str("user.email", "dust@example.invalid")
            .unwrap();
    }
    for (index, subject) in subjects.iter().enumerate() {
        let name = format!("h{index}.txt");
        fs::write(root.join(&name), "x\n").unwrap();
        let mut index_file = repo.index().unwrap();
        index_file.add_path(std::path::Path::new(&name)).unwrap();
        index_file.write().unwrap();
        let tree = repo.find_tree(index_file.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("Dust Fixture", "dust@example.invalid").unwrap();
        let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, subject, &tree, &parents)
            .unwrap();
    }
    // A clone so the caller keeps a handle whose index is not cached stale.
    git2::Repository::open(root).unwrap()
}

#[test]
fn help_preserves_a_buffer_selection_byte_for_byte() {
    let (_app_dirs, mut app) = app();
    let (buffer, view) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("alpha beta gamma delta\n"),
            None,
        )
        .unwrap();
    app.state.focus_view(view).unwrap();
    app.state.views[view].buffer = buffer;
    app.state.views[view].cursor = crate::editor::text::ByteOffset(15);
    app.state.views[view].mode = crate::editor::EditorMode::VisualChar;
    app.state.views[view].selection = Some(crate::editor::Selection {
        anchor: crate::editor::text::ByteOffset(6),
        head: crate::editor::text::ByteOffset(15),
        kind: crate::editor::SelectionKind::Character,
    });
    let before = app.state.views[view].selection;
    let mode_before = app.state.views[view].mode;

    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.contextual", InvocationSource::Test)
            .expect("command id"),
    );
    assert!(app.search.picker_projection().is_some(), "help opened");
    app.search.close_picker();

    assert_eq!(
        app.state.views[view].selection, before,
        "the selection is identical, anchor and head, after help"
    );
    assert_eq!(
        app.state.views[view].mode, mode_before,
        "help does not drop the editor out of visual mode"
    );
}

#[test]
fn the_advertised_help_key_is_the_key_that_resolves() {
    let (_app_dirs, mut app) = app();
    app.open_confirmation(
        crate::ui::PromptKind::Command,
        "Discard changes?".into(),
        "The buffer has unsaved edits.".into(),
        crate::ui::ConfirmationChoice::No,
    );
    app.project_input_prompt();
    let hint = app
        .ui
        .prompt
        .as_ref()
        .and_then(|prompt| prompt.confirmation.as_ref())
        .expect("a confirmation is pending")
        .hint
        .clone();

    // The hint advertises "<key> help"; the keymap must resolve that exact key to the command
    // the hint is advertising, so the advertisement and the dispatch cannot drift apart.
    let advertised = hint
        .split(" · ")
        .find_map(|part| part.strip_suffix(" help"))
        .expect("the hint advertises a help key")
        .to_owned();
    let resolved = app
        .configuration
        .keymaps()
        .inspect_key("confirmation_prompt", &advertised)
        .unwrap_or_else(|| panic!("the advertised key {advertised:?} does not resolve"));
    assert_eq!(
        resolved.command.as_deref(),
        Some("overlay.help.form"),
        "the advertised key {advertised:?} dispatches exactly the command the hint names"
    );

    // And the composer form advertises its own key the same way.
    let composer = app
        .configuration
        .keymaps()
        .binding_for("command_prompt", "overlay.help.form")
        .expect("the form has a help binding");
    assert_eq!(composer, "ctrl-g");
    let resolved = app
        .configuration
        .keymaps()
        .inspect_key("command_prompt", composer)
        .expect("the form's advertised key resolves");
    assert_eq!(resolved.command.as_deref(), Some("overlay.help.form"));
}

#[test]
fn accepting_inside_help_closes_the_page_and_invokes_nothing() {
    let (_app_dirs, mut app) = app();
    app.open_builtin_picker(crate::app::commands::PickerKind::Commands);
    app.picker_insert("overlay.help.reference");
    let palette = app.search.picker_projection().expect("palette");
    let query_before = palette.query.clone();
    let selected_before = palette.selected.expect("a row is selected");

    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.reference", InvocationSource::Test)
            .expect("command id"),
    );
    assert!(
        app.search.picker_projection().is_some(),
        "help opened over the palette"
    );

    // MEASURED: accepting a help row must close the page and return the palette. It must not
    // run the row's command and must not leave the help page on screen.
    app.accept_picker();
    let restored = app
        .search
        .picker_projection()
        .expect("accepting help returned the palette rather than leaving help up");
    assert_eq!(
        restored.query, query_before,
        "the palette came back with its query"
    );
    assert_eq!(
        restored.selected,
        Some(selected_before),
        "the palette came back with its selected row"
    );
    assert_ne!(
        restored.title, "Help · overlay.help.reference",
        "the help page is gone, not re-opened by its own row payload"
    );
}

#[test]
fn a_surface_installed_over_help_discards_the_snapshot() {
    let (_app_dirs, mut app) = app();
    app.open_builtin_picker(crate::app::commands::PickerKind::Commands);
    app.picker_insert("overlay.help.reference");
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.reference", InvocationSource::Test)
            .expect("command id"),
    );
    assert!(app.search.picker_projection().is_some(), "help is up");

    // A real surface is installed while help is open. The stale palette must not come back
    // when that new surface is closed.
    app.open_builtin_picker(crate::app::commands::PickerKind::Buffers);
    assert!(
        app.search.picker_projection().is_some(),
        "the new surface is up"
    );
    app.search.close_picker();
    assert!(
        app.search.picker_projection().is_none(),
        "closing the new surface leaves nothing: it did not restore the palette help was over"
    );
}

#[test]
fn help_opened_over_help_keeps_the_original_surface() {
    let (_app_dirs, mut app) = app();
    app.open_builtin_picker(crate::app::commands::PickerKind::Commands);
    app.picker_insert("overlay.help.reference");
    let palette = app.search.picker_projection().expect("palette");
    let query_before = palette.query.clone();

    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.reference", InvocationSource::Test)
            .expect("command id"),
    );
    let first = app
        .search
        .picker_projection()
        .expect("first help page")
        .title
        .clone();
    // A second help page over the first.
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.contextual", InvocationSource::Test)
            .expect("command id"),
    );
    let second = app
        .search
        .picker_projection()
        .expect("second help page")
        .title
        .clone();
    assert_ne!(first, second, "a different help page is on screen");

    // MEASURED: closing returns the surface the user started from, not the intermediate help
    // page, and the snapshot never becomes a stack.
    app.search.close_picker();
    let restored = app
        .search
        .picker_projection()
        .expect("closing help-over-help returned the original surface");
    assert_eq!(
        restored.query, query_before,
        "the original palette came back with its query"
    );
    assert_ne!(
        restored.title, first,
        "the intermediate help page was dropped rather than stacked"
    );
}

#[test]
fn choosing_a_side_edits_the_result_buffer_and_leaves_save_and_stage_separate() {
    let (_app_dirs, mut app) = app();
    let repo = history_fixture(&app.state.workspace_root, &["base"]);
    let root = app.state.workspace_root.clone();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let base_tip = repo.head().unwrap().target().unwrap();
    let base_commit = repo.find_commit(base_tip).unwrap();
    repo.branch("theirs", &base_commit, false).unwrap();
    {
        let mut builder = repo
            .treebuilder(Some(&base_commit.tree().unwrap()))
            .unwrap();
        let blob = repo.blob(b"\xef\xbb\xbfTHEIR VERSION\r\n").unwrap();
        builder.insert("h0.txt", blob, 0o100644).unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        repo.commit(
            Some("refs/heads/theirs"),
            &sig,
            &sig,
            "theirs",
            &tree,
            &[&base_commit],
        )
        .unwrap();
    }
    fs::write(root.join("h0.txt"), "OUR VERSION\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("h0.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "ours", &tree, &[&base_commit])
            .unwrap();
    }
    let their_tip = repo
        .find_reference("refs/heads/theirs")
        .unwrap()
        .target()
        .unwrap();
    let their_commit = repo.find_commit(their_tip).unwrap();
    let annotated = repo.find_annotated_commit(their_commit.id()).unwrap();
    repo.merge(&[&annotated], None, None).unwrap();
    drop(annotated);
    drop(their_commit);
    drop(base_commit);

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    // The RESULT buffer is the working-tree file git wrote with conflict markers.
    let conflicted = root.join("h0.txt");
    let on_disk = fs::read(&conflicted).unwrap();
    assert!(
        String::from_utf8_lossy(&on_disk).contains("<<<<<<<"),
        "the working tree really holds conflict markers: {:?}",
        String::from_utf8_lossy(&on_disk)
    );
    let mut bom_contents = b"\xef\xbb\xbf".to_vec();
    bom_contents.extend_from_slice(&on_disk);
    let (buffer, view) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_bytes(&bom_contents).unwrap(),
            Some(conflicted.clone()),
        )
        .unwrap();
    app.state.focus_view(view).unwrap();
    app.state.views[view].buffer = buffer;
    let before_choose = app.state.buffers[buffer].text.to_string();
    let index_before = crate::git::git_index_fingerprint(&root).unwrap();

    // ---- CHOOSING A SIDE.
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.choose",
            InvocationSource::Test,
        )
        .expect("command id")
        .with_argument("side", CommandValue::String("theirs".into())),
    );
    assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
    let content =
        crate::git::read_side_content(&root, &conflicted, crate::git::GitConflictChoice::Theirs)
            .expect("the theirs side is readable");
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        crate::editor::text::decode_utf8(content.as_bytes())
            .unwrap()
            .text,
        "the result buffer holds exactly the chosen side's decoded content"
    );
    assert!(
        app.state.buffers[buffer].dirty,
        "the buffer is dirty after the choice"
    );
    assert_eq!(
        configured_file_bytes(
            &app.state.buffers[buffer].text,
            &app.configuration.settings().editor
        ),
        b"\xef\xbb\xbfTHEIR VERSION\n",
        "saving keeps one metadata BOM, not the blob's BOM in the text"
    );
    // SAVING IS SEPARATE: the file on disk is byte-identical until an explicit save.
    assert_eq!(
        fs::read(&conflicted).unwrap(),
        on_disk,
        "the file on disk is untouched by choosing a side"
    );
    // STAGING IS SEPARATE: the index is unchanged until an explicit stage.
    assert_eq!(
        crate::git::git_index_fingerprint(&root).unwrap(),
        index_before,
        "the index is untouched by choosing a side"
    );

    // ---- UNDO goes through the editor's own path, which is what proves this was an ordinary
    // transaction rather than a side door.
    app.invoke(
        crate::editor::CommandInvocation::new("history.undo", InvocationSource::Test)
            .expect("command id"),
    );
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        before_choose,
        "undo restored the conflict markers, so the change was an ordinary transaction"
    );
}

#[test]
fn a_previewed_replacement_writes_nothing_until_it_is_confirmed() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    let (our_tip, _their_tip) = {
        let repo = history_fixture(&root, &["base"]);
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        let base_tip = repo.head().unwrap().target().unwrap();
        let base_commit = repo.find_commit(base_tip).unwrap();
        repo.branch("theirs", &base_commit, false).unwrap();
        {
            let mut builder = repo
                .treebuilder(Some(&base_commit.tree().unwrap()))
                .unwrap();
            let blob = repo.blob(b"THEIR VERSION\n").unwrap();
            builder.insert("h0.txt", blob, 0o100644).unwrap();
            let tree = repo.find_tree(builder.write().unwrap()).unwrap();
            repo.commit(
                Some("refs/heads/theirs"),
                &sig,
                &sig,
                "theirs",
                &tree,
                &[&base_commit],
            )
            .unwrap();
        }
        fs::write(root.join("h0.txt"), "OUR VERSION\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(std::path::Path::new("h0.txt")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "ours", &tree, &[&base_commit])
                .unwrap();
        }
        let our_tip = repo.head().unwrap().target().unwrap();
        let their_tip = repo
            .find_reference("refs/heads/theirs")
            .unwrap()
            .target()
            .unwrap();
        let their_commit = repo.find_commit(their_tip).unwrap();
        let annotated = repo.find_annotated_commit(their_commit.id()).unwrap();
        repo.merge(&[&annotated], None, None).unwrap();
        repo.reference("ORIG_HEAD", our_tip, true, "fixture")
            .unwrap();
        (our_tip, their_tip)
    };

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let target = root.join("h0.txt");
    let before = fs::read(&target).unwrap();
    let index_before = crate::git::git_index_fingerprint(&root).unwrap();

    // PREVIEW: a confirmation naming the file and the side, and nothing written yet.
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.replace",
            InvocationSource::Test,
        )
        .expect("command id")
        .with_argument("side", CommandValue::String("theirs".into()))
        .with_argument("files", CommandValue::String("h0.txt".into())),
    );
    app.project_input_prompt();
    let confirmation = app
        .ui
        .prompt
        .as_ref()
        .and_then(|prompt| prompt.confirmation.as_ref())
        .expect("the preview asks for confirmation");
    assert!(
        confirmation.message.contains("h0.txt") && confirmation.message.contains("theirs"),
        "the preview names the file and the side: {:?}",
        confirmation.message
    );
    assert_eq!(
        confirmation.selected,
        crate::ui::ConfirmationChoice::No,
        "the confirmation defaults to No"
    );
    assert_eq!(
        fs::read(&target).unwrap(),
        before,
        "nothing is written while the question is open"
    );
    assert!(app.git.has_pending_replacement());

    // CANCEL: answering No writes nothing at all.
    app.submit_prompt();
    assert!(
        !app.git.has_pending_replacement(),
        "the question is answered once"
    );
    assert_eq!(
        fs::read(&target).unwrap(),
        before,
        "a cancelled preview leaves the file byte-identical"
    );
    assert_eq!(
        crate::git::git_index_fingerprint(&root).unwrap(),
        index_before,
        "and the index untouched"
    );
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|notice| notice.contains("cancelled")),
        "and says so: {:?}",
        app.ui.notification
    );

    // Escape must close the preview before a worktree-removal confirmation is opened.
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.replace",
            InvocationSource::Test,
        )
        .unwrap()
        .with_argument("side", CommandValue::String("theirs".into()))
        .with_argument("files", CommandValue::String("h0.txt".into())),
    );
    app.handle_configured_key("escape".parse().unwrap());
    assert!(!app.git.has_pending_replacement());
    app.install_picker(
        "Worktrees".into(),
        vec![(
            crate::workspace::PickerItem {
                badge: String::new(),
                kind: crate::workspace::PickerItemKind::Entry,
                id: crate::workspace::PickerItemId("missing-worktree".into()),
                label: "missing-worktree".into(),
                detail: String::new(),
                source: "worktrees".into(),
                action: crate::workspace::PickerAction::None,
            },
            crate::app::picker::PickerPayload::GitWorktree {
                main: false,
                name: "missing-worktree".into(),
                path: root.join("missing-worktree"),
            },
        )],
    );
    app.request_git_worktree_remove();
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    while app.git.branch_pending() {
        app.process_git_branch_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert_eq!(
        fs::read(&target).unwrap(),
        before,
        "removal confirmation must not replace conflicted files"
    );
    assert!(
        app.ui.notification_error,
        "the nonexistent worktree removal was attempted"
    );

    // Replacing the confirmation with another prompt must also discard its plan.
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.replace",
            InvocationSource::Test,
        )
        .unwrap()
        .with_argument("side", CommandValue::String("theirs".into()))
        .with_argument("files", CommandValue::String("h0.txt".into())),
    );
    app.open_prompt(crate::ui::PromptKind::Command, String::new());
    assert!(!app.git.has_pending_replacement());
    app.close_prompt();

    // CONFIRM: the file is replaced with the chosen side's content.
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.replace",
            InvocationSource::Test,
        )
        .expect("command id")
        .with_argument("side", CommandValue::String("theirs".into()))
        .with_argument("files", CommandValue::String("h0.txt".into())),
    );
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert_eq!(
        fs::read(&target).unwrap(),
        b"THEIR VERSION\n",
        "the confirmed replacement wrote exactly the chosen side's content"
    );
    assert!(!app.git.has_pending_replacement());
    let _ = (our_tip, index_before);
}

#[test]
fn the_changes_surface_renders_its_sections_columns_badge_and_footer() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let write = |name: &str, text: &str| {
        fs::write(app.state.workspace_root.join(name), text).unwrap();
    };
    let stage = |name: &str| {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
    };
    write("staged.txt", "one\n");
    write("unstaged.txt", "one\n");
    write("unsaved.txt", "one\n");
    write("conflicted.txt", "one\n");
    for name in [
        "staged.txt",
        "unstaged.txt",
        "unsaved.txt",
        "conflicted.txt",
    ] {
        stage(name);
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();

    // A real conflict: both sides change the same line.
    let base = repo.head().unwrap().peel_to_commit().unwrap();
    write("conflicted.txt", "ours\n");
    stage("conflicted.txt");
    let ours = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let ours = repo
        .commit(Some("refs/heads/side"), &sig, &sig, "ours", &ours, &[&base])
        .unwrap();
    write("conflicted.txt", "theirs\n");
    stage("conflicted.txt");
    let theirs = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    let theirs = repo
        .commit(
            Some("refs/heads/other"),
            &sig,
            &sig,
            "theirs",
            &theirs,
            &[&base],
        )
        .unwrap();
    let ours_annotated = repo.find_annotated_commit(ours).unwrap();
    let theirs_annotated = repo.find_annotated_commit(theirs).unwrap();
    // Merge into the side that carries `ours`, so the two sides really meet.
    repo.set_head("refs/heads/side").unwrap();
    repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
        .unwrap();
    let _ = ours_annotated;
    repo.merge(&[&theirs_annotated], None, None)
        .expect("the merge runs");
    assert!(
        repo.index().unwrap().has_conflicts(),
        "both sides changed the same line, so the index is conflicted"
    );

    // One staged change, one unstaged change, and one path with only unsaved edits.
    write("staged.txt", "two\n");
    stage("staged.txt");
    write("unstaged.txt", "two\n");
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(app.state.workspace_root.join("unsaved.txt"));
    wait_vfs(&mut app);
    let id = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[id].text = crate::editor::text::TextBuffer::from_text("buffer\n");
    app.state.buffers[id].dirty = true;
    app.state.buffers[id]
        .text
        .bump_revision()
        .expect("revision bump");

    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    // Render through the application's own path, which places the surface's window.
    app.handle_event(crate::ui::UiEvent::Resize(220, 50));
    app.reconcile_editor_windows();
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    let rendered: Vec<String> = (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect();
    let has = |needle: &str| rendered.iter().any(|row| row.contains(needle));

    // The three sections, in the order the document draws them, each naming its count.
    assert!(
        has("── Conflicts 1"),
        "the Conflicts section header is drawn: {rendered:?}"
    );
    assert!(has("── Unstaged 2"), "the Unstaged section header is drawn");
    assert!(has("── Staged 1"), "the Staged section header is drawn");
    let conflicts = rendered
        .iter()
        .position(|row| row.contains("── Conflicts"))
        .expect("conflicts header");
    let unstaged = rendered
        .iter()
        .position(|row| row.contains("── Unstaged"))
        .expect("unstaged header");
    let staged = rendered
        .iter()
        .position(|row| row.contains("── Staged"))
        .expect("staged header");
    assert!(
        conflicts < unstaged && unstaged < staged,
        "the sections keep the drawn order"
    );

    // The status column carries a two-character code followed by the full path; the section
    // header already names the comparison.
    assert!(
        has("UU conflicted.txt"),
        "a conflicted path shows its code: {rendered:?}"
    );
    assert!(has("M  staged.txt"), "a staged path shows its code");
    assert!(has(" M unstaged.txt"), "an unstaged path shows its code");
    assert!(has("unsaved.txt"), "an unsaved-only path is listed");
    // The badge is its own drawn column with its own style. Asserting merely that the word
    // "unsaved" appears would be satisfied by the path `unsaved.txt`, so this reads the
    // styled run that only the badge column produces.
    let badge_row = u16::try_from(
        rendered
            .iter()
            .enumerate()
            // Not the buffer tab or the "opened …/unsaved.txt" notice above the sections.
            .position(|(row, text)| {
                row > conflicts && text.contains("unsaved.txt") && !text.contains("opened")
            })
            .expect("the unsaved-only row"),
    )
    .expect("row index");
    let mut runs: Vec<String> = Vec::new();
    let mut run = String::new();
    for x in 0..frame.grid.width() {
        let cell = frame.grid.get(x, badge_row);
        if cell.is_some_and(|cell| cell.style == crate::ui::Style::Warning) {
            run.push_str(&cell.expect("a cell").symbol);
        } else if !run.is_empty() {
            runs.push(std::mem::take(&mut run));
        }
    }
    if !run.is_empty() {
        runs.push(run);
    }
    assert!(
        runs.iter().any(|run| run.trim() == "unsaved"),
        "the badge column carries the badge: {runs:?}"
    );
    // The footer describes the operation rather than the widget's mode.
    assert!(
        has("s Stage"),
        "the footer names what the key does here: {rendered:?}"
    );
    assert!(has("u Unstage"), "the footer names the inverse action");
    assert!(has("Tab Diff"), "the footer names the focus switch");
    assert!(
        !has("Insert · escape normal"),
        "the widget's mode hint is replaced by the surface's own footer"
    );

    // Read only the footer text, not its surrounding border, on a surface that truncates it.
    let legend = app.ui.picker.as_ref().unwrap().footer.clone().unwrap();
    app.handle_event(crate::ui::UiEvent::Resize(55, 24));
    app.reconcile_editor_windows();
    let narrow = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    let rect = app
        .ui
        .surface_content
        .picker
        .expect("picker geometry")
        .outer;
    let footer_y = rect.y + rect.height - 1;
    let footer = (rect.x + 2..rect.x + rect.width - 1)
        .filter_map(|x| narrow.grid.get(x, footer_y))
        .filter(|cell| cell.style == crate::ui::Style::Muted)
        .map(|cell| cell.symbol.as_str())
        .collect::<String>();
    let entries = footer.trim().split(" · ").collect::<Vec<_>>();
    let full_entries = legend.split(" · ").collect::<Vec<_>>();
    assert!(
        !entries.is_empty() && entries.len() < full_entries.len(),
        "{footer:?}"
    );
    assert_eq!(
        entries,
        full_entries[..entries.len()],
        "only complete legend entries: {footer:?}"
    );
}

pub(in crate::app) fn wait_changes_preview(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !app
        .search
        .preview()
        .as_ref()
        .is_some_and(|preview| matches!(preview.body, crate::ui::PickerPreviewBody::Diff(_)))
    {
        app.process_git_hunk_events();
        assert!(Instant::now() < deadline, "{:?}", app.search.preview());
        thread::yield_now();
    }
}

/// `:e` on a path with nothing there starts that file, and the first save creates it.
#[test]
fn editing_a_missing_path_starts_a_new_file_that_saving_creates() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("new-file.txt");
    app.edit_path(PathBuf::from("new-file.txt"));
    let buffer = app
        .state
        .active_buffer_id()
        .expect("the new file is on screen");
    assert_eq!(
        app.state.buffers[buffer]
            .path
            .as_deref()
            .and_then(std::path::Path::file_name),
        path.file_name()
    );
    assert!(!path.exists(), "nothing is written before the save");
    app.handle_configured_key(KeyEvent::character('i'));
    app.handle_configured_key(KeyEvent::character('o'));
    app.handle_configured_key("escape".parse::<KeyEvent>().expect("escape"));
    let save = crate::editor::CommandInvocation::new("file.save", InvocationSource::Test)
        .expect("static command");
    app.invoke(save);
    crate::app::tests::wait_vfs(&mut app);
    let saved = fs::read_to_string(&path).expect("the save created the file");
    assert!(saved.contains('o'), "{saved:?}");
    assert!(!app.state.buffers[buffer].new_file);
}

#[test]
fn a_change_refresh_keeps_the_selection_on_the_same_row() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(app.state.workspace_root.join(name), "one\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new(name)).unwrap();
        index.write().unwrap();
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(app.state.workspace_root.join(name), "two\n").unwrap();
    }
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let refresh = |app: &mut App| {
        app.start_git_status_refresh();
        let deadline = Instant::now() + Duration::from_secs(4);
        while app.git.status_pending() {
            app.process_git_status_events();
            assert!(Instant::now() < deadline);
            thread::yield_now()
        }
    };
    refresh(&mut app);
    app.open_git_status_picker();
    let selected_id = |app: &App| {
        let picker = app.ui.picker.as_ref().expect("picker");
        picker.selected.map(|index| picker.rows[index].id.clone())
    };
    // Move off the first entry so a reset to position 0 would be visible.
    app.picker_move(1);
    let before = selected_id(&app).expect("a selected row");
    assert!(!before.is_empty(), "the selected row has an identity");

    refresh(&mut app);
    app.open_git_status_picker();
    let after = selected_id(&app).expect("a selected row after the refresh");
    assert_eq!(
        before, after,
        "the refresh left the cursor on the same row rather than moving it"
    );
}

#[test]
fn staging_a_row_moves_it_to_the_staged_section_of_the_open_list() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    let path = app.state.workspace_root.join("changed.txt");
    fs::write(&path, "base\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("changed.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    drop(tree);
    fs::write(&path, "changed\n").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    let labels = |app: &App| -> Vec<String> {
        app.ui
            .picker
            .as_ref()
            .expect("picker")
            .rows
            .iter()
            .map(|row| row.label.clone())
            .collect()
    };
    assert!(
        labels(&app).iter().any(|label| label == "Unstaged 1"),
        "{:?}",
        labels(&app)
    );
    app.input.set_picker_normal(true);
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("static command"),
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    while !labels(&app).iter().any(|label| label == "Staged 1") {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline, "{:?}", labels(&app));
        thread::yield_now()
    }
    assert!(
        !labels(&app).iter().any(|label| label == "Unstaged 1"),
        "{:?}",
        labels(&app)
    );
    assert!(
        app.ui.picker.as_ref().expect("picker").selected.is_some(),
        "a row stays selected"
    );
    assert!(
        app.input.picker_normal(),
        "the next s or u still acts instead of searching"
    );
}

#[test]
fn ctrl_q_sends_the_change_rows_to_the_results_surface() {
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    fs::write(app.state.workspace_root.join("a.txt"), "one\n").unwrap();
    {
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("a.txt")).unwrap();
        index.write().unwrap();
    }
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "base", &tree, &[])
        .unwrap();
    drop(tree);
    fs::write(app.state.workspace_root.join("a.txt"), "two\n").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    app.open_git_status_picker();
    assert_eq!(
        app.search.picker_surface(),
        crate::ui::PickerSurfaceKind::Changes,
        "the surface starts as the change list"
    );
    let rows_before = app.ui.picker.as_ref().expect("picker").rows.len();
    app.invoke(
        crate::editor::CommandInvocation::new("picker.results", InvocationSource::Test)
            .expect("static command"),
    );
    assert_eq!(
        app.search.picker_surface(),
        crate::ui::PickerSurfaceKind::Problems,
        "the rows now live on the shared Results surface"
    );
    assert_eq!(
        app.ui.picker.as_ref().expect("picker").rows.len(),
        rows_before,
        "sending to Results keeps the rows rather than dropping them"
    );
}

#[test]
fn opening_changes_before_status_arrives_opens_them_when_it_does() {
    let (_app_dirs, mut app) = app();
    git2::Repository::init(&app.state.workspace_root).unwrap();
    fs::write(app.state.workspace_root.join("a.txt"), "one\n").unwrap();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.open_git_status_picker();
    assert!(
        app.ui.picker.is_none(),
        "nothing opens before status exists"
    );
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert_eq!(
        app.search.picker_surface(),
        crate::ui::PickerSurfaceKind::Changes,
        "the change list opens once status arrives"
    );
}

#[test]
fn command_line_completes_commands_without_an_alias_and_fills_in_their_arguments() {
    let (_app_dirs, mut app) = app();
    app.open_prompt(crate::ui::PromptKind::Command, "wor".into());
    app.refresh_completions();
    let completions = |app: &App| -> Vec<String> {
        app.input
            .prompt()
            .unwrap()
            .completions
            .iter()
            .map(|item| item.label.clone())
            .collect()
    };
    let rows = completions(&app);
    assert_eq!(rows[0], "worktree", "{rows:?}");
    assert!(
        rows.iter()
            .any(|row| row == "workspace.git.worktree.remove"),
        "{rows:?}"
    );
    app.input.prompt_complete(1);
    assert!(app.input.accept_completion(app.configuration.settings()));
    let prompt = app.input.prompt().expect("completed prompt");
    assert_eq!(prompt.input, "worktree ");
    assert_eq!(prompt.cursor, "worktree ".len());

    app.close_prompt();
    app.open_prompt(crate::ui::PromptKind::Command, String::new());
    app.refresh_completions();
    let rows = completions(&app);
    assert_eq!(rows[0].len(), 1, "short aliases come first: {rows:?}");
    assert!(
        rows.iter().any(|row| row == "workspace.git.hunk.stage"),
        "{rows:?}"
    );
    assert!(
        !rows.iter().any(|row| row.starts_with("input.")),
        "{rows:?}"
    );
}

#[test]
fn completing_a_path_after_w_keeps_the_save_command() {
    let (_app_dirs, mut app) = app();
    fs::write(app.state.workspace_root.join("q!"), b"").expect("file");
    app.open_prompt(crate::ui::PromptKind::Command, "w q".into());
    wait_vfs(&mut app);
    app.input.prompt_complete(1);
    assert!(app.input.accept_completion(app.configuration.settings()));
    assert_eq!(app.input.prompt().expect("prompt").input, "w q!");
}

#[test]
fn deferred_navigation_does_not_replace_a_newer_buffer_choice() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("late.txt");
    fs::write(&path, "destination").unwrap();
    app.navigate_matcher_source(crate::app::picker::MatcherSource {
        path,
        line: 0,
        column: 0,
        encoding: crate::editor::text::PositionEncoding::Utf8,
    });
    let (newer, _) = app
        .state
        .open_text(TextBuffer::from_text("newer choice"), None)
        .unwrap();
    wait_vfs(&mut app);
    assert_eq!(app.state.active_buffer_id(), Some(newer));
}

#[test]
fn location_labels_read_only_the_required_prefix_of_a_large_file() {
    let (_app_dirs, app) = app();
    let path = app.state.workspace_root.join("sparse.txt");
    let file = fs::File::create(&path).unwrap();
    file.set_len(4 * 1024 * 1024 * 1024).unwrap();
    let label = app.persisted_location_label(&crate::persistence::PersistedLocation {
        path,
        byte: 1,
        anchor: 1,
        scroll_line: 0,
        scroll_column: 0,
    });
    assert!(label.ends_with(":1"));
}

#[test]
fn failed_and_cancelled_staging_never_replays_on_an_ordinary_save() {
    let (_app_dirs, mut app, _repo, path) = near_changes_fixture();
    let index_before = index_bytes(&app.state.workspace_root, "near.txt");
    app.process_effect(AppEffect::GitLinesStage {
        start: 999,
        end: 999,
    });
    settle_git_work(&mut app);
    assert!(app.input.take_pending_stage().is_none());
    let buffer = app.state.active_buffer_id().unwrap();
    app.replace_with_recovered(buffer, "ordinary save after failure\n".into())
        .unwrap();
    app.start_save(false, None, false);
    wait_vfs(&mut app);
    settle_git_work(&mut app);
    assert!(!app.git.mutation_pending());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "ordinary save after failure\n"
    );
    assert_eq!(
        index_bytes(&app.state.workspace_root, "near.txt"),
        index_before
    );
    app.replace_with_recovered(buffer, "changed unsaved\n".into())
        .unwrap();
    app.process_effect(AppEffect::GitFileStage);
    settle_git_work(&mut app);
    assert_eq!(
        app.input
            .prompt()
            .unwrap_or_else(|| panic!("missing stage prompt: {:?}", app.ui.notification))
            .kind,
        crate::ui::PromptKind::GitStageUnsaved
    );
    app.close_prompt();
    app.start_save(false, None, false);
    wait_vfs(&mut app);
    assert!(!app.git.mutation_pending());
    assert_eq!(fs::read_to_string(path).unwrap(), "changed unsaved\n");
    assert_eq!(
        index_bytes(&app.state.workspace_root, "near.txt"),
        index_before
    );
    assert!(app.input.take_pending_stage().is_none());
}

#[test]
fn disk_polling_retries_reloads_refused_by_document_capacity() {
    let (_app_dirs, mut app) = app();
    let mut buffers = Vec::new();
    for index in 0..9 {
        let path = app.state.workspace_root.join(format!("poll-{index}"));
        fs::write(&path, "old").unwrap();
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("old"), Some(path.clone()))
            .unwrap();
        buffers.push((buffer, path));
    }
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    for (_, path) in &buffers {
        fs::write(path, "new contents").unwrap();
    }
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    assert!(
        buffers
            .iter()
            .all(|(buffer, _)| app.state.buffers[*buffer].text.to_string() == "new contents")
    );
}

#[test]
fn failed_save_and_stage_does_not_leak_into_a_later_save() {
    let (_app_dirs, mut app, repo, path) = near_changes_fixture();
    let buffer = app.state.active_buffer_id().unwrap();
    app.replace_with_recovered(buffer, "unsaved\n".into())
        .unwrap();
    app.process_effect(AppEffect::GitFileStage);
    settle_git_work(&mut app);
    fs::write(&path, "external change\n").unwrap();
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(app.input.prompt().is_none());
    wait_vfs(&mut app);
    assert!(app.ui.notification_error);
    app.start_save(false, None, true);
    wait_vfs(&mut app);
    settle_git_work(&mut app);
    let mut index = repo.index().unwrap();
    index.read(true).unwrap();
    let blob = repo
        .find_blob(
            index
                .get_path(std::path::Path::new("near.txt"), 0)
                .unwrap()
                .id,
        )
        .unwrap();
    assert_eq!(blob.content(), b"one\ntwo\nthree\nfour\nfive\n");
}

#[test]
fn staging_an_unsaved_buffer_offers_an_explicit_choice_slot5() {
    fn index_bytes(root: &std::path::Path, name: &str) -> String {
        let repo = git2::Repository::open(root).unwrap();
        let index = repo.index().unwrap();
        let entry = index.get_path(std::path::Path::new(name), 0).unwrap();
        String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
    }
    let (_app_dirs, mut app) = app();
    let repo = git2::Repository::init(&app.state.workspace_root).unwrap();
    let path = app.state.workspace_root.join("choice.txt");
    fs::write(&path, "disk\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("choice.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "fixture", &tree, &[])
        .unwrap();
    drop(tree);
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }

    // The buffer holds text that is not on disk, at a non-zero revision.
    let id = app.state.active_buffer_id().expect("buffer");
    app.state.buffers[id].text = crate::editor::text::TextBuffer::from_text("buffer\n");
    app.state.buffers[id].dirty = true;
    app.state.buffers[id]
        .text
        .bump_revision()
        .expect("revision bump");
    assert!(app.state.buffers[id].text.revision().0 > 0);

    let wait_for_choice = |app: &mut App| {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            app.process_git_mutation_events();
            if app
                .input
                .prompt()
                .is_some_and(|prompt| prompt.kind == crate::ui::PromptKind::GitStageUnsaved)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no save-and-stage choice appeared"
            );
            thread::yield_now()
        }
    };

    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("stage command"),
    );
    wait_for_choice(&mut app);
    assert_eq!(
        app.input
            .prompt()
            .unwrap()
            .confirmation
            .as_ref()
            .unwrap()
            .selected,
        crate::ui::ConfirmationChoice::No,
        "the choice defaults to No"
    );
    assert_eq!(
        index_bytes(&app.state.workspace_root, "choice.txt"),
        "disk\n"
    );

    // Negative branch: nothing is saved and nothing is staged.
    app.submit_prompt();
    assert_eq!(
        index_bytes(&app.state.workspace_root, "choice.txt"),
        "disk\n"
    );
    assert!(app.state.buffers[id].dirty);
    assert_eq!(fs::read_to_string(&path).unwrap(), "disk\n");

    // Positive branch: save, then stage the saved revision rather than the old disk bytes.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.stage", InvocationSource::Test)
            .expect("stage command"),
    );
    // A second request must not erase the first request's save-and-stage intent.
    app.invoke(
        crate::editor::CommandInvocation::new("workspace.git.file.unstage", InvocationSource::Test)
            .expect("unstage command"),
    );
    assert_eq!(
        app.ui.notification.as_deref(),
        Some("a Git mutation is already running")
    );
    wait_for_choice(&mut app);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    wait_vfs(&mut app);
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.mutation_pending() || app.git.status_pending() {
        app.process_git_mutation_events();
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "buffer\n",
        "the save reached disk"
    );
    assert_eq!(
        index_bytes(&app.state.workspace_root, "choice.txt"),
        "buffer\n",
        "the saved revision was staged, not the previous disk content"
    );
    assert!(!app.state.buffers[id].dirty);
}

#[test]
fn a_previewed_replacement_writes_nothing_until_it_is_confirmed_slot5() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    let (our_tip, _their_tip) = {
        let repo = history_fixture(&root, &["base"]);
        let sig = git2::Signature::now("Dust", "dust@example.invalid").unwrap();
        let base_tip = repo.head().unwrap().target().unwrap();
        let base_commit = repo.find_commit(base_tip).unwrap();
        repo.branch("theirs", &base_commit, false).unwrap();
        {
            let mut builder = repo
                .treebuilder(Some(&base_commit.tree().unwrap()))
                .unwrap();
            let blob = repo.blob(b"THEIR VERSION\n").unwrap();
            builder.insert("h0.txt", blob, 0o100644).unwrap();
            let tree = repo.find_tree(builder.write().unwrap()).unwrap();
            repo.commit(
                Some("refs/heads/theirs"),
                &sig,
                &sig,
                "theirs",
                &tree,
                &[&base_commit],
            )
            .unwrap();
        }
        fs::write(root.join("h0.txt"), "OUR VERSION\n").unwrap();
        {
            let mut index = repo.index().unwrap();
            index.add_path(std::path::Path::new("h0.txt")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            repo.commit(Some("HEAD"), &sig, &sig, "ours", &tree, &[&base_commit])
                .unwrap();
        }
        let our_tip = repo.head().unwrap().target().unwrap();
        let their_tip = repo
            .find_reference("refs/heads/theirs")
            .unwrap()
            .target()
            .unwrap();
        let their_commit = repo.find_commit(their_tip).unwrap();
        let annotated = repo.find_annotated_commit(their_commit.id()).unwrap();
        repo.merge(&[&annotated], None, None).unwrap();
        repo.reference("ORIG_HEAD", our_tip, true, "fixture")
            .unwrap();
        (our_tip, their_tip)
    };

    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone())
    }
    app.reconcile_workspace_trust();
    let deadline = Instant::now() + Duration::from_secs(10);
    app.start_git_status_refresh();
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now()
    }
    let target = root.join("h0.txt");
    let before = fs::read(&target).unwrap();
    let index_before = crate::git::git_index_fingerprint(&root).unwrap();

    // PREVIEW: a confirmation naming the file and the side, and nothing written yet.
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.replace",
            InvocationSource::Test,
        )
        .expect("command id")
        .with_argument("side", CommandValue::String("theirs".into()))
        .with_argument("files", CommandValue::String("h0.txt".into())),
    );
    app.project_input_prompt();
    let confirmation = app
        .ui
        .prompt
        .as_ref()
        .and_then(|prompt| prompt.confirmation.as_ref())
        .expect("the preview asks for confirmation");
    assert!(
        confirmation.message.contains("h0.txt") && confirmation.message.contains("theirs"),
        "the preview names the file and the side: {:?}",
        confirmation.message
    );
    assert_eq!(
        confirmation.selected,
        crate::ui::ConfirmationChoice::No,
        "the confirmation defaults to No"
    );
    assert_eq!(
        fs::read(&target).unwrap(),
        before,
        "nothing is written while the question is open"
    );
    assert!(app.git.has_pending_replacement());

    // CANCEL: answering No writes nothing at all.
    app.submit_prompt();
    assert!(
        !app.git.has_pending_replacement(),
        "the question is answered once"
    );
    assert_eq!(
        fs::read(&target).unwrap(),
        before,
        "a cancelled preview leaves the file byte-identical"
    );
    assert_eq!(
        crate::git::git_index_fingerprint(&root).unwrap(),
        index_before,
        "and the index untouched"
    );
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|notice| notice.contains("cancelled")),
        "and says so: {:?}",
        app.ui.notification
    );

    // Escape must close the preview, and neither kind of destructive confirmation
    // may inherit the other kind's target.
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.replace",
            InvocationSource::Test,
        )
        .unwrap()
        .with_argument("side", CommandValue::String("theirs".into()))
        .with_argument("files", CommandValue::String("h0.txt".into())),
    );
    let replacement = app.git.take_pending_replacement().unwrap();
    app.git.set_pending_replacement(replacement.clone());
    app.git
        .set_pending_destructive(crate::app::git::GitDestructiveAction::RemoveWorktree(
            "missing".into(),
        ));
    assert!(!app.git.has_pending_replacement());
    app.git.set_pending_replacement(replacement);
    assert!(app.git.take_pending_destructive().is_none());
    app.close_prompt();
    assert!(
        !app.git.has_pending_replacement(),
        "Escape cancels the replacement"
    );
    app.git
        .set_pending_destructive(crate::app::git::GitDestructiveAction::RemoveWorktree(
            "missing".into(),
        ));
    app.open_confirmation(
        crate::ui::PromptKind::GitDestructive,
        "Remove worktree".into(),
        "Remove missing?".into(),
        crate::ui::ConfirmationChoice::Yes,
    );
    app.submit_prompt();
    assert_eq!(
        fs::read(&target).unwrap(),
        before,
        "the later confirmation cannot replace the file"
    );

    // CONFIRM: the file is replaced with the chosen side's content.
    app.invoke(
        crate::editor::CommandInvocation::new(
            "workspace.git.conflict.replace",
            InvocationSource::Test,
        )
        .expect("command id")
        .with_argument("side", CommandValue::String("theirs".into()))
        .with_argument("files", CommandValue::String("h0.txt".into())),
    );
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert_eq!(
        fs::read(&target).unwrap(),
        b"THEIR VERSION\n",
        "the confirmed replacement wrote exactly the chosen side's content"
    );
    assert!(!app.git.has_pending_replacement());
    let _ = (our_tip, index_before);
}

#[test]
fn opening_changes_refreshes_cached_unsaved_document_information() {
    let (_app_dirs, mut app) = app();
    history_fixture(&app.state.workspace_root, &["base"]);
    let path = app.state.workspace_root.join("h0.txt");
    app.start_open(path);
    wait_vfs(&mut app);
    app.start_git_status_refresh();
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.git.status_pending() {
        app.process_git_status_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    let id = app.state.active_buffer_id().unwrap();
    app.state.buffers[id].text = TextBuffer::from_text("unsaved\n");
    app.state.buffers[id].text.bump_revision().unwrap();
    app.state.buffers[id].dirty = true;
    let status_revision = app.git.status().unwrap().revision;
    app.open_git_status_picker();
    assert!(
        !app.git.status_pending(),
        "editor-only changes need no disk refresh"
    );
    assert_eq!(app.git.status().unwrap().revision, status_revision);
    let projection = app.search.picker_projection().unwrap();
    assert!(projection.rows.iter().any(|row| row.label == "h0.txt"));
    let entry = app
        .git
        .status()
        .unwrap()
        .entries
        .iter()
        .find(|entry| entry.relative_path == std::path::Path::new("h0.txt"))
        .unwrap();
    assert!(entry.sources.unsaved_document_revision.is_some());
    app.close_picker();
    app.state.buffers[id].dirty = false;
    app.open_git_status_picker();
    assert!(
        app.git
            .status()
            .unwrap()
            .entries
            .iter()
            .all(|entry| entry.sources.unsaved_document_revision.is_none())
    );
    assert!(
        !app.search
            .picker_projection()
            .unwrap()
            .rows
            .iter()
            .any(|row| row.label == "h0.txt")
    );
}

#[test]
fn file_rows_dim_the_folder_and_count_what_the_query_leaves_of_all() {
    let (_app_dirs, mut app) = app();
    fs::create_dir(app.state.workspace_root.join("src")).unwrap();
    fs::write(app.state.workspace_root.join("src/lib.rs"), "").unwrap();
    fs::write(app.state.workspace_root.join("NOTICE"), "").unwrap();
    app.start_workspace_file_picker();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.search.files_pending() {
        app.process_workspace_file_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.picker_insert("lib");
    let picker = app.ui.picker.as_ref().unwrap();
    assert_eq!(picker.surface, crate::ui::PickerSurfaceKind::Files);
    assert_eq!((picker.rows.len(), picker.total), (1, 2));
    let cells = &picker.rows[0].cells;
    let icon = crate::files::file_icon("lib.rs").unwrap().glyph;
    let texts: Vec<_> = cells.iter().map(|cell| cell.text.as_str()).collect();
    let folder = format!("src{}", std::path::MAIN_SEPARATOR);
    assert_eq!(
        texts,
        [format!("{icon} ").as_str(), folder.as_str(), "lib.rs"]
    );
    assert_eq!(cells[1].style, crate::ui::Style::Muted);
    assert_eq!(cells[2].style, crate::ui::Style::Default);
}

#[test]
fn perpendicular_docked_editor_resizes_do_nothing() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let editor = app.window_session.graph().focused().unwrap();
    app.place_graph_window(
        &format!("window:{}", editor.0),
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 20,
        },
    )
    .unwrap();
    let graph = app.window_session.graph().clone();
    for key in ["ctrl-up", "ctrl-down"] {
        app.handle_configured_key(key.parse().unwrap());
        assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
        assert_eq!(app.window_session.graph(), &graph);
    }
}

#[test]
fn directional_editor_resize_skips_hidden_splits_and_reaches_the_visible_dock() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let first = app.window_session.graph().focused().unwrap();
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    )));
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    )));
    app.place_graph_window("focused", WindowPlacement::Hidden)
        .unwrap();
    let tab = app.state.active_tab().unwrap();
    app.handle_configured_key("ctrl-right".parse().unwrap());
    let crate::windows::TiledLayoutNode::Split { ratio, second, .. } =
        &app.state.tab_layout(tab).unwrap()
    else {
        panic!("outer split");
    };
    assert!(*ratio < 500);
    assert!(matches!(
        **second,
        crate::windows::TiledLayoutNode::Split { ratio: 500, .. }
    ));
    assert!(!app.ui.notification_error, "{:?}", app.ui.notification);

    app.place_graph_window(&format!("window:{}", first.0), WindowPlacement::Hidden)
        .unwrap();
    app.terminal_session.fixture(
        PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(20, 20, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.select(PtyId(1));
    app.set_terminal_focus(true);
    app.sync_terminal_panel();
    app.place_graph_window(
        "terminal",
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 20,
        },
    )
    .unwrap();
    app.process_effect(AppEffect::TerminalUnfocus);
    let root = app.state.tab_layout(tab).unwrap().clone();
    app.handle_configured_key("ctrl-right".parse().unwrap());
    assert_eq!(app.state.tab_layout(tab).unwrap(), root);
    let terminal = app.window_session.native_ids().terminal(1).unwrap().window;
    assert_eq!(
        app.window_session
            .graph()
            .window(terminal)
            .unwrap()
            .placement,
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 20 - crate::ui::render::TERMINAL_PANEL_RESIZE_STEP
        }
    );
    assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
}

#[test]
fn notifications_expire_but_remain_available_in_bounded_history() {
    let (_app_dirs, mut app) = app();
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(
        std::time::UNIX_EPOCH,
        Instant::now(),
    ));
    app.clock = clock.clone();
    app.notify_message("first".into(), false);
    app.update_notification_timer();
    app.notify_message("second".into(), true);
    app.update_notification_timer();
    assert_eq!(app.ui.notification.as_deref(), Some("second"));
    clock.advance(Duration::from_secs(5));
    app.update_notification_timer();
    assert!(app.ui.notification.is_none());
    app.invoke_mapped_command("application.notifications.open");
    let picker = app.ui.picker.as_ref().expect("notification history");
    assert_eq!(picker.title, "Notifications");
    assert_eq!(picker.rows.len(), 2);
    assert_eq!(picker.rows[0].label, "second");
    assert_eq!(picker.rows[0].detail, "error");
    assert_eq!(picker.rows[1].label, "first");

    app.close_picker();
    for index in 0..205 {
        app.notify_message(format!("notice {index}"), false);
    }
    app.invoke_mapped_command("application.notifications.open");
    let picker = app
        .ui
        .picker
        .as_ref()
        .expect("bounded notification history");
    assert_eq!(picker.rows.len(), 200);
    assert_eq!(picker.rows.first().unwrap().label, "notice 204");
    assert_eq!(picker.rows.last().unwrap().label, "notice 5");
}

#[test]
fn log_selection_survives_eviction_of_an_earlier_entry() {
    let (_app_dirs, mut app) = app();
    for line in ["first", "selected", "last"] {
        app.language.append_log(line.into(), 3);
    }
    app.show_lsp_logs();
    app.picker_move(-1);
    let selected = app.ui.picker.as_ref().unwrap().selected.unwrap();
    assert_eq!(
        app.ui.picker.as_ref().unwrap().rows[selected].label,
        "selected"
    );
    app.language.append_log("new".into(), 3);
    app.show_lsp_logs();
    let picker = app.ui.picker.as_ref().unwrap();
    assert_eq!(picker.rows[picker.selected.unwrap()].label, "selected");
}

#[test]
fn normalized_mouse_position_invokes_real_cursor_placement() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcdef"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let view = app.state.active_view().expect("view");
    let workspace = app.workspace_bounds().expect("workspace");
    let viewport = active_pane_viewport(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        workspace.width,
        app.workspace_render_height(),
    )
    .expect("editor viewport");
    let gutter = crate::ui::editor_state_gutter_width(
        &app.state,
        &app.state.buffers[app.state.views[view].buffer].text,
        &app.configuration.settings().editor,
        viewport.width,
    );
    app.input.observe_mouse(
        workspace
            .x
            .saturating_add(viewport.x)
            .saturating_add(gutter + 2),
        workspace.y.saturating_add(viewport.y),
    );
    app.apply_mouse_cursor(false);
    assert_eq!(
        app.state.views[view].cursor,
        crate::editor::text::ByteOffset(2)
    );
    app.input.observe_mouse(
        workspace
            .x
            .saturating_add(viewport.x)
            .saturating_add(gutter + 4),
        workspace.y.saturating_add(viewport.y),
    );
    app.apply_mouse_cursor(true);
    let selection = app.state.views[view].selection.expect("mouse selection");
    assert_eq!(selection.anchor, crate::editor::text::ByteOffset(2));
    assert_eq!(selection.head, crate::editor::text::ByteOffset(4));
}

pub(in crate::app) fn gate_document_worker(app: &mut App) -> std::sync::mpsc::Sender<()> {
    let (worker, gate) = crate::files::VfsWorker::start_gated_for_test(8);
    app.documents = crate::app::documents::Documents::new(
        worker,
        &StartupTarget::Dashboard {
            root: app.state.workspace_root.clone(),
        },
        app.state.window_version(),
        app.workspace.root_revision(),
        8,
    );
    gate
}

#[test]
fn a_pending_open_reserves_its_save_destination() {
    for alias in [false, true] {
        let (_app_dirs, mut app) = app();
        let root = app.state.workspace_root.clone();
        let destination = root.join("b.txt");
        fs::write(&destination, "original B").unwrap();
        fs::create_dir(root.join("sub")).unwrap();
        app.state
            .open_text(TextBuffer::from_text("A"), Some(root.join("a.txt")))
            .unwrap();
        let gate = gate_document_worker(&mut app);
        app.start_open(if alias {
            root.join("sub/../b.txt")
        } else {
            destination.clone()
        });
        app.start_save(false, Some(destination.clone()), true);
        gate.send(()).unwrap();
        wait_vfs(&mut app);
        assert_eq!(fs::read(&destination).unwrap(), b"original B");
        assert!(app.state.buffers.values().any(|b| {
            b.path.as_ref() == Some(&destination) && b.text.to_string() == "original B"
        }));
    }
}

#[test]
fn an_alternate_spelling_of_the_current_file_uses_its_saved_bytes() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    let path = root.join("file.txt");
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(&path, "old").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("old"), Some(path.clone()))
        .unwrap();
    app.invoke_insert_text("new ".into());
    app.start_save(false, Some(root.join("sub/../file.txt")), false);
    wait_vfs(&mut app);
    assert_eq!(fs::read(&path).unwrap(), b"new old");
    assert!(!app.state.buffers[buffer].dirty);
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    // The resolved identity must not bypass external-modification protection.
    fs::write(&path, "external").unwrap();
    app.start_save(false, Some(root.join("sub/../file.txt")), false);
    wait_vfs(&mut app);
    assert_eq!(fs::read(&path).unwrap(), b"external");
}

#[cfg(unix)]
#[test]
fn save_alias_identity_is_resolved_by_the_worker_before_writing() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    let original = root.join("original.txt");
    let other = root.join("other.txt");
    let alias = root.join("alias.txt");
    fs::write(&original, "old").unwrap();
    fs::write(&other, "unrelated").unwrap();
    std::os::unix::fs::symlink(&original, &alias).unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("old"), Some(original.clone()))
        .unwrap();
    app.invoke_insert_text("new ".into());
    let gate = gate_document_worker(&mut app);
    app.start_save(false, Some(alias.clone()), false);
    fs::remove_file(&alias).unwrap();
    std::os::unix::fs::symlink(&other, &alias).unwrap();
    gate.send(()).unwrap();
    wait_vfs(&mut app);
    assert_eq!(fs::read(&original).unwrap(), b"old");
    assert_eq!(fs::read(&other).unwrap(), b"unrelated");
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&original));
}

#[test]
fn saved_line_ending_policy_updates_the_live_buffer_without_losing_new_edits() {
    for edit_during_save in [false, true] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("file.txt");
        fs::write(&path, "a\nb\n").unwrap();
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("a\nb\n"), Some(path.clone()))
            .unwrap();
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.line_endings = crate::config::LineEndingPolicy::CrLf;
        app.configuration.install_fixture(fixture);
        let gate = gate_document_worker(&mut app);
        app.start_save(false, None, false);
        if edit_during_save {
            app.invoke_insert_text("new\n".into());
        }
        gate.send(()).unwrap();
        wait_vfs(&mut app);
        assert_eq!(fs::read(&path).unwrap(), b"a\r\nb\r\n");
        assert_eq!(app.state.buffers[buffer].dirty, edit_during_save);
        assert_eq!(
            app.state.buffers[buffer]
                .text
                .format()
                .preferred_line_ending,
            crate::editor::text::LineEnding::CrLf
        );
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.line_endings = crate::config::LineEndingPolicy::Preserve;
        app.configuration.install_fixture(fixture);
        app.start_save(false, None, false);
        wait_vfs(&mut app);
        let expected = if edit_during_save {
            b"new\r\na\r\nb\r\n".as_slice()
        } else {
            b"a\r\nb\r\n".as_slice()
        };
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert!(!app.state.buffers[buffer].dirty);
    }
}

#[test]
fn shutdown_saves_normalized_text_with_resumable_undo_history() {
    for (source, policy, saved) in [
        ("base", crate::config::FinalNewlinePolicy::Insert, "Xbase\n"),
        (
            "base\n\n",
            crate::config::FinalNewlinePolicy::Remove,
            "Xbase",
        ),
    ] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("normalized.txt");
        fs::write(&path, source).unwrap();
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text(source), Some(path.clone()))
            .unwrap();
        app.set_dashboard_visibility(false);
        let directory = app.persistence.undo_directory();
        app.state.undo_directory = Some(directory.clone());
        app.state.undo_maximum_bytes = 1 << 20;
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.final_newline = policy;
        app.configuration.install_fixture(fixture);
        app.invoke_insert_text("X".into());
        app.start_save(true, None, false);
        app.finish_shutdown_before(Instant::now() + Duration::from_secs(5))
            .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), saved);
        assert_eq!(app.state.buffers[buffer].text.to_string(), saved);
        assert!(!app.persistence.undo_pending());
        let mut reopened = EditorState::new(app.state.workspace_root.clone());
        reopened.undo_directory = Some(directory);
        reopened.undo_maximum_bytes = 1 << 20;
        let (id, _) = reopened
            .open_text(TextBuffer::from_text(saved), Some(path))
            .unwrap();
        for expected in [format!("X{source}"), source.into()] {
            reopened
                .apply_transaction(
                    &EditorTransaction::new(TransactionId(1))
                        .with_operation(EditorOperation::Undo { buffer: id }),
                )
                .unwrap();
            assert_eq!(reopened.buffers[id].text.to_string(), expected);
        }
    }
}

#[test]
fn undo_history_failures_are_notified_and_fail_shutdown() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("history.txt");
    fs::write(&path, "base").unwrap();
    app.state
        .open_text(TextBuffer::from_text("base"), Some(path))
        .unwrap();
    app.set_dashboard_visibility(false);
    let invalid = app.state.workspace_root.join("not-a-directory");
    fs::write(&invalid, "occupied").unwrap();
    app.state.undo_directory = Some(invalid);
    app.state.undo_maximum_bytes = 1 << 20;
    app.invoke_insert_text("X".into());
    app.start_save(false, None, false);
    wait_vfs(&mut app);
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.persistence.undo_pending() {
        app.process_persistence_events();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        app.ui
            .notification
            .as_ref()
            .is_some_and(|text| text.contains("could not save undo history"))
    );
    assert!(app.finish_shutdown_before(deadline).is_err());
}

#[test]
fn configured_file_format_policy_is_applied_at_save_boundary() {
    let (_app_dirs, mut app) = app();
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.settings.editor.line_endings = crate::config::LineEndingPolicy::Lf;
    fixture.settings.editor.final_newline = crate::config::FinalNewlinePolicy::Insert;
    app.configuration.install_fixture(fixture);
    let text = crate::editor::text::TextBuffer::from_bytes(b"a\r\nb").expect("CRLF text");
    let (buffer, _) = app.state.open_text(text, None).unwrap();
    app.set_dashboard_visibility(false);
    let path = app.state.workspace_root.join("formatted.txt");
    app.start_save(false, Some(path.clone()), false);
    assert_eq!(app.documents.pending_count(), 1);
    wait_vfs(&mut app);
    assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
    assert!(!app.state.buffers[buffer].dirty);
    assert_eq!(fs::read(path).unwrap(), b"a\nb\n");
}

#[test]
fn dirty_buffer_close_save_discard_cancel_and_stale_ack_are_source_bound() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("dirty-close.txt");
    std::fs::write(&path, "saved\n").unwrap();
    let (buffer, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("saved\n"),
            Some(path.clone()),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.buffers[buffer]
        .text
        .restore_text("discard me\n")
        .unwrap();
    app.state.set_buffer_dirty(buffer, true);
    app.reconcile_editor_windows();
    let arrangement = (
        app.state.active_tab(),
        app.state.active_view(),
        app.state.editor_tabs().count(),
        app.state.editor_windows().count(),
        app.state.views.len(),
    );

    app.close_active_buffer(false);
    let prompt = app.input.prompt().expect("dirty close prompt");
    let confirmation = prompt.confirmation.as_ref().unwrap();
    assert_eq!(prompt.kind, crate::ui::PromptKind::DirtyClose);
    assert_eq!(confirmation.positive_label, "Save");
    assert_eq!(confirmation.negative_label, "Discard");
    assert_eq!(confirmation.selected, crate::ui::ConfirmationChoice::No);
    app.close_prompt();
    assert!(app.state.buffers.contains_key(buffer));
    assert_eq!(
        (
            app.state.active_tab(),
            app.state.active_view(),
            app.state.editor_tabs().count(),
            app.state.editor_windows().count(),
            app.state.views.len(),
        ),
        arrangement
    );

    app.close_active_buffer(false);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(app.state.buffers.contains_key(buffer));
    assert!(app.documents.file_work_pending());
    app.state.buffers[buffer]
        .text
        .restore_text("newer edit\n")
        .unwrap();
    app.state.set_buffer_dirty(buffer, true);
    crate::app::tests::wait_vfs(&mut app);
    assert!(app.state.buffers.contains_key(buffer));
    assert!(app.state.buffers[buffer].dirty);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("close was cancelled"))
    );

    app.close_active_buffer(false);
    app.submit_prompt();
    assert!(!app.state.buffers.contains_key(buffer));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "discard me\n");
}

#[test]
fn dirty_close_save_waits_for_exact_vfs_ack_and_failure_preserves_arrangement() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("save-before-close.txt");
    std::fs::write(&path, "old\n").unwrap();
    let (buffer, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("old\n"),
            Some(path.clone()),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.buffers[buffer]
        .text
        .restore_text("new\n")
        .unwrap();
    app.state.set_buffer_dirty(buffer, true);
    app.reconcile_editor_windows();
    app.close_active_buffer(false);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    assert!(app.state.buffers.contains_key(buffer));
    crate::app::tests::wait_vfs(&mut app);
    assert!(!app.state.buffers.contains_key(buffer));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new\n");

    let directory = app.state.workspace_root.join("not-a-file");
    std::fs::create_dir(&directory).unwrap();
    let (failed, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("dirty\n"),
            Some(directory),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.set_buffer_dirty(failed, true);
    app.reconcile_editor_windows();
    let arrangement = (
        app.state.active_tab(),
        app.state.active_view(),
        app.state.editor_tabs().count(),
        app.state.editor_windows().count(),
        app.state.views.len(),
    );
    app.close_active_buffer(false);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    crate::app::tests::wait_vfs(&mut app);
    assert!(app.state.buffers.contains_key(failed));
    assert!(app.state.buffers[failed].dirty);
    assert_eq!(
        (
            app.state.active_tab(),
            app.state.active_view(),
            app.state.editor_tabs().count(),
            app.state.editor_windows().count(),
            app.state.views.len(),
        ),
        arrangement
    );
}

#[test]
fn recovery_debounce_coalesces_revisions_and_clean_state_deletes_last_journal() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let buffer = app.state.active_buffer_id().expect("buffer");
    app.invoke_insert_text("A".into());
    app.schedule_recovery_work();
    let first_revision = app.state.buffers[buffer].text.revision();
    app.invoke_insert_text("B".into());
    app.schedule_recovery_work();
    let latest_revision = app.state.buffers[buffer].text.revision();
    assert_ne!(first_revision, latest_revision);
    assert_eq!(app.recovery.scheduled().len(), 1);
    assert_eq!(app.recovery.scheduled()[&buffer].0, latest_revision);
    assert!(!app.recovery.writing(None));
    std::thread::sleep(Duration::from_millis(
        crate::app::documents::recovery::WRITE_DEBOUNCE_MS + 10,
    ));
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    let journal = app.recovery.path(buffer);
    let recovered = crate::files::discover_recovery(app.recovery.directory());
    assert_eq!(recovered.records.len(), 1);
    assert_eq!(recovered.records[0].1.revision, latest_revision.0);

    app.state.buffers[buffer].mark_saved(latest_revision);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(!journal.exists());
    assert!(!app.recovery.revisions().contains_key(&buffer));

    app.invoke_insert_text("X".into());
    let revision = app.state.buffers[buffer].text.revision();
    app.schedule_recovery_work();
    app.state.buffers[buffer].mark_saved(revision);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(!app.recovery.path(buffer).exists());
}

#[test]
fn recovery_retention_keeps_all_live_dirty_journals() {
    let (_app_dirs, mut app) = app();
    let mut journals = Vec::new();
    for index in 0..101 {
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text(&format!("draft {index}")), None)
            .unwrap();
        app.state.set_buffer_dirty(buffer, true);
        journals.push(app.recovery.path(buffer));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while app
        .recovery
        .unwritten(&app.state, &app.configuration.settings().recovery)
        > 0
    {
        assert!(Instant::now() < deadline);
        app.queue_dirty_recovery_before_exit();
        wait_vfs(&mut app);
    }
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(
        journals.iter().all(|path| path.exists()),
        "retention must not delete live drafts"
    );
    assert_eq!(
        app.recovery
            .unwritten(&app.state, &app.configuration.settings().recovery),
        0
    );
}

#[test]
fn recovery_save_and_close_removes_the_older_journal() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    let path = app.state.workspace_root.join("close-recovery.txt");
    fs::write(&path, "saved").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("saved"), Some(path.clone()))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.invoke_insert_text("A".into());
    app.queue_dirty_recovery_before_exit();
    wait_vfs(&mut app);
    let journal = app.recovery.path(buffer);
    assert!(journal.exists());
    app.invoke_insert_text("B".into());
    let expected = app.state.buffers[buffer].text.to_string();
    app.reconcile_editor_windows();
    app.close_active_buffer(false);
    app.input
        .select_confirmation(crate::ui::ConfirmationChoice::Yes);
    app.submit_prompt();
    wait_vfs(&mut app);
    assert!(!app.state.buffers.contains_key(buffer));
    assert_eq!(fs::read_to_string(path).unwrap(), expected);
    app.schedule_recovery_work();
    wait_vfs(&mut app);
    assert!(!journal.exists());
}

#[test]
fn recovery_rewrites_the_path_without_a_text_revision_change() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let buffer = app.state.active_buffer_id().unwrap();
    app.invoke_insert_text("A".into());
    app.queue_dirty_recovery_before_exit();
    wait_vfs(&mut app);
    let revision = app.state.buffers[buffer].text.revision();
    let renamed = app.state.workspace_root.join("renamed.txt");
    app.state.buffers[buffer].path = Some(renamed.clone());
    app.queue_dirty_recovery_before_exit();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.revision(), revision);
    let discovery = crate::files::discover_recovery(app.recovery.directory());
    assert_eq!(discovery.records.len(), 1);
    assert_eq!(discovery.records[0].1.path(), Some(renamed));
}

#[test]
fn shutdown_returns_the_reason_dirty_buffers_cannot_be_recovered() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    app.state
        .open_text(
            crate::editor::text::TextBuffer::from_text(&"x".repeat(12 * 1024 * 1024)),
            None,
        )
        .unwrap();
    let buffer = app.state.active_buffer_id().unwrap();
    app.state.buffers[buffer].dirty = true;
    let error = app
        .finish_shutdown_before(Instant::now())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("exceed the recovery journal size limit"),
        "{error}"
    );
    assert!(
        error.contains("unsaved contents cannot be recovered"),
        "{error}"
    );
}

#[test]
fn shutdown_retries_recovery_after_document_requests_drain() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    let root = app.state.workspace_root.clone();
    let (worker, gate) = crate::files::VfsWorker::start_gated_for_test(8);
    app.documents = crate::app::documents::Documents::new(
        worker,
        &StartupTarget::Dashboard { root: root.clone() },
        app.state.window_version(),
        app.workspace.root_revision(),
        8,
    );
    let file = root.join("source");
    std::fs::write(&file, "source").unwrap();
    for _ in 0..8 {
        app.documents.read(file.clone()).unwrap();
    }
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let buffer = app.state.active_buffer_id().unwrap();
    app.invoke_insert_text("A".into());
    app.queue_dirty_recovery_before_exit();
    assert!(!app.recovery.writing(None));
    assert!(app.recovery.scheduled().contains_key(&buffer));
    let directory = app.recovery.directory().to_path_buf();
    gate.send(()).unwrap();
    app.finish_shutdown_before(Instant::now() + Duration::from_secs(2))
        .unwrap();
    let records = crate::files::discover_recovery(&directory).records;
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].1.content,
        app.state.buffers[buffer].text.to_string()
    );
}

#[test]
fn recovery_dialog_discards_one_journal_and_recovers_another() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    let directory = app.recovery.directory().to_owned();
    std::fs::create_dir_all(&directory).unwrap();
    let kept = directory.join("buffer-kept.toml");
    let dropped = directory.join("buffer-dropped.toml");
    std::fs::write(&kept, "").unwrap();
    std::fs::write(&dropped, "").unwrap();
    let notes = app.state.workspace_root.join("notes.md");
    std::fs::write(&notes, "saved\n").unwrap();
    app.recovery.fixture_records(vec![
        (
            kept.clone(),
            crate::files::RecoveryRecord::new(Some(&notes), 3, "draft\nsecond\n".into()),
        ),
        (
            dropped.clone(),
            crate::files::RecoveryRecord {
                version: crate::files::RECOVERY_VERSION,
                original_path: None,
                revision: 1,
                content: "scratch".into(),
            },
        ),
    ]);

    app.invoke_mapped_command("recovery.inspect");
    let picker = app.ui.picker.as_ref().expect("recovery dialog");
    assert_eq!(picker.title, "Recover Unsaved Work");
    assert_eq!(picker.rows.len(), 2);
    assert_eq!(picker.rows[0].detail, "1 line");
    assert!(picker.rows[1].label.contains("notes.md"));

    app.invoke_mapped_command("picker.remove");
    wait_vfs(&mut app);
    assert!(!dropped.exists());
    let picker = app.ui.picker.as_ref().expect("the dialog stays open");
    assert_eq!(picker.rows.len(), 1);
    assert!(
        picker
            .preview
            .as_ref()
            .and_then(crate::ui::PickerPreview::as_text)
            .is_some_and(|text| text.lines() == ["draft", "second"])
    );

    app.accept_picker();
    wait_vfs(&mut app);
    assert!(kept.exists(), "the durable journal survives recovery");
    assert!(app.recovery.records().is_empty());
    // The work comes back as an unsaved change to its own file; undo goes back to the disk.
    let buffer = app.state.active_buffer_id().expect("recovered buffer");
    assert_eq!(
        app.state.buffers[buffer].path.as_deref(),
        Some(notes.as_path())
    );
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(
        app.state.buffers[buffer].text.to_bytes(),
        b"draft\nsecond\n"
    );
    app.invoke_mapped_command("history.undo");
    assert_eq!(app.state.buffers[buffer].text.to_bytes(), b"saved\n");
}

#[test]
fn disabled_recovery_policy_schedules_no_work() {
    let (_app_dirs, mut app) = app();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.recovery.enabled = false;
        app.configuration.install_fixture(fixture);
    }
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("base"), None)
        .unwrap();
    app.invoke_insert_text("X".into());
    app.schedule_recovery_work();
    assert!(app.recovery.scheduled().is_empty());
    assert_eq!(app.documents.legacy_count(), 0);
}

#[test]
fn bundled_upper_h_and_l_cycle_open_buffers_in_normal_mode() {
    let (_app_dirs, mut app) = app();
    let (first, _) = app
        .state
        .open_text(
            TextBuffer::from_text("first"),
            Some(PathBuf::from("/workspace/first.rs")),
        )
        .unwrap();
    let (second, _) = app
        .state
        .open_text(
            TextBuffer::from_text("second"),
            Some(PathBuf::from("/workspace/second.rs")),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    assert_eq!(app.state.active_buffer_id(), Some(second));

    app.handle_key_in_context("editor.normal", KeyEvent::character('H'));
    assert_eq!(app.state.active_buffer_id(), Some(first));
    app.handle_key_in_context("editor.normal", KeyEvent::character('L'));
    assert_eq!(app.state.active_buffer_id(), Some(second));
}

#[test]
fn buffer_line_mouse_selects_and_right_click_closes_with_dirty_safety() {
    let (_app_dirs, mut app) = app();
    let (first, _) = app
        .state
        .open_text(
            TextBuffer::from_text("first"),
            Some(PathBuf::from("/workspace/first.rs")),
        )
        .unwrap();
    let (second, _) = app
        .state
        .open_text(
            TextBuffer::from_text("second"),
            Some(PathBuf::from("/workspace/second.rs")),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let bounds = app.workspace_bounds().unwrap();
    let target = app.state.content_for_buffer(first).unwrap();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                app.configuration.settings(),
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == target)
        })
        .unwrap();
    let row = bounds.y + u16::from(app.state.editor_tabs().count() > 1);
    let left = KeyEvent::mouse(
        crate::editor::input::KeyCode::MouseLeftDown,
        crate::editor::input::Modifiers::default(),
    );
    assert!(app.handle_content_tab_mouse(&left, bounds.x + local, row));
    assert_eq!(app.state.active_buffer_id(), Some(first));
    app.state.set_buffer_dirty(first, true);
    app.reconcile_editor_windows();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                app.configuration.settings(),
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == target)
        })
        .unwrap();
    let right = KeyEvent::mouse(
        crate::editor::input::KeyCode::MouseRightDown,
        crate::editor::input::Modifiers::default(),
    );
    assert!(app.handle_content_tab_mouse(&right, bounds.x + local, row));
    assert!(app.state.buffers.contains_key(first));
    assert_eq!(
        app.input.prompt().map(|prompt| prompt.kind),
        Some(crate::ui::PromptKind::DirtyClose)
    );
    app.close_prompt();
    app.state.set_buffer_dirty(first, false);
    app.reconcile_editor_windows();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                app.configuration.settings(),
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == target)
        })
        .unwrap();
    assert!(app.handle_content_tab_mouse(&right, bounds.x + local, row));
    assert!(
        !app.state.buffers.contains_key(first),
        "{:?}",
        app.ui.notification
    );
    assert_eq!(app.state.active_buffer_id(), Some(second));
}

#[test]
fn content_line_pins_reorder_middle_mouse_and_survive_bridge_reset() {
    let (_app_dirs, mut app) = app();
    let (a, _) = app
        .state
        .open_text(
            TextBuffer::from_text("a"),
            Some(PathBuf::from("/workspace/a.rs")),
        )
        .unwrap();
    let (b, _) = app
        .state
        .open_text(
            TextBuffer::from_text("b"),
            Some(PathBuf::from("/workspace/b.rs")),
        )
        .unwrap();
    let (c, _) = app
        .state
        .open_text(
            TextBuffer::from_text("c"),
            Some(PathBuf::from("/workspace/c.rs")),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.state
        .open_text(TextBuffer::from_text("scratch"), None)
        .unwrap();
    app.reconcile_editor_windows();
    assert!(
        app.ui
            .content_tabs
            .iter()
            .any(|entry| entry.icon == SCRATCH_ICON && entry.kind == ContentTabKind::Text)
    );
    app.state.show_buffer(b).unwrap();
    app.reconcile_editor_windows();
    app.toggle_active_buffer_pin();
    assert_eq!(
        app.ui.content_tabs.first().map(|entry| entry.pinned),
        Some(true),
        "{:?} active={:?} pins={:?}",
        app.ui.content_tabs,
        app.state.active_buffer_id(),
        app.window_session.pins()
    );
    app.state.show_buffer(a).unwrap();
    app.reconcile_editor_windows();
    app.toggle_active_buffer_pin();
    app.state.show_buffer(b).unwrap();
    app.reconcile_editor_windows();
    app.move_active_buffer_line(-1);
    let pinned = app
        .ui
        .content_tabs
        .iter()
        .filter(|entry| entry.pinned)
        .map(|entry| entry.title.clone())
        .collect::<Vec<_>>();
    assert_eq!(pinned, vec!["b.rs", "a.rs"]);
    let before = app.window_session.content_order().to_vec();
    app.move_active_buffer_line(-1);
    assert_eq!(app.window_session.content_order(), before);
    let bounds = app.workspace_bounds().unwrap();
    let target = app.state.content_for_buffer(c).unwrap();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                app.configuration.settings(),
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == target)
        })
        .unwrap();
    let row = bounds.y + u16::from(app.state.editor_tabs().count() > 1);
    let middle = KeyEvent::mouse(
        crate::editor::input::KeyCode::MouseMiddleDown,
        crate::editor::input::Modifiers::default(),
    );
    assert!(app.handle_content_tab_mouse(&middle, bounds.x + local, row));
    assert!(app.window_session.pins().contains(&c));
    let pins = app.window_session.pins().clone();
    let order = app.window_session.content_order().to_vec();
    app.window_session
        .reset_runtime(&[], &mut app.state)
        .unwrap();
    app.reconcile_editor_windows();
    assert_eq!(app.window_session.pins(), &pins);
    assert_eq!(app.window_session.content_order(), order);
    let views = app
        .state
        .views
        .iter()
        .filter_map(|(view, state)| (state.buffer == c).then_some(view))
        .collect::<Vec<_>>();
    for view in views {
        let _ = app.state.close_view(view);
    }
    app.state.remove_buffer(c);
    app.reconcile_editor_windows();
    assert!(
        !app.window_session.pins().contains(&c) && !app.window_session.content_order().contains(&c)
    );
}

#[test]
fn content_line_diagnostics_custom_entries_and_idle_projection_are_pure() {
    let (_app_dirs, mut app) = app();
    let path = PathBuf::from("/workspace/diagnostic.rs");
    app.state
        .open_text(TextBuffer::from_text("bad"), Some(path.clone()))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let producer = crate::workspace::DiagnosticProducer::new(
        crate::workspace::DiagnosticOwner::new("fixture").unwrap(),
    );
    app.diagnostics.activate(producer.clone());
    let uri = crate::language::lsp::file_uri(&path);
    app.diagnostics
        .publish(
            crate::app::diagnostics::DiagnosticBatch {
                producer,
                uri: uri.clone(),
                document: crate::app::diagnostics::DiagnosticDocument::capture(
                    &app.state,
                    app.state.active_buffer_id().unwrap(),
                ),
                validity: crate::app::diagnostics::DiagnosticValidity::RequestedSnapshot,
                items: vec![crate::workspace::Diagnostic {
                    uri,
                    version: None,
                    range: ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap(),
                    severity: DiagnosticSeverity::Error,
                    source: "rustc".into(),
                    code: Some("E0001".into()),
                    message: "bad".into(),
                    related: Vec::new(),
                    tags: Vec::new(),
                    fixes: Vec::new(),
                    data: None,
                    timestamp: std::time::SystemTime::UNIX_EPOCH,
                }],
            },
            &app.state,
        )
        .unwrap();
    app.sync_ui_diagnostics();
    let buffer_count = app.state.buffers.len();
    let mut draft = app
        .window_session
        .begin_plugin(
            "demo",
            1,
            (
                &app.state.window_version(),
                &app.extension.contributions().version(),
            ),
        )
        .unwrap();
    let custom = draft
        .create_content_with_contract(
            crate::windows::ContentTypeId::new("plugin.demo-custom").unwrap(),
            crate::windows::ContentContract::adapter_defaults(
                crate::windows::ContentAdapterKind::DeclarativeUi,
            ),
            "Custom surface".into(),
            false,
            "plugin.demo",
            1,
        )
        .unwrap();
    app.window_session
        .commit_plugin(
            draft,
            &app.state.window_version(),
            &app.extension.contributions().version(),
            &mut app.state,
        )
        .unwrap();
    app.reconcile_editor_windows();
    let text = app
        .ui
        .content_tabs
        .iter()
        .find(|entry| entry.kind == ContentTabKind::Text)
        .unwrap();
    assert_eq!(text.icon, crate::files::file_icon("a.rs").unwrap().glyph);
    assert_eq!(
        text.diagnostic,
        Some(ContentTabDiagnostic {
            severity: ContentTabDiagnosticSeverity::Error,
            count: 1
        })
    );
    assert_eq!(app.state.buffers.len(), buffer_count);
    let custom_entry = app
        .ui
        .content_tabs
        .iter()
        .find(|entry| entry.identity.0 == custom)
        .unwrap();
    assert_eq!(
        (custom_entry.icon.as_str(), custom_entry.closeable),
        (CUSTOM_ICON, false)
    );
    app.window_session
        .observe_size((300, app.window_session.height()));
    app.reconcile_editor_windows();
    let bounds = app.workspace_bounds().unwrap();
    let local = (0..bounds.width)
        .find(|column| {
            crate::ui::content_tab_at_column(
                &app.ui.content_tabs,
                app.configuration.settings(),
                bounds.width,
                *column,
            )
            .is_some_and(|id| id.0 == custom)
        })
        .unwrap();
    let row = bounds.y + u16::from(app.state.editor_tabs().count() > 1);
    let right = KeyEvent::mouse(
        crate::editor::input::KeyCode::MouseRightDown,
        crate::editor::input::Modifiers::default(),
    );
    assert!(app.handle_content_tab_mouse(&right, bounds.x + local, row));
    assert!(
        app.ui.notification_error
            && app
                .window_session
                .graph()
                .content_snapshots()
                .iter()
                .any(|entry| entry.id == custom)
    );
    app.window_session
        .remove_plugin_load("demo", 2, &mut app.state)
        .unwrap();
    assert!(
        app.window_session
            .graph()
            .content_snapshots()
            .iter()
            .any(|entry| entry.id == custom)
    );
    app.window_session
        .remove_plugin_load("demo", 1, &mut app.state)
        .unwrap();
    app.reconcile_editor_windows();
    assert!(
        !app.ui
            .content_tabs
            .iter()
            .any(|entry| entry.identity.0 == custom)
    );
    let signature = app.window_session.content_stamp();
    let tabs = app.ui.content_tabs.clone();
    for _ in 0..256 {
        app.reconcile_editor_windows();
        let frame = render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            app.window_session.width(),
            app.window_session.height(),
        );
        assert_eq!(frame.grid.width(), app.window_session.width());
        assert_eq!(app.window_session.content_stamp(), signature);
        assert_eq!(app.ui.content_tabs, tabs);
    }
}

#[cfg(unix)]
#[test]
fn ctrl_arrows_in_a_lone_editor_resize_the_agent_docked_beside_it() {
    use crate::app::tests::driver::TestSession;
    let mut loaded = crate::config::load_bundled_config();
    let agents = &mut loaded.config.agents;
    agents.connection = crate::config::AgentConnection::Terminal;
    let mut profile = agents.profiles[&agents.default].clone();
    profile.cli = vec!["sh".into(), "-c".into(), "echo READY; cat".into()];
    agents.profiles.insert("echoer".into(), profile);
    agents.default = "echoer".into();
    let mut driver = TestSession::new(loaded);
    driver.new_buffer();
    driver.trust_owned_terminal_fixture();
    let deadline = Instant::now() + Duration::from_secs(6);
    driver
        .application_mut()
        .process_effect(AppEffect::Agent(AgentCommand::Ask));
    while !driver
        .terminal_lines()
        .iter()
        .any(|line| line.contains("READY"))
    {
        driver.pump_terminal();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let agent = driver
        .application_mut()
        .displayed_terminal()
        .expect("agent");
    driver.key("ctrl-h");
    let app = driver.application_mut();
    assert_eq!(app.window_session.focused_terminal(), None);
    let start = app.terminal_rect(agent).expect("agent").width;

    // The editor has no split, so ctrl-left grows the code into the agent's side.
    driver.key("ctrl-left");
    let app = driver.application_mut();
    let step = crate::ui::render::TERMINAL_PANEL_RESIZE_STEP;
    assert_eq!(
        app.terminal_rect(agent).map(|rect| rect.width),
        Some(start + step)
    );
    assert_eq!(app.window_session.focused_terminal(), None);
    driver.key("ctrl-right");
    let app = driver.application_mut();
    assert_eq!(app.terminal_rect(agent).map(|rect| rect.width), Some(start));
}

#[test]
fn a_late_result_updates_its_own_record_and_leaves_a_newer_draft_untouched() {
    use crate::app::debugger::DebugResponse;
    use crate::app::debugger::tests::attach_adapter;
    use crate::debugger::DapStackFrame;
    use crate::debugger::DapVariable;

    let (_app_dirs, mut app) = app();
    let owner = &mut app.debugger_session;
    owner.begin_stop();
    let stop = owner.stop_epoch();
    let adapter = attach_adapter(owner);
    let DebugResponse::Picker { rows, .. } =
        owner.response(DapResponse::Stack(vec![DapStackFrame {
            id: 42,
            name: "main".into(),
            path: None,
            line: 1,
            column: 1,
            source_reference: None,
        }]))
    else {
        panic!("expected a frames picker");
    };
    assert_eq!(rows.len(), 1);
    let _frame = owner.take_selection(&rows[0].key).unwrap();

    // Leave an older request pending so the reply must be matched by request ID, not FIFO.
    app.process_effect(AppEffect::DapEvaluate("older".into()));
    app.process_effect(AppEffect::DapEvaluate("first".into()));
    let older_request = adapter.control().expect("the older evaluation was sent");
    assert!(matches!(
        &older_request,
        DapControl::Evaluate { expression, frame_id: Some(42), context }
            if expression == "older" && context == "repl"
    ));
    let request = adapter.control().expect("the evaluation was sent");
    assert!(matches!(
        &request,
        DapControl::Evaluate { expression, frame_id: Some(42), context }
            if expression == "first" && context == "repl"
    ));

    app.open_prompt(crate::ui::PromptKind::Command, "dap.evaluate ".into());
    app.prompt_insert("second");
    let draft = app.input.prompt().unwrap().clone();

    adapter.event(
        1,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 1,
            request_id: 2,
            request: Some(request),
            response: DapResponse::Evaluate(DapVariable {
                name: "result".into(),
                value: "1".into(),
                type_name: None,
                variables_reference: 0,
            }),
        },
    );
    app.process_dap_events();

    let entries = app.debugger_session.console().cloned().collect::<Vec<_>>();
    let first = entries.iter().find(|entry| entry.text == "first").unwrap();
    let older = entries.iter().find(|entry| entry.text == "older").unwrap();
    let answer = entries
        .iter()
        .find(|entry| entry.text == "result = 1")
        .unwrap();
    assert_eq!(first.actor.as_deref(), Some("same-name"));
    assert_eq!(first.stop, Some(stop));
    assert_eq!(first.frame, Some(42));
    assert!(first.submission.is_some());
    assert_ne!(first.submission, older.submission);
    assert_eq!(first.result.as_deref(), Some("result = 1"));
    assert_eq!(answer.submission, first.submission);
    assert_eq!(answer.actor, first.actor);
    assert_eq!(answer.stop, first.stop);
    assert_eq!(answer.frame, first.frame);
    assert_eq!(
        older.result, None,
        "the reply must not update the oldest request"
    );
    assert_eq!(entries.len(), 3, "the draft has not been submitted");

    let prompt = app.input.prompt().expect("the draft stays open");
    assert_eq!(prompt.kind, draft.kind);
    assert_eq!(prompt.input, "dap.evaluate second");
    assert_eq!(prompt.input, draft.input);
    assert_eq!(prompt.cursor, draft.cursor);

    // The older request remains independently answerable after the out-of-order reply.
    adapter.event(
        1,
        DapEvent::Response {
            adapter: "same-name".into(),
            stop_id: 1,
            request_id: 1,
            request: Some(older_request),
            response: DapResponse::Evaluate(DapVariable {
                name: "result".into(),
                value: "0".into(),
                type_name: None,
                variables_reference: 0,
            }),
        },
    );
    app.process_dap_events();
    let older = app
        .debugger_session
        .console()
        .find(|entry| entry.text == "older")
        .unwrap();
    assert_eq!(older.result.as_deref(), Some("result = 0"));
    let prompt = app
        .input
        .prompt()
        .expect("the draft stays open after both replies");
    assert_eq!(prompt.input, draft.input);
    assert_eq!(prompt.cursor, draft.cursor);
}

#[test]
fn live_marks_follow_command_edits_before_saving_and_jumping() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("marked.txt");
    fs::write(&path, "abc").unwrap();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("abc"), Some(path))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.state.views[view].cursor = ByteOffset(2);
    app.process_effect(AppEffect::MarkSet("a".into()));
    app.state.views[view].cursor = ByteOffset(0);
    app.handle_configured_key(KeyEvent::character('i'));
    // More edits than TextBuffer retains, without a save or lookup between them.
    for _ in 0..100 {
        app.handle_configured_key(KeyEvent::character('x'));
    }
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.save_persistence();
    assert_eq!(app.persistence.marks()[0].location.byte, 102);
    app.state.views[view].cursor = ByteOffset(0);
    app.handle_configured_key(KeyEvent::character('i'));
    for _ in 0..80 {
        app.handle_configured_key(KeyEvent::character('y'));
    }
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.process_effect(AppEffect::MarkJump("a".into()));
    wait_vfs(&mut app);
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()].cursor,
        ByteOffset(182)
    );
    assert_eq!(app.persistence.marks()[0].location.anchor, 182);
}

#[test]
fn an_agent_edit_of_a_file_with_unsaved_changes_warns_in_its_approval() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("draft.txt");
    let (buffer, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("draft"),
            Some(path.clone()),
        )
        .unwrap();
    let root = app.state.workspace_root.clone();
    let buffers = vec![(buffer, path.clone())];
    let changes = |request: &serde_json::Value| {
        crate::agent::permission_changes(request)
            .iter()
            .map(|path| root.join(path))
            .collect::<Vec<_>>()
    };
    let request =
        serde_json::json!({"toolCall": {"kind": "edit", "rawInput": {"file_path": path}}});
    assert_eq!(
        app.unsaved_agent_changes(&changes(&request), &buffers),
        "",
        "a clean file needs no warning"
    );
    app.state.buffers[buffer].dirty = true;
    assert_eq!(
        app.unsaved_agent_changes(&changes(&request), &buffers),
        "! draft.txt has unsaved changes; this changes the saved copy\n"
    );
    let relative =
        serde_json::json!({"toolCall": {"kind": "edit", "locations": [{"path": "draft.txt"}]}});
    assert!(
        app.unsaved_agent_changes(&changes(&relative), &buffers)
            .contains("draft.txt")
    );
    let read = serde_json::json!({"toolCall": {"kind": "read", "rawInput": {"file_path": path}}});
    assert_eq!(
        app.unsaved_agent_changes(&changes(&read), &buffers),
        "",
        "a read changes nothing"
    );
}

#[test]
fn a_saved_location_reads_as_file_and_line() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("marked-location.txt");
    std::fs::write(&path, "one\nline two\nline three\n").unwrap();
    let location = |byte| crate::persistence::PersistedLocation {
        path: path.clone(),
        byte,
        anchor: byte,
        scroll_line: 0,
        scroll_column: 0,
    };
    assert!(
        app.persisted_location_label(&location(13))
            .ends_with("marked-location.txt:3")
    );
    assert!(
        app.persisted_location_label(&location(0))
            .ends_with("marked-location.txt:1")
    );
    std::fs::write(&path, b"\xef\xbb\xbfa\r\nb\r\nc\r\n").unwrap();
    assert!(
        app.persisted_location_label(&location(4))
            .ends_with("marked-location.txt:3")
    );
    std::fs::remove_file(&path).unwrap();
    assert!(
        app.persisted_location_label(&location(13))
            .ends_with("marked-location.txt")
    );
    let _ = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("one\nline two\nline three\n"),
            Some(path.clone()),
        )
        .unwrap();
    assert!(
        app.persisted_location_label(&location(5))
            .ends_with("marked-location.txt:2")
    );
}

#[test]
fn buffer_cycling_follows_the_buffer_line_order() {
    let (_app_dirs, mut app) = app();
    let mut buffers = Vec::new();
    for name in ["a.txt", "b.txt", "c.txt"] {
        let path = app.state.workspace_root.join(name);
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text(name), Some(path))
            .unwrap();
        buffers.push(buffer);
    }
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.project_window_content_tabs();
    app.process_effect(AppEffect::MoveBufferLine(-1));
    app.process_effect(AppEffect::MoveBufferLine(-1));
    app.project_window_content_tabs();
    assert_eq!(
        app.window_session.content_tab_buffers(&app.state),
        [buffers[2], buffers[0], buffers[1]]
    );
    // c is first on the line now, so next goes to a, then b, then wraps to c.
    for (offset, expected) in [(1, 0), (1, 1), (1, 2), (-1, 1)] {
        app.process_effect(AppEffect::CycleBuffer(offset));
        assert_eq!(app.state.active_buffer_id(), Some(buffers[expected]));
    }
}

#[test]
fn nested_border_drag_uses_the_border_axis_on_both_sides() {
    fn drag(right: bool) -> u16 {
        let (_app_dirs, mut app) = app();
        app.state
            .open_text(TextBuffer::from_text("top"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.state
            .split_active(crate::windows::TileAxis::Vertical, 500)
            .unwrap();
        app.state
            .split_active(crate::windows::TileAxis::Horizontal, 500)
            .unwrap();
        app.handle_event(UiEvent::Resize(120, 40));
        app.reconcile_editor_windows();
        let projection = app.compositor_projection().unwrap();
        let border = projection
            .borders
            .iter()
            .find(|border| border.axis == crate::windows::TileAxis::Vertical && border.length > 100)
            .unwrap();
        let (x, y) = (
            border.x + border.length * if right { 3 } else { 1 } / 4,
            border.y,
        );
        app.handle_event(UiEvent::Mouse {
            key: "mouse-left-down".parse().unwrap(),
            column: x,
            row: y,
        });
        app.handle_event(UiEvent::Mouse {
            key: "mouse-left-drag".parse().unwrap(),
            column: x,
            row: y + 3,
        });
        match &app
            .state
            .tab_layout(app.state.active_tab().unwrap())
            .unwrap()
        {
            crate::windows::TiledLayoutNode::Split { ratio, .. } => {
                assert!(
                    *ratio > 500,
                    "dragging down must enlarge the top pane (right={right}): {ratio}"
                );
                *ratio
            }
            _ => panic!("split"),
        }
    }
    assert_eq!(drag(false), drag(true));
}

#[test]
fn clipped_dirty_close_cannot_save_or_discard() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("unsaved"), None)
        .unwrap();
    app.state.set_buffer_dirty(buffer, true);
    app.set_dashboard_visibility(false);
    app.window_session.observe_size((80, 24));
    app.reconcile_editor_windows();
    app.close_active_buffer(false);
    app.window_session.observe_size((10, 3));
    for choice in [
        crate::ui::ConfirmationChoice::No,
        crate::ui::ConfirmationChoice::Yes,
    ] {
        app.input.select_confirmation(choice);
        app.submit_prompt();
        assert!(app.state.buffers.contains_key(buffer));
        assert!(app.input.prompt().is_some());
    }
    app.close_prompt();
    assert!(app.input.prompt().is_none());
}

#[test]
fn recovered_missing_file_can_be_saved_without_force() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    let path = app.state.workspace_root.join("deleted.txt");
    let journal = app.recovery.directory().join("deleted.toml");
    fs::create_dir_all(journal.parent().unwrap()).unwrap();
    fs::write(&journal, "durable copy").unwrap();
    app.recovery.fixture_records(vec![(
        journal.clone(),
        crate::files::RecoveryRecord::new(Some(&path), 1, "recovered\n".into()),
    )]);
    app.open_recovery_picker();
    app.accept_picker();
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "recovered\n");
    assert!(app.state.buffers[buffer].dirty);
    assert!(app.state.buffers[buffer].new_file);
    assert!(journal.exists());
    app.invoke_mapped_command("file.save");
    wait_vfs(&mut app);
    assert_eq!(fs::read(&path).unwrap(), b"recovered\n");
    assert!(!app.state.buffers[buffer].dirty);
    assert!(!app.state.buffers[buffer].new_file);
}

#[test]
fn recovered_scratch_keeps_its_unsaved_baseline_and_durable_journal() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    let journal = app.recovery.directory().join("old-scratch.toml");
    fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let record = crate::files::RecoveryRecord::new(None, 1, "recovered".into());
    crate::files::write_recovery(&journal, &record).unwrap();
    let discovery = crate::files::discover_recovery(app.recovery.directory());
    assert!(discovery.diagnostics.is_empty());
    assert_eq!(discovery.records, vec![(journal.clone(), record.clone())]);
    app.recovery.fixture_records(discovery.records);
    app.open_recovery_picker();
    app.accept_picker();
    let buffer = app.state.active_buffer_id().unwrap();
    wait_vfs(&mut app);
    assert_eq!(
        crate::files::discover_recovery(app.recovery.directory()).records,
        vec![(journal.clone(), record)],
    );
    assert_eq!(app.recovery.path(buffer), journal);
    app.replace_with_recovered(buffer, "changed".into())
        .unwrap();
    app.invoke_mapped_command("history.undo");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "recovered");
    assert!(app.state.buffers[buffer].dirty);
    app.schedule_recovery_work();
    // Flush the debounced write as well as any queued deletion before checking
    // what a fresh editor would discover after a restart.
    app.queue_dirty_recovery_before_exit();
    wait_vfs(&mut app);
    let discovery = crate::files::discover_recovery(app.recovery.directory());
    assert!(discovery.diagnostics.is_empty());
    assert_eq!(
        discovery.records,
        vec![(
            journal,
            crate::files::RecoveryRecord::new(
                None,
                app.state.buffers[buffer].text.revision().0,
                "recovered".into(),
            ),
        )]
    );
    app.invoke_mapped_command("history.undo");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "");
}

use crate::app::documents::*;
pub(in crate::app) fn fixture() -> (EditorState, Documents) {
    let state = EditorState::new("/workspace".into());
    let startup = StartupTarget::Dashboard {
        root: "/workspace".into(),
    };
    let owner = Documents::new(VfsWorker::start(2), &startup, state.window_version(), 7, 2);
    (state, owner)
}
#[test]
fn startup_reply_is_exact_once_and_rejects_root_or_editor_replacement_before_decode() {
    let (state, mut owner) = fixture();
    // Owner-local fake delivery avoids filesystem timing and checks request submission
    // precedes UTF-8 decoding: invalid bytes must be discarded, not decoded.
    let id = RequestId(91);
    owner.pending_startup.insert(
        id,
        StartupIntent {
            primary: true,
            delivered: false,
        },
    );
    let stale = owner.route(
        VfsResult::Read {
            id,
            path: "old".into(),
            result: Ok(vec![255]),
        },
        &state.window_version(),
        8,
    );
    assert!(matches!(stale, DocumentEvent::Discarded));
    assert!(!owner.has_startup_work());
    owner.pending_startup.insert(
        id,
        StartupIntent {
            primary: true,
            delivered: false,
        },
    );
    let foreign = EditorState::new("/workspace".into());
    assert!(matches!(
        owner.route(
            VfsResult::Read {
                id,
                path: "old".into(),
                result: Ok(vec![255])
            },
            &foreign.window_version(),
            7
        ),
        DocumentEvent::Discarded
    ));
    owner.pending_startup.insert(
        id,
        StartupIntent {
            primary: true,
            delivered: false,
        },
    );
    owner.cancel_startup();
    let materializations = TextBuffer::test_materializations();
    assert!(matches!(
        owner.route(
            VfsResult::Read {
                id,
                path: "old".into(),
                result: Ok(vec![255])
            },
            &state.window_version(),
            7
        ),
        DocumentEvent::Discarded
    ));
    assert_eq!(TextBuffer::test_materializations(), materializations);
    let mut exhausted = state.clone();
    exhausted.exhaust_window_source_for_test();
    owner.pending_startup.insert(
        id,
        StartupIntent {
            primary: true,
            delivered: false,
        },
    );
    let materializations = TextBuffer::test_materializations();
    assert!(matches!(
        owner.route(
            VfsResult::Read {
                id,
                path: "exhausted".into(),
                result: Ok(vec![255])
            },
            &exhausted.window_version(),
            7
        ),
        DocumentEvent::Discarded
    ));
    assert_eq!(TextBuffer::test_materializations(), materializations);
    owner.pending_startup.insert(
        id,
        StartupIntent {
            primary: true,
            delivered: false,
        },
    );
    let event = owner.route(
        VfsResult::Read {
            id,
            path: "first".into(),
            result: Ok(b"hello".to_vec()),
        },
        &state.window_version(),
        7,
    );
    let DocumentEvent::Startup { read, text, .. } = event else {
        panic!("startup event");
    };
    assert_eq!(text.unwrap().to_string(), "hello");
    assert!(matches!(
        owner.route(
            VfsResult::Read {
                id,
                path: "duplicate".into(),
                result: Ok(vec![255])
            },
            &state.window_version(),
            7
        ),
        DocumentEvent::Discarded
    ));
    assert!(owner.finish_startup(read, None, &state.window_version(), 7));
    assert!(!owner.has_startup_work());
    assert!(matches!(
        owner.route(
            VfsResult::Read {
                id,
                path: "late".into(),
                result: Ok(vec![255])
            },
            &state.window_version(),
            7
        ),
        DocumentEvent::Discarded
    ));
}
#[test]
fn total_outstanding_requests_are_bounded_and_policy_does_not_follow_reused_buffer_ids() {
    let (mut state, mut owner) = fixture();
    owner.pending_legacy.extend([RequestId(1), RequestId(2)]);
    assert!(matches!(
        owner.read("not-submitted".into()),
        Err(VfsError::QueueFull)
    ));
    owner.route(
        VfsResult::Read {
            id: RequestId(1),
            path: "one".into(),
            result: Err(VfsError::QueueFull),
        },
        &state.window_version(),
        7,
    );
    assert!(owner.check_capacity().is_ok());
    let (id, _) = state
        .open_text(TextBuffer::from_text("tiny"), None)
        .unwrap();
    let policy = crate::config::LargeFileConfig::default();
    owner.set_large_file_mode(id, None, LargeFileOverride::On);
    assert_eq!(
        owner.reconcile_policy(
            &state.window_version(),
            state
                .buffers
                .iter()
                .map(|(id, b)| (id, b.path.as_deref(), &b.text)),
            &policy
        ),
        vec![id]
    );
    let mut foreign = EditorState::new("/workspace".into());
    let (reused, _) = foreign
        .open_text(TextBuffer::from_text("tiny"), None)
        .unwrap();
    assert_eq!(id, reused);
    assert!(
        owner
            .reconcile_policy(
                &foreign.window_version(),
                foreign
                    .buffers
                    .iter()
                    .map(|(id, b)| (id, b.path.as_deref(), &b.text)),
                &policy
            )
            .is_empty()
    );
}
