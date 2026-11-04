use super::{ProjectionRequest, WindowSession};
use crate::{
    config::load_bundled_config,
    ui::{
        NativeSurfaceWindowBinding,
        window::{ContentAdapterKind, ContentContract, WindowId, WindowPlacement, WindowRect},
    },
};

fn editor_fixture() -> (crate::editor::EditorState, WindowSession) {
    let mut state = crate::editor::EditorState::new("/workspace".into());
    state.open_text(crate::editor::text::TextBuffer::from_text("editor"), None);
    let mut windows = WindowSession::default();
    let candidate = windows
        .prepare_editor(&(&state).into(), "[scratch]")
        .unwrap();
    windows.commit(candidate).unwrap();
    (state, windows)
}

#[test]
fn session_restore_retires_plugin_windows_before_replacing_their_tabs() {
    use crate::persistence::{PersistedLayout, PersistedPane, PersistedSession, PersistedTab};
    use crate::ui::window::ContentTypeId;
    let (state, mut windows) = editor_fixture();
    let tab = windows.graph().active_tab().unwrap();
    let graph = &mut windows.core.graph;
    let content = graph
        .create_content(
            ContentTypeId::new("plugin.demo.panel").unwrap(),
            ContentAdapterKind::DeclarativeUi,
            "Panel".into(),
            false,
            "plugin.demo",
            1,
        )
        .unwrap();
    let view = graph.create_view(content, "plugin.demo", 1).unwrap();
    let panel = graph
        .open_window(
            tab,
            view,
            WindowPlacement::Fullscreen,
            true,
            "plugin.demo",
            1,
        )
        .unwrap();
    let path = std::path::PathBuf::from("/workspace/restored.rs");
    let session = PersistedSession {
        roots: vec!["/workspace".into()],
        tabs: vec![PersistedTab {
            layout: PersistedLayout::Pane {
                pane: PersistedPane {
                    path: path.clone(),
                    cursor: 0,
                    anchor: 0,
                    scroll_line: 0,
                    scroll_column: 0,
                },
            },
            focused_path: Some(path.clone()),
        }],
        active_tab: 0,
        window_rules: Vec::new(),
        content_descriptors: Vec::new(),
        pinned: Vec::new(),
    };
    let mut draft = state.draft();
    draft
        .restore_layout(
            &session,
            &std::collections::HashMap::from([(
                path.clone(),
                (
                    path.clone(),
                    crate::editor::text::TextBuffer::from_text("restored"),
                ),
            )]),
        )
        .unwrap();
    let catalog = crate::extensions::PluginCatalogVersion::default();
    let transition = windows
        .prepare_restore_transition(
            draft,
            (&state.version(), catalog.clone()),
            "scratch",
            super::NativeDescriptions {
                signature: super::NativeSurfaceSignature {
                    editor_tab: Some(tab),
                    editor_window: None,
                    focused_plugin: None,
                    explorer_visible: false,
                    explorer_views: std::sync::Arc::from([1]),
                    focused_explorer: None,
                    explorer_size: 24,
                    active_terminal: None,
                    shown_terminals: std::sync::Arc::from([]),
                    terminal_focused: false,
                    terminal_size: 8,
                    terminal_shrunk: false,
                    terminal_set_revision: Some(0),
                    terminal_width: 80,
                    terminal_height: 24,
                    dashboard_visible: false,
                    picker_rect: None,
                    picker_surface: None,
                    picker_title: "Picker".into(),
                    protected_prompt_height: 0,
                    configuration_generation: 0,
                    screen_bottom: 0,
                },
                explorer_title: "Explorer",
                dashboard_title: "Dust",
                picker_title: "Picker",
                terminals: &[],
            },
            &[("demo".into(), 1)],
            super::RestoreWindowPolicy {
                rules: &[],
                descriptors: &[],
            },
        )
        .unwrap();
    assert!(
        windows.graph().window(panel).is_some(),
        "preparation is detached"
    );
    let restored = windows
        .accept_editor_transition(transition, &state.version(), &catalog)
        .unwrap();
    assert_eq!(
        restored.buffers[restored.active_buffer_id().unwrap()]
            .path
            .as_ref(),
        Some(&path)
    );
    assert!(windows.graph().window(panel).is_none());
    assert!(windows.graph().workspace_tab(tab).is_none());
    windows.graph().validate().unwrap();
}

