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
        .extension
        .index_from_store(
            "demo".into(),
            root.path().to_path_buf(),
            PathBuf::from("manifest.toml"),
            PathBuf::from("missing.wasm"),
            app.plugin_limits(),
        )
        .expect("index request");
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.extension.contributions().get("demo").is_none() {
        app.process_plugin_events();
        assert!(Instant::now() < deadline, "index integration timed out");
        thread::yield_now();
    }
    let indexed = app
        .configuration
        .keymaps()
        .inspect_key("dashboard", "x y")
        .expect("indexed binding");
    assert_eq!(indexed.contribution_owner.as_deref(), Some("demo"));
    assert_eq!(indexed.contribution_generation, load.0);
    assert!(!app.extension.is_active_generation("demo", load));
    let previous = app.extension.load_id("demo");
    app.invoke_mapped(
        crate::editor::CommandInvocation::new("plugin.demo.run", InvocationSource::Test)
            .expect("static command"),
    );
    assert_eq!(app.extension.load_id("demo"), previous);
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.extension.contributions().get("demo").is_some() {
        app.process_plugin_events();
        assert!(Instant::now() < deadline, "activation rollback timed out");
        thread::yield_now();
    }
    assert!(
        app.configuration
            .keymaps()
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
                    keys: "<leader> j x".into(),
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
                prefix: "<leader> j".into(),
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
            keys: Some("<leader> j z".into()),
            command: Some("plugin.demo.run".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
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
            desc: None,
        },
    ]);
    let mut catalog = crate::extensions::PluginContributionCatalog::default();
    catalog.install(index).expect("catalog install");
    let resolved = super::compile_keymaps_with_plugin_catalog(
        &app.commands,
        &loaded,
        &catalog,
        &app.configuration.settings().input.keymaps,
    )
    .expect("typed contributions");
    assert_eq!(
        resolved
            .inspect_id("plugin.demo.keymap.action")
            .map(|inspection| inspection.keys),
        Some("space j z".into())
    );
    assert!(resolved.inspect_key("editor.normal", "space j x").is_none());
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
            .any(|group| { group.id == "plugin.demo.group.action" && group.prefix == "space j" })
    );
}

