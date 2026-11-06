//! Tests for markdown layout and drawing.

use crate::editor::EditorState;
use crate::ui::render::editor::render_editor_view;
use crate::ui::render::markdown::*;
use std::ops::Range;
use std::path::PathBuf;

fn fixture(source: &str, spans: Vec<(Range<usize>, MarkdownMark)>) -> (EditorState, UiState) {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, _) = state
        .open_text(TextBuffer::from_text(source), None)
        .unwrap();
    let mut ui = UiState {
        markdown_marks: spans
            .into_iter()
            .map(|(range, mark)| UiMarkdownMark {
                buffer,
                revision: state.buffers[buffer].text.revision(),
                range: ByteRange::new(ByteOffset(range.start), ByteOffset(range.end)).unwrap(),
                mark,
            })
            .collect(),
        ..UiState::default()
    };
    ui.markdown_marks.sort_by_key(|mark| mark.range.start);
    (state, ui)
}

fn table_spans(source: &str) -> Vec<(Range<usize>, MarkdownMark)> {
    let row = source.find('\n').unwrap() + 1;
    let start = row + source[row..].len() - source[row..].trim_start_matches(['>', ' ']).len();
    let end = source.len();
    let mut spans = vec![(start..end, MarkdownMark::Table)];
    spans.extend(
        source
            .match_indices('|')
            .map(|(at, _)| (at..at + 1, MarkdownMark::TablePipe)),
    );
    spans.extend(
        source
            .match_indices('>')
            .map(|(at, _)| (at..at + 1, MarkdownMark::Quote { callout: None })),
    );
    spans
}

fn markdown<'a>(
    state: &'a EditorState,
    ui: &'a UiState,
    editor: &'a EditorConfig,
    width: usize,
) -> MarkdownMarks<'a> {
    let view = &state.views[state.active_view().unwrap()];
    MarkdownMarks::new(ui, view, &state.buffers[view.buffer].text, editor, width).unwrap()
}

#[test]
fn inline_code_and_hidden_link_suffixes_continue_across_lines() {
    let source = "x\n`one\ntwo` [label](url\ncontinued)\nlast";
    let code = source.find('`').unwrap();
    let code_end = source.find("` [").unwrap() + 1;
    let suffix = source.find("](url").unwrap();
    let suffix_end = source.find(")\n").unwrap() + 1;
    let (state, ui) = fixture(
        source,
        vec![
            (code..code_end, MarkdownMark::InlineCode),
            (code..code + 1, MarkdownMark::Hide),
            (code_end - 1..code_end, MarkdownMark::Hide),
            (suffix..suffix_end, MarkdownMark::Hide),
        ],
    );
    let editor = crate::config::load_bundled_config().config.editor;
    let marks = markdown(&state, &ui, &editor, 80);
    let second = marks.line(2).unwrap();
    assert_eq!(
        second.restyle(ByteOffset(source.find("two").unwrap()), Style::Default),
        Style::MarkdownCode(None)
    );
    assert!(matches!(
        second.at(ByteOffset(code_end - 1)),
        Some(Drawn::Hidden)
    ));
    let third = marks.line(3).unwrap();
    assert!(matches!(
        third.at(ByteOffset(source.find("continued").unwrap())),
        Some(Drawn::Hidden)
    ));
    assert!(
        marks
            .line(4)
            .unwrap()
            .at(ByteOffset(source.find("last").unwrap()))
            .is_none()
    );
}

#[test]
fn tables_own_borders_next_to_a_blank_cursor_line() {
    let source = "\n| a | b |\n|---|---|\n| c | d |\n\nlast";
    let end = source.find("\n\nlast").unwrap() + 1;
    let mut spans = table_spans(source);
    spans[0].0.end = end;
    let (mut state, ui) = fixture(source, spans);
    let editor = crate::config::load_bundled_config().config.editor;
    let marks = markdown(&state, &ui, &editor, 80);
    assert!(marks.line(0).is_none());
    assert!(marks.line(1).unwrap().table.unwrap().frames[0].starts_with('┌'));
    let view = state.active_view().unwrap();
    state.views[view].cursor = ByteOffset(end);
    let marks = markdown(&state, &ui, &editor, 80);
    assert!(marks.line(4).is_none());
    assert!(
        marks
            .line(3)
            .unwrap()
            .table
            .unwrap()
            .frames
            .last()
            .unwrap()
            .starts_with('└')
    );
}

#[test]
fn table_geometry_uses_display_cells_for_wide_borders_and_alignment() {
    let source = "x\n| a | b |\n|:--|--:|\n| c | d |";
    let (state, ui) = fixture(source, table_spans(source));
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.git_signs.enabled = false;
    config.ui.status_line.enabled = false;
    for ambiguous in [AmbiguousWidth::Narrow, AmbiguousWidth::Wide] {
        config.editor.ambiguous_width = ambiguous;
        config.editor.markdown.glyphs.table[9] = "界".into();
        config.editor.markdown.glyphs.table[10] = "界".into();
        let marks = markdown(&state, &ui, &config.editor, 12);
        let table = &marks.tables[0];
        assert_eq!(table.borders, [2; 3]);
        for line in 1..=3 {
            let row = marks.line(line).unwrap().table.unwrap();
            assert!(
                row.frames
                    .iter()
                    .all(|frame| text_width(frame, ambiguous) == 12)
            );
        }
        let row = marks.line(1).unwrap().table.unwrap();
        let a = row.place(ByteOffset(source.find('a').unwrap())).unwrap();
        let b = row.place(ByteOffset(source.find('b').unwrap())).unwrap();
        assert_eq!((a.1, b.1), (3, 8));
        let frame = render_editor_view(&state, state.active_view().unwrap(), &ui, &config, 12, 8);
        assert_eq!(frame.grid.get(3, 2).unwrap().symbol, "a");
        assert_eq!(frame.grid.get(8, 2).unwrap().symbol, "b");
        assert!(markdown(&state, &ui, &config.editor, 11).tables.is_empty());
    }
}

