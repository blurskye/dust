use super::*;

#[test]
fn retained_plugin_window_projection_is_idle_and_generation_owned() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    let index = crate::extensions::index_manifest_contributions(
        &manifest,
        77,
        "manifest.toml:bytes 0..1".into(),
    )
    .expect("index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    let request = crate::extensions::PluginUiRequest {
        token: 1,
        kind: crate::extensions::PluginUiRequestKind::OpenWindow {
            expected_graph_revision: app.window_session.graph().revision().expect("revision"),
            content_type: "plugin.demo.panel".into(),
            title: "Demo".into(),
            placement: crate::extensions::PluginRequestedPlacement::Floating {
                x: 5,
                y: 3,
                width: 24,
                height: 7,
            },
            tree: Some(crate::extensions::PluginUiTree {
                revision: 1,
                root: 1,
                nodes: vec![crate::extensions::PluginUiNode {
                    id: 1,
                    kind: crate::extensions::PluginUiNodeKind::Text,
                    text: "retained plugin".into(),
                    style: crate::extensions::PluginUiStyle::Accent,
                    action: None,
                    children: Vec::new(),
                }],
            }),
        },
    };
    let crate::editor::session::extensions::WindowUpdate::Publish(source, transition) = app
        .extension
        .prepare_window(&index, request.clone(), &app.state, &app.window_session)
    else {
        panic!("expected staged window");
    };
    app.window_session.invalidate_native();
    let receipt = app.window_session.accept_plugin_transition(
        *transition,
        &app.state.window_version(),
        source.catalog(),
    );
    assert!(
        source
            .finish(receipt, app.window_session.graph().revision())
            .error
            .is_some()
    );
    assert!(app.extension.window_snapshots().is_empty());
    let crate::editor::session::extensions::WindowUpdate::Publish(source, transition) = app
        .extension
        .prepare_window(&index, request.clone(), &app.state, &app.window_session)
    else {
        panic!("expected fresh window proposal");
    };
    let (other, _) = app
        .window_session
        .begin_plugin(
            &index.owner,
            index.generation,
            (&app.state.window_version(), source.catalog()),
        )
        .unwrap()
        .seal();
    let receipt = app.window_session.accept_plugin_transition(
        other,
        &app.state.window_version(),
        source.catalog(),
    );
    assert!(
        source
            .finish(receipt, app.window_session.graph().revision())
            .error
            .is_some()
    );
    drop(transition);
    assert!(app.extension.window_snapshots().is_empty());
    let event = app.apply_plugin_ui_request(&index, request);
    assert!(event.error.is_none(), "{:?}", event.error);
    let window = app.extension.window_snapshots()[0].window;
    app.ui.plugin_surfaces = app.extension.window_snapshots();
    app.window_session.invalidate_native();
    app.reconcile_editor_windows();
    assert!(
        app.compositor_projection()
            .expect("projection")
            .window(window)
            .is_some()
    );
    let reconciliation_count = app.window_session.editor_ids().reconciliation_count();
    let graph = app.window_session.graph().clone();
    for _ in 0..256 {
        app.reconcile_editor_windows();
    }
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        reconciliation_count
    );
    assert_eq!(app.window_session.graph(), &graph);
    app.remove_plugin_contribution_generation("demo", PluginLoadId(77));
    assert!(app.ui.plugin_surfaces.is_empty());
    assert!(app.window_session.graph().window(window).is_none());
}

