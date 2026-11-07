//! Tab tests for editor surfaces and frame composition.

use crate::editor::EditorState;
use crate::ui::render::UiState;
use std::path::PathBuf;

use crate::ui::render::composition_tests::attach_test_compositor;
use crate::ui::render::composition_tests::editor_layout;
use crate::ui::render::composition_tests::render;
use crate::ui::render::composition_tests::ui_with_editor_layout;

#[test]
fn split_tree_and_tab_line_render_real_independent_views() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let text = format!("left\n{}right", "filler\n".repeat(12));
    let (_, first_view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text(&text),
            Some(PathBuf::from("/workspace/main.rs")),
        )
        .unwrap();
    let first_tab = state.active_tab().expect("first tab");
    state.new_tab().expect("second tab");
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("other tab"),
            Some(PathBuf::from("/workspace/other.rs")),
        )
        .unwrap();
    state.activate_tab(first_tab).expect("return to split tab");
    let second_view = state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("split");
    state.views[second_view].scroll_line = 13;
    state.views[second_view].cursor = crate::editor::text::ByteOffset(text.find("right").unwrap());
    assert_eq!(state.views[first_view].scroll_line, 0);
    let loaded = crate::config::load_bundled_config();
    let config = loaded.config;
    let mut ui = ui_with_editor_layout(&state);
    attach_test_compositor(&state, &mut ui, &config, 60, 8);
    let panes = crate::ui::render::pane_viewports(&state, &ui, &config, 60, 7);
    let frame = crate::ui::render::render(&state, &ui, &config, &loaded.dashboard, 60, 8);
    assert!(frame.grid.row_text(0).contains("[main.rs]"));
    assert!(frame.grid.row_text(0).contains("other.rs"));
    for (view, label) in [(first_view, "left"), (second_view, "right")] {
        let pane = panes.iter().find(|pane| pane.view == view).expect("pane");
        let (x, y) = drawn_at(&frame, label).expect("independent pane content");
        assert!(x >= pane.x && x + label.len() as u16 <= pane.x + pane.width);
        assert_eq!(y, pane.y);
    }
    assert_eq!(frame.grid.get(29, 2).expect("split border").symbol, "│");
    assert!(frame.cursor.is_some_and(|(x, y, _)| x > 29 && y == 1));
}

#[test]
fn workspace_tab_line_keeps_the_active_label_visible() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("first"),
            Some(PathBuf::from("/workspace/an-earlier-long-filename.rs")),
        )
        .unwrap();
    let first = state.active_tab().unwrap();
    let second = state.new_tab().unwrap();
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("second"),
            Some(PathBuf::from("/workspace/界界.rs")),
        )
        .unwrap();
    let third = state.new_tab().unwrap();
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("third"),
            Some(PathBuf::from("/workspace/last.rs")),
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    for (tab, label) in [
        (first, "an-earlier"),
        (second, "界界.rs"),
        (third, "last.rs"),
    ] {
        state.activate_tab(tab).unwrap();
        let ui = ui_with_editor_layout(&state);
        let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 16, 6);
        let line = frame.grid.row_text(0);
        assert!(line.starts_with('['), "{line:?}");
        assert!(line.contains(label), "{line:?}");
        assert_eq!(
            frame.grid.get(0, 0).unwrap().style,
            crate::ui::Style::StatusAccent
        );
    }
}

#[test]
fn workspace_tab_line_measures_sanitized_labels() {
    let loaded = crate::config::load_bundled_config();
    for ambiguous in [
        crate::config::AmbiguousWidth::Narrow,
        crate::config::AmbiguousWidth::Wide,
    ] {
        let mut config = loaded.config.clone();
        config.editor.ambiguous_width = ambiguous;
        for title in ["\n".repeat(20), "\t".repeat(20), "\u{200b}".repeat(20)] {
            let mut state = EditorState::new(PathBuf::from("/workspace"));
            state
                .open_text(
                    crate::editor::text::TextBuffer::from_text("first"),
                    Some(PathBuf::from(format!("/workspace/{title}"))),
                )
                .unwrap();
            state.new_tab().unwrap();
            state
                .open_text(
                    crate::editor::text::TextBuffer::from_text("second"),
                    Some(PathBuf::from("/workspace/active.rs")),
                )
                .unwrap();
            let ui = ui_with_editor_layout(&state);
            let frame = render(&state, &ui, &config, &loaded.dashboard, 16, 6);
            assert!(frame.grid.row_text(0).starts_with("[active.rs]"));
            assert_eq!(
                frame.grid.get(0, 0).unwrap().style,
                crate::ui::Style::StatusAccent
            );
        }
    }
}