#[test]
fn candidates_are_bound_to_the_session_and_owner_epoch_not_only_graph_revision() {
    let (state, mut windows) = editor_fixture();
    let (_, mut other) = editor_fixture();
    assert_eq!(windows.graph(), other.graph());
    let candidate = windows
        .prepare_editor(&(&state).into(), "[scratch]")
        .unwrap();
    let before = other.graph().clone();
    assert!(other.commit(candidate).is_err());
    assert_eq!(other.graph(), &before);
    let candidate = windows
        .prepare_editor(&(&state).into(), "[scratch]")
        .unwrap();
    let before = windows.graph().clone();
    windows.invalidate_native();
    assert_eq!(windows.graph(), &before);
    assert!(windows.commit(candidate).is_err());
    assert_eq!(windows.graph(), &before);
}

#[test]
fn failed_explorer_reconstruction_does_not_publish_a_partial_graph_or_bridge() {
    let (state, windows) = editor_fixture();
    let graph = windows.graph().clone();
    let native = windows.core.native.clone();
    let epoch = windows.epoch;
    let signature = super::bridge::NativeSurfaceSignature {
        editor_tab: None,
        editor_window: None,
        focused_plugin: None,
        explorer_visible: true,
        explorer_views: (1..=crate::UNIVERSAL_LAYOUT_WINDOW_CEILING as u64)
            .collect::<Vec<_>>()
            .into(),
        focused_explorer: Some(1),
        explorer_size: 24,
        active_terminal: None,
        shown_terminals: Vec::new().into(),
        terminal_focused: false,
        terminal_size: 8,
        terminal_shrunk: false,
        terminal_set_revision: Some(0),
        terminal_width: 80,
        terminal_height: 24,
        dashboard_visible: false,
        picker_rect: None,
        picker_surface: None,
        picker_title: "Picker".into(),
        protected_prompt_height: 0,
        configuration_generation: 0,
        screen_bottom: 0,
    };
    let error = match windows.prepare_reconcile(
        &(&state).into(),
        "[scratch]",
        super::NativeDescriptions {
            signature,
            explorer_title: "Explorer",
            dashboard_title: "Dust",
            picker_title: "Picker",
            terminals: &[],
        },
    ) {
        Ok(_) => panic!("graph ceiling must reject reconstruction"),
        Err(error) => error,
    };
    assert!(error.contains("ceiling"), "{error}");
    assert_eq!(windows.graph(), &graph);
    assert_eq!(windows.core.native, native);
    assert_eq!(windows.epoch, epoch);
}

#[test]
fn editor_transition_rejects_changed_and_foreign_sources_without_window_publication() {
    let catalog = crate::extensions::PluginCatalogVersion::default();
    let (mut state, mut windows) = editor_fixture();
    let base = state.clone();
    let before = state.window_version();
    let proposed = state
        .prepare_transaction(
            &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
                .with_operation(crate::editor::EditorOperation::NewTab),
        )
        .unwrap();
    let candidate = windows
        .prepare_editor(&proposed.topology(), "[scratch]")
        .unwrap();
    let transition = windows
        .stage_editor_transition(proposed, candidate, catalog.clone())
        .unwrap();
    let graph = windows.graph().clone();
    state.open_text(
        crate::editor::text::TextBuffer::from_text("intervening"),
        None,
    );
    assert!(
        windows
            .accept_editor_transition(transition, &state.version(), &catalog)
            .is_err()
    );
    assert_eq!(windows.graph(), &graph);
    let proposed = base
        .prepare_transaction(
            &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
                .with_operation(crate::editor::EditorOperation::NewTab),
        )
        .unwrap();
    let candidate = windows
        .prepare_editor(&proposed.topology(), "[scratch]")
        .unwrap();
    let transition = windows
        .stage_editor_transition(proposed, candidate, catalog.clone())
        .unwrap();
    let (foreign, _) = editor_fixture();
    assert_eq!(foreign.window_revision(), before.revision());
    assert!(
        windows
            .accept_editor_transition(transition, &foreign.version(), &catalog)
            .is_err()
    );
    assert_eq!(windows.graph(), &graph);
    let proposed = base
        .prepare_transaction(
            &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
                .with_operation(crate::editor::EditorOperation::NewTab),
        )
        .unwrap();
    let candidate = windows
        .prepare_editor(&proposed.topology(), "[scratch]")
        .unwrap();
    let transition = windows
        .stage_editor_transition(proposed, candidate, catalog.clone())
        .unwrap();
    windows.invalidate_native();
    assert!(
        windows
            .accept_editor_transition(transition, &base.version(), &catalog)
            .is_err()
    );
    assert_eq!(windows.graph(), &graph);
}

