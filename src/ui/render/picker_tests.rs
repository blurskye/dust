//! Picker tests for editor surfaces and frame composition.

use crate::editor::EditorState;
use crate::ui::render::UiState;
use std::path::PathBuf;

use crate::ui::render::composition_tests::editor_layout;
use crate::ui::render::composition_tests::render;

#[test]
fn theme_geometry_is_compact_and_bounded_without_resizing_other_pickers() {
    use crate::ui::PickerRow;
    use crate::ui::PickerSurfaceKind;
    use crate::ui::PickerUiState;
    use crate::windows::WindowRect;
    let config = crate::config::load_bundled_config().config;
    let mut picker = PickerUiState {
        preview_side_scroll: (0, 0),
        footer: None,
        input_hint: String::new(),
        surface: PickerSurfaceKind::Theme,
        title: "Dust themes".into(),
        query: String::new(),
        rows: vec![],
        selected: None,
        scroll: 0,
        total: 0,
        loading: false,
        preview: None,
    };
    for count in [0, 1, 7, 14, 46] {
        picker.rows = (0..count)
            .map(|index| PickerRow::text(index.to_string(), "Theme", String::new(), String::new()))
            .collect();
        for (width, height) in [(220, 70), (100, 30), (40, 12), (20, 8), (4, 6), (3, 5)] {
            let bounds = WindowRect {
                x: 3,
                y: 2,
                width,
                height,
            };
            picker.surface = PickerSurfaceKind::Theme;
            if let Some(rect) = crate::ui::render::picker::picker_window_geometry(&picker, bounds) {
                assert!(rect.width <= 64 && rect.height <= 12);
                assert!(rect.x >= bounds.x && rect.y >= bounds.y);
                assert!(rect.x + rect.width <= bounds.x + width);
                assert!(rect.y + rect.height <= bounds.y + height);
                let content =
                    crate::ui::render::picker::picker_content_geometry(&picker, &config, rect)
                        .unwrap();
                assert_eq!(content.outer, rect);
                assert_eq!(content.list.height, rect.height - 4);
            } else {
                assert!(width < 4 || height < 6);
            }
            picker.surface = PickerSurfaceKind::Generic;
            assert_eq!(
                crate::ui::render::picker::picker_window_geometry(&picker, bounds),
                crate::ui::render::picker_geometry(bounds)
            );
        }
    }
}

#[test]
fn picker_sources_share_a_right_column_and_do_not_overwrite_labels_or_borders() {
    use crate::ui::CellGrid;
    use crate::ui::Frame;
    use crate::ui::PickerRow;
    use crate::ui::PickerSurfaceKind;
    use crate::ui::PickerUiState;
    use crate::ui::Style;
    use crate::windows::WindowRect;
    let config = crate::config::load_bundled_config().config;
    for surface in [PickerSurfaceKind::Generic, PickerSurfaceKind::Theme] {
        for width in [64, 20, 8] {
            let rect = WindowRect {
                x: 3,
                y: 2,
                width,
                height: 8,
            };
            let picker = PickerUiState {
                preview_side_scroll: (0, 0),
                footer: None,
                input_hint: String::new(),
                surface,
                title: "Choices".into(),
                query: String::new(),
                rows: vec![
                    PickerRow::text("one", "Dust", "Current", "Builtin"),
                    PickerRow::text("two", "Long 名称 ".repeat(12), String::new(), "Builtin"),
                    PickerRow::text("three", "Other", String::new(), "插件"),
                ],
                selected: Some(1),
                scroll: 0,
                total: 0,
                loading: false,
                preview: None,
            };
            let mut frame = Frame {
                grid: CellGrid::new(72, 12),
                cursor: None,
            };
            crate::ui::render::picker::render_picker(&mut frame, &picker, &config, rect, None);
            for row in 5..8 {
                assert_eq!(frame.grid.get(rect.x + width - 1, row).unwrap().symbol, "│");
                assert_eq!(frame.grid.get(rect.x + width, row).unwrap().symbol, " ");
            }
            if width == 64 {
                let right = rect.x + width - 2;
                for (row, style) in [(5, Style::Muted), (6, Style::Selection)] {
                    assert_eq!(frame.grid.get(right - 7, row).unwrap().symbol, "B");
                    assert_eq!(frame.grid.get(right - 1, row).unwrap().symbol, "n");
                    assert_eq!(frame.grid.get(right - 7, row).unwrap().style, style);
                    assert_eq!(frame.grid.get(right - 8, row).unwrap().symbol, " ");
                }
                assert!(frame.grid.row_text(5).contains("Dust  Current"));
                assert_eq!(frame.grid.get(right - 4, 7).unwrap().symbol, "插");
                assert_eq!(frame.grid.get(right - 2, 7).unwrap().symbol, "件");
            }
        }
    }
}

