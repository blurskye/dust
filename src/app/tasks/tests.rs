//! Tasks behavior and application integration tests.

use crate::app::tests::*;
use crate::app::App;
use crate::app::AppEffect;
use crate::app::PickerKind;
use crate::app::startup::StartupTarget;
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
                    .is_some_and(|message| message.contains("formatter document incarnation")),
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
    assert_eq!(app.state.active_view, Some(view));
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
        app.state.views[app.state.active_view.unwrap()].cursor,
        ByteOffset(8)
    );
}
