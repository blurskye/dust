//! Viewport tests for window commands, focus and application state.

use crate::app::FULL_PAGE_PERCENT;
use crate::app::tests::*;
use crate::ui::render::documentation::DOCUMENTATION_SCROLL_STEP;

#[test]
pub(in crate::app) fn adversarial_grapheme_rows_make_render_mouse_and_reversed_pages_agree() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((3, 7));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = true;
        fixture.settings.editor.line_numbers = crate::config::LineNumbers::Off;
        fixture.settings.editor.viewport.vertical_margin = 0;
        fixture.settings.ui.status_line.enabled = false;
        app.configuration.install_fixture(fixture);
    }
    let (_, view) = app
        .state
        .open_text(
            TextBuffer::from_text("aa界界\naa\t\t\ne\u{301}界x\ntail"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let viewport = active_pane_viewport(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.workspace_bounds().unwrap().width,
        app.workspace_render_height(),
    )
    .unwrap();
    assert_eq!(viewport.width, 3);

    // Padding before another wrapped row stays on the clicked row's last grapheme.
    // Expected rows and byte offsets are specified independently of VisualRowModel.
    for (line, rows) in [
        (
            0,
            vec![("aa ", [0, 1, 1]), ("界 ", [2, 2, 2]), ("界 ", [5, 5, 8])],
        ),
        (
            1,
            vec![
                ("aa ", [9, 10, 10]),
                ("   ", [11, 11, 11]),
                ("   ", [12, 12, 12]),
            ],
        ),
        (2, vec![("e\u{301}界", [14, 17, 17]), ("x  ", [20, 21, 21])]),
    ] {
        app.state.views[view].scroll_line = line;
        app.state.views[view].scroll_wrap_row = 0;
        let frame = render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            app.window_session.width(),
            app.window_session.height(),
        );
        for (wrap_row, (painted, offsets)) in rows.iter().enumerate() {
            let y = viewport.y + u16::try_from(wrap_row).unwrap();
            assert_eq!(
                frame.grid.row_text(y),
                *painted,
                "line {line} row {wrap_row}"
            );
            if painted.contains('界') {
                let x = if line == 2 { 1 } else { 0 };
                assert_eq!(frame.grid.get(x, y).unwrap().symbol, "界");
                assert!(frame.grid.get(x + 1, y).unwrap().continuation);
            }
            for (cell, &expected) in offsets.iter().enumerate() {
                app.state.views[view].scroll_line = line;
                app.state.views[view].scroll_wrap_row = 0;
                app.input
                    .observe_mouse(viewport.x + u16::try_from(cell).unwrap(), y);
                app.apply_mouse_cursor(false);
                assert_eq!(
                    app.state.views[view].cursor,
                    ByteOffset(expected),
                    "mouse line {line} row {wrap_row} cell {cell}"
                );
            }
        }
    }

    app.state.views[view].cursor = ByteOffset(0);
    app.state.views[view].scroll_line = 0;
    app.state.views[view].scroll_wrap_row = 0;
    let body = usize::from(viewport.height.max(1));
    let amount = body.saturating_mul(usize::from(FULL_PAGE_PERCENT)) / 100;
    let expected_rows = [
        (0, 0),
        (0, 1),
        (0, 2),
        (1, 0),
        (1, 1),
        (1, 2),
        (2, 0),
        (2, 1),
        (3, 0),
        (3, 1),
    ];
    let (line, wrap_row) = expected_rows[amount.max(1)];
    let expected_down = VisualRowAnchor { line, wrap_row };
    app.scroll_view("full_down", 1);
    assert_eq!(
        VisualRowAnchor {
            line: app.state.views[view].scroll_line,
            wrap_row: app.state.views[view].scroll_wrap_row,
        },
        expected_down
    );
    app.scroll_view("full_up", 1);
    assert_eq!(app.state.views[view].scroll_line, 0);
    assert_eq!(app.state.views[view].scroll_wrap_row, 0);
}