#[test]
fn generic_picker_is_centered_filtered_presentation_with_configured_markers() {
    let state = EditorState::new(PathBuf::from("/tmp"));
    let ui = UiState {
        dashboard: true,
        picker: Some(crate::ui::render::PickerUiState {
            preview_side_scroll: (0, 0),
            footer: None,
            input_hint: "Insert · escape normal".into(),
            surface: crate::ui::render::PickerSurfaceKind::Generic,
            title: "Commands".into(),
            query: "health".into(),
            rows: vec![crate::ui::render::PickerRow::text(
                "health",
                "health.open",
                "Show health",
                "core",
            )],
            selected: Some(0),
            scroll: 0,
            total: 0,
            loading: false,
            preview: Some(crate::ui::render::PickerPreview::text(
                "health.rs",
                crate::ui::render::PickerTextPreview::new(
                    vec!["fn health() {}".to_string()],
                    0,
                    Some(0),
                    None,
                ),
            )),
        }),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 30);
    assert!((0..30).any(|row| frame.grid.row_text(row).contains("health.open")));
    assert!((0..30).any(|row| frame.grid.row_text(row).contains("fn health() {}")));
    assert!((0..30).any(|row| frame.grid.row_text(row).contains("1/1")));
    assert!(frame.cursor.is_some());
}

#[test]
fn picker_preview_highlighting_is_prepared_before_paint_and_replaced_with_its_text() {
    let text = crate::ui::render::PickerTextPreview::new(
        vec!["fn cached() {}".into()],
        0,
        None,
        Some(crate::language::syntax::SyntaxLanguage::Rust),
    );
    assert!(!text.spans[0].is_empty());
    let spans = text.spans.clone();
    let loaded = crate::config::load_bundled_config();
    let mut picker = diff_picker(crate::ui::diff::parse_unified_patch("", "test"))
        .picker
        .unwrap();
    picker.preview = Some(crate::ui::render::PickerPreview::text("cached.rs", text));
    let allocation = picker
        .preview
        .as_ref()
        .unwrap()
        .as_text()
        .unwrap()
        .spans
        .as_ptr();
    let rect = crate::windows::WindowRect {
        x: 0,
        y: 0,
        width: 100,
        height: 24,
    };
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(100, 24),
        cursor: None,
    };
    for query in ["", "cached"] {
        picker.query = query.into();
        crate::ui::render::picker::render_picker(&mut frame, &picker, &loaded.config, rect, None);
        assert!((0..24).any(|y| (0..100).any(|x| matches!(
            frame.grid.get(x, y).unwrap().style,
            crate::ui::Style::Syntax(_)
        ))));
        let text = picker.preview.as_ref().unwrap().as_text().unwrap();
        assert_eq!(text.spans, spans);
        assert_eq!(text.spans.as_ptr(), allocation);
    }
    let text = picker.preview.as_mut().unwrap().as_text_mut().unwrap();
    text.set_content(vec!["x".into()], None);
    assert!(text.spans.is_empty());
    crate::ui::render::picker::render_picker(&mut frame, &picker, &loaded.config, rect, None);
    assert!((0..24).any(|y| frame.grid.row_text(y).contains("1 x")));
}

