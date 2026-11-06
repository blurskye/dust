use crate::app::tests::*;

use crate::app::workspace::project_tests::wait_root_operation;

#[test]
pub(in crate::app) fn a_pending_root_switch_rechecks_new_edits_and_terminals() {
    for terminal in [None, Some(false), Some(true)] {
        let (_app_dirs, mut app) = app();
        let old = app.state.workspace_root.clone();
        let new = old.join(".worktree/topic");
        fs::create_dir_all(&new).unwrap();
        prepare_trust_identity(&mut app);
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("clean"), Some(old.join("file.txt")))
            .unwrap();
        app.switch_workspace_root(new);
        assert!(app.workspace.root_operation_pending());
        if let Some(running) = terminal {
            // A newly opened terminal must also block before its process reports Running.
            app.terminal_session.fixture(
                PtyId(1),
                crate::app::terminal::TerminalFixture {
                    profile: "shell".into(),
                    screen: crate::ui::PtyScreen::new(80, 8, 100),
                    running,
                    exit_code: None,
                    truncated: false,
                },
                app.workspace.version(),
            );
            app.terminal_session.fixture_cwd(PtyId(1), old.clone());
        } else {
            app.replace_with_recovered(buffer, "dirty".into()).unwrap();
        }
        app.process_trust_events();
        assert!(!app.workspace.root_operation_pending());
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            app.process_trust_events();
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(app.state.workspace_root, old);
        assert!(app.state.buffers.contains_key(buffer));
        if let Some(running) = terminal {
            assert!(app.terminal_session.contains(PtyId(1)));
            assert_eq!(app.terminal_session.running_count(), usize::from(running));
        }
    }
}

#[test]
pub(in crate::app) fn saving_an_existing_empty_file_detects_external_deletion() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("existing-empty.txt");
    fs::write(&path, b"").unwrap();
    app.edit_path(path.clone());
    crate::app::tests::wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    assert!(!app.state.buffers[buffer].new_file);
    app.handle_configured_key(KeyEvent::character('i'));
    app.handle_configured_key(KeyEvent::character('x'));
    app.handle_configured_key("escape".parse::<KeyEvent>().unwrap());
    fs::remove_file(&path).unwrap();
    // Reopening the absent path must not turn this existing buffer into a new file.
    app.edit_path(path.clone());
    assert!(!app.state.buffers[buffer].new_file);
    app.invoke(crate::editor::CommandInvocation::new("file.save", InvocationSource::Test).unwrap());
    crate::app::tests::wait_vfs(&mut app);
    assert!(!path.exists());
    assert!(app.state.buffers[buffer].dirty);
}

/// The page keys have names a keymap can use, and a focused terminal receives them.
#[test]
pub(in crate::app) fn page_keys_parse_and_count_as_terminal_navigation() {
    assert_eq!(
        "pagedown".parse::<KeyEvent>().unwrap().code,
        crate::editor::input::KeyCode::PageDown
    );
    assert_eq!(
        "pageup".parse::<KeyEvent>().unwrap().code,
        crate::editor::input::KeyCode::PageUp
    );
    assert_eq!(
        "pagedown"
            .parse::<KeyEvent>()
            .unwrap()
            .terminal_navigation(),
        Some(crate::editor::input::TerminalNavigationKey::PageDown)
    );
}

#[test]
pub(in crate::app) fn an_empty_plugin_list_says_how_to_get_one() {
    let (_app_dirs, mut app) = app();
    app.process_effect(AppEffect::PluginList);
    assert_eq!(
        app.ui.picker.as_ref().expect("picker").footer.as_deref(),
        Some("No plugins yet · :pkgsearch <name> finds one · Esc Esc Close")
    );
}

