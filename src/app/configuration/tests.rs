//! Configuration reloads, user keymaps and settings behavior.

use crate::app::configuration::Configuration;
use crate::app::configuration::ResolvedConfiguration;
use crate::app::configuration::ResolvedKeymaps;
use crate::app::configuration::ViewToggle;
use crate::app::configuration::themes::Themes;
use std::sync::Arc;

impl Configuration {
    pub(in crate::app) fn install_bindings(&mut self, keymaps: ResolvedKeymaps, deferred: bool) {
        self.accepted.keymaps = keymaps;
        self.accepted.deferred_plugin_keymaps = deferred;
        self.stamp = Arc::new(());
        self.binding_revision = self.binding_revision.saturating_add(1);
    }

    pub(in crate::app) fn fixture_snapshot(&self) -> ResolvedConfiguration {
        self.accepted.clone()
    }

    pub(in crate::app) fn install_fixture(&mut self, candidate: ResolvedConfiguration) {
        candidate
            .settings
            .validate()
            .expect("valid owned fixture policy");
        self.accepted = candidate;
        self.themes = Themes::default();
        self.stamp = Arc::new(());
        self.binding_revision = self.binding_revision.saturating_add(1);
    }
}
use crate::app::tests::app;

#[test]
pub(in crate::app) fn saved_theme_restore_rejects_invalid_data_and_cannot_overwrite_newer_interactions()
 {
    let (_app_dirs, mut app) = app();
    let original = app.configuration.theme().clone();
    let mut saved = original.clone();
    saved.name = "remembered".into();
    saved.styles.default.foreground = crate::config::ThemeColor::Red;
    let snapshot = serde_json::to_string(&saved).unwrap();
    let wait = |app: &mut App| {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = app.configuration.poll_theme_restore() {
                return result;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    };
    app.configuration.begin_theme_restore();
    app.configuration
        .restore_theme(Some("invalid JSON"))
        .unwrap();
    assert!(wait(&mut app).is_err());
    app.configuration.begin_theme_restore();
    app.configuration
        .restore_theme(Some(&snapshot.replace("\"red\"", "\"#aébcd\"")))
        .unwrap();
    assert!(
        wait(&mut app).is_err(),
        "malformed multibyte RGB must not panic the configuration worker"
    );
    assert_eq!(app.configuration.theme(), &original);
    app.configuration.begin_theme_restore();
    assert!(
        app.configuration
            .restore_theme(Some(&"x".repeat(128 * 1024 + 1)))
            .is_err()
    );
    app.configuration.begin_theme_restore();
    app.process_effect(AppEffect::SelectTheme);
    // Refuse an old state result before validating its payload size.
    app.configuration
        .restore_theme(Some(&"x".repeat(128 * 1024 + 1)))
        .unwrap();
    assert!(app.configuration.poll_theme_restore().is_none());
    app.close_picker();
    app.configuration.begin_theme_restore();
    app.configuration.restore_theme(Some(&snapshot)).unwrap();
    app.process_effect(AppEffect::SelectTheme);
    app.close_picker();
    assert!(app.configuration.poll_theme_restore().is_none());
    assert_eq!(app.configuration.theme(), &original);
    app.configuration.begin_theme_restore();
    app.configuration.restore_theme(Some(&snapshot)).unwrap();
    app.apply_configuration_candidate(crate::config::load_bundled_config(), false)
        .unwrap();
    assert!(app.configuration.poll_theme_restore().is_none());
    assert_eq!(app.configuration.theme(), &original);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        app.configuration.begin_theme_restore();
        match app.configuration.restore_theme(Some(&snapshot)) {
            Ok(()) => break,
            Err(error) if error == "configuration reload is already pending" => {
                assert!(Instant::now() < deadline);
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => panic!("theme restore request failed: {error}"),
        }
    }
    loop {
        if let Some(result) = app.configuration.poll_theme_restore() {
            result.unwrap();
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(app.configuration.theme(), &saved);
    assert_eq!(
        app.configuration.theme_state().unwrap().as_deref(),
        Some(
            crate::config::theme_snapshot(&crate::config::ThemeChoice {
                theme: saved.clone(),
                path: "<editor-state:theme:remembered>".into(),
            })
            .unwrap()
            .as_str()
        )
    );
}

#[test]
pub(in crate::app) fn delayed_persistence_preserves_newer_runtime_breakpoint_and_watch_choices() {
    use crate::persistence::PersistedBreakpoint;
    use crate::persistence::PersistedWatch;
    let (_app_dirs, mut app) = app();
    let saved = tempdir().unwrap();
    let path = app.state.workspace_root.join("main.rs");
    let mut state = PersistedState::default();
    state
        .metadata
        .insert("lsp.runtime.rust-analyzer".into(), "managed".into());
    state
        .metadata
        .insert("lsp.version.rust-analyzer".into(), "0.3.3008".into());
    state.breakpoints = [1, 2, 3]
        .into_iter()
        .map(|line| PersistedBreakpoint {
            path: path.clone(),
            line,
            column: None,
            condition: Some("saved".into()),
            hit_condition: None,
            log_message: None,
        })
        .collect();
    state.watches = ["saved", "removed", "untouched"]
        .into_iter()
        .map(|expression| PersistedWatch {
            expression: expression.into(),
        })
        .collect();
    let state_path = saved.path().join("session.json");
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    let (worker, release) = PersistenceWorker::gated_for_test(1);
    app.persistence = crate::app::persistence::Persistence::new(worker, state_path);
    let row = |line, condition: &str| crate::debugger::DapSourceBreakpoint {
        path: path.clone(),
        line,
        column: None,
        condition: Some(condition.into()),
        hit_condition: None,
        log_message: None,
    };
    app.debugger_session
        .breakpoint(row(2, "old"), false)
        .unwrap();
    app.debugger_session.add_watch("removed".into());
    app.load_persistence();
    app.tooling
        .select_runtime("rust-analyzer", LspRuntimeSource::Path, None)
        .unwrap();
    app.tooling
        .select_version("rust-analyzer", "0.3.3025".into())
        .unwrap();
    app.debugger_session
        .breakpoint(row(1, "new"), false)
        .unwrap();
    app.debugger_session
        .breakpoint(row(2, "old"), true)
        .unwrap();
    app.debugger_session.add_watch("live".into());
    app.debugger_session.remove_watch("removed");
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.persistence.pending() {
        app.process_persistence_events();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(
        !app.tooling
            .metadata()
            .contains_key("lsp.runtime.rust-analyzer")
    );
    assert_eq!(
        app.tooling
            .managed_version("rust-analyzer")
            .map(String::as_str),
        Some("0.3.3025")
    );
    let breakpoints = app.debugger_session.persisted_breakpoints();
    assert_eq!(breakpoints.len(), 2);
    assert!(
        breakpoints
            .iter()
            .any(|row| row.line == 1 && row.condition.as_deref() == Some("new"))
    );
    assert!(
        breakpoints
            .iter()
            .any(|row| row.line == 3 && row.condition.as_deref() == Some("saved"))
    );
    assert_eq!(
        app.debugger_session.watches(),
        ["live", "saved", "untouched"]
    );
}

#[test]
pub(in crate::app) fn theme_picker_replaces_the_current_choice_with_its_installed_override() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    let mut installed = app.configuration.theme().clone();
    installed.styles.accent.foreground = crate::config::ThemeColor::Red;
    fs::create_dir(root.join("themes")).unwrap();
    fs::write(
        root.join("themes").join(format!("{}.toml", installed.name)),
        toml::to_string(&installed).unwrap(),
    )
    .unwrap();
    let mut configuration = app.configuration.fixture_snapshot();
    configuration.sources.settings = root.join("config.toml");
    app.configuration.install_fixture(configuration);
    app.process_effect(AppEffect::SelectTheme);
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.ui.picker.as_ref().unwrap().loading {
        app.process_theme_results();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let label = crate::config::theme_display_name(&installed.name);
    let rows = &app.ui.picker.as_ref().unwrap().rows;
    assert_eq!(rows.iter().filter(|row| row.label == label).count(), 1);
    let key = match app.selected_picker_payload().unwrap() {
        crate::app::picker::PickerPayload::Theme(key) => key.clone(),
        _ => panic!("theme key"),
    };
    assert_eq!(app.configuration.apply_theme(&key).unwrap(), installed.name);
    assert_eq!(app.configuration.theme(), &installed);
}

#[test]
pub(in crate::app) fn theme_picker_previews_cancels_applies_and_never_overwrites_a_newer_reload() {
    let root = tempdir().unwrap();
    let saved = tempdir().unwrap();
    fs::create_dir(root.path().join("themes")).unwrap();
    let original_file = "# untouched configuration\n";
    fs::write(root.path().join("config.toml"), original_file).unwrap();
    let mut amber = crate::config::load_bundled_config().theme;
    amber.name = "amber".into();
    amber.styles.accent.foreground = crate::config::ThemeColor::Yellow;
    fs::write(
        root.path().join("themes/amber.toml"),
        toml::to_string(&amber).unwrap(),
    )
    .unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.path = root.path().join("config.toml");
    let mut app = App::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        loaded.clone(),
        false,
        saved.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((100, 30));
    app.process_effect(AppEffect::NewBuffer);
    app.handle_configured_key(KeyEvent::character('i'));
    let view = app.state.active_view().unwrap();
    assert_eq!(
        app.state.views[view].mode,
        crate::editor::EditorMode::Insert
    );
    let original_theme = app.configuration.theme().clone();
    let original_settings = app.configuration.settings().clone();
    let original_revision = app.configuration.binding_revision();
    let original_workspace = app.workspace.version();
    let choose = |app: &mut App| {
        app.process_effect(AppEffect::SelectTheme);
        assert_eq!(app.active_input_context(), "theme_picker");
        assert_eq!(
            app.ui.picker.as_ref().unwrap().input_hint,
            "Press Escape to cancel"
        );
        assert!(app.ui.picker.as_ref().unwrap().loading);
        for key in "amber".chars() {
            app.handle_configured_key(KeyEvent::character(key));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.ui.picker.as_ref().unwrap().loading {
            app.process_theme_results();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(app.ui.picker.as_ref().unwrap().query, "amber");
        app.reconcile_editor_windows();
        let rect = app.ui.surface_content.picker.as_ref().unwrap().outer;
        assert!(rect.width <= 64 && rect.height <= 12);
        let rows = &app.ui.picker.as_ref().unwrap().rows;
        assert!(!rows.is_empty());
        assert!(rows.iter().all(|row| row.label == "amber"));
        assert_eq!(
            rows.len(),
            1,
            "the applied installed theme must not appear twice"
        );
    };
    choose(&mut app);
    assert_eq!(app.configuration.theme(), &amber);
    assert_eq!(app.configuration.settings(), &original_settings);
    assert_eq!(app.configuration.binding_revision(), original_revision);
    assert_eq!(app.workspace.version(), original_workspace);
    assert!(
        app.configuration
            .explain("theme.styles.accent.foreground")
            .unwrap()
            .source
            .contains("not saved")
    );
    for _ in 0..5 {
        app.handle_configured_key("backspace".parse().unwrap());
    }
    assert_eq!(app.configuration.theme(), &amber); // clearing the query retains the selected row
    assert_eq!(app.ui.picker.as_ref().unwrap().rows.len(), 15);
    assert_eq!(
        app.ui
            .picker
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .filter(|row| row.label == "Dust")
            .count(),
        1
    );
    app.window_session.observe_size((220, 70));
    app.reconcile_editor_windows();
    let rect = app.ui.surface_content.picker.as_ref().unwrap().outer;
    assert_eq!((rect.width, rect.height), (64, 12));
    app.handle_configured_key("down".parse().unwrap()); // wrap from installed amber to current Dust
    assert_eq!(app.configuration.theme(), &original_theme);
    app.handle_configured_key("up".parse().unwrap()); // wrap back to installed amber
    assert_eq!(app.configuration.theme(), &amber);
    let old_key = match app.selected_picker_payload().unwrap() {
        crate::app::picker::PickerPayload::Theme(key) => key.clone(),
        _ => panic!("theme key"),
    };
    app.handle_configured_key("escape".parse().unwrap());
    assert!(app.ui.picker.is_none());
    assert_eq!(app.configuration.theme(), &original_theme);
    assert!(app.configuration.apply_theme(&old_key).is_err());
    choose(&mut app);
    // Unrelated view toggles must preserve both the live preview and its selectable rows.
    app.process_effect(AppEffect::ViewToggle(ViewToggle::Wrap));
    assert_eq!(app.configuration.theme(), &amber);
    assert!(app.ui.picker.is_some());
    app.process_theme_results();
    assert_eq!(app.configuration.theme(), &amber);
    app.process_effect(AppEffect::ViewToggle(ViewToggle::Wrap));
    // A query with no selection must stop previewing the previously selected row.
    app.handle_configured_key(KeyEvent::character('z'));
    assert_eq!(app.configuration.theme(), &original_theme);
    app.handle_configured_key("backspace".parse().unwrap());
    assert_eq!(app.configuration.theme(), &amber);
    app.handle_configured_key("enter".parse().unwrap());
    assert!(app.ui.picker.is_none());
    assert_eq!(app.configuration.theme(), &amber);
    assert_eq!(app.configuration.binding_revision(), original_revision);
    assert_eq!(app.configuration.settings(), &original_settings);
    assert_eq!(
        app.state.views[view].mode,
        crate::editor::EditorMode::Insert
    );
    assert!(
        app.configuration
            .explain("ui.theme")
            .unwrap()
            .value
            .contains("amber")
    );
    // Enter queues the ordinary state writer; it is not necessary to exit first.
    app.process_persistence_events();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.persistence.pending() {
        app.process_persistence_events();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let saved_state: crate::persistence::PersistedState =
        serde_json::from_slice(&fs::read(saved.path().join("session.json")).unwrap()).unwrap();
    let snapshot = saved_state.metadata.get("ui.theme.snapshot").unwrap();
    let mut snapshot: serde_json::Value = serde_json::from_str(snapshot).unwrap();
    assert_eq!(
        snapshot.as_object_mut().unwrap().remove("bundled"),
        Some(serde_json::Value::Bool(false))
    );
    assert_eq!(
        serde_json::from_value::<crate::config::ThemeConfig>(snapshot).unwrap(),
        amber
    );
    let mut restarted = App::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        loaded.clone(),
        false,
        saved.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    restarted.load_persistence();
    let deadline = Instant::now() + Duration::from_secs(3);
    while restarted.configuration.theme() != &amber {
        restarted.process_persistence_events();
        restarted.process_theme_results();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(restarted.configuration.settings(), &original_settings);
    assert!(
        restarted
            .configuration
            .explain("ui.theme")
            .unwrap()
            .source
            .contains("editor-state")
    );
    // Opening another picker discards the preview, not the already applied theme.
    choose(&mut app);
    app.open_builtin_picker(PickerKind::Commands);
    assert_eq!(app.configuration.theme(), &amber);
    choose(&mut app);
    loaded.theme.name = "reloaded".into();
    loaded.config.ui.theme = "reloaded".into();
    app.apply_configuration_candidate(loaded.clone(), false)
        .unwrap();
    app.close_picker();
    app.process_theme_results();
    assert_eq!(app.configuration.theme().name, "reloaded");
    assert!(app.configuration.apply_theme(&old_key).is_err());
    assert_eq!(
        fs::read_to_string(root.path().join("config.toml")).unwrap(),
        original_file
    );
    // A saved-theme read shares discovery's FIFO. Waiting for its rejection establishes
    // completion without changing the theme or advancing the binding revision.
    let wait_for_discovery = |app: &mut App| {
        app.configuration.begin_theme_restore();
        app.configuration
            .restore_theme(Some("invalid JSON"))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = app.configuration.poll_theme_restore() {
                assert!(result.is_err());
                break;
            }
            assert!(Instant::now() < deadline, "theme discovery did not finish");
            thread::sleep(Duration::from_millis(1));
        }
        app.process_theme_results();
    };
    // Cancellation before worker delivery cannot reopen the surface.
    app.process_effect(AppEffect::SelectTheme);
    app.handle_configured_key("escape".parse().unwrap());
    wait_for_discovery(&mut app);
    assert!(app.ui.picker.is_none());
    assert_eq!(app.configuration.theme().name, "reloaded");
    app.process_effect(AppEffect::SelectTheme);
    assert!(app.ui.picker.as_ref().unwrap().loading);
    loaded.theme.name = "newest".into();
    loaded.config.ui.theme = "newest".into();
    app.apply_configuration_candidate(loaded, false).unwrap();
    wait_for_discovery(&mut app);
    assert!(
        app.ui.picker.is_none(),
        "an outdated load must not republish its picker"
    );
    assert_eq!(app.configuration.theme().name, "newest");
}

#[test]
fn stale_configuration_reloads_are_never_applied() {
    assert!(crate::app::configuration::configuration_reload_is_newer(
        4, 5
    ));
    assert!(!crate::app::configuration::configuration_reload_is_newer(
        5, 5
    ));
    assert!(!crate::app::configuration::configuration_reload_is_newer(
        6, 5
    ));
}

#[test]
pub(in crate::app) fn complete_configuration_reload_is_atomic() {
    let (_app_dirs, mut app) = app();
    let original_theme = app.configuration.theme().clone();
    let explanation = app
        .configuration
        .explain("editor.tab_width")
        .expect("explanation");
    assert_eq!(explanation.source, "bundled defaults");
    assert_eq!(explanation.contributors, vec!["bundled defaults"]);
    let original_width = app.configuration.settings().editor.tab_width;
    let mut invalid = crate::config::load_bundled_config();
    invalid.config.editor.tab_width = original_width + 1;
    invalid
        .config
        .input
        .text_commands
        .insert("editor.insert".into(), "unknown.command".into());
    assert!(app.apply_configuration_candidate(invalid, false).is_err());
    assert_eq!(
        app.configuration.settings().editor.tab_width,
        original_width
    );
    assert_eq!(app.configuration.theme(), &original_theme);

    let mut valid = crate::config::load_bundled_config();
    valid.config.editor.tab_width = original_width + 1;
    valid.dashboard.identity = "CUSTOM DASH".into();
    app.apply_configuration_candidate(valid, false)
        .expect("valid complete snapshot");
    assert_eq!(
        app.configuration.settings().editor.tab_width,
        original_width + 1
    );
    assert_eq!(app.configuration.dashboard().identity, "CUSTOM DASH");
}

#[test]
pub(in crate::app) fn resolved_explain_reports_exact_layered_and_replacement_provenance() {
    let (_app_dirs, app) = app();
    let root = tempdir().expect("tempdir");
    let config_path = root.path().join("config.toml");
    let config_source = "editor = { tab_width = 7, markdown = { glyphs = { bullets = ['*'] } } }\n[command_labels]\n\"view.cursor.move\" = \"MOVE\"\n";
    fs::write(&config_path, config_source).expect("config");
    let mut loaded = crate::config::load_config(&config_path);

    let dashboard_path = root.path().join("dashboard.toml");
    let dashboard_source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/defaults/dashboard.toml"
    ))
    .replace("D U S T", "PROVENANCE DASH");
    loaded.dashboard = toml::from_str(&dashboard_source).expect("dashboard");
    loaded.dashboard_path = dashboard_path.clone();
    loaded.dashboard_spans = crate::config::setting_spans(&dashboard_source);

    let theme_path = root.path().join("provenance.toml");
    let theme_source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/themes/dust.toml"
    ))
    .replace("name = \"dust\"", "name = \"provenance\"");
    loaded.theme = toml::from_str(&theme_source).expect("theme");
    loaded.theme_path = theme_path.clone();
    loaded.theme_spans = crate::config::setting_spans(&theme_source);

    let (resolved, diagnostic) =
        crate::app::configuration::resolve_configuration(&app.commands, loaded, true)
            .expect("resolve");
    assert!(diagnostic.is_none());
    let label = resolved
        .explain(r#"command_labels."view.cursor.\u006dove""#)
        .expect("escaped command label");
    assert_eq!(label.value, "\"MOVE\"");
    assert!(label.source.starts_with(&config_path.display().to_string()));
    let setting = resolved.explain("editor.tab_width").expect("setting");
    assert!(
        setting
            .source
            .starts_with(&config_path.display().to_string())
    );
    assert_eq!(setting.contributors[0], "bundled defaults");
    assert_eq!(setting.contributors[1], setting.source);
    let bullet = resolved
        .explain("editor.markdown.glyphs.bullets.0")
        .expect("bullet");
    assert_eq!(bullet.value, "\"*\"");
    assert!(
        bullet
            .source
            .starts_with(&config_path.display().to_string())
    );
    assert_eq!(bullet.contributors, ["bundled defaults", &bullet.source]);
    let inherited = resolved.explain("editor.indent_width").expect("inherited");
    assert_eq!(inherited.source, "bundled defaults");

    let dashboard = resolved
        .explain("dashboard.identity")
        .expect("dashboard identity");
    assert!(
        dashboard
            .source
            .starts_with(&dashboard_path.display().to_string())
    );
    assert_eq!(dashboard.contributors, vec![dashboard.source.clone()]);
    let dashboard_action = resolved
        .explain("dashboard.action.0.label")
        .expect("dashboard action label");
    assert_eq!(
        dashboard_action.contributors,
        vec![dashboard_action.source.clone()]
    );

    let theme = resolved.explain("theme.name").expect("theme name");
    assert!(theme.source.starts_with(&theme_path.display().to_string()));
    assert_eq!(theme.contributors, vec![theme.source.clone()]);

    for (source, explanation, expected) in [
        (config_source, setting, "7"),
        (dashboard_source.as_str(), dashboard, "\"PROVENANCE DASH\""),
        (dashboard_source.as_str(), dashboard_action, "\"Find File\""),
        (theme_source.as_str(), theme, "\"provenance\""),
    ] {
        let range = explanation
            .source
            .split_once(":bytes ")
            .and_then(|(_, range)| range.split_once(".."))
            .and_then(|(start, end)| Some(start.parse().ok()?..end.parse().ok()?))
            .expect("byte range");
        assert_eq!(&source[range], expected);
    }
}

#[test]
pub(in crate::app) fn disabled_insert_prefix_is_consumed_on_timeout_and_rejection() {
    let (_app_dirs, mut app) = app();
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(
        std::time::SystemTime::now(),
        Instant::now(),
    ));
    app.clock = clock.clone();
    app.state
        .open_text(TextBuffer::from_text(""), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.handle_configured_key(KeyEvent::character('i'));
    let mut loaded = crate::config::load_bundled_config().keymaps;
    for (id, keys, command) in [
        ("user.disabled-j", "j", None),
        ("user.long-jk", "j k", Some("edit.insert_text")),
    ] {
        loaded.entries.push(crate::config::KeymapEntry {
            id: id.into(),
            context: Some("editor".into()),
            mode: "insert".into(),
            keys: Some(keys.into()),
            command: command.map(Into::into),
            arguments: command
                .map(|_| {
                    [("text".into(), toml::Value::String("mapped".into()))]
                        .into_iter()
                        .collect()
                })
                .unwrap_or_default(),
            unmap: command.is_none(),
            disable: false,
            pass_through: false,
            desc: None,
        });
    }
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    app.configuration.install_bindings(keymaps, false);
    let buffer = app.state.active_buffer_id().unwrap();
    app.handle_configured_key(KeyEvent::character('j'));
    clock.advance(Duration::from_millis(
        app.configuration.settings().input.mapping_timeout_ms,
    ));
    app.flush_keymap_timeout();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "");
    app.handle_configured_key(KeyEvent::character('j'));
    app.handle_configured_key(KeyEvent::character('x'));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "x");
    app.handle_configured_key(KeyEvent::character('j'));
    app.handle_configured_key(KeyEvent::character('k'));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "xmapped");
}

#[test]
pub(in crate::app) fn user_keymaps_execute_typed_commands_and_unmap_defaults() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.path = PathBuf::from("keymaps.toml");
    loaded.entries.extend([
        crate::config::KeymapEntry {
            id: "user.test.user-keymaps-execute-typed-commands-and-unmap-defaults".into(),
            context: Some("editor".into()),
            mode: "normal".into(),
            keys: Some("<leader> q".into()),
            command: Some("editor.quit".into()),
            arguments: [("force".into(), toml::Value::Boolean(true))]
                .into_iter()
                .collect(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
        },
        crate::config::KeymapEntry {
            id: "user.test.user-keymaps-execute-typed-commands-and-unmap-defaults.case-2".into(),
            context: Some("editor".into()),
            mode: "normal".into(),
            keys: Some("x".into()),
            command: None,
            arguments: Default::default(),
            unmap: true,
            disable: false,
            pass_through: false,
            desc: None,
        },
    ]);
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none());
    assert!(
        keymaps
            .conflicts
            .iter()
            .any(|conflict| conflict.contains("shadows"))
    );
    app.configuration.install_bindings(keymaps, false);
    let inspection = app
        .configuration
        .keymaps()
        .inspect_id("user.test.user-keymaps-execute-typed-commands-and-unmap-defaults")
        .expect("inspect remap");
    assert_eq!(inspection.source_keys, "<leader> q");
    assert_eq!(inspection.keys, "space q");
    let unmap = app
        .configuration
        .keymaps()
        .inspect_id("user.test.user-keymaps-execute-typed-commands-and-unmap-defaults.case-2")
        .expect("exact-sequence unmap inspection");
    assert_eq!(unmap.operation, KeymapOperation::Unmap);
    assert_eq!(unmap.source_keys, "x");
    assert_eq!(
        inspection.arguments.get("force"),
        Some(&CommandValue::Bool(true))
    );
    app.handle_configured_key(KeyEvent::character('x'));
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "abc");
    for key in ["i", "!", "escape"] {
        app.handle_configured_key(key.parse().unwrap());
    }
    assert!(app.state.buffers[buffer].dirty);
    app.handle_configured_key(KeyEvent::character(' '));
    app.handle_configured_key(KeyEvent::character('q'));
    assert!(app.should_quit);
}