#[test]
pub(in crate::app) fn wrapped_visual_rows_drive_margin_paging_resize_folds_and_mouse_consistently()
{
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((12, 8));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = true;
        fixture.settings.editor.line_numbers = crate::config::LineNumbers::Off;
        fixture.settings.ui.status_line.enabled = false;
        fixture.settings.editor.viewport.vertical_margin = 2;
        app.configuration.install_fixture(fixture);
    }
    let text = format!(
        "{}\nfold\nhidden 界\nhidden e\u{301}\ntail",
        "x".repeat(150)
    );
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text(&text), None)
        .unwrap();
    let buffer = app.state.views[view].buffer;
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let viewport = active_pane_viewport(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.workspace_bounds().unwrap().width,
        app.workspace_render_height(),
    )
    .unwrap();
    app.state.views[view].cursor = ByteOffset(120);
    app.ensure_cursor_visible();
    assert!(app.state.views[view].scroll_wrap_row > 0);
    let model = VisualRowModel::new(
        &app.state.buffers[buffer].text,
        view,
        &app.ui,
        app.configuration.settings(),
        viewport.width,
        true,
    );
    let start = VisualRowAnchor {
        line: app.state.views[view].scroll_line,
        wrap_row: app.state.views[view].scroll_wrap_row,
    };
    let cursor = model.cursor_anchor(app.state.views[view].cursor);
    let row = model
        .screen_row(start, cursor, usize::from(viewport.height))
        .unwrap();
    assert!(row >= 2 && row < usize::from(viewport.height).saturating_sub(2));

    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.viewport.vertical_margin = 100;
        app.configuration.install_fixture(fixture);
    }
    app.window_session
        .observe_size((app.window_session.width(), 4));
    app.ensure_cursor_visible();
    let tiny_viewport = active_pane_viewport(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.workspace_bounds().unwrap().width,
        app.workspace_render_height(),
    )
    .unwrap();
    let tiny_model = VisualRowModel::new(
        &app.state.buffers[buffer].text,
        view,
        &app.ui,
        app.configuration.settings(),
        tiny_viewport.width,
        true,
    );
    assert!(
        tiny_model
            .screen_row(
                VisualRowAnchor {
                    line: app.state.views[view].scroll_line,
                    wrap_row: app.state.views[view].scroll_wrap_row,
                },
                tiny_model.cursor_anchor(app.state.views[view].cursor),
                usize::from(tiny_viewport.height),
            )
            .is_some()
    );

    app.window_session
        .observe_size((app.window_session.width(), 8));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.viewport.vertical_margin = 0;
        app.configuration.install_fixture(fixture);
    }
    app.state.views[view].cursor = ByteOffset(0);
    app.state.views[view].scroll_line = 0;
    app.state.views[view].scroll_wrap_row = 0;
    app.scroll_view("full_down", 1);
    let down = app.state.views[view].scroll_wrap_row;
    assert!(down > 0);
    app.scroll_view("full_up", 1);
    assert_eq!(app.state.views[view].scroll_line, 0);
    assert_eq!(app.state.views[view].scroll_wrap_row, 0);

    app.ui.lsp_folds = vec![UiFold {
        view,
        start_line: 1,
        end_line: 3,
        collapsed: true,
    }];
    app.state.views[view].cursor = app.state.buffers[buffer].text.line_to_byte(2).unwrap();
    app.ensure_cursor_visible();
    let folded_model = VisualRowModel::new(
        &app.state.buffers[buffer].text,
        view,
        &app.ui,
        app.configuration.settings(),
        viewport.width,
        true,
    );
    assert_eq!(
        folded_model
            .cursor_anchor(app.state.views[view].cursor)
            .line,
        1
    );

    app.ui.lsp_folds.clear();
    app.state.views[view].cursor = ByteOffset(0);
    app.state.views[view].scroll_line = 0;
    app.state.views[view].scroll_wrap_row = 0;
    let workspace = app.workspace_bounds().unwrap();
    app.input.observe_mouse(
        workspace.x.saturating_add(viewport.x).saturating_add(1),
        workspace.y.saturating_add(viewport.y).saturating_add(2),
    );
    app.apply_mouse_cursor(false);
    let mouse_model = VisualRowModel::new(
        &app.state.buffers[buffer].text,
        view,
        &app.ui,
        app.configuration.settings(),
        viewport.width,
        true,
    );
    assert_eq!(
        mouse_model
            .cursor_anchor(app.state.views[view].cursor)
            .wrap_row,
        2
    );
    app.state.views[view].scroll_wrap_row = 1;
    let sibling = app
        .state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("split view");
    assert_eq!(app.state.views[sibling].scroll_wrap_row, 1);
    app.reconcile_editor_windows();
    app.ensure_cursor_visible();
    let split_viewport = active_pane_viewport(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.workspace_bounds().unwrap().width,
        app.workspace_render_height(),
    )
    .unwrap();
    assert!(split_viewport.width > 0 && split_viewport.height > 0);
    let reconciliation_count = app.window_session.test_publications();
    let window_revision = app.state.window_revision();
    let transaction = app.next_transaction;
    let accepted_scroll = (
        app.state.views[sibling].scroll_line,
        app.state.views[sibling].scroll_wrap_row,
        app.state.views[sibling].scroll_column,
    );
    for _ in 0..256 {
        app.ensure_cursor_visible();
        let _ = render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            app.window_session.width(),
            app.window_session.height(),
        );
    }
    assert_eq!(app.window_session.test_publications(), reconciliation_count);
    assert_eq!(app.state.window_revision(), window_revision);
    assert_eq!(app.next_transaction, transaction);
    assert_eq!(
        (
            app.state.views[sibling].scroll_line,
            app.state.views[sibling].scroll_wrap_row,
            app.state.views[sibling].scroll_column,
        ),
        accepted_scroll
    );
}

#[test]
pub(in crate::app) fn vertical_margin_keeps_cursor_context_above_and_below() {
    let (_app_dirs, mut app) = app();
    app.window_session
        .observe_size((app.window_session.width(), 10));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.viewport.vertical_margin = 2;
        app.configuration.install_fixture(fixture);
    }
    let text = (0..30).map(|n| format!("line {n}\n")).collect::<String>();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text(&text), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.views[view].cursor = app.state.buffers[app.state.views[view].buffer]
        .text
        .line_to_byte(15)
        .unwrap();
    app.ensure_cursor_visible();
    let first = app.state.views[view].scroll_line;
    assert!(15usize.saturating_sub(first) >= 2);
    let viewport = active_pane_viewport(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.window_session.width(),
        app.workspace_render_height(),
    )
    .unwrap();
    assert!(15 < first + usize::from(viewport.height).saturating_sub(2));
    app.state.views[view].cursor = app.state.buffers[app.state.views[view].buffer]
        .text
        .line_to_byte(first + 1)
        .unwrap();
    app.ensure_cursor_visible();
    assert!(app.state.views[view].scroll_line < first);
}