#[test]
fn picker_stacks_preview_when_two_readable_columns_do_not_fit() {
    let state = EditorState::new(PathBuf::from("/tmp"));
    let long = "x".repeat(80);
    let ui = UiState {
        dashboard: true,
        picker: Some(crate::ui::render::PickerUiState {
            preview_side_scroll: (0, 0),
            footer: None,
            input_hint: "Insert · escape normal".into(),
            surface: crate::ui::render::PickerSurfaceKind::Generic,
            title: "Grep".into(),
            query: "needle".into(),
            rows: vec![crate::ui::render::PickerRow::text(
                "match",
                format!("src/long.rs:309:5 {long}"),
                long.clone(),
                String::new(),
            )],
            selected: Some(0),
            scroll: 0,
            total: 0,
            loading: false,
            preview: Some(crate::ui::render::PickerPreview::text(
                "src/long.rs",
                crate::ui::render::PickerTextPreview::new(
                    (300..320)
                        .map(|line| format!("preview line {line} {long}"))
                        .collect(),
                    300,
                    Some(308),
                    None,
                ),
            )),
        }),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 30);
    let result_y = (0..30)
        .find(|row| frame.grid.row_text(*row).contains("src/long.rs:309:5"))
        .expect("result row");
    let preview_y = (0..30)
        .find(|row| frame.grid.row_text(*row).contains("309 preview line 308"))
        .expect("highlighted preview row");
    assert!(preview_y > result_y + 1);
}

#[test]
fn a_debug_view_survives_a_narrow_width_in_the_shared_picker() {
    use crate::ui::PickerUiState;
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("fn main() {}"),
            Some(PathBuf::from("/workspace/main.rs")),
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let config = loaded.config.clone();
    let dashboard = loaded.dashboard.clone();

    // The debug views are the shared picker, so what must survive a narrow width is the ROW.
    let mut ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        ..UiState::default()
    };
    ui.picker = Some(PickerUiState {
        preview_side_scroll: (0, 0),
        footer: None,
        input_hint: String::new(),
        surface: crate::ui::PickerSurfaceKind::Debug,
        title: "Variables".into(),
        query: String::new(),
        rows: vec![crate::ui::render::PickerRow::text(
            "variables:0",
            "count = 3",
            "i32",
            "locals",
        )],
        selected: Some(0),
        scroll: 0,
        total: 0,
        loading: false,
        preview: None,
    });

    // WIDE: the row is present.
    let wide = render(&state, &ui, &config, &dashboard, 80, 12);
    let wide_rows: Vec<String> = (0..12).map(|y| wide.grid.row_text(y)).collect();
    assert!(
        wide_rows.iter().any(|row| row.contains("count = 3")),
        "the debug row is shown at a wide width: {wide_rows:?}"
    );

    // NARROW: the ROW still survives, which is what the clause asks the layout to preserve.
    let narrow = render(&state, &ui, &config, &dashboard, 24, 12);
    let narrow_rows: Vec<String> = (0..12).map(|y| narrow.grid.row_text(y)).collect();
    assert!(
        narrow_rows.iter().any(|row| row.contains("count = 3")),
        "and the row survives a narrow width: {narrow_rows:?}"
    );

    // ABSENT: with no debug view open there is no row, so its presence means something.
    let quiet = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        ..UiState::default()
    };
    let frame = render(&state, &quiet, &config, &dashboard, 80, 12);
    let none: Vec<String> = (0..12).map(|y| frame.grid.row_text(y)).collect();
    assert!(
        !none.iter().any(|row| row.contains("count = 3")),
        "with no debug view open the row is not drawn: {none:?}"
    );
}