#[test]
fn plugin_host_events_are_snapshot_owned_coalesced_and_idle() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::extensions::PluginCapability::EventSubscriptions);
    manifest.contributions.subscriptions =
        vec![crate::extensions::PluginSubscriptionContribution {
            id: "plugin.demo.subscription.buffer".into(),
            kind: crate::extensions::PluginSubscriptionKind::BufferSummaryChanged,
            maximum_queued: 8,
            maximum_rate_per_second: 120,
            coalesce: true,
            provenance: None,
        }];
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 77, "event manifest".into())
            .expect("event index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    app.extension.install_events(&index).expect("event owner");
    app.refresh_plugin_host_events();
    assert_eq!(app.extension.events().pending("demo", PluginLoadId(77)), 1);
    let stable_queue = app.extension.events().clone();
    let stable_observation = app.extension.observation().clone();
    for _ in 0..256 {
        app.refresh_plugin_host_events();
    }
    assert_eq!(*app.extension.events(), stable_queue);
    assert_eq!(*app.extension.observation(), stable_observation);

    let view = app.state.active_view.expect("view");
    app.state.views[view].cursor = ByteOffset(1);
    app.refresh_plugin_host_events();
    let event = app
        .extension
        .deliver("demo", PluginLoadId(77))
        .pop()
        .expect("coalesced buffer event");
    assert_eq!(event.coalesced, 1);
    assert!(event.payload.contains("cursor=1"));
    app.remove_plugin_contribution_generation("demo", PluginLoadId(77));
    assert_eq!(app.extension.events().pending("demo", PluginLoadId(77)), 0);
}

#[test]
fn plugin_picker_provider_uses_the_generic_presenter_and_cancels_exactly() {
    let mut app = app();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::extensions::PluginCapability::PickerProviders);
    manifest.contributions.providers = vec![crate::extensions::PluginProviderContribution {
        id: "plugin.demo.provider.files".into(),
        kind: crate::extensions::PluginProviderKind::PickerSource,
        command: "plugin.demo.run".into(),
        maximum_concurrent_requests: 1,
        maximum_items: 16,
        maximum_bytes: 8_192,
        deadline_milliseconds: 30_000,
        provenance: None,
    }];
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 77, "provider manifest".into())
            .expect("provider index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    app.extension
        .fixture_provider_install(&index)
        .expect("provider owner");
    let request = app
        .extension
        .fixture_provider_start(crate::extensions::PluginProviderStart {
            plugin: "demo".into(),
            load: PluginLoadId(77),
            provider: "plugin.demo.provider.files".into(),
            query: "be".into(),
            context: "editor".into(),
            workspace_revision: Some(app.workspace.root_revision()),
            buffer_revision: None,
        })
        .expect("request");
    app.extension
        .provider_chunk(
            "demo",
            PluginLoadId(77),
            (Some(app.workspace.root_revision()), None),
            crate::extensions::PluginPickerChunk {
                request,
                sequence: 1,
                items: vec![
                    crate::extensions::PluginPickerItem {
                        id: "z".into(),
                        primary: "Zebra".into(),
                        secondary: None,
                        action_data: vec![1],
                    },
                    crate::extensions::PluginPickerItem {
                        id: "b".into(),
                        primary: "Beta".into(),
                        secondary: Some("ranked first".into()),
                        action_data: vec![2],
                    },
                ],
                terminal: crate::extensions::PluginProviderTerminal::Complete,
            },
        )
        .expect("chunk");
    app.extension.picker_installed(Some(request));
    app.present_plugin_picker(request);
    assert_eq!(app.search.picker_surface(), PickerSurfaceKind::Generic);
    assert_eq!(
        app.search
            .picker()
            .and_then(crate::workspace::PickerState::selected_item)
            .map(|item| item.label.as_str()),
        Some("Beta")
    );
    let stable = app.extension.provider_projection(request);
    let stable_ui = app.ui.picker.clone();
    for _ in 0..256 {
        assert_eq!(app.extension.provider_projection(request), stable);
        assert_eq!(app.ui.picker, stable_ui);
    }
    app.install_picker("Another picker".into(), Vec::new());
    let replacement_ui = app.ui.picker.clone();
    assert!(!app.extension.provider_visible(request));
    app.extension.picker_installed(Some(request)); // cancelled sources cannot reactivate
    app.present_plugin_picker(request);
    assert_eq!(app.ui.picker, replacement_ui);
    assert!(!app.extension.provider_visible(request));
    app.close_picker();
    assert_eq!(
        app.extension
            .provider_chunk(
                "demo",
                PluginLoadId(77),
                (Some(app.workspace.root_revision()), None),
                crate::extensions::PluginPickerChunk {
                    request,
                    sequence: 2,
                    items: Vec::new(),
                    terminal: crate::extensions::PluginProviderTerminal::Complete,
                },
            )
            .unwrap_err()
            .code,
        crate::extensions::PluginProviderErrorCode::Cancelled
    );
    // Reopening a one-request provider reserves new metadata without destroying
    // the displayed source if subsequent picker admission is refused.
    let mut broker = crate::extensions::PluginProviderBroker::default();
    broker.install(&index).unwrap();
    let start = || crate::extensions::PluginProviderStart {
        plugin: "demo".into(),
        load: PluginLoadId(77),
        provider: "plugin.demo.provider.files".into(),
        query: String::new(),
        context: "editor".into(),
        workspace_revision: None,
        buffer_revision: None,
    };
    let old = broker.start(start()).unwrap();
    let next = broker.reserve_replacing(start(), Some(old)).unwrap();
    assert!(broker.presentable(old) && broker.presentable(next));
    assert_eq!(
        broker
            .reserve_replacing(start(), Some(old))
            .unwrap_err()
            .code,
        crate::extensions::PluginProviderErrorCode::QueueFull
    );
    broker.cancel(next);
    assert!(broker.presentable(old));
    let next = broker.reserve_replacing(start(), Some(old)).unwrap();
    broker.cancel(old);
    assert!(broker.presentable(next));
}