#[test]
pub(in crate::app) fn exact_pane_horizontal_follow_is_display_safe_and_view_owned() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((18, 8));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = false;
        app.configuration.install_fixture(fixture);
    }
    let text = "a\t界e\u{301}bcdefghijklmnopqrstuvwxyz";
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text(text), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.views[view].cursor = ByteOffset(text.len());
    app.ensure_cursor_visible();
    let shifted = app.state.views[view].scroll_column;
    assert!(shifted > 0);
    assert_eq!(
        shifted,
        snap_display_column(
            text,
            shifted,
            app.configuration.settings().editor.tab_width,
            app.configuration.settings().editor.ambiguous_width,
        )
    );
    let sibling = app
        .state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("split view");
    assert_eq!(app.state.views[sibling].scroll_column, shifted);
    let first_pane = app.state.view_window(view).expect("first pane");
    app.state
        .focus_pane_id(first_pane)
        .expect("first view focus");
    app.state.views[view].cursor = ByteOffset(0);
    app.ensure_cursor_visible();
    assert_eq!(app.state.views[view].scroll_column, 0);
    assert_eq!(app.state.views[sibling].scroll_column, shifted);
    app.state.views[view].scroll_column = 8;
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = true;
        app.configuration.install_fixture(fixture);
    }
    app.ensure_cursor_visible();
    assert_eq!(app.state.views[view].scroll_column, 0);
}

#[test]
pub(in crate::app) fn cursor_follow_skips_an_unrenderable_tab_in_a_narrow_pane() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("a\tX"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.views[view].cursor = ByteOffset("a\t".len());
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = false;
        fixture.settings.editor.line_numbers = crate::config::LineNumbers::Off;
        fixture.settings.editor.viewport.horizontal_margin = 0;
        fixture.settings.ui.buffer_line.hide_when_single = true;
        app.configuration.install_fixture(fixture);
    }
    app.window_session.observe_size((2, 6));
    app.ensure_cursor_visible();
    assert_eq!(app.state.views[view].scroll_column, 4);
    let frame = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    assert!(frame.cursor.is_some_and(|(x, y, _)| x == 0 && y == 0));
    app.input.observe_mouse(1, 0);
    app.apply_mouse_cursor(false);
    assert_eq!(app.state.views[view].cursor, ByteOffset("a\tX".len()));

    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.line_numbers = crate::config::LineNumbers::Absolute;
        app.configuration.install_fixture(fixture);
    }
    app.state.views[view].cursor = ByteOffset("a\t".len());
    app.state.views[view].scroll_column = 0;
    app.window_session
        .observe_size((1, app.window_session.height()));
    app.ensure_cursor_visible();
    assert_eq!(app.state.views[view].scroll_column, 4);
    let one_cell = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    assert!(one_cell.cursor.is_some_and(|(x, _, _)| x == 0));

    app.window_session
        .observe_size((2, app.window_session.height()));
    app.ensure_cursor_visible();
    let two_cell = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    assert!(two_cell.cursor.is_some_and(|(x, _, _)| x == 1));
    app.input.observe_mouse(1, 0);
    app.apply_mouse_cursor(false);
    assert_eq!(app.state.views[view].cursor, ByteOffset("a\t".len()));

    app.window_session
        .observe_size((50, app.window_session.height()));
    app.ensure_cursor_visible();
    let widened = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    assert!(
        widened
            .cursor
            .is_some_and(|(x, _, _)| x < app.window_session.width())
    );

    let buffer = app.state.views[view].buffer;
    app.window_session
        .observe_size((2, app.window_session.height()));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = true;
        app.configuration.install_fixture(fixture);
    }
    app.ui.large_file_buffers.push(buffer);
    app.state.views[view].scroll_column = 0;
    app.ensure_cursor_visible();
    assert_eq!(app.state.views[view].scroll_column, 4);
    app.ui
        .large_file_buffers
        .retain(|candidate| *candidate != buffer);
    app.ensure_cursor_visible();
    assert_eq!(app.state.views[view].scroll_column, 0);

    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = false;
        fixture.settings.editor.line_numbers = crate::config::LineNumbers::Off;
        app.configuration.install_fixture(fixture);
    }
    app.window_session
        .observe_size((2, app.window_session.height()));
    app.state.buffers[buffer].text = TextBuffer::from_text("ab界X");
    app.state.views[view].cursor = ByteOffset("ab界".len());
    app.state.views[view].scroll_column = 0;
    app.ensure_cursor_visible();
    assert_eq!(app.state.views[view].scroll_column, 4);
    let wide = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    assert!(wide.cursor.is_some_and(|(x, _, _)| x == 0));

    {
        let fixture = app.configuration.fixture_snapshot();
        app.configuration.install_fixture(fixture);
    }
    app.window_session
        .observe_size((6, app.window_session.height()));
    app.state
        .split_active(crate::windows::TileAxis::Horizontal, 100)
        .expect("minimum-ratio split");
    app.state.focus_pane(-1, false).expect("zero-width leaf");
    let zero_view = app.state.active_view().expect("zero-width view");
    app.state.views[zero_view].scroll_column = 9;
    app.ensure_cursor_visible();
    let zero_viewport = active_pane_viewport(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.window_session.width(),
        app.workspace_render_height(),
    )
    .expect("focused tiny-layout viewport");
    assert_eq!(zero_viewport.width, 6);
    assert_eq!(app.state.views[zero_view].scroll_column, 4);
}