#[test]
pub(in crate::app) fn symbolic_leaders_expand_before_dispatch_and_preserve_literal_space_actions() {
    let (_app_dirs, app) = app();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    let bundled_count = loaded.entries.len();
    loaded.entries.push(crate::config::KeymapEntry {
        id: "user.test.symbolic-leaders-expand-before-dispatch-and-preserve-literal-space-actions"
            .into(),
        context: Some("dashboard".into()),
        mode: String::new(),
        keys: Some("<localleader> z".into()),
        command: Some("health.open".into()),
        arguments: Default::default(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    });
    loaded.bundled_count = bundled_count;
    let policy = crate::config::KeymapPolicyConfig {
        leader: "\\".into(),
        local_leader: ";".into(),
    };
    let (keymaps, diagnostic) = compile_keymaps(&app.commands, &loaded, &policy);
    assert!(diagnostic.is_none(), "{diagnostic:?}");

    let leader_binding = keymaps
        .inspect_id("builtin.global.settings.editor-normal")
        .expect("bundled leader binding");
    assert_eq!(leader_binding.source_keys, "<leader> ,");
    assert_eq!(leader_binding.keys, "\\ ,");
    assert_eq!(
        keymaps
            .binding_for("dashboard", "health.open")
            .expect("local leader binding"),
        "; z"
    );
    let literal_space = keymaps
        .inspect("workspace_edit", "workspace.edit.toggle")
        .expect("literal space action");
    assert_eq!(literal_space.source_keys, "space");
    assert_eq!(literal_space.keys, "space");

    let mut dispatch = crate::editor::input::KeymapSequence::default();
    assert_eq!(
        keymaps.resolve(&mut dispatch, "editor.normal", KeyEvent::character('\\')),
        MappingResolution::Pending
    );
    assert_eq!(
        keymaps.resolve(&mut dispatch, "editor.normal", KeyEvent::character('f')),
        MappingResolution::Pending
    );
    assert!(matches!(
        keymaps.resolve(&mut dispatch, "editor.normal", KeyEvent::character('b')),
        MappingResolution::Ready(invocation)
            if invocation.id.as_str() == "buffer.picker.open"
    ));
}

#[test]
pub(in crate::app) fn expanded_duplicate_sequences_are_rejected_with_symbolic_diagnostics() {
    let (_app_dirs, app) = app();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    let mapping = |id: &str, keys: &str| crate::config::KeymapEntry {
        id: id.into(),
        context: Some("dashboard".into()),
        mode: String::new(),
        keys: Some(keys.into()),
        command: Some("health.open".into()),
        arguments: Default::default(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    };
    loaded.entries.extend([
        mapping("user.test.expanded-symbolic", "<leader> z"),
        mapping("user.test.expanded-literal", "space z"),
    ]);
    let (_, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    let diagnostic = diagnostic.expect("expanded duplicate must fail");
    assert!(diagnostic.contains("<leader> z"));
    assert!(diagnostic.contains("expanded to `space z`"));
    assert!(diagnostic.contains("conflicts with id"));
}

#[test]
pub(in crate::app) fn invalid_leader_policy_rejects_the_complete_configuration_transactionally() {
    let (_app_dirs, mut app) = app();
    let original_policy = app.configuration.settings().input.keymaps.clone();
    let original_binding = app
        .configuration
        .keymaps()
        .binding_for("editor.normal", "buffer.picker.open")
        .expect("bundled binding")
        .to_owned();
    for invalid_leader in ["", "<leader>", "a b c d e", "not-a-key"] {
        let mut candidate = crate::config::load_bundled_config();
        candidate.config.input.keymaps.leader = invalid_leader.into();
        let error = app
            .apply_configuration_candidate(candidate, false)
            .expect_err("invalid leader must reject the whole candidate");
        assert!(error.contains("input.keymaps.leader"), "{error}");
        assert_eq!(app.configuration.settings().input.keymaps, original_policy);
        assert_eq!(
            app.configuration
                .keymaps()
                .binding_for("editor.normal", "buffer.picker.open"),
            Some(original_binding.as_str())
        );
    }
}

#[test]
pub(in crate::app) fn identity_rebind_moves_a_bundled_mapping_without_leaving_the_old_sequence() {
    let (_app_dirs, app) = app();
    let target = "builtin.global.files-buffers.editor-normal";
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.path = PathBuf::from("keymaps.toml");
    loaded.user_spans.clear();
    loaded.entries.push(crate::config::KeymapEntry {
        id: target.into(),
        context: Some("editor".into()),
        mode: "normal".into(),
        keys: Some("g b".into()),
        command: Some("buffer.picker.open".into()),
        arguments: BTreeMap::new(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    });
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    assert!(
        !keymaps
            .conflicts()
            .iter()
            .any(|conflict| conflict.contains(target))
    );
    assert_eq!(
        keymaps.binding_for("editor.normal", "buffer.picker.open"),
        Some("g b")
    );
    assert!(keymaps.inspect_key("editor.normal", "space f b").is_none());
    let by_id = keymaps.inspect_id(target).expect("replacement identity");
    assert_eq!(by_id.source_keys, "g b");
    assert_eq!(by_id.keys, "g b");
    assert!(
        by_id.source.contains("keymaps.toml:declaration 1"),
        "unexpected source {}",
        by_id.source
    );
    assert_eq!(
        keymaps
            .inspect("editor.normal", "buffer.picker.open")
            .expect("reverse lookup")
            .id,
        target
    );

    let mut dispatch = crate::editor::input::KeymapSequence::default();
    assert_eq!(
        keymaps.resolve(&mut dispatch, "editor.normal", KeyEvent::character('g')),
        MappingResolution::Pending
    );
    assert!(matches!(
        keymaps.resolve(&mut dispatch, "editor.normal", KeyEvent::character('b')),
        MappingResolution::Ready(invocation)
            if invocation.id.as_str() == "buffer.picker.open"
    ));
}

#[test]
pub(in crate::app) fn identity_disable_inherits_symbolic_location_and_tracks_leader_changes() {
    let (_app_dirs, app) = app();
    let target = "builtin.global.files-explorer.editor-normal";
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.entries.push(crate::config::KeymapEntry {
        id: target.into(),
        context: None,
        mode: String::new(),
        keys: None,
        command: None,
        arguments: BTreeMap::new(),
        unmap: false,
        disable: true,
        pass_through: false,
        desc: None,
    });
    for leader in [",", ";"] {
        let policy = crate::config::KeymapPolicyConfig {
            leader: leader.into(),
            local_leader: "\\".into(),
        };
        let (keymaps, diagnostic) = compile_keymaps(&app.commands, &loaded, &policy);
        assert!(diagnostic.is_none(), "{diagnostic:?}");
        assert_ne!(
            keymaps.binding_for("editor.normal", "explorer.panel.toggle"),
            Some(format!("{leader} f e").as_str())
        );
        let inspection = keymaps.inspect_id(target).expect("disable inspection");
        assert_eq!(inspection.operation, KeymapOperation::Disable);
        assert_eq!(inspection.command, None);
        assert_eq!(inspection.source_keys, "<leader> f e");
        assert_eq!(inspection.keys, format!("{leader} f e"));

        let mut dispatch = crate::editor::input::KeymapSequence::default();
        assert_eq!(
            keymaps.resolve(
                &mut dispatch,
                "editor.normal",
                KeyEvent::character(leader.chars().next().expect("leader"))
            ),
            MappingResolution::Ambiguous
        );
        assert_eq!(
            keymaps.resolve(&mut dispatch, "editor.normal", KeyEvent::character('f')),
            MappingResolution::Ambiguous
        );
        assert_eq!(
            keymaps.resolve(&mut dispatch, "editor.normal", KeyEvent::character('e')),
            MappingResolution::Disabled
        );
    }
}

#[test]
pub(in crate::app) fn identity_disable_rejects_unknown_omitted_and_overdeclared_targets() {
    let (_app_dirs, app) = app();
    let target = "builtin.global.files-explorer.editor-normal";
    for (id, context, expected) in [
        (
            "user.missing.binding",
            None,
            "targets no lower contribution",
        ),
        (
            target,
            Some("editor".to_owned()),
            "must contain only `id` and `disable = true`",
        ),
    ] {
        let mut loaded = crate::config::load_bundled_config().keymaps;
        loaded.entries.push(crate::config::KeymapEntry {
            id: id.into(),
            context,
            mode: String::new(),
            keys: None,
            command: None,
            arguments: BTreeMap::new(),
            unmap: false,
            disable: true,
            pass_through: false,
            desc: None,
        });
        let policy = crate::config::KeymapPolicyConfig::default();
        let (_, diagnostic) = compile_keymaps(&app.commands, &loaded, &policy);
        assert!(
            diagnostic
                .as_deref()
                .is_some_and(|diagnostic| diagnostic.contains(expected)),
            "missing `{expected}` in {diagnostic:?}"
        );
    }
}

#[test]
pub(in crate::app) fn malformed_keymap_identity_rejects_the_configuration_transactionally() {
    let (_app_dirs, mut app) = app();
    let original = app
        .configuration
        .keymaps()
        .binding_for("editor.normal", "buffer.picker.open")
        .expect("original")
        .to_owned();
    for id in [
        String::new(),
        "UPPER.invalid".into(),
        "one-segment".into(),
        format!("user.{}", "x".repeat(129)),
    ] {
        let mut candidate = crate::config::load_bundled_config();
        candidate.keymaps.entries.push(crate::config::KeymapEntry {
            id,
            context: Some("dashboard".into()),
            mode: String::new(),
            keys: Some("z".into()),
            command: Some("health.open".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
        });
        let error = app
            .apply_configuration_candidate(candidate, false)
            .expect_err("invalid identity must reject candidate");
        assert!(error.contains("invalid id"), "{error}");
        assert_eq!(
            app.configuration
                .keymaps()
                .binding_for("editor.normal", "buffer.picker.open"),
            Some(original.as_str())
        );
    }
}

#[test]
pub(in crate::app) fn invalid_user_keymap_set_is_rejected_transactionally() {
    let (_app_dirs, mut app) = app();
    let mut current = crate::config::load_bundled_config();
    current.keymaps.entries.push(crate::config::KeymapEntry {
        id: "user.test.retained-health".into(),
        context: Some("editor".into()),
        mode: "normal".into(),
        keys: Some("z z".into()),
        command: Some("health.open".into()),
        arguments: Default::default(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    });
    app.apply_configuration_candidate(current, false).unwrap();
    assert!(
        app.configuration
            .keymaps()
            .inspect_id("user.test.retained-health")
            .is_some()
    );
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.path = PathBuf::from("keymaps.toml");
    loaded.entries.push(crate::config::KeymapEntry {
        id: "user.test.invalid-user-keymap-set-is-rejected-transactionally".into(),
        context: Some("editor".into()),
        mode: "normal".into(),
        keys: Some("x".into()),
        command: Some("unknown.command".into()),
        arguments: Default::default(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    });
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(keymaps.has_bindings());
    assert!(diagnostic.is_some_and(|message| message.contains("keeping bundled keymaps")));

    let before = app.configuration.keymaps().clone();
    let revision = app.configuration.binding_revision();
    let mut candidate = crate::config::load_bundled_config();
    candidate.keymaps = loaded;
    let diagnostic = app
        .apply_configuration_candidate(candidate, false)
        .unwrap_err();
    assert!(diagnostic.contains("unknown.command"), "{diagnostic}");
    assert_eq!(app.configuration.binding_revision(), revision);
    let reloaded = app.configuration.keymaps();
    assert_eq!(
        reloaded.binding_for("editor.normal", "edit.delete_char"),
        before.binding_for("editor.normal", "edit.delete_char")
    );
    let retained = reloaded
        .inspect_id("user.test.retained-health")
        .expect("user mapping survives rejection");
    assert_eq!(retained.keys, "z z");
    assert_eq!(retained.command.as_deref(), Some("health.open"));
}

#[test]
pub(in crate::app) fn keymap_inspection_by_id_reports_identity_operation_sequences_and_source() {
    let (_app_dirs, mut app) = app();
    let id = "builtin.global.files-explorer.editor-normal";
    app.process_effect(crate::app::commands::AppEffect::InspectKeymap {
        id: Some(id.into()),
        context: None,
        key: None,
        command: None,
    });
    let message = app
        .ui
        .notification
        .clone()
        .expect("inspection notification");
    assert!(message.contains(id));
    assert!(message.contains("<leader> f e => space f e"));
    assert!(message.contains("[command] explorer.panel.toggle"));
    assert!(message.contains("<bundled:keymaps>:declaration"));
}

#[test]
pub(in crate::app) fn keymap_inspection_reports_real_user_path_and_byte_range() {
    let (_app_dirs, mut app) = app();
    let root = tempdir().expect("tempdir");
    let path = root.path().join("keymaps.toml");
    fs::write(
            &path,
            "[[keymap]]\nid='user.test.inspection-path'\ncontext='dashboard'\nkeys='z'\ncommand='health.open'\n",
        )
        .expect("keymap");
    let user = crate::config::load_keymaps(&path);
    let mut bundled = crate::config::load_bundled_config().keymaps;
    let bundled_count = bundled.entries.len();
    bundled.entries.extend(user.entries);
    bundled.bundled_count = bundled_count;
    bundled.path = path.clone();
    bundled.user_spans = user.user_spans;
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &bundled,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none());
    let inspection = keymaps
        .inspect_id("user.test.inspection-path")
        .expect("inspection by stable id");
    assert_eq!(inspection.operation, KeymapOperation::Command);
    assert_eq!(inspection.command.as_deref(), Some("health.open"));
    assert_eq!(inspection.source_keys, "z");
    assert_eq!(inspection.keys, "z");
    assert!(inspection.source.starts_with(&path.display().to_string()));
    assert!(inspection.source.contains(":bytes 0.."));
    app.configuration.install_bindings(keymaps, false);
}

#[test]
pub(in crate::app) fn duplicate_user_ids_are_rejected_transactionally() {
    let (_app_dirs, app) = app();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.path = PathBuf::from("keymaps.toml");
    let mapping = crate::config::KeymapEntry {
        id: "user.test.duplicate-user-mappings-report-conflicts-transactionally".into(),
        context: Some("dashboard".into()),
        mode: String::new(),
        keys: Some("z".into()),
        command: Some("editor.quit".into()),
        arguments: [("force".into(), toml::Value::Boolean(false))]
            .into_iter()
            .collect(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    };
    loaded.entries.extend([mapping.clone(), mapping]);
    let (_, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(
        diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("duplicates id"))
    );
}

#[test]
pub(in crate::app) fn configured_focus_loss_cancels_pending_mapping() {
    let (_app_dirs, mut app) = app();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.entries.push(crate::config::KeymapEntry {
        id: "user.test.configured-focus-loss-cancels-pending-mapping".into(),
        context: Some("dashboard".into()),
        mode: String::new(),
        keys: Some("z z".into()),
        command: Some("health.open".into()),
        arguments: Default::default(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    });
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none());
    app.configuration.install_bindings(keymaps, false);
    app.handle_configured_key(KeyEvent::character('z'));
    assert!(app.input.mapping_pending());
    app.handle_event(crate::ui::UiEvent::FocusLost);
    assert!(!app.input.mapping_pending());
}

#[test]
pub(in crate::app) fn prompt_special_keys_are_remappable_from_configuration() {
    let (_app_dirs, mut app) = app();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.entries.extend([
        crate::config::KeymapEntry {
            id: "user.test.prompt-special-keys-are-remappable-from-configuration".into(),
            context: Some("command_prompt".into()),
            mode: String::new(),
            keys: Some("escape".into()),
            command: None,
            arguments: Default::default(),
            unmap: true,
            disable: false,
            pass_through: false,
            desc: None,
        },
        crate::config::KeymapEntry {
            id: "user.test.prompt-special-keys-are-remappable-from-configuration.case-2".into(),
            context: Some("command_prompt".into()),
            mode: String::new(),
            keys: Some("ctrl-c".into()),
            command: Some("command_line.cancel".into()),
            arguments: Default::default(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
        },
    ]);
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none());
    app.configuration.install_bindings(keymaps, false);
    app.open_prompt(crate::ui::PromptKind::Command, String::new());
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    assert!(app.ui.prompt.is_some());
    app.handle_configured_key("ctrl-c".parse().expect("configured key"));
    assert!(app.ui.prompt.is_none());
}

#[test]
pub(in crate::app) fn confirmation_keys_and_hints_follow_accepted_remaps() {
    let (_app_dirs, mut app) = app();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.entries.extend([
        crate::config::KeymapEntry {
            id: "user.test.confirmation.cancel".into(),
            context: Some("confirmation_prompt".into()),
            mode: String::new(),
            keys: Some("ctrl-c".into()),
            command: Some("command_line.cancel".into()),
            arguments: Default::default(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
        },
        crate::config::KeymapEntry {
            id: "user.test.confirmation.yes".into(),
            context: Some("confirmation_prompt".into()),
            mode: String::new(),
            keys: Some("z".into()),
            command: Some("surface.confirmation.select_yes".into()),
            arguments: Default::default(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
        },
    ]);
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none());
    app.configuration.install_bindings(keymaps, false);
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let confirm = |app: &mut App, token| {
        app.process_effect(AppEffect::PluginExecute {
            plugin: "demo".into(),
            command: "run".into(),
            arguments: Vec::new(),
        });
        let invocation = fixture.take();
        fixture.reply_confirmation_request(
            &invocation,
            crate::plugins::PluginConfirmationRequest {
                token,
                title: "Delete?".into(),
                message: "target".into(),
                default_answer: crate::plugins::PluginConfirmationAnswer::No,
            },
        );
        app.process_plugin_events();
    };
    confirm(&mut app, 7);
    assert!(
        app.input
            .prompt()
            .unwrap()
            .confirmation
            .as_ref()
            .unwrap()
            .hint
            .contains("ctrl-c")
    );
    app.handle_configured_key("ctrl-c".parse().unwrap());
    assert!(
        app.input.prompt().is_none(),
        "the remapped cancel key closes it"
    );
    app.process_plugin_events();
    let cancelled = fixture.take();
    assert_eq!(
        cancelled.confirmation_results(),
        [crate::plugins::PluginConfirmationResult {
            request: 7,
            outcome: crate::plugins::PluginConfirmationOutcome::Answered(
                crate::plugins::PluginConfirmationAnswer::Cancelled
            ),
        }]
    );
    fixture.reply(&cancelled, Vec::new());
    app.process_plugin_events();
    confirm(&mut app, 8);
    app.handle_configured_key(KeyEvent::character('z'));
    assert!(
        app.input.prompt().is_none(),
        "the remapped yes key answers at once"
    );
    let yes = fixture.take();
    assert_eq!(
        yes.confirmation_results(),
        [crate::plugins::PluginConfirmationResult {
            request: 8,
            outcome: crate::plugins::PluginConfirmationOutcome::Answered(
                crate::plugins::PluginConfirmationAnswer::Yes
            ),
        }]
    );
    app.submit_prompt();
    fixture.reply(&yes, Vec::new());
    app.process_plugin_events();
    assert_eq!(app.extension.confirmation_count(), 0);
    assert_eq!(app.extension.fixture_queued(), 0);
}

/// Compile the bundled keymaps exactly as application construction does.
pub(in crate::app) fn bundled_keymaps(app: &App) -> crate::app::configuration::ResolvedKeymaps {
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &crate::config::load_bundled_config().keymaps,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    keymaps
}

/// Drive a documented key sequence through the same resolution path the dispatcher uses.
///
/// `<leader>` expands to the bundled leader and every key is fed as a real key event, so this
/// proves what a keypress does rather than comparing strings in a configuration file.
pub(in crate::app) fn documented_key_reaches(
    keymaps: &crate::app::configuration::ResolvedKeymaps,
    keys: &str,
) -> Option<crate::editor::CommandInvocation> {
    let mut sequence = crate::editor::input::KeymapSequence::default();
    for token in keys.replace("<leader>", "space").split_whitespace() {
        let character = match token {
            "space" => ' ',
            other => other.chars().next().expect("single-character key token"),
        };
        match keymaps.resolve(
            &mut sequence,
            "editor.normal",
            KeyEvent::character(character),
        ) {
            MappingResolution::Pending => {}
            MappingResolution::Ready(invocation)
            | MappingResolution::ReadyAndReplay(invocation, _) => {
                return Some(invocation);
            }
            _ => return None,
        }
    }
    None
}

#[test]
pub(in crate::app) fn the_agent_keys_each_reach_their_command() {
    let (_app_dirs, app) = app();
    let keymaps = bundled_keymaps(&app);
    for (keys, command) in [
        ("<leader> a a", "agent.toggle"),
        ("<leader> a n", "agent.new"),
        ("<leader> a s", "agent.sessions"),
        ("<leader> a ]", "agent.session.next"),
        ("<leader> a [", "agent.session.previous"),
        ("<leader> a x", "agent.session.close"),
        ("<leader> a R", "agent.resume"),
        ("<leader> a m", "agent.model"),
        ("<leader> a M", "agent.mode"),
        ("<leader> a r", "agent.thinking"),
        ("<leader> a o", "agent.options"),
    ] {
        assert_eq!(
            documented_key_reaches(&keymaps, keys)
                .map(|invocation| invocation.id.as_str().to_owned())
                .as_deref(),
            Some(command),
            "{keys}"
        );
    }
}

/// E-09: the document is the source of truth for which keys exist, so a key added to its
/// prefix table without a binding fails here instead of shipping as a dead prefix entry, and
/// a key it marks as having no command must not be bound to something else.
#[test]
pub(in crate::app) fn the_documented_git_and_debugger_keys_each_reach_their_command() {
    let (_app_dirs, app) = app();
    let keymaps = bundled_keymaps(&app);
    let cases = [
        ("<leader> g g", "terminal.open"),
        ("<leader> g s", "workspace.git.status.open"),
        ("<leader> g G", "terminal.open"),
        ("<leader> g c", "terminal.open"),
        ("<leader> g f", "terminal.open"),
        ("<leader> g l", "terminal.open"),
        ("<leader> g x", "workspace.git.conflicts.open"),
        ("<leader> g w", "workspace.git.worktrees.open"),
        ("<leader> g h p", "workspace.git.hunk.preview"),
        ("<leader> d s", "dap.session.start"),
        ("<leader> d c", "dap.session.continue"),
        ("<leader> d O", "dap.session.next"),
        ("<leader> d i", "dap.session.step_in"),
        ("<leader> d o", "dap.session.step_out"),
        ("<leader> d p", "dap.session.pause"),
        ("<leader> d t", "dap.session.stop"),
        ("<leader> d R", "dap.session.restart"),
        ("<leader> d b", "dap.breakpoint.toggle"),
        ("<leader> d B", "dap.breakpoint.configure"),
        ("<leader> d l", "dap.breakpoints.states"),
        ("<leader> d D", "dap.detach"),
        ("<leader> d r", "dap.console"),
    ];
    for (keys, command) in cases {
        assert_eq!(
            documented_key_reaches(&keymaps, keys)
                .map(|invocation| invocation.id.as_str().to_owned())
                .as_deref(),
            Some(command),
            "{keys} must reach {command} through the real input path"
        );
    }
    for (keys, profile) in [
        ("<leader> g g", "lazygit"),
        ("<leader> g G", "lazygit_cwd"),
        ("<leader> g c", "lazygit_log"),
        ("<leader> g f", "lazygit_file"),
        ("<leader> g l", "lazygit_log"),
    ] {
        let invocation = documented_key_reaches(&keymaps, keys).unwrap();
        assert_eq!(
            invocation.arguments.get("profile"),
            Some(&CommandValue::String(profile.into())),
            "{keys}"
        );
    }
    // `Space d w` names `dap.watch.add`, which requires an expression the user must type. No
    // keymap entry can supply it and no prompting command exists, so the document records it
    // as `no prompt command`. Binding it to something else would be a lie, and binding it to
    // this command fails application construction outright.
    assert_eq!(
        documented_key_reaches(&keymaps, "<leader> d w"),
        None,
        "`Space d w` cannot reach an argument-requiring command from a bare key"
    );
}

#[test]
pub(in crate::app) fn documented_git_and_debugger_keys_match_the_keymap() {
    let (_app_dirs, app) = app();
    let keymaps = bundled_keymaps(&app);
    let document = fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/user-guide-linux.md"
    ))
    .expect("the user guide is part of the checkout");
    // Check the explicit binding status, not other tables describing the same shortcuts.
    let table = document
        .split("#### The Git and debugger prefixes")
        .nth(1)
        .and_then(|rest| rest.split("\nThe debug console is opened with").next())
        .expect("the document's Git and debugger prefix table");

    let mut checked = 0usize;
    for line in table.lines() {
        let Some(first_cell) = line.strip_prefix("| `") else {
            continue;
        };
        let Some((named, _)) = first_cell.split_once("` |") else {
            continue;
        };
        if !named.starts_with("Space g ") && !named.starts_with("Space d ") {
            continue;
        }
        let keys = named.replacen("Space ", "<leader> ", 1);
        let status = line.rsplit('|').nth(1).unwrap_or_default().trim();
        checked += 1;
        let reached = documented_key_reaches(&keymaps, &keys);
        // The document's own status cell is the claim, and both directions are checked: a row
        // that says `bound` must resolve, and a row that says anything else must not.
        if status.contains("bound") {
            assert!(
                reached.is_some(),
                "{keys} is marked `bound` by the document but reaches nothing; \
                 the keymap must bind it or the document must record why it cannot be"
            );
        } else {
            assert_eq!(
                reached, None,
                "{keys} is marked `{status}` by the document but resolves to {reached:?}"
            );
        }
    }
    assert!(
        checked >= 25,
        "the document's prefix table should name at least 25 keys, found {checked}"
    );
}

#[test]
pub(in crate::app) fn the_pi_profile_adds_dust_tools_to_pi() {
    let (_app_dirs, app) = app();
    let agents = &app.configuration.settings().agents;
    assert!(agents.add_tools);
    assert_eq!(
        agents.profiles["pi"].add_tools.join(" "),
        "pi mcp add dust --exposure direct -- {dust} --agent-tools"
    );
    assert!(
        agents.profiles["claude"].add_tools.is_empty(),
        "Dust gives other agents the tool itself"
    );
}

#[test]
pub(in crate::app) fn agents_own_subagents_are_off_over_acp_only() {
    let (_app_dirs, app) = app();
    let profiles = &app.configuration.settings().agents.profiles;
    assert_eq!(profiles["codex"].cli, ["codex"]);
    let codex: serde_json::Value =
        serde_json::from_str(&profiles["codex"].acp_environment["CODEX_CONFIG"]).unwrap();
    assert_eq!(codex["features"]["multi_agent"], false);
    assert_eq!(profiles["opencode"].cli, ["opencode"]);
    let opencode: serde_json::Value =
        serde_json::from_str(&profiles["opencode"].acp_environment["OPENCODE_CONFIG_CONTENT"])
            .unwrap();
    assert_eq!(opencode["tools"]["task"], false);
    assert_eq!(
        profiles["claude"].meta.as_ref().unwrap()["claudeCode"]["options"]["disallowedTools"],
        serde_json::json!(["Task", "Agent", "Workflow"])
    );
    for profile in profiles.values() {
        assert!(!profile.environment.contains_key("CODEX_CONFIG"));
        assert!(!profile.environment.contains_key("OPENCODE_CONFIG_CONTENT"));
    }
}

#[test]
pub(in crate::app) fn the_agent_panel_opens_over_the_dashboard() {
    let (_app_dirs, mut app) = app();
    assert!(app.window_session.dashboard_visible());
    for key in [' ', 'a', 't'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert!(!app.window_session.dashboard_visible());
    assert!(app.state.active_view().is_some());
    // Keys now go to the panel's input, not to the placeholder buffer.
    assert_eq!(app.active_input_context(), "agent");
    for key in "hi ab".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Backspace));
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Left));
    app.handle_configured_key(KeyEvent::character('x'));
    let panel = app.ui.agent_panel.as_ref().expect("the panel is drawn");
    assert_eq!((panel.input.as_str(), panel.cursor), ("hi xa", 4));
    assert!(app.ui.content_tabs.iter().all(|tab| tab.title != "Agent"));
}

#[test]
pub(in crate::app) fn asking_an_agent_whose_adapter_is_missing_says_how_to_install_it() {
    // Isolate discovery without changing the environment of parallel tests.
    if std::env::var_os("DUST_TEST_MISSING_ADAPTER").is_none() {
        let root = tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "app::configuration::tests::asking_an_agent_whose_adapter_is_missing_says_how_to_install_it", "--nocapture"])
            .env("DUST_TEST_MISSING_ADAPTER", "1")
            .env("DUST_STATE_HOME", root.path().join("state"))
            .env("DUST_CONFIG_HOME", root.path().join("config"))
            .env("PATH", "")
            .output().unwrap();
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
            "child must run the missing-adapter test: {output:?}"
        );
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (_app_dirs, mut app) = app();
    let mut fixture = app.configuration.fixture_snapshot();
    let agents = &mut fixture.settings.agents;
    agents.connection = crate::config::AgentConnection::Acp;
    agents.default = "claude".into();
    agents.profiles.get_mut("claude").unwrap().program = "claude-agent-acp".into();
    app.configuration.install_fixture(fixture);
    for key in [' ', 'a', 't'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    for key in "hi".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Enter));
    // The panel says so for good, not just in the notice, and keeps the question.
    let panel = app.ui.agent_panel.as_ref().expect("the panel is drawn");
    assert!(
        panel
            .transcript
            .text
            .split_whitespace().collect::<Vec<_>>().join(" ")
            .contains("claude-agent-acp is not installed; install it in the tool manager (<leader>cm, Agent)")
    );
    assert_eq!(
        (
            panel.input.as_str(),
            app.ui.notification.as_deref(),
            app.ui.notification_error
        ),
        (
            "hi",
            Some(
                "claude-agent-acp is not installed; install it in the tool manager (<leader>cm, Agent)"
            ),
            true
        )
    );
    app.handle_configured_key(KeyEvent::character('!'));
    assert_eq!(
        app.ui
            .agent_panel
            .as_ref()
            .map(|panel| panel.input.as_str()),
        Some("hi!")
    );
}