#[test]
fn private_plugin_surface_geometry_snapshot_and_idle_projection_are_application_owned() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::extensions::PluginCapability::PrivateCells);
    let content = &mut manifest.contributions.content_types[0];
    content.presenter = crate::extensions::PluginPresenterKind::PrivateCells;
    content.private_cells = Some(crate::extensions::PluginPrivateCellPolicy {
        maximum_cells: 64,
        maximum_patch_cells: 32,
        maximum_dirty_rectangles: 8,
        maximum_styles: 8,
        maximum_hit_regions: 8,
        maximum_bytes: 4_096,
        cursor: true,
    });
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 77, "private manifest".into())
            .expect("index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    let revision = app.window_session.graph().revision().expect("revision");
    let opened = app.apply_plugin_ui_request(
        &index,
        crate::extensions::PluginUiRequest {
            token: 1,
            kind: crate::extensions::PluginUiRequestKind::OpenWindow {
                expected_graph_revision: revision,
                content_type: "plugin.demo.panel".into(),
                title: "Cells".into(),
                placement: crate::extensions::PluginRequestedPlacement::Floating {
                    x: 4,
                    y: 2,
                    width: 12,
                    height: 4,
                },
                tree: None,
            },
        },
    );
    assert!(opened.error.is_none(), "{:?}", opened.error);
    let handle = opened.handle.expect("handle");
    app.window_session.invalidate_native();
    app.reconcile_editor_windows();
    let geometry = app
        .extension
        .ui_events()
        .get(&("demo".into(), PluginLoadId(77)))
        .and_then(|events| events.iter().find_map(|event| event.geometry.clone()))
        .expect("local geometry event");
    assert_eq!((geometry.width, geometry.height), (12, 4));
    let replaced = app.apply_plugin_ui_request(
        &index,
        crate::extensions::PluginUiRequest {
            token: 2,
            kind: crate::extensions::PluginUiRequestKind::ReplacePrivate {
                handle,
                snapshot: crate::extensions::PluginPrivateSnapshot {
                    revision: 1,
                    dimensions_revision: geometry.dimensions_revision,
                    styles: vec![crate::extensions::PluginPrivateStyle {
                        id: 1,
                        style: crate::extensions::PluginUiStyle::Accent,
                    }],
                    cells: vec![crate::extensions::PluginPrivateCell {
                        x: 0,
                        y: 0,
                        grapheme: "X".into(),
                        continuation: false,
                        style: 1,
                    }],
                    cursor: Some(crate::extensions::PluginPrivateCursor { x: 0, y: 0 }),
                    hit_regions: vec![crate::extensions::PluginPrivateHitRegion {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                        action: "plugin.demo.run".into(),
                    }],
                },
            },
        },
    );
    assert!(replaced.error.is_none(), "{:?}", replaced.error);
    app.window_session.invalidate_native();
    app.reconcile_editor_windows();
    assert_eq!(app.ui.private_surfaces.len(), 1);
    assert_eq!(app.ui.private_surfaces[0].cells[0].grapheme, "X");
    let graph = app.window_session.graph().clone();
    let ui = app.ui.private_surfaces.clone();
    let reconciliations = app.window_session.editor_ids().reconciliation_count();
    for _ in 0..256 {
        app.reconcile_editor_windows();
    }
    assert_eq!(app.window_session.graph(), &graph);
    assert_eq!(app.ui.private_surfaces, ui);
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        reconciliations
    );
}

