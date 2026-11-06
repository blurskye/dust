use crate::app::tests::*;
use crate::editor::EditorOperation;
use crate::editor::EditorTransaction;
use crate::editor::TransactionId;
use crate::ui::render::overlays::SCRATCH_ICON;
use crate::ui::render::overlays::VIRTUAL_ICON;




#[test]
pub(in crate::app) fn health_virtual_dogfood_opens_refreshes_and_remains_read_only() {
    let (_app_dirs, mut app) = app();
    app.process_effect(AppEffect::HealthVirtualOpen);
    let buffer = app.state.active_buffer_id().expect("health buffer");
    assert!(
        app.state.buffers[buffer]
            .text
            .to_string()
            .contains("# Dust health")
    );
    assert!(app.state.buffers[buffer].is_virtual_read_only());
    app.reconcile_editor_windows();
    assert!(
        app.ui
            .content_tabs
            .iter()
            .any(|entry| entry.icon == VIRTUAL_ICON
                && entry.closeable
                && entry.kind == ContentTabKind::VirtualDocument)
    );
    let virtual_content = app
        .window_session
        .graph()
        .content_for_window(
            app.window_session
                .graph()
                .focused()
                .expect("virtual window"),
        )
        .expect("virtual content");
    assert_eq!(
        virtual_content.adapter,
        crate::windows::ContentAdapterKind::VirtualDocument
    );
    assert!(virtual_content.capabilities.persist_descriptor);
    let persisted = app.capture_session().expect("virtual descriptor session");
    assert!(persisted.tabs.is_empty());
    assert_eq!(persisted.content_descriptors.len(), 1);
    let descriptor = &persisted.content_descriptors[0];
    assert_eq!(descriptor.semantic_id, "virtual:0");
    assert_eq!(descriptor.content_type, "builtin.virtual-document");
    assert_eq!(descriptor.adapter, "virtual-document");
    assert_eq!(descriptor.provider.as_deref(), Some("builtin.health"));
    assert_eq!(descriptor.source.as_deref(), Some("builtin.health.virtual"));
    assert_eq!(descriptor.key.as_deref(), Some("health"));
    assert_eq!(
        persisted.unavailable_virtual_descriptors(),
        std::collections::BTreeSet::from(["builtin.health:builtin.health.virtual:health".into()])
    );
    let encoded = serde_json::to_value(descriptor).expect("virtual descriptor JSON");
    for forbidden in [
        "generation",
        "load",
        "revision",
        "callback",
        "cells",
        "text",
    ] {
        assert!(encoded.get(forbidden).is_none(), "serialized {forbidden}");
    }
    app.report_unavailable_persisted_content(&persisted);
    assert!(app.ui.config_diagnostic.as_deref().is_some_and(|message| {
        message.contains("production API 0.2")
            && message.contains("builtin.health:builtin.health.virtual:health")
            && message.contains("continuing without")
    }));
    let mut unavailable_restore = persisted.clone();
    unavailable_restore
        .window_rules
        .push(crate::persistence::PersistedWindowRule {
            content_type: "builtin.terminal".into(),
            placement: crate::persistence::PersistedWindowPlacement::Floating {
                x: 7,
                y: 5,
                width: 40,
                height: 9,
            },
        });
    app.persistence
        .fixture_restore_source(crate::app::persistence::RestoreContext {
            editor: &app.state,
            workspace: &app.workspace,
            input: &app.input,
            windows: &app.window_session,
            configuration: app.configuration.binding_revision(),
        });
    app.restore_session(unavailable_restore);
    assert!(app.terminal_session.descriptions().is_empty());
    assert!(app.window_session.native_ids().terminal(1).is_none());
    assert!(
        app.window_session
            .native_ids()
            .semantic_defaults(None)
            .contains(&(
                "builtin.terminal",
                WindowPlacement::Floating(WindowRect {
                    x: 7,
                    y: 5,
                    width: 40,
                    height: 9,
                })
            ))
    );
    let ids = (
        buffer,
        app.state.active_view,
        app.window_session.graph().focused(),
    );
    app.process_effect(AppEffect::HealthVirtualRefresh);
    assert_eq!(
        (
            app.state.active_buffer_id().unwrap(),
            app.state.active_view,
            app.window_session.graph().focused()
        ),
        ids
    );
    let revision = app.state.buffers[buffer].text.revision();
    let transaction = crate::editor::EditorTransaction::new(crate::editor::TransactionId(999))
        .with_operation(crate::editor::EditorOperation::ApplyEdits {
            buffer,
            edits: crate::editor::text::TextEditSet::new(
                revision,
                vec![crate::editor::text::TextEdit::new(
                    crate::editor::text::ByteRange::new(
                        crate::editor::text::ByteOffset(0),
                        crate::editor::text::ByteOffset(0),
                    )
                    .unwrap(),
                    "x",
                )],
            ),
        });
    assert!(matches!(
        app.state.apply_transaction(&transaction),
        Err(crate::editor::TransactionError::ReadOnly)
    ));
    for _ in 0..256 {
        app.reconcile_editor_windows();
        assert_eq!(app.window_session.graph().focused(), ids.2);
    }
    app.remove_plugin_contribution_generation("builtin.health", PluginLoadId(1));
    assert!(!app.state.buffers.contains_key(buffer));
    assert!(app.state.active_view.is_none());
    assert!(app.window_session.dashboard_visible());
    app.window_session.graph().validate().unwrap();
}