/// U-15: a row carries per-cell text, per-cell style and glyph cells, and the columns line
/// up because each cell declares its own width rather than being concatenated into a string.
///
/// The assertions read the rendered grid: the text at each column position AND the style at
/// each cell, so a rendering that drew the right characters in the wrong styles fails here.
#[test]
fn a_row_with_cells_draws_each_column_at_its_own_width_and_style() {
    use crate::ui::CellAlign;
    use crate::ui::PickerCell;
    use crate::ui::Style;

    let state = EditorState::new(PathBuf::from("/tmp"));
    let mut row = super::PickerRow::text("row", "", "", "");
    row.cells = vec![
        PickerCell::glyph("U", Style::Error),
        PickerCell::new("src/main.rs", Style::Default, 16),
        PickerCell::new("staged", Style::Muted, 10),
        PickerCell {
            text: "12".into(),
            style: Style::Warning,
            width: 6,
            align: CellAlign::Right,
        },
    ];
    let ui = UiState {
        picker: Some(super::PickerUiState {
            preview_side_scroll: (0, 0),
            footer: None,
            input_hint: "Insert · escape normal".into(),
            surface: super::PickerSurfaceKind::Generic,
            title: "Changes".into(),
            query: String::new(),
            rows: vec![row],
            selected: None,
            scroll: 0,
            total: 0,
            loading: false,
            preview: None,
        }),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 30);

    let (glyph_x, y) = drawn_at(&frame, "U").expect("the glyph cell is drawn");
    let (path_x, path_y) = drawn_at(&frame, "src/main.rs").expect("the path cell is drawn");
    let (state_x, _) = drawn_at(&frame, "staged").expect("the provenance cell is drawn");
    let (count_x, _) = drawn_at(&frame, "12").expect("the count cell is drawn");
    assert_eq!(y, path_y, "every cell of one row is drawn on that row");

    // Columns follow the declared widths, not the text that happens to be in them.
    assert_eq!(
        path_x,
        glyph_x + 1,
        "the path column starts after the one-wide glyph"
    );
    assert_eq!(
        state_x,
        path_x + 16,
        "the provenance column follows a 16-wide path column"
    );
    assert_eq!(
        count_x + 2,
        state_x + 10 + 6,
        "a right-aligned cell ends at its column edge"
    );

    // Each cell keeps its own style, so a status column and a lane can be coloured apart.
    let style_at = |x: u16, y: u16| frame.grid.get(x, y).expect("a drawn cell").style;
    assert_eq!(
        style_at(glyph_x, y),
        Style::Error,
        "the glyph cell carries its own style"
    );
    assert_eq!(
        style_at(path_x, y),
        Style::Default,
        "the path cell carries its own style"
    );
    assert_eq!(
        style_at(state_x, y),
        Style::Muted,
        "the provenance cell carries its own style"
    );
    assert_eq!(
        style_at(count_x, y),
        Style::Warning,
        "the count cell carries its own style"
    );
}

/// U-15/U-16 (base form): a diff carried as structure renders its provenance, its hunk
/// headers, the exact count of a collapsed run, and each line with both numbers and a
/// classification glyph. Intra-line emphasis, side-by-side mode and syntax highlighting are
/// the diff renderer's own work and are not claimed by this test.
#[test]
fn a_short_diff_box_opens_on_the_change_and_keeps_the_comparison_on_top() {
    use crate::ui::DiffHunk;
    use crate::ui::DiffLine;
    use crate::ui::DiffLineKind;
    use crate::ui::DiffPreview;

    let line = |kind, text: &str| DiffLine {
        kind,
        old_number: Some(1),
        new_number: Some(1),
        text: text.to_string(),
        spans: Vec::new(),
    };
    let diff = DiffPreview {
        provenance: "HEAD -> index".into(),
        truncated: false,
        total_lines: 0,
        layout: crate::ui::DiffLayout::Unified,
        hunks: vec![DiffHunk {
            header: "@@ -42,4 +42,4 @@".into(),
            collapsed_before: 41,
            lines: vec![
                line(DiffLineKind::Context, "one"),
                line(DiffLineKind::Context, "two"),
                line(DiffLineKind::Context, "three"),
                line(DiffLineKind::Removed, "old"),
                line(DiffLineKind::Added, "new"),
            ],
        }],
    };
    // The change sits five rows below the pinned one, so a five-row box moves to it.
    assert_eq!(diff.opening_scroll(5), 4);
    // A box that holds it opens at the top, on the hunk's own header.
    assert_eq!(diff.opening_scroll(7), 0);
    assert_eq!(diff.last_scroll(5), 3);
    assert_eq!(diff.last_scroll(50), 0);
}