#[test]
fn retained_interaction_commands_use_host_focus_scroll_and_mouse_state() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 77, "manifest.toml".into())
            .expect("index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    let revision = app.window_session.graph().revision().expect("revision");
    let event = app.apply_plugin_ui_request(
        &index,
        crate::extensions::PluginUiRequest {
            token: 1,
            kind: crate::extensions::PluginUiRequestKind::OpenWindow {
                expected_graph_revision: revision,
                content_type: "plugin.demo.panel".into(),
                title: "Demo".into(),
                placement: crate::extensions::PluginRequestedPlacement::Floating {
                    x: 4,
                    y: 2,
                    width: 24,
                    height: 7,
                },
                tree: Some(crate::extensions::PluginUiTree {
                    revision: 1,
                    root: 1,
                    nodes: vec![
                        crate::extensions::PluginUiNode {
                            id: 1,
                            kind: crate::extensions::PluginUiNodeKind::Container,
                            text: String::new(),
                            style: crate::extensions::PluginUiStyle::Default,
                            action: None,
                            children: vec![2, 3],
                        },
                        crate::extensions::PluginUiNode {
                            id: 2,
                            kind: crate::extensions::PluginUiNodeKind::List,
                            text: "List".into(),
                            style: crate::extensions::PluginUiStyle::Default,
                            action: None,
                            children: Vec::new(),
                        },
                        crate::extensions::PluginUiNode {
                            id: 3,
                            kind: crate::extensions::PluginUiNodeKind::Button,
                            text: "Run".into(),
                            style: crate::extensions::PluginUiStyle::Accent,
                            action: Some("plugin.demo.run".into()),
                            children: Vec::new(),
                        },
                    ],
                }),
            },
        },
    );
    assert!(event.error.is_none(), "{:?}", event.error);
    app.ui.plugin_surfaces = app.extension.window_snapshots();
    assert_eq!(
        app.ui.plugin_surfaces[0]
            .rows
            .iter()
            .find(|row| row.focused)
            .map(|row| row.node),
        Some(2)
    );
    app.invoke_mapped_command("surface.retained.focus.next");
    assert_eq!(
        app.ui.plugin_surfaces[0]
            .rows
            .iter()
            .find(|row| row.focused)
            .map(|row| row.node),
        Some(3)
    );
    app.invoke_mapped_command("surface.retained.scroll.down");
    assert_eq!(app.ui.plugin_surfaces[0].scroll, 1);
    let window = app.ui.plugin_surfaces[0].window;
    app.window_session.invalidate_native();
    app.reconcile_editor_windows();
    let rect = app
        .compositor_projection()
        .and_then(|projection| projection.window(window).map(|window| window.rect))
        .expect("plugin rectangle");
    app.input.observe_mouse(rect.x, rect.y);
    app.invoke_mapped_command("surface.retained.mouse.select");
    assert_eq!(
        app.ui.plugin_surfaces[0]
            .rows
            .iter()
            .find(|row| row.focused)
            .map(|row| row.node),
        Some(3),
        "scroll-adjusted mouse row selects the visible button"
    );
}