#[test]
pub(in crate::app) fn accepted_virtual_snapshot_publishes_through_the_tiled_editor_bridge() {
    let (_app_dirs, mut app) = app();
    let snapshot = crate::plugins::PluginVirtualDocumentSnapshot {
        plugin: "demo".into(),
        load: PluginLoadId(7),
        source: "plugin.demo.virtual.help".into(),
        document: 9,
        key: "help".into(),
        title: "Help".into(),
        language: "markdown".into(),
        text: "# Héllo\nvirtual".into(),
        revision: 1,
        closed: false,
    };
    let mut draft = app.state.draft();
    let buffer = app
        .extension
        .fixture_virtual_publish(&mut draft, &snapshot)
        .expect("publish");
    *app.state = draft.accept(&app.state.version()).unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    assert!(
        app.window_session
            .editor_ids()
            .content_for(buffer)
            .is_some()
    );
    assert!(app.window_session.graph().focused().is_some());
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "# Héllo\nvirtual"
    );
    let ids = (app.state.active_view, app.window_session.graph().focused());
    let mut refresh = snapshot.clone();
    refresh.revision = 2;
    refresh.text = "# Héllo\nrefreshed".into();
    let mut draft = app.state.draft();
    assert_eq!(
        app.extension
            .fixture_virtual_publish(&mut draft, &refresh)
            .unwrap(),
        buffer
    );
    *app.state = draft.accept(&app.state.version()).unwrap();
    app.reconcile_editor_windows();
    assert_eq!(app.state.buffers[buffer].text.to_string(), refresh.text);
    assert_eq!(
        app.state.buffers[buffer].origin,
        crate::editor::BufferOrigin::VirtualReadOnly {
            owner: refresh.plugin.clone(),
            generation: refresh.load.0,
            document: refresh.document,
            revision: refresh.revision,
        }
    );
    assert_eq!(
        (app.state.active_view, app.window_session.graph().focused()),
        ids
    );
    assert!(matches!(
        app.window_session
            .graph()
            .window(app.window_session.graph().focused().unwrap())
            .unwrap()
            .placement,
        WindowPlacement::Tiled
    ));
}


