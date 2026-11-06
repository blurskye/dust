//! Window workflows tests for cross-feature application workflows.

use crate::app::tests::driver::TestSession;
use crate::ui::Style;

#[test]
fn universal_window_switcher_reports_type_lifecycle_location_and_owner() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.reconcile_windows();
    session.window_list();
    let picker = session.projection().picker.as_ref().expect("window picker");
    assert_eq!(picker.title, "Windows");
    let editor = picker
        .rows
        .iter()
        .find(|row| row.source == "builtin.editor")
        .expect("native editor window");
    assert!(editor.detail.contains("builtin.text"), "{}", editor.detail);
    assert!(editor.detail.contains("Ready"), "{}", editor.detail);
    assert!(editor.detail.contains("Tiled"), "{}", editor.detail);
    assert!(editor.detail.contains("tab "), "{}", editor.detail);
}

#[test]
fn universal_window_switcher_focuses_an_exact_window_across_editor_tabs() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.reconcile_windows();
    let original = session.focused_window().expect("first editor window");
    let original_identity = session.window_identity(original);
    let original_tab = session.active_tab().expect("first editor tab");
    session.new_tab();
    assert_ne!(session.active_tab(), Some(original_tab));
    session.window_list();
    let target = format!("window:{}", original.0);
    let index = session
        .projection()
        .picker
        .as_ref()
        .expect("window picker")
        .rows
        .iter()
        .position(|row| row.id == target)
        .expect("original window row");
    for _ in 0..index {
        session.key("down");
    }
    session.key("enter");
    assert_eq!(session.active_tab(), Some(original_tab));
    assert_eq!(session.focused_window(), Some(original));
    assert_eq!(session.window_identity(original), original_identity);
}

#[test]
fn tiny_terminal_projection_clips_safely_and_restores_the_same_sessions() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.terminal_fixture();
    let sessions = session.terminal_snapshots();
    assert_eq!(sessions.len(), 2);
    assert_ne!(sessions[0].0, sessions[1].0);
    for (id, lines) in &sessions {
        assert!(
            lines
                .iter()
                .any(|line| line.contains(&format!("terminal {id} contents")))
        );
    }
    session.resize(0, 0);
    let zero = session.frame();
    assert_eq!((zero.grid.width(), zero.grid.height()), (0, 0));
    assert_eq!(zero.cursor, None);
    assert_eq!(session.terminal_snapshots(), sessions);
    session.resize(3, 3);
    let tiny = session.frame();
    assert_eq!((tiny.grid.width(), tiny.grid.height()), (3, 3));
    if let Some((x, y, _)) = tiny.cursor {
        assert!(x < 3 && y < 3);
    }
    assert_eq!(session.terminal_snapshots(), sessions);
    session.resize(80, 24);
    let restored = session.frame();
    assert_eq!(session.terminal_snapshots(), sessions);
    assert_eq!(
        session
            .projection()
            .terminal_panel
            .as_ref()
            .expect("terminal projection restored")
            .tabs
            .len(),
        2
    );
    if let Some((x, y, _)) = restored.cursor {
        assert!(x < 80 && y < 24);
    }
}

#[test]
fn universal_window_clone_publishes_independent_explorer_views_over_shared_tree_data() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.explorer_fixture();
    session.float_explorer();
    let primary_selection = session
        .projection()
        .explorer
        .as_ref()
        .and_then(|explorer| explorer.rows.iter().find(|row| row.selected))
        .map(|row| row.node_id)
        .unwrap_or_else(|| {
            panic!(
                "primary explorer selection: {:?}",
                session.projection().explorer
            )
        });

    session.clone_explorer();
    let ids = session.explorer_content_ids();
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0].1, ids[1].1, "tree data has one content identity");
    assert_ne!(ids[0].2, ids[1].2, "each tree has its own graph view");
    let projection = session.projection();
    assert_eq!(projection.explorer_views.len(), 1);
    assert!(projection.explorer_views[0].focused);
    assert_eq!(
        projection.explorer_views[0]
            .content
            .rows
            .iter()
            .find(|row| row.selected)
            .map(|row| row.node_id),
        Some(primary_selection)
    );

    session.page_explorer(1);
    let projection = session.projection();
    let primary_after = projection
        .explorer
        .as_ref()
        .and_then(|explorer| explorer.rows.iter().find(|row| row.selected))
        .map(|row| row.node_id);
    let clone_after = projection.explorer_views[0]
        .content
        .rows
        .iter()
        .find(|row| row.selected)
        .map(|row| row.node_id);
    assert_eq!(primary_after, Some(primary_selection));
    assert_ne!(clone_after, primary_after);
    let _frame = session.frame();
}