#[test]
fn stale_generation_cleanup_preserves_replacement_ui_events() {
    let mut app = app();
    let event = |token| crate::extensions::PluginUiEvent {
        token,
        handle: None,
        graph_revision: None,
        tree_revision: None,
        surface_revision: None,
        geometry: None,
        action: None,
        error: None,
    };
    app.extension
        .queue_ui("demo".into(), PluginLoadId(77), [event(1)]);
    app.extension
        .queue_ui("demo".into(), PluginLoadId(78), [event(2)]);
    let storage_result = |load, token| crate::extensions::PluginStorageResult {
        plugin: "demo".into(),
        load: PluginLoadId(load),
        token,
        value: None,
        revision: None,
        entries: Vec::new(),
        truncated: false,
        error: None,
    };
    app.extension.queue_storage(storage_result(77, 1));
    app.extension.queue_storage(storage_result(78, 2));
    app.extension.install_storage("demo", PluginLoadId(78));
    let manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    let replacement = crate::extensions::index_manifest_contributions(
        &manifest,
        78,
        "replacement manifest".into(),
    )
    .expect("replacement index");
    app.extension
        .fixture_contribution(replacement)
        .expect("replacement catalog");
    app.remove_plugin_contribution_generation("demo", PluginLoadId(77));
    assert!(
        !app.extension
            .ui_events()
            .contains_key(&("demo".into(), PluginLoadId(77)))
    );
    assert_eq!(
        app.extension
            .ui_events()
            .get(&("demo".into(), PluginLoadId(78)))
            .and_then(|events| events.first())
            .map(|event| event.token),
        Some(2)
    );
    assert_eq!(
        app.extension
            .contributions()
            .get("demo")
            .map(|index| index.generation),
        Some(78)
    );
    assert!(
        !app.extension
            .storage_results()
            .contains_key(&("demo".into(), PluginLoadId(77)))
    );
    assert_eq!(
        app.extension
            .storage_results()
            .get(&("demo".into(), PluginLoadId(78)))
            .and_then(|results| results.first())
            .map(|result| result.token),
        Some(2)
    );
    app.extension
        .queue_ui("demo".into(), PluginLoadId(78), (10..80).map(event));
    assert_eq!(
        app.extension.ui_events()[&("demo".into(), PluginLoadId(78))].len(),
        64
    );
    assert!(
        app.extension
            .push_action("demo".into(), PluginLoadId(78), event(999))
            .is_err()
    );
    let delivered = app.extension.take_ui("demo", PluginLoadId(78));
    assert_eq!(delivered.len(), 64);
    assert_eq!(delivered.last().unwrap().token, 79);
    assert!(app.extension.take_ui("demo", PluginLoadId(78)).is_empty());
}

#[test]
fn expected_index_failure_before_sibling_index_aborts_the_complete_batch() {
    let mut app = app();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.deferred_plugin_keymaps = true;
        fixture.expected_plugin_indexes =
            ["bad".to_owned(), "good".to_owned()].into_iter().collect();
        app.configuration.install_fixture(fixture);
    }
    let bad = tempfile::tempdir().expect("bad plugin store");
    fs::write(bad.path().join("manifest.toml"), "not = [valid").expect("bad manifest");
    let good = tempfile::tempdir().expect("good plugin store");
    write_plugin_manifest(
        good.path(),
        &plugin_manifest("good", "plugin.good.keymap.dashboard", "x y"),
    );
    let limits = app.plugin_limits();
    let bad_load = app
        .extension
        .index_from_store(
            "bad".into(),
            bad.path().to_path_buf(),
            PathBuf::from("manifest.toml"),
            PathBuf::from("missing.wasm"),
            limits.clone(),
        )
        .expect("bad index request");
    let good_load = app
        .extension
        .index_from_store(
            "good".into(),
            good.path().to_path_buf(),
            PathBuf::from("manifest.toml"),
            PathBuf::from("missing.wasm"),
            limits,
        )
        .expect("good index request");
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.extension.load_id("good").is_some()
        || app.extension.load_id("bad").is_some()
        || !app.extension.pending_indexes().is_empty()
    {
        app.process_plugin_events();
        assert!(Instant::now() < deadline, "batch rejection timed out");
        thread::yield_now();
    }
    app.process_plugin_events();
    assert!(app.extension.contributions().get("bad").is_none());
    assert!(app.extension.contributions().get("good").is_none());
    assert!(
        app.configuration
            .keymaps()
            .inspect_id("plugin.good.keymap.dashboard")
            .is_none()
    );
    assert!(app.extension.pending_indexes().is_empty());
    assert!(!app.extension.is_active_generation("bad", bad_load));
    assert!(!app.extension.is_active_generation("good", good_load));
}