#[test]
fn exhausted_owner_rejects_transition_before_either_live_state_is_replaced() {
    let catalog = crate::extensions::PluginCatalogVersion::default();
    let (state, mut windows) = editor_fixture();
    windows.epoch = u64::MAX - 1;
    let proposed = state
        .prepare_transaction(
            &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1))
                .with_operation(crate::editor::EditorOperation::NewTab),
        )
        .unwrap();
    let candidate = windows
        .prepare_editor(&proposed.topology(), "[scratch]")
        .unwrap();
    let transition = windows
        .stage_editor_transition(proposed, candidate, catalog.clone())
        .unwrap();
    let expected = transition.proposal.window_version();
    let accepted = windows
        .accept_editor_transition(transition, &state.version(), &catalog)
        .unwrap();
    assert_eq!(accepted.window_version(), expected);
    assert_eq!(windows.epoch, u64::MAX);
    let graph = windows.graph().clone();
    let copies = crate::ui::window::WindowGraph::test_clone_count();
    assert!(
        windows
            .prepare_editor(&(&accepted).into(), "[scratch]")
            .is_err()
    );
    assert_eq!(crate::ui::window::WindowGraph::test_clone_count(), copies);
    assert_eq!(windows.graph(), &graph);
}

#[test]
fn plugin_drafts_cannot_write_editor_resources_foreign_generations_tabs_or_protected_layers() {
    let catalog = crate::extensions::PluginCatalogVersion::default();
    use crate::ui::window::{ContentTypeId, WindowError};
    let (state, mut windows) = editor_fixture();
    let editor_window = windows.graph().focused().unwrap();
    let active = windows.graph().active_tab().unwrap();
    let other_tab = windows
        .core
        .graph
        .create_workspace_tab("fixture", 1)
        .unwrap();
    let before = windows.graph().clone();
    let mut draft = windows
        .begin_plugin("demo", 1, (&state.window_version(), &catalog))
        .unwrap();
    assert_eq!(
        draft.place(editor_window, WindowPlacement::Hidden),
        Err(WindowError::InvalidOwner)
    );
    let kind = ContentTypeId::new("plugin.demo.panel").unwrap();
    assert_eq!(
        draft.create_content_with_contract(
            kind.clone(),
            ContentContract::adapter_defaults(ContentAdapterKind::DeclarativeUi),
            "foreign".into(),
            false,
            "plugin.other",
            1
        ),
        Err(WindowError::InvalidOwner)
    );
    assert_eq!(
        draft.create_content_with_contract(
            kind.clone(),
            ContentContract::adapter_defaults(ContentAdapterKind::DeclarativeUi),
            "foreign".into(),
            false,
            "plugin.demo",
            2
        ),
        Err(WindowError::InvalidOwner)
    );
    assert!(
        draft
            .create_content_with_contract(
                ContentTypeId::new("builtin.text-buffer").unwrap(),
                ContentContract::adapter_defaults(ContentAdapterKind::DeclarativeUi),
                "reserved".into(),
                false,
                "plugin.demo",
                1
            )
            .is_err()
    );
    assert!(
        draft
            .create_content_with_contract(
                kind.clone(),
                ContentContract::adapter_defaults(ContentAdapterKind::TextBuffer),
                "editor".into(),
                false,
                "plugin.demo",
                1
            )
            .is_err()
    );
    let content = draft
        .create_content_with_contract(
            kind,
            ContentContract::adapter_defaults(ContentAdapterKind::DeclarativeUi),
            "panel".into(),
            false,
            "plugin.demo",
            1,
        )
        .unwrap();
    let view = draft.create_view(content, "plugin.demo", 1).unwrap();
    let placement = WindowPlacement::Floating(WindowRect {
        x: 1,
        y: 1,
        width: 20,
        height: 8,
    });
    assert_eq!(
        draft.open_window(other_tab, view, placement, true, "plugin.demo", 1),
        Err(WindowError::WindowOutsideActiveTab)
    );
    assert_eq!(
        draft.open_window(active, view, WindowPlacement::Tiled, true, "plugin.demo", 1),
        Err(WindowError::InvalidPlacement)
    );
    let window = draft
        .open_window(active, view, placement, true, "plugin.demo", 1)
        .unwrap();
    assert_eq!(
        draft.place(
            window,
            WindowPlacement::Overlay(WindowRect {
                x: 0,
                y: 0,
                width: 20,
                height: 8
            })
        ),
        Err(WindowError::InvalidPlacement)
    );
    assert_eq!(windows.graph(), &before);
    let duplicate = draft.clone();
    windows
        .commit_plugin(draft, &state.window_version(), &catalog)
        .unwrap();
    assert!(windows.graph().window(window).is_some());
    assert!(
        windows
            .commit_plugin(duplicate, &state.window_version(), &catalog)
            .is_err()
    );
    let pending = windows
        .begin_plugin("demo", 1, (&state.window_version(), &catalog))
        .unwrap();
    windows.remove_plugin_generation("demo", 1).unwrap();
    assert!(windows.graph().window(window).is_none());
    assert!(
        windows
            .commit_plugin(pending, &state.window_version(), &catalog)
            .is_err()
    );
    assert!(windows.graph().window(editor_window).is_some());
}

