//! Editor tests for editor surfaces and frame composition.

use crate::editor::EditorState;
use crate::ui::render::CompletionItem;
use crate::ui::render::DocumentationMarkupKind;
use crate::ui::render::DocumentationPopupKind;
use crate::ui::render::DocumentationPopupState;
use crate::ui::render::DocumentationSection;
use std::path::PathBuf;

use crate::ui::render::composition_tests::attach_test_compositor;
use crate::ui::render::composition_tests::render;
use crate::ui::render::composition_tests::ui_with_editor_layout;

#[test]
fn editor_popups_are_suppressed_while_a_modal_owns_input() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(crate::editor::text::TextBuffer::from_text("code"), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    for modal in 0..3 {
        let mut ui = ui_with_editor_layout(&state);
        match modal {
            0 => ui.settings = Some(crate::ui::render::SettingsPageState::default()),
            1 => {
                ui.tool_menu = Some(crate::ui::render::ToolMenuState {
                    category: "lsp".into(),
                    selected: 0,
                    scroll: 0,
                    rows: Vec::new(),
                    runtime_picker: None,
                    query: String::new(),
                    searching: false,
                })
            }
            _ => {
                ui.picker = Some(crate::ui::render::PickerUiState {
                    preview_side_scroll: (0, 0),
                    footer: None,
                    input_hint: String::new(),
                    surface: crate::ui::render::PickerSurfaceKind::Generic,
                    title: "Picker".into(),
                    query: String::new(),
                    rows: Vec::new(),
                    selected: None,
                    scroll: 0,
                    total: 0,
                    loading: false,
                    preview: None,
                })
            }
        }
        let expected = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 24);
        ui.language_completion = Some(crate::ui::render::LanguageCompletionState {
            typed: 0,
            items: vec![CompletionItem {
                icon: None,
                color: None,
                label: "candidate".into(),
                detail: String::new(),
            }],
            selected: 0,
            scroll: 0,
        });
        ui.documentation_popup = Some(DocumentationPopupState {
            kind: DocumentationPopupKind::Hover,
            title: "Hover".into(),
            provider: String::new(),
            source_range: None,
            scroll: 0,
            sections: vec![DocumentationSection {
                heading: None,
                markup: DocumentationMarkupKind::PlainText,
                lines: vec!["old documentation".into()],
                spans: Vec::new(),
            }],
        });
        ui.lsp_loading = Some("Loading".into());
        let actual = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 24);
        assert_eq!(actual.grid, expected.grid, "modal {modal}");
    }
}

#[test]
fn workspace_editor_cursor_follows_compositor_focus() {
    use crate::windows::CompositorLayer;
    use crate::windows::ProjectedCompositorWindow;
    use crate::windows::WindowId;
    use crate::windows::WindowRect;

    let loaded = crate::config::load_bundled_config();
    for split in [false, true] {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state
            .open_text(crate::editor::text::TextBuffer::from_text("editor"), None)
            .unwrap();
        if split {
            state
                .split_active(crate::windows::TileAxis::Horizontal, 500)
                .unwrap();
        }
        let mut ui = ui_with_editor_layout(&state);
        attach_test_compositor(&state, &mut ui, &loaded.config, 60, 8);
        let frame =
            crate::ui::render::render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 8);
        assert!(frame.cursor.is_some());

        let windows = &mut ui.compositor_layout.as_mut().unwrap().projection.windows;
        for window in windows.iter_mut() {
            window.focused = false;
        }
        windows.push(ProjectedCompositorWindow {
            window: WindowId(99),
            rect: WindowRect {
                x: 50,
                y: 0,
                width: 10,
                height: 3,
            },
            layer: CompositorLayer::Overlay,
            z_index: 1,
            focused: true,
            focusable: true,
        });
        ui.plugin_surfaces
            .push(crate::ui::render::PluginSurfaceUiState {
                handle: 9,
                window: WindowId(99),
                tree_revision: 1,
                scroll: 0,
                rows: vec![crate::ui::render::PluginSurfaceRow {
                    node: 1,
                    depth: 0,
                    text: "plugin".into(),
                    style: crate::ui::Style::Accent,
                    actionable: true,
                    focused: true,
                }],
            });
        let frame =
            crate::ui::render::render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 8);
        assert!(frame.grid.row_text(0).contains("plugin"));
        assert!(frame.cursor.is_none());
    }
}

use crate::ui::render::UiState;

#[test]
fn indent_guides_inside_a_multiline_string_keep_the_guide_colour() {
    let text = "fn f() {\n    let s = r\"a\n        b\";\n}\n";
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text(text),
            Some(PathBuf::from("/workspace/file.rs")),
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let start = text.find("r\"").expect("string start");
    let end = text.find("\";").expect("string end") + 1;
    let mut ui = ui_with_editor_layout(&state);
    ui.syntax_spans = vec![crate::ui::render::UiSyntaxSpan {
        buffer,
        revision: state.buffers[buffer].text.revision(),
        range: crate::editor::text::ByteRange::new(
            crate::editor::text::ByteOffset(start),
            crate::editor::text::ByteOffset(end),
        )
        .expect("string range"),
        role: crate::config::Highlight::STRING,
    }];
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 30, 5);
    let guide = frame.grid.get(4, 2).expect("guide inside the string");
    assert_ne!(guide.symbol, " ");
    assert!(
        matches!(
            guide.style,
            crate::ui::Style::Border | crate::ui::Style::Muted
        ),
        "{:?}",
        guide.style
    );
}

