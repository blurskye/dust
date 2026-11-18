//! Window topology, resource limits, focus and compositor behavior.

use crate::windows::CompositorLayer;
use crate::windows::ContentAdapterKind;
use crate::windows::ContentCapabilities;
use crate::windows::ContentContract;
use crate::windows::ContentId;
use crate::windows::ContentLifecycle;
use crate::windows::ContentTypeId;
use crate::windows::DockEdge;
use crate::windows::MAX_SPLIT_DEPTH;
use crate::windows::MAX_WINDOWS;
use crate::windows::ProjectedWindow;
use crate::windows::SpatialDirection;
use crate::windows::TileAxis;
use crate::windows::TiledLayoutNode;
use crate::windows::ViewDescriptor;
use crate::windows::ViewId;
use crate::windows::WindowDescriptor;
use crate::windows::WindowError;
use crate::windows::WindowGraph;
use crate::windows::WindowId;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use crate::windows::WindowTarget;
use crate::windows::WorkspaceTabId;

fn balanced_layout(windows: &[WindowId]) -> TiledLayoutNode {
    if windows.len() == 1 {
        return TiledLayoutNode::Window(windows[0]);
    }
    let middle = windows.len() / 2;
    TiledLayoutNode::Split {
        axis: TileAxis::Horizontal,
        ratio: 500,
        first: Box::new(balanced_layout(&windows[..middle])),
        second: Box::new(balanced_layout(&windows[middle..])),
    }
}

#[test]
fn hidden_and_floating_tiles_keep_their_split_positions_and_ratios() {
    let (mut graph, content, view, tab) = graph_with_content();
    let first = graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    let view = graph.create_view(content, "builtin", 1).unwrap();
    let middle = graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    graph
        .tile(middle, Some(first), TileAxis::Vertical, true, 320)
        .unwrap();
    let view = graph.create_view(content, "builtin", 1).unwrap();
    let last = graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    graph
        .tile(last, Some(middle), TileAxis::Horizontal, true, 670)
        .unwrap();
    let original = graph.tiled_layout_snapshot(tab).unwrap();
    for placement in [
        WindowPlacement::Hidden,
        WindowPlacement::Floating(WindowRect {
            x: 2,
            y: 3,
            width: 20,
            height: 8,
        }),
    ] {
        graph.place(middle, placement).unwrap();
        assert_eq!(
            graph.logical_layout(tab).unwrap().windows(),
            vec![first, middle, last]
        );
        assert_eq!(
            graph.tiled_layout_snapshot(tab).unwrap().root.windows(),
            vec![first, last]
        );
        graph.place(middle, WindowPlacement::Tiled).unwrap();
        assert_eq!(graph.tiled_layout_snapshot(tab).unwrap(), original);
    }
    graph.place(middle, WindowPlacement::Hidden).unwrap();
    graph.close_window(middle).unwrap();
    assert_eq!(
        graph.logical_layout(tab).unwrap().windows(),
        vec![first, last]
    );
    graph.validate().unwrap();
}

