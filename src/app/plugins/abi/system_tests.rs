//! System imports exercised by the retained Wasm guest and the native owners.

use super::*;
use crate::app::plugins::abi::tests::{action, pump, real_app_with};
use crate::editor::text::TextBuffer;
use crate::editor::{CommandInvocation, CommandValue, InvocationSource};
use crate::plugins::PluginCapability;

fn real_app(capabilities: &[PluginCapability]) -> (Vec<tempfile::TempDir>, App) {
    let (roots, mut app) = real_app_with(&["demo"], capabilities, &[]);
    crate::app::tests::prepare_trust_identity(&mut app);
    (roots, app)
}

fn trust(app: &mut App) {
    crate::app::tests::prepare_trust_identity(app);
    for root in app.workspace.roots().to_vec() {
        app.workspace.fixture_trust_identity(root.identity);
    }
    app.reconcile_workspace_trust();
}

fn settled(app: &mut App) {
    pump(app, |app| {
        app.extension.abi.system.pending_files.is_empty()
            && app.extension.abi.system.events.is_empty()
            && app.extension.abi.waiting.is_empty()
    });
}
fn results(app: &mut App) -> String {
    action(app, "demo", "system-results");
    app.ui
        .notification
        .as_deref()
        .unwrap_or_default()
        .to_owned()
}
fn request(app: &mut App, operation: &str) -> String {
    action(app, "demo", "system-clear");
    action(app, "demo", operation);
    settled(app);
    results(app)
}

#[test]
fn real_guest_files_read_write_list_stat_rename_delete_and_reload_clean_buffers() {
    let (_roots, mut app) = real_app(&[PluginCapability::Files]);
    let root = app.state.workspace_root.clone();
    let path = root.join("system.txt");
    std::fs::write(&path, "old contents\n").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("old contents\n"), Some(path.clone()))
        .unwrap();
    assert!(request(&mut app, "system-write").contains("Done"));
    assert_eq!(std::fs::read(&path).unwrap(), b"new contents\n");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "new contents\n");
    assert!(!app.state.buffers[buffer].dirty);
    assert!(request(&mut app, "system-read").contains("Bytes([110, 101, 119"));
    assert!(request(&mut app, "system-stat").contains("bytes: 13"));
    assert!(request(&mut app, "system-mkdir").contains("Done"));
    assert!(request(&mut app, "system-list").contains("system-dir"));
    assert!(request(&mut app, "system-rename").contains("Done"));
    assert!(!path.exists());
    assert!(root.join("system-renamed.txt").exists());
    assert!(request(&mut app, "system-delete").contains("Done"));
    assert!(!root.join("system-renamed.txt").exists());
}

#[test]
fn real_guest_file_writes_refuse_unsaved_buffers_and_symlink_escapes() {
    let (_roots, mut app) = real_app(&[PluginCapability::Files]);
    let root = app.state.workspace_root.clone();
    let path = root.join("system.txt");
    std::fs::write(&path, "saved").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("saved"), Some(path.clone()))
        .unwrap();
    app.state.buffers[buffer].dirty = true;
    assert!(request(&mut app, "system-write").contains("unsaved changes"));
    assert_eq!(std::fs::read(&path).unwrap(), b"saved");
    // The dirty-buffer check is shared; this escape uses a Unix symlink.
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("system-link")).unwrap();
        assert!(request(&mut app, "system-escape").contains("outside the workspace"));
    }
}

#[test]
fn real_guest_process_streams_before_exit_accepts_stdin_and_reports_exit_status() {
    let (_roots, mut app) = real_app(&[PluginCapability::Process]);
    trust(&mut app);
    action(&mut app, "demo", "system-process");
    pump(&mut app, |app| {
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("Stdout"))
    });
    assert_eq!(
        app.extension.abi.system.processes.len(),
        1,
        "raw chunks arrive before the process exits"
    );
    action(&mut app, "demo", "system-input");
    pump(&mut app, |app| {
        app.extension.abi.system.processes.is_empty()
    });
    settled(&mut app);
    let output = results(&mut app);
    assert!(output.contains("Stderr"), "{output}");
    assert!(output.contains("108, 97, 116, 101, 114"), "{output}");
    assert!(output.contains("Exited(Some(7))"), "{output}");
    action(&mut app, "demo", "system-clear");
    action(&mut app, "demo", "system-process-escape");
    pump(&mut app, |app| {
        app.extension.abi.system.processes.is_empty()
    });
    settled(&mut app);
    let error = results(&mut app);
    assert!(
        error.contains("workspace") || error.contains("root"),
        "{error}"
    );
}

