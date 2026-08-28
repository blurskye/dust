use super::*;

#[test]
fn configured_notification_timeout_expires_visible_message() {
    let mut app = app();
    app.configuration.settings.ui.notifications.timeout_ms = 1;
    app.ui.notification = Some("transient".into());
    app.update_notification_timer();
    std::thread::sleep(Duration::from_millis(2));
    app.update_notification_timer();
    assert!(app.ui.notification.is_none());
}

#[test]
fn stale_configuration_generations_are_never_applied() {
    assert!(super::configuration_generation_is_newer(4, 5));
    assert!(!super::configuration_generation_is_newer(5, 5));
    assert!(!super::configuration_generation_is_newer(6, 5));
}

#[test]
fn complete_configuration_reload_is_atomic() {
    let mut app = app();
    let original_theme = app.configuration.theme.clone();
    let explanation = app
        .configuration
        .explain("ui.command_line.width")
        .expect("explanation");
    assert_eq!(explanation.source, "bundled defaults");
    assert_eq!(explanation.contributors, vec!["bundled defaults"]);
    let original_width = app.configuration.settings.ui.command_line.width;
    let mut invalid = crate::config::load_bundled_config();
    invalid.config.ui.command_line.width = original_width + 1;
    invalid
        .config
        .input
        .text_commands
        .insert("editor.insert".into(), "unknown.command".into());
    assert!(app.apply_configuration_candidate(invalid, false).is_err());
    assert_eq!(
        app.configuration.settings.ui.command_line.width,
        original_width
    );
    assert_eq!(app.configuration.theme, original_theme);

    let mut valid = crate::config::load_bundled_config();
    valid.config.ui.command_line.width = original_width + 1;
    valid.dashboard.identity = "CUSTOM DASH".into();
    app.apply_configuration_candidate(valid, false)
        .expect("valid complete snapshot");
    assert_eq!(
        app.configuration.settings.ui.command_line.width,
        original_width + 1
    );
    assert_eq!(app.configuration.dashboard.identity, "CUSTOM DASH");
}

#[test]
fn resolved_explain_reports_exact_layered_and_replacement_provenance() {
    let app = app();
    let root = tempdir().expect("tempdir");
    let config_path = root.path().join("config.toml");
    let config_source = "editor = { tab_width = 7 }\n";
    fs::write(&config_path, config_source).expect("config");
    let mut loaded = crate::config::load_config(&config_path);

    let dashboard_path = root.path().join("dashboard.toml");
    let dashboard_source = include_str!("../../../../assets/defaults/dashboard.toml")
        .replace("D U S T", "PROVENANCE DASH");
    loaded.dashboard = toml::from_str(&dashboard_source).expect("dashboard");
    loaded.dashboard_path = dashboard_path.clone();
    loaded.dashboard_spans = crate::config::setting_spans(&dashboard_source);

    let theme_path = root.path().join("provenance.toml");
    let theme_source = include_str!("../../../../assets/themes/dust.toml")
        .replace("name = \"dust\"", "name = \"provenance\"");
    loaded.theme = toml::from_str(&theme_source).expect("theme");
    loaded.theme_path = theme_path.clone();
    loaded.theme_spans = crate::config::setting_spans(&theme_source);

    let (resolved, diagnostic) =
        super::resolve_configuration(&app.commands, loaded, true).expect("resolve");
    assert!(diagnostic.is_none());
    let setting = resolved.explain("editor.tab_width").expect("setting");
    assert!(
        setting
            .source
            .starts_with(&config_path.display().to_string())
    );
    assert_eq!(setting.contributors[0], "bundled defaults");
    assert_eq!(setting.contributors[1], setting.source);

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
        (dashboard_source.as_str(), dashboard_action, "\"Command\""),
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
fn user_keymaps_execute_typed_commands_and_unmap_defaults() {
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abc"), None);
    app.ui.dashboard = false;
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
        },
    ]);
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings.input.keymaps,
    );
    assert!(diagnostic.is_none());
    assert!(
        keymaps
            .conflicts
            .iter()
            .any(|conflict| conflict.contains("shadows"))
    );
    app.configuration.keymaps = keymaps;
    let inspection = app
        .configuration
        .keymaps
        .inspect("editor.normal", "editor.quit")
        .expect("inspect remap");
    assert_eq!(inspection.source_keys, "<leader> q");
    assert_eq!(inspection.keys, "space q");
    let unmap = app
        .configuration
        .keymaps
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
    app.handle_configured_key(KeyEvent::character(' '));
    app.handle_configured_key(KeyEvent::character('q'));
    assert!(app.should_quit);
}