#[test]
fn cloned_explorer_rehosts_and_restores_without_replacing_shared_or_view_identity() {
    use crate::windows::DockEdge;
    use crate::windows::WindowPlacement;
    use crate::windows::WindowRect;

    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.explorer_fixture();
    session.float_explorer();
    session.clone_explorer();
    let before = session.explorer_content_ids();
    let primary_window = session.explorer_window(1);
    let clone_window = session.explorer_window(2);
    let first_tab = session.active_window_tab();
    session.new_tab();
    let second_tab = session.active_window_tab();
    assert_ne!(second_tab, first_tab);
    assert_eq!(session.explorer_content_ids(), before);
    assert_eq!(session.window_tab(primary_window), second_tab);
    assert_eq!(session.window_tab(clone_window), second_tab);

    for placement in [
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 17,
        },
        WindowPlacement::Floating(WindowRect {
            x: 9,
            y: 4,
            width: 33,
            height: 9,
        }),
        WindowPlacement::Fullscreen,
        WindowPlacement::Hidden,
    ] {
        session
            .place_window(clone_window, placement)
            .expect("source-owned Explorer placement");
        assert_eq!(session.window_placement(clone_window), placement);
        assert_eq!(session.explorer_content_ids(), before);
    }
    session
        .show_window(clone_window)
        .expect("show exact cloned Explorer view");
    assert_eq!(
        session.window_placement(clone_window),
        WindowPlacement::Fullscreen
    );
    assert_eq!(session.explorer_content_ids(), before);
    session
        .place_window(clone_window, WindowPlacement::Tiled)
        .expect("tile exact cloned Explorer view");
    assert_eq!(
        session.window_placement(clone_window),
        WindowPlacement::Tiled
    );
    session.reconcile_windows();
    assert_eq!(
        session.window_placement(clone_window),
        WindowPlacement::Tiled
    );
    assert_eq!(session.explorer_content_ids(), before);
    session.close_explorer_view(2).expect("close cloned view");
    assert_eq!(session.explorer_content_ids().len(), 1);
    assert_eq!(session.explorer_content_ids()[0].1, before[0].1);
}

#[test]
fn explorer_clone_paging_uses_its_own_height() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.explorer_fixture();
    let primary = session.explorer_selections()[&1];
    session.clone_explorer();
    let clone = session.explorer_window(2);
    session
        .place_window(
            clone,
            crate::windows::WindowPlacement::Floating(crate::windows::WindowRect {
                x: 25,
                y: 3,
                width: 40,
                height: 8,
            }),
        )
        .unwrap();
    session.reconcile_windows();
    let rows = usize::from(session.explorer_body(2).height);
    assert!(rows < usize::from(session.explorer_body(1).height));
    assert_eq!(session.explorer_focus(), Some(2));
    session.page_explorer(1);
    let paged = session.explorer_selections()[&2];
    assert_eq!(session.explorer_selections()[&1], primary);
    session.page_explorer(-1);
    assert_eq!(session.explorer_selections()[&2], primary);
    for _ in 0..rows {
        session.key("j");
    }
    assert_eq!(session.explorer_selections()[&2], paged);
}