#[test]
fn a_structured_diff_preview_draws_provenance_hunks_and_classified_lines() {
    use crate::ui::DiffHunk;
    use crate::ui::DiffLine;
    use crate::ui::DiffLineKind;
    use crate::ui::DiffPreview;
    use crate::ui::PickerPreviewBody;
    use crate::ui::Style;

    let line = |kind, old_number, new_number, text: &str| DiffLine {
        kind,
        old_number,
        new_number,
        text: text.to_string(),
        spans: Vec::new(),
    };
    let diff = DiffPreview {
        provenance: "staged = HEAD -> index".into(),
        truncated: false,
        total_lines: 0,
        layout: crate::ui::DiffLayout::Unified,
        hunks: vec![DiffHunk {
            header: "@@ -31,6 +31,9 @@ impl Status {".into(),
            collapsed_before: 4,
            lines: vec![
                line(
                    DiffLineKind::Context,
                    Some(31),
                    Some(31),
                    "let raw = read(path)?;",
                ),
                line(
                    DiffLineKind::Added,
                    None,
                    Some(33),
                    "ensure!(gen > self.gen);",
                ),
                line(DiffLineKind::Removed, Some(32), None, "let gen = self.gen;"),
            ],
        }],
    };
    let state = EditorState::new(PathBuf::from("/tmp"));
    let ui = UiState {
        picker: Some(super::PickerUiState {
            preview_side_scroll: (0, 0),
            footer: None,
            input_hint: "Insert · escape normal".into(),
            surface: super::PickerSurfaceKind::Generic,
            title: "Changes".into(),
            query: String::new(),
            rows: vec![super::PickerRow::text(
                "row",
                "src/main.rs",
                "unstaged",
                "git",
            )],
            selected: None,
            scroll: 0,
            total: 0,
            loading: false,
            preview: Some(super::PickerPreview {
                title: "src/main.rs".into(),
                body: PickerPreviewBody::Diff(diff),
            }),
        }),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 120, 30);
    let rendered: Vec<String> = (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect();
    let has = |needle: &str| rendered.iter().any(|row| row.contains(needle));

    assert!(
        has("staged = HEAD -> index"),
        "the provenance names the comparison"
    );
    assert!(
        has("@@ -31,6 +31,9 @@ impl Status {"),
        "the hunk header is drawn"
    );
    assert!(
        has("4 unchanged lines"),
        "a collapsed run states its exact count"
    );
    // A line the other side does not have still occupies its number column, so the
    // classification glyphs form one column instead of drifting with the text.
    assert!(
        has("31 31   let raw = read(path)?;"),
        "a context line shows both numbers"
    );
    assert!(
        has("  33 + ensure!(gen > self.gen);"),
        "an added line keeps its old column blank"
    );
    assert!(
        has("32    - let gen = self.gen;"),
        "a removed line keeps its new column blank"
    );
    let (context_x, _) = drawn_at(&frame, "let raw = read(path)?;").expect("the context line");
    let (added_x, _) = drawn_at(&frame, "ensure!(gen > self.gen);").expect("the added line");
    let (removed_x, _) = drawn_at(&frame, "let gen = self.gen;").expect("the removed line");
    assert_eq!(
        context_x, added_x,
        "the added line's text starts where the context line's does"
    );
    assert_eq!(
        context_x, removed_x,
        "the removed line's text starts where the context line's does"
    );

    // Classification is carried by style as well as by the glyph, so the two agree.
    let (added_x, added_y) =
        drawn_at(&frame, "ensure!(gen > self.gen);").expect("the added line is drawn");
    assert_eq!(
        frame.grid.get(added_x, added_y).expect("cell").style,
        Style::StatusAccent
    );
    let (removed_x, removed_y) =
        drawn_at(&frame, "let gen = self.gen;").expect("the removed line is drawn");
    assert_eq!(
        frame.grid.get(removed_x, removed_y).expect("cell").style,
        Style::Error
    );
}

/// The same picker with the preview's two sides scrolled by their own offsets.
fn diff_picker_scrolled(diff: super::DiffPreview, scroll: (usize, usize)) -> UiState {
    let mut state = diff_picker(diff);
    if let Some(picker) = state.picker.as_mut() {
        picker.preview_side_scroll = scroll;
    }
    state
}

/// A picker whose preview carries a parsed diff.
pub(super) fn diff_picker(diff: super::DiffPreview) -> UiState {
    UiState {
        picker: Some(super::PickerUiState {
            preview_side_scroll: (0, 0),
            footer: None,
            input_hint: "Insert · escape normal".into(),
            surface: super::PickerSurfaceKind::Generic,
            title: "Changes".into(),
            query: String::new(),
            rows: vec![super::PickerRow::text("row", "f.txt", "unstaged", "git")],
            selected: None,
            scroll: 0,
            total: 0,
            loading: false,
            preview: Some(super::PickerPreview {
                title: "f.txt".into(),
                body: super::PickerPreviewBody::Diff(diff),
            }),
        }),
        ..UiState::default()
    }
}

/// U-16: every element the requirement names is drawn from real patch text.
#[test]
fn a_parsed_diff_renders_every_element_the_requirement_names() {
    use crate::ui::parse_unified_patch;

    let diff = parse_unified_patch(RENDER_PATCH, "unstaged = index -> disk");
    let state = EditorState::new(PathBuf::from("/tmp"));
    let ui = diff_picker(diff);
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 120, 30);
    let rendered: Vec<String> = (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect();
    let has = |needle: &str| rendered.iter().any(|row| row.contains(needle));

    assert!(
        has("unstaged = index -> disk"),
        "the provenance names the comparison"
    );
    assert!(has("@@ -1,6 +1,6 @@"), "the first hunk header is drawn");
    assert!(
        has("@@ -32,7 +32,7 @@ line thirty-one"),
        "the second hunk header is drawn"
    );
    assert!(
        has("25 unchanged lines"),
        "the collapsed run states its exact count"
    );
    // Both numbers are drawn in their own columns, and the side a line does not have is
    // left blank so the classification glyphs form one column.
    assert!(
        has(" 1  1   line one"),
        "a context line shows both of its numbers"
    );
    assert!(
        has("   3 + line three changed"),
        "an added line blanks its old-number column"
    );
    assert!(
        has(" 3    - line three"),
        "a removed line blanks its new-number column"
    );
    assert!(
        has("32 32   line thirty-two"),
        "the second hunk starts at the line its header names"
    );
    assert!(
        has("   35 + line thirty-five modified"),
        "the second hunk is numbered from its own header"
    );
    // Every classification glyph sits in one column.
    let glyph_column = |needle: &str| {
        let (x, _) = drawn_at(&frame, needle).expect("a drawn diff line");
        x
    };
    assert_eq!(
        glyph_column("line one"),
        glyph_column("line three changed"),
        "the context line's text and the added line's text start in the same column"
    );
}

/// U-16: the changed span inside a modified line is emphasised, and the rest of that line is
/// not, so a one-word change in a long line is visible.
#[test]
fn intra_line_emphasis_marks_the_changed_span_and_not_the_rest_of_the_line() {
    use crate::ui::Style;
    use crate::ui::parse_unified_patch;

    let diff = parse_unified_patch(RENDER_PATCH, "unstaged = index -> disk");
    let state = EditorState::new(PathBuf::from("/tmp"));
    let ui = diff_picker(diff);
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 120, 30);

    let (changed_x, y) = drawn_at(&frame, "changed").expect("the added line is drawn");
    assert_eq!(
        frame.grid.get(changed_x, y).expect("a drawn cell").style,
        Style::Search,
        "the changed word carries the emphasis style"
    );
    let (unchanged_x, same_y) = drawn_at(&frame, "line three changed").expect("the added line");
    assert_eq!(same_y, y, "both probes read the same row");
    assert_eq!(
        frame
            .grid
            .get(unchanged_x, same_y)
            .expect("a drawn cell")
            .style,
        Style::StatusAccent,
        "the unchanged part of the added line keeps the addition style"
    );
}