#[test]
fn body_and_notice_bounds_fail_closed_at_tiny_sizes() {
    let config = load_bundled_config().config;
    for width in 0..=10 {
        for height in 0..=10 {
            let rect = WindowRect {
                x: 5,
                y: 7,
                width,
                height,
            };
            let body = crate::ui::geometry::explorer_body_geometry(rect);
            assert!(body.y >= rect.y && body.y + body.height <= rect.y + height);
            assert!(body.width <= width);
            if let Some(notice) = crate::ui::geometry::error_notification_geometry(width, height) {
                assert!(notice.x + notice.width <= width);
                assert!(notice.y + notice.height <= height);
            }
            let rows = crate::ui::geometry::documentation_page_rows(&config, width, height);
            assert!(rows <= usize::from(height.saturating_sub(2)));
        }
    }
}

#[test]
fn accepted_projection_rejects_old_source_graph_size_and_invalidation() {
    let config = load_bundled_config().config;
    let bounds = WindowRect {
        x: 0,
        y: 0,
        width: 100,
        height: 30,
    };
    let mut graph = crate::ui::window::WindowGraph::default();
    let tab = graph.create_workspace_tab("fixture", 1).unwrap();
    let content = graph
        .create_content(
            crate::ui::window::ContentTypeId::new("fixture.explorer").unwrap(),
            ContentAdapterKind::DeclarativeUi,
            "Explorer".into(),
            false,
            "fixture",
            1,
        )
        .unwrap();
    let view = graph.create_view(content, "fixture", 1).unwrap();
    graph
        .open_window(
            tab,
            view,
            WindowPlacement::Docked {
                edge: crate::ui::window::DockEdge::Left,
                size: 25,
            },
            true,
            "fixture",
            1,
        )
        .unwrap();
    let revision = graph.revision().unwrap();
    let mut windows = WindowSession::from_test_graph(graph);
    assert!(windows.current(Some(1), (100, 30)).is_none());
    windows.project(ProjectionRequest {
        tab,
        source_revision: 1,
        graph_revision: revision,
        bounds,
        minimum_size: (4, 2),
        insets: crate::ui::window::TiledInsets {
            top: 1,
            bottom: 1,
            ..Default::default()
        },
        surfaces: NativeSurfaceWindowBinding {
            explorer: Some(WindowId(1)),
            terminal: None,
            dashboard: None,
            picker: None,
        },
        protected: Some(WindowRect {
            x: 20,
            y: 1,
            width: 60,
            height: 4,
        }),
    });
    windows.project_content(None, &config);
    assert!(windows.current(Some(1), (100, 30)).is_some());
    assert!(windows.current(Some(2), (100, 30)).is_none());
    assert!(windows.current(Some(1), (80, 24)).is_none());
    assert_eq!(
        windows.layout().unwrap().projection.protected_regions.len(),
        1
    );
    let body = windows.content().explorer.unwrap();
    assert_eq!((body.y, body.height), (3, 27));
    assert_eq!(
        windows.content().documentation_rows,
        crate::ui::geometry::documentation_page_rows(&config, 75, 30)
    );
    // Exercise the revision predicate itself, rather than merely clearing the cache.
    windows
        .core
        .graph
        .place(WindowId(1), WindowPlacement::Hidden)
        .unwrap();
    assert!(windows.current(Some(1), (100, 30)).is_none());
    windows.invalidate();
    assert!(windows.layout().is_none());
    assert!(windows.content().explorer.is_none());
}