#[test]
fn ellipsized_text_reserves_the_measured_ellipsis_width() {
    use crate::config::AmbiguousWidth::Narrow;
    use crate::config::AmbiguousWidth::Wide;
    for policy in [Narrow, Wide] {
        for maximum in 0..8 {
            let text = crate::ui::geometry::ellipsized("abcdef", maximum, policy);
            assert!(crate::ui::geometry::text_width(&text, policy) <= maximum);
        }
    }
    assert_eq!(crate::ui::geometry::ellipsized("abcdef", 4, Wide), "ab…");
    assert_eq!(crate::ui::geometry::ellipsized("abcdef", 1, Wide), "");
    assert_eq!(crate::ui::geometry::ellipsized("abcdef", 2, Wide), "…");
    assert_eq!(crate::ui::geometry::ellipsized("abcdef", 4, Narrow), "abc…");
}

#[test]
fn eof_fold_stops_forward_scrolling() {
    let mut state = EditorState::new("/tmp".into());
    let (buffer, view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text("header\nlast"),
            None,
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let ui = UiState {
        lsp_folds: vec![crate::ui::render::UiFold {
            view,
            start_line: 0,
            end_line: 1,
            collapsed: true,
        }],
        ..ui_with_editor_layout(&state)
    };
    let model = crate::ui::render::VisualRowModel::new(
        &state.buffers[buffer].text,
        view,
        &ui,
        &loaded.config,
        80,
        true,
    );
    let first = crate::ui::render::VisualRowAnchor::default();
    assert_eq!(model.next(first), first);
    assert_eq!(model.forward(first, 100), first);
}

#[test]
fn inlay_budgets_survive_wrapped_scrolling_and_leave_source_visible() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    let mut state = EditorState::new("/tmp".into());
    let (buffer, view) = state
        .open_text(TextBuffer::from_text(&"x".repeat(40)), None)
        .unwrap();
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.wrap = true;
    config.ui.status_line.enabled = false;
    state.views[view].scroll_wrap_row = 7;
    state.views[view].cursor = ByteOffset(39);
    let mut ui = ui_with_editor_layout(&state);
    ui.lsp_inlay_hints = (0..40)
        .map(|at| crate::ui::render::UiInlayHint {
            buffer,
            revision: state.buffers[buffer].text.revision(),
            position: ByteOffset(at),
            label: "h".into(),
        })
        .collect();
    let frame = crate::ui::render::editor::render_editor_view(&state, view, &ui, &config, 10, 1);
    assert_eq!(frame.grid.row_text(0).trim(), "xx");
    assert_eq!(frame.cursor.map(|(x, y, _)| (x, y)), Some((1, 0)));
    state.buffers[buffer].text = TextBuffer::from_text("a");
    state.views[view].cursor = ByteOffset(0);
    state.views[view].scroll_wrap_row = 0;
    ui.lsp_inlay_hints = vec![crate::ui::render::UiInlayHint {
        buffer,
        revision: state.buffers[buffer].text.revision(),
        position: ByteOffset(0),
        label: "hhhh".into(),
    }];
    let frame = crate::ui::render::editor::render_editor_view(&state, view, &ui, &config, 4, 1);
    assert_eq!(frame.grid.row_text(0), "hhha");
    assert_eq!(frame.cursor.map(|(x, y, _)| (x, y)), Some((3, 0)));
    ui.lsp_inlay_hints[0].label = "界界".into();
    let frame = crate::ui::render::editor::render_editor_view(&state, view, &ui, &config, 4, 1);
    assert_eq!(frame.grid.get(2, 0).unwrap().symbol, "a");
    assert_eq!(frame.cursor.map(|(x, y, _)| (x, y)), Some((2, 0)));
}

#[test]
fn nonwrapped_cursor_uses_the_painted_inlay_budget_after_horizontal_scrolling() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    let mut state = EditorState::new("/tmp".into());
    let (buffer, view) = state
        .open_text(TextBuffer::from_text(&format!("{}Z", "x".repeat(50))), None)
        .unwrap();
    state.views[view].scroll_column = 20;
    state.views[view].cursor = ByteOffset(50);
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.wrap = false;
    config.ui.status_line.enabled = false;
    let ui = UiState {
        lsp_inlay_hints: (0..40)
            .map(|at| crate::ui::render::UiInlayHint {
                buffer,
                revision: state.buffers[buffer].text.revision(),
                position: ByteOffset(at),
                label: "h".into(),
            })
            .collect(),
        ..UiState::default()
    };
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 64, 1);
    assert_eq!(frame.cursor.map(|(x, y, _)| (x, y)), Some((42, 0)));
    assert_eq!(frame.grid.get(42, 0).unwrap().symbol, "Z");
    assert_eq!(frame.grid.row_text(0).matches('h').count(), 12);
}