#[test]
pub(in crate::app) fn switching_folders_with_open_terminals_warns_before_ending_them() {
    let main = tempdir().unwrap();
    let state_dir = tempdir().unwrap();
    let main = main.path().canonicalize().unwrap();
    let nested = main.join(".worktree/feature");
    fs::create_dir_all(&nested).unwrap();
    let mut app = App::new(
        StartupTarget::Dashboard {
            root: nested.clone(),
        },
        crate::config::load_bundled_config(),
        false,
        state_dir.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.terminal_session.fixture(
        crate::process::PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: crate::ui::PtyScreen::new(80, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.fixture_cwd(PtyId(1), nested);
    app.switch_workspace_root(main.clone());
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|text| text.contains("switch again")),
        "{:?}",
        app.ui.notification
    );
    assert_eq!(app.terminal_session.running_count(), 1, "nothing ended yet");
    app.switch_workspace_root(main.clone());
    assert_eq!(app.ui.notification.as_deref(), Some("switching…"));
    wait_root_operation(&mut app);
    assert_eq!(app.state.workspace_root, main);
    assert_eq!(app.workspace.roots().len(), 1);
    assert_eq!(app.workspace.roots()[0].identity.canonical_path, main);
    assert!(!app.terminal_session.contains(PtyId(1)));
    assert_eq!(app.terminal_session.running_count(), 0);
}

#[test]
pub(in crate::app) fn reloading_terminal_settings_keeps_running_terminals() {
    let (_app_dirs, mut app) = app();
    app.terminal_session.fixture(
        crate::process::PtyId(1),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: crate::ui::PtyScreen::new(80, 8, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    let mut candidate = crate::config::load_bundled_config();
    let mut profile = candidate.config.terminals.profiles["shell"].clone();
    profile.rows = 40;
    candidate
        .config
        .terminals
        .profiles
        .insert("another".into(), profile);
    app.apply_configuration_candidate(candidate, false)
        .expect("accepted configuration");
    assert_eq!(app.terminal_session.running_count(), 1);
    assert!(
        app.configuration
            .settings()
            .terminals
            .profiles
            .contains_key("another")
    );
}

#[test]
pub(in crate::app) fn removing_a_root_preserves_terminals_and_updates_relative_paths() {
    let (_app_dirs, mut app) = app();
    prepare_trust_identity(&mut app);
    let first = app.workspace.active_root().unwrap();
    let path = app.state.workspace_root.clone();
    let extra = tempfile::tempdir().unwrap();
    let second = app
        .workspace
        .fixture_add_root(
            "second".into(),
            crate::files::RootIdentity::capture(extra.path()).unwrap(),
        )
        .unwrap();
    app.activate_workspace_root(second);
    app.terminal_session.fixture(
        PtyId(101),
        crate::app::terminal::TerminalFixture {
            profile: "shell".into(),
            screen: PtyScreen::new(80, 24, 100),
            running: true,
            exit_code: None,
            truncated: false,
        },
        app.workspace.version(),
    );
    app.terminal_session.fixture_cwd(PtyId(101), path.clone());
    let first_file = path.join("first.txt");
    let second_file = extra.path().canonicalize().unwrap().join("second.txt");
    assert_eq!(app.workspace.roots().len(), 2);
    assert_eq!(app.workspace_relative_label(&first_file), "first.txt");
    assert_eq!(app.workspace_relative_label(&second_file), "second.txt");
    app.remove_workspace_root(second);
    assert_eq!(app.workspace.roots().len(), 1);
    assert!(app.workspace.roots().iter().all(|root| root.id != second));
    assert_eq!(app.workspace_relative_label(&first_file), "first.txt");
    assert_eq!(
        app.workspace_relative_label(&second_file),
        second_file.display().to_string()
    );
    assert_eq!(app.workspace.active_root(), Some(first));
    assert_eq!(app.state.workspace_root, path);
    assert!(app.terminal_session.contains(PtyId(101)));
    app.process_pty_events();
    assert!(app.terminal_session.contains(PtyId(101)));
}
use crate::app::App;
use crate::app::AppEffect;
use crate::app::startup::StartupTarget;
use crate::editor::InvocationSource;
use crate::editor::input::KeyEvent;
use crate::editor::text::TextBuffer;
use crate::process::PtyId;
use crate::ui::PtyScreen;
use std::fs;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::tempdir;