/// U-16: side by side, the old and new sides occupy their own columns with a divider between.
#[test]
fn side_by_side_puts_the_two_sides_in_their_own_columns() {
    use crate::ui::DiffLayout;
    use crate::ui::Style;
    use crate::ui::parse_unified_patch;

    let mut diff = parse_unified_patch(RENDER_PATCH, "unstaged = index -> disk");
    diff.layout = DiffLayout::SideBySide;
    let state = EditorState::new(PathBuf::from("/tmp"));
    let ui = diff_picker(diff);
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 120, 30);

    let (removed_x, y) = drawn_at(&frame, "line three").expect("the old side is drawn");
    let (added_x, added_y) = drawn_at(&frame, "line three changed").expect("the new side is drawn");
    assert_eq!(
        y, added_y,
        "both sides of one change are drawn on the same row"
    );
    assert!(
        added_x > removed_x,
        "the new side sits to the right of the old side: {removed_x} then {added_x}"
    );
    // The divider sits between the two sides.
    let divider = (removed_x..added_x).find(|x| {
        frame
            .grid
            .get(*x, y)
            .is_some_and(|cell| cell.symbol == "│" && cell.style == Style::Border)
    });
    assert!(
        divider.is_some(),
        "a border divider separates the two sides on row {y}"
    );
}

