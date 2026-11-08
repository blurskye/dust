//! Completion tests for editor surfaces and frame composition.

use crate::editor::EditorState;
use crate::ui::render::CompletionItem;
use crate::ui::render::PromptKind;
use crate::ui::render::PromptState;
use crate::ui::render::UiState;
use crate::ui::render::prompt_geometry;
use std::path::PathBuf;

use crate::ui::render::composition_tests::attach_test_compositor;
use crate::ui::render::composition_tests::editor_layout;
use crate::ui::render::composition_tests::render;
use crate::ui::render::composition_tests::ui_with_editor_layout;

#[test]
fn completion_attaches_below_globally_centered_prompt() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    let mut prompt = PromptState::new(PromptKind::Command);
    prompt.input = "e".into();
    prompt.cursor = 1;
    prompt.completions = vec![
        CompletionItem {
            icon: None,
            color: None,
            label: "e".into(),
            detail: "Open file".into(),
        },
        CompletionItem {
            icon: None,
            color: None,
            label: "edit.file".into(),
            detail: "Example".into(),
        },
    ];
    prompt.selected_completion = Some(0);
    let ui = UiState {
        dashboard: true,
        prompt: Some(prompt),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let config = loaded.config;
    let dashboard = loaded.dashboard;
    let frame = render(&state, &ui, &config, &dashboard, 100, 20);
    let geometry = prompt_geometry(100, 20, false);
    assert_eq!(geometry.left, 20);
    let row = frame.grid.row_text(geometry.top + 3);
    assert!(row.contains("Open file"));
    // The selected row's description sits on the selection, not a dark gap.
    let x = u16::try_from(row[..row.find("Open file").unwrap()].chars().count()).unwrap();
    assert_eq!(
        frame
            .grid
            .get(x, geometry.top + 3)
            .expect("detail cell")
            .style,
        crate::ui::Style::Selection
    );
}

#[test]
fn prompt_completion_viewport_follows_a_selected_item_beyond_its_first_page() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    let mut prompt = PromptState::new(PromptKind::Command);
    let count = usize::from(crate::ui::render::COMPLETION_MAX_ROWS) * 2;
    prompt.completions = (0..count)
        .map(|index| CompletionItem {
            icon: None,
            color: None,
            label: format!("candidate-{index}"),
            detail: format!("detail-{index}"),
        })
        .collect();
    prompt.selected_completion = Some(count - 1);
    let ui = UiState {
        dashboard: true,
        prompt: Some(prompt),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let config = loaded.config;
    let frame = render(&state, &ui, &config, &loaded.dashboard, 100, 20);
    let screen = (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>();

    let last = format!("candidate-{}", count - 1);
    assert!(screen.iter().any(|line| line.contains(&last)));
    assert!(!screen.iter().any(|line| line.contains("candidate-0")));
}

#[test]
fn completion_is_clipped_to_the_focused_split() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("left and right"),
            Some(PathBuf::from("/workspace/main.rs")),
        )
        .unwrap();
    state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("split");
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    ui.language_completion = Some(crate::ui::render::LanguageCompletionState {
        typed: 0,
        items: vec![CompletionItem {
            icon: None,
            color: None,
            label: "selected-completion".into(),
            detail: "Function · fake".into(),
        }],
        selected: 0,
        scroll: 0,
    });
    attach_test_compositor(&state, &mut ui, &loaded.config, 80, 20);
    let pane = crate::ui::render::active_pane_viewport(&state, &ui, &loaded.config, 80, 19)
        .expect("active pane");
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 20);
    assert!(drawn_at(&frame, "selected-completion").is_some());
    let mut selected_cells = 0;
    for row in 0..20 {
        for column in 0..80 {
            if frame
                .grid
                .get(column, row)
                .is_some_and(|cell| cell.style == crate::ui::Style::Selection)
            {
                selected_cells += 1;
                assert!(column >= pane.x && column < pane.x.saturating_add(pane.width));
                assert!(row >= pane.y && row < pane.y.saturating_add(pane.height));
            }
        }
    }
    assert!(
        selected_cells > 0,
        "the completion selection must be painted"
    );
}