#[test]
fn clearing_a_session_does_not_reuse_runtime_window_or_tab_ids() {
    let (mut graph, _, view, tab) = graph_with_content();
    let old = graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    graph.clear().unwrap();
    assert_eq!(graph.counts(), (0, 0, 0, 0));
    let next_tab = graph.create_workspace_tab("builtin", 1).unwrap();
    let content = graph
        .create_content(
            ContentTypeId::new("builtin.text-buffer").unwrap(),
            ContentAdapterKind::TextBuffer,
            "new".into(),
            false,
            "builtin",
            1,
        )
        .unwrap();
    let view = graph.create_view(content, "builtin", 1).unwrap();
    let next = graph
        .open_window(next_tab, view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    assert_ne!(next_tab, tab);
    assert_ne!(next, old);
    assert!(graph.window(old).is_none());
    assert!(graph.workspace_tab(tab).is_none());
}

fn graph_with_content() -> (WindowGraph, ContentId, ViewId, WorkspaceTabId) {
    let mut graph = WindowGraph::default();
    // Topology tests intentionally need many views of one content. A normal
    // terminal uses its adapter default and rejects clones.
    let mut capabilities = ContentCapabilities::for_adapter(ContentAdapterKind::TerminalSession);
    capabilities.clone_view = true;
    let content = graph
        .create_content_with_contract(
            ContentTypeId::new("builtin.terminal").expect("type"),
            ContentContract::declared(ContentAdapterKind::TerminalSession, capabilities),
            "shell".into(),
            false,
            "builtin",
            1,
        )
        .expect("content");
    let view = graph.create_view(content, "builtin", 1).expect("view");
    let tab = graph
        .create_workspace_tab("builtin", 1)
        .expect("workspace tab");
    (graph, content, view, tab)
}

#[test]
fn one_content_view_and_window_move_without_identity_replacement() {
    let (mut graph, content, view, tab) = graph_with_content();
    let window = graph
        .open_window(
            tab,
            view,
            WindowPlacement::Docked {
                edge: DockEdge::Bottom,
                size: 12,
            },
            true,
            "builtin",
            1,
        )
        .expect("window");
    let floating = WindowPlacement::Floating(WindowRect {
        x: 5,
        y: 3,
        width: 80,
        height: 24,
    });
    graph.place(window, floating).expect("float");
    assert_eq!(
        graph.window(window).expect("same window").placement,
        floating
    );
    assert_eq!(graph.window(window).expect("same window").view, view);
    assert_eq!(graph.view(view).expect("same view").content, content);
    assert_eq!(graph.focused(), Some(window));
    graph.validate().expect("valid floating graph");
    graph
        .place(window, WindowPlacement::Fullscreen)
        .expect("fullscreen");
    assert_eq!(
        graph.window(window).expect("same window").placement,
        WindowPlacement::Fullscreen
    );
    assert_eq!(graph.window(window).expect("same window").view, view);
    assert_eq!(graph.view(view).expect("same view").content, content);
    assert_eq!(graph.focused(), Some(window));
    graph.validate().expect("valid graph");
}

#[test]
fn content_can_own_independent_views_but_each_view_has_one_window() {
    let (mut graph, content, first_view, tab) = graph_with_content();
    let second_view = graph.create_view(content, "builtin", 1).expect("view");
    let first = graph
        .open_window(
            tab,
            first_view,
            WindowPlacement::Fullscreen,
            true,
            "builtin",
            1,
        )
        .expect("first");
    graph
        .open_window(
            tab,
            second_view,
            WindowPlacement::Floating(WindowRect {
                x: 1,
                y: 1,
                width: 10,
                height: 5,
            }),
            true,
            "builtin",
            1,
        )
        .expect("second");
    assert_eq!(
        graph.open_window(
            tab,
            first_view,
            WindowPlacement::Fullscreen,
            true,
            "builtin",
            1,
        ),
        Err(WindowError::ViewAlreadyHosted)
    );
    graph.close_window(first).expect("close first window");
    graph.remove_view(first_view).expect("remove first view");
    assert!(graph.content(content).is_some());
    assert_eq!(graph.content_snapshots()[0].view_count, 1);
}

#[test]
fn tiled_topology_rejects_stale_duplicate_orphan_and_excess_depth() {
    let (mut graph, content, first_view, tab) = graph_with_content();
    let second_view = graph.create_view(content, "builtin", 1).expect("view");
    let first = graph
        .open_window(tab, first_view, WindowPlacement::Tiled, true, "builtin", 1)
        .expect("first");
    let second = graph
        .open_window(tab, second_view, WindowPlacement::Tiled, true, "builtin", 1)
        .expect("second");
    let split = |left, right| TiledLayoutNode::Split {
        axis: TileAxis::Horizontal,
        ratio: 300,
        first: Box::new(TiledLayoutNode::Window(left)),
        second: Box::new(TiledLayoutNode::Window(right)),
    };
    assert_eq!(
        graph.set_tiled_layout(tab, Some(split(first, first))),
        Err(WindowError::DuplicateTiledWindow)
    );
    assert_eq!(
        graph.set_tiled_layout(tab, Some(split(first, WindowId(999)))),
        Err(WindowError::StaleTiledWindow)
    );
    assert_eq!(
        graph.set_tiled_layout(tab, Some(TiledLayoutNode::Window(first))),
        Err(WindowError::OrphanTiledWindow)
    );
    let floating_view = graph
        .create_view(content, "builtin", 1)
        .expect("floating view");
    let floating = graph
        .open_window(
            tab,
            floating_view,
            WindowPlacement::Floating(WindowRect {
                x: 1,
                y: 1,
                width: 10,
                height: 5,
            }),
            true,
            "builtin",
            1,
        )
        .expect("floating window");
    assert_eq!(
        graph.set_tiled_layout(tab, Some(split(first, floating))),
        Err(WindowError::NonTiledWindowInLayout)
    );
    let foreign_view = graph
        .create_view(content, "builtin", 1)
        .expect("foreign view");
    let foreign_tab = graph
        .create_workspace_tab("builtin", 1)
        .expect("foreign tab");
    let foreign = graph
        .open_window(
            foreign_tab,
            foreign_view,
            WindowPlacement::Tiled,
            true,
            "builtin",
            1,
        )
        .expect("foreign tiled window");
    assert_eq!(
        graph.set_tiled_layout(tab, Some(split(first, foreign))),
        Err(WindowError::NonTiledWindowInLayout)
    );
    let mut too_deep = TiledLayoutNode::Window(WindowId(999));
    for _ in 0..MAX_SPLIT_DEPTH {
        too_deep = TiledLayoutNode::Split {
            axis: TileAxis::Horizontal,
            ratio: 500,
            first: Box::new(too_deep),
            second: Box::new(TiledLayoutNode::Window(WindowId(998))),
        };
    }
    assert_eq!(
        graph.set_tiled_layout(tab, Some(too_deep)),
        Err(WindowError::TiledLayoutTooDeep)
    );
    graph
        .set_tiled_layout(tab, Some(split(first, second)))
        .expect("valid layout");
    graph.validate().expect("valid graph");
}

#[test]
fn window_limit_rejects_even_a_shallow_balanced_tree() {
    let (mut graph, content, first_view, tab) = graph_with_content();
    let mut windows = Vec::new();
    for index in 0..=MAX_WINDOWS {
        let view = if index == 0 {
            first_view
        } else {
            let id = ViewId(u64::try_from(index + 1).expect("view id"));
            graph.views.insert(
                id,
                ViewDescriptor {
                    id,
                    content,
                    owner: "builtin".into(),
                    instance_id: 1,
                },
            );
            graph.next_view = graph.next_view.max(id.0);
            id
        };
        let window = WindowId(u64::try_from(index + 1).expect("window id"));
        graph.windows.insert(
            window,
            WindowDescriptor {
                id: window,
                tab,
                view,
                placement: WindowPlacement::Tiled,
                restore_placement: WindowPlacement::Tiled,
                focusable: true,
                owner: "builtin".into(),
                instance_id: 1,
            },
        );
        graph.next_window = graph.next_window.max(window.0);
        windows.push(window);
    }
    graph.workspace_tabs.get_mut(&tab).expect("tab").tiled_root = Some(balanced_layout(&windows));
    assert_eq!(graph.validate(), Err(WindowError::TooManyTiledWindows));
}

#[test]
fn checked_topology_mutations_reject_without_partial_graph_changes() {
    let (mut graph, content, first_view, tab) = graph_with_content();
    let mut tiled = Vec::new();
    for index in 0..MAX_WINDOWS {
        let view = if index == 0 {
            first_view
        } else {
            graph.create_view(content, "builtin", 1).expect("view")
        };
        tiled.push(
            graph
                .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
                .expect("window within topology limit"),
        );
    }
    graph.validate().expect("graph at the limit remains valid");

    let rejected_view = graph
        .create_view(content, "builtin", 1)
        .expect("extra view");
    let before_open = graph.clone();
    assert_eq!(
        graph.open_window(
            tab,
            rejected_view,
            WindowPlacement::Tiled,
            true,
            "builtin",
            1
        ),
        Err(WindowError::TooManyTiledWindows)
    );
    assert_eq!(graph, before_open);

    let floating_view = graph
        .create_view(content, "builtin", 1)
        .expect("floating view");
    let floating = graph
        .open_window(
            tab,
            floating_view,
            WindowPlacement::Floating(WindowRect {
                x: 2,
                y: 2,
                width: 20,
                height: 8,
            }),
            true,
            "builtin",
            1,
        )
        .expect("floating window");
    let before_place = graph.clone();
    assert_eq!(
        graph.place(floating, WindowPlacement::Tiled),
        Err(WindowError::TooManyTiledWindows)
    );
    assert_eq!(graph, before_place);

    let source_tab = graph
        .create_workspace_tab("builtin", 1)
        .expect("source tab");
    graph
        .rehost(
            floating,
            source_tab,
            floating_view,
            WindowPlacement::Tiled,
            true,
        )
        .expect("tile in source tab");
    let before_rehost = graph.clone();
    assert_eq!(
        graph.rehost(floating, tab, floating_view, WindowPlacement::Tiled, true),
        Err(WindowError::TooManyTiledWindows)
    );
    assert_eq!(graph, before_rehost);

    let before_stale = graph.clone();
    assert_eq!(
        graph.resize_tiled_split(tab, WindowId(u64::MAX), 100),
        Err(WindowError::WindowNotInTiledLayout)
    );
    assert_eq!(
        graph.swap_tiled_windows(tab, tiled[0], WindowId(u64::MAX)),
        Err(WindowError::WindowNotInTiledLayout)
    );
    assert_eq!(graph, before_stale);
    graph.validate().expect("all rejected mutations are atomic");
}

#[test]
fn depth_limit_refusals_preserve_the_entire_graph() {
    let (mut graph, content, view, tab) = graph_with_content();
    let first = graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
        .expect("first window");
    let mut root = TiledLayoutNode::Window(first);
    for _ in 1..MAX_SPLIT_DEPTH {
        let view = graph.create_view(content, "builtin", 1).expect("view");
        let window = graph
            .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
            .expect("window");
        root = TiledLayoutNode::Split {
            axis: TileAxis::Horizontal,
            ratio: 500,
            first: Box::new(root),
            second: Box::new(TiledLayoutNode::Window(window)),
        };
    }
    graph
        .set_tiled_layout(tab, Some(root.clone()))
        .expect("maximum depth");
    graph.validate().expect("valid at depth limit");

    let view = graph
        .create_view(content, "builtin", 1)
        .expect("extra view");
    let extra = graph
        .open_window(tab, view, WindowPlacement::Hidden, true, "builtin", 1)
        .expect("hidden window");
    let before = graph.clone();
    assert_eq!(
        graph.tile(extra, Some(first), TileAxis::Vertical, true, 500),
        Err(WindowError::TiledLayoutTooDeep)
    );
    assert_eq!(graph, before);

    let too_deep = TiledLayoutNode::Split {
        axis: TileAxis::Horizontal,
        ratio: 500,
        first: Box::new(root),
        second: Box::new(TiledLayoutNode::Window(first)),
    };
    assert_eq!(
        graph.set_tiled_layout(tab, Some(too_deep)),
        Err(WindowError::TiledLayoutTooDeep)
    );
    assert_eq!(graph, before);
    graph
        .validate()
        .expect("refused mutations leave a valid graph");
}

#[test]
fn nested_projection_tiny_fallback_resize_and_swap_preserve_window_ids() {
    let (mut graph, content, first_view, tab) = graph_with_content();
    let second_view = graph
        .create_view(content, "builtin", 1)
        .expect("second view");
    let third_view = graph
        .create_view(content, "builtin", 1)
        .expect("third view");
    let open = |graph: &mut WindowGraph, view| {
        graph
            .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
            .expect("window")
    };
    let first = open(&mut graph, first_view);
    let second = open(&mut graph, second_view);
    let third = open(&mut graph, third_view);
    graph
        .set_tiled_layout(
            tab,
            Some(TiledLayoutNode::Split {
                axis: TileAxis::Horizontal,
                ratio: 300,
                first: Box::new(TiledLayoutNode::Window(first)),
                second: Box::new(TiledLayoutNode::Split {
                    axis: TileAxis::Vertical,
                    ratio: 600,
                    first: Box::new(TiledLayoutNode::Window(second)),
                    second: Box::new(TiledLayoutNode::Window(third)),
                }),
            }),
        )
        .expect("layout");
    graph.focus(third).expect("focus third");
    let snapshot = graph.tiled_layout_snapshot(tab).expect("snapshot");
    let projected = snapshot.project(
        WindowRect {
            x: 0,
            y: 1,
            width: 100,
            height: 30,
        },
        1,
        1,
    );
    assert_eq!(projected.windows.len(), 3);
    assert_eq!(projected.borders.len(), 2);
    assert_eq!(projected.windows[0].window, first);
    assert!(projected.windows[0].rect.width < projected.windows[1].rect.width);

    let tiny = snapshot.project(
        WindowRect {
            x: 0,
            y: 0,
            width: 8,
            height: 3,
        },
        5,
        2,
    );
    assert!(tiny.collapsed_to_focused);
    assert_eq!(
        tiny.windows,
        vec![ProjectedWindow {
            window: third,
            rect: WindowRect {
                x: 0,
                y: 0,
                width: 8,
                height: 3
            },
        }]
    );

    graph.resize_tiled_split(tab, first, 100).expect("resize");
    let resized = graph.tiled_layout_snapshot(tab).expect("resized").project(
        WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
        1,
        1,
    );
    assert!(resized.windows[0].rect.width > projected.windows[0].rect.width);
    graph.swap_tiled_windows(tab, first, third).expect("swap");
    let swapped = graph.tiled_layout_snapshot(tab).expect("swapped").project(
        WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
        1,
        1,
    );
    assert_eq!(swapped.windows[0].window, third);
    assert!(graph.window(first).is_some() && graph.window(third).is_some());
    graph.validate().expect("valid graph");
}

#[test]
fn spatial_navigation_prefers_the_topmost_equal_candidate() {
    let (mut graph, content, view, tab) = graph_with_content();
    let tiled = graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    let dock_view = graph.create_view(content, "builtin", 1).unwrap();
    let dock = graph
        .open_window(
            tab,
            dock_view,
            WindowPlacement::Docked {
                edge: DockEdge::Left,
                size: 20,
            },
            true,
            "builtin",
            1,
        )
        .unwrap();
    let float_view = graph.create_view(content, "builtin", 1).unwrap();
    let float = graph
        .open_window(
            tab,
            float_view,
            WindowPlacement::Floating(WindowRect {
                x: 0,
                y: 0,
                width: 20,
                height: 30,
            }),
            true,
            "builtin",
            1,
        )
        .unwrap();
    let projection = graph.compositor_snapshot(tab).unwrap().project(
        WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
        5,
        2,
    );
    assert_eq!(
        projection.window(dock).unwrap().rect,
        projection.window(float).unwrap().rect
    );
    assert_eq!(projection.hit_test(1, 1), Some(float));
    assert_eq!(
        projection.spatial_neighbor(tiled, SpatialDirection::Left),
        Some(float)
    );
}

#[test]
fn docks_reserve_tiled_minimum_and_chrome_on_both_axes() {
    let (mut graph, content, view, tab) = graph_with_content();
    let tiled = graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    for edge in [DockEdge::Bottom, DockEdge::Right] {
        let view = graph.create_view(content, "builtin", 1).unwrap();
        graph
            .open_window(
                tab,
                view,
                WindowPlacement::Docked { edge, size: 100 },
                true,
                "builtin",
                1,
            )
            .unwrap();
    }
    let snapshot = graph.compositor_snapshot(tab).unwrap();
    let insets = crate::windows::TiledInsets {
        screen_bottom: 1,
        top: 1,
        bottom: 2,
        left: 2,
        right: 1,
    };
    let projection = snapshot.project_with_tiled_insets(
        WindowRect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        },
        5,
        2,
        insets,
    );
    assert_eq!(projection.workspace_bounds.width, 8);
    assert_eq!(projection.workspace_bounds.height, 5);
    assert_eq!(projection.window(tiled).unwrap().rect.width, 5);
    assert_eq!(projection.window(tiled).unwrap().rect.height, 2);
    let tiny = snapshot.project_with_tiled_insets(
        WindowRect {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
        },
        5,
        2,
        insets,
    );
    assert_eq!(tiny.workspace_bounds.width, 2);
    assert_eq!(tiny.workspace_bounds.height, 1);
}

