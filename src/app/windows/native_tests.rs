//! Tests for editor and native resource mapping into the window graph.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::editor::text::TextBuffer;

use crate::windows::TileAxis;

use crate::editor::EditorState;

use crate::windows::ContentAdapterKind;

use crate::windows::ContentTypeId;

use crate::windows::WindowError;

use crate::windows::WindowGraph;

use crate::windows::WindowPlacement;

use crate::app::windows::native::NativeSurfaces;

use crate::app::windows::native::NativeSurfaceSignature;
use crate::windows::ContentLifecycle;
use crate::windows::WindowRect;
use std::sync::Arc;

fn reconcile(state: &mut EditorState) {
    state.register_window_content().unwrap();
    let mut windows = crate::app::windows::Windows::default();
    let candidate = windows
        .prepare_editor(&(&*state).into(), "[scratch]")
        .unwrap();
    windows
        .accept_current(candidate, &state.window_version(), state)
        .unwrap();
}

#[test]
fn same_content_has_independent_views_and_windows_with_stable_focus_identity() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, first_view) = state
        .open_text(
            TextBuffer::from_text("one"),
            Some(PathBuf::from("/workspace/main.rs")),
        )
        .unwrap();
    let first_pane = state
        .tab_focused_window(state.active_tab().expect("tab"))
        .unwrap();
    let second_view = state
        .split_active(TileAxis::Horizontal, 500)
        .expect("split");
    let second_pane = state
        .tab_focused_window(state.active_tab().expect("tab"))
        .unwrap();
    reconcile(&mut state);
    state.validate_layout().unwrap();

    let content = state.content_for_buffer(buffer).expect("content");
    let first = state
        .view_window(first_view)
        .and_then(|window| state.graph.window(window))
        .map(|window| window.view)
        .expect("first view");
    let second = state
        .view_window(second_view)
        .and_then(|window| state.graph.window(window))
        .map(|window| window.view)
        .expect("second view");
    assert_ne!(first, second);
    assert_eq!(state.graph.view(first).expect("first").content, content);
    assert_eq!(state.graph.view(second).expect("second").content, content);
    let first_window = state
        .graph
        .window(first_pane)
        .map(|window| window.id)
        .expect("first window");
    let second_window = state
        .graph
        .window(second_pane)
        .map(|window| window.id)
        .expect("second window");
    assert_eq!(state.graph.focused(), Some(second_window));

    state.focus_pane_id(first_pane).expect("focus first");
    reconcile(&mut state);
    assert_eq!(
        state.graph.window(first_pane).map(|window| window.id),
        Some(first_window)
    );
    assert_eq!(
        state.graph.window(second_pane).map(|window| window.id),
        Some(second_window)
    );
    assert_eq!(state.graph.focused(), Some(first_window));
    assert_eq!(state.graph.content_snapshots()[0].view_count, 2);
    let tab = state.active_tab().expect("tab");
    let before = state
        .graph
        .tiled_layout_snapshot(tab)
        .expect("layout")
        .project(
            crate::windows::WindowRect {
                x: 0,
                y: 0,
                width: 100,
                height: 20,
            },
            1,
            1,
        );
    let revision = state.graph.revision();
    state.resize_active_pane(100).expect("resize");
    assert_ne!(state.graph.revision(), revision);
    reconcile(&mut state);
    let after = state
        .graph
        .tiled_layout_snapshot(tab)
        .expect("layout")
        .project(
            crate::windows::WindowRect {
                x: 0,
                y: 0,
                width: 100,
                height: 20,
            },
            1,
            1,
        );
    assert_eq!(
        state.graph.window(first_pane).map(|window| window.id),
        Some(first_window)
    );
    assert_eq!(
        state.graph.window(second_pane).map(|window| window.id),
        Some(second_window)
    );
    assert!(after.windows[0].rect.width > before.windows[0].rect.width);
    state.validate_layout().unwrap();
}