#[test]
fn language_diagnostics_and_completion_are_projected_without_service_calls() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let path = PathBuf::from("/tmp/main.rs");
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("bad"),
            Some(path.clone()),
        )
        .unwrap();
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        diagnostics: vec![crate::ui::render::UiDiagnostic {
            buffer: None,
            path,
            severity: crate::workspace::DiagnosticSeverity::Error,
            range: crate::editor::text::ByteRange::new(
                crate::editor::text::ByteOffset(0),
                crate::editor::text::ByteOffset(3),
            )
            .expect("range"),
            source: "rustc".into(),
            code: Some("E0001".into()),
            message: "problem".into(),
        }],
        git_branch: Some("lean".into()),
        running_tasks: 2,
        language_completion: Some(crate::ui::render::LanguageCompletionState {
            typed: 0,
            items: vec![CompletionItem {
                icon: None,
                color: None,
                label: "better".into(),
                detail: "fake LSP".into(),
            }],
            selected: 0,
            scroll: 0,
        }),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 10);
    let view = state.active_view().expect("active view");
    let gutter = crate::ui::render::editor_state_gutter_width(
        &state,
        &state.buffers[state.views[view].buffer].text,
        &loaded.config.editor,
        100,
    );
    assert_eq!(
        frame.grid.get(gutter, 0).expect("diagnostic text").style,
        crate::ui::Style::DiagnosticError
    );
    assert!(frame.grid.row_text(0).contains("●● problem [E0001]"));
    assert!((0..10).any(|row| frame.grid.row_text(row).contains("better")));
    let status = frame.grid.row_text(9);
    assert!(status.contains("\u{f057} 1") && status.contains("lean") && status.contains("Tasks 2"));
}

#[test]
fn language_completion_keeps_the_selected_item_inside_its_bounded_viewport() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("value"),
            Some(PathBuf::from("/tmp/main.rs")),
        )
        .unwrap();
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        language_completion: Some(crate::ui::render::LanguageCompletionState {
            typed: 0,
            items: (0..20)
                .map(|index| CompletionItem {
                    icon: None,
                    color: None,
                    label: format!("item-{index:02}"),
                    detail: "candidate".into(),
                })
                .collect(),
            selected: 15,
            scroll: 0,
        }),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let config = loaded.config;
    let frame = render(&state, &ui, &config, &loaded.dashboard, 40, 10);
    let selected_row = (0..10)
        .find(|row| frame.grid.row_text(*row).contains("item-15"))
        .expect("selected completion remains visible");
    assert!(!((0..10).any(|row| frame.grid.row_text(row).contains("item-00"))));
    assert!((0..40).any(|column| {
        frame
            .grid
            .get(column, selected_row)
            .is_some_and(|cell| cell.style == crate::ui::Style::Selection)
    }));
}
use crate::ui::render::composition_tests::drawn_at;

use crate::ui::render::DocumentationMarkupKind;
use crate::ui::render::DocumentationPopupKind;
use crate::ui::render::DocumentationPopupState;
use crate::ui::render::DocumentationSection;

#[test]
fn indented_completion_keeps_room_for_documentation() {
    for width in [60, 80, 100, 160] {
        let mut frame = crate::ui::grid::Frame {
            grid: crate::ui::CellGrid::new(width, 24),
            cursor: Some((12, 20, crossterm::cursor::SetCursorStyle::SteadyBar)),
        };
        let completion = crate::ui::render::LanguageCompletionState {
            typed: 0,
            items: vec![CompletionItem {
                icon: None,
                color: None,
                label: "long_completion_method_name".into(),
                detail: "rust-analyzer".into(),
            }],
            selected: 0,
            scroll: 0,
        };
        let rect = crate::ui::render::overlays::render_language_completion(
            &mut frame,
            &completion,
            &crate::config::load_bundled_config().config,
            crate::windows::WindowRect {
                x: 0,
                y: 0,
                width,
                height: 24,
            },
        )
        .unwrap();
        assert!(rect.x >= 20 || width - rect.x - rect.width >= 20);
    }
}

