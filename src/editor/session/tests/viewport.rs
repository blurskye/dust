use super::*;
use crate::editor::session::dispatch::FULL_PAGE_PERCENT;
use crate::ui::render::overlays::DOCUMENTATION_SCROLL_STEP;

#[test]
fn adversarial_grapheme_rows_make_render_mouse_and_reversed_pages_agree() {
    let mut app = app();
    app.window_session.observe_size((3, 7));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = true;
        fixture.settings.editor.line_numbers = crate::config::LineNumbers::Off;
        fixture.settings.editor.viewport.vertical_margin = 0;
        fixture.settings.ui.status_line.enabled = false;
        app.configuration.install_fixture(fixture);
    }
    let (buffer, view) = app.state.open_text(
        TextBuffer::from_text("aa界界\naa\t\t\ne\u{301}界x\ntail"),
        None,
    );
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

    for wrap_row in 0..3 {
        for cell in 0..3 {
            app.state.views[view].scroll_line = 0;
            app.state.views[view].scroll_wrap_row = 0;
            let expected = {
                let model = VisualRowModel::new(
                    &app.state.buffers[buffer].text,
                    view,
                    &app.ui,
                    app.configuration.settings(),
                    viewport.width,
                    true,
                );
                model.byte_at_cell(VisualRowAnchor { line: 0, wrap_row }, cell)
            };
            app.input.observe_mouse(
                viewport.x.saturating_add(u16::try_from(cell).unwrap()),
                viewport.y.saturating_add(u16::try_from(wrap_row).unwrap()),
            );
            app.apply_mouse_cursor(false);
            assert_eq!(
                app.state.views[view].cursor,
                ByteOffset(expected),
                "mouse row {wrap_row} cell {cell}"
            );
        }
    }

    app.state.views[view].cursor = ByteOffset(0);
    app.state.views[view].scroll_line = 0;
    app.state.views[view].scroll_wrap_row = 0;
    let body = usize::from(viewport.height.max(1));
    let amount = body.saturating_mul(usize::from(FULL_PAGE_PERCENT)) / 100;
    let expected_down = {
        let model = VisualRowModel::new(
            &app.state.buffers[buffer].text,
            view,
            &app.ui,
            app.configuration.settings(),
            viewport.width,
            true,
        );
        model.forward(VisualRowAnchor::default(), amount.max(1))
    };
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
fn wrapped_visual_rows_drive_margin_paging_resize_folds_and_mouse_consistently() {
    let mut app = app();
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
    let (_, view) = app.state.open_text(TextBuffer::from_text(&text), None);
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
        .split_active(crate::editor::Axis::Horizontal, 500)
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
    let reconciliation_count = app.window_session.editor_ids().reconciliation_count();
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
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        reconciliation_count
    );
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
fn vertical_margin_keeps_cursor_context_above_and_below() {
    let mut app = app();
    app.window_session
        .observe_size((app.window_session.width(), 10));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.viewport.vertical_margin = 2;
        app.configuration.install_fixture(fixture);
    }
    let text = (0..30).map(|n| format!("line {n}\n")).collect::<String>();
    let (_, view) = app.state.open_text(TextBuffer::from_text(&text), None);
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
fn exact_pane_horizontal_follow_is_display_safe_and_view_owned() {
    let mut app = app();
    app.window_session.observe_size((18, 8));
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = false;
        app.configuration.install_fixture(fixture);
    }
    let text = "a\t界e\u{301}bcdefghijklmnopqrstuvwxyz";
    let (_, view) = app.state.open_text(TextBuffer::from_text(text), None);
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
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("split view");
    assert_eq!(app.state.views[sibling].scroll_column, shifted);
    let first_pane = app
        .state
        .panes
        .iter()
        .find_map(|(pane, state)| (state.view == view).then_some(pane))
        .expect("first pane");
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
fn cursor_follow_skips_an_unrenderable_tab_in_a_narrow_pane() {
    let mut app = app();
    let (_, view) = app.state.open_text(TextBuffer::from_text("a\tX"), None);
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
        .split_active(crate::editor::Axis::Horizontal, 100)
        .expect("minimum-ratio split");
    app.state.focus_pane(-1, false).expect("zero-width leaf");
    let zero_view = app.state.active_view.expect("zero-width view");
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
fn session_layout_capture_and_restore_preserve_each_view_scroll_column() {
    let path = PathBuf::from("/workspace/main.rs");
    let text = TextBuffer::from_text("abcdefghijklmnopqrstuvwxyz");
    let mut source = crate::editor::EditorState::new(PathBuf::from("/workspace"));
    let (_, first) = source.open_text(text.clone(), Some(path.clone()));
    source.views[first].scroll_column = 4;
    let second = source
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("split source view");
    source.views[second].scroll_column = 17;
    let source_tab = source.active_tab.expect("source tab");
    let persisted = capture_persisted_layout(&source, &source.tabs[source_tab].root)
        .expect("capture clean layout");

    let mut restored = crate::editor::EditorState::new(PathBuf::from("/workspace"));
    let loaded = HashMap::from([(path.clone(), (path.clone(), text))]);
    let (_, panes) = build_restored_layout(&mut restored, &persisted, &loaded)
        .expect("restore persisted layout");
    let mut columns = panes
        .iter()
        .map(|(_, pane)| restored.views[restored.panes[*pane].view].scroll_column)
        .collect::<Vec<_>>();
    columns.sort_unstable();
    assert_eq!(columns, vec![4, 17]);
}

#[test]
fn session_capture_keeps_project_panes_after_external_navigation() {
    let mut state = crate::editor::EditorState::new("/workspace".into());
    let path = PathBuf::from("/workspace/main.rs");
    state.open_text(TextBuffer::from_text("project"), Some(path.clone()));
    state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .unwrap();
    let (external, _) = state.open_text(
        TextBuffer::from_text("library"),
        Some("/library/lib.rs".into()),
    );
    state.buffers[external].read_only = true;
    let tab = state.active_tab.unwrap();
    let layout = capture_persisted_layout(&state, &state.tabs[tab].root).unwrap();
    assert!(
        matches!(layout, crate::persistence::PersistedLayout::Pane { pane } if pane.path == path)
    );
}

#[test]
fn horizontal_follow_reconciles_gutter_explorer_resize_and_nested_panes() {
    let mut app = app();
    let text = format!("{}\nx", "0123456789".repeat(8));
    let (_, view) = app.state.open_text(TextBuffer::from_text(&text), None);
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
        .split_active(crate::editor::Axis::Horizontal, 300)
        .expect("first split");
    app.state
        .split_active(crate::editor::Axis::Horizontal, 400)
        .expect("nested split");
    let focused = app.state.active_view.expect("focused nested view");
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
fn documentation_keys_scroll_clamp_and_close_the_popup_context() {
    let mut app = app();
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
fn dragging_the_border_between_two_windows_resizes_them() {
    let mut app = app();
    app.window_session.observe_size((100, 30));
    app.state.open_text(TextBuffer::from_text("a\n"), None);
    app.set_dashboard_visibility(false);
    app.apply_layout_action(crate::editor::LayoutAction::Split(
        crate::editor::Axis::Horizontal,
    ));
    app.reconcile_editor_windows();
    let border = |app: &InteractiveApplication| {
        app.compositor_projection()
            .expect("projection")
            .borders
            .iter()
            .find(|border| border.axis == crate::ui::window::TileAxis::Horizontal)
            .copied()
            .expect("side by side border")
    };
    let before = border(&app);
    let mouse = |app: &mut InteractiveApplication, code, column| {
        app.handle_event(crate::ui::UiEvent::Mouse {
            key: KeyEvent::mouse(code, crate::editor::input::Modifiers::default()),
            column,
            row: before.y + 2,
        });
    };
    use crate::editor::input::KeyCode::{MouseLeftDown, MouseLeftDrag, MouseLeftUp};
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
fn the_agent_panel_opens_narrow_and_ctrl_arrows_resize_it() {
    let mut app = app();
    app.window_session.observe_size((160, 45));
    app.state.open_text(TextBuffer::from_text("a\n"), None);
    app.set_dashboard_visibility(false);
    for key in [' ', 'a', 't'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.reconcile_editor_windows();
    let border = |app: &InteractiveApplication| {
        let projection = app.compositor_projection().expect("projection");
        let borders = projection
            .borders
            .iter()
            .filter(|border| border.axis == crate::ui::window::TileAxis::Horizontal)
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
                app.state.tabs[app.state.active_tab.unwrap()].root
            ),
        )
    };
    let (opened, layout) = border(&app);
    assert_eq!(opened.len(), 1, "{layout}");
    assert!(
        (100..=106).contains(&opened[0]),
        "the panel opens at about 35%: {layout}"
    );
    app.handle_configured_key("ctrl-left".parse::<KeyEvent>().expect("ctrl-left"));
    // As in LazyVim, ctrl-left narrows the focused window and ctrl-right widens it.
    let (narrowed, layout) = border(&app);
    assert!(
        narrowed[0] > opened[0],
        "ctrl-left narrows the panel: {opened:?} -> {layout}"
    );
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.handle_configured_key("ctrl-right".parse::<KeyEvent>().expect("ctrl-right"));
    let (widened, layout) = border(&app);
    assert!(
        widened[0] < narrowed[0],
        "ctrl-right widens it in Normal mode: {narrowed:?} -> {layout}"
    );
}

#[test]
fn half_page_scroll_moves_a_cursor_already_inside_the_new_viewport() {
    let mut app = app();
    app.window_session.observe_size((80, 44));
    let text = (0..200).map(|_| "line\n").collect::<String>();
    let (buffer, view) = app.state.open_text(TextBuffer::from_text(&text), None);
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
fn horizontal_cursor_following_counts_inlay_and_substitute_widths() {
    let mut app = app();
    app.window_session.observe_size((40, 12));
    let mut config = app.configuration.fixture_snapshot();
    config.settings.editor.wrap = false;
    config.settings.editor.line_numbers = crate::config::LineNumbers::Off;
    app.configuration.install_fixture(config);
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text(&"x".repeat(60)), None);
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
fn operation_bar_visibility_reprojects_the_editor_footer_inset() {
    let mut app = app();
    app.window_session.observe_size((80, 24));
    app.state
        .open_text(TextBuffer::from_text(&"line\n".repeat(60)), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let before = app.compositor_projection().unwrap().workspace_bounds.height;
    app.ui.operation_bar = Some(crate::ui::OperationBarState {
        operation: "merge".into(),
        phase: "conflicts".into(),
        blockers: Vec::new(),
        actions: Vec::new(),
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
