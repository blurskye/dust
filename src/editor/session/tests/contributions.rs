use super::*;

#[test]
fn indexed_plugin_defaults_install_before_activation_and_roll_back_on_failure() {
    let mut app = app();
    let root = tempfile::tempdir().expect("plugin store");
    let manifest = crate::extensions::PluginManifest {
        id: "demo".into(),
        version: "1.0.0".into(),
        component_sha256: "0".repeat(64),
        capabilities: std::collections::BTreeSet::new(),
        commands: [(
            "run".into(),
            crate::extensions::PluginCommandDeclaration {
                title: "Run demo".into(),
                description: String::new(),
                arguments: BTreeMap::new(),
                context: crate::extensions::PluginCommandContext::Application,
                activation: crate::extensions::PluginActivationBehavior::ActiveOnly,
            },
        )]
        .into_iter()
        .collect(),
        contributions: crate::extensions::PluginContributions {
            schema_version: 1,
            input_contexts: Vec::new(),
            keymaps: vec![crate::extensions::PluginKeymapContribution {
                id: "plugin.demo.keymap.dashboard".into(),
                context: "dashboard".into(),
                mode: String::new(),
                keys: "x y".into(),
                command: "plugin.demo.run".into(),
                arguments: BTreeMap::new(),
            }],
            key_groups: vec![crate::extensions::PluginKeyGroupContribution {
                id: "plugin.demo.group.dashboard".into(),
                context: "dashboard".into(),
                mode: String::new(),
                prefix: "x".into(),
                label: "Demo".into(),
                icon: None,
                order: 1,
            }],
            content_types: Vec::new(),
            subscriptions: Vec::new(),
            providers: Vec::new(),
            virtual_documents: Vec::new(),
            storage: None,
        },
    };
    fs::write(
        root.path().join("manifest.toml"),
        toml::to_string(&manifest).expect("manifest TOML"),
    )
    .expect("manifest");
    let load = app
        .plugins
        .index_from_store(
            "demo".into(),
            root.path().to_path_buf(),
            PathBuf::from("manifest.toml"),
            PathBuf::from("missing.wasm"),
            app.plugin_limits(),
        )
        .expect("index request");
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.plugin_contributions.get("demo").is_none() {
        app.process_plugin_events();
        assert!(Instant::now() < deadline, "index integration timed out");
        thread::yield_now();
    }
    let indexed = app
        .configuration
        .keymaps
        .inspect_key("dashboard", "x y")
        .expect("indexed binding");
    assert_eq!(indexed.contribution_owner.as_deref(), Some("demo"));
    assert_eq!(indexed.contribution_generation, load.0);
    assert!(!app.plugins.is_active_generation("demo", load));
    let previous = app.plugins.load_id("demo");
    app.invoke_mapped(
        crate::editor::CommandInvocation::new("plugin.demo.run", InvocationSource::Test)
            .expect("static command"),
    );
    assert_eq!(app.plugins.load_id("demo"), previous);
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.plugin_contributions.get("demo").is_some() {
        app.process_plugin_events();
        assert!(Instant::now() < deadline, "activation rollback timed out");
        thread::yield_now();
    }
    assert!(
        app.configuration
            .keymaps
            .inspect_key("dashboard", "x y")
            .is_none()
    );
    assert!(
        app.commands
            .specs()
            .all(|spec| spec.id.as_str() != "plugin.demo.run")
    );
}