#[test]
fn expanded_diagnostics_share_painted_mouse_rows() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::editor::text::TextBuffer;
    let mut state = EditorState::new("/tmp".into());
    let path = PathBuf::from("/tmp/file");
    let (buffer, view) = state
        .open_text(TextBuffer::from_text("a\nb\nc\nd"), Some(path.clone()))
        .unwrap();
    let mut config = crate::config::load_bundled_config().config;
    config.ui.status_line.enabled = false;
    let mut ui = ui_with_editor_layout(&state);
    ui.diagnostics = vec![crate::ui::render::UiDiagnostic {
        buffer: None,
        path: path.clone(),
        range: ByteRange::empty(ByteOffset(0)),
        severity: crate::workspace::DiagnosticSeverity::Error,
        source: String::new(),
        code: None,
        message: "bad".into(),
    }];
    ui.expanded_diagnostic = Some(crate::ui::render::ExpandedDiagnosticState {
        buffer: None,
        path,
        line: 0,
        severities: vec![crate::workspace::DiagnosticSeverity::Error],
    });
    let model = crate::ui::render::VisualRowModel::new_for_state(
        &state,
        &state.buffers[buffer].text,
        view,
        &ui,
        &config,
        80,
        false,
    );
    let first = crate::ui::render::VisualRowAnchor::default();
    assert_eq!(model.painted_row(first, 1).unwrap().line, 0);
    assert_eq!(model.painted_row(first, 3).unwrap().line, 1);
    assert_eq!(
        model.screen_row(
            first,
            crate::ui::render::VisualRowAnchor {
                line: 1,
                wrap_row: 0
            },
            10
        ),
        Some(3)
    );
    let frame = crate::ui::render::editor::render_editor_view(&state, view, &ui, &config, 80, 6);
    assert!(frame.grid.row_text(3).contains('b'));
}

#[test]
fn debugger_markers_survive_disabled_git_signs() {
    let mut state = EditorState::new("/tmp".into());
    let path = PathBuf::from("/tmp/file");
    let (_, view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text("line"),
            Some(path.clone()),
        )
        .unwrap();
    let mut config = crate::config::load_bundled_config().config;
    config.editor.git_signs.enabled = false;
    let mut ui = ui_with_editor_layout(&state);
    ui.debug_signs = vec![crate::ui::render::DebugSign {
        path,
        line: 0,
        kind: crate::ui::render::DebugSignKind::Breakpoint,
    }];
    let frame = crate::ui::render::editor::render_editor_view(&state, view, &ui, &config, 40, 4);
    assert!(frame.grid.row_text(0).contains('●'));
    ui.debug_signs[0].kind = crate::ui::render::DebugSignKind::DisabledBreakpoint;
    let frame = crate::ui::render::editor::render_editor_view(&state, view, &ui, &config, 40, 4);
    assert!(frame.grid.row_text(0).contains('○'));
    assert!(!frame.grid.row_text(0).contains('●'));
}

#[test]
fn floating_editor_uses_its_whole_body_below_the_screen_statusline() {
    let mut state = EditorState::new("/tmp".into());
    let (_, view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text("first\nsecond"),
            None,
        )
        .unwrap();
    state.views[view].cursor = crate::editor::text::ByteOffset(6);
    let config = crate::config::load_bundled_config().config;
    let ui = ui_with_editor_layout(&state);
    let binding = ui.editor_window_layout.as_ref().unwrap().bindings[0];
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(40, 10),
        cursor: None,
    };
    let projected = crate::windows::ProjectedCompositorWindow {
        window: binding.window,
        rect: crate::windows::WindowRect {
            x: 1,
            y: 1,
            width: 25,
            height: 2,
        },
        layer: crate::windows::CompositorLayer::Floating,
        z_index: 1,
        focused: true,
        focusable: true,
    };
    assert!(
        crate::ui::render::workspace::render_non_tiled_editor_window(
            &mut frame, &state, &ui, &config, &projected
        )
    );
    assert!(frame.grid.row_text(2).contains("second"));
    assert_eq!(frame.cursor.map(|(_, y, _)| y), Some(2));
}