#[test]
fn compositor_projects_docks_tiles_floats_fullscreen_and_overlays_deterministically() {
    let (mut graph, content, tiled_view, tab) = graph_with_content();
    let open = |graph: &mut WindowGraph, placement| {
        let view = graph.create_view(content, "builtin", 1).expect("view");
        graph
            .open_window(tab, view, placement, true, "builtin", 1)
            .expect("window")
    };
    let tiled = graph
        .open_window(tab, tiled_view, WindowPlacement::Tiled, true, "builtin", 1)
        .expect("tiled");
    let left = open(
        &mut graph,
        WindowPlacement::Docked {
            edge: DockEdge::Left,
            size: 20,
        },
    );
    let bottom = open(
        &mut graph,
        WindowPlacement::Docked {
            edge: DockEdge::Bottom,
            size: 6,
        },
    );
    let floating = open(
        &mut graph,
        WindowPlacement::Floating(WindowRect {
            x: 95,
            y: 28,
            width: 20,
            height: 10,
        }),
    );
    let overlay = open(
        &mut graph,
        WindowPlacement::Overlay(WindowRect {
            x: 2,
            y: 2,
            width: 12,
            height: 4,
        }),
    );
    let snapshot = graph.compositor_snapshot(tab).expect("snapshot");
    let projection = snapshot.project(
        WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
        5,
        2,
    );
    assert_eq!(projection.window(left).expect("left").rect.width, 20);
    assert_eq!(projection.window(bottom).expect("bottom").rect.height, 6);
    assert_eq!(
        projection.window(tiled).expect("tile").rect,
        WindowRect {
            x: 20,
            y: 0,
            width: 80,
            height: 24,
        }
    );
    assert_eq!(
        projection.window(floating).expect("float projected").rect,
        WindowRect {
            x: 80,
            y: 20,
            width: 20,
            height: 10,
        }
    );
    assert_eq!(
        projection.windows.last().expect("top overlay").window,
        overlay
    );
    assert_eq!(projection.hit_test(3, 3), Some(overlay));
    assert_eq!(
        projection.spatial_neighbor(tiled, SpatialDirection::Left),
        Some(left)
    );
    assert_eq!(
        projection.spatial_neighbor(tiled, SpatialDirection::Down),
        Some(bottom)
    );

    let mut tiny = snapshot.project(
        WindowRect {
            x: 0,
            y: 0,
            width: 4,
            height: 2,
        },
        5,
        2,
    );
    assert_eq!(tiny.workspace_bounds.width, 4);
    assert_eq!(tiny.workspace_bounds.height, 2);
    assert_eq!(
        tiny.window(tiled).expect("tiny tile").rect,
        tiny.tiled_bounds
    );
    let protected = WindowRect {
        x: 0,
        y: 0,
        width: 2,
        height: 1,
    };
    tiny.set_protected_regions([protected]);
    assert_eq!(tiny.protected_regions, vec![protected]);
    assert_eq!(tiny.hit_test(0, 0), None);

    let fullscreen = open(&mut graph, WindowPlacement::Fullscreen);
    let before_second = graph.clone();
    let second_fullscreen_view = graph.create_view(content, "builtin", 1).expect("view");
    let before_rejected = graph.clone();
    assert_eq!(
        graph.open_window(
            tab,
            second_fullscreen_view,
            WindowPlacement::Fullscreen,
            true,
            "builtin",
            1,
        ),
        Err(WindowError::MultipleFullscreenWindows)
    );
    assert_eq!(graph, before_rejected);
    assert_ne!(graph, before_second);
    let fullscreen_projection = graph.compositor_snapshot(tab).expect("snapshot").project(
        WindowRect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        },
        5,
        2,
    );
    assert_eq!(fullscreen_projection.windows[0].window, fullscreen);
    assert_eq!(
        fullscreen_projection.windows[0].layer,
        CompositorLayer::Fullscreen
    );
    assert_eq!(
        fullscreen_projection
            .windows
            .last()
            .expect("overlay")
            .window,
        overlay
    );
    assert!(fullscreen_projection.window(tiled).is_none());
    graph.validate().expect("valid compositor graph");
}