#[test]
fn editor_reconciliation_preserves_foreign_tiled_windows_and_identity() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state.open_text(TextBuffer::from_text("one"), None).unwrap();
    reconcile(&mut state);
    let tab = state.active_tab().expect("tab");
    let content = state
        .graph
        .create_content(
            ContentTypeId::new("terminal.external-tui").unwrap(),
            ContentAdapterKind::ExternalTui,
            "external".into(),
            false,
            "external",
            1,
        )
        .unwrap();
    let view = state.graph.create_view(content, "external", 1).unwrap();
    let window = state
        .graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "external", 1)
        .unwrap();

    state.set_buffer_dirty(state.active_buffer_id().expect("buffer"), true);
    reconcile(&mut state);
    assert_eq!(state.graph.window(window).expect("foreign tile").view, view);
    assert_eq!(
        state.graph.view(view).expect("foreign view").content,
        content
    );
    assert_eq!(
        state
            .graph
            .window(window)
            .expect("foreign tiled placement")
            .placement,
        WindowPlacement::Tiled
    );
    assert!(
        state
            .graph
            .tiled_layout_snapshot(tab)
            .expect("mixed tiled layout")
            .project(
                crate::windows::WindowRect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 20,
                },
                1,
                1,
            )
            .windows
            .iter()
            .any(|projected| projected.window == window)
    );
}

#[test]
fn pane_close_removes_only_its_view_and_keeps_shared_content() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, _) = state.open_text(TextBuffer::from_text("one"), None).unwrap();
    state.split_active(TileAxis::Vertical, 500).expect("split");
    reconcile(&mut state);
    let content = state.content_for_buffer(buffer).expect("content");

    state.close_active_pane().expect("close pane");
    reconcile(&mut state);
    assert!(state.graph.content(content).is_some());
    assert_eq!(state.graph.counts(), (1, 1, 1, 1));
}

#[test]
fn workspace_tabs_are_distinct_from_one_shared_buffer_entry() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, _) = state.open_text(TextBuffer::from_text("one"), None).unwrap();
    let first_tab = state.active_tab().expect("tab");
    let second_tab = state.new_tab().expect("new tab");
    reconcile(&mut state);

    assert_eq!(state.graph.counts(), (1, 2, 2, 2));
    assert_ne!(Some(first_tab), Some(second_tab));
    assert_eq!(state.graph.content_snapshots().len(), 1);
    assert_eq!(
        state.content_for_buffer(buffer),
        Some(state.graph.content_snapshots()[0].id)
    );
}

#[test]
fn reset_then_session_shaped_state_has_no_stale_resources() {
    let mut first = EditorState::new(PathBuf::from("/workspace"));
    first
        .open_text(
            TextBuffer::from_text("old"),
            Some(PathBuf::from("/workspace/old.rs")),
        )
        .unwrap();
    reconcile(&mut first);
    first.close_active_view().unwrap();
    let buffers = first.buffers.keys().collect::<Vec<_>>();
    for buffer in buffers {
        first.remove_buffer(buffer);
    }
    reconcile(&mut first);
    assert_eq!(first.graph.counts(), (0, 0, 0, 0));

    let mut restored = EditorState::new(PathBuf::from("/workspace"));
    restored
        .open_text(
            TextBuffer::from_text("new"),
            Some(PathBuf::from("/workspace/new.rs")),
        )
        .unwrap();
    restored.new_tab().expect("restored second tab");
    reconcile(&mut restored);
    assert_eq!(restored.graph.counts(), (1, 2, 2, 2));
    assert_eq!(restored.graph.content_snapshots()[0].title, "new.rs");
}

#[test]
fn native_reset_reconstructs_runtime_ids_but_retains_semantic_policy() {
    let mut bridge = NativeSurfaces::default();
    let mut graph = WindowGraph::default();
    bridge
        .set_configured_placements(BTreeMap::from([(
            "builtin.dashboard".into(),
            WindowPlacement::Fullscreen,
        )]))
        .expect("configured policy");
    bridge
        .set_semantic_default(
            "builtin.explorer",
            WindowPlacement::Docked {
                edge: crate::windows::DockEdge::Right,
                size: 29,
            },
        )
        .expect("restored policy");
    bridge.reset(&mut graph).expect("reset native runtime");
    assert_eq!(
        bridge.semantic_defaults(None),
        vec![(
            "builtin.explorer",
            WindowPlacement::Docked {
                edge: crate::windows::DockEdge::Right,
                size: 29,
            }
        )]
    );
    assert_eq!(
        bridge.set_semantic_default("builtin.terminal", WindowPlacement::Fullscreen),
        Err(WindowError::MultipleFullscreenWindows)
    );
}

