use crate::app::tests::*;

use crate::app::workspace::project_tests::wait_workspace_search;


#[test]
pub(in crate::app) fn search_moves_forward_wraps_and_repeats() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("one two one"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.ui.search_query = Some("one".into());
    app.perform_search(true, 1);
    assert_eq!(
        app.state.views[view].cursor,
        crate::editor::text::ByteOffset(8)
    );
    app.perform_search(true, 1);
    assert_eq!(
        app.state.views[view].cursor,
        crate::editor::text::ByteOffset(0)
    );
    app.perform_search(false, 1);
    assert_eq!(
        app.state.views[view].cursor,
        crate::editor::text::ByteOffset(8)
    );
}


#[test]
pub(in crate::app) fn help_filters_do_not_restart_the_suspended_live_grep() {
    let (_app_dirs, mut app) = app();
    fs::write(app.state.workspace_root.join("match.txt"), "needle\n").unwrap();
    app.invoke_mapped_command("workspace.search.prompt");
    app.picker_insert("needle");
    wait_workspace_search(&mut app);
    assert!(app.search.live());
    let sequence = app.search.sequence();
    app.open_contextual_help();
    assert!(!app.search.live());
    app.picker_insert("help");
    app.picker_backspace();
    assert!(app.search.help_is_open());
    assert_eq!(app.search.query(), "needle");
    assert_eq!(app.search.sequence(), sequence);
    assert_eq!(app.search.matches().len(), 1);
    app.close_picker();
    assert!(app.search.live());
    assert_eq!(app.ui.picker.as_ref().unwrap().query, "needle");
    app.accept_picker();
    wait_vfs(&mut app);
    assert_eq!(
        app.state.views[app.state.active_view.unwrap()].cursor,
        ByteOffset(0)
    );
}


#[test]
pub(in crate::app) fn grep_rows_draw_the_icon_location_and_highlighted_text_as_lazyvim_does() {
    let (_app_dirs, mut app) = app();
    fs::write(
        app.state.workspace_root.join("main.rs"),
        "\n    let needle = 1;\n",
    )
    .unwrap();
    app.invoke_mapped_command("workspace.search.prompt");
    app.picker_insert("needle");
    wait_workspace_search(&mut app);
    let picker = app.search.picker_projection().unwrap();
    assert_eq!(picker.surface, crate::ui::PickerSurfaceKind::Grep);
    let cells = &picker.rows[0].cells;
    let icon = crate::files::file_icon("main.rs").unwrap().glyph;
    assert_eq!(cells[0].text, format!("{icon} "));
    let texts: Vec<_> = cells[1..6].iter().map(|cell| cell.text.as_str()).collect();
    assert_eq!(texts, ["main.rs", ":", "2", ":", "9"]);
    assert_eq!(cells[3].style, crate::ui::Style::Accent);
    assert_eq!(cells[5].style, crate::ui::Style::Muted);
    let keyword = cells.iter().find(|cell| cell.text == "let").unwrap();
    assert!(matches!(keyword.style, crate::ui::Style::Syntax(_)));
}
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use crate::ui::PickerSurfaceKind;
use std::fs;