#[test]
fn explorer_clone_uses_the_exact_nonfocused_semantic_source() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.explorer_fixture();
    session.float_explorer();
    let primary = session.explorer_selections()[&1];

    session.clone_explorer_view(1).unwrap();
    session.page_explorer(1);
    let clone = session.explorer_selections()[&2];
    assert_ne!(clone, primary);

    session.clone_explorer_view(1).unwrap();
    assert_eq!(session.explorer_focus(), Some(3));
    assert_eq!(session.explorer_selections()[&3], primary);

    session.clone_explorer_view(2).unwrap();
    assert_eq!(session.explorer_focus(), Some(4));
    assert_eq!(session.explorer_selections()[&4], clone);
}

#[test]
fn explorer_clone_runtime_reset_preserves_semantic_views_state_focus_and_shared_content() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.explorer_fixture();
    session.float_explorer();
    session.clone_explorer();
    session.page_explorer(1);
    let before_ids = session.explorer_content_ids();
    let before_state = session.explorer_selections();
    assert_eq!(session.explorer_focus(), Some(2));

    session.reset_window_runtime().unwrap();

    let after_ids = session.explorer_content_ids();
    assert_eq!(
        after_ids.iter().map(|entry| entry.0).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(after_ids[0].1, after_ids[1].1);
    assert_eq!(session.explorer_graph_shape(), (1, 2, 2));
    assert!(after_ids.iter().all(|after| {
        before_ids
            .iter()
            .find(|before| before.0 == after.0)
            .is_some_and(|before| before.1 != after.1 && before.2 != after.2)
    }));
    assert_eq!(session.explorer_selections(), before_state);
    assert_eq!(session.explorer_focus(), Some(2));
}

#[test]
fn configuration_rebuild_keeps_cloned_explorer_identity_state_and_focus() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.explorer_fixture();
    session.float_explorer();
    session.clone_explorer();
    session.page_explorer(1);
    let ids = session.explorer_content_ids();
    let state = session.explorer_selections();

    let mut candidate = crate::config::load_bundled_config();
    candidate.config.windows.rules = vec![crate::config::WindowRuleConfig {
        content_type: "builtin.explorer".into(),
        placement: crate::config::WindowPlacementConfig::Docked {
            edge: "right".into(),
            size: 31,
        },
    }];
    session.apply_candidate(candidate).unwrap();

    assert_eq!(session.explorer_content_ids(), ids);
    assert_eq!(session.explorer_selections(), state);
    assert_eq!(session.explorer_focus(), Some(2));
}

#[test]
fn closing_and_recloning_an_explorer_never_reuses_the_semantic_view_identity() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.explorer_fixture();
    session.float_explorer();
    session.clone_explorer();
    session.close_explorer_view(2).unwrap();
    assert_eq!(
        session
            .explorer_content_ids()
            .iter()
            .map(|entry| entry.0)
            .collect::<Vec<_>>(),
        vec![1]
    );
    session.clone_explorer_view(1).unwrap();
    assert_eq!(
        session
            .explorer_content_ids()
            .iter()
            .map(|entry| entry.0)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
}

#[test]
fn explorer_mouse_and_keyboard_input_target_the_exact_cloned_or_primary_view() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.explorer_fixture();
    session.float_explorer();
    session.clone_explorer();
    let initial = session.explorer_selections();

    session.key("j");
    let keyboard = session.explorer_selections();
    assert_eq!(keyboard[&1], initial[&1]);
    assert_ne!(keyboard[&2], initial[&2]);

    let primary = session.explorer_body(1);
    session.click(primary.x, primary.y.saturating_add(2));
    let mouse = session.explorer_selections();
    assert_ne!(mouse[&1], keyboard[&1]);
    assert_eq!(mouse[&2], keyboard[&2]);

    // The click opened the file, which took focus; the primary view can be focused again.
    session.wait_for_opened_file().expect("opened file");
    assert_eq!(session.explorer_focus(), None);
    session.focus_explorer(true);
    assert_eq!(session.explorer_focus(), Some(1));
    session.key("j");
    let primary_keyboard = session.explorer_selections();
    assert_ne!(primary_keyboard[&1], mouse[&1]);
    assert_eq!(primary_keyboard[&2], mouse[&2]);
}