#[test]
fn plugin_focus_survives_metadata_reconciliation_and_tab_close() {
    let mut state = crate::editor::EditorState::new("/workspace".into());
    let (buffer, _) = state
        .open_text(crate::editor::text::TextBuffer::from_text("a"), None)
        .unwrap();
    state.new_tab().unwrap();
    reconcile(&mut state);
    let tab = state.graph.active_tab().unwrap();
    let content = state
        .graph
        .create_content(
            ContentTypeId::new("plugin.demo.panel").unwrap(),
            ContentAdapterKind::DeclarativeUi,
            "Panel".into(),
            false,
            "plugin.demo",
            1,
        )
        .unwrap();
    let view = state.graph.create_view(content, "plugin.demo", 1).unwrap();
    let panel = state
        .graph
        .open_window(
            tab,
            view,
            WindowPlacement::Floating(WindowRect {
                x: 1,
                y: 1,
                width: 20,
                height: 5,
            }),
            true,
            "plugin.demo",
            1,
        )
        .unwrap();
    state.set_buffer_dirty(buffer, true);
    reconcile(&mut state);
    assert_eq!(state.graph.focused(), Some(panel));
    state
        .open_text(TextBuffer::from_text("another file"), None)
        .unwrap();
    reconcile(&mut state);
    let pane = state
        .tab_focused_window(state.active_tab().unwrap())
        .unwrap();
    assert_eq!(
        state.graph.focused(),
        state.graph.window(pane).map(|window| window.id)
    );
    state.graph.focus(panel).unwrap();
    state.close_active_tab().unwrap();
    reconcile(&mut state);
    assert_ne!(state.graph.window(panel).unwrap().tab, tab);
    assert_eq!(state.graph.focused(), Some(panel));
    state.graph.validate().unwrap();
}

#[test]
fn closing_the_final_document_keeps_a_populated_plugin_host() {
    let mut state = EditorState::new("/workspace".into());
    let (buffer, _) = state.open_text(TextBuffer::from_text("a"), None).unwrap();
    reconcile(&mut state);
    let tab = state.graph.active_tab().unwrap();
    let content = state
        .graph
        .create_content(
            ContentTypeId::new("plugin.demo.panel").unwrap(),
            ContentAdapterKind::DeclarativeUi,
            "Panel".into(),
            false,
            "plugin.demo",
            1,
        )
        .unwrap();
    let view = state.graph.create_view(content, "plugin.demo", 1).unwrap();
    let panel = state
        .graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "plugin.demo", 1)
        .unwrap();
    state.close_active_view().unwrap();
    state.remove_buffer(buffer).unwrap();
    reconcile(&mut state);
    assert!(state.active_tab().is_none());
    assert_eq!(state.graph.window(panel).unwrap().tab, tab);
    assert!(state.graph.workspace_tab(tab).is_some());
    state.graph.validate().unwrap();

    let mut native = NativeSurfaces::default();
    let signature = NativeSurfaceSignature {
        editor_tab: None,
        dashboard_visible: true,
        ..native_signature(tab)
    };
    native
        .reconcile(
            &mut state.graph,
            signature.clone(),
            "Explorer",
            "Dashboard",
            "Picker",
            &[],
        )
        .unwrap();
    assert_eq!(native.workspace_tab(), Some(tab));
    assert_eq!(state.graph.window(panel).unwrap().tab, tab);
    state.graph.validate().unwrap();

    state
        .graph
        .place(panel, WindowPlacement::Fullscreen)
        .unwrap();
    native
        .reconcile(
            &mut state.graph,
            signature,
            "Explorer",
            "Dashboard",
            "Picker",
            &[],
        )
        .unwrap();
    assert_eq!(state.graph.focused(), Some(panel));
    state.graph.validate().unwrap();
}

fn native_signature(tab: crate::windows::WorkspaceTabId) -> NativeSurfaceSignature {
    NativeSurfaceSignature {
        editor_tab: Some(tab),
        editor_window: None,
        focused_plugin: None,
        explorer_visible: false,
        explorer_views: Arc::from([1]),
        focused_explorer: None,
        explorer_size: 20,
        explorer_edge: crate::windows::DockEdge::Left,
        active_terminal: None,
        shown_terminals: Arc::from([]),
        terminal_focused: false,
        terminal_size: 8,
        terminal_shrunk: false,
        terminal_set_revision: Some(1),
        terminal_width: 80,
        terminal_height: 24,
        dashboard_visible: false,
        picker_rect: None,
        picker_surface: None,
        picker_title: "Picker".into(),
        protected_prompt_height: 0,
        configuration_reload_id: 0,
        screen_bottom: 0,
    }
}