/// U-16: a bounded patch says what it is not showing, with the true total.
#[test]
fn a_bounded_patch_states_its_true_total() {
    use crate::ui::DiffHunk;
    use crate::ui::DiffLayout;
    use crate::ui::DiffLine;
    use crate::ui::DiffLineKind;
    use crate::ui::DiffPreview;

    let diff = DiffPreview {
        provenance: "staged = HEAD -> index".into(),
        hunks: vec![DiffHunk {
            header: "@@ -1 +1 @@".into(),
            collapsed_before: 0,
            lines: vec![DiffLine {
                kind: DiffLineKind::Added,
                old_number: None,
                new_number: Some(1),
                text: "visible".into(),
                spans: Vec::new(),
            }],
        }],
        truncated: true,
        total_lines: 1_200,
        layout: DiffLayout::Unified,
    };
    let state = EditorState::new(PathBuf::from("/tmp"));
    let ui = diff_picker(diff);
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 120, 30);
    let rendered: Vec<String> = (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect();
    assert!(
        rendered
            .iter()
            .any(|row| row.contains("1200 changed lines in total")),
        "a bounded view states the true total: {rendered:?}"
    );
}

/// U-16: the two halves of a side-by-side diff scroll INDEPENDENTLY — moving one side leaves
/// the other side's content exactly where it was.
#[test]
fn side_by_side_halves_scroll_independently() {
    use crate::ui::DiffLayout;
    use crate::ui::parse_unified_patch;

    // Four changes, so each side has its own sequence to scroll through.
    let patch = "@@ -1,4 +1,4 @@\n-old1\n+new1\n-old2\n+new2\n-old3\n+new3\n-old4\n+new4\n";
    let mut diff = parse_unified_patch(patch, "index -> disk");
    diff.layout = DiffLayout::SideBySide;
    let state = EditorState::new(PathBuf::from("/tmp"));
    let loaded = crate::config::load_bundled_config();
    let render_at = |scroll: (usize, usize)| {
        let ui = diff_picker_scrolled(diff.clone(), scroll);
        let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 120, 30);
        (0..frame.grid.height())
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<String>>()
            .join("\n")
    };

    let at_top = render_at((0, 0));
    assert!(
        at_top.contains("old1"),
        "the old side starts at its first line"
    );
    assert!(
        at_top.contains("new1"),
        "the new side starts at its first line"
    );

    // Move ONLY the new side down two rows.
    let new_scrolled = render_at((0, 2));
    assert!(
        new_scrolled.contains("old1"),
        "the old side did not move when the new side scrolled: {new_scrolled}"
    );
    assert!(
        !new_scrolled.contains("new1"),
        "the new side moved: {new_scrolled}"
    );
    assert!(
        new_scrolled.contains("new3"),
        "the new side now starts two rows further down: {new_scrolled}"
    );

    // And the other direction: move ONLY the old side, which must leave the new side alone.
    let old_scrolled = render_at((2, 0));
    assert!(
        !old_scrolled.contains("old1"),
        "the old side moved: {old_scrolled}"
    );
    assert!(
        old_scrolled.contains("old3"),
        "the old side now starts two rows further down: {old_scrolled}"
    );
    assert!(
        old_scrolled.contains("new1"),
        "the new side did not move when the old side scrolled: {old_scrolled}"
    );
}

#[test]
fn a_long_path_in_a_picker_row_keeps_its_end() {
    let ambiguous = crate::config::load_bundled_config()
        .config
        .editor
        .ambiguous_width;
    let path = "/tmp/a/very/long/prefix/that/repeats/on/every/row/src/main.py:12";
    let (label, detail) =
        crate::ui::render::picker::fitted_picker_row(path, "print(name)", 40, ambiguous);
    assert!(
        label.starts_with('…') && label.ends_with("main.py:12"),
        "{label}"
    );
    assert_eq!(detail, "print(name)");
    let (label, detail) = crate::ui::render::picker::fitted_picker_row(
        "main",
        "/tmp/a/very/long/prefix/main.go:7:2",
        24,
        ambiguous,
    );
    assert_eq!(label, "main");
    assert!(
        detail.starts_with('…') && detail.ends_with("main.go:7:2"),
        "{detail}"
    );
    let (label, _) =
        crate::ui::render::picker::fitted_picker_row("Git: stage/unstage all", "", 12, ambiguous);
    assert_eq!(
        label, "Git: stage/unstage all",
        "ordinary text is not a path"
    );
}