#[test]
fn graph_revision_and_typed_targets_cover_changes_rejection_and_exhaustion() {
    let (mut graph, content, view, tab) = graph_with_content();
    let window = graph
        .open_window(
            tab,
            view,
            WindowPlacement::Docked {
                edge: DockEdge::Left,
                size: 4,
            },
            true,
            "builtin",
            1,
        )
        .expect("window");
    let opened = graph.revision().expect("revision");
    graph
        .update_content(content, "shell".into(), false)
        .expect("no-op update");
    assert_eq!(graph.revision(), Some(opened));
    graph
        .place(
            window,
            WindowPlacement::Floating(WindowRect {
                x: 1,
                y: 2,
                width: 8,
                height: 4,
            }),
        )
        .expect("placement");
    assert_eq!(graph.revision(), Some(opened + 1));
    let before_rejection = graph.clone();
    assert_eq!(
        graph.place(
            WindowId(999_999),
            WindowPlacement::Docked {
                edge: DockEdge::Right,
                size: 2,
            }
        ),
        Err(WindowError::UnknownWindow)
    );
    assert_eq!(graph, before_rejection);
    assert_eq!(graph.resolve_target(&WindowTarget::Id(window)), Ok(window));
    assert_eq!(
        graph.resolve_target(&WindowTarget::ContentType(
            ContentTypeId::new("builtin.terminal").expect("type")
        )),
        Ok(window)
    );
    let ambiguous_view = graph.create_view(content, "builtin", 1).expect("view");
    let ambiguous = graph
        .open_window(
            tab,
            ambiguous_view,
            WindowPlacement::Floating(WindowRect {
                x: 2,
                y: 2,
                width: 3,
                height: 2,
            }),
            true,
            "builtin",
            1,
        )
        .expect("ambiguous window");
    assert_eq!(
        graph.resolve_target(&WindowTarget::ContentType(
            ContentTypeId::new("builtin.terminal").expect("type")
        )),
        Err(WindowError::AmbiguousWindowTarget)
    );
    graph.close_window(ambiguous).expect("close ambiguous");
    graph.remove_view(ambiguous_view).expect("remove view");

    let second_tab = graph.create_workspace_tab("builtin", 1).expect("tab");
    let second_view = graph.create_view(content, "builtin", 1).expect("view");
    let second = graph
        .open_window(
            second_tab,
            second_view,
            WindowPlacement::Floating(WindowRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2,
            }),
            true,
            "builtin",
            1,
        )
        .expect("second");
    graph.set_active_tab(tab).expect("first tab");
    assert_eq!(
        graph.resolve_target(&WindowTarget::Id(second)),
        Err(WindowError::WindowOutsideActiveTab)
    );

    let mut exhausted = graph.clone();
    exhausted.exhaust_revision_for_test();
    let before = exhausted.clone();
    assert_eq!(
        exhausted.update_content(content, "changed".into(), true),
        Err(WindowError::RevisionExhausted)
    );
    assert_eq!(exhausted, before);
    assert_eq!(exhausted.revision(), None);
}