#[test]
fn native_display_titles_are_sanitized_before_graph_reconciliation() {
    let mut graph = WindowGraph::default();
    let tab = graph.create_workspace_tab("fixture", 1).unwrap();
    let mut native = NativeSurfaces::default();
    let mut signature = native_signature(tab);
    signature.explorer_visible = true;
    signature.dashboard_visible = true;
    signature.picker_rect = Some(WindowRect {
        x: 1,
        y: 1,
        width: 30,
        height: 10,
    });
    signature.picker_surface = Some(crate::ui::PickerSurfaceKind::Generic);
    let terminals = [(1, "Terminal\nprofile".into(), ContentLifecycle::Running)];
    for title in ["Action\nname\t\u{1b}", &"é".repeat(1024)] {
        native
            .reconcile(
                &mut graph,
                signature.clone(),
                title,
                title,
                title,
                &terminals,
            )
            .unwrap();
        for ids in [
            native.explorer.unwrap(),
            native.dashboard.unwrap(),
            native.picker().unwrap(),
        ] {
            let actual = &graph.content(ids.content).unwrap().title;
            assert_eq!(actual, &crate::app::windows::content::sanitize_title(title));
            assert!(!actual.chars().any(char::is_control));
        }
        assert_eq!(
            graph.content(native.terminals[&1].content).unwrap().title,
            "Terminal\u{fffd}profile"
        );
    }
}

#[test]
fn fullscreen_terminal_switch_hides_the_outgoing_host_before_showing_a_picker() {
    let mut graph = WindowGraph::default();
    let tab = graph.create_workspace_tab("builtin.editor", 1).unwrap();
    let mut native = NativeSurfaces::default();
    native.prefer_terminal_placement(1, WindowPlacement::Fullscreen);
    native.prefer_terminal_placement(2, WindowPlacement::Fullscreen);
    let terminals = [
        (1, "one".into(), ContentLifecycle::Running),
        (2, "two".into(), ContentLifecycle::Running),
    ];
    let mut signature = native_signature(tab);
    signature.active_terminal = Some(2);
    signature.terminal_focused = true;
    native
        .reconcile(
            &mut graph,
            signature.clone(),
            "Explorer",
            "Dashboard",
            "Picker",
            &terminals,
        )
        .unwrap();
    signature.active_terminal = Some(1);
    native
        .reconcile(
            &mut graph,
            signature.clone(),
            "Explorer",
            "Dashboard",
            "Picker",
            &terminals,
        )
        .unwrap();
    assert_eq!(
        graph.window(native.terminals[&2].window).unwrap().placement,
        WindowPlacement::Hidden
    );
    let fullscreen = native.terminals[&1].window;
    assert_eq!(graph.focused(), Some(fullscreen));

    let rect = WindowRect {
        x: 2,
        y: 2,
        width: 50,
        height: 15,
    };
    signature.picker_rect = Some(rect);
    signature.picker_surface = Some(crate::ui::PickerSurfaceKind::Output);
    signature.picker_title = "Output A".into();
    native
        .reconcile(
            &mut graph,
            signature.clone(),
            "Explorer",
            "Dashboard",
            "Output A",
            &terminals,
        )
        .unwrap();
    let picker = native.picker().unwrap();
    assert_eq!(graph.focused(), Some(picker.window));
    assert_eq!(
        graph.window(fullscreen).unwrap().placement,
        WindowPlacement::Fullscreen
    );
    let projection = graph.compositor_snapshot(tab).unwrap().project(
        WindowRect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        },
        1,
        1,
    );
    assert!(projection.window(picker.window).is_some());
    signature.picker_title = "Output B".into();
    assert!(!native.is_current(&signature));
    native
        .reconcile(
            &mut graph,
            signature,
            "Explorer",
            "Dashboard",
            "Output B",
            &terminals,
        )
        .unwrap();
    assert_eq!(graph.content(picker.content).unwrap().title, "Output B");
    graph.validate().unwrap();
}

#[test]
fn remembered_plugin_focus_returns_after_terminal_and_picker_capture() {
    let mut graph = WindowGraph::default();
    let tab = graph.create_workspace_tab("builtin.editor", 1).unwrap();
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
        .open_window(tab, view, WindowPlacement::Tiled, true, "plugin.demo", 1)
        .unwrap();
    let mut native = NativeSurfaces::default();
    let mut signature = native_signature(tab);
    signature.active_terminal = Some(1);
    signature.terminal_focused = true;
    let terminals = [(1, "shell".into(), ContentLifecycle::Running)];
    native
        .reconcile(
            &mut graph,
            signature.clone(),
            "Explorer",
            "Dashboard",
            "Picker",
            &terminals,
        )
        .unwrap();
    assert_eq!(graph.focused(), Some(native.terminals[&1].window));
    graph
        .place(native.terminals[&1].window, WindowPlacement::Hidden)
        .unwrap();
    signature.active_terminal = None;
    signature.terminal_focused = false;
    signature.focused_plugin = Some(panel);
    native
        .reconcile(
            &mut graph,
            signature.clone(),
            "Explorer",
            "Dashboard",
            "Picker",
            &terminals,
        )
        .unwrap();
    assert_eq!(graph.focused(), Some(panel));
    signature.picker_rect = Some(WindowRect {
        x: 1,
        y: 1,
        width: 30,
        height: 10,
    });
    signature.picker_surface = Some(crate::ui::PickerSurfaceKind::Generic);
    native
        .reconcile(
            &mut graph,
            signature.clone(),
            "Explorer",
            "Dashboard",
            "Picker",
            &terminals,
        )
        .unwrap();
    assert_eq!(graph.focused(), Some(native.picker().unwrap().window));
    signature.picker_rect = None;
    signature.picker_surface = None;
    native
        .reconcile(
            &mut graph,
            signature,
            "Explorer",
            "Dashboard",
            "Picker",
            &terminals,
        )
        .unwrap();
    assert_eq!(graph.focused(), Some(panel));
}