#[test]
fn resolved_key_hints_follow_leaders_groups_and_configured_command_labels() {
    let app = app();
    let labels = &app.configuration.settings().command_labels;
    let hints = app
        .configuration
        .keymaps()
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
        Some("builtin.global.group.files.editor-normal")
    );
    assert_eq!(
        hints
            .rows
            .iter()
            .find(|row| row.next_key == "s")
            .and_then(|row| row.group_id.as_deref()),
        Some("builtin.global.group.search.editor-normal")
    );
    assert!(files.mappings.iter().all(|mapping| {
        mapping.id.starts_with("builtin.")
            && mapping.source.starts_with("<bundled:keymaps>")
            && mapping.operation == KeymapOperation::Command
    }));

    // A keymap `desc` names the binding in hints ahead of the command description.
    let mut custom_labels = labels.clone();
    custom_labels.insert("buffer.picker.open".into(), "Choose a buffer".into());
    let file_hints = app
        .configuration
        .keymaps()
        .hints_for_prefix("editor.normal", "space f", &custom_labels)
        .expect("file continuations");
    assert_eq!(
        file_hints
            .rows
            .iter()
            .find(|row| row.next_key == "b")
            .map(|row| row.label.as_str()),
        Some("Buffers")
    );
    let goto_hints = app
        .configuration
        .keymaps()
        .hints_for_prefix("editor.normal", "g", labels)
        .expect("goto continuations");
    let definition_command = goto_hints
        .rows
        .iter()
        .find(|row| row.next_key == "d")
        .and_then(|row| row.mappings[0].command.clone())
        .expect("g d command");
    custom_labels.insert(definition_command, "Jump to definition".into());
    assert_eq!(
        app.configuration
            .keymaps()
            .hints_for_prefix("editor.normal", "g", &custom_labels)
            .and_then(|hints| hints.rows.into_iter().find(|row| row.next_key == "d"))
            .map(|row| row.label),
        Some("Jump to definition".into())
    );
    for key in ["d", "D", "I", "y", "r", "K", "O"] {
        assert!(
            goto_hints.rows.iter().any(|row| row.next_key == key),
            "missing g {key} navigation hint"
        );
    }

    let mut policy = app.configuration.settings().input.keymaps.clone();
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
        desc: None,
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
    // `overlay.help.contextual` is bound to `<leader> ?` as a LEAF, and a hint is projected
    // from a declared key group rather than from a binding, so the bundled keymap still
    // resolves no leader hint under space. This pins that distinction.
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
        id: "builtin.global.group.files.editor-normal".into(),
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
            id: "builtin.global.files-buffers.editor-normal".into(),
            context: Some("editor".into()),
            mode: "normal".into(),
            keys: Some("<leader> f z".into()),
            command: Some("buffer.picker.open".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
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
            desc: None,
        },
    ]);
    let (resolved, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    let leader = resolved
        .hints_for_prefix(
            "editor.normal",
            "space",
            &app.configuration.settings().command_labels,
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
            &app.configuration.settings().command_labels,
        )
        .expect("file hints");
    assert!(!files.rows.iter().any(|row| row.next_key == "b"));
    assert!(!files.rows.iter().any(|row| row.next_key == "c"));
    assert!(files.rows.iter().any(|row| {
        row.next_key == "z"
            && row.mappings.iter().any(|mapping| {
                mapping.id == "builtin.global.files-buffers.editor-normal"
                    && mapping.remaining_keys == "z"
            })
    }));

    let mut disabled = crate::config::load_bundled_config().keymaps;
    disabled.groups.push(crate::config::KeyGroupEntry {
        id: "builtin.global.group.files.editor-normal".into(),
        context: None,
        mode: String::new(),
        prefix: None,
        label: None,
        icon: None,
        order: 0,
        disable: true,
    });
    disabled.entries.push(crate::config::KeymapEntry {
        id: "builtin.global.files-buffers.editor-normal".into(),
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
    let (disabled, diagnostic) = compile_keymaps(
        &app.commands,
        &disabled,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    assert!(
        !disabled
            .groups()
            .iter()
            .any(|group| group.id == "builtin.global.group.files.editor-normal")
    );
    let files = disabled
        .hints_for_prefix(
            "editor.normal",
            "space f",
            &app.configuration.settings().command_labels,
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
                prefix: Some("<leader> z".into()),
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
            &app.configuration.settings().input.keymaps,
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
        &app.configuration.settings().input.keymaps,
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
                id: "builtin.global.group.files.editor-normal".into(),
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
            &app.configuration.settings().input.keymaps,
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
        prefix: Some("<leader> z".into()),
        label: Some("Unused".into()),
        icon: None,
        order: 0,
        disable: false,
    });
    let (retained, diagnostic) = reload_keymaps(
        &app.commands,
        app.configuration.keymaps(),
        &invalid,
        &app.configuration.settings().input.keymaps,
    );
    assert!(
        diagnostic
            .as_deref()
            .is_some_and(|message| message.contains("keeping previous keymaps"))
    );
    assert_eq!(
        retained.groups(),
        app.configuration.keymaps().groups(),
        "rejected candidate must retain groups and maps atomically"
    );
}

#[test]
fn macros_record_replay_last_register_and_stop_at_limits() {
    let mut app = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abc"), None);
    app.set_dashboard_visibility(false);
    for key in ['q', 'a', 'x', 'q', '@', 'a', '@', '@'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "");
    assert_eq!(app.input.last_macro(), Some('a'));

    let recursive = crate::editor::CommandInvocation::new("macro.replay", InvocationSource::Macro)
        .expect("macro command")
        .with_argument("register", CommandValue::String("r".into()));
    app.state.registers.set_macro('r', vec![recursive]);
    app.replay_macro('r', 1);
    assert_eq!(app.input.replay_depth(), 0);
    assert!(app.ui.notification_error);
    assert!(
        app.ui
            .notification
            .as_deref()
            .is_some_and(|message| message.contains("recursion limit"))
    );
}

#[test]
fn question_mark_still_navigates_and_only_the_leader_opens_help() {
    let app = app();
    let keymaps = app.configuration.keymaps();

    // `?` alone keeps its command-line binding in the editor normal context.
    let plain = keymaps
        .inspect_key("editor.normal", "?")
        .expect("? resolves in the editor normal context");
    assert_eq!(
        plain.command.as_deref(),
        Some("command_line.open"),
        "? still opens the command line rather than help"
    );
    assert_ne!(
        plain.command.as_deref(),
        Some("overlay.help.contextual"),
        "? must not have been rebound to help"
    );

    // The leader sequence is what opens contextual help, and the leader is space.
    let leader = keymaps
        .inspect_key("editor.normal", "<leader> ?")
        .expect("the leader help binding resolves");
    assert_eq!(
        leader.command.as_deref(),
        Some("overlay.help.contextual"),
        "the leader sequence opens contextual help"
    );
    assert_eq!(
        leader.keys, "space ?",
        "the leader is space, so the sequence is literally `space ?`"
    );

    // And the two are different bindings, which is the whole point of the alias.
    assert_ne!(plain.command, leader.command);
}

#[test]
fn help_is_reachable_from_the_confirmation_and_from_a_form() {
    let app = app();
    let keymaps = app.configuration.keymaps();

    // The confirmation context: `?` is the action, and it is not an answer to the question.
    let confirmation = keymaps
        .inspect_key("confirmation_prompt", "?")
        .expect("? resolves in the confirmation context");
    assert_eq!(
        confirmation.command.as_deref(),
        Some("overlay.help.form"),
        "? in a confirmation opens help for the question"
    );

    // A form context: the commit composer is a form with no confirmation, and `ctrl-g` reaches
    // help there. A character key cannot be used in a text prompt because it would be typed.
    let form = keymaps
        .inspect_key("command_prompt", "ctrl-g")
        .expect("ctrl-g resolves in the command prompt context");
    assert_eq!(
        form.command.as_deref(),
        Some("overlay.help.form"),
        "a form reaches help without stealing a typing key"
    );
    assert_ne!(
        form.command.as_deref(),
        keymaps
            .inspect_key("command_prompt", "enter")
            .and_then(|enter| enter.command)
            .as_deref(),
        "the form's help key is not its submit key"
    );
}

#[test]
fn effective_bindings_report_remaps_unbound_commands_and_the_asked_context() {
    let app = app();
    let mut policy = app.configuration.settings().input.keymaps.clone();
    policy.leader = ",".into();
    policy.local_leader = ";".into();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.path = PathBuf::from("keymaps.toml");

    // The bundled defaults, read through the same resolved table the dispatcher uses.
    // `command_line.open` answers to MORE THAN ONE key, and the resolved table is a HashMap, so
    // the answer must be the whole sorted set: naming one arbitrary key would let a lesson teach
    // a different key from run to run.
    let (bundled, diagnostic) = compile_keymaps(&app.commands, &loaded, &policy);
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    let default = bundled.effective_binding("editor.normal", "command_line.open");
    assert_eq!(
        default.keys(),
        ["/".to_owned(), ":".to_owned(), "?".to_owned()],
        "every key that resolves is reported, in a stable order"
    );
    assert_eq!(default.primary(), Some("/"), "the primary key is stable");
    assert!(default.is_bound());
    assert_eq!(default.display(), "/ : ?  command_line.open");

    // A user remap: unmap `?` and bind a different key to the same command.
    loaded.entries.push(crate::config::KeymapEntry {
        id: "user.test.unmap-question".into(),
        context: Some("editor".into()),
        mode: "normal".into(),
        keys: Some("?".into()),
        command: None,
        arguments: BTreeMap::new(),
        unmap: true,
        disable: false,
        pass_through: false,
        desc: None,
    });
    loaded.entries.push(crate::config::KeymapEntry {
        id: "user.test.remap-command-line-open".into(),
        context: Some("editor".into()),
        mode: "normal".into(),
        keys: Some("ctrl-p".into()),
        command: Some("command_line.open".into()),
        arguments: BTreeMap::from([("kind".to_owned(), toml::Value::String("command".into()))]),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    });
    let (remapped, diagnostic) = compile_keymaps(&app.commands, &loaded, &policy);
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    let effective = remapped.effective_binding("editor.normal", "command_line.open");
    assert_eq!(
        effective.keys(),
        ["/".to_owned(), "ctrl-p".to_owned()],
        "the remapped key replaces the bundled binding for the same arguments, and the          unmapped key is gone"
    );
    assert!(
        !effective.keys().contains(&"?".to_owned()),
        "a key the user unmapped is never offered to a lesson"
    );
    assert_eq!(effective.display(), "/ ctrl-p  command_line.open");

    // A command with no binding in the asked context is reported as unbound, never defaulted.
    let unbound = remapped.effective_binding("editor.normal", "health.open");
    assert_eq!(
        unbound,
        crate::editor::session::configuration::EffectiveBinding::Unbound {
            command: "health.open".into(),
        },
        "a command with no binding here is UNBOUND rather than given a default"
    );
    assert_eq!(unbound.keys(), [] as [String; 0]);
    assert_eq!(unbound.primary(), None);
    assert!(!unbound.is_bound());
    assert_eq!(unbound.display(), "unbound  health.open");

    // The answer belongs to the context that was asked about: unmapping `?` in editor.normal
    // says nothing about a context where the same command is bound elsewhere.
    assert!(
        !remapped
            .effective_binding("editor.normal", "command_line.open")
            .keys()
            .contains(&"?".to_owned())
    );
}

#[test]
fn binding_for_names_the_same_key_every_time_for_a_multi_bound_command() {
    let app = app();
    let keymaps = app.configuration.keymaps();
    // `command_line.open` answers to several keys and `reverse` is a HashMap, so before this was
    // pinned the SHIPPED hint text could name a different key between runs.
    let shown = keymaps
        .binding_for("editor.normal", "command_line.open")
        .expect("the command resolves in the editor");
    assert_eq!(
        shown, "/",
        "the smallest matching key is named, not whichever the hash reached first"
    );
    for _ in 0..64 {
        assert_eq!(
            keymaps.binding_for("editor.normal", "command_line.open"),
            Some("/"),
            "the answer never varies between calls"
        );
    }
    // It agrees with the sorted-first key of the full set, so the two accessors cannot disagree.
    let effective = keymaps.effective_binding("editor.normal", "command_line.open");
    assert_eq!(effective.primary(), Some(shown));
}