#[test]
pub(in crate::app) fn session_layout_capture_and_restore_preserve_each_view_scroll_column() {
    let path = PathBuf::from("/workspace/main.rs");
    let text = TextBuffer::from_text("abcdefghijklmnopqrstuvwxyz");
    let mut source = crate::editor::EditorState::new(PathBuf::from("/workspace"));
    let (_, first) = source.open_text(text.clone(), Some(path.clone())).unwrap();
    source.views[first].scroll_column = 4;
    let second = source
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("split source view");
    source.views[second].scroll_column = 17;
    let source_tab = source.active_tab().expect("source tab");
    let persisted = capture_persisted_layout(
        &source,
        &source.tab_layout(source_tab).unwrap(),
        std::slice::from_ref(&source.workspace_root),
    )
    .expect("capture clean layout");

    let mut restored = crate::editor::EditorState::new(PathBuf::from("/workspace"));
    let loaded = HashMap::from([(path.clone(), (path.clone(), text))]);
    let (_, panes) = build_restored_layout(&mut restored, &persisted, &loaded)
        .expect("restore persisted layout");
    let columns = panes
        .iter()
        .map(|(_, pane)| restored.views[restored.window_view(*pane).unwrap()].scroll_column)
        .collect::<Vec<_>>();
    assert_eq!(columns, vec![4, 17]);
}

#[test]
pub(in crate::app) fn session_capture_keeps_project_panes_after_external_navigation() {
    let mut state = crate::editor::EditorState::new("/workspace".into());
    let path = PathBuf::from("/workspace/main.rs");
    state
        .open_text(TextBuffer::from_text("project"), Some(path.clone()))
        .unwrap();
    state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .unwrap();
    let (external, _) = state
        .open_text(
            TextBuffer::from_text("library"),
            Some("/library/lib.rs".into()),
        )
        .unwrap();
    state.buffers[external].read_only = true;
    let tab = state.active_tab().unwrap();
    let roots = std::slice::from_ref(&state.workspace_root);
    let layout = capture_persisted_layout(&state, &state.tab_layout(tab).unwrap(), roots).unwrap();
    state.buffers[external].read_only = false;
    let writable_layout =
        capture_persisted_layout(&state, &state.tab_layout(tab).unwrap(), roots).unwrap();
    assert_eq!(
        serde_json::to_value(&layout).unwrap(),
        serde_json::to_value(&writable_layout).unwrap()
    );
    assert!(
        matches!(layout, crate::persistence::PersistedLayout::Pane { pane } if pane.path == path)
    );
}

#[test]
pub(in crate::app) fn horizontal_follow_reconciles_gutter_explorer_resize_and_nested_panes() {
    let (_app_dirs, mut app) = app();
    let text = format!("{}\nx", "0123456789".repeat(8));
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text(&text), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = false;
        fixture.settings.editor.line_numbers = crate::config::LineNumbers::Off;
        app.configuration.install_fixture(fixture);
    }
    app.window_session.observe_size((90, 18));
    app.state.views[view].cursor = ByteOffset(80);
    app.ensure_cursor_visible();
    assert_eq!(app.state.views[view].scroll_column, 0);

    app.ui.explorer = Some(ExplorerUiState {
        title: "Files".into(),
        rows: Vec::new(),
        scroll: 0,
        loading: false,
        total: 0,
        matches: 0,
        query: String::new(),
        searching: false,
    });
    app.ensure_cursor_visible();
    let docked = app.state.views[view].scroll_column;
    assert!(docked > 0);
    let panel = explorer_panel_width(app.configuration.settings(), app.window_session.width())
        .expect("explorer panel");
    let frame = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    assert!(frame.cursor.is_some_and(|(x, y, _)| {
        x > panel && x < app.window_session.width() && y < app.window_session.height() - 1
    }));

    app.window_session
        .observe_size((30, app.window_session.height()));
    app.ensure_cursor_visible();
    let narrowed = app.state.views[view].scroll_column;
    assert!(narrowed > docked);
    app.ui.explorer = None;
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.line_numbers = crate::config::LineNumbers::Absolute;
        app.configuration.install_fixture(fixture);
    }
    app.state.views[view].scroll_column = 0;
    app.ensure_cursor_visible();
    let gutter_shifted = app.state.views[view].scroll_column;
    assert!(gutter_shifted > 0);
    app.window_session
        .observe_size((100, app.window_session.height()));
    app.ensure_cursor_visible();
    let widened = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    assert!(widened.cursor.is_some_and(|(x, y, _)| {
        x < app.window_session.width() && y < app.window_session.height() - 1
    }));

    // Moving onto a shorter line must pull a stale horizontal offset back.
    app.state.views[view].cursor = ByteOffset(text.len());
    app.ensure_cursor_visible();
    assert_eq!(app.state.views[view].scroll_column, 0);

    // Unequal nested splits use the exact focused leaf width and keep the
    // terminal cursor inside that leaf after the geometry changes.
    app.window_session
        .observe_size((90, app.window_session.height()));
    app.state.views[view].cursor = ByteOffset(80);
    app.state
        .split_active(crate::windows::TileAxis::Horizontal, 300)
        .expect("first split");
    app.state
        .split_active(crate::windows::TileAxis::Horizontal, 400)
        .expect("nested split");
    let focused = app.state.active_view().expect("focused nested view");
    app.state.views[focused].scroll_column = 0;
    app.ensure_cursor_visible();
    let viewport = active_pane_viewport(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.window_session.width(),
        app.workspace_render_height(),
    )
    .expect("focused leaf viewport");
    assert!(app.state.views[focused].scroll_column > 0);
    let frame = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    assert!(frame.cursor.is_some_and(|(x, y, _)| {
        x >= viewport.x
            && x < viewport.x.saturating_add(viewport.width)
            && y >= viewport.y
            && y < viewport.y.saturating_add(viewport.height)
    }));
}

