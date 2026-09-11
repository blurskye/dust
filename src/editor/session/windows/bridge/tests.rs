use std::{collections::BTreeMap, path::PathBuf};

use crate::{
    editor::text::TextBuffer,
    editor::{Axis, EditorState},
    ui::window::{ContentAdapterKind, ContentTypeId, WindowError, WindowGraph, WindowPlacement},
};

use super::{EditorWindowBridge, NativeSurfaceBridge};

fn reconcile(bridge: &mut EditorWindowBridge, graph: &mut WindowGraph, state: &EditorState) {
    bridge
        .reconcile(&state.into(), graph, "[scratch]")
        .expect("reconcile graph");
}

#[test]
fn same_content_has_independent_views_and_windows_with_stable_focus_identity() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, first_view) = state.open_text(
        TextBuffer::from_text("one"),
        Some(PathBuf::from("/workspace/main.rs")),
    );
    let first_pane = state.tabs[state.active_tab.expect("tab")].focused;
    let second_view = state.split_active(Axis::Horizontal, 500).expect("split");
    let second_pane = state.tabs[state.active_tab.expect("tab")].focused;
    let mut bridge = EditorWindowBridge::default();
    let mut graph = WindowGraph::default();
    reconcile(&mut bridge, &mut graph, &state);
    assert!(bridge.is_current(&(&state).into(), "[scratch]"));

    let content = bridge.content_for(buffer).expect("content");
    let first = bridge.view_for(first_view).expect("first view");
    let second = bridge.view_for(second_view).expect("second view");
    assert_ne!(first, second);
    assert_eq!(graph.view(first).expect("first").content, content);
    assert_eq!(graph.view(second).expect("second").content, content);
    let first_window = bridge.window_for(first_pane).expect("first window");
    let second_window = bridge.window_for(second_pane).expect("second window");
    assert_eq!(graph.focused(), Some(second_window));

    state.focus_pane_id(first_pane).expect("focus first");
    reconcile(&mut bridge, &mut graph, &state);
    assert_eq!(bridge.window_for(first_pane), Some(first_window));
    assert_eq!(bridge.window_for(second_pane), Some(second_window));
    assert_eq!(graph.focused(), Some(first_window));
    assert_eq!(graph.content_snapshots()[0].view_count, 2);
    let tab = bridge.tab_for(state.active_tab.expect("tab")).expect("tab");
    let before = graph.tiled_layout_snapshot(tab).expect("layout").project(
        crate::ui::window::WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 20,
        },
        1,
        1,
    );
    state.resize_active_pane(100).expect("resize");
    assert!(!bridge.is_current(&(&state).into(), "[scratch]"));
    reconcile(&mut bridge, &mut graph, &state);
    let after = graph.tiled_layout_snapshot(tab).expect("layout").project(
        crate::ui::window::WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 20,
        },
        1,
        1,
    );
    assert_eq!(bridge.window_for(first_pane), Some(first_window));
    assert_eq!(bridge.window_for(second_pane), Some(second_window));
    assert!(after.windows[0].rect.width > before.windows[0].rect.width);
    assert!(bridge.is_current(&(&state).into(), "[scratch]"));
}

#[test]
fn editor_reconciliation_preserves_foreign_tiled_windows_and_identity() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state.open_text(TextBuffer::from_text("one"), None);
    let mut bridge = EditorWindowBridge::default();
    let mut graph = WindowGraph::default();
    reconcile(&mut bridge, &mut graph, &state);
    let tab = bridge
        .tab_for(state.active_tab.expect("tab"))
        .expect("graph tab");
    let content = graph
        .create_content(
            ContentTypeId::new("terminal.external-tui").unwrap(),
            ContentAdapterKind::ExternalTui,
            "external".into(),
            false,
            "external",
            1,
        )
        .unwrap();
    let view = graph.create_view(content, "external", 1).unwrap();
    let window = graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "external", 1)
        .unwrap();

    state.set_buffer_dirty(state.active_buffer_id().expect("buffer"), true);
    reconcile(&mut bridge, &mut graph, &state);
    assert_eq!(graph.window(window).expect("foreign tile").view, view);
    assert_eq!(graph.view(view).expect("foreign view").content, content);
    assert_eq!(
        graph
            .window(window)
            .expect("foreign tiled placement")
            .placement,
        WindowPlacement::Tiled
    );
    assert!(
        graph
            .tiled_layout_snapshot(tab)
            .expect("mixed tiled layout")
            .project(
                crate::ui::window::WindowRect {
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
fn pane_close_retires_only_its_view_and_keeps_shared_content() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, _) = state.open_text(TextBuffer::from_text("one"), None);
    state.split_active(Axis::Vertical, 500).expect("split");
    let mut bridge = EditorWindowBridge::default();
    let mut graph = WindowGraph::default();
    reconcile(&mut bridge, &mut graph, &state);
    let content = bridge.content_for(buffer).expect("content");

    state.close_active_pane().expect("close pane");
    reconcile(&mut bridge, &mut graph, &state);
    assert!(graph.content(content).is_some());
    assert_eq!(graph.counts(), (1, 1, 1, 1));
}

#[test]
fn workspace_tabs_are_distinct_from_one_shared_buffer_entry() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, _) = state.open_text(TextBuffer::from_text("one"), None);
    let first_tab = state.active_tab.expect("tab");
    let second_tab = state.new_tab().expect("new tab");
    let mut bridge = EditorWindowBridge::default();
    let mut graph = WindowGraph::default();
    reconcile(&mut bridge, &mut graph, &state);

    assert_eq!(graph.counts(), (1, 2, 2, 2));
    assert_ne!(bridge.tab_for(first_tab), bridge.tab_for(second_tab));
    assert_eq!(graph.content_snapshots().len(), 1);
    assert_eq!(
        bridge.content_for(buffer),
        Some(graph.content_snapshots()[0].id)
    );
}

#[test]
fn reset_then_session_shaped_state_has_no_stale_resources() {
    let mut first = EditorState::new(PathBuf::from("/workspace"));
    first.open_text(
        TextBuffer::from_text("old"),
        Some(PathBuf::from("/workspace/old.rs")),
    );
    let mut bridge = EditorWindowBridge::default();
    let mut graph = WindowGraph::default();
    reconcile(&mut bridge, &mut graph, &first);
    bridge.reset(&mut graph).expect("reset graph");
    assert_eq!(graph.counts(), (0, 0, 0, 0));

    let mut restored = EditorState::new(PathBuf::from("/workspace"));
    restored.open_text(
        TextBuffer::from_text("new"),
        Some(PathBuf::from("/workspace/new.rs")),
    );
    restored.new_tab().expect("restored second tab");
    reconcile(&mut bridge, &mut graph, &restored);
    assert_eq!(graph.counts(), (1, 2, 2, 2));
    assert_eq!(graph.content_snapshots()[0].title, "new.rs");
}

#[test]
fn native_reset_reconstructs_runtime_ids_but_retains_semantic_policy() {
    let mut bridge = NativeSurfaceBridge::default();
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
                edge: crate::ui::window::DockEdge::Right,
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
                edge: crate::ui::window::DockEdge::Right,
                size: 29,
            }
        )]
    );
    assert_eq!(
        bridge.set_semantic_default("builtin.terminal", WindowPlacement::Fullscreen),
        Err(WindowError::MultipleFullscreenWindows)
    );
}