#[test]
pub(in crate::app) fn colorscheme_with_a_name_applies_that_theme_and_refuses_an_unknown_one() {
    let (_app_dirs, mut app) = app();
    let root = tempdir().unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.path = root.path().join("config.toml");
    app.apply_configuration_candidate(loaded, false).unwrap();
    let set = |name: &str, app: &mut App| {
        app.process_effect(AppEffect::SetTheme(name.into()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.ui.picker.is_some() {
            assert!(Instant::now() < deadline, "theme list did not load");
            app.process_theme_results();
            thread::sleep(Duration::from_millis(2));
        }
    };
    set("Catppuccin Latte", &mut app);
    assert_eq!(app.configuration.theme().name, "catppuccin-latte");
    set("rose pine", &mut app);
    assert_eq!(app.configuration.theme().name, "rose-pine");
    set("nope", &mut app);
    assert!(app.ui.notification_error);
    assert_eq!(app.configuration.theme().name, "rose-pine");
}

#[test]
pub(in crate::app) fn the_settings_page_searches_edits_and_closes_layer_by_layer() {
    use crate::app::commands::SettingsAction;
    use crate::ui::SettingsEdit;
    use crate::ui::SettingsValue;
    let (_app_dirs, mut app) = app();
    app.process_effect(AppEffect::Settings(SettingsAction::Open));
    assert_eq!(app.active_input_context(), "settings");
    let page = app.ui.settings.as_ref().expect("settings page");
    assert_eq!(page.selected_row().unwrap().key, "editor.wrap");

    app.process_effect(AppEffect::Settings(SettingsAction::Search));
    assert_eq!(app.active_input_context(), "settings_search");
    app.process_effect(AppEffect::Settings(SettingsAction::Insert(
        "tab width".into(),
    )));
    let page = app.ui.settings.as_ref().unwrap();
    assert_eq!(page.selected_row().unwrap().key, "editor.tab_width");
    assert_eq!(
        page.selected_row().unwrap().value,
        SettingsValue::Number("4".into())
    );

    // Enter leaves the search box; Enter again edits the number.
    app.process_effect(AppEffect::Settings(SettingsAction::Activate));
    app.process_effect(AppEffect::Settings(SettingsAction::Activate));
    assert_eq!(app.active_input_context(), "settings_edit");
    app.process_effect(AppEffect::Settings(SettingsAction::Clear));
    app.process_effect(AppEffect::Settings(SettingsAction::Insert("x".into())));
    app.process_effect(AppEffect::Settings(SettingsAction::Activate));
    assert!(matches!(
        &app.ui.settings.as_ref().unwrap().edit,
        Some(SettingsEdit::Text { error: Some(error), .. }) if error == "Enter a whole number"
    ));
    // The test runs on the bundled defaults alone, so a valid value is refused, not written,
    // and the draft stays to fix or cancel.
    app.process_effect(AppEffect::Settings(SettingsAction::Clear));
    app.process_effect(AppEffect::Settings(SettingsAction::Insert("2".into())));
    app.process_effect(AppEffect::Settings(SettingsAction::Activate));
    assert!(matches!(
        &app.ui.settings.as_ref().unwrap().edit,
        Some(SettingsEdit::Text { text, error: Some(error), .. })
            if text == "2" && error.contains("user configuration")
    ));

    for context in ["settings_edit", "settings", "settings"] {
        assert_eq!(app.active_input_context(), context);
        app.process_effect(AppEffect::Settings(SettingsAction::Cancel));
    }
    assert!(app.ui.settings.is_none());
}

#[test]
pub(in crate::app) fn the_settings_page_saves_and_reloads_user_configuration() {
    use crate::app::commands::SettingsAction;
    use crate::ui::SettingsValue;
    // The worker resolves the user configuration from the environment, so use a child
    // process rather than changing those paths underneath other tests.
    if std::env::var_os("DUST_TEST_SETTINGS_SAVE").is_none() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("config.toml"),
            "# keep this comment\n[editor]\nwrap = false\n",
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app::configuration::tests::the_settings_page_saves_and_reloads_user_configuration",
                "--nocapture",
            ])
            .env("DUST_TEST_SETTINGS_SAVE", "1")
            .env("DUST_CONFIG_HOME", root.path())
            .env("DUST_STATE_HOME", root.path().join("state"))
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
            "child must run the settings-save test: {output:?}"
        );
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = tempdir().unwrap();
    let saved = tempdir().unwrap();
    let mut app = App::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        crate::config::load_user_config(),
        false,
        saved.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((80, 24));
    assert_eq!(app.configuration.settings().editor.tab_width, 4);
    app.process_effect(AppEffect::Settings(SettingsAction::Open));
    app.process_effect(AppEffect::Settings(SettingsAction::Search));
    app.process_effect(AppEffect::Settings(SettingsAction::Insert(
        "tab width".into(),
    )));
    assert_eq!(
        app.ui
            .settings
            .as_ref()
            .unwrap()
            .selected_row()
            .unwrap()
            .key,
        "editor.tab_width"
    );
    app.process_effect(AppEffect::Settings(SettingsAction::Activate));
    app.process_effect(AppEffect::Settings(SettingsAction::Activate));
    app.process_effect(AppEffect::Settings(SettingsAction::Clear));
    app.process_effect(AppEffect::Settings(SettingsAction::Insert("2".into())));
    let reload_id = app.configuration.reload_id();
    app.process_effect(AppEffect::Settings(SettingsAction::Activate));
    let page = app.ui.settings.as_ref().unwrap();
    assert!(page.edit.is_none());
    assert_eq!(page.status, Some(("Saved".into(), false)));
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.configuration.reload_id() == reload_id {
        app.process_configuration_result();
        assert!(Instant::now() < deadline, "saved setting was not reloaded");
        thread::sleep(Duration::from_millis(1));
    }
    let source = fs::read_to_string(crate::config::user_config_path()).unwrap();
    assert!(source.contains("# keep this comment"));
    let disk: toml::Value = toml::from_str(&source).unwrap();
    assert_eq!(disk["editor"]["tab_width"].as_integer(), Some(2));
    assert_eq!(disk["editor"]["wrap"].as_bool(), Some(false));
    assert_eq!(app.configuration.settings().editor.tab_width, 2);
    assert!(!app.configuration.settings().editor.wrap);
    let row = app.ui.settings.as_ref().unwrap().selected_row().unwrap();
    assert_eq!(row.key, "editor.tab_width");
    assert_eq!(row.value, SettingsValue::Number("2".into()));
    assert!(row.modified);
}