#[test]
fn insert_selection_highlighting_is_half_open_while_visual_selection_is_inclusive() {
    use crate::editor::EditorMode;
    use crate::editor::Selection;
    use crate::editor::SelectionKind;
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    let mut state = EditorState::new("/workspace".into());
    let (_, view) = state.open_text(TextBuffer::from_text("abc"), None).unwrap();
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    for (anchor, head) in [(0, 2), (2, 0)] {
        let selection = Some(Selection {
            anchor: ByteOffset(anchor),
            head: ByteOffset(head),
            kind: SelectionKind::Character,
        });
        for (mode, expected) in [
            (EditorMode::Insert, [true, true, false]),
            (EditorMode::VisualChar, [true, true, true]),
        ] {
            state.views[view].selection = selection;
            state.views[view].mode = mode;
            let frame = crate::ui::render::render_editor_view(
                &state,
                view,
                &UiState::default(),
                &config,
                8,
                2,
            );
            let selected = std::array::from_fn(|offset| {
                frame.grid.get(offset as u16, 0).unwrap().style == crate::ui::Style::Selection
            });
            assert_eq!(selected, expected);
        }
    }
}

#[test]
fn visual_selections_keep_syntax_and_semantic_colors() {
    use crate::config::Highlight;
    use crate::editor::text::{ByteOffset, ByteRange, TextBuffer};
    use crate::editor::{EditorMode, Selection, SelectionKind};
    use crate::ui::Style;
    let mut state = EditorState::new("/workspace".into());
    let (buffer, view) = state
        .open_text(TextBuffer::from_text("let value = 21;"), None)
        .unwrap();
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.git_signs.enabled = false;
    config.ui.status_line.enabled = false;
    let revision = state.buffers[buffer].text.revision();
    let ui = UiState {
        syntax_spans: vec![crate::ui::render::UiSyntaxSpan {
            buffer,
            revision,
            range: ByteRange::new(ByteOffset(0), ByteOffset(3)).unwrap(),
            role: Highlight::KEYWORD,
        }],
        lsp_semantic_spans: vec![crate::ui::render::UiSyntaxSpan {
            buffer,
            revision,
            range: ByteRange::new(ByteOffset(4), ByteOffset(9)).unwrap(),
            role: Highlight::STRING,
        }],
        ..UiState::default()
    };
    for (mode, kind) in [
        (EditorMode::VisualChar, SelectionKind::Character),
        (EditorMode::VisualLine, SelectionKind::Line),
        (EditorMode::VisualBlock, SelectionKind::Block),
    ] {
        state.views[view].mode = mode;
        state.views[view].selection = Some(Selection {
            anchor: ByteOffset(0),
            head: ByteOffset(13),
            kind,
        });
        let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 30, 3);
        assert_eq!(
            frame.grid.get(0, 0).unwrap().style,
            Style::SelectionSyntax(Highlight::KEYWORD)
        );
        assert_eq!(frame.grid.get(3, 0).unwrap().style, Style::Selection);
        assert_eq!(
            frame.grid.get(4, 0).unwrap().style,
            Style::SelectionSyntax(Highlight::STRING)
        );
    }
}

#[test]
fn concealed_markdown_preserves_source_tab_columns() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::editor::text::TextBuffer;
    use crate::language::syntax::MarkdownMark;
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, view) = state
        .open_text(TextBuffer::from_text("*a*\tb\nlast"), None)
        .unwrap();
    state.views[view].cursor = ByteOffset(6);
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.tab_width = 4;
    config.ui.status_line.enabled = false;
    let ui = UiState {
        markdown_marks: [0, 2]
            .into_iter()
            .map(|at| crate::ui::render::UiMarkdownMark {
                buffer,
                revision: state.buffers[buffer].text.revision(),
                range: ByteRange::new(ByteOffset(at), ByteOffset(at + 1)).unwrap(),
                mark: MarkdownMark::Hide,
            })
            .collect(),
        ..Default::default()
    };
    for wrap in [true, false] {
        config.editor.wrap = wrap;
        let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 4, 2);
        assert_eq!(frame.grid.row_text(0).trim_end(), "a b");
    }
}

#[test]
fn markdown_draws_in_place_except_on_the_cursor_line_and_outside_normal_mode() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::language::syntax::MarkdownMark;
    let text = "# Title\n- item [x](u)\nlast";
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text(text), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.git_signs.enabled = false;
    config.ui.status_line.enabled = false;
    state.views[view].cursor = ByteOffset(text.find("last").expect("last line"));
    let revision = state.buffers[buffer].text.revision();
    let mark = |start: &str, length: usize, mark| crate::ui::render::UiMarkdownMark {
        buffer,
        revision,
        range: {
            let start = text.find(start).expect(start);
            ByteRange::new(ByteOffset(start), ByteOffset(start + length)).expect("mark range")
        },
        mark,
    };
    let mut ui = ui_with_editor_layout(&state);
    ui.markdown_marks = vec![
        mark("# ", 2, MarkdownMark::Heading(1)),
        mark("# ", 7, MarkdownMark::HeadingLine(1)),
        mark("- ", 1, MarkdownMark::Bullet(0)),
        mark(
            "[x]",
            1,
            MarkdownMark::Link {
                url: Some((
                    text.find("u)").expect("url"),
                    text.find("u)").expect("url") + 1,
                )),
            },
        ),
        mark("](u)", 4, MarkdownMark::Hide),
    ];
    let frame = render(&state, &ui, &config, &loaded.dashboard, 20, 3);
    assert_eq!(frame.grid.row_text(0).trim_end(), "󰲡 Title");
    assert!(matches!(
        frame.grid.get(19, 0).expect("heading bar").style,
        crate::ui::Style::MarkdownHeading(_)
    ));
    assert_eq!(frame.grid.row_text(1).trim_end(), "● item 󰌹 x");
    assert_eq!(frame.grid.row_text(2).trim_end(), "last");

    // A link's icon comes from where it points.
    config
        .editor
        .markdown
        .glyphs
        .links
        .insert("u".into(), "U ".into());
    let frame = render(&state, &ui, &config, &loaded.dashboard, 20, 3);
    assert_eq!(frame.grid.row_text(1).trim_end(), "● item U x");
    config.editor.markdown.glyphs.links.clear();

    // The cursor's line shows its source.
    state.views[view].cursor = ByteOffset(0);
    let frame = render(&state, &ui, &config, &loaded.dashboard, 20, 3);
    assert_eq!(frame.grid.row_text(0).trim_end(), "# Title");
    assert_eq!(frame.grid.row_text(1).trim_end(), "● item 󰌹 x");

    // So does every line in Insert mode.
    state.views[view].mode = crate::editor::EditorMode::Insert;
    let frame = render(&state, &ui, &config, &loaded.dashboard, 20, 3);
    assert_eq!(frame.grid.row_text(1).trim_end(), "- item [x](u)");
}