#[test]
fn non_wrapped_horizontal_projection_snaps_graphemes_and_preserves_source_tabs() {
    let text = "a\t界e\u{301}bcdef";
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text(text), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = false;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;

    // Column two is inside the source tab. The viewport snaps to the tab's
    // column-one boundary, so only its three remaining source cells render.
    state.views[view].scroll_column = 2;
    state.views[view].cursor = crate::editor::text::ByteOffset("a\t界e\u{301}".len());
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        syntax_spans: vec![crate::ui::render::UiSyntaxSpan {
            buffer,
            revision: state.buffers[buffer].text.revision(),
            range: crate::editor::text::ByteRange::new(
                crate::editor::text::ByteOffset("a\t界".len()),
                crate::editor::text::ByteOffset("a\t界e\u{301}".len()),
            )
            .expect("combining syntax range"),
            role: crate::config::Highlight::STRING,
        }],
        lsp_inlay_hints: vec![crate::ui::render::UiInlayHint {
            buffer,
            revision: state.buffers[buffer].text.revision(),
            position: crate::editor::text::ByteOffset("a\t界e\u{301}b".len()),
            label: "H".into(),
        }],
        ..UiState::default()
    };
    let frame = render(&state, &ui, &config, &loaded.dashboard, 8, 2);
    assert_eq!(frame.grid.get(0, 0).expect("tab remainder").symbol, " ");
    assert_eq!(frame.grid.get(2, 0).expect("last tab cell").symbol, " ");
    assert_eq!(frame.grid.get(3, 0).expect("wide grapheme").symbol, "界");
    assert!(
        frame
            .grid
            .get(4, 0)
            .expect("wide continuation")
            .continuation
    );
    assert_eq!(
        frame.grid.get(5, 0).expect("combining grapheme").symbol,
        "e\u{301}"
    );
    assert_eq!(
        frame.grid.get(5, 0).expect("syntax after clipping").style,
        crate::ui::Style::Syntax(crate::config::Highlight::STRING)
    );
    assert_eq!(
        frame.grid.get(8 - 1, 0).expect("projected inlay").symbol,
        "H"
    );
    assert!(frame.cursor.is_some_and(|(x, y, _)| x == 6 && y == 0));

    // Column five is inside the wide glyph and snaps to its column-four start.
    state.views[view].scroll_column = 5;
    let ui = ui_with_editor_layout(&state);
    let frame = render(&state, &ui, &config, &loaded.dashboard, 8, 2);
    assert_eq!(
        frame.grid.get(0, 0).expect("snapped wide glyph").symbol,
        "界"
    );
    assert!(
        frame
            .grid
            .get(1, 0)
            .expect("wide continuation")
            .continuation
    );
    assert!(frame.cursor.is_some_and(|(x, _, _)| x == 3));

    config.editor.ambiguous_width = crate::config::AmbiguousWidth::Narrow;
    assert_eq!(
        crate::ui::render::snap_display_column("a·b", 2, 4, config.editor.ambiguous_width),
        2
    );
    config.editor.ambiguous_width = crate::config::AmbiguousWidth::Wide;
    assert_eq!(
        crate::ui::render::snap_display_column("a·b", 2, 4, config.editor.ambiguous_width),
        1
    );
}

#[test]
fn configurable_indent_guides_follow_leading_space_and_tab_display_columns() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text("root\n    one\n        two\n\t\ttab\n"),
            Some(PathBuf::from("/workspace/file.rs")),
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 20, 5);

    assert_eq!(frame.grid.get(0, 1).unwrap().symbol, "│");
    assert_eq!(frame.grid.get(3, 1).unwrap().symbol, " ");
    assert_eq!(frame.grid.get(4, 1).unwrap().symbol, "o");
    assert_eq!(frame.grid.get(0, 2).unwrap().symbol, "│");
    assert_eq!(frame.grid.get(4, 2).unwrap().symbol, "│");
    assert_eq!(
        frame.grid.get(4, 2).unwrap().style,
        crate::ui::Style::Border
    );
    assert_eq!(frame.grid.get(8, 2).unwrap().symbol, "t");
    assert_eq!(frame.grid.get(0, 3).unwrap().symbol, "│");
    assert_eq!(frame.grid.get(4, 3).unwrap().symbol, "│");
    assert_eq!(frame.grid.get(8, 3).unwrap().symbol, "t");

    config.editor.indent_guides.enabled = false;
    let disabled = crate::ui::render::render_editor_view(&state, view, &ui, &config, 20, 5);
    assert_eq!(disabled.grid.get(0, 1).unwrap().symbol, " ");
    assert_eq!(disabled.grid.get(4, 2).unwrap().symbol, " ");
    assert_eq!(disabled.grid.get(4, 3).unwrap().symbol, " ");

    config.editor.indent_guides.enabled = true;
    config.editor.ambiguous_width = crate::config::AmbiguousWidth::Wide;
    let wide = crate::ui::render::render_editor_view(&state, view, &ui, &config, 20, 5);
    assert_eq!(wide.grid.get(4, 2).unwrap().symbol, "|");
    assert!(!wide.grid.get(4, 2).unwrap().continuation);
}