#[test]
pub(in crate::app) fn the_first_start_setup_saves_the_picked_choices_to_a_new_config() {
    use crate::app::commands::SetupAction;
    // As above, a child process so the user configuration paths can point at an empty folder.
    if std::env::var_os("DUST_TEST_FIRST_START").is_none() {
        let root = tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "app::configuration::tests::the_first_start_setup_saves_the_picked_choices_to_a_new_config", "--nocapture"])
            .env("DUST_TEST_FIRST_START", "1")
            .env("DUST_CONFIG_HOME", root.path())
            .env("DUST_STATE_HOME", root.path().join("state"))
            .output().unwrap();
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
            "child must run the first-start test: {output:?}"
        );
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = tempdir().unwrap();
    let saved = tempdir().unwrap();
    let mut app = App::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        crate::config::load_user_config(),
        false,
        saved.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((120, 40));
    let path = crate::config::user_config_path();
    assert!(!path.exists());
    app.open_setup_on_first_start();
    assert_eq!(app.active_input_context(), "setup");
    // The first page is the layout; its second choice puts the explorer right, agent left.
    app.process_effect(AppEffect::Setup(SetupAction::Move(1)));
    app.process_effect(AppEffect::Setup(SetupAction::Next));
    app.process_effect(AppEffect::Setup(SetupAction::Back));
    let setup = app.ui.settings.as_ref().unwrap().setup.as_ref().unwrap();
    assert_eq!((setup.page, setup.picked[0]), (0, 1));
    let pages = setup.pages.len();
    for _ in 0..pages {
        app.process_effect(AppEffect::Setup(SetupAction::Next));
    }
    assert!(app.ui.settings.is_none(), "setup should close once saved");
    let source = fs::read_to_string(&path).unwrap();
    let disk: toml::Value = toml::from_str(&source).unwrap();
    assert_eq!(disk["ui"]["explorer"]["side"].as_str(), Some("right"));
    assert_eq!(disk["agents"]["side"].as_str(), Some("left"));
    // Choices that match the defaults are left out of the file.
    assert!(disk["ui"].get("terminal_panel").is_none(), "{source}");
    let reload_id = app.configuration.reload_id();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.configuration.reload_id() == reload_id {
        app.process_configuration_result();
        assert!(Instant::now() < deadline, "setup was not reloaded");
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        app.configuration.settings().agents.side,
        crate::config::PanelSide::Left
    );
    // Once config.toml exists the questions aren't asked on start.
    app.open_setup_on_first_start();
    assert!(app.ui.settings.is_none());
    // `:setup` asks again, starting from the saved answers, and Skip leaves the file alone.
    app.process_effect(AppEffect::Setup(SetupAction::Open));
    let setup = app.ui.settings.as_ref().unwrap().setup.as_ref().unwrap();
    assert_eq!(setup.picked[0], 1);
    app.process_effect(AppEffect::Setup(SetupAction::Skip));
    assert!(app.ui.settings.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), source);
}