#[test]
fn checked_tiled_insertion_honors_neighbor_axis_order_ratio_and_rejects_atomically() {
    let mut graph = WindowGraph::default();
    let tab = graph.create_workspace_tab("native", 1).unwrap();
    let content = graph
        .create_content(
            ContentTypeId::new("builtin.text-buffer").unwrap(),
            ContentAdapterKind::TextBuffer,
            "text".into(),
            false,
            "native",
            1,
        )
        .unwrap();
    let first_view = graph.create_view(content, "native", 1).unwrap();
    let first = graph
        .open_window(tab, first_view, WindowPlacement::Tiled, true, "native", 1)
        .unwrap();
    let second_view = graph.create_view(content, "native", 1).unwrap();
    let second = graph
        .open_window(
            tab,
            second_view,
            WindowPlacement::Floating(WindowRect {
                x: 1,
                y: 1,
                width: 10,
                height: 4,
            }),
            true,
            "native",
            1,
        )
        .unwrap();
    graph
        .tile(second, Some(first), TileAxis::Horizontal, false, 300)
        .unwrap();
    assert_eq!(
        graph.workspace_tab(tab).unwrap().tiled_root,
        Some(TiledLayoutNode::Split {
            axis: TileAxis::Horizontal,
            ratio: 300,
            first: Box::new(TiledLayoutNode::Window(second)),
            second: Box::new(TiledLayoutNode::Window(first)),
        })
    );
    let before = graph.clone();
    assert_eq!(
        graph.tile(second, Some(first), TileAxis::Vertical, true, 0),
        Err(WindowError::InvalidPlacement)
    );
    assert_eq!(graph, before);
    let foreign_tab = graph.create_workspace_tab("native", 1).unwrap();
    let foreign_view = graph.create_view(content, "native", 1).unwrap();
    let foreign = graph
        .open_window(
            foreign_tab,
            foreign_view,
            WindowPlacement::Tiled,
            true,
            "native",
            1,
        )
        .unwrap();
    let before = graph.clone();
    assert_eq!(
        graph.tile(second, Some(foreign), TileAxis::Vertical, true, 500),
        Err(WindowError::WindowNotInTiledLayout)
    );
    assert_eq!(graph, before);
}