#[test]
pub(in crate::app) fn universal_window_clone_uses_editor_owned_independent_text_and_virtual_views() {
    let (_app_dirs, mut app) = app();
    let (buffer, original_view) = app
        .state
        .open_text(TextBuffer::from_text("αbc\nline"), None)
        .unwrap();
    {
        let view = &mut app.state.views[original_view];
        view.cursor = crate::editor::text::ByteOffset(2);
        view.selection = Some(crate::editor::Selection {
            anchor: crate::editor::text::ByteOffset(0),
            head: crate::editor::text::ByteOffset(2),
            kind: crate::editor::SelectionKind::Character,
        });
        view.scroll_line = 1;
        view.scroll_column = 3;
        view.mode = crate::editor::EditorMode::Insert;
    }
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let original_window = app.window_session.graph().focused().expect("text window");
    let original_content = app
        .window_session
        .graph()
        .content_for_window(original_window)
        .expect("text content")
        .id;
    let before_counts = app.window_session.graph().counts();

    let invocation = crate::editor::CommandInvocation::new(
        "window.clone",
        crate::editor::InvocationSource::Test,
    )
    .expect("clone command")
    .with_argument(
        "target",
        crate::editor::CommandValue::String(format!("window:{}", original_window.0)),
    )
    .with_argument(
        "axis",
        crate::editor::CommandValue::String("vertical".into()),
    );
    let outcome = app
        .commands
        .execute(&app.state, &invocation)
        .expect("typed clone command");
    let [effect] = outcome.effects.as_slice() else {
        panic!("clone command must emit one effect");
    };
    assert!(matches!(
        effect,
        AppEffect::WindowClone {
            target,
            axis: crate::editor::Axis::Vertical,
        } if target == &format!("window:{}", original_window.0)
    ));
    app.process_effect(outcome.effects.into_iter().next().expect("clone effect"));

    let cloned_view = app.state.active_view.expect("cloned editor view");
    let cloned_window = app.window_session.graph().focused().expect("cloned window");
    assert_ne!(cloned_view, original_view);
    assert_ne!(cloned_window, original_window);
    assert_eq!(app.state.views[cloned_view].buffer, buffer);
    assert_eq!(
        app.window_session
            .graph()
            .content_for_window(cloned_window)
            .expect("cloned text content")
            .id,
        original_content
    );
    assert_eq!(
        app.window_session.graph().counts(),
        (
            before_counts.0,
            before_counts.1 + 1,
            before_counts.2 + 1,
            before_counts.3,
        )
    );
    assert_eq!(
        app.state.views[cloned_view].cursor,
        app.state.views[original_view].cursor
    );
    assert_eq!(
        app.state.views[cloned_view].selection,
        app.state.views[original_view].selection
    );
    assert_eq!(app.state.views[cloned_view].scroll_line, 1);
    assert_eq!(app.state.views[cloned_view].scroll_column, 3);
    assert_eq!(
        app.state.views[cloned_view].mode,
        crate::editor::EditorMode::Insert
    );

    let transaction = crate::editor::EditorTransaction::new(crate::editor::TransactionId(999))
        .with_operation(crate::editor::EditorOperation::SetCursor {
            view: cloned_view,
            position: crate::editor::text::ByteOffset(3),
        })
        .with_operation(crate::editor::EditorOperation::SetScroll {
            view: cloned_view,
            first_line: 0,
        })
        .with_operation(crate::editor::EditorOperation::SetScrollColumn {
            view: cloned_view,
            first_column: 0,
        })
        .with_operation(crate::editor::EditorOperation::SetSelection {
            view: cloned_view,
            selection: None,
        })
        .with_operation(crate::editor::EditorOperation::SetMode {
            view: cloned_view,
            mode: crate::editor::EditorMode::Normal,
        });
    app.apply_authoritative_editor_transaction(&transaction)
        .expect("update only cloned view");
    assert_eq!(
        app.state.views[original_view].cursor,
        crate::editor::text::ByteOffset(2)
    );
    assert_eq!(app.state.views[original_view].scroll_line, 1);
    assert_eq!(app.state.views[original_view].scroll_column, 3);
    assert!(app.state.views[original_view].selection.is_some());
    assert_eq!(
        app.state.views[original_view].mode,
        crate::editor::EditorMode::Insert
    );

    app.process_effect(AppEffect::WindowClone {
        target: format!("window:{}", original_window.0),
        axis: crate::editor::Axis::Horizontal,
    });
    let exact_clone = app.state.active_view.expect("exact source clone");
    assert_ne!(exact_clone, cloned_view);
    assert_eq!(app.state.views[exact_clone].buffer, buffer);
    assert_eq!(
        app.state.views[exact_clone].cursor,
        app.state.views[original_view].cursor
    );
    assert_eq!(
        app.state.views[exact_clone].selection,
        app.state.views[original_view].selection
    );
    assert_eq!(
        app.state.views[exact_clone].scroll_line,
        app.state.views[original_view].scroll_line
    );
    assert_eq!(
        app.state.views[exact_clone].scroll_column,
        app.state.views[original_view].scroll_column
    );
    assert_eq!(
        app.state.views[exact_clone].mode,
        app.state.views[original_view].mode
    );
    assert_ne!(
        app.state.views[exact_clone].cursor,
        app.state.views[cloned_view].cursor
    );
    assert_ne!(
        app.state.views[exact_clone].selection,
        app.state.views[cloned_view].selection
    );

    let (_virtual_app_dirs, mut virtual_app) = crate::app::tests::app();
    virtual_app.process_effect(AppEffect::HealthVirtualOpen);
    let virtual_buffer = virtual_app.state.active_buffer_id().expect("health buffer");
    let virtual_view = virtual_app.state.active_view.expect("health view");
    virtual_app.reconcile_editor_windows();
    let virtual_window = virtual_app
        .window_session
        .graph()
        .focused()
        .expect("health window");
    let virtual_content = virtual_app
        .window_session
        .graph()
        .content_for_window(virtual_window)
        .expect("health content")
        .id;
    virtual_app.process_effect(AppEffect::WindowClone {
        target: format!("window:{}", virtual_window.0),
        axis: crate::editor::Axis::Horizontal,
    });
    let cloned_virtual_view = virtual_app.state.active_view.expect("cloned health view");
    let cloned_virtual_window = virtual_app
        .window_session
        .graph()
        .focused()
        .expect("cloned health window");
    assert_ne!(cloned_virtual_view, virtual_view);
    assert_eq!(
        virtual_app.state.views[cloned_virtual_view].buffer,
        virtual_buffer
    );
    assert!(virtual_app.state.buffers[virtual_buffer].is_virtual_read_only());
    assert_eq!(
        virtual_app
            .window_session
            .graph()
            .content_for_window(cloned_virtual_window)
            .expect("cloned health content")
            .id,
        virtual_content
    );
}


