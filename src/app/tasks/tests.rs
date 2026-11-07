//! Tasks behavior and application integration tests.

use crate::app::App;
use crate::app::AppEffect;
use crate::app::PickerKind;
use crate::app::startup::StartupTarget;
use crate::app::tests::*;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::tempdir;

#[cfg(unix)]
#[test]
fn successful_task_dependencies_publish_their_matched_diagnostics() {
    let root = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    fs::write(root.path().join("a.txt"), "source\n").unwrap();
    let mut loaded = crate::config::load_bundled_config();
    let dependency = crate::config::TaskConfig {
        kind: crate::config::TaskKind::Task,
        executable: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            "printf 'a.txt:1:warning:dependency warning\\n'".into(),
        ],
        cwd: None,
        environment: BTreeMap::new(),
        dependencies: vec![],
        timeout_ms: Some(2000),
        stdin: false,
        matcher: Some(crate::config::ProblemMatcherConfig {
            pattern: r"(?m)^(?P<path>[^:]+):(?P<line>\d+):(?P<severity>[^:]+):(?P<message>[^\n]+)$"
                .into(),
            path_group: "path".into(),
            line_group: "line".into(),
            column_group: None,
            column_encoding: None,
            severity_group: Some("severity".into()),
            message_group: "message".into(),
            name_group: None,
            status_group: None,
            duration_group: None,
        }),
    };
    let mut parent = dependency.clone();
    parent.arguments = vec![
        "-c".into(),
        "printf 'a.txt:1:warning:parent warning\\n'; printf done > parent-finished".into(),
    ];
    parent.dependencies = vec!["dependency".into()];
    loaded.config.tasks.insert("dependency".into(), dependency);
    loaded.config.tasks.insert("parent".into(), parent);
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
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity);
    }
    app.reconcile_workspace_trust();
    app.process_effect(AppEffect::TaskRun("parent".into()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.path().join("parent-finished").exists() || app.diagnostics.choices().count() < 2 {
        app.process_execution_preflight();
        app.process_job_events();
        app.process_vfs_results();
        assert!(Instant::now() < deadline, "{:?}", app.ui.notification);
        thread::sleep(Duration::from_millis(2));
    }
    app.open_builtin_picker(PickerKind::Diagnostics);
    let rows = &app.ui.picker.as_ref().unwrap().rows;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|row| row.label.contains("dependency warning")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.label.contains("parent warning")),
        "{rows:?}"
    );
}