#[test]
fn clicking_a_file_in_the_explorer_opens_it() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.explorer_fixture();
    let body = session.explorer_body(1);
    session.click(body.x, body.y.saturating_add(2));
    let opened = session.wait_for_opened_file().expect("opened file");
    assert!(opened.starts_with("file"), "{opened}");
}

#[test]
fn a_full_click_shows_the_file_in_place_of_the_open_one_and_leaves_focus_on_it() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.explorer_fixture();
    let body = session.explorer_body(1);
    session.click(body.x, body.y.saturating_add(2));
    let first = session.wait_for_opened_file().expect("first file");
    let body = session.explorer_body(1);
    session.click(body.x, body.y.saturating_add(3));
    session.mouse(
        crate::editor::input::KeyCode::MouseLeftUp,
        body.x,
        body.y.saturating_add(3),
    );
    let second = session.wait_for_opened_file().expect("second file");
    assert_ne!(first, second, "the clicked file replaces the one on screen");
    assert!(
        !session.explorer_focused(),
        "the release leaves focus on the file"
    );
}

#[test]
fn moving_the_pointer_does_not_move_focus() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.explorer_fixture();
    assert!(
        session.explorer_focused(),
        "the fixture starts in the explorer"
    );
    session.mouse(crate::editor::input::KeyCode::MouseMove, 60, 5);
    assert!(
        session.explorer_focused(),
        "hovering the editor keeps the explorer"
    );
    session.click(60, 5);
    assert!(!session.explorer_focused(), "a click moves focus");
    let body = session.explorer_body(1);
    session.mouse(crate::editor::input::KeyCode::MouseMove, body.x, body.y);
    assert!(
        !session.explorer_focused(),
        "hovering the tree keeps the editor"
    );
}

#[test]
fn stale_contribution_policy_is_rejected_before_window_graph_publication() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.frame();
    let before = session.window_policy_snapshot();
    assert!(session.reject_stale_contribution().is_err());
    assert_eq!(session.window_policy_snapshot(), before);
}

#[test]
fn floating_explorer_pages_cursor_and_hit_test_use_its_body_not_the_workspace_height() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.resize(100, 30);
    session.explorer_fixture();
    session.float_explorer();
    let body = session.projection().surface_content.explorer.unwrap();
    assert_eq!((body.x, body.y, body.height), (5, 8, 5));
    assert_eq!(
        session.projection().explorer.as_ref().unwrap().rows.len(),
        5
    );
    session.page_explorer(1);
    let explorer = session.projection().explorer.as_ref().unwrap();
    assert_eq!(explorer.scroll, 2);
    assert!(explorer.rows.last().unwrap().selected);
    let frame = session.frame();
    assert_eq!(frame.cursor.unwrap().1, body.y + body.height - 1);
    session.click(body.x + 1, body.y + 1);
    assert!(session.projection().explorer.as_ref().unwrap().rows[1].selected);
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.ui.status_line.enabled = false;
    session.apply_candidate(loaded).unwrap();
    let body = session.projection().surface_content.explorer.unwrap();
    assert_eq!(body.height, 5);
    assert_eq!(
        session.projection().explorer.as_ref().unwrap().rows.len(),
        5
    );
}