#[test]
pub(in crate::app) fn virtual_read_only_origin_never_queues_save_reload_or_recovery_io() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("virtual"), None)
        .unwrap();
    app.state.buffers[buffer].read_only = true;
    app.state.buffers[buffer].origin = crate::editor::BufferOrigin::VirtualReadOnly {
        owner: "demo".into(),
        generation: 7,
        document: 9,
        revision: 1,
    };
    let path = app.state.workspace_root.join("forbidden");
    fs::write(&path, b"unchanged").unwrap();
    app.state.buffers[buffer].path = Some(path.clone());
    let before = app.documents.pending_count();
    app.start_save(false, Some(path.clone()), false);
    assert_eq!(app.documents.pending_count(), before);
    app.start_reload(true);
    assert_eq!(app.documents.pending_count(), before);
    wait_vfs(&mut app);
    assert_eq!(fs::read(&path).unwrap(), b"unchanged");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "virtual");
    app.state.buffers[buffer].dirty = true;
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.recovery.enabled = true;
        app.configuration.install_fixture(fixture);
    }
    app.recovery.skip_maintenance();
    app.schedule_recovery_work();
    assert!(!app.recovery.scheduled().contains_key(&buffer));
    app.queue_dirty_recovery_before_exit();
    assert!(!app.recovery.writing(None));
    assert!(app.capture_session().is_none());
}