#[test]
fn symbolic_leaders_expand_before_dispatch_and_preserve_literal_space_actions() {
    let app = app();
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
    });
    loaded.bundled_count = bundled_count;
    let policy = crate::config::KeymapPolicyConfig {
        load_bundled_defaults: true,
        leader: ",".into(),
        local_leader: ";".into(),
    };
    let (keymaps, diagnostic) = compile_keymaps(&app.commands, &loaded, &policy);
    assert!(diagnostic.is_none(), "{diagnostic:?}");

    let leader_binding = keymaps
        .inspect("editor.normal", "buffer.picker.open")
        .expect("bundled leader binding");
    assert_eq!(leader_binding.source_keys, "<leader> f b");
    assert_eq!(leader_binding.keys, ", f b");
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

    let mut dispatch = keymaps.clone();
    assert_eq!(
        dispatch.resolve("editor.normal", KeyEvent::character(',')),
        MappingResolution::Pending
    );
    assert_eq!(
        dispatch.resolve("editor.normal", KeyEvent::character('f')),
        MappingResolution::Pending
    );
    assert!(matches!(
        dispatch.resolve("editor.normal", KeyEvent::character('b')),
        MappingResolution::Ready(invocation)
            if invocation.id.as_str() == "buffer.picker.open"
    ));
}

#[test]
fn expanded_duplicate_sequences_are_rejected_with_symbolic_diagnostics() {
    let app = app();
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
    };
    loaded.entries.extend([
        mapping("user.test.expanded-symbolic", "<leader> z"),
        mapping("user.test.expanded-literal", "space z"),
    ]);
    let (_, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings.input.keymaps,
    );
    let diagnostic = diagnostic.expect("expanded duplicate must fail");
    assert!(diagnostic.contains("<leader> z"));
    assert!(diagnostic.contains("expanded to `space z`"));
    assert!(diagnostic.contains("conflicts with id"));
}

#[test]
fn invalid_leader_policy_rejects_the_complete_configuration_transactionally() {
    let mut app = app();
    let original_policy = app.configuration.settings.input.keymaps.clone();
    let original_binding = app
        .configuration
        .keymaps
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
        assert_eq!(app.configuration.settings.input.keymaps, original_policy);
        assert_eq!(
            app.configuration
                .keymaps
                .binding_for("editor.normal", "buffer.picker.open"),
            Some(original_binding.as_str())
        );
    }
}

#[test]
fn disabling_bundled_keymaps_keeps_only_explicit_user_declarations() {
    let app = app();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.entries.push(crate::config::KeymapEntry {
        id: "user.test.disabling-bundled-keymaps-keeps-only-explicit-user-declarations".into(),
        context: Some("dashboard".into()),
        mode: String::new(),
        keys: Some("<leader> z".into()),
        command: Some("health.open".into()),
        arguments: Default::default(),
        unmap: false,
        disable: false,
        pass_through: false,
    });
    let policy = crate::config::KeymapPolicyConfig {
        load_bundled_defaults: false,
        leader: ",".into(),
        local_leader: ";".into(),
    };
    let (keymaps, diagnostic) = compile_keymaps(&app.commands, &loaded, &policy);
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    assert!(
        keymaps
            .binding_for("editor.normal", "buffer.picker.open")
            .is_none()
    );
    assert_eq!(keymaps.binding_for("dashboard", "health.open"), Some(", z"));
}

#[test]
fn identity_rebind_moves_a_bundled_mapping_without_leaving_the_old_sequence() {
    let app = app();
    let target = "builtin.editor.normal.buffer-picker-open.leader-f-b";
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
    });
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings.input.keymaps,
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

    let mut dispatch = keymaps;
    assert_eq!(
        dispatch.resolve("editor.normal", KeyEvent::character('g')),
        MappingResolution::Pending
    );
    assert!(matches!(
        dispatch.resolve("editor.normal", KeyEvent::character('b')),
        MappingResolution::Ready(invocation)
            if invocation.id.as_str() == "buffer.picker.open"
    ));
}