#[test]
fn callout_lookup_does_not_slice_through_trailing_unicode() {
    let source = format!("x\n> [!NOTE] {}é", "a".repeat(23));
    let at = source.find("[!").unwrap();
    let (state, ui) = fixture(
        &source,
        vec![
            (2..3, MarkdownMark::Quote { callout: Some(at) }),
            (at..at + 7, MarkdownMark::Callout),
        ],
    );
    let editor = crate::config::load_bundled_config().config.editor;
    let marks = markdown(&state, &ui, &editor, 80);
    let (title, style) = marks.callout(at).unwrap();
    assert_eq!(title, editor.markdown.glyphs.callouts["note"].text);
    assert!(
        matches!(marks.line(1).unwrap().at(ByteOffset(at)), Some(Drawn::Glyph(shown, shown_style)) if shown == title && shown_style == style)
    );
}

#[test]
fn table_can_end_at_a_multibyte_character_without_a_final_pipe_or_newline() {
    let source = "x\n| a | b |\n|---|---|\n| c | é";
    let (state, ui) = fixture(source, table_spans(source));
    let editor = crate::config::load_bundled_config().config.editor;
    let marks = markdown(&state, &ui, &editor, 80);
    assert_eq!(marks.tables.len(), 1);
    assert_eq!(marks.tables[0].last_line, 3);
    let row = marks.line(3).unwrap().table.unwrap();
    assert!(row.place(ByteOffset(source.find('é').unwrap())).is_some());
}

#[test]
fn quoted_tables_do_not_include_quote_prefixes_as_cells() {
    for source in [
        "x\n> | a | b |\n> |---|---|\n> | c | d |",
        "x\n> a | b\n> ---|---\n> c | d",
        "x\n> > | a | b |\n> > |---|---|\n> > | c | d |",
    ] {
        let (state, ui) = fixture(source, table_spans(source));
        let editor = crate::config::load_bundled_config().config.editor;
        let marks = markdown(&state, &ui, &editor, 80);
        let table = &marks.tables[0];
        assert_eq!(table.widths, [1, 1]);
        assert!(table.rows.iter().all(|cells| cells.len() == 2));
    }
}

#[test]
fn tables_that_cannot_fit_their_minimum_width_show_source_and_scroll() {
    let source =
        "x\n| a | b | c | d | e | f |\n|---|---|---|---|---|---|\n| g | h | i | j | k | l |";
    let (mut state, ui) = fixture(source, table_spans(source));
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.git_signs.enabled = false;
    config.editor.wrap = false;
    config.ui.status_line.enabled = false;
    let marks = markdown(&state, &ui, &config.editor, 20);
    assert!(marks.tables.is_empty());
    assert!(marks.line(1).unwrap().table.is_none());
    let frame = render_editor_view(&state, state.active_view().unwrap(), &ui, &config, 20, 5);
    assert_eq!(
        frame.grid.row_text(1),
        &source.lines().nth(1).unwrap()[..20]
    );
    let view = state.active_view().unwrap();
    state.views[view].scroll_column = 8;
    let frame = render_editor_view(&state, state.active_view().unwrap(), &ui, &config, 20, 5);
    assert!(frame.grid.row_text(1).starts_with("| c | d | e | f |"));
}

#[test]
fn narrowing_handles_a_very_wide_column_without_decrementing_each_cell() {
    let mut widths = vec![1; 1_000];
    widths[999] = 1_000_000_000;
    narrow(&mut widths, 2_000);
    assert!(widths[..999].iter().all(|width| *width == 1));
    assert_eq!(widths[999], 1_001);
    let mut widths = [2, 10, 20];
    narrow(&mut widths, 13);
    assert_eq!(widths, [2, 6, 5]);
}

#[test]
fn link_heavy_cells_keep_hidden_markup_and_icons_while_advancing_marks() {
    let source = "[x](u) ".repeat(10_000);
    let spans = (0..10_000)
        .flat_map(|index| {
            let at = index * 7;
            [
                (at..at + 1, MarkdownMark::Link { url: None }),
                (at + 2..at + 6, MarkdownMark::Hide),
            ]
        })
        .collect();
    let (state, ui) = fixture(&source, spans);
    let view = &state.views[state.active_view().unwrap()];
    let text = &state.buffers[view.buffer].text;
    let marks = ui.markdown_marks.iter().collect::<Vec<_>>();
    let glyphs = MarkdownGlyphs {
        link: "L ".into(),
        ..MarkdownGlyphs::default()
    };
    let graphemes = cell_graphemes(
        text,
        &marks,
        ByteRange::new(ByteOffset(0), ByteOffset(source.len())).unwrap(),
        &glyphs,
        AmbiguousWidth::Narrow,
    );
    assert_eq!(graphemes.len(), source.len());
    for chunk in graphemes.chunks_exact(7) {
        assert_eq!(
            chunk.iter().map(|(_, width, _)| *width).collect::<Vec<_>>(),
            [2, 1, 0, 0, 0, 0, 1]
        );
        assert!(chunk[6].2);
    }
}