#[test]
pub(in crate::app) fn content_line_uses_filename_icons_for_extensionless_files() {
    let (_app_dirs, mut app) = app();
    for name in ["Dockerfile", "Makefile", "README", ".gitignore"] {
        app.state
            .open_text(
                TextBuffer::from_text("file"),
                Some(PathBuf::from("/workspace").join(name)),
            )
            .unwrap();
    }
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    for name in ["Dockerfile", "Makefile", "README", ".gitignore"] {
        let entry = app
            .ui
            .content_tabs
            .iter()
            .find(|entry| entry.title == name)
            .unwrap();
        assert_eq!(entry.icon, crate::files::file_icon(name).unwrap().glyph);
        assert_ne!(entry.icon, SCRATCH_ICON);
    }
}


#[test]
pub(in crate::app) fn text_and_virtual_windows_project_input_and_preserve_identity_across_ordinary_placements() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((80, 24));
    app.state
        .open_text(
            TextBuffer::from_text("floating editor contents"),
            Some(PathBuf::from("/workspace/floating.rs")),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    let tab = app.state.active_tab.expect("editor tab");
    let source_pane = app.state.tabs[tab].focused;
    let source_view = app.state.panes[source_pane].view;
    app.process_effect(AppEffect::Layout(crate::app::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    )));
    let source_window = app
        .window_session
        .editor_ids()
        .window_for(source_pane)
        .expect("source window");
    let source_content = app
        .window_session
        .graph()
        .content_for_window(source_window)
        .expect("source content")
        .id;

    app.process_effect(AppEffect::WindowFloat {
        target: format!("window:{}", source_window.0),
        x: 7,
        y: 4,
        width: 31,
        height: 9,
    });
    let floating = app
        .compositor_projection()
        .and_then(|projection| projection.window(source_window))
        .expect("floating editor projection");
    assert_eq!(floating.layer, crate::windows::CompositorLayer::Floating);
    assert_eq!(
        floating.rect,
        WindowRect {
            x: 7,
            y: 4,
            width: 31,
            height: 9,
        }
    );
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        80,
        24,
    );
    assert!(
        (4..13).any(|row| (7..38).any(|column| {
            frame
                .grid
                .get(column, row)
                .is_some_and(|cell| cell.symbol == "f")
        })),
        "floating editor content is painted in its projected rectangle"
    );
    app.input.observe_mouse(12, 5);
    app.focus_mouse_pane();
    assert_eq!(app.state.tabs[tab].focused, source_pane);
    assert_eq!(app.state.active_view, Some(source_view));

    app.process_effect(AppEffect::WindowMove {
        target: format!("window:{}", source_window.0),
        x: 11,
        y: 6,
    });
    app.process_effect(AppEffect::WindowResize {
        target: format!("window:{}", source_window.0),
        amount: 3,
    });
    let moved_source = WindowPlacement::Floating(WindowRect {
        x: 11,
        y: 6,
        width: 34,
        height: 12,
    });
    assert_eq!(
        app.window_session
            .graph()
            .window(source_window)
            .expect("moved editor")
            .placement,
        moved_source
    );
    let other_pane = app
        .state
        .panes
        .keys()
        .find(|pane| *pane != source_pane)
        .expect("other pane");
    let other_window = app
        .window_session
        .editor_ids()
        .window_for(other_pane)
        .expect("other editor window");
    let other_placement = WindowPlacement::Floating(WindowRect {
        x: 48,
        y: 2,
        width: 25,
        height: 7,
    });
    app.place_graph_window(&format!("window:{}", other_window.0), other_placement)
        .expect("float second editor");
    app.swap_graph_windows(
        &format!("window:{}", source_window.0),
        &format!("window:{}", other_window.0),
    )
    .expect("swap non-tiled editor placements");
    assert_eq!(
        app.window_session
            .graph()
            .window(source_window)
            .expect("swapped source")
            .placement,
        other_placement
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(other_window)
            .expect("swapped other")
            .placement,
        moved_source
    );

    for placement in [
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 18,
        },
        WindowPlacement::Fullscreen,
        WindowPlacement::Hidden,
    ] {
        app.place_graph_window(&format!("window:{}", source_window.0), placement)
            .expect("supported editor placement");
        assert_eq!(
            app.window_session
                .graph()
                .window(source_window)
                .expect("same editor window")
                .placement,
            placement
        );
        assert_eq!(
            app.window_session
                .graph()
                .content_for_window(source_window)
                .expect("same content")
                .id,
            source_content
        );
        assert_eq!(
            app.window_session.editor_ids().window_for(source_pane),
            Some(source_window)
        );
        if placement == WindowPlacement::Hidden {
            assert_ne!(app.state.tabs[tab].focused, source_pane);
            assert_eq!(
                app.window_session.graph().focused(),
                app.window_session
                    .editor_ids()
                    .window_for(app.state.tabs[tab].focused)
            );
        }
    }
    app.show_graph_window(&format!("window:{}", source_window.0))
        .expect("show hidden editor");
    assert_eq!(app.state.active_view, Some(source_view));
    assert_eq!(app.state.tabs[tab].focused, source_pane);
    assert_eq!(app.window_session.graph().focused(), Some(source_window));
    assert_eq!(
        app.window_session
            .graph()
            .window(source_window)
            .expect("restored editor")
            .placement,
        WindowPlacement::Fullscreen
    );
    app.place_graph_window(
        &format!("window:{}", source_window.0),
        WindowPlacement::Tiled,
    )
    .expect("return editor to exact logical tile");
    assert!(
        app.window_session
            .graph()
            .tiled_layout_snapshot(app.window_session.graph().active_tab().expect("active tab"))
            .expect("restored tiled layout")
            .project(
                WindowRect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 24,
                },
                1,
                1,
            )
            .windows
            .iter()
            .any(|window| window.window == source_window)
    );
    app.close_graph_window(&format!("window:{}", other_window.0))
        .expect("close exact non-tiled editor view");
    assert!(app.window_session.graph().window(other_window).is_none());
    assert_eq!(
        app.window_session
            .graph()
            .content_for_window(source_window)
            .expect("shared text content survives view close")
            .id,
        source_content
    );

    let (_virtual_app_dirs, mut virtual_app) = crate::app::tests::app();
    virtual_app.process_effect(AppEffect::HealthVirtualOpen);
    virtual_app.reconcile_editor_windows();
    let virtual_window = virtual_app
        .window_session
        .graph()
        .focused()
        .expect("virtual window");
    let virtual_content = virtual_app
        .window_session
        .graph()
        .content_for_window(virtual_window)
        .expect("virtual content")
        .clone();
    assert_eq!(
        virtual_content.adapter,
        crate::windows::ContentAdapterKind::VirtualDocument
    );
    virtual_app
        .place_graph_window(
            &format!("window:{}", virtual_window.0),
            WindowPlacement::Floating(WindowRect {
                x: 4,
                y: 3,
                width: 40,
                height: 10,
            }),
        )
        .expect("float virtual document");
    virtual_app
        .move_graph_window(&format!("window:{}", virtual_window.0), 6, 5)
        .expect("move virtual document");
    virtual_app
        .resize_graph_window(&format!("window:{}", virtual_window.0), 2)
        .expect("resize virtual document");
    for placement in [
        WindowPlacement::Docked {
            edge: DockEdge::Left,
            size: 16,
        },
        WindowPlacement::Fullscreen,
        WindowPlacement::Hidden,
    ] {
        virtual_app
            .place_graph_window(&format!("window:{}", virtual_window.0), placement)
            .expect("place virtual document");
        assert_eq!(
            virtual_app
                .window_session
                .graph()
                .window(virtual_window)
                .expect("same virtual window")
                .placement,
            placement
        );
    }
    virtual_app
        .show_graph_window(&format!("window:{}", virtual_window.0))
        .expect("show virtual document");
    virtual_app
        .place_graph_window(
            &format!("window:{}", virtual_window.0),
            WindowPlacement::Tiled,
        )
        .expect("tile virtual document");
    assert_eq!(
        virtual_app
            .window_session
            .graph()
            .content_for_window(virtual_window)
            .expect("same virtual content")
            .id,
        virtual_content.id
    );
    let virtual_pane = virtual_app
        .window_session
        .editor_ids()
        .pane_for_window(virtual_window)
        .expect("virtual pane");
    virtual_app
        .clone_graph_window(
            &format!("window:{}", virtual_window.0),
            crate::editor::Axis::Horizontal,
        )
        .expect("clone virtual view for close qualification");
    let virtual_clone = virtual_app
        .window_session
        .graph()
        .focused()
        .expect("virtual clone window");
    assert_ne!(virtual_clone, virtual_window);
    virtual_app
        .place_graph_window(
            &format!("window:{}", virtual_clone.0),
            WindowPlacement::Floating(WindowRect {
                x: 8,
                y: 4,
                width: 30,
                height: 8,
            }),
        )
        .expect("float virtual clone");
    virtual_app
        .close_graph_window(&format!("window:{}", virtual_clone.0))
        .expect("close exact virtual view");
    assert!(
        virtual_app
            .window_session
            .graph()
            .window(virtual_clone)
            .is_none()
    );
    assert_eq!(
        virtual_app
            .window_session
            .editor_ids()
            .window_for(virtual_pane),
        Some(virtual_window)
    );
    assert_eq!(virtual_app.state.buffers.len(), 1);
}