#[test]
fn documentation_scroll_reaches_the_last_line_in_a_short_actual_workspace() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.resize(80, 8);
    session.documentation(
        (0..100)
            .map(|line| format!("doc-line-{line:03}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    for _ in 0..100 {
        session.key("down");
    }
    let frame = session.frame();
    let projection = session.projection();
    let popup = projection.documentation_popup.as_ref().unwrap();
    let rows = projection.surface_content.documentation_rows;
    assert!(rows < 8);
    assert_eq!(popup.scroll, popup.line_count() - rows);
    let painted = (0..8)
        .map(|y| {
            (0..80)
                .filter_map(|x| frame.grid.get(x, y))
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(painted.contains("doc-line-099"), "{painted}");
}

#[test]
fn a_key_press_dismisses_information_but_not_errors() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.notice("read once");
    assert!(session.projection().notification.is_some());
    session.key("j");
    assert!(
        session.projection().notification.is_none(),
        "information goes"
    );
    session.error("broken");
    session.key("j");
    assert_eq!(session.projection().notification.as_deref(), Some("broken"));
}

#[test]
fn wide_notice_text_stays_inside_its_box() {
    let loaded = crate::config::load_bundled_config();
    let mut session = TestSession::new(loaded);
    session.resize(40, 12);
    let before = session.frame();
    session.notice(&"界".repeat(30));
    let after = session.frame();
    let symbol = |x: u16, y: u16| after.grid.get(x, y).unwrap().symbol.clone();
    let left = (0..40)
        .find(|x| symbol(*x, 1) == "╭")
        .expect("top left corner");
    let right = (left..40)
        .find(|x| symbol(*x, 1) == "╮")
        .expect("top right corner");
    let bottom = (2..12)
        .find(|y| symbol(left, *y) == "╰")
        .expect("bottom corner");
    assert!(
        right < 40 && bottom >= 4,
        "the text wraps inside the screen"
    );
    for y in 0..12 {
        for x in 0..40 {
            if !((left..=right).contains(&x) && (1..=bottom).contains(&y)) {
                assert_eq!(
                    before.grid.get(x, y),
                    after.grid.get(x, y),
                    "outside notice at {x},{y}"
                );
            }
        }
    }
    let wide = (2..bottom)
        .flat_map(|y| (left..right).map(move |x| (x, y)))
        .filter(|(x, y)| symbol(*x, *y) == "界")
        .count();
    assert_eq!(wide, 30);
    assert_eq!(after.grid.get(left, 1).unwrap().style, Style::Accent);
}

#[test]
fn errors_render_as_top_right_bordered_notifications() {
    let loaded = crate::config::load_bundled_config();
    let mut session = TestSession::new(loaded);
    session.resize(40, 12);
    session.error("bad");
    let frame = session.frame();
    assert_eq!(frame.grid.get(32, 1).unwrap().symbol, "╭");
    assert_eq!(frame.grid.get(34, 2).unwrap().symbol, "b");
    assert_eq!(frame.grid.get(38, 3).unwrap().symbol, "╯");
    assert_eq!(frame.grid.get(32, 1).unwrap().style, Style::Error);
}

#[test]
fn stacked_picker_selected_row_and_late_preview_share_the_painted_capacity_after_resize() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.resize(100, 30);
    session.grep_snapshot_fixture();
    for _ in 0..17 {
        session.key("down");
    }
    let eighteenth = session.frame();
    let picker = session.projection().picker.as_ref().unwrap();
    assert_eq!(picker.selected, Some(17)); // User-visible count 18/45.
    let body = session.projection().surface_content.picker.unwrap().list;
    let offset = 17 - picker.scroll;
    assert!(offset < usize::from(body.height));
    assert_eq!(
        eighteenth
            .grid
            .get(body.x, body.y + u16::try_from(offset).unwrap())
            .unwrap()
            .style,
        Style::Selection
    );
    session.key("down");
    for (width, height, stacked) in [
        (100, 30, true),
        (220, 45, false),
        (100, 30, true),
        (60, 18, true),
    ] {
        session.resize(width, height);
        let frame = session.frame();
        let projection = session.projection();
        let picker = projection.picker.as_ref().unwrap();
        assert_eq!(picker.selected, Some(18));
        let geometry = projection.surface_content.picker.unwrap();
        assert_eq!(geometry.stacked_divider.is_some(), stacked);
        let visible = 18 - picker.scroll;
        assert!(visible < usize::from(geometry.list.height));
        let y = geometry.list.y + u16::try_from(visible).unwrap();
        assert_eq!(
            frame.grid.get(geometry.list.x, y).unwrap().style,
            Style::Selection
        );
        let row = (geometry.list.x..geometry.list.x + geometry.list.width)
            .filter_map(|x| frame.grid.get(x, y))
            .map(|cell| cell.symbol.as_str())
            .collect::<String>();
        assert!(row.contains("row-18"), "{row}");
        let painted = (0..height)
            .map(|y| {
                (0..width)
                    .filter_map(|x| frame.grid.get(x, y))
                    .map(|cell| cell.symbol.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            painted.contains("line1087"),
            "late preview was clipped: {painted}"
        );
    }
    session.resize(2, 2);
    assert!(session.projection().surface_content.picker.is_none());
    session.frame();
    session.resize(100, 30);
    assert_eq!(
        session.projection().picker.as_ref().unwrap().selected,
        Some(18)
    );
    session.key("escape");
    assert!(
        session
            .projection()
            .picker
            .as_ref()
            .unwrap()
            .input_hint
            .starts_with("Normal")
    );
    assert_eq!(
        session.projection().picker.as_ref().unwrap().selected,
        Some(18)
    );
    session.key("escape");
    assert!(session.projection().picker.is_none());
    assert!(session.projection().surface_content.picker.is_none());
    session.grep_snapshot_fixture();
    assert!(session.projection().picker.is_some());
    let candidate = crate::config::load_bundled_config();
    session.apply_candidate(candidate).unwrap();
    session.frame();
    assert!(
        session.projection().picker.is_none(),
        "stale rows survived accepted configuration"
    );
    // Enter now reaches the dashboard, which runs Find File; no stale row is opened.
    session.key("enter");
    assert!(session.application_mut().state.active_view().is_none());
}

#[test]
fn prompt_and_documentation_capture_preserve_explicit_focus_changes_and_closed_targets() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.resize(100, 30);
    session.explorer_fixture();
    session.focus_explorer(true);
    assert_eq!(session.context(), "explorer");
    session.documentation("documentation".into());
    session.open_command_prompt();
    assert_eq!(session.context(), "command_prompt");
    session.close_prompt();
    assert_eq!(session.context(), "documentation");
    session.focus_explorer(false);
    session.close_documentation();
    assert_eq!(session.context(), "editor.normal");
    session.focus_explorer(true);
    session.open_command_prompt();
    session.close_explorer();
    session.close_prompt();
    assert!(session.projection().explorer.is_none());
    assert_eq!(session.context(), "editor.normal");
}

#[test]
fn plugin_requests_count_all_graph_and_host_copies_in_invalid_unchanged_valid_and_idle_cases() {
    use crate::plugins::PluginRequestedPlacement;
    use crate::plugins::PluginUiErrorCode;
    use crate::plugins::PluginUiNode;
    use crate::plugins::PluginUiNodeKind;
    use crate::plugins::PluginUiRequest;
    use crate::plugins::PluginUiRequestKind;
    use crate::plugins::PluginUiStyle;
    use crate::plugins::PluginUiTree;
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.resize(100, 30);
    let manifest =
        crate::app::tests::plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 77, "request fixture".into())
            .unwrap();
    session.install_plugin_fixture(index.clone());
    let tree = |revision| PluginUiTree {
        revision,
        root: 1,
        nodes: vec![PluginUiNode {
            id: 1,
            kind: PluginUiNodeKind::Text,
            text: "retained payload".into(),
            style: PluginUiStyle::Accent,
            action: None,
            children: Vec::new(),
        }],
    };
    let placement = PluginRequestedPlacement::Floating {
        x: 5,
        y: 3,
        width: 24,
        height: 7,
    };
    let open = |revision| PluginUiRequest {
        token: 1,
        kind: PluginUiRequestKind::OpenWindow {
            expected_graph_revision: revision,
            content_type: "plugin.demo.panel".into(),
            title: "Demo".into(),
            placement,
            tree: Some(tree(1)),
        },
    };
    let before = session.copy_counts();
    let rejected = session.plugin_request(&index, open(session.graph_revision() + 1));
    assert_eq!(rejected.error.unwrap().code, PluginUiErrorCode::StaleGraph);
    assert_eq!(session.copy_counts(), before);
    let mut malformed = open(session.graph_revision());
    if let PluginUiRequestKind::OpenWindow { title, .. } = &mut malformed.kind {
        *title = "bad\nwindow".into();
    }
    assert!(session.plugin_request(&index, malformed).error.is_some());
    assert_eq!(session.copy_counts(), before);
    let opened = session.plugin_request(&index, open(session.graph_revision()));
    assert!(opened.error.is_none(), "{:?}", opened.error);
    let handle = opened.handle.unwrap();
    let after = session.copy_counts();
    let valid_copies = (after.0 - before.0, after.1 - before.1);
    assert!(
        (1..=5).contains(&valid_copies.0),
        "graph mutation copy budget: {valid_copies:?}"
    );
    assert_eq!(valid_copies.1, 1);
    let unchanged = session.plugin_request(
        &index,
        PluginUiRequest {
            token: 2,
            kind: PluginUiRequestKind::PlaceWindow {
                handle,
                expected_graph_revision: session.graph_revision(),
                placement,
            },
        },
    );
    assert!(unchanged.error.is_none());
    assert_eq!(session.copy_counts(), after);
    let invalid = session.plugin_request(
        &index,
        PluginUiRequest {
            token: 3,
            kind: PluginUiRequestKind::ReplaceUi {
                handle,
                expected_tree_revision: 99,
                tree: tree(2),
            },
        },
    );
    assert_eq!(invalid.error.unwrap().code, PluginUiErrorCode::StaleTree);
    assert_eq!(session.copy_counts(), after);
    let replaced = session.plugin_request(
        &index,
        PluginUiRequest {
            token: 4,
            kind: PluginUiRequestKind::ReplaceUi {
                handle,
                expected_tree_revision: 1,
                tree: tree(2),
            },
        },
    );
    assert!(replaced.error.is_none());
    assert_eq!(replaced.tree_revision, Some(2));
    assert_eq!(session.copy_counts(), after);
    session.reconcile_windows();
    let idle = session.copy_counts();
    for _ in 0..256 {
        session.reconcile_windows();
    }
    assert_eq!(session.copy_counts(), idle);
    session.remove_catalog_load("demo", 77);
    let stale = session.plugin_request(
        &index,
        PluginUiRequest {
            token: 5,
            kind: PluginUiRequestKind::ReplaceUi {
                handle,
                expected_tree_revision: 2,
                tree: tree(3),
            },
        },
    );
    assert_eq!(
        stale.error.unwrap().code,
        PluginUiErrorCode::LoadUnavailable
    );
    assert_eq!(session.copy_counts(), idle);
    eprintln!(
        "request copies (graph, host): invalid=(0,0), unchanged=(0,0), valid-open={valid_copies:?}, valid-retained=(0,0); 256 idle reconciliations=(0,0), content builds=0"
    );
}

#[test]
fn production_02_retained_and_private_clone_refusals_do_not_touch_host_tokens_or_graph() {
    use crate::plugins::PluginPrivateCellPolicy;
    use crate::plugins::PluginRequestedPlacement;
    use crate::plugins::PluginUiNode;
    use crate::plugins::PluginUiNodeKind;
    use crate::plugins::PluginUiRequest;
    use crate::plugins::PluginUiRequestKind;
    use crate::plugins::PluginUiStyle;
    use crate::plugins::PluginUiTree;
    for private in [false, true] {
        let mut session = TestSession::new(crate::config::load_bundled_config());
        session.new_buffer();
        session.resize(100, 30);
        let mut manifest =
            crate::app::tests::plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
        if private {
            manifest
                .capabilities
                .insert(crate::plugins::PluginCapability::PrivateCells);
            let contribution = &mut manifest.contributions.content_types[0];
            contribution.presenter = crate::plugins::PluginPresenterKind::PrivateCells;
            contribution.private_cells = Some(PluginPrivateCellPolicy {
                maximum_cells: 128,
                maximum_patch_cells: 32,
                maximum_dirty_rectangles: 8,
                maximum_styles: 8,
                maximum_hit_regions: 8,
                maximum_bytes: 4_096,
                cursor: true,
            });
        }
        let index = crate::plugins::index_manifest_contributions(
            &manifest,
            77,
            "clone refusal fixture".into(),
        )
        .unwrap();
        session.install_plugin_fixture(index.clone());
        let tree = (!private).then_some(PluginUiTree {
            revision: 1,
            root: 1,
            nodes: vec![PluginUiNode {
                id: 1,
                kind: PluginUiNodeKind::Text,
                text: "retained".into(),
                style: PluginUiStyle::Accent,
                action: None,
                children: Vec::new(),
            }],
        });
        let opened = session.plugin_request(
            &index,
            PluginUiRequest {
                token: 41,
                kind: PluginUiRequestKind::OpenWindow {
                    expected_graph_revision: session.graph_revision(),
                    content_type: "plugin.demo.panel".into(),
                    title: "Demo".into(),
                    placement: PluginRequestedPlacement::Floating {
                        x: 5,
                        y: 3,
                        width: 24,
                        height: 7,
                    },
                    tree,
                },
            },
        );
        assert!(opened.error.is_none(), "{:?}", opened.error);
        let target = session.plugin_window_target("plugin.demo");
        let before = session.plugin_clone_state();
        session.clone_plugin_window(target.clone());
        assert_eq!(session.plugin_clone_state(), before);
        session.attempt_host_plugin_window_operations(&target);
        assert_eq!(
            session.plugin_clone_state(),
            before,
            "production 0.2 host commands cannot bypass plugin-owned mutation authority"
        );

        session.remove_catalog_load("demo", 77);
        let unloaded = session.plugin_clone_state();
        session.clone_plugin_window(target.clone());
        assert_eq!(session.plugin_clone_state(), unloaded);

        let replacement = crate::plugins::index_manifest_contributions(
            &manifest,
            78,
            "replacement fixture".into(),
        )
        .unwrap();
        session.install_plugin_fixture(replacement);
        let replaced = session.plugin_clone_state();
        session.clone_plugin_window(target);
        assert_eq!(session.plugin_clone_state(), replaced);
    }
}

#[test]
fn catalog_change_without_configuration_change_rejects_before_host_or_graph_staging() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.frame();
    let manifest =
        crate::app::tests::plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 77, "catalog fixture".into())
            .unwrap();
    let policy = session.window_policy_snapshot();
    let copies = session.copy_counts();
    assert!(session.reject_catalog_contribution(index).is_err());
    assert_eq!(session.copy_counts(), copies);
    assert_eq!(session.window_policy_snapshot(), policy);
}