#[test]
fn workspace_tabs_restore_remembered_focus_and_forget_ineligible_windows() {
    let mut graph = WindowGraph::default();
    let first_tab = graph.create_workspace_tab("native", 1).unwrap();
    let content = graph
        .create_content(
            ContentTypeId::new("builtin.text-buffer").unwrap(),
            ContentAdapterKind::TextBuffer,
            "text".into(),
            false,
            "native",
            1,
        )
        .unwrap();
    let first_view = graph.create_view(content, "native", 1).unwrap();
    let first = graph
        .open_window(
            first_tab,
            first_view,
            WindowPlacement::Tiled,
            true,
            "native",
            1,
        )
        .unwrap();
    let second_view = graph.create_view(content, "native", 1).unwrap();
    let second = graph
        .open_window(
            first_tab,
            second_view,
            WindowPlacement::Tiled,
            true,
            "native",
            1,
        )
        .unwrap();
    graph.focus(first).unwrap();

    let second_tab = graph.create_workspace_tab("native", 1).unwrap();
    let third_view = graph.create_view(content, "native", 1).unwrap();
    let third = graph
        .open_window(
            second_tab,
            third_view,
            WindowPlacement::Tiled,
            true,
            "native",
            1,
        )
        .unwrap();
    assert_eq!(graph.focused(), Some(third));
    graph.set_active_tab(first_tab).unwrap();
    assert_eq!(graph.focused(), Some(first));
    graph.set_active_tab(second_tab).unwrap();
    assert_eq!(graph.focused(), Some(third));

    graph.set_active_tab(first_tab).unwrap();
    graph.place(first, WindowPlacement::Hidden).unwrap();
    assert_eq!(graph.focused(), Some(second));
    graph.place(first, WindowPlacement::Tiled).unwrap();
    graph.set_active_tab(second_tab).unwrap();
    graph.set_active_tab(first_tab).unwrap();
    assert_eq!(graph.focused(), Some(second));
    graph.validate().unwrap();
}

#[test]
fn content_capabilities_reject_clone_focus_placement_and_close_before_mutation() {
    let mut graph = WindowGraph::default();
    let tab = graph
        .create_workspace_tab("native", 1)
        .expect("workspace tab");

    let terminal = graph
        .create_content(
            ContentTypeId::new("terminal.live").expect("type"),
            ContentAdapterKind::TerminalSession,
            "terminal".into(),
            false,
            "native",
            1,
        )
        .expect("terminal content");
    graph
        .create_view(terminal, "native", 1)
        .expect("authoritative terminal view");
    assert_eq!(
        graph
            .content(terminal)
            .expect("terminal descriptor")
            .lifecycle,
        ContentLifecycle::Starting
    );
    let before_lifecycle = graph.revision();
    graph
        .update_content_lifecycle(
            terminal,
            "terminal".into(),
            false,
            ContentLifecycle::Running,
        )
        .expect("terminal lifecycle update");
    assert_ne!(graph.revision(), before_lifecycle);
    let running_revision = graph.revision();
    graph
        .update_content_lifecycle(
            terminal,
            "terminal".into(),
            false,
            ContentLifecycle::Running,
        )
        .expect("unchanged terminal lifecycle");
    assert_eq!(graph.revision(), running_revision);
    assert!(
        graph
            .content(terminal)
            .expect("terminal descriptor")
            .capabilities
            .stop_resource
    );
    graph
        .require_stop_resource(terminal)
        .expect("terminal declares stop-resource");
    graph
        .require_selection(terminal)
        .expect("terminal selection is supported");
    assert_eq!(
        graph.require_editing(terminal),
        Err(WindowError::Unsupported("editing"))
    );
    assert_eq!(
        graph.require_persist_descriptor(terminal),
        Err(WindowError::Unsupported("persist-descriptor"))
    );
    assert_eq!(
        graph.create_view(terminal, "native", 1),
        Err(WindowError::Unsupported("clone-view"))
    );

    let external_tui = graph
        .create_content(
            ContentTypeId::new("terminal.external-tui").expect("type"),
            ContentAdapterKind::ExternalTui,
            "external tui".into(),
            false,
            "native",
            1,
        )
        .expect("external tui content");
    graph
        .create_view(external_tui, "native", 1)
        .expect("authoritative external tui view");
    let before_external_clone = graph.clone();
    assert_eq!(
        graph.require_clone_view(external_tui),
        Err(WindowError::Unsupported("clone-view"))
    );
    assert_eq!(
        graph.create_view(external_tui, "native", 1),
        Err(WindowError::Unsupported("clone-view"))
    );
    assert_eq!(graph, before_external_clone);

    let text = graph
        .create_content(
            ContentTypeId::new("editor.text").expect("type"),
            ContentAdapterKind::TextBuffer,
            "text".into(),
            false,
            "native",
            1,
        )
        .expect("text content");
    graph
        .create_view(text, "native", 1)
        .expect("first text view");
    graph
        .create_view(text, "native", 1)
        .expect("text declares clone-view");
    graph.require_selection(text).expect("text selection");
    graph.require_editing(text).expect("text editing");
    graph
        .require_persist_descriptor(text)
        .expect("text descriptor persistence");

    let plugin = graph
        .create_content(
            ContentTypeId::new("plugin.panel").expect("type"),
            ContentAdapterKind::DeclarativeUi,
            "plugin".into(),
            false,
            "plugin",
            1,
        )
        .expect("plugin content");
    let plugin_view = graph
        .create_view(plugin, "plugin", 1)
        .expect("first plugin view");
    graph.require_selection(plugin).expect("plugin selection");
    assert_eq!(
        graph.require_editing(plugin),
        Err(WindowError::Unsupported("editing"))
    );
    assert_eq!(
        graph.require_persist_descriptor(plugin),
        Err(WindowError::Unsupported("persist-descriptor"))
    );
    assert_eq!(
        graph.require_stop_resource(plugin),
        Err(WindowError::Unsupported("stop-resource"))
    );
    let before = graph.clone();
    assert_eq!(
        graph.create_view(plugin, "plugin", 1),
        Err(WindowError::Unsupported("clone-view"))
    );
    assert_eq!(graph, before);

    let mut restricted_capabilities =
        ContentCapabilities::for_adapter(ContentAdapterKind::DeclarativeUi);
    restricted_capabilities.focusable = false;
    restricted_capabilities.float = false;
    restricted_capabilities.close_view = false;
    let restricted = graph
        .create_content_with_contract(
            ContentTypeId::new("native.restricted").expect("type"),
            ContentContract::declared(ContentAdapterKind::DeclarativeUi, restricted_capabilities),
            "restricted".into(),
            false,
            "native",
            1,
        )
        .expect("restricted content");
    assert_eq!(
        graph
            .content_snapshots()
            .into_iter()
            .find(|snapshot| snapshot.id == restricted)
            .expect("restricted snapshot")
            .capabilities,
        restricted_capabilities
    );
    let restricted_view = graph
        .create_view(restricted, "native", 1)
        .expect("restricted view");
    let before = graph.clone();
    assert_eq!(
        graph.open_window(
            tab,
            restricted_view,
            WindowPlacement::Hidden,
            true,
            "native",
            1,
        ),
        Err(WindowError::Unsupported("focus"))
    );
    assert_eq!(graph, before);
    let restricted_window = graph
        .open_window(
            tab,
            restricted_view,
            WindowPlacement::Hidden,
            false,
            "native",
            1,
        )
        .expect("non-focusable hidden window");
    let before = graph.clone();
    assert_eq!(
        graph.place(
            restricted_window,
            WindowPlacement::Floating(WindowRect {
                x: 1,
                y: 1,
                width: 10,
                height: 4,
            }),
        ),
        Err(WindowError::Unsupported("placement"))
    );
    assert_eq!(graph, before);
    assert_eq!(
        graph.close_window(restricted_window),
        Err(WindowError::Unsupported("close-view"))
    );
    assert_eq!(graph, before);

    // A refused clone must leave the earlier plugin view usable.
    let plugin_window = graph
        .open_window(
            tab,
            plugin_view,
            WindowPlacement::Floating(WindowRect {
                x: 20,
                y: 2,
                width: 12,
                height: 6,
            }),
            true,
            "plugin",
            1,
        )
        .expect("plugin original view");
    let before = graph.clone();
    assert_eq!(
        graph.swap_non_tiled_placements(restricted_window, plugin_window),
        Err(WindowError::Unsupported("placement"))
    );
    assert_eq!(graph, before);
}