#[cfg(unix)]
#[test]
fn task_formatter_uses_authorized_worker_and_refuses_reloaded_snapshot() {
    let root = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.tasks.insert(
        "owned-dependency".into(),
        crate::config::TaskConfig {
            kind: crate::config::TaskKind::Task,
            executable: PathBuf::from("/bin/sh"),
            arguments: vec![
                "-c".into(),
                "printf dependency-output; printf ok >> dependency-runs".into(),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            dependencies: vec![],
            timeout_ms: Some(2000),
            stdin: false,
            matcher: None,
        },
    );
    loaded.config.tasks.insert(
        "owned-format".into(),
        crate::config::TaskConfig {
            kind: crate::config::TaskKind::Formatter,
            executable: PathBuf::from("/bin/sh"),
            arguments: vec![
                "-c".into(),
                "IFS= read -r input || :; printf ready > ready; while [ ! -f go ]; do :; done; if [ -f invalid ]; then printf '\\377'; else printf XYZ; fi".into(),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            dependencies: vec!["owned-dependency".into()],
            timeout_ms: Some(2000),
            stdin: true,
            matcher: None,
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
    let path = root.path().join("source.txt");
    fs::write(&path, "abc").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("abc"), Some(path.clone()))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    for stale in [true, false] {
        if stale {
            fs::write(root.path().join("invalid"), "").unwrap();
        }
        app.process_effect(AppEffect::FormatRun("owned-format".into()));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !root.path().join("ready").exists() && Instant::now() < deadline {
            app.process_execution_preflight();
            app.process_job_events();
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            root.path().join("ready").exists(),
            "{:?}",
            app.ui.notification
        );
        if stale {
            app.state.buffers[buffer].text = TextBuffer::from_text("abc");
        }
        fs::write(root.path().join("go"), "").unwrap();
        while app.tasks.running_count() != 0 && Instant::now() < deadline {
            app.process_job_events();
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(app.tasks.running_count(), 0);
        assert_eq!(
            app.state.buffers[buffer].text.to_string(),
            if stale { "abc" } else { "XYZ" },
            "notification: {:?}; history: {:?}",
            app.ui.notification,
            app.notices.history().collect::<Vec<_>>()
        );
        if stale {
            assert!(
                app.ui
                    .notification
                    .as_ref()
                    .is_some_and(|message| message.contains("formatter document instance")),
                "{:?}",
                app.ui.notification
            );
            fs::remove_file(root.path().join("invalid")).unwrap();
        }
        fs::remove_file(root.path().join("ready")).unwrap();
        fs::remove_file(root.path().join("go")).unwrap();
    }
    assert_eq!(fs::read_to_string(path).unwrap(), "abc");
    assert_eq!(
        fs::read_to_string(root.path().join("dependency-runs")).unwrap(),
        "okok"
    );

    // Restart formats the original document's current text, not the newly focused buffer.
    app.state.buffers[buffer]
        .text
        .restore_text("again")
        .unwrap();
    let (other, view) = app
        .state
        .open_text(TextBuffer::from_text("leave alone"), None)
        .unwrap();
    app.reconcile_editor_windows();
    fs::write(root.path().join("go"), "").unwrap();
    app.process_effect(AppEffect::TaskRestart("owned-format".into()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.tasks.is_running("owned-format") && Instant::now() < deadline {
        app.process_execution_preflight();
        app.process_job_events();
        thread::sleep(Duration::from_millis(2));
    }
    assert!(!app.tasks.is_running("owned-format"));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "XYZ");
    assert_eq!(app.state.buffers[other].text.to_string(), "leave alone");
    assert_eq!(app.state.active_view(), Some(view));
    assert_eq!(
        fs::read_to_string(root.path().join("dependency-runs")).unwrap(),
        "okokok"
    );
}

#[cfg(unix)]
#[test]
fn a_task_inherits_the_shell_environment_and_its_own_settings_win() {
    let root = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.tasks.insert(
        "env".into(),
        crate::config::TaskConfig {
            kind: crate::config::TaskKind::Task,
            executable: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "printf '%s|%s' \"$PATH\" \"$HOME\" > env.tmp; mv env.tmp env.txt".into(),
            ],
            cwd: None,
            environment: BTreeMap::from([("HOME".to_owned(), "/task-home".to_owned())]),
            dependencies: vec![],
            timeout_ms: Some(2000),
            stdin: false,
            matcher: None,
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
    app.process_effect(AppEffect::TaskRun("env".into()));
    let output = root.path().join("env.txt");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !output.exists() && Instant::now() < deadline {
        app.process_execution_preflight();
        app.process_job_events();
        thread::sleep(Duration::from_millis(2));
    }
    let path = std::env::var("PATH").unwrap();
    assert_eq!(
        fs::read_to_string(output).unwrap(),
        format!("{path}|/task-home")
    );
}

#[cfg(unix)]
#[test]
fn linter_job_and_vfs_publish_only_original_source_then_recover() {
    let root = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    fs::write(root.path().join("open.txt"), "abc\n").unwrap();
    fs::write(root.path().join("disk.txt"), "abc\n").unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.tasks.insert(
        "owned-lint".into(),
        crate::config::TaskConfig {
            kind: crate::config::TaskKind::Linter,
            executable: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "printf 'open.txt:1:1:open message\ndisk.txt:1:1:disk message\n'".into(),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            dependencies: vec![],
            timeout_ms: Some(2000),
            stdin: false,
            matcher: Some(crate::config::ProblemMatcherConfig {
                pattern: r"(?m)^(?P<path>[^:]+):(?P<line>\d+):(?P<column>\d+):(?P<message>[^\n]+)$"
                    .into(),
                path_group: "path".into(),
                line_group: "line".into(),
                column_group: Some("column".into()),
                column_encoding: None,
                severity_group: None,
                message_group: "message".into(),
                name_group: None,
                status_group: None,
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
    let (buffer, _) = app
        .state
        .open_text(
            TextBuffer::from_text("abc\n"),
            Some(root.path().join("open.txt")),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    prepare_trust_identity(&mut app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity.clone());
    }
    app.reconcile_workspace_trust();
    for stale in [true, false] {
        app.process_effect(AppEffect::LintRun("owned-lint".into()));
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.tasks.linter_count() == 0 && Instant::now() < deadline {
            app.process_execution_preflight();
            app.process_job_events();
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            app.tasks.linter_count(),
            1,
            "{:?}",
            app.notices.history().collect::<Vec<_>>()
        );
        if stale {
            app.state.buffers[buffer].text = TextBuffer::from_text("abc\n");
        }
        let before = TextBuffer::test_materializations();
        while app.tasks.linter_count() != 0 && Instant::now() < deadline {
            app.process_vfs_results();
            app.process_job_events();
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(app.tasks.linter_count(), 0);
        assert_eq!(
            app.diagnostics.choices().count(),
            if stale { 0 } else { 2 },
            "{:?}",
            app.notices.history().collect::<Vec<_>>()
        );
        if stale {
            assert_eq!(TextBuffer::test_materializations(), before);
        }
    }
    assert!(app.diagnostics.choices().all(
        |(_, _, validity)| validity == crate::app::diagnostics::DiagnosticValidity::DiskReport
    ));
    assert_eq!(
        fs::read_to_string(root.path().join("open.txt")).unwrap(),
        "abc\n"
    );
}

#[test]
fn a_task_with_a_matcher_puts_what_it_matched_in_problems() {
    check_task_diagnostics(false);
}

pub(in crate::app) fn check_task_diagnostics(with_dependency: bool) {
    let root = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    fs::write(root.path().join("notes.txt"), "one\ntwo\n").unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.tasks.insert(
        "build".into(),
        crate::config::TaskConfig {
            kind: crate::config::TaskKind::Task,
            executable: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "printf 'notes.txt:2:1: error: broken\n'; exit 1".into(),
            ],
            cwd: None,
            environment: BTreeMap::new(),
            dependencies: vec![],
            timeout_ms: Some(2000),
            stdin: false,
            matcher: Some(crate::config::ProblemMatcherConfig {
                pattern: r"(?m)^(?P<path>[^:]+):(?P<line>\d+):(?P<column>\d+): (?P<severity>\w+): (?P<message>[^\n]+)$"
                    .into(),
                path_group: "path".into(),
                line_group: "line".into(),
                column_group: Some("column".into()),
                column_encoding: None,
                severity_group: Some("severity".into()),
                message_group: "message".into(),
                name_group: None,
                status_group: None,
                duration_group: None,
            }),
        },
    );
    if with_dependency {
        let mut dependency = loaded.config.tasks["build"].clone();
        dependency.arguments = vec!["-c".into(), "printf 'notes.txt:1:1: warning: stdout'; exec 1>&-; printf 'notes.txt:2:1: warning: stderr' >&2".into()];
        loaded.config.tasks.insert("check".into(), dependency);
        let build = loaded.config.tasks.get_mut("build").unwrap();
        build.dependencies = vec!["check".into()];
        build.arguments = vec![
            "-c".into(),
            "printf 'notes.txt:2:1: warning: main task\\n'".into(),
        ];
    }
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
    app.process_effect(AppEffect::TaskRun("build".into()));
    let deadline = Instant::now() + Duration::from_secs(3);
    let expected = if with_dependency { 3 } else { 1 };
    while (app.diagnostics.choices().count() < expected || app.tasks.is_running("build"))
        && Instant::now() < deadline
    {
        app.process_execution_preflight();
        app.process_job_events();
        app.process_vfs_results();
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        app.diagnostics.choices().count(),
        expected,
        "{:?}",
        app.notices.history().collect::<Vec<_>>()
    );
    // A build that exits 1 still failed, unlike a linter reporting findings.
    let status = if with_dependency {
        "succeeded"
    } else {
        "failed"
    };
    assert!(
        app.tasks
            .task_items()
            .iter()
            .any(|item| item.label == "build" && item.detail == status)
    );
    if with_dependency {
        let messages = app
            .diagnostics
            .choices()
            .map(|(_, diagnostic, _)| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert!(messages.contains(&"stdout"));
        assert!(messages.contains(&"stderr"));
        assert!(messages.contains(&"main task"));
    }
}

#[test]
fn matcher_navigation_uses_the_open_buffer_after_its_file_is_deleted() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("deleted.txt");
    fs::write(&path, "first\nsecond\n").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("first\nsecond\n"), Some(path.clone()))
        .unwrap();
    fs::remove_file(&path).unwrap();
    app.state
        .open_text(TextBuffer::from_text("other"), None)
        .unwrap();
    app.navigate_matcher_source(crate::app::picker::MatcherSource {
        path,
        line: 1,
        column: 2,
        encoding: crate::editor::text::PositionEncoding::Utf8,
    });
    assert_eq!(app.state.active_buffer_id(), Some(buffer));
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()].cursor,
        ByteOffset(8)
    );
}

use crate::app::tasks::*;
use crate::files::RootIdentity;
use crate::workspace::WorkspaceId;
use crate::workspace::WorkspaceState;
#[test]
fn formatter_stderr_is_visible_without_entering_the_replacement_payload() {
    let root = tempfile::tempdir().unwrap();
    let scope = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(scope, root.path().join("trust"), 1, false, false, 1);
    let mut editor = EditorState::new(root.path().into());
    let (buffer, _) = editor
        .open_text(TextBuffer::from_text("old"), None)
        .unwrap();
    let mut tasks = Tasks::new(&Config::default());
    tasks.fixture_formatter_result("format", buffer, b"formatted", &editor, &workspace, 1);
    tasks.events.push_front(JobEvent::Output {
        job: JobId(1000),
        stream: JobStream::Stderr,
        bytes: b"syntax error at line 4\n".to_vec(),
        truncated: false,
    });
    assert!(matches!(
        tasks.next_update(&workspace, 1),
        Some(TaskUpdate::Output)
    ));
    assert!(
        tasks.output_items("format")[0]
            .label
            .contains("syntax error")
    );
    let Some(TaskUpdate::Finished { task, .. }) = tasks.next_update(&workspace, 1) else {
        panic!("completion missing")
    };
    assert_eq!(task.output, b"formatted");
}
pub(in crate::app) fn fixture() -> (tempfile::TempDir, EditorState, Workspace, Tasks) {
    let root = tempfile::tempdir().unwrap();
    let scope = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(scope, root.path().join("trust"), 1, false, false, 1);
    let mut editor = EditorState::new(root.path().into());
    let (buffer, _) = editor
        .open_text(TextBuffer::from_text("text"), None)
        .unwrap();
    let mut tasks = Tasks::new(&Config::default());
    tasks.fixture_formatter_result("task", buffer, b"text", &editor, &workspace, 1);
    (root, editor, workspace, tasks)
}
#[test]
fn configuration_changes_cancel_pending_statuses_but_keeps_finished_ones() {
    let (_root, _editor, workspace, mut tasks) = fixture();
    tasks.statuses.insert("task".into(), "running:task".into());
    tasks.statuses.insert("queued".into(), "queued".into());
    tasks
        .statuses
        .insert("starting".into(), "starting:build".into());
    tasks.statuses.insert("finished".into(), "succeeded".into());
    tasks.configure(&Config::default());
    for name in ["task", "queued", "starting"] {
        assert_eq!(tasks.statuses[name], "cancelled");
    }
    assert_eq!(tasks.statuses["finished"], "succeeded");
    assert!(tasks.next_update(&workspace, 1).is_none());
    assert_eq!(
        tasks
            .task_items()
            .iter()
            .filter(|item| item.detail == "cancelled")
            .count(),
        3
    );
}
#[test]
fn stopping_a_finished_task_keeps_its_test_results_current() {
    let (root, editor, workspace, mut tasks) = fixture();
    // The cancelled predecessor can still be waiting for its terminal event when
    // its replacement has already finished.
    tasks.events.clear();
    tasks.stop("task").unwrap();
    let buffer = editor.buffers.iter().next().unwrap().0;
    tasks.fixture_formatter_result("task", buffer, b"new", &editor, &workspace, 1);
    assert!(matches!(
        tasks.next_update(&workspace, 1),
        Some(TaskUpdate::Finished { .. })
    ));
    tasks.tests.insert(
        "task".into(),
        vec![TestCaseResult {
            name: "test".into(),
            status: "failed".into(),
            duration: String::new(),
            message: "failure".into(),
            source: Some(crate::app::picker::MatcherSource {
                path: root.path().join("source"),
                line: 0,
                column: 0,
                encoding: crate::editor::text::PositionEncoding::Utf8,
            }),
        }],
    );
    let key = tasks.test_items("task")[0].1.clone();
    assert!(tasks.stop("task").is_err());
    assert_eq!(tasks.test_items("task").len(), 1);
    assert_eq!(tasks.failed_tests().len(), 1);
    let source = tasks.resolve_test(&key).expect("finished test source");
    assert_eq!(source.path, root.path().join("source"));
    assert_eq!((source.line, source.column), (0, 0));
    assert_eq!(source.encoding, crate::editor::text::PositionEncoding::Utf8);
}
#[test]
fn restarted_task_suppresses_the_cancelled_runs_launch_failure() {
    let (_root, editor, workspace, mut tasks) = fixture();
    tasks.events.clear();
    tasks.stop("task").unwrap();
    let buffer = editor.buffers.iter().next().unwrap().0;
    tasks.fixture_formatter_result("task", buffer, b"new", &editor, &workspace, 1);
    tasks.statuses.insert("task".into(), "starting:task".into());
    tasks.events.push_front(JobEvent::Failed {
        job: JobId(1000),
        message: "cancelled before launch".into(),
    });
    assert!(matches!(
        tasks.next_update(&workspace, 1),
        Some(TaskUpdate::Finished { .. })
    ));
    assert_eq!(tasks.statuses["task"], "process exited");
}
#[test]
fn output_tabs_expand_without_altering_the_matcher_payload() {
    let (_root, _editor, workspace, mut tasks) = fixture();
    tasks.running.get_mut(&JobId(1000)).unwrap().adapter = AdapterContext::Plain;
    tasks.running.get_mut(&JobId(1000)).unwrap().output.clear();
    tasks.events.push_front(JobEvent::Output {
        job: JobId(1000),
        stream: JobStream::Stdout,
        bytes: b"A\tB".to_vec(),
        truncated: false,
    });
    assert!(matches!(
        tasks.next_update(&workspace, 1),
        Some(TaskUpdate::Output)
    ));
    assert_eq!(tasks.output_items("task")[0].label, "A    B");
    assert_eq!(tasks.running[&JobId(1000)].output, b"A\tB\n");
}
#[test]
fn stopping_with_a_buffered_terminal_event_still_finalizes_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let scope = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(scope, root.path().join("trust"), 1, false, false, 1);
    let mut editor = EditorState::new(root.path().into());
    let (buffer, _) = editor
        .open_text(TextBuffer::from_text("text"), None)
        .unwrap();
    let mut tasks = Tasks::new(&Config::default());
    tasks.fixture_formatter_result("format", buffer, b"text", &editor, &workspace, 1);
    assert!(tasks.stop("format").is_ok());
    assert_eq!(tasks.statuses["format"], "cancelled");
    assert!(tasks.next_update(&workspace, 1).is_none());
}
#[test]
fn identity_formatter_normalizes_bom_and_line_endings() {
    let root = tempfile::tempdir().unwrap();
    let scope = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(scope, root.path().join("trust"), 1, false, false, 1);
    for bytes in [b"a\r\n".as_slice(), b"\xef\xbb\xbfa\r\n".as_slice()] {
        let mut editor = EditorState::new(root.path().into());
        let (buffer, _) = editor
            .open_text(TextBuffer::from_bytes(bytes).unwrap(), None)
            .unwrap();
        let source = FormatterSource::capture(&editor, buffer).unwrap();
        let mut owner = Tasks::new(&Config::default());
        let key = RunKey {
            epoch: Arc::new(()),
            workspace: workspace.version(),
            configuration: 1,
        };
        owner.keys.insert("format".into(), key.clone());
        let task = RunningTask {
            output: bytes.to_vec(),
            cancelled: false,
            key,
            diagnostic_source: None,
            requested: "format".into(),
            name: "format".into(),
            adapter: AdapterContext::Formatter { source },
            root: root.path().into(),
            cwd: root.path().into(),
        };
        let transaction = owner
            .formatter_transaction(&task, &editor, &workspace, 1, TransactionId(1))
            .unwrap()
            .unwrap();
        editor.apply_transaction(&transaction).unwrap();
        assert_eq!(editor.buffers[buffer].text.to_bytes(), bytes);
        assert_eq!(editor.buffers[buffer].text.to_string(), "a\n");
    }
}
#[test]
fn task_owner_rejects_stale_plans_and_formatter_instances_before_materialization() {
    let root = tempfile::tempdir().unwrap();
    let scope = WorkspaceState::new(
        WorkspaceId(1),
        [("root".into(), RootIdentity::capture(root.path()).unwrap())],
    )
    .unwrap();
    let workspace = Workspace::new(scope, root.path().join("trust"), 1, false, false, 1);
    let mut editor = EditorState::new(root.path().into());
    let (buffer, _) = editor
        .open_text(
            TextBuffer::from_text("abc"),
            Some(root.path().join("source")),
        )
        .unwrap();
    let mut config = Config::default();
    config.tasks.insert(
        "task".into(),
        TaskConfig {
            kind: crate::config::TaskKind::Task,
            executable: PathBuf::from("/bin/sh"),
            arguments: vec![],
            cwd: None,
            environment: BTreeMap::new(),
            dependencies: vec![],
            timeout_ms: None,
            stdin: false,
            matcher: None,
        },
    );
    let mut owner = Tasks::new(&config);
    owner.configure(&config);
    owner
        .plan_checked("task", &AdapterContext::Plain, &config, &workspace, 1)
        .unwrap();
    assert!(
        owner
            .plan_checked("task", &AdapterContext::Plain, &config, &workspace, 1)
            .is_err()
    );
    let (step, _) = owner.step("task", &workspace, 1).unwrap();
    assert!(owner.step("task", &workspace, 1).is_err());
    owner.cancel_workspace_tasks();
    let before = TextBuffer::test_materializations();
    assert!(
        owner
            .launch(
                step,
                crate::app::workspace::ExecutionReady::fixture(
                    &workspace,
                    ExecutionPreflightResult {
                        id: crate::process::PreflightId(1),
                        executable: PathBuf::from("/bin/sh"),
                        cwd: root.path().into(),
                        root: root.path().into(),
                        root_revision: workspace.root_revision()
                    }
                ),
                &editor,
                &workspace,
                1
            )
            .is_err()
    );
    assert_eq!(TextBuffer::test_materializations(), before);
    let source = FormatterSource::capture(&editor, buffer).unwrap();
    editor.buffers[buffer].text = TextBuffer::from_text("abc");
    assert!(source.stdin(&editor, 1024).is_err());
    assert_eq!(TextBuffer::test_materializations(), before);
    owner
        .plan_checked("task", &AdapterContext::Plain, &config, &workspace, 1)
        .unwrap();
    let (step, _) = owner.step("task", &workspace, 1).unwrap();
    let key = TestKey {
        name: "task".into(),
        run: step.key.epoch.clone(),
        index: 0,
    };
    owner.tests.insert(
        "task".into(),
        vec![TestCaseResult {
            name: "one".into(),
            status: "ok".into(),
            duration: String::new(),
            message: String::new(),
            source: Some(crate::app::picker::MatcherSource {
                path: root.path().join("source"),
                line: 0,
                column: 0,
                encoding: crate::editor::text::PositionEncoding::Utf8,
            }),
        }],
    );
    assert!(owner.resolve_test(&key).is_some());
    owner.cancel_workspace_tasks();
    assert!(owner.resolve_test(&key).is_none());
}
