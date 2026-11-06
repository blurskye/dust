use std::path::PathBuf;
use crate::editor::EditorState;
use crate::ui::render::UiState;

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
                    crate::ui::render::picker::picker_content_geometry(&picker, &config, rect).unwrap();
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
                crate::ui::render::PickerTextPreview::new(vec!["fn health() {}".to_string()], 0, Some(0), None),
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