#[test]
fn identity_disable_inherits_symbolic_location_and_tracks_leader_changes() {
    let app = app();
    let target = "builtin.editor.normal.explorer-open.leader-f-e";
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
    });
    for leader in [",", ";"] {
        let policy = crate::config::KeymapPolicyConfig {
            load_bundled_defaults: true,
            leader: leader.into(),
            local_leader: "\\".into(),
        };
        let (keymaps, diagnostic) = compile_keymaps(&app.commands, &loaded, &policy);
        assert!(diagnostic.is_none(), "{diagnostic:?}");
        assert!(
            keymaps
                .binding_for("editor.normal", "explorer.open")
                .is_none()
        );
        let inspection = keymaps.inspect_id(target).expect("disable inspection");
        assert_eq!(inspection.operation, KeymapOperation::Disable);
        assert_eq!(inspection.command, None);
        assert_eq!(inspection.source_keys, "<leader> f e");
        assert_eq!(inspection.keys, format!("{leader} f e"));

        let mut dispatch = keymaps;
        assert_eq!(
            dispatch.resolve(
                "editor.normal",
                KeyEvent::character(leader.chars().next().expect("leader"))
            ),
            MappingResolution::Pending
        );
        assert_eq!(
            dispatch.resolve("editor.normal", KeyEvent::character('f')),
            MappingResolution::Pending
        );
        assert_eq!(
            dispatch.resolve("editor.normal", KeyEvent::character('e')),
            MappingResolution::Disabled
        );
    }
}

#[test]
fn identity_disable_rejects_unknown_omitted_and_overdeclared_targets() {
    let app = app();
    let target = "builtin.editor.normal.explorer-open.leader-f-e";
    for (load_bundled_defaults, id, context, expected) in [
        (
            true,
            "user.missing.binding",
            None,
            "targets no lower contribution",
        ),
        (false, target, None, "targets no lower contribution"),
        (
            true,
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
        });
        let policy = crate::config::KeymapPolicyConfig {
            load_bundled_defaults,
            ..crate::config::KeymapPolicyConfig::default()
        };
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
fn malformed_keymap_identity_rejects_the_configuration_transactionally() {
    let mut app = app();
    let original = app
        .configuration
        .keymaps
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
        });
        let error = app
            .apply_configuration_candidate(candidate, false)
            .expect_err("invalid identity must reject candidate");
        assert!(error.contains("invalid id"), "{error}");
        assert_eq!(
            app.configuration
                .keymaps
                .binding_for("editor.normal", "buffer.picker.open"),
            Some(original.as_str())
        );
    }
}

#[test]
fn invalid_user_keymap_set_is_rejected_transactionally() {
    let app = app();
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
    });
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings.input.keymaps,
    );
    assert!(!keymaps.resolvers.is_empty());
    assert!(diagnostic.is_some_and(|message| message.contains("keeping bundled keymaps")));

    let (reloaded, reload_diagnostic) = reload_keymaps(
        &app.commands,
        &app.configuration.keymaps,
        &loaded,
        &app.configuration.settings.input.keymaps,
    );
    assert_eq!(
        reloaded.binding_for("editor.normal", "edit.delete_char"),
        app.configuration
            .keymaps
            .binding_for("editor.normal", "edit.delete_char")
    );
    assert!(
        reload_diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("keeping previous keymaps"))
    );
}

#[test]
fn keymap_inspection_by_id_reports_identity_operation_sequences_and_source() {
    let mut app = app();
    let id = "builtin.editor.normal.explorer-open.leader-f-e";
    app.process_effect(crate::editor::AppEffect::InspectKeymap {
        id: Some(id.into()),
        context: None,
        key: None,
        command: None,
    });
    let message = app.ui.notification.expect("inspection notification");
    assert!(message.contains(id));
    assert!(message.contains("<leader> f e => space f e"));
    assert!(message.contains("[command] explorer.open"));
    assert!(message.contains("<bundled:keymaps>:declaration"));
}

#[test]
fn keymap_inspection_reports_real_user_path_and_byte_range() {
    let mut app = app();
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
        &app.configuration.settings.input.keymaps,
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
    app.configuration.keymaps = keymaps;
}

#[test]
fn duplicate_user_ids_are_rejected_transactionally() {
    let app = app();
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
    };
    loaded.entries.extend([mapping.clone(), mapping]);
    let (_, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings.input.keymaps,
    );
    assert!(
        diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("duplicates id"))
    );
}

#[test]
fn configured_focus_loss_cancels_pending_mapping() {
    let mut app = app();
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
    });
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings.input.keymaps,
    );
    assert!(diagnostic.is_none());
    app.configuration.keymaps = keymaps;
    app.handle_configured_key(KeyEvent::character('z'));
    assert!(app.pending_keymap.is_some());
    app.handle_event(crate::ui::UiEvent::FocusLost);
    assert!(app.pending_keymap.is_none());
}

#[test]
fn prompt_special_keys_are_remappable_from_configuration() {
    let mut app = app();
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
        },
    ]);
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings.input.keymaps,
    );
    assert!(diagnostic.is_none());
    app.configuration.keymaps = keymaps;
    app.open_prompt(crate::ui::PromptKind::Command, String::new());
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    assert!(app.ui.prompt.is_some());
    app.handle_configured_key("ctrl-c".parse().expect("configured key"));
    assert!(app.ui.prompt.is_none());
}