#[test]
fn staged_window_work_rejects_editor_and_catalog_changes_without_a_window_epoch_change() {
    use crate::extensions::{PluginContributionCatalog, PluginContributionIndex};
    let (mut state, mut windows) = editor_fixture();
    let mut catalog = PluginContributionCatalog::default();
    let before = state.window_version();
    let draft = windows
        .begin_plugin("demo", 7, (&before, &catalog.version()))
        .unwrap();
    let candidate = windows
        .prepare_editor(&(&state).into(), "[scratch]")
        .unwrap();
    let transition = windows
        .stage_editor_transition(state.draft(), candidate, catalog.version())
        .unwrap();
    let epoch = windows.epoch;
    let graph = windows.graph().clone();
    catalog
        .install(PluginContributionIndex {
            owner: "demo".into(),
            version: "1.0.0".into(),
            checksum: "0".repeat(64),
            generation: 7,
            source: "source fixture".into(),
            commands: Default::default(),
            contexts: Vec::new(),
            keymaps: Vec::new(),
            key_groups: Vec::new(),
            content_types: Vec::new(),
            subscriptions: Vec::new(),
            providers: Vec::new(),
            virtual_documents: Vec::new(),
            storage: None,
        })
        .unwrap();
    assert_eq!(windows.epoch, epoch);
    let copies = crate::ui::window::WindowGraph::test_clone_count();
    assert!(
        windows
            .commit_plugin(draft, &before, &catalog.version())
            .is_err()
    );
    assert!(
        windows
            .accept_editor_transition(transition, &state.version(), &catalog.version())
            .is_err()
    );
    assert_eq!(crate::ui::window::WindowGraph::test_clone_count(), copies);
    assert_eq!(windows.graph(), &graph);
    let draft = windows
        .begin_plugin("demo", 7, (&before, &catalog.version()))
        .unwrap();
    let window = windows.graph().focused().unwrap();
    let plan = windows
        .plan_focus(window, &before, catalog.version())
        .unwrap();
    state.open_text(
        crate::editor::text::TextBuffer::from_text("intervening source"),
        None,
    );
    assert_eq!(windows.epoch, epoch);
    let copies = crate::ui::window::WindowGraph::test_clone_count();
    assert!(
        windows
            .check_plan(&plan, &state.window_version(), &catalog.version())
            .is_err()
    );
    assert!(
        windows
            .commit_plugin(draft, &state.window_version(), &catalog.version())
            .is_err()
    );
    assert!(
        windows
            .begin_plugin("demo", 7, (&state.window_version(), &catalog.version()))
            .is_err()
    );
    assert_eq!(crate::ui::window::WindowGraph::test_clone_count(), copies);
    assert_eq!(windows.graph(), &graph);
}

#[test]
fn cross_tab_focus_plan_targets_the_exact_editor_tab_without_eager_mutation() {
    use crate::editor::{EditorOperation, EditorTransaction, TransactionId};
    let (mut state, mut windows) = editor_fixture();
    let catalog = crate::extensions::PluginContributionCatalog::default();
    let original_window = windows.graph().focused().expect("first tab window");
    state
        .apply_transaction(
            &EditorTransaction::new(TransactionId(1)).with_operation(EditorOperation::NewTab),
        )
        .unwrap();
    let candidate = windows
        .prepare_editor(&(&state).into(), "[scratch]")
        .unwrap();
    windows.commit(candidate).unwrap();
    let active_before = state.active_tab;
    let graph_before = windows.graph().clone();
    let plan = windows
        .plan_focus(original_window, &state.window_version(), catalog.version())
        .unwrap();
    assert!(matches!(
        plan.operations().first(),
        Some(EditorOperation::FocusTabId { tab }) if Some(*tab) != active_before
    ));
    assert!(matches!(
        plan.operations().get(1),
        Some(EditorOperation::FocusPaneId { .. })
    ));
    assert_eq!(state.active_tab, active_before);
    assert_eq!(windows.graph(), &graph_before);
}

#[test]
fn focus_capture_survives_geometry_invalidation_but_not_reset_or_another_editor() {
    let (state, mut windows) = editor_fixture();
    let catalog = crate::extensions::PluginContributionCatalog::default();
    let capture = windows.capture_focus().unwrap();
    windows.invalidate_native();
    assert!(
        windows
            .restore_focus_plan(capture, &state.window_version(), catalog.version())
            .is_some()
    );
    let capture = windows.capture_focus().unwrap();
    let (foreign, _) = editor_fixture();
    assert_eq!(state.active_view, foreign.active_view);
    assert!(
        windows
            .restore_focus_plan(capture, &foreign.window_version(), catalog.version())
            .is_none()
    );
    let capture = windows.capture_focus().unwrap();
    windows.reset_runtime(&[]).unwrap();
    assert!(
        windows
            .restore_focus_plan(capture, &state.window_version(), catalog.version())
            .is_none()
    );
}

