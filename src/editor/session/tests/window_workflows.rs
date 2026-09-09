use super::driver::TestSession;
use crate::ui::Style;

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
    assert_eq!((body.x, body.y, body.height), (5, 8, 4));
    assert_eq!(
        session.projection().explorer.as_ref().unwrap().rows.len(),
        4
    );
    session.page_explorer(1);
    let explorer = session.projection().explorer.as_ref().unwrap();
    assert_eq!(explorer.scroll, 1);
    assert!(explorer.rows.last().unwrap().selected);
    let frame = session.frame();
    assert_eq!(frame.cursor.unwrap().1, body.y + body.height - 1);
    session.click(body.x + 1, body.y + 1);
    assert!(session.projection().explorer.as_ref().unwrap().rows[1].selected);
    let selected = session
        .projection()
        .explorer
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .find(|row| row.selected)
        .unwrap()
        .node_id;
    session.click(body.x + 1, body.y + body.height); // Reserved status row is not an item.
    assert_eq!(
        session
            .projection()
            .explorer
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .find(|row| row.selected)
            .unwrap()
            .node_id,
        selected
    );
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
fn wide_notice_text_stays_inside_its_configured_cell_rectangle() {
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.ui.notifications.left = 2;
    loaded.config.ui.notifications.max_width = 6;
    let policy = loaded.config.clone();
    let mut session = TestSession::new(loaded);
    session.resize(40, 12);
    let before = session.frame();
    session.notice("界界界界界界");
    let after = session.frame();
    let rect = crate::ui::geometry::notification_geometry(&policy, 40, 12).unwrap();
    for y in 0..12 {
        for x in 0..40 {
            if y != rect.y || x < rect.x || x >= rect.x + rect.width {
                assert_eq!(
                    before.grid.get(x, y),
                    after.grid.get(x, y),
                    "outside notice at {x},{y}"
                );
            }
        }
    }
    assert_eq!(after.grid.get(rect.x, rect.y).unwrap().symbol, "界");
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
    let mut candidate = crate::config::load_bundled_config();
    candidate.config.workspace.maximum_picker_items += 1;
    session.apply_candidate(candidate).unwrap();
    session.frame();
    assert!(
        session.projection().picker.is_none(),
        "stale rows survived accepted configuration"
    );
    session.key("enter");
    assert!(session.projection().picker.is_none());
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
fn plugin_admission_counts_all_graph_and_host_copies_in_invalid_unchanged_valid_and_idle_cases() {
    use crate::extensions::{
        PluginRequestedPlacement, PluginUiErrorCode, PluginUiNode, PluginUiNodeKind,
        PluginUiRequest, PluginUiRequestKind, PluginUiStyle, PluginUiTree,
    };
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.resize(100, 30);
    let manifest = super::plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 77, "admission fixture".into())
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
    assert_eq!(stale.error.unwrap().code, PluginUiErrorCode::GenerationLost);
    assert_eq!(session.copy_counts(), idle);
    eprintln!(
        "admission copies (graph, host): invalid=(0,0), unchanged=(0,0), valid-open={valid_copies:?}, valid-retained=(0,0); 256 idle reconciliations=(0,0), content builds=0"
    );
}

#[test]
fn catalog_change_without_configuration_change_rejects_before_host_or_graph_staging() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.new_buffer();
    session.frame();
    let manifest = super::plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 77, "catalog fixture".into())
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
    assert_eq!(session.keyboard_terminal(), Some(1));
    session.select_terminal_without_focus(2);
    assert_eq!(session.keyboard_terminal(), Some(1));
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