#[test]
pub(in crate::app) fn the_shortcuts_page_records_new_keys_and_resets_them() {
    use crate::app::commands::SettingsAction;
    use crate::ui::SettingsValue;
    // As above, a child process so the user configuration paths can point at a temporary folder.
    if std::env::var_os("DUST_TEST_SHORTCUTS").is_none() {
        let root = tempdir().unwrap();
        fs::write(root.path().join("config.toml"), "").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app::configuration::tests::the_shortcuts_page_records_new_keys_and_resets_them",
                "--nocapture",
            ])
            .env("DUST_TEST_SHORTCUTS", "1")
            .env("DUST_CONFIG_HOME", root.path())
            .env("DUST_STATE_HOME", root.path().join("state"))
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
            "child must run the shortcuts test: {output:?}"
        );
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = tempdir().unwrap();
    let saved = tempdir().unwrap();
    let mut app = App::new(
        StartupTarget::Dashboard {
            root: root.path().into(),
        },
        crate::config::load_user_config(),
        false,
        saved.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((120, 40));
    let reloaded = |app: &mut App, reload_id| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.configuration.reload_id() == reload_id {
            app.process_configuration_result();
            assert!(Instant::now() < deadline, "keymaps were not reloaded");
            thread::sleep(Duration::from_millis(1));
        }
    };
    let press_keys = |app: &mut App, keys: &[&str]| {
        for key in keys {
            app.handle_event(UiEvent::Key {
                key: key.parse().unwrap(),
                kind: crate::ui::KeyInputKind::Press,
            });
        }
    };
    let selected = |app: &App| {
        app.ui
            .settings
            .as_ref()
            .unwrap()
            .selected_row()
            .unwrap()
            .clone()
    };
    app.process_effect(AppEffect::Settings(SettingsAction::Shortcuts));
    // The leader keys come first, and each section's bindings sit together under one heading.
    let page = app.ui.settings.as_ref().unwrap();
    assert_eq!(page.selected_row().unwrap().key, "input.keymaps.leader");
    assert_eq!(page.sections[1], "Everywhere");
    let mut headings: Vec<_> = page
        .lines
        .iter()
        .filter(|line| matches!(line, crate::ui::SettingsLine::Heading(_)))
        .collect();
    let count = headings.len();
    headings.dedup();
    assert_eq!(headings.len(), count, "a section is split");
    app.process_effect(AppEffect::Settings(SettingsAction::Search));
    app.process_effect(AppEffect::Settings(SettingsAction::Insert(
        "search-keymaps".into(),
    )));
    app.process_effect(AppEffect::Settings(SettingsAction::Activate));
    let row = selected(&app);
    assert_eq!(row.key, "builtin.global.search-keymaps");
    assert_eq!(row.value, SettingsValue::Keys("<leader> s k".into()));
    assert!(!row.modified);
    // Escape leaves the recording without saving; Backspace takes back a key.
    app.process_effect(AppEffect::Settings(SettingsAction::Activate));
    press_keys(&mut app, &["x", "escape"]);
    assert!(app.ui.settings.as_ref().unwrap().edit.is_none());
    let path = crate::config::user_keymaps_path();
    assert!(!path.exists());
    app.process_effect(AppEffect::Settings(SettingsAction::Activate));
    let reload_id = app.configuration.reload_id();
    press_keys(&mut app, &["space", "s", "x", "backspace", "K", "enter"]);
    let page = app.ui.settings.as_ref().unwrap();
    assert_eq!(page.status, Some(("Saved".into(), false)));
    let source = fs::read_to_string(&path).unwrap();
    let disk: toml::Value = toml::from_str(&source).unwrap();
    let entry = &disk["keymap"][0];
    assert_eq!(entry["id"].as_str(), Some("builtin.global.search-keymaps"));
    assert_eq!(entry["keys"].as_str(), Some("<leader> s K"));
    assert_eq!(entry["command"].as_str(), Some("settings.shortcuts"));
    reloaded(&mut app, reload_id);
    let row = selected(&app);
    assert_eq!(row.value, SettingsValue::Keys("<leader> s K".into()));
    assert!(row.modified);
    let reload_id = app.configuration.reload_id();
    app.process_effect(AppEffect::Settings(SettingsAction::Reset));
    reloaded(&mut app, reload_id);
    let row = selected(&app);
    assert_eq!(row.value, SettingsValue::Keys("<leader> s k".into()));
    assert!(!row.modified);
    assert!(
        !fs::read_to_string(&path)
            .unwrap()
            .contains("search-keymaps")
    );
}