#[test]
fn picker_diff_geometry_does_not_count_line_numbers_twice() {
    use crate::ui::PickerPreview;
    use crate::ui::PickerPreviewBody;
    use crate::ui::PickerUiState;
    use crate::windows::WindowRect;
    let config = crate::config::load_bundled_config().config;
    let diff = crate::ui::DiffPreview {
        hunks: vec![crate::ui::DiffHunk {
            header: "@@".into(),
            collapsed_before: 0,
            lines: vec![crate::ui::DiffLine {
                old_number: Some(100),
                new_number: Some(100),
                kind: crate::ui::DiffLineKind::Context,
                text: "x".repeat(30),
                spans: Vec::new(),
            }],
        }],
        provenance: String::new(),
        truncated: false,
        total_lines: 1,
        layout: crate::ui::DiffLayout::Unified,
    };
    assert_eq!(diff.content_width(config.editor.ambiguous_width), 40);
    let picker = PickerUiState {
        preview: Some(PickerPreview {
            title: "Diff".into(),
            body: PickerPreviewBody::Diff(diff),
        }),
        preview_side_scroll: (0, 0),
        footer: None,
        input_hint: String::new(),
        surface: crate::ui::PickerSurfaceKind::Changes,
        title: "Changes".into(),
        query: String::new(),
        rows: Vec::new(),
        selected: None,
        scroll: 0,
        total: 0,
        loading: false,
    };
    let rect = WindowRect {
        x: 0,
        y: 0,
        width: 80,
        height: 24,
    };
    let geometry =
        crate::ui::render::picker::picker_content_geometry(&picker, &config, rect).unwrap();
    assert!(geometry.divider.is_some());
    assert!(geometry.stacked_divider.is_none());
    let narrow = WindowRect { width: 74, ..rect };
    let geometry =
        crate::ui::render::picker::picker_content_geometry(&picker, &config, narrow).unwrap();
    assert!(geometry.divider.is_none());
    assert!(geometry.stacked_divider.is_some());
}

#[test]
fn large_diff_rows_share_number_width() {
    let diff = crate::ui::DiffPreview {
        hunks: vec![crate::ui::DiffHunk {
            header: "@@".into(),
            collapsed_before: 0,
            lines: (1..=50_000)
                .map(|n| crate::ui::DiffLine {
                    old_number: None,
                    new_number: Some(n),
                    kind: crate::ui::DiffLineKind::Added,
                    text: "x".into(),
                    spans: Vec::new(),
                })
                .collect(),
        }],
        provenance: String::new(),
        truncated: false,
        total_lines: 50_000,
        layout: crate::ui::DiffLayout::Unified,
    };
    assert_eq!(
        diff.content_width(crate::config::AmbiguousWidth::Narrow),
        15
    );
    let rows = diff.rows();
    assert_eq!(rows.len(), 50_001);
    assert!(rows[1].text.contains("    1 + x"));
    assert!(rows.last().unwrap().text.contains("50000 + x"));
}

#[test]
fn diff_tabs_and_combining_marks_keep_text_and_emphasis_aligned() {
    let state = EditorState::new("/tmp".into());
    let loaded = crate::config::load_bundled_config();
    let ui = diff_picker(crate::ui::diff::parse_unified_patch(
        "@@ -1 +1 @@\n-\tlet x = e\u{300};\n+\tlet x = e\u{301};\n",
        "test",
    ));
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 120, 30);
    let (x, y) = drawn_at(&frame, "e\u{301}").unwrap();
    assert_eq!(
        frame.grid.get(x, y).unwrap().style,
        crate::ui::Style::Search
    );
    assert!(!(0..30).any(|row| frame.grid.row_text(row).contains('◌')));
}
use crate::ui::render::composition_tests::drawn_at;

/// Verbatim `git diff` output, the same real patch text the parser tests use, so these render
/// tests exercise the whole chain: real patch text -> parser -> rendered cells.
const RENDER_PATCH: &str = "\
diff --git a/f.txt b/f.txt
index 9792656..89d6f9e 100644
--- a/f.txt
+++ b/f.txt
@@ -1,6 +1,6 @@
 line one
 line two
-line three
+line three changed
 line four
 line five
 line six
@@ -32,7 +32,7 @@ line thirty-one
 line thirty-two
 line thirty-three
 line thirty-four
-line thirty-five
+line thirty-five modified
 line thirty-six
 line thirty-seven
 line thirty-eight
";