#[test]
pub(in crate::app) fn startup_plugin_publication_waits_for_session_restoration() {
    use crate::persistence::PersistedLayout;
    use crate::persistence::PersistedPane;
    use crate::persistence::PersistedSession;
    use crate::persistence::PersistedState;
    use crate::persistence::PersistedTab;
    use crate::persistence::PersistenceService;
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("restored.txt");
    fs::write(&path, "restored text\n").unwrap();
    let session = PersistedSession {
        roots: vec![app.state.workspace_root.clone()],
        tabs: vec![PersistedTab {
            layout: PersistedLayout::Pane {
                pane: PersistedPane {
                    path: path.clone(),
                    cursor: 3,
                    anchor: 3,
                    scroll_line: 0,
                    scroll_column: 0,
                },
            },
            focused_path: Some(path.clone()),
            focused_pane: None,
        }],
        active_tab: 0,
        window_rules: Vec::new(),
        content_descriptors: Vec::new(),
        pinned: Vec::new(),
    };
    let saved = tempdir().unwrap();
    let state_path = saved.path().join("session.json");
    fs::write(
        &state_path,
        serde_json::to_vec(&PersistedState {
            session: Some(session),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    let (worker, release) = PersistenceService::gated_for_test(1);
    app.persistence =
        crate::app::persistence::PersistenceSession::new(worker, state_path);
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    app.load_persistence();
    fixture.indexed(
        crate::plugins::index_manifest_contributions(
            &plugin_manifest("demo", "plugin.demo.test", "x y"),
            1,
            "startup".into(),
        )
        .unwrap(),
    );
    let revision = app.configuration.binding_revision();
    app.process_plugin_events();
    assert!(app.extension.contributions().get("demo").is_none());
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.persistence.pending() {
        app.process_plugin_events();
        app.process_persistence_events();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(app.persistence.restoring());
    app.process_plugin_events();
    assert_eq!(app.configuration.binding_revision(), revision);
    wait_vfs(&mut app);
    assert!(!app.persistence.restoring());
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "restored text\n"
    );
    assert_eq!(
        app.state.views[app.state.active_view.unwrap()].cursor,
        ByteOffset(3)
    );
    app.process_plugin_events();
    assert!(app.extension.contributions().get("demo").is_some());
    assert!(app.configuration.binding_revision() > revision);
}