#[test]
fn markdown_background_fill_leaves_room_for_diagnostics_and_fold_labels() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::editor::text::TextBuffer;
    use crate::language::syntax::MarkdownMark;
    let text = "# Title\n```\nbody\nend\n```\nlast";
    let path = PathBuf::from("/tmp/test.md");
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state
        .open_text(TextBuffer::from_text(text), Some(path.clone()))
        .unwrap();
    state.views[view].cursor = ByteOffset(text.find("last").unwrap());
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let mark = |start, end, mark| crate::ui::render::UiMarkdownMark {
        buffer,
        revision: state.buffers[buffer].text.revision(),
        range: ByteRange::new(ByteOffset(start), ByteOffset(end)).unwrap(),
        mark,
    };
    let ui = UiState {
        markdown_marks: vec![
            mark(0, 2, MarkdownMark::Heading(1)),
            mark(0, 7, MarkdownMark::HeadingLine(1)),
            mark(8, text.find("last").unwrap(), MarkdownMark::Code),
        ],
        diagnostics: vec![crate::ui::render::UiDiagnostic {
            buffer: None,
            path,
            range: ByteRange::empty(ByteOffset(0)),
            severity: crate::workspace::DiagnosticSeverity::Warning,
            source: String::new(),
            code: None,
            message: "heading warning".into(),
        }],
        syntax_folds: vec![crate::ui::render::UiFold {
            view,
            start_line: 2,
            end_line: 3,
            collapsed: true,
        }],
        ..UiState::default()
    };
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 60, 6);
    assert!(frame.grid.row_text(0).contains("Title  ●● heading warning"));
    assert!(frame.grid.row_text(2).contains("body ⋯ 1 lines"));
    assert!(matches!(
        frame.grid.get(59, 0).unwrap().style,
        crate::ui::Style::MarkdownHeading(_)
    ));
    assert!(matches!(
        frame.grid.get(59, 2).unwrap().style,
        crate::ui::Style::MarkdownCode(_)
    ));
}

#[test]
fn table_tabs_are_single_spaces_and_do_not_overwrite_column_borders() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::editor::text::TextBuffer;
    use crate::language::syntax::MarkdownMark;
    let text = "| a\tb |\n|---|\n| c\td |\nlast";
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state.open_text(TextBuffer::from_text(text), None).unwrap();
    let last = text.find("last").unwrap();
    state.views[view].cursor = ByteOffset(last);
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.tab_width = 8;
    config.ui.status_line.enabled = false;
    let mark = |start, end, mark| crate::ui::render::UiMarkdownMark {
        buffer,
        revision: state.buffers[buffer].text.revision(),
        range: ByteRange::new(ByteOffset(start), ByteOffset(end)).unwrap(),
        mark,
    };
    let mut ui = UiState {
        markdown_marks: text
            .match_indices('|')
            .map(|(at, _)| mark(at, at + 1, MarkdownMark::TablePipe))
            .collect(),
        ..UiState::default()
    };
    ui.markdown_marks.push(mark(0, last, MarkdownMark::Table));
    ui.markdown_marks.sort_by_key(|mark| mark.range.start);
    for wrap in [false, true] {
        config.editor.wrap = wrap;
        for width in [7, 20] {
            let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, width, 6);
            let rows = (0..6)
                .map(|row| frame.grid.row_text(row).trim_end().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                rows,
                [
                    "┌─────┐",
                    "│ a b │",
                    "├─────┤",
                    "│ c d │",
                    "└─────┘",
                    "last"
                ],
                "wrap {wrap}, width {width}"
            );
        }
    }
}