#[test]
pub(in crate::app) fn undo_persistence_policy_changes_with_configuration() {
    let (_app_dirs, mut app) = app();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.persistence.enabled = true;
    loaded.config.persistence.undo_history = true;
    app.apply_configuration_candidate(loaded.clone(), false)
        .unwrap();
    assert!(app.state.undo_directory.as_ref().unwrap().ends_with("undo"));
    loaded.config.persistence.undo_history = false;
    app.apply_configuration_candidate(loaded.clone(), false)
        .unwrap();
    assert!(app.state.undo_directory.is_none());
    loaded.config.persistence.undo_history = true;
    app.apply_configuration_candidate(loaded, false).unwrap();
    assert!(app.state.undo_directory.is_some());
}

#[test]
pub(in crate::app) fn settings_refresh_cancels_an_edit_when_the_selected_key_disappears() {
    let (_app_dirs, mut app) = app();
    app.open_settings();
    let page = app.ui.settings.as_mut().unwrap();
    page.query = "@modified".into();
    page.edit = Some(crate::ui::SettingsEdit::Text {
        text: "8".into(),
        cursor: 1,
        error: None,
    });
    app.refresh_settings();
    assert!(app.ui.settings.as_ref().unwrap().edit.is_none());
}

#[test]
pub(in crate::app) fn reload_preserves_zen_exit_and_plugin_command_titles() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("a"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_explorer();
    app.process_effect(AppEffect::Zen);
    assert!(app.ui.explorer.is_none());
    let mut spec = crate::editor::CommandSpec::builtin("plugin.demo.run", "Run demo").unwrap();
    spec.source = crate::editor::CommandSource::Plugin("demo".into());
    app.commands
        .register(spec, |_, _| Ok(crate::editor::CommandOutcome::empty()))
        .unwrap();
    app.apply_configuration_candidate(crate::config::load_bundled_config(), false)
        .unwrap();
    app.process_effect(AppEffect::Zen);
    assert!(app.ui.explorer.is_some());
    let command = app
        .commands
        .specs()
        .find(|spec| spec.id.as_str() == "plugin.demo.run")
        .expect("plugin command survives reload");
    assert_eq!(command.title, "Run demo");
    app.open_builtin_picker(PickerKind::Commands);
    let row = app
        .ui
        .picker
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .find(|row| row.detail == "plugin.demo.run")
        .expect("plugin command in palette");
    assert_eq!(row.label, "Plugins: Run demo");
}