#[test]
fn immutable_git_signs_use_the_reserved_gutter_and_exact_buffer_revision() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, _) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text("added\nchanged\ndeleted\n"),
            Some(PathBuf::from("/workspace/file.rs")),
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    let revision = state.buffers[buffer].text.revision().0;
    ui.git_signs = vec![
        crate::ui::render::UiGitSign {
            buffer,
            buffer_revision: revision,
            root_revision: 1,
            status_revision: 1,
            repository_id: crate::git::GitRepositoryId(1),
            line: 0,
            kind: crate::ui::render::UiGitSignKind::Added,
            staged: false,
        },
        crate::ui::render::UiGitSign {
            buffer,
            buffer_revision: revision,
            root_revision: 1,
            status_revision: 1,
            repository_id: crate::git::GitRepositoryId(1),
            line: 1,
            kind: crate::ui::render::UiGitSignKind::Changed,
            staged: false,
        },
        crate::ui::render::UiGitSign {
            buffer,
            buffer_revision: revision,
            root_revision: 1,
            status_revision: 1,
            repository_id: crate::git::GitRepositoryId(1),
            line: 2,
            kind: crate::ui::render::UiGitSignKind::Deleted,
            staged: false,
        },
    ];
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 20, 5);
    let gutter = crate::ui::render::editor_gutter_width(
        &state.buffers[buffer].text,
        &loaded.config.editor,
        20,
    );
    let sign_column =
        crate::ui::render::visual_rows::git_sign_column(gutter, &loaded.config.editor).unwrap();
    assert_eq!(
        (
            frame.grid.get(sign_column, 0).unwrap().symbol.as_str(),
            frame.grid.get(sign_column, 0).unwrap().style,
        ),
        ("┃", crate::ui::Style::GitSignAdded)
    );
    assert_eq!(
        (
            frame.grid.get(sign_column, 1).unwrap().symbol.as_str(),
            frame.grid.get(sign_column, 1).unwrap().style,
        ),
        ("┃", crate::ui::Style::GitSignChanged)
    );
    assert_eq!(
        (
            frame.grid.get(sign_column, 2).unwrap().symbol.as_str(),
            frame.grid.get(sign_column, 2).unwrap().style,
        ),
        ("_", crate::ui::Style::GitSignDeleted)
    );
    assert_eq!(
        (
            frame.grid.get(sign_column, 3).unwrap().symbol.as_str(),
            frame.grid.get(sign_column, 3).unwrap().style,
        ),
        ("│", crate::ui::Style::Muted)
    );

    let mut wide_config = loaded.config.clone();
    wide_config.editor.ambiguous_width = crate::config::AmbiguousWidth::Wide;
    let wide = render(&state, &ui, &wide_config, &loaded.dashboard, 20, 5);
    assert_eq!(wide.grid.get(sign_column, 0).unwrap().symbol, "|");
    assert!(!wide.grid.get(sign_column, 0).unwrap().continuation);
    assert_eq!(wide.grid.get(sign_column, 3).unwrap().symbol, "|");

    ui.git_signs[0].buffer_revision = revision.saturating_add(1);
    let stale = render(&state, &ui, &loaded.config, &loaded.dashboard, 20, 5);
    assert_ne!(
        stale.grid.get(sign_column, 0).unwrap().style,
        crate::ui::Style::GitSignAdded
    );
    let tiny = render(&state, &ui, &loaded.config, &loaded.dashboard, 1, 3);
    assert_eq!(tiny.grid.width(), 1);
}