#[test]
fn a_table_too_wide_for_its_window_wraps_each_cell_inside_its_column_with_or_without_wrap() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::language::syntax::MarkdownMark;
    let text = "\n| a | long text here and more |\n|---|---|\n| b | c |\n\nlast";
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text(text), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.git_signs.enabled = false;
    config.ui.status_line.enabled = false;
    state.views[view].cursor = ByteOffset(text.find("last").expect("last line"));
    let revision = state.buffers[buffer].text.revision();
    let mark = |start: usize, end: usize, mark| crate::ui::render::UiMarkdownMark {
        buffer,
        revision,
        range: ByteRange::new(ByteOffset(start), ByteOffset(end)).expect("mark range"),
        mark,
    };
    let mut ui = ui_with_editor_layout(&state);
    ui.markdown_marks = text
        .match_indices('|')
        .map(|(at, _)| mark(at, at + 1, MarkdownMark::TablePipe))
        .collect();
    let table_end = text.find("\n\nlast").expect("table end") + 1;
    ui.markdown_marks
        .push(mark(1, table_end, MarkdownMark::Table));
    ui.markdown_marks.sort_by_key(|mark| mark.range.start);
    // Neovim draws tables to fit the window even when lines don't wrap.
    for wrap in [true, false] {
        config.editor.wrap = wrap;
        let frame = render(&state, &ui, &config, &loaded.dashboard, 20, 8);
        let rows = (0..8)
            .map(|row| frame.grid.row_text(row).trim_end().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            [
                "┌───┬──────────────┐",
                "│ a │ long text    │",
                "│   │ here and     │",
                "│   │ more         │",
                "├───┼──────────────┤",
                "│ b │ c            │",
                "└───┴──────────────┘",
                "last",
            ],
            "wrap {wrap}"
        );
    }
}

#[test]
fn a_table_without_blank_lines_around_it_is_still_closed_and_its_borders_follow_settings() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::language::syntax::MarkdownMark;
    let text = "| a | b |\n|---|---|\n| c | d |\n| e | f |\nlast";
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text(text), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.git_signs.enabled = false;
    config.ui.status_line.enabled = false;
    let last = text.find("last").expect("last line");
    state.views[view].cursor = ByteOffset(last);
    let revision = state.buffers[buffer].text.revision();
    let mark = |start: usize, end: usize, mark| crate::ui::render::UiMarkdownMark {
        buffer,
        revision,
        range: ByteRange::new(ByteOffset(start), ByteOffset(end)).expect("mark range"),
        mark,
    };
    let mut ui = ui_with_editor_layout(&state);
    ui.markdown_marks = text
        .match_indices('|')
        .map(|(at, _)| mark(at, at + 1, MarkdownMark::TablePipe))
        .collect();
    ui.markdown_marks.push(mark(0, last, MarkdownMark::Table));
    ui.markdown_marks.sort_by_key(|mark| mark.range.start);
    let rows = |config: &crate::config::Config| {
        let frame = render(&state, &ui, config, &loaded.dashboard, 20, 8);
        (0..8)
            .map(|row| frame.grid.row_text(row).trim_end().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        rows(&config),
        [
            "┌───┬───┐",
            "│ a │ b │",
            "├───┼───┤",
            "│ c │ d │",
            "│ e │ f │",
            "└───┴───┘",
            "last",
            "~"
        ]
    );
    config.editor.markdown.table_row_lines = true;
    config.editor.markdown.table_bottom = false;
    assert_eq!(
        rows(&config),
        [
            "┌───┬───┐",
            "│ a │ b │",
            "├───┼───┤",
            "│ c │ d │",
            "├───┼───┤",
            "│ e │ f │",
            "last",
            "~"
        ]
    );
}

#[test]
fn a_table_at_the_end_of_a_file_with_a_final_newline_is_closed() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::language::syntax::MarkdownMark;
    let text = "x\n| a | b |\n|---|---|\n| c | d |\n";
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text(text), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.git_signs.enabled = false;
    config.ui.status_line.enabled = false;
    state.views[view].cursor = ByteOffset(0);
    let revision = state.buffers[buffer].text.revision();
    let mark = |start: usize, end: usize, mark| crate::ui::render::UiMarkdownMark {
        buffer,
        revision,
        range: ByteRange::new(ByteOffset(start), ByteOffset(end)).expect("mark range"),
        mark,
    };
    let mut ui = ui_with_editor_layout(&state);
    ui.markdown_marks = text
        .match_indices('|')
        .map(|(at, _)| mark(at, at + 1, MarkdownMark::TablePipe))
        .collect();
    ui.markdown_marks
        .push(mark(2, text.len(), MarkdownMark::Table));
    ui.markdown_marks.sort_by_key(|mark| mark.range.start);
    let frame = render(&state, &ui, &config, &loaded.dashboard, 20, 7);
    let rows = (0..7)
        .map(|row| frame.grid.row_text(row).trim_end().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            "x",
            "┌───┬───┐",
            "│ a │ b │",
            "├───┼───┤",
            "│ c │ d │",
            "└───┴───┘",
            "~"
        ]
    );
}