#[test]
fn typed_plugin_defaults_compile_between_bundled_and_user_contributions() {
    let mut app = app();
    app.commands
        .register(
            crate::editor::CommandSpec {
                id: crate::editor::CommandId::new("plugin.demo.run").expect("plugin id"),
                title: "Run demo".into(),
                description: String::new(),
                arguments: crate::editor::ArgumentSchema::default(),
                context: crate::editor::ContextRequirement::Application,
                source: crate::editor::CommandSource::Plugin("demo".into()),
            },
            |_, _| Ok(crate::editor::CommandOutcome::empty()),
        )
        .expect("plugin command");
    let manifest = crate::extensions::PluginManifest {
        id: "demo".into(),
        version: "1.0.0".into(),
        component_sha256: "0".repeat(64),
        capabilities: std::collections::BTreeSet::new(),
        commands: [(
            "run".into(),
            crate::extensions::PluginCommandDeclaration {
                title: "Run demo".into(),
                description: String::new(),
                arguments: BTreeMap::new(),
                context: crate::extensions::PluginCommandContext::Application,
                activation: crate::extensions::PluginActivationBehavior::ActiveOnly,
            },
        )]
        .into_iter()
        .collect(),
        contributions: crate::extensions::PluginContributions {
            schema_version: 1,
            input_contexts: vec![crate::extensions::PluginInputContextContribution {
                id: "plugin.demo.chat".into(),
                title: "Chat".into(),
                parent_surface: Some("panel".into()),
            }],
            keymaps: vec![
                crate::extensions::PluginKeymapContribution {
                    id: "plugin.demo.keymap.action".into(),
                    context: "editor".into(),
                    mode: "normal".into(),
                    keys: "<leader> a x".into(),
                    command: "plugin.demo.run".into(),
                    arguments: BTreeMap::new(),
                },
                crate::extensions::PluginKeymapContribution {
                    id: "plugin.demo.keymap.buffer-shadow".into(),
                    context: "editor".into(),
                    mode: "normal".into(),
                    keys: "<leader> f b".into(),
                    command: "plugin.demo.run".into(),
                    arguments: BTreeMap::new(),
                },
                crate::extensions::PluginKeymapContribution {
                    id: "plugin.demo.keymap.chat".into(),
                    context: "plugin.demo.chat".into(),
                    mode: String::new(),
                    keys: "g".into(),
                    command: "plugin.demo.run".into(),
                    arguments: BTreeMap::new(),
                },
            ],
            key_groups: vec![crate::extensions::PluginKeyGroupContribution {
                id: "plugin.demo.group.action".into(),
                context: "editor".into(),
                mode: "normal".into(),
                prefix: "<leader> a".into(),
                label: "Demo".into(),
                icon: None,
                order: 1,
            }],
            content_types: Vec::new(),
            subscriptions: Vec::new(),
            providers: Vec::new(),
            virtual_documents: Vec::new(),
            storage: None,
        },
    };
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 4, "demo-manifest.toml".into())
            .expect("index");
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.path = PathBuf::from("keymaps.toml");
    loaded.entries.extend([
        crate::config::KeymapEntry {
            id: "plugin.demo.keymap.action".into(),
            context: Some("editor".into()),
            mode: "normal".into(),
            keys: Some("<leader> a z".into()),
            command: Some("plugin.demo.run".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
        },
        crate::config::KeymapEntry {
            id: "user.test.buffer-shadow".into(),
            context: Some("editor".into()),
            mode: "normal".into(),
            keys: Some("<leader> f b".into()),
            command: Some("health.open".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
        },
    ]);
    let mut catalog = crate::extensions::PluginContributionCatalog::default();
    catalog.install(index).expect("catalog install");
    let resolved = super::compile_keymaps_with_plugin_catalog(
        &app.commands,
        &loaded,
        &catalog,
        &app.configuration.settings.input.keymaps,
    )
    .expect("typed contributions");
    assert_eq!(
        resolved
            .inspect_id("plugin.demo.keymap.action")
            .map(|inspection| inspection.keys),
        Some("space a z".into())
    );
    assert!(resolved.inspect_key("editor.normal", "space a x").is_none());
    assert_eq!(
        resolved
            .inspect_key("editor.normal", "space f b")
            .and_then(|inspection| inspection.command),
        Some("health.open".into())
    );
    let chat = resolved
        .inspect_key("plugin.demo.chat", "g")
        .expect("plugin context binding");
    assert_eq!(
        chat.contribution_layer,
        crate::editor::input::InputContextLayer::Plugin
    );
    assert_eq!(chat.contribution_owner.as_deref(), Some("demo"));
    assert_eq!(chat.contribution_generation, 4);
    assert!(
        resolved
            .groups()
            .iter()
            .any(|group| { group.id == "plugin.demo.group.action" && group.prefix == "space a" })
    );
}

#[test]
fn resolved_key_hints_follow_leaders_groups_and_configured_command_labels() {
    let app = app();
    let labels = &app.configuration.settings.command_labels;
    let hints = app
        .configuration
        .keymaps
        .hints_for_prefix("editor.normal", "space", labels)
        .expect("leader continuations");
    let files = hints
        .rows
        .iter()
        .find(|row| row.next_key == "f")
        .expect("files group");
    assert_eq!(files.label, "Files");
    assert_eq!(
        files.group_id.as_deref(),
        Some("builtin.editor.normal.group.files")
    );
    assert!(files.mappings.iter().all(|mapping| {
        mapping.id.starts_with("builtin.")
            && mapping.source.starts_with("<bundled:keymaps>")
            && mapping.operation == KeymapOperation::Command
    }));

    let mut custom_labels = labels.clone();
    custom_labels.insert("buffer.picker.open".into(), "Choose a buffer".into());
    let file_hints = app
        .configuration
        .keymaps
        .hints_for_prefix("editor.normal", "space f", &custom_labels)
        .expect("file continuations");
    assert_eq!(
        file_hints
            .rows
            .iter()
            .find(|row| row.next_key == "b")
            .map(|row| row.label.as_str()),
        Some("Choose a buffer")
    );

    let mut policy = app.configuration.settings.input.keymaps.clone();
    policy.leader = ",".into();
    policy.local_leader = ";".into();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.entries.push(crate::config::KeymapEntry {
        id: "user.test.local-leader-command".into(),
        context: Some("editor".into()),
        mode: "normal".into(),
        keys: Some("<localleader> a".into()),
        command: Some("health.open".into()),
        arguments: BTreeMap::new(),
        unmap: false,
        disable: false,
        pass_through: false,
    });
    loaded.groups.push(crate::config::KeyGroupEntry {
        id: "user.test.local-leader-group".into(),
        context: Some("editor".into()),
        mode: "normal".into(),
        prefix: Some("<localleader>".into()),
        label: Some("Local".into()),
        icon: None,
        order: 1,
        disable: false,
    });
    let (resolved, diagnostic) = compile_keymaps(&app.commands, &loaded, &policy);
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    assert!(
        resolved
            .groups()
            .iter()
            .any(|group| group.source_prefix == "<leader> f" && group.prefix == ", f")
    );
    assert!(
        resolved
            .groups()
            .iter()
            .any(|group| group.source_prefix == "<localleader>" && group.prefix == ";")
    );
    assert!(
        resolved
            .hints_for_prefix("editor.normal", ";", labels)
            .is_some_and(|projection| projection.rows.iter().any(|row| row.next_key == "a"))
    );
    assert!(
        resolved
            .hints_for_prefix("editor.normal", "space", labels)
            .is_none()
    );
}

#[test]
fn key_hint_projection_uses_final_identity_and_sequence_operations_only() {
    let app = app();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.path = PathBuf::from("keymaps.toml");
    loaded.groups.push(crate::config::KeyGroupEntry {
        id: "builtin.editor.normal.group.files".into(),
        context: Some("editor".into()),
        mode: "normal".into(),
        prefix: Some("<leader> f".into()),
        label: Some("Project files".into()),
        icon: Some("F".into()),
        order: 1,
        disable: false,
    });
    loaded.entries.extend([
        crate::config::KeymapEntry {
            id: "builtin.editor.normal.buffer-picker-open.leader-f-b".into(),
            context: Some("editor".into()),
            mode: "normal".into(),
            keys: Some("<leader> f z".into()),
            command: Some("buffer.picker.open".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
        },
        crate::config::KeymapEntry {
            id: "user.test.unmap-command-palette-hint".into(),
            context: Some("editor".into()),
            mode: "normal".into(),
            keys: Some("<leader> f c".into()),
            command: None,
            arguments: BTreeMap::new(),
            unmap: true,
            disable: false,
            pass_through: false,
        },
    ]);
    let (resolved, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings.input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    let leader = resolved
        .hints_for_prefix(
            "editor.normal",
            "space",
            &app.configuration.settings.command_labels,
        )
        .expect("leader hints");
    assert_eq!(
        leader
            .rows
            .iter()
            .find(|row| row.next_key == "f")
            .map(|row| row.label.as_str()),
        Some("Project files")
    );
    let files = resolved
        .hints_for_prefix(
            "editor.normal",
            "space f",
            &app.configuration.settings.command_labels,
        )
        .expect("file hints");
    assert!(!files.rows.iter().any(|row| row.next_key == "b"));
    assert!(!files.rows.iter().any(|row| row.next_key == "c"));
    assert!(files.rows.iter().any(|row| {
        row.next_key == "z"
            && row.mappings.iter().any(|mapping| {
                mapping.id == "builtin.editor.normal.buffer-picker-open.leader-f-b"
                    && mapping.remaining_keys == "z"
            })
    }));

    let mut disabled = crate::config::load_bundled_config().keymaps;
    disabled.groups.push(crate::config::KeyGroupEntry {
        id: "builtin.editor.normal.group.files".into(),
        context: None,
        mode: String::new(),
        prefix: None,
        label: None,
        icon: None,
        order: 0,
        disable: true,
    });
    disabled.entries.push(crate::config::KeymapEntry {
        id: "builtin.editor.normal.buffer-picker-open.leader-f-b".into(),
        context: None,
        mode: String::new(),
        keys: None,
        command: None,
        arguments: BTreeMap::new(),
        unmap: false,
        disable: true,
        pass_through: false,
    });
    let (disabled, diagnostic) = compile_keymaps(
        &app.commands,
        &disabled,
        &app.configuration.settings.input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    assert!(
        !disabled
            .groups()
            .iter()
            .any(|group| group.id == "builtin.editor.normal.group.files")
    );
    let files = disabled
        .hints_for_prefix(
            "editor.normal",
            "space f",
            &app.configuration.settings.command_labels,
        )
        .expect("remaining file hints");
    assert!(!files.rows.iter().any(|row| row.next_key == "b"));
}

#[test]
fn invalid_key_groups_reject_compilation_and_reload_transactionally() {
    let app = app();
    for (group, expected) in [
        (
            crate::config::KeyGroupEntry {
                id: "user.test.unreachable-group".into(),
                context: Some("editor".into()),
                mode: "normal".into(),
                prefix: Some("<leader> u".into()),
                label: Some("Unused".into()),
                icon: None,
                order: 0,
                disable: false,
            },
            "reaches no resolved binding",
        ),
        (
            crate::config::KeyGroupEntry {
                id: "user.test.duplicate-files-group".into(),
                context: Some("editor".into()),
                mode: "normal".into(),
                prefix: Some("<leader> f".into()),
                label: Some("Duplicate".into()),
                icon: None,
                order: 0,
                disable: false,
            },
            "duplicates id",
        ),
        (
            crate::config::KeyGroupEntry {
                id: "BAD".into(),
                context: Some("editor".into()),
                mode: "normal".into(),
                prefix: Some("<leader> f".into()),
                label: Some("Bad".into()),
                icon: None,
                order: 0,
                disable: false,
            },
            "invalid id",
        ),
        (
            crate::config::KeyGroupEntry {
                id: "user.test.unknown-context-group".into(),
                context: Some("unknown".into()),
                mode: String::new(),
                prefix: Some("<leader> f".into()),
                label: Some("Bad context".into()),
                icon: None,
                order: 0,
                disable: false,
            },
            "unknown context",
        ),
        (
            crate::config::KeyGroupEntry {
                id: "user.test.malformed-prefix-group".into(),
                context: Some("editor".into()),
                mode: "normal".into(),
                prefix: Some("<leader> not-a-key".into()),
                label: Some("Bad prefix".into()),
                icon: None,
                order: 0,
                disable: false,
            },
            "unknown key",
        ),
        (
            crate::config::KeyGroupEntry {
                id: "user.test.control-label-group".into(),
                context: Some("editor".into()),
                mode: "normal".into(),
                prefix: Some("<leader> f".into()),
                label: Some("Bad\nlabel".into()),
                icon: None,
                order: 0,
                disable: false,
            },
            "non-control",
        ),
        (
            crate::config::KeyGroupEntry {
                id: "user.test.oversized-icon-group".into(),
                context: Some("editor".into()),
                mode: "normal".into(),
                prefix: Some("<leader> f".into()),
                label: Some("Bad icon".into()),
                icon: Some("123456789".into()),
                order: 0,
                disable: false,
            },
            "icon must contain",
        ),
    ] {
        let mut loaded = crate::config::load_bundled_config().keymaps;
        loaded.path = PathBuf::from("keymaps.toml");
        loaded.groups.push(group);
        let (_, diagnostic) = compile_keymaps(
            &app.commands,
            &loaded,
            &app.configuration.settings.input.keymaps,
        );
        assert!(
            diagnostic
                .as_deref()
                .is_some_and(|message| message.contains(expected)),
            "expected {expected}: {diagnostic:?}"
        );
    }

    let mut duplicate = crate::config::load_bundled_config().keymaps;
    duplicate.path = PathBuf::from("keymaps.toml");
    let group = crate::config::KeyGroupEntry {
        id: "user.test.duplicate-identity".into(),
        context: Some("editor".into()),
        mode: "normal".into(),
        prefix: Some("<leader> f b".into()),
        label: Some("Buffer".into()),
        icon: None,
        order: 0,
        disable: false,
    };
    duplicate.groups.extend([group.clone(), group]);
    let (_, diagnostic) = compile_keymaps(
        &app.commands,
        &duplicate,
        &app.configuration.settings.input.keymaps,
    );
    assert!(
        diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("same contribution layer"))
    );

    for (group, expected) in [
        (
            crate::config::KeyGroupEntry {
                id: "user.test.unknown-group-disable".into(),
                context: None,
                mode: String::new(),
                prefix: None,
                label: None,
                icon: None,
                order: 0,
                disable: true,
            },
            "targets no lower contribution",
        ),
        (
            crate::config::KeyGroupEntry {
                id: "builtin.editor.normal.group.files".into(),
                context: Some("editor".into()),
                mode: String::new(),
                prefix: None,
                label: None,
                icon: None,
                order: 0,
                disable: true,
            },
            "must contain only",
        ),
    ] {
        let mut loaded = crate::config::load_bundled_config().keymaps;
        loaded.groups.push(group);
        let (_, diagnostic) = compile_keymaps(
            &app.commands,
            &loaded,
            &app.configuration.settings.input.keymaps,
        );
        assert!(
            diagnostic
                .as_deref()
                .is_some_and(|message| message.contains(expected)),
            "expected {expected}: {diagnostic:?}"
        );
    }

    let mut invalid = crate::config::load_bundled_config().keymaps;
    invalid.path = PathBuf::from("keymaps.toml");
    invalid.groups.push(crate::config::KeyGroupEntry {
        id: "user.test.transactional-unreachable".into(),
        context: Some("editor".into()),
        mode: "normal".into(),
        prefix: Some("<leader> u".into()),
        label: Some("Unused".into()),
        icon: None,
        order: 0,
        disable: false,
    });
    let (retained, diagnostic) = reload_keymaps(
        &app.commands,
        &app.configuration.keymaps,
        &invalid,
        &app.configuration.settings.input.keymaps,
    );
    assert!(
        diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("keeping previous keymaps"))
    );
    assert_eq!(
        retained.groups(),
        app.configuration.keymaps.groups(),
        "rejected candidate must retain groups and maps atomically"
    );
}

#[test]
fn macros_record_replay_last_register_and_stop_at_limits() {
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abc"), None);
    app.ui.dashboard = false;
    for key in ['q', 'a', 'x', 'q', '@', 'a', '@', '@'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "");
    assert_eq!(app.last_macro, Some('a'));

    let recursive = crate::editor::CommandInvocation::new("macro.replay", InvocationSource::Macro)
        .expect("macro command")
        .with_argument("register", CommandValue::String("r".into()));
    app.state.registers.set_macro('r', vec![recursive]);
    app.replay_macro('r');
    assert_eq!(app.replay_depth, 0);
    assert!(app.ui.notification_error);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("recursion limit"))
    );
}