#[test]
fn line_number_gutter_uses_the_largest_open_buffer_across_tabs() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (short, short_view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text("short\n"),
            Some(PathBuf::from("/workspace/short.rs")),
        )
        .unwrap();
    let first_tab = state.active_tab().unwrap();
    state.new_tab().unwrap();
    let long_text = (0..1_000).map(|_| "long\n").collect::<String>();
    let (long, long_view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text(&long_text),
            Some(PathBuf::from("/workspace/long.rs")),
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    assert_eq!(
        crate::ui::render::visual_rows::editor_line_number_digits(&state),
        4
    );
    let short_gutter = crate::ui::render::editor_state_gutter_width(
        &state,
        &state.buffers[short].text,
        &loaded.config.editor,
        80,
    );
    let long_gutter = crate::ui::render::editor_state_gutter_width(
        &state,
        &state.buffers[long].text,
        &loaded.config.editor,
        80,
    );
    assert_eq!(short_gutter, long_gutter);
    assert_eq!(short_gutter, 7);
    state.focus_tab(-1, true).unwrap();
    assert_eq!(state.active_tab(), Some(first_tab));
    let ui = ui_with_editor_layout(&state);
    let frame =
        crate::ui::render::render_editor_view(&state, short_view, &ui, &loaded.config, 80, 4);
    let short_sign =
        crate::ui::render::visual_rows::git_sign_column(short_gutter, &loaded.config.editor)
            .unwrap();
    let long_sign =
        crate::ui::render::visual_rows::git_sign_column(long_gutter, &loaded.config.editor)
            .unwrap();
    assert_eq!(frame.grid.get(3, 0).unwrap().symbol, "1");
    assert_eq!(frame.grid.get(short_sign, 0).unwrap().symbol, "│");
    assert_eq!(
        frame.grid.get(short_sign, 0).unwrap().style,
        crate::ui::Style::Muted
    );
    assert_eq!(frame.grid.get(short_sign + 1, 0).unwrap().symbol, " ");
    assert_eq!(frame.grid.get(short_gutter, 0).unwrap().symbol, "s");
    let long_frame =
        crate::ui::render::render_editor_view(&state, long_view, &ui, &loaded.config, 80, 4);
    assert_eq!(long_frame.grid.get(long_sign, 0).unwrap().symbol, "│");
    assert_eq!(long_frame.grid.get(long_sign + 1, 0).unwrap().symbol, " ");
    assert_eq!(long_frame.grid.get(long_gutter, 0).unwrap().symbol, "l");
}
use crate::ui::render::composition_tests::drawn_at;

use crate::ui::render::overlays::BUFFER_OVERFLOW_RIGHT;
use crate::ui::render::overlays::BUFFER_SEPARATOR;

#[test]
fn content_tabs_keep_literal_placeholders_and_measure_visible_replacements() {
    let config = crate::config::load_bundled_config().config;
    let entries = [
        "a{dirty}.rs".to_owned(),
        format!("a{}.rs", "\u{200b}".repeat(10)),
        "last.rs".into(),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, title)| crate::ui::render::ContentTabEntry {
        identity: crate::ui::render::ContentTabIdentity(crate::windows::ContentId(index as u64)),
        kind: crate::ui::render::ContentTabKind::Text,
        title,
        active: index == 0,
        dirty: false,
        pinned: false,
        diagnostic: None,
        icon: "R".into(),
        closeable: false,
        segments: None,
    })
    .collect::<Vec<_>>();
    let mut grid = crate::ui::CellGrid::new(120, 1);
    crate::ui::render::overlays::render_content_tab_line(&mut grid, &entries, &config, 0);
    assert!(grid.row_text(0).contains("a{dirty}.rs"));
    let separators = (0..120)
        .filter(|x| grid.get(*x, 0).unwrap().symbol == BUFFER_SEPARATOR)
        .collect::<Vec<_>>();
    let mut start = 0;
    for (entry, separator) in entries.iter().zip(separators) {
        assert_eq!(
            usize::from(separator + 1 - start),
            crate::ui::render::content_tab_width(entry, &config)
        );
        for column in start..=separator {
            assert_eq!(
                crate::ui::render::content_tab_at_column(&entries, &config, 120, column),
                Some(entry.identity)
            );
        }
        start = separator + 1;
    }
    // The last two cells are painted over the clipped first tab by the overflow marker.
    let mut grid = crate::ui::CellGrid::new(12, 1);
    crate::ui::render::overlays::render_content_tab_line(&mut grid, &entries, &config, 0);
    assert!(grid.row_text(0).ends_with(BUFFER_OVERFLOW_RIGHT));
    for column in 10..12 {
        assert_eq!(
            crate::ui::render::content_tab_at_column(&entries, &config, 12, column),
            None
        );
    }
}

#[test]
fn content_tabs_follow_the_last_of_many_entries() {
    let config = crate::config::load_bundled_config().config;
    let entries = (0..1000)
        .map(|index| crate::ui::render::ContentTabEntry {
            identity: crate::ui::render::ContentTabIdentity(crate::windows::ContentId(index)),
            kind: crate::ui::render::ContentTabKind::Text,
            title: format!("file-{index}.rs"),
            active: index == 999,
            dirty: false,
            pinned: false,
            diagnostic: None,
            icon: "R".into(),
            closeable: false,
            segments: None,
        })
        .collect::<Vec<_>>();
    let mut grid = crate::ui::CellGrid::new(40, 1);
    crate::ui::render::overlays::render_content_tab_line(&mut grid, &entries, &config, 0);
    assert!(grid.row_text(0).contains("file-999.rs"));
    let column = (0..40)
        .rev()
        .find(|x| grid.get(*x, 0).unwrap().symbol == "9")
        .unwrap();
    assert_eq!(
        crate::ui::render::content_tab_at_column(&entries, &config, 40, column),
        Some(entries[999].identity)
    );
}