#[test]
fn a_table_is_drawn_with_its_columns_lined_up_however_its_source_is_aligned() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::language::syntax::MarkdownMark;
    // Unaligned, the second row without its outer pipes and the last one short of a cell.
    let text = "\n| `a` | long head |\n|---|--:|\nwide cell | b\n| c |\n\nlast";
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text(text), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.git_signs.enabled = false;
    config.ui.status_line.enabled = false;
    state.views[view].cursor = ByteOffset(text.find("last").expect("last line"));
    let revision = state.buffers[buffer].text.revision();
    let mark = |start: usize, end: usize, mark| crate::ui::render::UiMarkdownMark {
        buffer,
        revision,
        range: ByteRange::new(ByteOffset(start), ByteOffset(end)).expect("mark range"),
        mark,
    };
    let table_end = text.find("\n\nlast").expect("table end") + 1;
    let mut ui = ui_with_editor_layout(&state);
    ui.markdown_marks = text
        .match_indices('|')
        .map(|(at, _)| mark(at, at + 1, MarkdownMark::TablePipe))
        .collect();
    let code = text.find('`').expect("code");
    ui.markdown_marks.extend([
        mark(1, table_end, MarkdownMark::Table),
        mark(code, code + 1, MarkdownMark::Hide),
        mark(code, code + 3, MarkdownMark::InlineCode),
        mark(code + 2, code + 3, MarkdownMark::Hide),
    ]);
    ui.markdown_marks.sort_by_key(|mark| mark.range.start);
    let frame = render(&state, &ui, &config, &loaded.dashboard, 30, 8);
    let rows = (0..6)
        .map(|row| frame.grid.row_text(row).trim_end().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            "┌───────────┬───────────┐",
            "│ a         │ long head │",
            "├───────────┼──────────━┤",
            "│ wide cell │         b │",
            "│ c         │           │",
            "└───────────┴───────────┘",
        ]
    );
    assert!(matches!(
        frame.grid.get(2, 1).expect("code").style,
        crate::ui::Style::MarkdownCode(_)
    ));
    // The cursor's row shows its source and the others stay lined up.
    state.views[view].cursor = ByteOffset(text.find("wide").expect("row"));
    let frame = render(&state, &ui, &config, &loaded.dashboard, 30, 8);
    assert_eq!(frame.grid.row_text(3).trim_end(), "wide cell | b");
    assert_eq!(
        frame.grid.row_text(4).trim_end(),
        "│ c         │           │"
    );
}

#[test]
fn inlay_budgets_include_visible_replacements_for_hints_and_previews() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    let mut state = EditorState::new("/tmp".into());
    let (buffer, view) = state.open_text(TextBuffer::from_text("x"), None).unwrap();
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let mut ui = ui_with_editor_layout(&state);
    for preview in [false, true] {
        for ambiguous in [
            crate::config::AmbiguousWidth::Narrow,
            crate::config::AmbiguousWidth::Wide,
        ] {
            config.editor.ambiguous_width = ambiguous;
            let hint = crate::ui::render::UiInlayHint {
                buffer,
                revision: state.buffers[buffer].text.revision(),
                position: ByteOffset(0),
                label: "a\u{200b}\tb".into(),
            };
            ui.lsp_inlay_hints.clear();
            ui.substitute_preview.clear();
            if preview {
                ui.substitute_preview.push(hint);
            } else {
                ui.lsp_inlay_hints.push(hint);
            }
            for wrap in [false, true] {
                config.editor.wrap = wrap;
                let mut expected = crate::ui::CellGrid::new_with_policy(
                    32,
                    1,
                    crate::ui::grid::CONTROL_CHARACTER_REPLACEMENT,
                    crate::ui::grid::ZERO_WIDTH_REPLACEMENT,
                    ambiguous == crate::config::AmbiguousWidth::Wide,
                );
                let end = expected.put_text(0, 0, "a\u{200b}\tb", crate::ui::Style::Muted);
                expected.put_text(end, 0, "x", crate::ui::Style::Default);
                let frame = crate::ui::render::editor::render_editor_view(
                    &state, view, &ui, &config, 32, 1,
                );
                assert_eq!(frame.grid.row_text(0), expected.row_text(0));
                assert_eq!(frame.cursor.map(|(x, y, _)| (x, y)), Some((end, 0)));
            }
        }
    }
}

#[test]
fn wrapped_inlays_reserve_only_the_following_tabs_actual_source_width() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    let mut state = EditorState::new("/tmp".into());
    let (buffer, view) = state
        .open_text(TextBuffer::from_text("aaa\tz"), None)
        .unwrap();
    state.views[view].cursor = ByteOffset(3);
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.tab_width = 4;
    config.editor.wrap = true;
    config.ui.status_line.enabled = false;
    let mut ui = ui_with_editor_layout(&state);
    ui.lsp_inlay_hints.push(crate::ui::render::UiInlayHint {
        buffer,
        revision: state.buffers[buffer].text.revision(),
        position: ByteOffset(3),
        label: "123456".into(),
    });
    let frame = crate::ui::render::editor::render_editor_view(&state, view, &ui, &config, 8, 2);
    assert_eq!(frame.grid.row_text(0).trim_end(), "aaa");
    assert_eq!(frame.grid.row_text(1), "123456 z");
    assert_eq!(frame.cursor.map(|(x, y, _)| (x, y)), Some((6, 1)));
}

