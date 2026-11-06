//! Documentation tests for editor surfaces and frame composition.

use crate::editor::EditorState;
use crate::ui::render::CompletionItem;
use crate::ui::render::DocumentationMarkupKind;
use crate::ui::render::DocumentationPopupKind;
use crate::ui::render::DocumentationPopupState;
use crate::ui::render::DocumentationSection;
use crate::ui::render::UiState;
use std::path::PathBuf;

use crate::ui::render::composition_tests::editor_layout;
use crate::ui::render::composition_tests::render;

#[test]
fn documentation_popup_preserves_lines_clamps_and_protects_status_row() {
    let mut state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("cursor\nline\nline\nline\nline\nline"),
            None,
        )
        .unwrap();
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        documentation_popup: Some(DocumentationPopupState {
            kind: DocumentationPopupKind::Hover,
            title: "Hover".into(),
            provider: "test".into(),
            source_range: None,
            sections: vec![DocumentationSection {
                heading: None,
                markup: DocumentationMarkupKind::Markdown,
                spans: Vec::new(),
                lines: std::iter::once("first line".into())
                    .chain(std::iter::once("wide 界界 clipped safely".into()))
                    .chain((2..20).map(|line| format!("line {line}")))
                    .collect(),
            }],
            scroll: 0,
        }),
        ..UiState::default()
    };
    let config = crate::config::load_bundled_config().config;
    let dashboard = crate::config::load_bundled_config().dashboard;
    let frame = render(&state, &ui, &config, &dashboard, 24, 8);
    let screen = (0..8)
        .map(|y| {
            (0..24)
                .map(|x| frame.grid.get(x, y).map_or("", |cell| cell.symbol.as_str()))
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(screen.iter().any(|line| line.contains("Hover")));
    assert!(screen.iter().any(|line| line.contains("first line")));
    assert!(screen.iter().any(|line| line.contains("wide 界")));
    assert!(screen.iter().any(|line| line.contains("j/k")));
    assert!(screen.last().is_some_and(|line| line.contains("NORMAL")));
}

#[test]
fn documentation_and_key_hints_stay_inside_an_offset_workspace() {
    use crate::ui::CellGrid;
    use crate::ui::Style;
    use crate::windows::WindowRect;
    let config = crate::config::load_bundled_config().config;
    let bounds = WindowRect {
        x: 7,
        y: 12,
        width: 60,
        height: 11,
    };
    let popup = DocumentationPopupState {
        kind: DocumentationPopupKind::Hover,
        title: "Hover".into(),
        provider: String::new(),
        source_range: None,
        scroll: 0,
        sections: vec![DocumentationSection {
            heading: None,
            markup: DocumentationMarkupKind::PlainText,
            lines: vec!["Documentation".into(); 20],
            spans: Vec::new(),
        }],
    };
    for cursor_y in [12, 16, 22] {
        let mut frame = crate::ui::grid::Frame {
            grid: CellGrid::new(80, 24),
            cursor: Some((10, cursor_y, crossterm::cursor::SetCursorStyle::SteadyBar)),
        };
        crate::ui::render::documentation::render_documentation_popup(
            &mut frame, &popup, &config, bounds, None,
        );
        assert!((12..23).any(|y| frame.grid.row_text(y).contains("Hover")));
        crate::ui::render::render_key_hints(
            &mut frame,
            &crate::ui::render::KeyHintUiState {
                prefix: "Space f".into(),
                rows: vec![crate::ui::render::KeyHintUiRow {
                    next_key: "f".into(),
                    label: "Files".into(),
                    icon: None,
                    group: false,
                }],
            },
            bounds,
            crate::config::AmbiguousWidth::Narrow,
        );
        assert!((12..23).any(|y| frame.grid.row_text(y).contains("Space f")));
        for y in 0..24 {
            for x in 0..80 {
                if x < bounds.x
                    || x >= bounds.x + bounds.width
                    || y < bounds.y
                    || y >= bounds.y + bounds.height
                {
                    assert_eq!(
                        frame.grid.get(x, y).unwrap().style,
                        Style::Default,
                        "at {x},{y}"
                    );
                }
            }
        }
    }
}

#[test]
fn completion_documentation_never_covers_the_candidates_on_a_narrow_screen() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("text-blue-\nb\nc\n"),
            Some(PathBuf::from("/tmp/index.html")),
        )
        .unwrap();
    if let Some(view) = state.active_view() {
        state.views[view].cursor = crate::editor::text::ByteOffset(9);
    }
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        language_completion: Some(crate::ui::render::LanguageCompletionState {
            typed: 0,
            items: (0..6)
                .map(|index| CompletionItem {
                    icon: None,
                    color: None,
                    label: format!("text-blue-{index}00"),
                    detail: "Color with a long detail for the half width".into(),
                })
                .collect(),
            selected: 0,
            scroll: 0,
        }),
        documentation_popup: Some(DocumentationPopupState {
            kind: DocumentationPopupKind::Completion,
            title: "Completion".into(),
            provider: "test".into(),
            source_range: None,
            sections: vec![DocumentationSection {
                heading: None,
                markup: DocumentationMarkupKind::Markdown,
                spans: Vec::new(),
                lines: vec!["#eff6ff with a line wide enough to fill the box".into()],
            }],
            scroll: 0,
        }),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 24);
    let text = (0..24)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    for index in 0..6 {
        assert!(text.contains(&format!("text-blue-{index}00")), "{text}");
    }
    assert!(text.contains("#eff6ff"), "{text}");
}

