use crate::app::tests::*;

#[test]
pub(in crate::app) fn file_preview_retries_after_capacity_returns_without_picker_input() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("preview.txt");
    fs::write(&path, "ready\n").unwrap();
    let roots = vec![app.state.workspace_root.clone()];
    let mut requests = Vec::new();
    loop {
        match app
            .documents
            .read_scoped_bounded(path.clone(), roots.clone(), 64)
        {
            Ok(id) => requests.push(id),
            Err(crate::files::VfsError::QueueFull) => break,
            Err(error) => panic!("unexpected admission error: {error}"),
        }
    }
    app.install_picker(
        "Preview".into(),
        vec![(
            crate::workspace::PickerItem {
                id: crate::workspace::PickerItemId("preview".into()),
                label: "preview.txt".into(),
                detail: String::new(),
                source: String::new(),
                badge: String::new(),
                action: crate::workspace::PickerAction::None,
                kind: crate::workspace::PickerItemKind::Entry,
            },
            PickerPayload::MatcherSource(MatcherSource {
                path,
                line: 0,
                column: 0,
                encoding: crate::editor::text::PositionEncoding::Utf8,
            }),
        )],
    );
    assert!(app.search.preview_needs_retry());
    for id in requests {
        app.documents.cancel_request(id);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.search.preview_needs_retry() || app.documents.has_work() {
        app.process_vfs_results();
        assert!(Instant::now() < deadline, "preview retry timed out");
        thread::yield_now();
    }
    let text = app
        .ui
        .picker
        .as_ref()
        .unwrap()
        .preview
        .as_ref()
        .unwrap()
        .as_text()
        .unwrap();
    assert_eq!(text.lines(), ["ready"]);
    assert!(!app.search.preview_needs_retry());
}

#[test]
pub(in crate::app) fn closing_help_returns_the_palette_with_its_query_and_selection_intact() {
    let (_app_dirs, mut app) = app();
    app.open_builtin_picker(crate::app::commands::PickerKind::Commands);
    app.picker_insert("overlay.help.");
    let palette = app.search.picker_projection().expect("palette");
    let query_before = palette.query.clone();
    let selected_before = palette.selected.expect("a row is selected");
    let label_before = palette.rows[selected_before].label.clone();
    let rows_before = palette.rows.len();
    assert!(!query_before.is_empty(), "the palette was filtered");

    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.reference", InvocationSource::Test)
            .expect("command id"),
    );
    let reference = app.search.picker_projection().expect("reference page");
    assert_eq!(
        reference.query, "",
        "the help page is its own surface, not a filtered palette"
    );

    app.search.close_picker();

    // MEASURED: closing help restores the surface help was opened over, with its query text,
    // its row count and its selected row all unchanged.
    let restored = app
        .search
        .picker_projection()
        .expect("closing help returned to the palette");
    assert_eq!(
        restored.query, query_before,
        "the palette's query text survived help"
    );
    assert_eq!(
        restored.rows.len(),
        rows_before,
        "the palette's rows survived help"
    );
    assert_eq!(
        restored.selected,
        Some(selected_before),
        "the palette's selected row survived help"
    );
    assert_eq!(
        restored.rows[selected_before].label, label_before,
        "the row selected before help is the row selected after it"
    );

    // A help page opened with no picker underneath restores nothing: the editor-context case.
    app.search.close_picker();
    assert!(app.search.picker_projection().is_none());
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.contextual", InvocationSource::Test)
            .expect("command id"),
    );
    assert!(
        app.search.picker_projection().is_some(),
        "help opened alone"
    );
    app.search.close_picker();
    assert!(
        app.search.picker_projection().is_none(),
        "closing help that had nothing underneath leaves no surface rather than inventing one"
    );
}

pub(in crate::app) fn palette_row_for(app: &mut App, query: &str) -> String {
    app.open_builtin_picker(crate::app::commands::PickerKind::Commands);
    app.picker_insert(query);
    let palette = app.search.picker_projection().expect("palette");
    let selected = palette
        .selected
        .unwrap_or_else(|| panic!("no row for `{query}`"));
    palette.rows[selected].label.clone()
}

#[test]
pub(in crate::app) fn palette_finds_commands_by_the_words_people_type() {
    let (_app_dirs, mut app) = app();
    for (query, expected) in [
        ("theme", "Select Color Theme"),
        ("color", "Select Color Theme"),
        ("colorscheme", "Select Color Theme"),
        ("mason", "Tool Manager"),
        ("quit dust", "Quit Dust"),
        ("close buffer", "Close buffer"),
        ("rename symbol", "Rename symbol"),
        ("open settings", "Open Settings"),
        ("undo", "Undo"),
    ] {
        let row = palette_row_for(&mut app, query);
        assert!(row.contains(expected), "`{query}` picked `{row}`");
    }
}

#[test]
pub(in crate::app) fn palette_runs_flagged_commands_with_the_flag_off() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("unsaved\n"), None)
        .unwrap();
    app.state.buffers[buffer].dirty = true;
    palette_row_for(&mut app, "quit dust");
    app.accept_picker();
    assert!(!app.should_quit, "ordinary quit refuses unsaved changes");
    assert!(app.ui.notification_error);
}

#[test]
pub(in crate::app) fn palette_rows_that_need_text_open_a_command_line_that_runs_them() {
    let (_app_dirs, mut app) = app();
    palette_row_for(&mut app, "file.open");
    app.accept_picker();
    assert_eq!(app.input.prompt().expect("prompt").input, "e ");
    let path = app.state.workspace_root.join("palette-open.txt");
    fs::write(&path, "opened from the palette\n").unwrap();
    app.prompt_insert("palette-open.txt");
    app.submit_prompt();
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("opened buffer");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "opened from the palette\n"
    );

    let settings = app.configuration.settings();
    for (id, initial) in &settings.command_palette.prompts {
        let invocation = crate::editor::input::parse_ex(
            &format!("{initial}value"),
            &settings.input.command_aliases,
        )
        .unwrap_or_else(|error| panic!("`{id}` prompt `{initial}`: {error}"));
        assert_eq!(invocation.id.as_str(), id, "prompt `{initial}`");
        app.commands
            .validate_invocation_shape(&invocation)
            .unwrap_or_else(|error| panic!("`{id}` prompt `{initial}`: {error}"));
    }
}

#[test]
pub(in crate::app) fn file_picker_can_filter_an_entry_after_the_display_limit() {
    let (_app_dirs, mut app) = app();
    app.start_workspace_file_picker();
    let items = (0..10_001)
        .map(|i| {
            (
                crate::workspace::PickerItem {
                    id: crate::workspace::PickerItemId(format!("file-{i}")),
                    label: format!("file-{i}"),
                    detail: String::new(),
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                    source: String::new(),
                    action: crate::workspace::PickerAction::None,
                },
                PickerPayload::Command(String::new()),
            )
        })
        .collect();
    app.install_surface_picker_state(
        PickerSurfaceKind::Generic,
        "Files".into(),
        items,
        String::new(),
        true,
        PickerOrigin::Files,
    );
    app.picker_insert("file-10000");
    assert_eq!(app.ui.picker.as_ref().unwrap().rows[0].label, "file-10000");
}
use crate::app::App;
use crate::editor::InvocationSource;
use crate::editor::text::TextBuffer;
use crate::ui::PickerSurfaceKind;
use std::fs;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use crate::app::picker::MatcherSource;
use crate::app::picker::PickerOrigin;
use crate::app::picker::PickerPayload;