#[test]
fn deferred_user_plugin_move_compiles_after_index_then_falls_back_after_activation_failure() {
    let mut app = app();
    let root = tempfile::tempdir().expect("plugin store");
    write_plugin_manifest(
        root.path(),
        &plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y"),
    );
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.plugins.entries.insert(
        "demo".into(),
        crate::config::PluginEntryConfig {
            manifest: PathBuf::from("manifest.toml"),
            component: PathBuf::from("missing.wasm"),
            enabled: true,
        },
    );
    loaded.config.windows.rules = vec![crate::config::WindowRuleConfig {
        content_type: "plugin.demo.panel".into(),
        placement: crate::config::WindowPlacementConfig::Floating {
            x: 4,
            y: 3,
            width: 50,
            height: 12,
        },
    }];
    loaded.keymaps.entries.extend([
        crate::config::KeymapEntry {
            id: "plugin.demo.keymap.dashboard".into(),
            context: Some("dashboard".into()),
            mode: String::new(),
            keys: Some("z".into()),
            command: Some("plugin.demo.run".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
        },
        crate::config::KeymapEntry {
            id: "user.test.demo-retained-next".into(),
            context: Some("plugin.demo.chat".into()),
            mode: String::new(),
            keys: Some("j".into()),
            command: Some("surface.retained.focus.next".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
        },
    ]);
    let (configuration, diagnostic) =
        super::resolve_configuration(&app.commands, loaded, true).expect("deferred config");
    assert!(diagnostic.is_none());
    assert!(configuration.deferred_plugin_keymaps);
    assert!(configuration.settings.windows.rules.is_empty());
    assert_eq!(configuration.raw_windows.rules.len(), 1);
    app.configuration.install_fixture(configuration);
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
        assert!(Instant::now() < deadline, "static acceptance timed out");
        thread::yield_now();
    }
    let moved = app
        .configuration
        .keymaps()
        .inspect_id("plugin.demo.keymap.dashboard")
        .expect("user replacement");
    assert_eq!(moved.keys, "z");
    assert_eq!(moved.contribution_layer, InputContextLayer::User);
    assert_eq!(
        app.configuration
            .keymaps()
            .inspect_id("user.test.demo-retained-next")
            .map(|mapping| mapping.context),
        Some("plugin.demo.chat".into())
    );
    assert_eq!(
        app.configuration.settings().windows.rules[0].content_type,
        "plugin.demo.panel"
    );
    assert!(
        app.configuration
            .keymaps()
            .inspect_key("dashboard", "x y")
            .is_none()
    );
    while app.extension.contributions().get("demo").is_some() {
        app.process_plugin_events();
        assert!(Instant::now() < deadline, "activation rollback timed out");
        thread::yield_now();
    }
    assert!(!app.extension.is_active_generation("demo", load));
    assert!(
        app.configuration
            .keymaps()
            .inspect_id("plugin.demo.keymap.dashboard")
            .is_none()
    );
    assert!(
        app.configuration
            .keymaps()
            .inspect_id("user.test.demo-retained-next")
            .is_none()
    );
    assert!(app.configuration.settings().windows.rules.is_empty());
}

#[test]
fn enabled_plugin_references_are_deferred_but_unknown_namespaces_still_reject() {
    let app = app();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.plugins.entries.insert(
        "demo".into(),
        crate::config::PluginEntryConfig {
            manifest: PathBuf::from("demo.toml"),
            component: PathBuf::from("demo.wasm"),
            enabled: true,
        },
    );
    loaded
        .config
        .windows
        .rules
        .push(crate::config::WindowRuleConfig {
            content_type: "plugin.demo.panel".into(),
            placement: crate::config::WindowPlacementConfig::Automatic,
        });
    loaded.keymaps.entries.extend([
        crate::config::KeymapEntry {
            id: "plugin.demo.keymap.default".into(),
            context: None,
            mode: String::new(),
            keys: None,
            command: None,
            arguments: BTreeMap::new(),
            unmap: false,
            disable: true,
            pass_through: false,
        },
        crate::config::KeymapEntry {
            id: "user.test.demo-command".into(),
            context: Some("dashboard".into()),
            mode: String::new(),
            keys: Some("z".into()),
            command: Some("plugin.demo.run".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
        },
        crate::config::KeymapEntry {
            id: "user.test.demo-retained-context".into(),
            context: Some("plugin.demo.chat".into()),
            mode: String::new(),
            keys: Some("j".into()),
            command: Some("surface.retained.focus.next".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
        },
    ]);
    let (resolved, diagnostic) = super::resolve_configuration(&app.commands, loaded, true)
        .expect("deferred plugin references");
    assert!(diagnostic.is_none());
    assert!(resolved.deferred_plugin_keymaps);
    assert!(resolved.expected_plugin_indexes.contains("demo"));
    assert!(
        resolved
            .settings
            .windows
            .rules
            .iter()
            .all(|rule| !rule.content_type.starts_with("plugin."))
    );
    assert!(
        resolved
            .raw_windows
            .rules
            .iter()
            .any(|rule| rule.content_type == "plugin.demo.panel")
    );
    assert!(
        resolved
            .keymaps
            .inspect_id("plugin.demo.keymap.default")
            .is_none()
    );
    assert!(
        resolved
            .keymaps
            .inspect_id("user.test.demo-command")
            .is_none()
    );

    let mut unknown = crate::config::load_bundled_config();
    unknown.keymaps.entries.push(crate::config::KeymapEntry {
        id: "user.test.unknown-plugin".into(),
        context: Some("dashboard".into()),
        mode: String::new(),
        keys: Some("z".into()),
        command: Some("plugin.unknown.run".into()),
        arguments: BTreeMap::new(),
        unmap: false,
        disable: false,
        pass_through: false,
    });
    assert!(super::resolve_configuration(&app.commands, unknown, true).is_err());
    let mut unknown_window = crate::config::load_bundled_config();
    unknown_window
        .config
        .windows
        .rules
        .push(crate::config::WindowRuleConfig {
            content_type: "plugin.unknown.panel".into(),
            placement: crate::config::WindowPlacementConfig::Hidden,
        });
    assert!(super::resolve_configuration(&app.commands, unknown_window, true).is_err());

    let mut running = self::app();
    let previous = running.configuration.settings().plugins.clone();
    let mut changed = crate::config::load_bundled_config();
    changed.config.plugins.entries.insert(
        "demo".into(),
        crate::config::PluginEntryConfig {
            manifest: PathBuf::from("demo.toml"),
            component: PathBuf::from("demo.wasm"),
            enabled: true,
        },
    );
    let error = running
        .apply_configuration_candidate(changed, false)
        .expect_err("plugin topology reload must stage before mutation");
    assert!(error.contains("plugin topology"));
    assert_eq!(running.configuration.settings().plugins, previous);
}

#[test]
fn deferred_batch_abort_and_worker_loss_clear_static_ownership() {
    fn index(owner: &str, generation: u64) -> crate::extensions::PluginContributionIndex {
        crate::extensions::PluginContributionIndex {
            owner: owner.into(),
            version: "1.0.0".into(),
            checksum: "0".repeat(64),
            generation,
            source: format!("test:{owner}"),
            commands: BTreeMap::new(),
            contexts: Vec::new(),
            keymaps: Vec::new(),
            key_groups: Vec::new(),
            content_types: Vec::new(),
            subscriptions: Vec::new(),
            providers: Vec::new(),
            virtual_documents: Vec::new(),
            storage: None,
        }
    }

    let mut app = app();
    for (owner, load) in [("one", PluginLoadId(41)), ("two", PluginLoadId(42))] {
        app.extension
            .fixture_contribution(index(owner, load.0))
            .expect("provisional index");
        app.extension
            .fixture_pending_index(owner.into(), (load, "0".repeat(64)));
    }
    app.abort_pending_index_batch("expected index failed");
    assert!(app.extension.pending_indexes().is_empty());
    assert!(app.extension.contributions().get("one").is_none());
    assert!(app.extension.contributions().get("two").is_none());
    assert!(
        app.extension
            .rejected_indexes()
            .contains(&("one".into(), PluginLoadId(41)))
    );
    assert!(
        app.extension
            .rejected_indexes()
            .contains(&("two".into(), PluginLoadId(42)))
    );

    app.extension
        .fixture_contribution(index("three", 43))
        .expect("accepted index");
    app.extension
        .fixture_pending_index("three".into(), (PluginLoadId(43), "0".repeat(64)));
    app.handle_plugin_worker_stopped(crate::extensions::PluginError::Stopped);
    assert!(app.extension.pending_indexes().is_empty());
    assert!(app.extension.rejected_indexes().is_empty());
    assert!(app.extension.contributions().get("three").is_none());
    assert!(
        app.commands
            .specs()
            .all(|spec| !matches!(spec.source, CommandSource::Plugin(_)))
    );
}

#[test]
fn virtual_publication_keeps_original_request_until_checked_window_acceptance() {
    use crate::editor::session::extensions::VirtualContext;
    let mut app = app();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::extensions::PluginCapability::VirtualDocuments);
    manifest.contributions.virtual_documents =
        vec![crate::extensions::PluginVirtualDocumentContribution {
            id: "plugin.demo.virtual.help".into(),
            title: "Help".into(),
            language: "markdown".into(),
            mode: crate::extensions::PluginVirtualDocumentMode::ReadOnly,
            maximum_concurrent_documents: 1,
            maximum_document_bytes: 4096,
            maximum_document_lines: 128,
            maximum_patch_bytes: 256,
            maximum_patches_per_request: 8,
            deadline_milliseconds: 30_000,
            command: "plugin.demo.run".into(),
            context: "dashboard".into(),
            provenance: None,
        }];
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 7, "virtual fixture".into())
            .unwrap();
    app.extension.fixture_contribution(index.clone()).unwrap();
    let request = app.extension.fixture_virtual_request(&index);
    let proposal = crate::extensions::PluginVirtualDocumentProposal {
        request: request.request,
        document: request.document,
        cancellation_generation: request.cancellation_generation,
        kind: crate::extensions::PluginVirtualDocumentProposalKind::Full(
            crate::extensions::PluginVirtualDocumentFull {
                expected_revision: None,
                revision: 1,
                title: "Help".into(),
                language: "markdown".into(),
                text: "# Original source\n".into(),
            },
        ),
    };
    let before = app.state.version();
    assert!(
        app.extension
            .prepare_virtual(
                "other",
                PluginLoadId(7),
                proposal.clone(),
                VirtualContext {
                    editor: &app.state,
                    windows: &app.window_session,
                    scratch: "scratch"
                }
            )
            .is_err()
    );
    let (source, candidate) = app
        .extension
        .prepare_virtual(
            "demo",
            PluginLoadId(7),
            proposal.clone(),
            VirtualContext {
                editor: &app.state,
                windows: &app.window_session,
                scratch: "scratch",
            },
        )
        .unwrap();
    // Retiring the window candidate cannot consume the original broker request.
    app.window_session.invalidate_native();
    assert!(
        app.window_session
            .accept_editor_transition(candidate, source.before(), source.catalog())
            .is_err()
    );
    drop(source);
    assert_eq!(app.state.version(), before);
    let (source, candidate) = app
        .extension
        .prepare_virtual(
            "demo",
            PluginLoadId(7),
            proposal.clone(),
            VirtualContext {
                editor: &app.state,
                windows: &app.window_session,
                scratch: "scratch",
            },
        )
        .unwrap();
    let accepted = app
        .window_session
        .accept_editor_transition(candidate, source.before(), source.catalog())
        .unwrap();
    app.state = source.finish(accepted).unwrap();
    let virtual_buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(
        app.state.buffers[virtual_buffer].text.to_string(),
        "# Original source\n"
    );
    assert!(
        app.extension
            .prepare_virtual(
                "demo",
                PluginLoadId(7),
                proposal,
                VirtualContext {
                    editor: &app.state,
                    windows: &app.window_session,
                    scratch: "scratch"
                }
            )
            .is_err()
    );
    let (ordinary, view) = app.state.open_text(TextBuffer::from_text("ordinary"), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let focus = app.window_session.graph().focused();
    app.remove_plugin_contribution_generation("demo", PluginLoadId(7));
    assert!(!app.state.buffers.contains_key(virtual_buffer));
    assert_eq!(app.state.active_buffer_id(), Some(ordinary));
    assert_eq!(app.state.active_view, Some(view));
    assert_eq!(app.window_session.graph().focused(), focus);
    assert_eq!(app.state.buffers[ordinary].text.to_string(), "ordinary");
}