#[test]
fn editor_proposals_cannot_swap_equal_revision_branches_or_rebase_stale_work() {
    use crate::editor::{EditorOperation, EditorTransaction, TransactionId};
    let (mut state, mut windows) = editor_fixture();
    let catalog = crate::extensions::PluginCatalogVersion::default();
    let transaction =
        EditorTransaction::new(TransactionId(1)).with_operation(EditorOperation::NewTab);
    let left = state.prepare_transaction(&transaction).unwrap();
    let right = state.prepare_transaction(&transaction).unwrap();
    assert_eq!(
        left.window_version().revision(),
        right.window_version().revision()
    );
    let candidate = windows.prepare_editor(&left.topology(), "scratch").unwrap();
    let graph = windows.graph().clone();
    assert!(
        windows
            .stage_editor_transition(right, candidate, catalog.clone())
            .is_err()
    );
    assert_eq!(windows.graph(), &graph);

    // A text/cursor-only live mutation does not change window geometry, but must
    // revoke publication of a full detached editor, not silently lose that change.
    let window_source = state.window_version();
    state
        .apply_transaction(&EditorTransaction::new(TransactionId(2)).with_operation(
            EditorOperation::SetCursor {
                view: state.active_view.unwrap(),
                position: crate::editor::text::ByteOffset(1),
            },
        ))
        .unwrap();
    assert_eq!(state.window_version(), window_source);
    let copies = crate::ui::window::WindowGraph::test_clone_count();
    assert!(
        windows
            .prepare_editor_transition(left, &state.version(), catalog.clone(), "scratch", None)
            .is_err()
    );
    assert_eq!(crate::ui::window::WindowGraph::test_clone_count(), copies);
    assert_eq!(windows.graph(), &graph);

    let proposal = state.prepare_transaction(&transaction).unwrap();
    let expected = proposal.window_version();
    let transition = windows
        .prepare_editor_transition(proposal, &state.version(), catalog.clone(), "scratch", None)
        .unwrap();
    let published = windows
        .accept_editor_transition(transition, &state.version(), &catalog)
        .unwrap();
    assert_eq!(published.window_version(), expected);
    assert_eq!(windows.editor_ids().source(), Some(&expected));
}

#[test]
fn invalid_editor_sources_reject_before_graph_copy() {
    let (state, windows) = editor_fixture();
    let mut exhausted = state.clone();
    exhausted.exhaust_window_source_for_test();
    let (foreign, _) = editor_fixture();
    let copies = crate::ui::window::WindowGraph::test_clone_count();
    assert!(
        windows
            .prepare_editor(&(&exhausted).into(), "scratch")
            .is_err()
    );
    assert!(
        windows
            .prepare_editor(&(&foreign).into(), "scratch")
            .is_err()
    );
    assert_eq!(crate::ui::window::WindowGraph::test_clone_count(), copies);
}

/// Run directly with --ignored --nocapture --test-threads=1. Optional Linux
/// allocation observer: tests/ownership_alloc.c; timings run WITHOUT preloading.
#[test]
#[ignore = "explicit admission cost probe; prints bounded fixture counters/timing"]
fn window_admission_cost_probe() {
    use crate::editor::text::TextBuffer;
    use crate::editor::{EditorOperation, EditorState, EditorTransaction, TransactionId};
    use crate::ui::window::WindowGraph;
    let case = std::env::var("DUST_COST_CASE").unwrap_or_else(|_| "idle".into());
    let iterations: usize = std::env::var("DUST_COST_ITERATIONS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    assert!((1..=10000).contains(&iterations));
    let mut state = EditorState::new("/workspace".into());
    let contents = "0123456789abcdef\n".repeat(3856); // 65,552 bytes, 3,857 lines per buffer.
    for n in 0..32 {
        state.open_text(
            TextBuffer::from_text(&contents),
            Some(format!("/workspace/{n}.txt").into()),
        );
    }
    let mut windows = WindowSession::default();
    let candidate = windows.prepare_editor(&(&state).into(), "scratch").unwrap();
    windows.commit(candidate).unwrap();
    let foreign = EditorState::new("/foreign".into());
    let mut exhausted = state.clone();
    exhausted.exhaust_window_source_for_test();
    let mut changed = state.clone();
    changed.open_text(TextBuffer::from_text("changed"), None);
    let current = state.version();
    let source = state.window_version();
    let catalog = crate::extensions::PluginCatalogVersion::default();
    let transaction =
        EditorTransaction::new(TransactionId(1)).with_operation(EditorOperation::NewTab);
    let baseline = (
        WindowGraph::test_clone_count(),
        EditorState::test_clone_count(),
        TextBuffer::test_materializations(),
    );
    eprintln!("DUST_ALLOC_START");
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        match case.as_str() {
            "idle" | "unchanged" => {
                std::hint::black_box(windows.require_editor_source(&source)).unwrap();
            }
            "invalid" => {
                assert!(
                    windows
                        .prepare_editor(&(&foreign).into(), "scratch")
                        .is_err()
                );
            }
            "exhausted" => {
                assert!(
                    windows
                        .prepare_editor(&(&exhausted).into(), "scratch")
                        .is_err()
                );
            }
            "stale" => {
                assert!(
                    windows
                        .require_editor_source(&changed.window_version())
                        .is_err()
                );
            }
            "valid" => {
                let draft = state.prepare_transaction(&transaction).unwrap();
                std::hint::black_box(windows.prepare_editor_transition(
                    draft,
                    &current,
                    catalog.clone(),
                    "scratch",
                    None,
                ))
                .unwrap();
            }
            _ => panic!("unknown cost case"),
        }
    }
    let elapsed = start.elapsed().as_nanos();
    eprintln!("DUST_ALLOC_END");
    let text = TextBuffer::test_materializations();
    println!(
        "case={case} iterations={iterations} buffers=32 bytes_per_buffer={} elapsed_ns={elapsed} graph_copies={} editor_copies={} text_decodes={} text_displays={}",
        contents.len(),
        WindowGraph::test_clone_count() - baseline.0,
        EditorState::test_clone_count() - baseline.1,
        text.0 - baseline.2.0,
        text.1 - baseline.2.1
    );
    if case != "valid" {
        assert_eq!(WindowGraph::test_clone_count(), baseline.0);
        assert_eq!(EditorState::test_clone_count(), baseline.1);
    }
    assert_eq!(text, baseline.2);
}