#[test]
fn removing_an_owner_instance_preserves_other_instances_and_owners() {
    let mut graph = WindowGraph::default();
    let old = graph
        .create_content(
            ContentTypeId::new("plugin.old").expect("type"),
            ContentAdapterKind::DeclarativeUi,
            "old".into(),
            false,
            "plugin",
            1,
        )
        .expect("old");
    let new = graph
        .create_content(
            ContentTypeId::new("plugin.new").expect("type"),
            ContentAdapterKind::DeclarativeUi,
            "new".into(),
            false,
            "plugin",
            2,
        )
        .expect("new");
    let other = graph
        .create_content(
            ContentTypeId::new("other.panel").expect("type"),
            ContentAdapterKind::DeclarativeUi,
            "other".into(),
            false,
            "other",
            1,
        )
        .expect("other owner content");
    let mut surviving_resources = Vec::new();
    for (content, owner, instance_id) in
        [(old, "plugin", 1), (new, "plugin", 2), (other, "other", 1)]
    {
        let view = graph
            .create_view(content, owner, instance_id)
            .expect("view");
        let tab = graph.create_workspace_tab(owner, instance_id).expect("tab");
        let window = graph
            .open_window(
                tab,
                view,
                WindowPlacement::Fullscreen,
                true,
                owner,
                instance_id,
            )
            .expect("window");
        if content != old {
            surviving_resources.push((content, view, tab, window));
        }
    }
    graph
        .remove_owner_instance("plugin", 1)
        .expect("remove owner instance");
    assert!(graph.content(old).is_none());
    for (content, view, tab, window) in surviving_resources {
        assert!(graph.content(content).is_some());
        assert!(graph.view(view).is_some());
        assert!(graph.workspace_tab(tab).is_some());
        assert!(graph.window(window).is_some());
    }
    assert_eq!(graph.counts(), (2, 2, 2, 2));
    graph.validate().expect("valid graph");
}

#[test]
fn ids_placements_focus_and_removal_order_are_validated() {
    assert!(ContentTypeId::new("Plugin.Bad").is_err());
    let (mut graph, content, view, tab) = graph_with_content();
    assert_eq!(
        graph.open_window(
            tab,
            view,
            WindowPlacement::Docked {
                edge: DockEdge::Left,
                size: 0,
            },
            true,
            "builtin",
            1,
        ),
        Err(WindowError::InvalidPlacement)
    );
    assert_eq!(
        graph.remove_content(content),
        Err(WindowError::ContentStillViewed)
    );
}