#[test]
fn explorer_reopen_clones_and_debug_terminal_sizes_use_effective_placements() {
    let mut graph = WindowGraph::default();
    let tab = graph.create_workspace_tab("builtin.editor", 1).unwrap();
    let mut native = NativeSurfaces::default();
    let right = WindowPlacement::Docked {
        edge: crate::windows::DockEdge::Right,
        size: 27,
    };
    native
        .set_configured_placements(BTreeMap::from([("builtin.explorer".into(), right)]))
        .unwrap();
    let mut signature = NativeSurfaceSignature {
        editor_tab: Some(tab),
        editor_window: None,
        focused_plugin: None,
        explorer_visible: true,
        explorer_views: Arc::from([1, 2]),
        focused_explorer: Some(1),
        explorer_size: 20,
        explorer_edge: crate::windows::DockEdge::Left,
        active_terminal: Some(1),
        shown_terminals: Arc::from([1]),
        terminal_focused: false,
        terminal_size: 12,
        terminal_shrunk: false,
        terminal_set_revision: Some(1),
        terminal_width: 80,
        terminal_height: 24,
        dashboard_visible: false,
        picker_rect: None,
        picker_surface: None,
        picker_title: "Picker".into(),
        protected_prompt_height: 0,
        configuration_reload_id: 0,
        screen_bottom: 0,
    };
    let terminals = [(1, "shell".into(), ContentLifecycle::Running)];
    native
        .reconcile(
            &mut graph,
            signature.clone(),
            "Explorer",
            "Dashboard",
            "Picker",
            &terminals,
        )
        .unwrap();
    native.explorer_placement = Some(WindowPlacement::Hidden);
    native
        .show_target(
            crate::app::windows::native::NativeSurfaceTarget::Explorer,
            WindowPlacement::Docked {
                edge: crate::windows::DockEdge::Left,
                size: 20,
            },
            WindowPlacement::Docked {
                edge: crate::windows::DockEdge::Bottom,
                size: 8,
            },
        )
        .unwrap();
    assert_eq!(native.explorer_placement, Some(right));
    native.explorer_placement = Some(WindowPlacement::Hidden);
    signature.configuration_reload_id += 1;
    native
        .reconcile(
            &mut graph,
            signature.clone(),
            "Explorer",
            "Dashboard",
            "Picker",
            &terminals,
        )
        .unwrap();
    assert_ne!(
        graph
            .window(native.explorer.unwrap().window)
            .unwrap()
            .placement,
        WindowPlacement::Hidden
    );
    signature.explorer_visible = false;
    signature.focused_explorer = None;
    native.terminal_placements.insert(
        1,
        WindowPlacement::Docked {
            edge: crate::windows::DockEdge::Bottom,
            size: 12,
        },
    );
    signature.terminal_shrunk = true;
    signature.terminal_size = 5;
    native
        .reconcile(
            &mut graph,
            signature,
            "Explorer",
            "Dashboard",
            "Picker",
            &terminals,
        )
        .unwrap();
    assert_eq!(
        graph
            .window(native.explorer_views[&2].window)
            .unwrap()
            .placement,
        WindowPlacement::Hidden
    );
    assert_eq!(
        graph.window(native.terminals[&1].window).unwrap().placement,
        WindowPlacement::Docked {
            edge: crate::windows::DockEdge::Bottom,
            size: 5
        }
    );
    assert_eq!(
        native.terminal_placements[&1],
        WindowPlacement::Docked {
            edge: crate::windows::DockEdge::Bottom,
            size: 12
        }
    );
}