#[test]
fn terminal_selection_does_not_capture_keys_and_temporary_input_precedes_terminal_bytes() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.resize(100, 30);
    session.terminal_fixture();
    assert_eq!(
        (
            session.selected_terminal(),
            session.displayed_terminal(),
            session.keyboard_terminal(),
        ),
        (Some(1), Some(1), Some(1))
    );
    session.select_terminal_without_focus(2);
    assert_eq!(
        (
            session.selected_terminal(),
            session.displayed_terminal(),
            session.keyboard_terminal(),
        ),
        (Some(2), Some(1), Some(1)),
        "manager selection must not change display or keyboard routing"
    );
    session.background_completion_fixture();
    assert_eq!(session.context(), "terminal");
    assert_eq!(session.keyboard_terminal(), Some(1));
    session.open_command_prompt();
    assert_eq!(session.context(), "command_prompt");
    assert_eq!(session.keyboard_terminal(), None);
    session.key("j");
    assert_eq!(session.projection().prompt.as_ref().unwrap().input, "j");
    session.close_prompt();
    assert_eq!(session.keyboard_terminal(), Some(1));
    session.documentation((0..100).map(|line| format!("line {line}\n")).collect());
    assert_eq!(session.context(), "documentation");
    assert_eq!(session.keyboard_terminal(), None);
    session.key("j");
    assert!(
        session
            .projection()
            .documentation_popup
            .as_ref()
            .unwrap()
            .scroll
            > 0
    );
    session.close_documentation();
    assert_eq!(session.keyboard_terminal(), Some(1));
}