#[test]
fn startup_document_and_dashboard_publish_together_only_for_the_accepted_proposal() {
    let (state, mut windows) = editor_fixture();
    windows.dashboard = true;
    let catalog = crate::extensions::PluginCatalogVersion::default();
    let current = state.version();
    let descriptions = || super::NativeDescriptions {
        signature: super::bridge::NativeSurfaceSignature {
            editor_tab: None,
            editor_window: None,
            focused_plugin: None,
            explorer_visible: false,
            explorer_views: vec![1].into(),
            focused_explorer: None,
            explorer_size: 24,
            active_terminal: None,
            shown_terminals: Vec::new().into(),
            terminal_focused: false,
            terminal_size: 8,
            terminal_shrunk: false,
            terminal_set_revision: Some(0),
            terminal_width: 80,
            terminal_height: 24,
            dashboard_visible: true,
            picker_rect: None,
            picker_surface: None,
            picker_title: "Picker".into(),
            protected_prompt_height: 0,
            configuration_generation: 0,
            screen_bottom: 0,
        },
        explorer_title: "Explorer",
        dashboard_title: "Dust",
        picker_title: "Picker",
        terminals: &[],
    };
    let mut draft = state.draft();
    draft.open_document(
        crate::editor::text::TextBuffer::from_text("startup"),
        "/workspace/new.txt".into(),
    );
    let transition = windows
        .prepare_document_transition(draft, &current, catalog.clone(), "scratch", descriptions())
        .unwrap();
    assert!(windows.dashboard_visible());
    let graph = windows.graph().clone();
    windows.epoch += 1;
    assert!(
        windows
            .accept_editor_transition(transition, &current, &catalog)
            .is_err()
    );
    assert!(windows.dashboard_visible());
    assert_eq!(windows.graph(), &graph);
    let mut draft = state.draft();
    let (buffer, _) = draft.open_document(
        crate::editor::text::TextBuffer::from_text("startup"),
        "/workspace/new.txt".into(),
    );
    let transition = windows
        .prepare_document_transition(draft, &current, catalog.clone(), "scratch", descriptions())
        .unwrap();
    let accepted = windows
        .accept_editor_transition(transition, &current, &catalog)
        .unwrap();
    assert_eq!(accepted.buffers[buffer].text.to_string(), "startup");
    assert!(!windows.dashboard_visible());
    windows.epoch = u64::MAX;
    let copies = crate::ui::window::WindowGraph::test_clone_count();
    assert!(windows.check_editor_admission(&(&accepted).into()).is_err());
    assert_eq!(crate::ui::window::WindowGraph::test_clone_count(), copies);
}