#[test]
fn completion_documentation_wraps_to_the_room_beside_a_wide_menu() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("div {\n co\n}\n"),
            Some(PathBuf::from("/tmp/completion.css")),
        )
        .unwrap();
    if let Some(view) = state.active_view() {
        state.views[view].cursor = crate::editor::text::ByteOffset(9);
    }
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        language_completion: Some(crate::ui::render::LanguageCompletionState {
            typed: 0,
            items: ["color", "content", "column-gap", "contain-intrinsic-size"]
                .into_iter()
                .map(|label| CompletionItem {
                    icon: None,
                    color: None,
                    label: label.into(),
                    detail: "vscode-css-language-server".into(),
                })
                .collect(),
            selected: 0,
            scroll: 0,
        }),
        documentation_popup: Some(DocumentationPopupState {
            kind: DocumentationPopupKind::Completion,
            title: "Completion · color".into(),
            provider: "test".into(),
            source_range: None,
            sections: vec![DocumentationSection {
                heading: None,
                markup: DocumentationMarkupKind::Markdown,
                spans: Vec::new(),
                lines: vec![
                    "Sets the color of an element's text".into(),
                    String::new(),
                    "Widely available across major browsers (Baseline since 2015)".into(),
                ],
            }],
            scroll: 0,
        }),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 24);
    let text = (0..24)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    for word in ["element's", "text", "2015)"] {
        assert!(text.contains(word), "{word} lost:\n{text}");
    }
}

#[test]
fn documentation_scroll_reaches_the_tail_of_a_single_wrapped_line() {
    let config = crate::config::load_bundled_config().config;
    let popup = DocumentationPopupState {
        kind: DocumentationPopupKind::Git,
        title: "Hunk".into(),
        provider: String::new(),
        source_range: None,
        sections: vec![DocumentationSection {
            heading: None,
            markup: DocumentationMarkupKind::PlainText,
            lines: vec![format!("{}THE-END", "x".repeat(2000))],
            spans: Vec::new(),
        }],
        scroll: 20,
    };
    let total = crate::ui::render::documentation::documentation_display_rows(&popup, &config, 80);
    assert!(total > 20);
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(80, 40),
        cursor: Some((0, 0, crossterm::cursor::SetCursorStyle::SteadyBar)),
    };
    crate::ui::render::documentation::render_documentation_popup(
        &mut frame,
        &popup,
        &config,
        crate::windows::WindowRect {
            x: 0,
            y: 0,
            width: 80,
            height: 40,
        },
        None,
    );
    assert!((0..40).any(|y| frame.grid.row_text(y).contains("THE-END")));
    assert!(
        crate::ui::render::documentation::wrap_ranges(
            &"·".repeat(6),
            10,
            crate::config::AmbiguousWidth::Wide
        )
        .len()
            > 1
    );
}

#[test]
fn documentation_popup_clamps_scroll_after_widening() {
    let config = crate::config::load_bundled_config().config;
    let mut popup = DocumentationPopupState {
        kind: DocumentationPopupKind::Hover,
        title: "Hover".into(),
        provider: String::new(),
        source_range: None,
        sections: vec![DocumentationSection {
            heading: None,
            markup: DocumentationMarkupKind::PlainText,
            spans: Vec::new(),
            lines: vec![format!("{}THE-END", "x".repeat(300))],
        }],
        scroll: 0,
    };
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(16, 24),
        cursor: None,
    };
    let (visible, total) = crate::ui::render::documentation::render_documentation_popup(
        &mut frame,
        &popup,
        &config,
        crate::windows::WindowRect {
            x: 0,
            y: 0,
            width: 16,
            height: 24,
        },
        None,
    )
    .unwrap();
    popup.scroll = total - visible;
    assert!(popup.scroll > 0);
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(80, 24),
        cursor: None,
    };
    crate::ui::render::documentation::render_documentation_popup(
        &mut frame,
        &popup,
        &config,
        crate::windows::WindowRect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        },
        None,
    );
    assert!((0..24).any(|y| frame.grid.row_text(y).contains("THE-END")));
}

#[test]
fn documentation_popup_wraps_visible_replacements_and_keeps_wide_borders_inside() {
    let mut config = crate::config::load_bundled_config().config;
    config.editor.ambiguous_width = crate::config::AmbiguousWidth::Wide;
    let popup = DocumentationPopupState {
        kind: DocumentationPopupKind::Hover,
        title: String::new(),
        provider: String::new(),
        source_range: None,
        sections: vec![DocumentationSection {
            heading: None,
            markup: DocumentationMarkupKind::PlainText,
            spans: Vec::new(),
            lines: vec![format!("{}END", "\t\u{200b}".repeat(12))],
        }],
        scroll: 0,
    };
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new_with_policy(30, 24, "�", "◌", true),
        cursor: Some((0, 0, crossterm::cursor::SetCursorStyle::SteadyBar)),
    };
    let (_, total) = crate::ui::render::documentation::render_documentation_popup(
        &mut frame,
        &popup,
        &config,
        crate::windows::WindowRect {
            x: 0,
            y: 0,
            width: 16,
            height: 24,
        },
        None,
    )
    .unwrap();
    assert!(total > 1);
    assert!((0..24).any(|y| frame.grid.row_text(y).contains("END")));
    assert_eq!(frame.grid.get(15, 1).unwrap().symbol, "+");
    assert_eq!(frame.grid.get(15, 2).unwrap().symbol, "|");
    assert_eq!(frame.grid.get(5, 1).unwrap().symbol, "-");
    for y in 0..24 {
        assert_eq!(frame.grid.get(16, y).unwrap().symbol, " ");
    }
}