#[test]
fn real_guest_can_kill_owned_processes_and_unload_cancels_remaining_jobs() {
    let (_roots, mut app) = real_app(&[PluginCapability::Process]);
    trust(&mut app);
    action(&mut app, "demo", "system-sleep");
    action(&mut app, "demo", "system-kill");
    pump(&mut app, |app| {
        app.extension.abi.system.processes.is_empty()
    });
    settled(&mut app);
    action(&mut app, "demo", "system-sleep");
    let jobs: Vec<_> = app.extension.abi.system.processes.keys().copied().collect();
    assert_eq!(jobs.len(), 1);
    let load = app.extension.load_id("demo").unwrap();
    app.extension.disable("demo").unwrap();
    app.remove_plugin_contributions("demo", load);
    assert!(app.extension.abi.system.processes.is_empty());
    pump(&mut app, |app| {
        !app.extension
            .abi
            .system
            .jobs
            .as_ref()
            .unwrap()
            .is_running(jobs[0])
    });
}

#[test]
fn real_guest_process_count_is_bounded_and_shutdown_cancels_jobs() {
    let (_roots, mut app) = real_app(&[PluginCapability::Process]);
    trust(&mut app);
    for _ in 0..9 {
        action(&mut app, "demo", "system-sleep");
    }
    settled(&mut app);
    assert_eq!(app.extension.abi.system.processes.len(), 8);
    assert!(results(&mut app).contains("process limit exceeded"));
    app.shutdown_plugin_system();
    pump(&mut app, |app| {
        app.extension.abi.system.processes.is_empty()
    });
}

#[test]
fn real_guest_system_grants_are_required_and_delegated_calls_cannot_widen_them() {
    let (_roots, mut app) = real_app(&[]);
    for operation in ["system-write", "system-process"] {
        action(&mut app, "demo", operation);
        assert!(
            app.ui
                .notification
                .as_deref()
                .is_some_and(|notice| notice.contains("capability denied"))
        );
    }
    let (_roots, mut app) = real_app(&[PluginCapability::Files, PluginCapability::Process]);
    trust(&mut app);
    for operation in ["system-write", "system-process"] {
        action(&mut app, "demo", "system-clear");
        let invocation = CommandInvocation::new("plugin.demo.drive", InvocationSource::Plugin)
            .unwrap()
            .with_argument("abi", CommandValue::String(operation.into()));
        assert!(app.invoke_with_authority(
            invocation,
            CommandAuthority::Plugin {
                initiator: "restricted".into(),
                grants: Default::default()
            }
        ));
        pump(&mut app, |app| !app.input.command_pending());
        settled(&mut app);
        let error = results(&mut app);
        assert!(
            error.contains("restricted") && error.contains("permission"),
            "{error}"
        );
        assert_eq!(app.effect_authority, CommandAuthority::Host);
    }
    assert!(!app.state.workspace_root.join("system.txt").exists());
    assert!(app.extension.abi.system.processes.is_empty());
}

#[test]
fn system_deliveries_keep_events_until_queued_and_never_mix_handler_authority() {
    let (_roots, app) = real_app(&[]);
    let authority = CommandAuthority::Plugin {
        initiator: "restricted".into(),
        grants: Default::default(),
    };
    let mut system = SystemAccess::default();
    system.reply(
        Callback {
            plugin: "demo".into(),
            load: app.extension.load_id("demo").unwrap(),
            handler: "drive".into(),
            authority: authority.clone(),
            token: 1,
        },
        Ok(wit::SystemValue::Done),
    );
    assert!(system.delivery("demo", "other", &authority).is_empty());
    assert!(
        system
            .delivery("demo", "drive", &CommandAuthority::Host)
            .is_empty()
    );
    assert_eq!(system.delivery("demo", "drive", &authority).len(), 1);
    assert_eq!(system.delivery("demo", "drive", &authority).len(), 1);
    system.delivered("demo", "drive", &authority, 1);
    assert!(system.delivery("demo", "drive", &authority).is_empty());
}

#[test]
fn real_guest_syntax_uses_the_current_buffer_tree_with_ranges_and_parent_nodes() {
    let (_roots, mut app) = real_app(&[]);
    assert!(request(&mut app, "system-query").contains("no parsed syntax tree"));
    let path = app.state.workspace_root.join("query.rs");
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("fn hello() {}"), Some(path))
        .unwrap();
    app.reconcile_editor_windows();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while app.syntax.result(buffer).is_none() {
        app.process_syntax();
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let output = request(&mut app, "system-query");
    assert!(
        output.contains("name: \"name\"") && output.contains("start: 3, end: 8"),
        "{output}"
    );
    assert!(request(&mut app, "system-query-range").contains("Captures([])"));
    let output = request(&mut app, "system-node");
    assert!(
        output.contains("identifier") && output.contains("source_file"),
        "{output}"
    );
}