#[test]
fn native_command_plans_are_source_bound_before_any_candidate_copy() {
    use super::{CommandPolicy, WindowCommand};
    let (state, mut windows) = editor_fixture();
    let source = state.window_version();
    let catalog = crate::extensions::PluginCatalogVersion::default();
    windows.project(ProjectionRequest {
        tab: windows.graph().active_tab().unwrap(),
        source_revision: state.window_revision().unwrap(),
        graph_revision: windows.graph().revision().unwrap(),
        bounds: WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
        minimum_size: (4, 2),
        insets: Default::default(),
        surfaces: NativeSurfaceWindowBinding {
            explorer: None,
            terminal: None,
            dashboard: None,
            picker: None,
        },
        protected: None,
    });
    let policy = || CommandPolicy {
        selected_terminal: None,
        pane_step: 2,
        terminal_step: 2,
    };
    let copies = crate::ui::window::WindowGraph::test_clone_count();
    let clone_plan = windows
        .plan_window_command(
            WindowCommand::Clone("focused", crate::editor::Axis::Horizontal, 500),
            &(&state).into(),
            policy(),
            catalog.clone(),
        )
        .unwrap_or_else(|_| panic!("clone plan"));
    assert!(matches!(
        clone_plan.operations(),
        [crate::editor::EditorOperation::ClonePane { .. }]
    ));
    assert_eq!(crate::ui::window::WindowGraph::test_clone_count(), copies);
    let mut changed = state.clone();
    changed
        .split_active(crate::editor::Axis::Vertical, 500)
        .expect("change clone source");
    let before_clone_rejection = windows.graph().clone();
    assert!(
        windows
            .check_plan(&clone_plan, &changed.window_version(), &catalog)
            .is_err()
    );
    assert_eq!(windows.graph(), &before_clone_rejection);
    let mut exhausted = state.clone();
    exhausted.exhaust_window_source_for_test();
    assert!(
        windows
            .check_plan(&clone_plan, &exhausted.window_version(), &catalog)
            .is_err()
    );
    assert_eq!(windows.graph(), &before_clone_rejection);

    let (foreign_state, mut foreign_windows) = editor_fixture();
    foreign_windows.project(ProjectionRequest {
        tab: foreign_windows.graph().active_tab().unwrap(),
        source_revision: foreign_state.window_revision().unwrap(),
        graph_revision: foreign_windows.graph().revision().unwrap(),
        bounds: WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
        minimum_size: (4, 2),
        insets: Default::default(),
        surfaces: NativeSurfaceWindowBinding {
            explorer: None,
            terminal: None,
            dashboard: None,
            picker: None,
        },
        protected: None,
    });
    let foreign_before = foreign_windows.graph().clone();
    assert!(
        foreign_windows
            .check_plan(&clone_plan, &foreign_state.window_version(), &catalog)
            .is_err()
    );
    assert_eq!(foreign_windows.graph(), &foreign_before);

    let placement_plan = windows
        .plan_window_command(
            WindowCommand::Place(
                "focused",
                crate::ui::window::WindowPlacement::Floating(WindowRect {
                    x: 3,
                    y: 2,
                    width: 30,
                    height: 8,
                }),
            ),
            &(&state).into(),
            policy(),
            catalog.clone(),
        )
        .unwrap_or_else(|_| panic!("source-bound editor placement plan"));
    let placement_before = windows.graph().clone();
    windows.invalidate_native();
    assert!(
        windows
            .accept_window_command(placement_plan, &source, &catalog)
            .is_err()
    );
    assert_eq!(windows.graph(), &placement_before);

    windows.project(ProjectionRequest {
        tab: windows.graph().active_tab().unwrap(),
        source_revision: state.window_revision().unwrap(),
        graph_revision: windows.graph().revision().unwrap(),
        bounds: WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
        minimum_size: (4, 2),
        insets: Default::default(),
        surfaces: NativeSurfaceWindowBinding {
            explorer: None,
            terminal: None,
            dashboard: None,
            picker: None,
        },
        protected: None,
    });
    let plan = windows
        .plan_window_command(
            WindowCommand::Swap("focused", "focused"),
            &(&state).into(),
            policy(),
            catalog.clone(),
        )
        .unwrap_or_else(|_| panic!("plan"));
    windows
        .accept_window_command(plan, &source, &catalog)
        .unwrap();
    let plan = windows
        .plan_window_command(
            WindowCommand::Clone("focused", crate::editor::Axis::Vertical, 500),
            &(&state).into(),
            policy(),
            catalog.clone(),
        )
        .unwrap_or_else(|_| panic!("clone plan"));
    windows.invalidate_native();
    let before = windows.graph().clone();
    let copies = crate::ui::window::WindowGraph::test_clone_count();
    assert!(
        windows
            .accept_window_command(plan, &source, &catalog)
            .is_err()
    );
    assert_eq!(windows.graph(), &before);
    assert_eq!(crate::ui::window::WindowGraph::test_clone_count(), copies);
}

#[test]
fn a_debug_session_shrinks_the_terminal_only_on_a_short_screen_and_gives_it_back() {
    let mut session = WindowSession::new(false, 12);
    session.observe_size((80, 24));
    session.set_debugging(true);
    assert_eq!(session.terminal_height(), 5);
    session.observe_size((120, 40));
    assert!(!session.terminal_shrunk());
    assert_eq!(session.terminal_height(), 12);
    session.observe_size((80, 24));
    assert!(session.terminal_shrunk());
    assert_eq!(session.terminal_height(), 5);
    session.set_debugging(false);
    assert_eq!(session.terminal_height(), 12);
    session.observe_size((120, 40));
    session.observe_size((80, 24));
    assert_eq!(session.terminal_height(), 12);
}