#[test]
pub(in crate::app) fn plugin_alias_reload_requires_a_safe_label_before_command_completion() {
    let (_app_dirs, mut app) = app();
    let mut spec = crate::editor::CommandSpec::builtin("plugin.demo.run", "Run demo").unwrap();
    spec.source = crate::editor::CommandSource::Plugin("demo".into());
    app.commands
        .register(spec, |_, _| Ok(crate::editor::CommandOutcome::empty()))
        .unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded
        .config
        .input
        .command_aliases
        .insert("Demo".into(), "plugin.demo.run".into());
    for label in [None, Some(""), Some("\u{1b}bad")] {
        if let Some(label) = label {
            loaded
                .config
                .command_labels
                .insert("plugin.demo.run".into(), label.into());
        }
        let error = app
            .apply_configuration_candidate(loaded.clone(), false)
            .unwrap_err();
        assert!(error.contains("label"), "{error}");
    }
    loaded
        .config
        .command_labels
        .insert("plugin.demo.run".into(), "Run demo".into());
    app.apply_configuration_candidate(loaded, false).unwrap();
    app.open_prompt(crate::ui::PromptKind::Command, "Demo".into());
    assert!(
        app.ui
            .prompt
            .as_ref()
            .unwrap()
            .completions
            .iter()
            .any(|item| item.label == "Demo" && item.detail == "Run demo")
    );
}

#[test]
pub(in crate::app) fn plugin_aliases_and_dashboard_actions_wait_for_indexed_declarations() {
    let (_app_dirs, mut app) = app();
    let mut manifest = crate::app::plugins::fixture_manifest("0".repeat(64));
    manifest
        .commands
        .get_mut("drive")
        .unwrap()
        .arguments
        .insert(
            "text".into(),
            crate::plugins::PluginArgumentDeclaration {
                kind: crate::plugins::PluginArgumentKind::String,
                required: true,
            },
        );
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 1, "fixture".into()).unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.installed_plugins.insert("demo".into());
    loaded
        .config
        .command_labels
        .insert("plugin.demo.drive".into(), "Drive demo".into());
    loaded
        .config
        .input
        .command_aliases
        .insert("Demo".into(), "plugin.demo.drive".into());
    loaded.dashboard.action[0].command = "plugin.demo.drive".into();
    loaded.dashboard.action[0].arguments =
        BTreeMap::from([("text".into(), toml::Value::String("hello".into()))]);
    let (resolved, diagnostic) =
        super::resolve_configuration(&app.commands, loaded.clone(), true).unwrap();
    assert!(diagnostic.is_none());
    assert!(resolved.deferred_plugin_keymaps);
    let mut invalid_arguments = loaded.clone();
    invalid_arguments.dashboard.action[0]
        .arguments
        .insert("text".into(), toml::Value::Boolean(true));
    let (invalid_arguments, _) =
        super::resolve_configuration(&app.commands, invalid_arguments, true).unwrap();
    let mut undeclared_alias = loaded.clone();
    undeclared_alias
        .config
        .input
        .command_aliases
        .insert("Unknown".into(), "plugin.demo.missing".into());
    undeclared_alias
        .config
        .command_labels
        .insert("plugin.demo.missing".into(), "Missing".into());
    let (undeclared_alias, _) =
        super::resolve_configuration(&app.commands, undeclared_alias, true).unwrap();
    app.configuration.install_fixture(resolved.clone());
    assert!(
        app.recompile_plugin_contributions().is_err(),
        "unindexed declarations cannot be accepted"
    );
    app.register_indexed_plugin_commands(&index).unwrap();
    app.extension.fixture_contribution(index).unwrap();
    app.recompile_plugin_contributions().unwrap();
    for invalid in [invalid_arguments, undeclared_alias] {
        app.configuration.install_fixture(invalid);
        assert!(app.recompile_plugin_contributions().is_err());
    }
    app.configuration.install_fixture(resolved);
    let mut invalid = loaded.clone();
    invalid.dashboard.action[0]
        .arguments
        .insert("text".into(), toml::Value::Boolean(true));
    assert!(app.apply_configuration_candidate(invalid, false).is_err());
    let mut missing = loaded.clone();
    missing
        .config
        .input
        .command_aliases
        .insert("Unknown".into(), "plugin.demo.missing".into());
    missing
        .config
        .command_labels
        .insert("plugin.demo.missing".into(), "Missing".into());
    assert!(app.apply_configuration_candidate(missing, false).is_err());
    loaded.dashboard.action[0].arguments.clear();
    assert!(
        app.apply_configuration_candidate(loaded, false).is_err(),
        "required arguments are checked after indexing"
    );
}