#[test]
pub(in crate::app) fn documentation_keys_scroll_clamp_and_close_the_popup_context() {
    let (_app_dirs, mut app) = app();
    {
        let fixture = app.configuration.fixture_snapshot();
        app.configuration.install_fixture(fixture);
    }
    app.present_documentation_popup(
        DocumentationPopupKind::Hover,
        "Hover".into(),
        "test".into(),
        None,
        vec![crate::language::lsp::LspMarkupContent {
            kind: crate::language::lsp::LspMarkupKind::PlainText,
            value: (0..200)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        }],
    );
    assert_eq!(app.active_input_context(), "documentation");
    app.handle_configured_key(KeyEvent::character('j'));
    assert_eq!(
        app.ui
            .documentation_popup
            .as_ref()
            .map(|popup| popup.scroll),
        Some(DOCUMENTATION_SCROLL_STEP)
    );
    app.handle_configured_key("ctrl-d".parse::<KeyEvent>().expect("Ctrl-d"));
    let paged = app
        .ui
        .documentation_popup
        .as_ref()
        .map(|popup| popup.scroll)
        .expect("popup");
    assert!(paged > DOCUMENTATION_SCROLL_STEP);
    app.handle_configured_key(KeyEvent::character('k'));
    assert_eq!(
        app.ui
            .documentation_popup
            .as_ref()
            .map(|popup| popup.scroll),
        Some(paged - DOCUMENTATION_SCROLL_STEP)
    );
    app.handle_configured_key("ctrl-u".parse::<KeyEvent>().expect("Ctrl-u"));
    assert_eq!(
        app.ui
            .documentation_popup
            .as_ref()
            .map(|popup| popup.scroll),
        Some(0)
    );
    app.handle_configured_key(KeyEvent::character('q'));
    assert!(app.ui.documentation_popup.is_none());
    app.invoke_mapped_command("lsp.documentation.scroll_down");
    assert!(app.ui.documentation_popup.is_none());
    assert_ne!(app.active_input_context(), "documentation");

    app.present_documentation_popup(
        DocumentationPopupKind::Hover,
        "Hover".into(),
        "test".into(),
        None,
        vec![crate::language::lsp::LspMarkupContent {
            kind: crate::language::lsp::LspMarkupKind::PlainText,
            value: (0..200)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        }],
    );
    app.handle_event(crate::ui::UiEvent::Mouse {
        key: KeyEvent::mouse(
            crate::editor::input::KeyCode::MouseScrollDown,
            crate::editor::input::Modifiers::default(),
        ),
        column: 0,
        row: 0,
    });
    assert_eq!(
        app.ui
            .documentation_popup
            .as_ref()
            .map(|popup| popup.scroll),
        Some(DOCUMENTATION_SCROLL_STEP)
    );
}

#[test]
pub(in crate::app) fn dragging_the_border_between_two_windows_resizes_them() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((100, 30));
    app.state
        .open_text(TextBuffer::from_text("a\n"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.apply_layout_action(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    ));
    app.reconcile_editor_windows();
    let border = |app: &App| {
        app.compositor_projection()
            .expect("projection")
            .borders
            .iter()
            .find(|border| border.axis == crate::windows::TileAxis::Horizontal)
            .copied()
            .expect("side by side border")
    };
    let before = border(&app);
    let mouse = |app: &mut App, code, column| {
        app.handle_event(crate::ui::UiEvent::Mouse {
            key: KeyEvent::mouse(code, crate::editor::input::Modifiers::default()),
            column,
            row: before.y + 2,
        });
    };
    use crate::editor::input::KeyCode::MouseLeftDown;
    use crate::editor::input::KeyCode::MouseLeftDrag;
    use crate::editor::input::KeyCode::MouseLeftUp;
    mouse(&mut app, MouseLeftDown, before.x);
    mouse(&mut app, MouseLeftDrag, before.x - 20);
    mouse(&mut app, MouseLeftUp, before.x - 20);
    let after = border(&app);
    assert!(
        after.x.abs_diff(before.x - 20) <= 1,
        "{} -> {}",
        before.x,
        after.x
    );
}