#[test]
fn expanded_diagnostics_share_painted_mouse_rows_slot5() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::editor::text::TextBuffer;
    let mut state = EditorState::new("/tmp".into());
    let path = PathBuf::from("/tmp/file");
    let (buffer, view) = state
        .open_text(TextBuffer::from_text("a\nb\nc\nd"), Some(path.clone()))
        .unwrap();
    let mut config = crate::config::load_bundled_config().config;
    config.ui.status_line.enabled = false;
    let mut ui = ui_with_editor_layout(&state);
    ui.diagnostics = vec![crate::ui::render::UiDiagnostic {
        buffer: None,
        path: path.clone(),
        range: ByteRange::empty(ByteOffset(0)),
        severity: crate::workspace::DiagnosticSeverity::Error,
        source: String::new(),
        code: None,
        message: "bad".into(),
    }];
    ui.expanded_diagnostic = Some(crate::ui::render::ExpandedDiagnosticState {
        buffer: None,
        path,
        line: 0,
        severities: vec![crate::workspace::DiagnosticSeverity::Error],
    });
    let model = crate::ui::render::VisualRowModel::new_for_state(
        &state,
        &state.buffers[buffer].text,
        view,
        &ui,
        &config,
        80,
        false,
    );
    let first = crate::ui::render::VisualRowAnchor::default();
    assert_eq!(model.painted_row(first, 1).unwrap().line, 0);
    assert_eq!(model.painted_row(first, 3).unwrap().line, 1);
    let second = crate::ui::render::VisualRowAnchor {
        line: 1,
        wrap_row: 0,
    };
    assert_eq!(model.forward(first, 1), first);
    assert_eq!(model.forward(first, 3), second);
    assert_eq!(model.backward(second, 1), second);
    assert_eq!(model.backward(second, 3), first);
    assert_eq!(
        model.screen_row(
            first,
            crate::ui::render::VisualRowAnchor {
                line: 1,
                wrap_row: 0
            },
            10
        ),
        Some(3)
    );
    let frame = crate::ui::render::editor::render_editor_view(&state, view, &ui, &config, 80, 6);
    assert!(frame.grid.row_text(3).contains('b'));
}

#[test]
fn a_table_too_wide_for_its_window_wraps_each_cell_inside_its_column_with_or_without_wrap_slot5() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::language::syntax::MarkdownMark;
    let text = "\n| a | long text here and more |\n|---|---|\n| b | c |\n\nlast";
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text(text), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.git_signs.enabled = false;
    config.ui.status_line.enabled = false;
    state.views[view].cursor = ByteOffset(text.find("last").expect("last line"));
    let revision = state.buffers[buffer].text.revision();
    let mark = |start: usize, end: usize, mark| crate::ui::render::UiMarkdownMark {
        buffer,
        revision,
        range: ByteRange::new(ByteOffset(start), ByteOffset(end)).expect("mark range"),
        mark,
    };
    let mut ui = ui_with_editor_layout(&state);
    ui.markdown_marks = text
        .match_indices('|')
        .map(|(at, _)| mark(at, at + 1, MarkdownMark::TablePipe))
        .collect();
    let table_end = text.find("\n\nlast").expect("table end") + 1;
    ui.markdown_marks
        .push(mark(1, table_end, MarkdownMark::Table));
    ui.markdown_marks.sort_by_key(|mark| mark.range.start);
    // Neovim draws tables to fit the window even when lines don't wrap.
    for wrap in [true, false] {
        config.editor.wrap = wrap;
        let model = crate::ui::render::VisualRowModel::new_for_state(
            &state,
            &state.buffers[buffer].text,
            view,
            &ui,
            &config,
            20,
            wrap,
        );
        for (wrap_row, word) in [(0, "long"), (1, "here"), (2, "more")] {
            let relative =
                model.byte_at_cell(crate::ui::render::VisualRowAnchor { line: 1, wrap_row }, 6);
            assert_eq!(&text[1 + relative..1 + relative + word.len()], word);
        }
        let frame = render(&state, &ui, &config, &loaded.dashboard, 20, 8);
        let rows = (0..8)
            .map(|row| frame.grid.row_text(row).trim_end().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            [
                "┌───┬──────────────┐",
                "│ a │ long text    │",
                "│   │ here and     │",
                "│   │ more         │",
                "├───┼──────────────┤",
                "│ b │ c            │",
                "└───┴──────────────┘",
                "last",
            ],
            "wrap {wrap}"
        );
    }
}