#[test]
fn fullscreen_moves_focus_and_rejects_suppressed_windows() {
    let (mut graph, content, view, tab) = graph_with_content();
    let a = graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    let bview = graph.create_view(content, "builtin", 1).unwrap();
    let b = graph
        .open_window(tab, bview, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    graph.focus(a).unwrap();
    graph.place(b, WindowPlacement::Fullscreen).unwrap();
    assert_eq!(graph.focused(), Some(b));
    assert!(graph.focus(a).is_err());
}

#[test]
fn non_focusable_content_can_move_to_fullscreen() {
    let (mut graph, text_content, view, tab) = graph_with_content();
    let tile = graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    let mut capabilities = ContentCapabilities::for_adapter(ContentAdapterKind::DeclarativeUi);
    capabilities.focusable = false;
    let content = graph
        .create_content_with_contract(
            ContentTypeId::new("plugin.status").unwrap(),
            ContentContract::declared(ContentAdapterKind::DeclarativeUi, capabilities),
            "status".into(),
            false,
            "plugin",
            1,
        )
        .unwrap();
    let view = graph.create_view(content, "plugin", 1).unwrap();
    let window = graph
        .open_window(
            tab,
            view,
            WindowPlacement::Docked {
                edge: DockEdge::Bottom,
                size: 5,
            },
            false,
            "plugin",
            1,
        )
        .unwrap();
    graph.focus(tile).unwrap();
    graph.place(window, WindowPlacement::Fullscreen).unwrap();
    assert_eq!(graph.focused(), None);
    assert!(!graph.can_focus(tile));
    graph.validate().unwrap();

    // Returning to this tab must not select its suppressed tile as a fallback.
    let other_tab = graph.create_workspace_tab("builtin", 1).unwrap();
    let other_view = graph.create_view(text_content, "builtin", 1).unwrap();
    graph
        .open_window(
            other_tab,
            other_view,
            WindowPlacement::Tiled,
            true,
            "builtin",
            1,
        )
        .unwrap();
    graph.set_active_tab(other_tab).unwrap();
    graph.set_active_tab(tab).unwrap();
    assert_eq!(graph.active_tab(), Some(tab));
    assert_eq!(graph.focused(), None);
    graph.validate().unwrap();
    graph.place(window, WindowPlacement::Hidden).unwrap();
    graph.focus(tile).unwrap();
}

#[test]
fn swapping_fullscreen_repairs_suppressed_focus() {
    for focusable in [true, false] {
        let (mut graph, content, view, tab) = graph_with_content();
        let dock = WindowPlacement::Docked {
            edge: DockEdge::Bottom,
            size: 5,
        };
        let a = graph
            .open_window(tab, view, WindowPlacement::Fullscreen, true, "builtin", 1)
            .unwrap();
        let view = graph.create_view(content, "builtin", 1).unwrap();
        let b = graph
            .open_window(tab, view, dock, focusable, "builtin", 1)
            .unwrap();
        graph.swap_non_tiled_placements(a, b).unwrap();
        assert_eq!(graph.focused(), focusable.then_some(b));
        assert_eq!(graph.window(a).unwrap().placement, dock);
        assert_eq!(
            graph.window(b).unwrap().placement,
            WindowPlacement::Fullscreen
        );
        graph.validate().unwrap();
    }
}

#[test]
fn overlays_above_fullscreen_can_receive_and_remember_focus() {
    let (mut graph, content, view, tab) = graph_with_content();
    let fullscreen = graph
        .open_window(tab, view, WindowPlacement::Fullscreen, true, "builtin", 1)
        .unwrap();
    let view = graph.create_view(content, "builtin", 1).unwrap();
    let overlay = graph
        .open_window(
            tab,
            view,
            WindowPlacement::Overlay(WindowRect {
                x: 1,
                y: 1,
                width: 20,
                height: 10,
            }),
            true,
            "builtin",
            1,
        )
        .unwrap();
    graph.focus(fullscreen).unwrap();
    graph.focus(overlay).unwrap();
    let other_tab = graph.create_workspace_tab("builtin", 1).unwrap();
    let view = graph.create_view(content, "builtin", 1).unwrap();
    graph
        .open_window(other_tab, view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    graph.set_active_tab(tab).unwrap();
    assert_eq!(graph.focused(), Some(overlay));
    graph.validate().unwrap();
}

#[test]
fn hiding_the_last_window_keeps_the_active_tab() {
    let (mut graph, content, view, tab) = graph_with_content();
    let a = graph
        .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    let other = graph.create_workspace_tab("builtin", 1).unwrap();
    let bview = graph.create_view(content, "builtin", 1).unwrap();
    graph
        .open_window(other, bview, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    graph.focus(a).unwrap();
    graph.place(a, WindowPlacement::Hidden).unwrap();
    assert_eq!(graph.focused(), None);
    assert_eq!(graph.active_tab(), Some(tab));
}

#[test]
fn panes_below_a_wide_pane_are_not_its_left_neighbor() {
    let top = WindowRect {
        x: 0,
        y: 0,
        width: 80,
        height: 10,
    };
    let bottom = WindowRect {
        x: 0,
        y: 10,
        width: 40,
        height: 10,
    };
    assert_eq!(
        crate::windows::projection::spatial_score(top, bottom, SpatialDirection::Left),
        None
    );
    assert!(
        crate::windows::projection::spatial_score(top, bottom, SpatialDirection::Down).is_some()
    );
}

#[test]
fn sixty_five_windows_can_be_installed_before_a_balanced_root() {
    let (mut graph, content, first, tab) = graph_with_content();
    let mut windows = Vec::new();
    for index in 0..65 {
        let view = if index == 0 {
            first
        } else {
            graph.create_view(content, "builtin", 1).unwrap()
        };
        windows.push(
            graph
                .open_window(tab, view, WindowPlacement::Tiled, true, "builtin", 1)
                .unwrap(),
        );
    }
    graph
        .set_tiled_layout(tab, Some(balanced_layout(&windows)))
        .unwrap();
    graph.validate().unwrap();
}

#[test]
fn tiled_resize_accepts_minimum_delta_for_either_child() {
    let (mut graph, content, first_view, tab) = graph_with_content();
    let second_view = graph.create_view(content, "builtin", 1).unwrap();
    let first = graph
        .open_window(tab, first_view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    let second = graph
        .open_window(tab, second_view, WindowPlacement::Tiled, true, "builtin", 1)
        .unwrap();
    for (window, expected) in [(second, 999), (first, 1)] {
        graph.resize_tiled_split(tab, window, i16::MIN).unwrap();
        let snapshot = graph.tiled_layout_snapshot(tab).unwrap();
        assert!(matches!(snapshot.root, TiledLayoutNode::Split { ratio, .. } if ratio == expected));
        graph.validate().unwrap();
    }
}