#[test]
pub(in crate::app) fn the_agent_panel_opens_narrow_and_ctrl_arrows_move_by_word_while_typing_and_resize_it_in_normal_mode()
 {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((160, 45));
    app.state
        .open_text(TextBuffer::from_text("a\n"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in [' ', 'a', 't'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.reconcile_editor_windows();
    let border = |app: &App| {
        let projection = app.compositor_projection().expect("projection");
        let borders = projection
            .borders
            .iter()
            .filter(|border| border.axis == crate::windows::TileAxis::Horizontal)
            .map(|border| border.x)
            .collect::<Vec<_>>();
        let windows = projection
            .windows
            .iter()
            .map(|window| (window.rect.x, window.rect.width))
            .collect::<Vec<_>>();
        (
            borders,
            format!(
                "{windows:?} {:?}",
                app.state
                    .tab_layout(app.state.active_tab().unwrap())
                    .unwrap()
            ),
        )
    };
    let (opened, layout) = border(&app);
    assert_eq!(opened.len(), 1, "{layout}");
    assert!(
        (100..=106).contains(&opened[0]),
        "the panel opens at about 35%: {layout}"
    );
    // While typing, ctrl-left and ctrl-right move by word, as in any other input.
    for key in "ask it".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.handle_configured_key("ctrl-left".parse::<KeyEvent>().expect("ctrl-left"));
    let cursor = |app: &App| app.ui.agent_panel.as_ref().map(|panel| panel.cursor);
    assert_eq!(cursor(&app), Some(4));
    assert_eq!(border(&app).0, opened);
    app.handle_configured_key("ctrl-right".parse::<KeyEvent>().expect("ctrl-right"));
    assert_eq!(cursor(&app), Some(6));
    // As in LazyVim, ctrl-left narrows the focused window in Normal mode and ctrl-right
    // widens it.
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.handle_configured_key("ctrl-left".parse::<KeyEvent>().expect("ctrl-left"));
    let (narrowed, layout) = border(&app);
    assert!(
        narrowed[0] > opened[0],
        "ctrl-left narrows the panel: {opened:?} -> {layout}"
    );
    app.handle_configured_key("ctrl-right".parse::<KeyEvent>().expect("ctrl-right"));
    let (widened, layout) = border(&app);
    assert!(
        widened[0] < narrowed[0],
        "ctrl-right widens it: {narrowed:?} -> {layout}"
    );
}

#[test]
pub(in crate::app) fn the_agent_panel_opens_on_the_left_when_its_side_says_so() {
    let (_app_dirs, mut app) = app();
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.settings.agents.side = crate::config::PanelSide::Left;
    app.configuration.install_fixture(fixture);
    app.window_session.observe_size((160, 45));
    app.state
        .open_text(TextBuffer::from_text("a\n"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in [' ', 'a', 't'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.reconcile_editor_windows();
    let projection = app.compositor_projection().expect("projection");
    let borders = projection
        .borders
        .iter()
        .filter(|border| border.axis == crate::windows::TileAxis::Horizontal)
        .map(|border| border.x)
        .collect::<Vec<_>>();
    assert_eq!(borders.len(), 1);
    assert!(
        (54..=60).contains(&borders[0]),
        "the panel takes about 35% on the left: {borders:?}"
    );
    assert!(
        app.ui.agent_panel.is_some(),
        "the focused panel is the agent's"
    );
    let focused = crate::ui::active_editor_pane_viewport(&app.state, &app.ui).expect("focus");
    assert!(
        focused.x < borders[0],
        "the conversation is the left pane: {focused:?}"
    );
}

#[test]
pub(in crate::app) fn scrolling_short_lines_keeps_normal_cursors_on_characters() {
    for mode in [
        crate::editor::EditorMode::Normal,
        crate::editor::EditorMode::Insert,
        crate::editor::EditorMode::Replace,
    ] {
        let (_app_dirs, mut app) = app();
        app.window_session.observe_size((80, 24));
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = false;
        app.configuration.install_fixture(fixture);
        let source = format!("0123456789abcdef\n{}", "x\n".repeat(100));
        let (buffer, view) = app
            .state
            .open_text(TextBuffer::from_text(&source), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.state.views[view].cursor = ByteOffset(10);
        app.state.views[view].mode = mode;
        app.scroll_view("half_down", 1);
        let text = &app.state.buffers[buffer].text;
        let cursor = app.state.views[view].cursor;
        let line = text.byte_to_line(cursor).unwrap();
        assert!(line > 0);
        assert_eq!(
            cursor.get() - text.line_to_byte(line).unwrap().get(),
            usize::from(matches!(
                mode,
                crate::editor::EditorMode::Insert | crate::editor::EditorMode::Replace
            ))
        );
        if mode == crate::editor::EditorMode::Normal {
            app.handle_configured_key(KeyEvent::character('x'));
            assert_eq!(app.state.buffers[buffer].text.line(line).unwrap(), "\n");
        }
    }
}

#[test]
pub(in crate::app) fn cancelling_incremental_search_restores_the_original_viewport() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((40, 20));
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.settings.editor.wrap = false;
    app.configuration.install_fixture(fixture);
    let source = format!("{}needle\n", "line\n".repeat(100));
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text(&source), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let cursor = app.state.buffers[buffer].text.line_to_byte(15).unwrap();
    app.state.views[view].cursor = cursor;
    app.state.views[view].scroll_line = 10;
    app.state.views[view].scroll_column = 0;
    for key in "/needle".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert!(app.state.views[view].scroll_line > 10);
    app.handle_configured_key("escape".parse().unwrap());
    assert_eq!(app.state.views[view].cursor, cursor);
    assert_eq!(app.state.views[view].scroll_line, 10);
    assert_eq!(app.state.views[view].scroll_column, 0);
    assert_eq!(app.state.views[view].scroll_wrap_row, 0);
}

#[test]
pub(in crate::app) fn half_page_scroll_moves_a_cursor_already_inside_the_new_viewport() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((80, 44));
    let text = (0..200).map(|_| "line\n").collect::<String>();
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text(&text), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.state.views[view].cursor = app.state.buffers[buffer].text.line_to_byte(30).unwrap();
    app.scroll_view("half_down", 1);
    let down = app.state.buffers[buffer]
        .text
        .byte_to_line(app.state.views[view].cursor)
        .unwrap();
    assert!(down > 30);
    app.scroll_view("half_up", 1);
    assert_eq!(
        app.state.buffers[buffer]
            .text
            .byte_to_line(app.state.views[view].cursor)
            .unwrap(),
        30
    );
}

#[test]
pub(in crate::app) fn horizontal_cursor_following_counts_inlay_and_substitute_widths() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((40, 12));
    let mut config = app.configuration.fixture_snapshot();
    config.settings.editor.wrap = false;
    config.settings.editor.line_numbers = crate::config::LineNumbers::Off;
    app.configuration.install_fixture(config);
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text(&"x".repeat(60)), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.views[view].cursor = ByteOffset(20);
    app.ui.lsp_inlay_hints.push(crate::ui::UiInlayHint {
        buffer,
        revision: app.state.buffers[buffer].text.revision(),
        position: ByteOffset(5),
        label: "hint ".repeat(5),
    });
    app.ui.substitute_preview.push(crate::ui::UiInlayHint {
        buffer,
        revision: app.state.buffers[buffer].text.revision(),
        position: ByteOffset(10),
        label: "preview".into(),
    });
    app.ensure_cursor_visible();
    assert!(app.state.views[view].scroll_column > 0);
    let frame = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        40,
        12,
    );
    assert!(frame.cursor.is_some());
}

#[test]
pub(in crate::app) fn viewport_accounts_for_inlays_at_the_cursor_exactly() {
    for substitute in [false, true] {
        let (_app_dirs, mut app) = app();
        app.window_session.observe_size((40, 12));
        let mut config = app.configuration.fixture_snapshot();
        config.settings.editor.wrap = false;
        config.settings.editor.line_numbers = crate::config::LineNumbers::Off;
        config.settings.editor.viewport.horizontal_margin = 4;
        app.configuration.install_fixture(config);
        let (buffer, view) = app
            .state
            .open_text(TextBuffer::from_text(&"x".repeat(60)), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.state.views[view].cursor = ByteOffset(20);
        app.ensure_cursor_visible();
        assert_eq!(app.state.views[view].scroll_column, 0);
        let hint = crate::ui::UiInlayHint {
            buffer,
            revision: app.state.buffers[buffer].text.revision(),
            position: ByteOffset(20),
            label: "hint ".repeat(5),
        };
        if substitute {
            app.ui.substitute_preview.push(hint);
        } else {
            app.ui.lsp_inlay_hints.push(hint);
        }
        app.ensure_cursor_visible();
        assert_eq!(app.state.views[view].scroll_column, 10);
        let frame = render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            40,
            12,
        );
        assert_eq!(frame.cursor.map(|(x, y, _)| (x, y)), Some((35, 1)));
    }
}

#[test]
pub(in crate::app) fn horizontal_cursor_following_ignores_hints_hidden_by_scrolling() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((22, 12));
    let mut config = app.configuration.fixture_snapshot();
    config.settings.editor.wrap = false;
    config.settings.editor.line_numbers = crate::config::LineNumbers::Off;
    app.configuration.install_fixture(config);
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text(&"x".repeat(200)), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.views[view].cursor = ByteOffset(40);
    app.ui.lsp_inlay_hints.push(crate::ui::UiInlayHint {
        buffer,
        revision: app.state.buffers[buffer].text.revision(),
        position: ByteOffset(0),
        label: "h".repeat(100),
    });
    for cursor in [40, 41, 60, 30] {
        app.state.views[view].cursor = ByteOffset(cursor);
        app.ensure_cursor_visible();
        let scroll = app.state.views[view].scroll_column;
        assert!(scroll <= cursor, "scroll {scroll} passed cursor {cursor}");
        let frame = render(
            &app.state,
            &app.ui,
            app.configuration.settings(),
            app.configuration.dashboard(),
            22,
            12,
        );
        assert!(
            frame.cursor.is_some(),
            "cursor {cursor} hidden at scroll {scroll}"
        );
    }
}

#[test]
pub(in crate::app) fn operation_bar_visibility_reprojects_the_editor_footer_inset() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((80, 24));
    app.state
        .open_text(TextBuffer::from_text(&"line\n".repeat(60)), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let before = app.compositor_projection().unwrap().workspace_bounds.height;
    app.ui.operation_bar = Some(crate::ui::OperationBarState {
        operation: "merge".into(),
        phase: "conflicts".into(),
        blockers: Vec::new(),
        actions: Vec::new(),
        abort_binding: None,
    });
    app.reconcile_editor_windows();
    let with_bar = app.compositor_projection().unwrap().workspace_bounds.height;
    assert_eq!(with_bar + 1, before);
    app.ui.operation_bar = None;
    app.reconcile_editor_windows();
    assert_eq!(
        app.compositor_projection().unwrap().workspace_bounds.height,
        before
    );
}

#[test]
pub(in crate::app) fn half_page_scrolling_and_cursor_follow_include_expanded_diagnostic_rows() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((80, 8));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.viewport.vertical_margin = 0;
        fixture.settings.ui.status_line.enabled = false;
        app.configuration.install_fixture(fixture);
    }
    let path = PathBuf::from("/tmp/expanded-diagnostics.rs");
    let (buffer, view) = app
        .state
        .open_text(
            TextBuffer::from_text("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl"),
            Some(path.clone()),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let cursor = app.state.buffers[buffer].text.line_to_byte(3).unwrap();
    app.state.views[view].cursor = cursor;
    app.ui.diagnostics.push(crate::ui::UiDiagnostic {
        buffer: None,
        path: path.clone(),
        range: ByteRange::empty(cursor),
        severity: DiagnosticSeverity::Error,
        source: String::new(),
        code: None,
        message: "diagnostic ".repeat(40),
    });
    app.ui.expanded_diagnostic = Some(crate::ui::ExpandedDiagnosticState {
        buffer: None,
        path,
        line: 3,
        severities: vec![DiagnosticSeverity::Error],
    });
    // Cursor follow must not count the expanded rows as ordinary source lines.
    app.state.views[view].cursor = app.state.buffers[buffer].text.line_to_byte(6).unwrap();
    app.ensure_cursor_visible();
    let assert_visible = |app: &App| {
        let viewport = crate::ui::active_editor_pane_viewport(&app.state, &app.ui).unwrap();
        let model = VisualRowModel::new_for_state(
            &app.state,
            &app.state.buffers[buffer].text,
            view,
            &app.ui,
            app.configuration.settings(),
            viewport.width,
            false,
        );
        let start = VisualRowAnchor {
            line: app.state.views[view].scroll_line,
            wrap_row: 0,
        };
        assert!(
            model
                .screen_row(
                    start,
                    model.cursor_anchor(app.state.views[view].cursor),
                    usize::from(viewport.height)
                )
                .is_some()
        );
    };
    assert_visible(&app);
    app.state.views[view].cursor = cursor;
    app.state.views[view].scroll_line = 0;
    app.scroll_view("half_down", 1);
    assert_visible(&app);
    assert!(app.ui.expanded_diagnostic.is_some());
    // A diagnostic block larger than the scroll step must not trap repeated wheel events.
    app.state.views[view].cursor = cursor;
    app.state.views[view].scroll_line = 3;
    app.scroll_view("half_down", 1);
    assert!(app.state.views[view].scroll_line > 3);
    assert_visible(&app);
    let after = app.state.buffers[buffer].text.line_to_byte(4).unwrap();
    app.state.views[view].cursor = after;
    app.state.views[view].scroll_line = 4;
    app.scroll_view("half_up", 1);
    assert!(app.state.views[view].scroll_line < 4);
    assert_visible(&app);
}

#[test]
pub(in crate::app) fn table_mouse_hits_follow_wrapped_cells_even_when_wrap_is_disabled() {
    use crate::language::syntax::MarkdownMark;
    let text = "\n| a | long text here and more |\n|---|---|\n| b | c |\n\nlast";
    for wrap in [false, true] {
        let (_app_dirs, mut app) = app();
        app.window_session.observe_size((20, 14));
        {
            let mut fixture = app.configuration.fixture_snapshot();
            fixture.settings.editor.wrap = wrap;
            fixture.settings.editor.line_numbers = crate::config::LineNumbers::Off;
            fixture.settings.editor.git_signs.enabled = false;
            fixture.settings.ui.status_line.enabled = false;
            app.configuration.install_fixture(fixture);
        }
        let (buffer, view) = app
            .state
            .open_text(TextBuffer::from_text(text), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.reconcile_editor_windows();
        let revision = app.state.buffers[buffer].text.revision();
        let mark = |start, end, mark| crate::ui::UiMarkdownMark {
            buffer,
            revision,
            range: ByteRange::new(ByteOffset(start), ByteOffset(end)).unwrap(),
            mark,
        };
        app.ui.markdown_marks = text
            .match_indices('|')
            .map(|(at, _)| mark(at, at + 1, MarkdownMark::TablePipe))
            .collect();
        app.ui.markdown_marks.push(mark(
            1,
            text.find("\n\nlast").unwrap() + 1,
            MarkdownMark::Table,
        ));
        app.ui.markdown_marks.sort_by_key(|mark| mark.range.start);
        let viewport = crate::ui::active_editor_pane_viewport(&app.state, &app.ui).unwrap();
        for (row, word) in [(1, "long"), (2, "here"), (3, "more")] {
            app.state.views[view].cursor = ByteOffset(text.find("last").unwrap());
            app.state.views[view].scroll_line = 0;
            app.state.views[view].scroll_wrap_row = 0;
            app.input.observe_mouse(viewport.x + 6, viewport.y + row);
            app.apply_mouse_cursor(false);
            assert_eq!(
                app.state.views[view].cursor,
                ByteOffset(text.find(word).unwrap()),
                "wrap {wrap}, row {row}"
            );
        }
    }
}

#[test]
pub(in crate::app) fn mouse_hits_use_painted_inlay_replacement_widths() {
    for wrap in [false, true] {
        for ambiguous in [
            crate::config::AmbiguousWidth::Narrow,
            crate::config::AmbiguousWidth::Wide,
        ] {
            let (_app_dirs, mut app) = app();
            app.window_session.observe_size((40, 12));
            let mut config = app.configuration.fixture_snapshot();
            config.settings.editor.wrap = wrap;
            config.settings.editor.ambiguous_width = ambiguous;
            config.settings.editor.line_numbers = crate::config::LineNumbers::Off;
            config.settings.editor.git_signs.enabled = false;
            app.configuration.install_fixture(config);
            let (buffer, view) = app
                .state
                .open_text(TextBuffer::from_text("xyz"), None)
                .unwrap();
            app.set_dashboard_visibility(false);
            app.reconcile_editor_windows();
            let label = "a\u{200b}\tb";
            app.ui.lsp_inlay_hints.push(crate::ui::UiInlayHint {
                buffer,
                revision: app.state.buffers[buffer].text.revision(),
                position: ByteOffset(0),
                label: label.into(),
            });
            let cells =
                u16::try_from(crate::ui::geometry::cell_text_width(label, ambiguous)).unwrap();
            let viewport = crate::ui::active_editor_pane_viewport(&app.state, &app.ui).unwrap();
            for column in 0..=cells + 1 {
                app.input.observe_mouse(viewport.x + column, viewport.y);
                app.apply_mouse_cursor(false);
                assert_eq!(
                    app.state.views[view].cursor,
                    ByteOffset(usize::from(column.saturating_sub(cells)))
                );
            }
        }
    }
}

use crate::app::App;
use crate::app::persistence::capture_persisted_layout;
use crate::app::persistence::restore::build_restored_layout;
use crate::editor::input::KeyEvent;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::TextBuffer;
use crate::ui::DocumentationPopupKind;
use crate::ui::ExplorerUiState;
use crate::ui::UiFold;
use crate::ui::VisualRowAnchor;
use crate::ui::VisualRowModel;
use crate::ui::active_pane_viewport;
use crate::ui::explorer_panel_width;
use crate::ui::render;
use crate::ui::snap_display_column;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::workspace::DiagnosticSeverity;