#[test]
pub(in crate::app) fn disabled_plugin_command_references_are_not_deferred() {
    let (_app_dirs, app) = app();
    let mut loaded = crate::config::load_bundled_config();
    loaded
        .config
        .input
        .command_aliases
        .insert("Demo".into(), "plugin.demo.run".into());
    loaded
        .config
        .command_labels
        .insert("plugin.demo.run".into(), "Run demo".into());
    assert!(super::resolve_configuration(&app.commands, loaded, true).is_err());
}

#[test]
pub(in crate::app) fn direct_motion_binding_preserves_an_explicit_count_of_one() {
    let (_app_dirs, mut app) = app();
    let mut loaded = crate::config::load_bundled_config();
    loaded.keymaps.entries.push(
        toml::from_str(
            r#"
        id = "user.direct-end"
        context = "editor"
        mode = "normal"
        keys = "Z"
        command = "view.cursor.move"
        arguments = { motion = "document_end" }
    "#,
        )
        .unwrap(),
    );
    app.apply_configuration_candidate(loaded, false).unwrap();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("first\nsecond\nthird"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.handle_configured_key(KeyEvent::character('Z'));
    assert_eq!(app.state.views[view].cursor, ByteOffset(13));
    for key in ['1', 'Z'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert_eq!(app.state.views[view].cursor, ByteOffset(0));
}

#[test]
pub(in crate::app) fn user_groups_with_deferred_plugin_continuations_wait_for_the_plugin() {
    let (_app_dirs, mut app) = app();
    let mut loaded = crate::config::load_bundled_config();
    loaded.installed_plugins.insert("demo".into());
    loaded.keymaps.entries.push(
        toml::from_str(
            r#"
        id = "user.demo"
        context = "editor"
        mode = "normal"
        keys = "<leader> j x"
        command = "plugin.demo.run"
    "#,
        )
        .unwrap(),
    );
    let group: crate::config::KeyGroupEntry = toml::from_str(
        r#"
        id = "user.group"
        context = "editor"
        mode = "normal"
        prefix = "space j"
        label = "Demo"
    "#,
    )
    .unwrap();
    loaded.keymaps.groups.push(group.clone());
    let (resolved, diagnostic) =
        super::resolve_configuration(&app.commands, loaded.clone(), true).unwrap();
    assert!(diagnostic.is_none());
    assert!(resolved.deferred_plugin_keymaps);
    assert!(
        resolved
            .base_keymaps
            .groups
            .iter()
            .all(|group| group.id != "user.group")
    );
    assert!(
        resolved
            .raw_keymaps
            .groups
            .iter()
            .any(|group| group.id == "user.group")
    );

    let mut spec = crate::editor::CommandSpec::builtin("plugin.demo.run", "Run demo").unwrap();
    spec.source = crate::editor::CommandSource::Plugin("demo".into());
    app.commands
        .register(spec, |_, _| Ok(crate::editor::CommandOutcome::empty()))
        .unwrap();
    let (compiled, diagnostic) = compile_keymaps(
        &app.commands,
        &resolved.raw_keymaps,
        &resolved.settings.input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    assert!(compiled.groups.iter().any(|group| group.id == "user.group"));
    assert!(compiled.inspect_id("user.demo").is_some());

    let mut unreachable = group;
    unreachable.id = "user.unreachable".into();
    unreachable.prefix = Some("<leader> j z".into());
    loaded.keymaps.groups.push(unreachable);
    assert!(
        super::resolve_configuration(&app.commands, loaded, true)
            .err()
            .expect("unreachable group")
            .contains("user.unreachable")
    );
}

#[test]
pub(in crate::app) fn installed_package_bindings_are_deferred_without_a_duplicate_plugin_entry() {
    let (_app_dirs, app) = app();
    let mut loaded = crate::config::load_bundled_config();
    loaded.installed_plugins.insert("demo".into());
    loaded.keymaps.entries.push(crate::config::KeymapEntry {
        id: "user.demo".into(),
        context: Some("dashboard".into()),
        mode: String::new(),
        keys: Some("z".into()),
        command: Some("plugin.demo.run".into()),
        arguments: BTreeMap::new(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    });
    let (resolved, diagnostic) = super::resolve_configuration(&app.commands, loaded, true).unwrap();
    assert!(diagnostic.is_none());
    assert!(resolved.deferred_plugin_keymaps);
    assert!(resolved.expected_plugin_indexes.contains("demo"));
}

#[test]
pub(in crate::app) fn reload_preserves_plugin_window_rules() {
    let (_app_dirs, mut app) = app();
    let manifest = crate::app::plugins::fixture_manifest("0".repeat(64));
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 1, "fixture".into()).unwrap();
    app.extension.fixture_contribution(index.clone()).unwrap();
    let mut loaded = crate::config::load_bundled_config();
    loaded.installed_plugins.insert("demo".into());
    loaded.config.windows.rules = vec![crate::config::WindowRuleConfig {
        content_type: "plugin.demo.panel".into(),
        placement: crate::config::WindowPlacementConfig::Docked {
            edge: "right".into(),
            size: 20,
        },
    }];
    app.apply_configuration_candidate(loaded.clone(), false)
        .unwrap();
    assert_eq!(app.configuration.settings().windows, loaded.config.windows);
    app.process_effect(AppEffect::NewBuffer);
    app.reconcile_editor_windows();
    let opened = app.apply_plugin_ui_request(
        &index,
        crate::plugins::PluginUiRequest {
            token: 1,
            kind: crate::plugins::PluginUiRequestKind::OpenWindow {
                expected_graph_revision: app.window_session.graph().revision().unwrap(),
                content_type: "plugin.demo.panel".into(),
                title: "Demo".into(),
                placement: crate::plugins::PluginRequestedPlacement::Floating {
                    x: 1,
                    y: 1,
                    width: 20,
                    height: 8,
                },
                tree: Some(crate::plugins::PluginUiTree {
                    revision: 1,
                    root: 1,
                    nodes: vec![crate::plugins::PluginUiNode {
                        id: 1,
                        kind: crate::plugins::PluginUiNodeKind::Text,
                        text: "Demo panel".into(),
                        style: crate::plugins::PluginUiStyle::Default,
                        action: None,
                        children: Vec::new(),
                    }],
                }),
            },
        },
    );
    assert!(opened.error.is_none(), "{:?}", opened.error);
    let window = app.extension.window_snapshots()[0].window;
    assert_eq!(
        app.window_session.graph().window(window).unwrap().placement,
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 20
        }
    );
}

#[test]
pub(in crate::app) fn settings_toggles_wait_for_the_saved_configuration_to_reload() {
    use crate::app::commands::SettingsAction;
    let (_app_dirs, mut app) = app();
    app.open_settings();
    let page = app.ui.settings.as_mut().unwrap();
    assert_eq!(page.selected_row().unwrap().key, "editor.wrap");
    page.saving = true;
    page.status = Some(("Saved".into(), false));
    app.settings_action(SettingsAction::Toggle);
    let page = app.ui.settings.as_ref().unwrap();
    assert!(page.saving);
    assert_eq!(page.status, Some(("Saved".into(), false)));
    app.refresh_settings();
    assert!(!app.ui.settings.as_ref().unwrap().saving);
    app.ui.settings.as_mut().unwrap().saving = true;
    app.settings_status("Reload rejected".into(), true);
    assert!(!app.ui.settings.as_ref().unwrap().saving);
}

use crate::app::App;
use crate::app::configuration::KeymapOperation;
use crate::app::startup::StartupTarget;
use crate::app::tools::LspRuntimeSource;
use crate::editor::CommandValue;
use crate::editor::input::KeyEvent;
use crate::editor::input::MappingResolution;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use crate::ui::UiEvent;
use crate::windows::DockEdge;
use crate::windows::WindowPlacement;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::tempdir;

use crate::app::configuration::compile_keymaps;

use crate::app::commands::AppEffect;
use crate::app::commands::PickerKind;

use crate::config::load_bundled_config;
use crate::config::load_config;
use crate::config::load_keymaps;

#[test]
fn global_explorer_identity_can_be_disabled_without_disabling_just_one_surface() {
    let user = crate::config::parse_keymaps(
        "[[keymap]]\nid='builtin.global.explorer'\ndisable=true\n",
        PathBuf::from("keymaps.toml"),
    );
    assert!(user.diagnostic.is_none());
    assert_eq!(user.entries.len(), 7);
    assert!(user.entries.iter().all(|entry| entry.disable));
    let mut loaded = load_bundled_config();
    loaded.keymaps.entries.extend(user.entries);
    loaded.keymaps.user_spans = user.user_spans;
    let mut commands = crate::editor::CommandRegistry::default();
    crate::editor::register_builtin_commands(&mut commands).unwrap();
    crate::app::register_app_commands(&mut commands).unwrap();
    let (_, diagnostic) = compile_keymaps(&commands, &loaded.keymaps, &loaded.config.input.keymaps);
    assert!(diagnostic.is_none(), "{diagnostic:?}");

    let surface = crate::config::parse_keymaps(
        "[[keymap]]\nid='builtin.global.explorer.explorer'\ndisable=true\n",
        PathBuf::from("keymaps.toml"),
    );
    assert_eq!(surface.entries.len(), 1);
    assert_eq!(surface.entries[0].id, "builtin.global.explorer.explorer");
}

#[test]
fn user_guide_terminal_profile_and_keymap_are_accepted() {
    let guide = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/docs/user-guide-linux.md"
    ));
    let profile = guide
        .split("Example profile in `config.toml`:")
        .nth(1)
        .unwrap()
        .split("```toml\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let mapping = guide
        .split("Mapping in `keymaps.toml`:")
        .nth(1)
        .unwrap()
        .split("```toml\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();
    let directory = tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, profile).unwrap();
    let loaded = load_config(&path);
    assert!(loaded.diagnostic.is_none(), "{:?}", loaded.diagnostic);
    let path = directory.path().join("keymaps.toml");
    fs::write(&path, mapping).unwrap();
    let mappings = load_keymaps(&path);
    assert!(mappings.diagnostic.is_none(), "{:?}", mappings.diagnostic);
    let mut config = load_bundled_config();
    config.config = loaded.config;
    config.keymaps = mappings;
    crate::app::validate_loaded_configuration(config).unwrap();
}

use crate::persistence::PersistedState;
use crate::persistence::PersistenceWorker;