#[test]
fn long_completion_labels_end_in_an_ellipsis_inside_the_menu() {
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(80, 24),
        cursor: Some((10, 4, crossterm::cursor::SetCursorStyle::SteadyBar)),
    };
    let completion = crate::ui::render::LanguageCompletionState {
        typed: 0,
        items: vec![CompletionItem {
            icon: None,
            color: None,
            label: "long_method_".repeat(12),
            detail: "rust-analyzer".into(),
        }],
        selected: 0,
        scroll: 0,
    };
    let rect = crate::ui::render::overlays::render_language_completion(
        &mut frame,
        &completion,
        &crate::config::load_bundled_config().config,
        crate::windows::WindowRect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        },
    )
    .unwrap();
    assert!(frame.grid.row_text(rect.y).trim_end().ends_with('…'));
    assert_eq!(
        frame
            .grid
            .get(rect.x + rect.width - 1, rect.y)
            .unwrap()
            .symbol,
        " "
    );
}

#[test]
fn completion_height_fits_on_one_side_of_the_cursor() {
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(40, 10),
        cursor: Some((3, 5, crossterm::cursor::SetCursorStyle::SteadyBar)),
    };
    frame
        .grid
        .put_text(0, 5, "editing", crate::ui::Style::Default);
    let completion = crate::ui::render::LanguageCompletionState {
        typed: 0,
        items: (0..9)
            .map(|n| CompletionItem {
                icon: None,
                color: None,
                label: format!("item{n}"),
                detail: String::new(),
            })
            .collect(),
        selected: 8,
        scroll: 0,
    };
    let config = crate::config::load_bundled_config().config;
    let rect = crate::ui::render::overlays::render_language_completion(
        &mut frame,
        &completion,
        &config,
        crate::windows::WindowRect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        },
    )
    .unwrap();
    assert!(rect.y + rect.height <= 5 || rect.y > 5);
    assert!(frame.grid.row_text(5).starts_with("editing"));
    assert!((0..10).any(|y| frame.grid.row_text(y).contains("item8")));
}

#[test]
fn completion_documentation_reports_final_wrapping_and_suppresses_cursor_overlap() {
    let config = crate::config::load_bundled_config().config;
    let popup = DocumentationPopupState {
        kind: DocumentationPopupKind::Completion,
        title: "Completion".into(),
        provider: String::new(),
        source_range: None,
        sections: vec![DocumentationSection {
            heading: None,
            markup: DocumentationMarkupKind::PlainText,
            spans: Vec::new(),
            lines: vec![format!("{}THE-END", "x".repeat(400))],
        }],
        scroll: usize::MAX,
    };
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(80, 24),
        cursor: Some((21, 20, crossterm::cursor::SetCursorStyle::SteadyBar)),
    };
    let (visible, total) = crate::ui::render::documentation::render_documentation_popup(
        &mut frame,
        &popup,
        &config,
        crate::windows::WindowRect {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        },
        Some(crate::windows::WindowRect {
            x: 20,
            y: 4,
            width: 53,
            height: 16,
        }),
    )
    .unwrap();
    let expected = crate::ui::render::documentation::wrap_ranges(
        &popup.sections[0].lines[0],
        16,
        config.editor.ambiguous_width,
    )
    .len();
    assert_eq!(total, expected);
    assert!(visible <= 14);
    assert!((0..24).any(|y| frame.grid.row_text(y).contains("THE-END")));
    assert!(
        !(0..80)
            .any(|x| frame.grid.get(x, 20).unwrap().style
                == crate::ui::Style::DocumentationBackground)
    );

    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(80, 3),
        cursor: Some((21, 2, crossterm::cursor::SetCursorStyle::SteadyBar)),
    };
    assert!(
        crate::ui::render::documentation::render_documentation_popup(
            &mut frame,
            &popup,
            &config,
            crate::windows::WindowRect {
                x: 0,
                y: 0,
                width: 80,
                height: 3
            },
            Some(crate::windows::WindowRect {
                x: 20,
                y: 0,
                width: 53,
                height: 2
            }),
        )
        .is_none()
    );
    assert!((0..3).all(|y| frame.grid.row_text(y).trim().is_empty()));
}
