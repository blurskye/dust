//! Composition tests for editor surfaces and frame composition.

use crate::editor::EditorState;
use crate::ui::grid::CONTROL_CHARACTER_REPLACEMENT;
use crate::ui::render::EditorWindowBinding;
use crate::ui::render::EditorWindowLayoutState;
use crate::ui::render::PromptKind;
use crate::ui::render::PromptState;
use crate::ui::render::UiState;
use crate::ui::render::overlays::BUFFER_OVERFLOW_LEFT;
use crate::windows::PANE_MINIMUM_HEIGHT;
use crate::windows::PANE_MINIMUM_WIDTH;
use std::path::PathBuf;

pub(super) fn editor_layout(state: &EditorState) -> EditorWindowLayoutState {
    let tab = state.active_tab().expect("active tab");
    EditorWindowLayoutState {
        source_revision: state.window_revision().expect("test revision"),
        layout: crate::windows::TiledLayoutSnapshot {
            root: state.tab_layout(tab).unwrap(),
            focused: state.focused_window(),
        },
        bindings: state
            .tab_windows(tab)
            .into_iter()
            .map(|window| EditorWindowBinding {
                window,
                view: state.window_view(window).unwrap(),
            })
            .collect(),
    }
}

pub(super) fn ui_with_editor_layout(state: &EditorState) -> UiState {
    UiState {
        editor_window_layout: Some(editor_layout(state)),
        ..UiState::default()
    }
}

pub(super) fn attach_test_compositor(
    state: &EditorState,
    ui: &mut UiState,
    config: &crate::config::Config,
    width: u16,
    height: u16,
) {
    let layout = ui.editor_window_layout.as_ref();
    let mut windows = layout
        .into_iter()
        .flat_map(|layout| &layout.bindings)
        .map(|binding| crate::windows::CompositorWindowSnapshot {
            window: binding.window,
            placement: crate::windows::WindowPlacement::Tiled,
            adapter: crate::windows::ContentAdapterKind::TextBuffer,
            focusable: true,
        })
        .collect::<Vec<_>>();
    let explorer_window = ui
        .explorer
        .as_ref()
        .map(|_| crate::windows::WindowId(10_001));
    if let Some(window) = explorer_window {
        let size = crate::ui::render::explorer_panel_width(config, width)
            .map_or(1, |panel| panel.saturating_add(1));
        windows.push(crate::windows::CompositorWindowSnapshot {
            window,
            placement: crate::windows::WindowPlacement::Docked {
                edge: crate::windows::DockEdge::Left,
                size,
            },
            adapter: crate::windows::ContentAdapterKind::DeclarativeUi,
            focusable: true,
        });
    }
    let terminal_window = ui
        .terminal_panel
        .as_ref()
        .map(|terminal| (terminal.id, crate::windows::WindowId(10_002)));
    if let Some((_, window)) = terminal_window {
        let size = crate::ui::render::terminal_panel_geometry(
            height,
            ui.terminal_panel
                .as_ref()
                .expect("terminal")
                .requested_height,
        )
        .map_or(1, |geometry| geometry.height);
        windows.push(crate::windows::CompositorWindowSnapshot {
            window,
            placement: crate::windows::WindowPlacement::Docked {
                edge: crate::windows::DockEdge::Bottom,
                size,
            },
            adapter: crate::windows::ContentAdapterKind::TerminalSession,
            focusable: true,
        });
    }
    let dashboard_window = ui.dashboard.then_some(crate::windows::WindowId(10_003));
    if let Some(window) = dashboard_window {
        windows.push(crate::windows::CompositorWindowSnapshot {
            window,
            placement: crate::windows::WindowPlacement::Overlay(crate::windows::WindowRect {
                x: 0,
                y: 0,
                width,
                height,
            }),
            adapter: crate::windows::ContentAdapterKind::DeclarativeUi,
            focusable: true,
        });
    }
    let picker_window = ui
        .picker
        .as_ref()
        .and_then(|picker| {
            crate::ui::render::picker::picker_window_geometry(
                picker,
                crate::windows::WindowRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
            )
        })
        .map(|rect| (crate::windows::WindowId(10_004), rect));
    if let Some((window, rect)) = picker_window {
        windows.push(crate::windows::CompositorWindowSnapshot {
            window,
            placement: crate::windows::WindowPlacement::Overlay(rect),
            adapter: crate::windows::ContentAdapterKind::DeclarativeUi,
            focusable: true,
        });
    }
    let focused = if let Some((window, _)) = picker_window {
        Some(window)
    } else if ui
        .terminal_panel
        .as_ref()
        .is_some_and(|terminal| terminal.focused)
    {
        terminal_window.map(|(_, window)| window)
    } else if ui.explorer_focused {
        explorer_window
    } else {
        layout.and_then(|layout| layout.layout.focused)
    };
    let snapshot = crate::windows::CompositorSnapshot {
        tab: crate::windows::WorkspaceTabId(1),
        tiled: layout.map(|layout| layout.layout.clone()),
        windows,
        focused,
    };
    let bounds = crate::windows::WindowRect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let top = crate::ui::render::workspace_top_rows(state, ui, config);
    let screen_bottom = crate::ui::render::screen_foot_rows(state, ui, config, height);
    let preliminary = snapshot.project_with_tiled_insets(
        bounds,
        PANE_MINIMUM_WIDTH,
        PANE_MINIMUM_HEIGHT,
        crate::windows::TiledInsets {
            screen_bottom,
            ..Default::default()
        },
    );
    let (left, right, bottom) = ui
        .debug_panes
        .as_ref()
        .filter(|_| !ui.dashboard && state.active_view().is_some())
        .and_then(|_| {
            crate::ui::render::debug_pane_insets(
                preliminary.workspace_bounds.width,
                preliminary.workspace_bounds.height.saturating_sub(top),
            )
        })
        .unwrap_or_default();
    ui.window_graph_revision = Some(1);
    ui.compositor_layout = Some(crate::ui::render::CompositorLayoutState {
        source_revision: state.window_revision().expect("test revision"),
        graph_revision: 1,
        projection: snapshot.project_with_tiled_insets(
            bounds,
            PANE_MINIMUM_WIDTH,
            PANE_MINIMUM_HEIGHT,
            crate::windows::TiledInsets {
                screen_bottom,
                top,
                left,
                right,
                bottom,
            },
        ),
        surfaces: crate::ui::render::NativeSurfaceWindowBinding {
            explorer: explorer_window,
            terminal: terminal_window,
            dashboard: dashboard_window,
            picker: picker_window.map(|(window, _)| window),
        },
    });
}

pub(super) fn render(
    state: &EditorState,
    ui: &UiState,
    config: &crate::config::Config,
    dashboard: &crate::config::DashboardConfig,
    width: u16,
    height: u16,
) -> crate::ui::grid::Frame {
    let mut ui = ui.clone();
    attach_test_compositor(state, &mut ui, config, width, height);
    crate::ui::render::render(state, &ui, config, dashboard, width, height)
}

#[test]
fn workspace_frame_respects_nonzero_compositor_origin_for_side_and_top_docks() {
    let config = crate::config::Config::default();
    let mut local = crate::ui::CellGrid::new(6, 3);
    local.set_cell(0, 0, "X", crate::ui::Style::Default);
    local.set_cell(5, 2, "Y", crate::ui::Style::Default);
    let frame = crate::ui::render::place_workspace_frame(
        crate::ui::grid::Frame {
            grid: local,
            cursor: Some((1, 1, crossterm::cursor::SetCursorStyle::SteadyBlock)),
        },
        crate::windows::WindowRect {
            x: 7,
            y: 4,
            width: 6,
            height: 3,
        },
        &config,
        20,
        10,
    );
    assert_eq!(frame.grid.get(7, 4).expect("offset first").symbol, "X");
    assert_eq!(frame.grid.get(12, 6).expect("offset last").symbol, "Y");
    assert_eq!(frame.grid.get(0, 0).expect("outside").symbol, " ");
    assert!(frame.cursor.is_some_and(|(x, y, _)| (x, y) == (8, 5)));

    let terminal = crate::ui::render::TerminalPanelState {
        id: 1,
        profile: "side".into(),
        tabs: vec![crate::ui::render::TerminalTabState {
            phase: crate::ui::render::TerminalPhase::Running,
            id: 1,
            profile: "side".into(),
            running: true,
            exit_code: None,
        }],
        mode: crate::ui::render::TerminalPanelMode::Input,
        running: true,
        exit_code: None,
        truncated: false,
        focused: false,
        columns: 5,
        rows: 8,
        cells: Vec::new(),
        cells_start: 0,
        cursor: None,
        selection: None,
        scrollback_lines: 0,
        scroll_offset: 0,
        requested_height: 5,
    };
    let composed = crate::ui::render::dock_terminal_panel(
        frame,
        &terminal,
        crate::ui::render::TerminalPanelGeometry {
            x: 0,
            width: 5,
            left_border: false,
            top: 0,
            height: 10,
            body_top: 1,
            body_height: 9,
        },
        &config,
        20,
        10,
    );
    assert_eq!(
        composed.grid.get(0, 0).expect("side terminal").style,
        crate::ui::Style::TerminalBorder
    );
    assert_eq!(
        composed.grid.get(7, 4).expect("workspace retained").symbol,
        "X"
    );
}

#[test]
fn ordinary_confirmation_wraps_unicode_and_uses_safe_default_geometry() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    let loaded = crate::config::load_bundled_config();
    let mut prompt = PromptState::new(PromptKind::ExplorerDelete);
    prompt.confirmation = Some(crate::ui::ConfirmationState {
        title: "Delete this path?".into(),
        message: "Permanently  delete `長い/界界界界界界/target.rs`?".into(),
        selected: crate::ui::ConfirmationChoice::No,
        positive_label: "Yes".into(),
        negative_label: "No".into(),
        hint: "Left/Right select · Enter confirm · Escape cancel".into(),
    });
    let geometry = crate::ui::prompt_geometry_for_state(30, 12, &loaded.config, &prompt);
    assert!(geometry.height > 4, "message must wrap in narrow geometry");
    let frame = render(
        &state,
        &UiState {
            dashboard: true,
            prompt: Some(prompt),
            ..UiState::default()
        },
        &loaded.config,
        &loaded.dashboard,
        30,
        12,
    );
    let rows = (geometry.top..geometry.top + geometry.height)
        .map(|row| frame.grid.row_text(row))
        .collect::<Vec<_>>();
    assert!(rows.iter().any(|row| row.contains("Permanently  delete")));
    assert!(rows.iter().any(|row| row.contains("[ Yes ]")));
    assert!(rows.iter().any(|row| row.contains("[ No ]")));
    let button_y = geometry.top + geometry.height - 2;
    let no_x = frame.grid.row_text(button_y).find("[ No ]").unwrap() as u16;
    assert_eq!(
        frame.grid.get(no_x, button_y).expect("selected No").style,
        crate::ui::Style::Selection
    );
    assert!(frame.cursor.is_none());
}

#[test]
fn confirmation_uses_custom_action_labels_for_geometry_and_paint() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    let loaded = crate::config::load_bundled_config();
    let mut prompt = PromptState::new(PromptKind::TerminalClose);
    prompt.confirmation = Some(crate::ui::ConfirmationState {
        title: "Terminal #7 is running".into(),
        message: "Hide this terminal or stop its process?".into(),
        selected: crate::ui::ConfirmationChoice::No,
        positive_label: "Stop".into(),
        negative_label: "Hide".into(),
        hint: "Enter confirm · Escape cancel".into(),
    });
    assert!(crate::ui::confirmation_fully_visible(
        40,
        8,
        &loaded.config,
        &prompt
    ));
    let frame = render(
        &state,
        &UiState {
            dashboard: true,
            prompt: Some(prompt),
            ..UiState::default()
        },
        &loaded.config,
        &loaded.dashboard,
        40,
        8,
    );
    let screen = (0..8)
        .map(|row| frame.grid.row_text(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("[ Stop ]"));
    assert!(screen.contains("[ Hide ]"));
    assert!(!screen.contains("[ Yes ]"));
}

#[test]
fn confirmation_visibility_uses_render_workspace_and_button_width() {
    let mut state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    state
        .open_text(crate::editor::text::TextBuffer::from_text("x"), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut prompt = PromptState::new(PromptKind::PluginConfirmation);
    prompt.confirmation = Some(crate::ui::ConfirmationState {
        title: "Boundary".into(),
        message: "/tmp/a".into(),
        selected: crate::ui::ConfirmationChoice::Yes,
        positive_label: "Yes".into(),
        negative_label: "No".into(),
        hint: "Enter confirm".into(),
    });
    assert!(crate::ui::confirmation_fully_visible(
        100,
        4,
        &loaded.config,
        &prompt
    ));
    let ui = UiState {
        dashboard: false,
        editor_window_layout: Some(editor_layout(&state)),
        prompt: Some(prompt.clone()),
        ..UiState::default()
    };
    let bounds = (100, 3);
    assert!(!crate::ui::confirmation_fully_visible(
        bounds.0,
        bounds.1,
        &loaded.config,
        &prompt
    ));
    assert!(!crate::ui::confirmation_fully_visible(
        1,
        10,
        &loaded.config,
        &prompt
    ));
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 3);
    let screen = (0..3)
        .map(|row| frame.grid.row_text(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("Resize to review"));
    assert!(!screen.contains("[ Yes ]"));
}

#[test]
fn confirmation_fails_closed_when_the_full_target_cannot_be_shown() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    let loaded = crate::config::load_bundled_config();
    let mut prompt = PromptState::new(PromptKind::ExplorerDelete);
    prompt.confirmation = Some(crate::ui::ConfirmationState {
        title: "Delete this path?".into(),
        message: format!("target/{}", "界  ".repeat(80)),
        selected: crate::ui::ConfirmationChoice::Yes,
        positive_label: "Yes".into(),
        negative_label: "No".into(),
        hint: "Enter confirm".into(),
    });
    assert!(!crate::ui::confirmation_fully_visible(
        20,
        6,
        &loaded.config,
        &prompt
    ));
    let geometry = crate::ui::prompt_geometry_for_state(20, 6, &loaded.config, &prompt);
    assert_eq!(geometry.height, 6);
    let frame = render(
        &state,
        &UiState {
            dashboard: true,
            prompt: Some(prompt),
            ..UiState::default()
        },
        &loaded.config,
        &loaded.dashboard,
        20,
        6,
    );
    let screen = (0..6)
        .map(|row| frame.grid.row_text(row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("Resize"), "{screen:?}");
    assert!(!screen.contains("[ Yes ]"));
    assert!(!screen.contains("[ No ]"));

    let mut tall = PromptState::new(PromptKind::PluginConfirmation);
    tall.confirmation = Some(crate::ui::ConfirmationState {
        title: "Tall".into(),
        message: (0..400).map(|_| "target\n").collect(),
        selected: crate::ui::ConfirmationChoice::No,
        positive_label: "Yes".into(),
        negative_label: "No".into(),
        hint: String::new(),
    });
    assert_eq!(
        crate::ui::prompt_geometry_for_state(20, 300, &loaded.config, &tall).height,
        300
    );
}

#[test]
fn visual_line_selection_paints_empty_lines() {
    use crate::editor::Selection;
    use crate::editor::SelectionKind;
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state
        .open_text(TextBuffer::from_text("a\n\nb"), None)
        .unwrap();
    state.views[view].mode = crate::editor::EditorMode::VisualLine;
    state.views[view].selection = Some(Selection {
        anchor: ByteOffset(0),
        head: ByteOffset(3),
        kind: SelectionKind::Line,
    });
    state.views[view].cursor = ByteOffset(3);
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let frame =
        crate::ui::render::render_editor_view(&state, view, &UiState::default(), &config, 12, 3);
    assert_eq!(
        frame.grid.get(0, 1).unwrap().style,
        crate::ui::Style::Selection
    );
    assert_eq!(
        frame.grid.get(1, 1).unwrap().style,
        crate::ui::Style::Default
    );
}

#[test]
fn search_highlights_preserve_word_boundaries_and_substitution_regex_flags() {
    use crate::editor::text::TextBuffer;
    use crate::ui::Style;
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state
        .open_text(
            TextBuffer::from_text("foo foobar foo\n123 abc ABC\n123 abc ABC"),
            None,
        )
        .unwrap();
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.wrap = false;
    config.ui.status_line.enabled = false;
    let mut ui = UiState {
        search_query: Some("foo".into()),
        search_whole_word: true,
        ..Default::default()
    };
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 32, 3);
    for (column, selected) in [(0, true), (4, false), (11, true)] {
        assert_eq!(
            frame.grid.get(column, 0).unwrap().style == Style::Search,
            selected
        );
    }
    // A freshly typed ordinary search doesn't inherit the previous word-search policy.
    let mut prompt = PromptState::new(PromptKind::SearchForward);
    prompt.input = "foo".into();
    ui.prompt = Some(prompt);
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 32, 3);
    assert_eq!(frame.grid.get(4, 0).unwrap().style, Style::Search);
    for (input, expected) in [
        ("%s/[0-9]+/x/g", [true, false, false]),
        ("%s/abc/x/g", [false, true, false]),
        ("%s/ABC/x/gi", [false, true, true]),
        ("%s/^123/x/g", [true, false, false]),
    ] {
        let mut prompt = PromptState::new(PromptKind::Command);
        prompt.input = input.into();
        ui.prompt = Some(prompt);
        let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 32, 3);
        for row in [1, 2] {
            for (column, selected) in [0, 4, 8].into_iter().zip(expected) {
                assert_eq!(
                    frame.grid.get(column, row).unwrap().style == Style::Search,
                    selected,
                    "{input} row {row} column {column}"
                );
            }
        }
    }
}

#[test]
fn block_selection_projection_uses_shared_display_cell_ranges() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text("\tZ\n界ab"),
            None,
        )
        .unwrap();
    state.views[view].mode = crate::editor::EditorMode::VisualBlock;
    state.views[view].selection = Some(crate::editor::Selection {
        anchor: crate::editor::text::ByteOffset(0),
        head: crate::editor::text::ByteOffset("\tZ\n界ab".len()),
        kind: crate::editor::SelectionKind::Block,
    });
    state.views[view].cursor = crate::editor::text::ByteOffset("\tZ\n界ab".len());
    let loaded = crate::config::load_bundled_config();
    let ui = ui_with_editor_layout(&state);
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 20, 6);
    let gutter = crate::ui::render::editor_state_gutter_width(
        &state,
        &state.buffers[state.views[view].buffer].text,
        &loaded.config.editor,
        20,
    );
    assert_eq!(
        frame.grid.get(gutter + 3, 0).expect("tab cell").style,
        crate::ui::Style::Selection
    );
    assert_eq!(
        frame.grid.get(gutter, 1).expect("wide cell").style,
        crate::ui::Style::Selection
    );
}

#[test]
fn block_selection_on_long_lines_is_resolved_once_per_line() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let text = format!("{}\n0123456789", "x".repeat(500_000));
    let (_, view) = state.open_text(TextBuffer::from_text(&text), None).unwrap();
    state.views[view].mode = crate::editor::EditorMode::VisualBlock;
    state.views[view].selection = Some(crate::editor::Selection {
        anchor: ByteOffset(500_006),
        head: ByteOffset(2),
        kind: crate::editor::SelectionKind::Block,
    });
    state.views[view].cursor = ByteOffset(2);
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.wrap = false;
    config.ui.status_line.enabled = false;
    let ui = UiState::default();
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 80, 2);
    for row in 0..2 {
        for column in 0..80 {
            assert_eq!(
                frame.grid.get(column, row).unwrap().style == crate::ui::Style::Selection,
                (2..=5).contains(&column),
                "row {row}, column {column}"
            );
        }
    }
}

#[test]
fn workspace_split_separators_stay_one_cell_under_wide_ambiguous_width() {
    use crate::windows::TileAxis;

    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.ambiguous_width = crate::config::AmbiguousWidth::Wide;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    for axis in [
        crate::windows::TileAxis::Horizontal,
        crate::windows::TileAxis::Vertical,
    ] {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state
            .open_text(crate::editor::text::TextBuffer::from_text("pane"), None)
            .unwrap();
        state.split_active(axis, 500).unwrap();
        let mut ui = ui_with_editor_layout(&state);
        attach_test_compositor(&state, &mut ui, &config, 60, 10);
        let frame = crate::ui::render::render(&state, &ui, &config, &loaded.dashboard, 60, 10);
        let projection = &ui.compositor_layout.as_ref().unwrap().projection;
        assert!(!projection.borders.is_empty());
        for border in &projection.borders {
            for offset in 0..border.length {
                let (x, y, expected) = match border.axis {
                    TileAxis::Horizontal => (border.x, border.y + offset, "|"),
                    TileAxis::Vertical => (border.x + offset, border.y, "-"),
                };
                let cell = frame.grid.get(x, y).unwrap();
                assert_eq!(cell.symbol, expected);
                assert!(!cell.continuation);
            }
        }
        for pane in crate::ui::render::pane_viewports(&state, &ui, &config, 60, 9) {
            assert_eq!(frame.grid.get(pane.x, pane.y).unwrap().symbol, "p");
            assert!(!frame.grid.get(pane.x, pane.y).unwrap().continuation);
        }
    }
}

#[test]
fn buffer_line_lists_unique_dirty_content_and_follows_active_overflow() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (first, _) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text("first"),
            Some(PathBuf::from("/workspace/src/main.rs")),
        )
        .unwrap();
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("second"),
            Some(PathBuf::from("/workspace/tests/main.rs")),
        )
        .unwrap();
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("third"),
            Some(PathBuf::from("/workspace/very-long-active-buffer-name.rs")),
        )
        .unwrap();
    state.buffers[first].dirty = true;
    let loaded = crate::config::load_bundled_config();
    let config = loaded.config;
    let entries = vec![
        crate::ui::render::ContentTabEntry {
            identity: crate::ui::render::ContentTabIdentity(crate::windows::ContentId(1)),
            kind: crate::ui::render::ContentTabKind::Text,
            title: "src/main.rs ·1".into(),
            active: false,
            dirty: true,
            pinned: false,
            diagnostic: None,
            icon: "R".into(),
            closeable: true,
            segments: None,
        },
        crate::ui::render::ContentTabEntry {
            identity: crate::ui::render::ContentTabIdentity(crate::windows::ContentId(2)),
            kind: crate::ui::render::ContentTabKind::Text,
            title: "tests/main.rs ·2".into(),
            active: false,
            dirty: false,
            pinned: false,
            diagnostic: None,
            icon: "R".into(),
            closeable: true,
            segments: None,
        },
        crate::ui::render::ContentTabEntry {
            identity: crate::ui::render::ContentTabIdentity(crate::windows::ContentId(3)),
            kind: crate::ui::render::ContentTabKind::Text,
            title: "very-long-active-buffer-name.rs".into(),
            active: true,
            dirty: false,
            pinned: false,
            diagnostic: None,
            icon: "R".into(),
            closeable: true,
            segments: None,
        },
    ];
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        content_tabs: entries,
        ..UiState::default()
    };

    let frame = render(&state, &ui, &config, &loaded.dashboard, 40, 7);
    let line = frame.grid.row_text(0);
    assert!(line.contains("buffer-name.rs"), "{line:?}");
    assert!(line.contains('‹'));
    assert!(frame.cursor.is_some_and(|(_, y, _)| y >= 1));

    for width in 1..=5 {
        let narrow = render(&state, &ui, &config, &loaded.dashboard, width, 5);
        assert_eq!(narrow.grid.width(), width);
        assert!(narrow.cursor.is_none_or(|(x, y, _)| x < width && y < 5));
    }
}

#[test]
fn content_line_segment_width_and_hit_testing_share_exact_boundaries() {
    let loaded = crate::config::load_bundled_config();
    let config = loaded.config;
    let entry = crate::ui::render::ContentTabEntry {
        identity: crate::ui::render::ContentTabIdentity(crate::windows::ContentId(9)),
        kind: crate::ui::render::ContentTabKind::Text,
        title: "a.rs".into(),
        active: true,
        dirty: true,
        pinned: true,
        diagnostic: Some(crate::ui::render::ContentTabDiagnostic {
            severity: crate::ui::render::ContentTabDiagnosticSeverity::Error,
            count: 2,
        }),
        icon: "R".into(),
        closeable: true,
        segments: None,
    };
    let width = crate::ui::render::content_tab_width(&entry, &config);
    let mut grid = crate::ui::CellGrid::new(u16::try_from(width).unwrap(), 1);
    crate::ui::render::overlays::render_content_tab_line(
        &mut grid,
        std::slice::from_ref(&entry),
        &config,
        0,
    );
    let styles = (0..grid.width())
        .filter_map(|x| grid.get(x, 0).map(|cell| cell.style))
        .collect::<Vec<_>>();
    for role in [
        crate::ui::Style::BufferLinePin,
        crate::ui::Style::BufferLineIcon,
        crate::ui::Style::BufferLineDirty,
        crate::ui::Style::BufferLineDiagnostic,
        crate::ui::Style::BufferLineClose,
    ] {
        assert!(styles.contains(&role));
    }
    for column in 0..u16::try_from(width).unwrap() {
        assert_eq!(
            crate::ui::render::content_tab_at_column(
                std::slice::from_ref(&entry),
                &config,
                u16::try_from(width).unwrap(),
                column
            ),
            Some(entry.identity)
        );
    }
    assert_eq!(
        crate::ui::render::content_tab_at_column(
            std::slice::from_ref(&entry),
            &config,
            u16::try_from(width).unwrap(),
            u16::try_from(width).unwrap()
        ),
        None
    );
    let mut entries = vec![entry.clone(), entry.clone(), entry.clone()];
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.identity =
            crate::ui::render::ContentTabIdentity(crate::windows::ContentId(index as u64 + 1));
        entry.title = char::from(b'a' + index as u8).to_string();
        entry.active = index == 2;
    }
    let left = crate::ui::geometry::text_width(BUFFER_OVERFLOW_LEFT, config.editor.ambiguous_width);
    for column in 0..u16::try_from(left).unwrap() {
        assert_eq!(
            crate::ui::render::content_tab_at_column(&entries, &config, 8, column),
            None
        )
    }
    let mut right_entries = entries.clone();
    for (index, entry) in right_entries.iter_mut().enumerate() {
        entry.active = index == 0;
        entry.pinned = false;
        entry.diagnostic = None;
        entry.dirty = false;
        entry.closeable = false;
    }
    let first = crate::ui::render::content_tab_width(&right_entries[0], &config);
    assert_eq!(
        crate::ui::render::content_tab_at_column(
            &right_entries,
            &config,
            5,
            u16::try_from(first).unwrap()
        ),
        None
    );
}

#[test]
fn lsp_progress_is_animated_right_aligned_and_hides_transport_identity() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("fn main() {}"),
            Some(PathBuf::from("/workspace/main.rs")),
        )
        .unwrap();
    let mut ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        explorer: Some(crate::ui::render::ExplorerUiState {
            title: "Explorer".into(),
            rows: Vec::new(),
            scroll: 0,
            loading: false,
            total: 0,
            matches: 0,
            query: String::new(),
            searching: false,
        }),
        lsp_progress: vec![
            crate::ui::render::UiLspProgress {
                server_key: "root:1:rust-analyzer".into(),
                token: "roots".into(),
                server: "rust-analyzer".into(),
                title: "Roots Scanned".into(),
                message: String::new(),
                percentage: None,
                done: true,
                completed_at: Some(std::time::Instant::now()),
            },
            crate::ui::render::UiLspProgress {
                server_key: "root:1:rust-analyzer".into(),
                token: "cargo-check".into(),
                server: "rust-analyzer".into(),
                title: "Building".into(),
                message: "ring (lib)".into(),
                percentage: Some(72),
                done: false,
                completed_at: None,
            },
        ],
        lsp_progress_frame: 8,
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 10);
    let panel =
        crate::ui::render::explorer_panel_width(&loaded.config, 80).expect("explorer width");
    let row = (0..10)
        .find(|row| frame.grid.row_text(*row).contains("ring (lib)"))
        .unwrap_or_else(|| {
            panic!(
                "progress row: {:?}",
                (0..10)
                    .map(|row| frame.grid.row_text(row))
                    .collect::<Vec<_>>()
            )
        });
    assert!(!frame.grid.row_text(row).contains("root:1"));
    assert!((panel.saturating_add(1)..79).any(|x| {
        frame
            .grid
            .get(x, row)
            .is_some_and(|cell| cell.symbol == "⠇" && cell.style == crate::ui::Style::Accent)
    }));
    assert_eq!(frame.grid.get(78, row).unwrap().symbol, "r");
    assert_eq!(frame.grid.get(79, row).unwrap().symbol, " ");
    let completed_row = (0..10)
        .find(|row| frame.grid.row_text(*row).contains("Roots Scanned"))
        .expect("completed progress row");
    assert!((panel.saturating_add(1)..79).any(|x| {
        frame
            .grid
            .get(x, completed_row)
            .is_some_and(|cell| cell.symbol == "✓")
    }));

    ui.lsp_progress_frame = 4;
    let next = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 10);
    assert!(
        (panel.saturating_add(1)..79)
            .any(|x| next.grid.get(x, row).is_some_and(|cell| cell.symbol == "⠼"))
    );
}

#[test]
fn visual_row_model_is_reversible_fold_aware_and_unicode_width_exact() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (_, view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text(
                "a\t界e\u{301}zz\nfold\nhidden one\nhidden two\ntail tail",
            ),
            None,
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = true;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    let ui = UiState {
        lsp_folds: vec![crate::ui::render::UiFold {
            view,
            start_line: 1,
            end_line: 3,
            collapsed: true,
        }],
        ..ui_with_editor_layout(&state)
    };
    let buffer = state.views[view].buffer;
    let model = crate::ui::render::VisualRowModel::new(
        &state.buffers[buffer].text,
        view,
        &ui,
        &config,
        4,
        true,
    );
    assert_eq!(model.rows_for_line(0), 3);
    assert_eq!(
        model.normalize(crate::ui::render::VisualRowAnchor {
            line: 3,
            wrap_row: 8
        }),
        crate::ui::render::VisualRowAnchor {
            line: 1,
            wrap_row: 0
        }
    );
    let start = crate::ui::render::VisualRowAnchor::default();
    let sequence = (0..7)
        .scan(start, |anchor, _| {
            let current = *anchor;
            *anchor = model.next(*anchor);
            Some(current)
        })
        .collect::<Vec<_>>();
    assert!(!sequence.iter().any(|anchor| matches!(anchor.line, 2 | 3)));
    for pair in sequence.windows(2) {
        assert_eq!(model.previous(pair[1]), pair[0]);
    }
    assert_eq!(model.backward(model.forward(start, 5), 5), start);
}

#[test]
fn grapheme_atomic_rows_match_render_cursor_and_tiny_pane_policy() {
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = true;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;

    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text("aa界界"), None)
        .unwrap();
    let ui = ui_with_editor_layout(&state);
    let model = crate::ui::render::VisualRowModel::new(
        &state.buffers[buffer].text,
        view,
        &ui,
        &config,
        3,
        true,
    );
    assert_eq!(model.rows_for_line(0), 3);
    assert_eq!(
        model.line_rows(0),
        vec![
            crate::ui::render::VisualRowSlice {
                byte_start: 0,
                byte_end: 2,
                source_column_start: 0,
                used_cells: 2,
            },
            crate::ui::render::VisualRowSlice {
                byte_start: 2,
                byte_end: 5,
                source_column_start: 2,
                used_cells: 2,
            },
            crate::ui::render::VisualRowSlice {
                byte_start: 5,
                byte_end: 8,
                source_column_start: 4,
                used_cells: 2,
            },
        ]
    );
    let frame = render(&state, &ui, &config, &loaded.dashboard, 3, 3);
    assert_eq!(frame.grid.get(0, 0).unwrap().symbol, "a");
    assert_eq!(frame.grid.get(1, 0).unwrap().symbol, "a");
    assert_eq!(frame.grid.get(0, 1).unwrap().symbol, "界");
    assert!(frame.grid.get(1, 1).unwrap().continuation);
    assert_eq!(frame.grid.get(0, 2).unwrap().symbol, "界");
    assert!(frame.grid.get(1, 2).unwrap().continuation);

    state.views[view].scroll_wrap_row = 1;
    state.views[view].cursor = crate::editor::text::ByteOffset(2);
    let ui = ui_with_editor_layout(&state);
    let scrolled = render(&state, &ui, &config, &loaded.dashboard, 3, 1);
    assert_eq!(scrolled.grid.get(0, 0).unwrap().symbol, "界");
    assert_eq!(scrolled.cursor.map(|(x, y, _)| (x, y)), Some((0, 0)));
    state.views[view].scroll_wrap_row = 0;

    for &(cursor, expected) in &[
        (0, (0, 0)),
        (1, (1, 0)),
        (2, (0, 1)),
        (5, (0, 2)),
        (8, (2, 2)),
    ] {
        state.views[view].cursor = crate::editor::text::ByteOffset(cursor);
        let ui = ui_with_editor_layout(&state);
        let frame = render(&state, &ui, &config, &loaded.dashboard, 3, 3);
        assert_eq!(
            frame.cursor.map(|(x, y, _)| (x, y)),
            Some(expected),
            "cursor byte {cursor}"
        );
    }

    state.buffers[buffer].text = crate::editor::text::TextBuffer::from_text("abc");
    state.views[view].cursor = crate::editor::text::ByteOffset(3);
    let ui = ui_with_editor_layout(&state);
    let exact = render(&state, &ui, &config, &loaded.dashboard, 3, 1);
    assert_eq!(exact.cursor.map(|(x, y, _)| (x, y)), Some((2, 0)));

    state.buffers[buffer].text = crate::editor::text::TextBuffer::from_text("界a");
    state.views[view].cursor = crate::editor::text::ByteOffset(0);
    let ui = ui_with_editor_layout(&state);
    let tiny_model = crate::ui::render::VisualRowModel::new(
        &state.buffers[buffer].text,
        view,
        &ui,
        &config,
        1,
        true,
    );
    assert_eq!(tiny_model.rows_for_line(0), 2);
    let tiny = render(&state, &ui, &config, &loaded.dashboard, 1, 2);
    assert_eq!(
        tiny.grid.get(0, 0).unwrap().symbol,
        CONTROL_CHARACTER_REPLACEMENT
    );
    assert_eq!(tiny.grid.get(0, 1).unwrap().symbol, "a");
}

#[test]
fn mixed_grapheme_row_sequences_are_partitioned_reversible_and_bounded() {
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = true;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    let samples = [
        "aa界界",
        "aa\t\t",
        "e\u{301}界\tx",
        "界界界",
        "abc",
        "\t界e\u{301}z",
    ];
    for sample in samples {
        let boundaries = unicode_segmentation::UnicodeSegmentation::grapheme_indices(sample, true)
            .map(|(byte, _)| byte)
            .chain(std::iter::once(sample.len()))
            .collect::<Vec<_>>();
        for width in 1..=4 {
            let mut state = EditorState::new(PathBuf::from("/tmp"));
            let (buffer, view) = state
                .open_text(crate::editor::text::TextBuffer::from_text(sample), None)
                .unwrap();
            let ui = ui_with_editor_layout(&state);
            let model = crate::ui::render::VisualRowModel::new(
                &state.buffers[buffer].text,
                view,
                &ui,
                &config,
                width,
                true,
            );
            let rows = model.line_rows(0);
            assert!(!rows.is_empty());
            assert_eq!(rows[0].byte_start, 0);
            assert_eq!(rows.last().unwrap().byte_end, sample.len());
            for (index, row) in rows.iter().enumerate() {
                assert!(row.used_cells <= usize::from(width));
                assert!(boundaries.contains(&row.byte_start));
                assert!(boundaries.contains(&row.byte_end));
                if let Some(next) = rows.get(index + 1) {
                    assert_eq!(row.byte_end, next.byte_start);
                    let current = crate::ui::render::VisualRowAnchor {
                        line: 0,
                        wrap_row: index,
                    };
                    let following = model.next(current);
                    assert_eq!(following.wrap_row, index + 1);
                    assert_eq!(model.previous(following), current);
                }
                let anchor = crate::ui::render::VisualRowAnchor {
                    line: 0,
                    wrap_row: index,
                };
                let bytes = (0..usize::from(width))
                    .map(|cell| model.byte_at_cell(anchor, cell))
                    .collect::<Vec<_>>();
                assert!(bytes.windows(2).all(|pair| pair[0] <= pair[1]));
                assert!(bytes.iter().all(|byte| {
                    *byte >= row.byte_start && *byte <= row.byte_end && boundaries.contains(byte)
                }));
            }
            let last = crate::ui::render::VisualRowAnchor {
                line: 0,
                wrap_row: rows.len() - 1,
            };
            assert_eq!(
                model.backward(
                    model.forward(crate::ui::render::VisualRowAnchor::default(), 999),
                    999
                ),
                crate::ui::render::VisualRowAnchor::default()
            );
            assert_eq!(model.forward(last, 1), last);
        }
    }
}

#[test]
fn overlapping_collapsed_folds_normalize_to_one_reversible_interval() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text("zero\none\ntwo\nthree\nfour\nfive"),
            None,
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let ui = UiState {
        lsp_folds: vec![crate::ui::render::UiFold {
            view,
            start_line: 1,
            end_line: 3,
            collapsed: true,
        }],
        syntax_folds: vec![crate::ui::render::UiFold {
            view,
            start_line: 2,
            end_line: 4,
            collapsed: true,
        }],
        ..ui_with_editor_layout(&state)
    };
    let model = crate::ui::render::VisualRowModel::new(
        &state.buffers[buffer].text,
        view,
        &ui,
        &loaded.config,
        20,
        true,
    );
    let fold_start = crate::ui::render::VisualRowAnchor {
        line: 1,
        wrap_row: 0,
    };
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &loaded.config, 30, 5);
    assert!(frame.grid.row_text(1).contains("one ⋯ 3 lines"));
    assert!(frame.grid.row_text(2).contains("five"));
    let after = model.next(fold_start);
    assert_eq!(after.line, 5);
    assert_eq!(model.previous(after), fold_start);
    for hidden in 2..=4 {
        assert_eq!(
            model.normalize(crate::ui::render::VisualRowAnchor {
                line: hidden,
                wrap_row: 99,
            }),
            fold_start
        );
    }
}

#[test]
fn configured_wrapping_uses_visual_rows() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("abcdefghij"),
            None,
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = true;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);
    let frame = render(&state, &ui, &config, &loaded.dashboard, 5, 3);
    assert!(frame.grid.row_text(0).starts_with("abcde"));
    assert!(frame.grid.row_text(1).starts_with("fghij"));

    let view = state.active_view().expect("view");
    let buffer = state.views[view].buffer;
    state.buffers[buffer].text =
        crate::editor::text::TextBuffer::from_text("abcdefghijklmnopqrstuvwxyz");
    state.views[view].cursor = crate::editor::text::ByteOffset(25);
    let ui = ui_with_editor_layout(&state);
    let frame = render(&state, &ui, &config, &loaded.dashboard, 5, 3);
    assert!(frame.cursor.is_some_and(|(_, y, _)| y < 3));
    assert!(frame.grid.row_text(2).contains('z'));
}

#[test]
fn inlay_hints_are_inline_virtual_text_that_shift_following_source_and_cursor() {
    let text = "let value = build();";
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text(text), None)
        .unwrap();
    state.views[view].cursor = crate::editor::text::ByteOffset("let value".len());
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = false;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        lsp_inlay_hints: vec![crate::ui::render::UiInlayHint {
            buffer,
            revision: state.buffers[buffer].text.revision(),
            position: crate::editor::text::ByteOffset("let value".len()),
            label: ": Result ".into(),
        }],
        ..UiState::default()
    };
    let frame = render(&state, &ui, &config, &loaded.dashboard, 32, 2);
    assert!(
        frame
            .grid
            .row_text(0)
            .starts_with("let value: Result  = build();")
    );
    for column in "let value".len().."let value: Result ".len() {
        assert_eq!(
            frame.grid.get(column as u16, 0).expect("inlay cell").style,
            crate::ui::Style::Muted
        );
    }
    assert!(
        frame
            .cursor
            .is_some_and(|(x, y, _)| x == "let value: Result ".len() as u16 && y == 0)
    );

    config.editor.wrap = true;
    let frame = render(&state, &ui, &config, &loaded.dashboard, 16, 3);
    assert!(frame.grid.row_text(0).starts_with("let value"));
    assert!(frame.grid.row_text(1).starts_with(": Result  = buil"));
    assert!(
        frame
            .cursor
            .is_some_and(|(x, y, _)| x == ": Result ".len() as u16 && y == 1)
    );
}

#[test]
fn interior_grapheme_hints_snap_forward_without_blocking_later_hints() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, view) = state
        .open_text(TextBuffer::from_text("e\u{301} x!"), None)
        .unwrap();
    state.views[view].cursor = ByteOffset(5);
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let ui = UiState {
        substitute_preview: [1, 5]
            .into_iter()
            .map(|position| crate::ui::render::UiInlayHint {
                buffer,
                revision: state.buffers[buffer].text.revision(),
                position: ByteOffset(position),
                label: ">".into(),
            })
            .collect(),
        ..Default::default()
    };
    for wrap in [false, true] {
        config.editor.wrap = wrap;
        let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 16, 2);
        assert_eq!(frame.grid.row_text(0).trim_end(), "e\u{301}> x>!");
        assert_eq!(frame.cursor.map(|(x, y, _)| (x, y)), Some((5, 0)));
        config.editor.wrap = true;
        let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 4, 3);
        assert_eq!(frame.grid.row_text(0).trim_end(), "e\u{301}> x");
        assert_eq!(frame.grid.row_text(1).trim_end(), ">!");
    }
}

#[test]
fn non_wrapped_cursor_counts_hints_at_unterminated_and_empty_line_ends() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    for text in ["", "a", "a\n"] {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        let (buffer, view) = state.open_text(TextBuffer::from_text(text), None).unwrap();
        state.views[view].cursor = ByteOffset(text.len());
        state.views[view].mode = crate::editor::EditorMode::Insert;
        let mut config = crate::config::load_bundled_config().config;
        config.editor.line_numbers = crate::config::LineNumbers::Off;
        config.editor.wrap = false;
        config.ui.status_line.enabled = false;
        let ui = UiState {
            lsp_inlay_hints: vec![crate::ui::render::UiInlayHint {
                buffer,
                revision: state.buffers[buffer].text.revision(),
                position: ByteOffset(text.len()),
                label: ">".into(),
            }],
            ..Default::default()
        };
        let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 16, 3);
        let (column, row) = if text == "a" {
            (2, 0)
        } else if text.is_empty() {
            (1, 0)
        } else {
            (1, 1)
        };
        assert_eq!(
            frame.cursor.map(|(x, y, _)| (x, y)),
            Some((column, row)),
            "{text:?}"
        );
        assert!(frame.grid.row_text(row).contains('>'));
    }
}

#[test]
fn active_indent_scope_guide_is_subtly_elevated_from_normal_guides() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text(
                "fn first() {\n    first();\n}\nfn second() {\n    second();\n}\n",
            ),
            Some(PathBuf::from("/workspace/file.rs")),
        )
        .unwrap();
    state.views[view].cursor = state.buffers[state.views[view].buffer]
        .text
        .line_to_byte(5)
        .expect("closing brace line");
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 30, 7);

    assert_eq!(
        frame.grid.get(0, 1).expect("inactive guide").style,
        crate::ui::Style::Border
    );
    assert_eq!(
        frame.grid.get(0, 4).expect("active scope guide").style,
        crate::ui::Style::Muted
    );
    assert_eq!(frame.grid.get(0, 5).expect("closing brace").symbol, "}");
}

#[test]
fn active_indent_scope_tracks_the_cursor_display_column() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state.open_text(
            crate::editor::text::TextBuffer::from_text(
                "fn run() {\n    if outer {\n        if inner {\n            work();\n        }\n    }\n}\n",
            ),
            Some(PathBuf::from("/workspace/file.rs")),
        ).unwrap();
    let work_start = state.buffers[state.views[view].buffer]
        .text
        .line_to_byte(3)
        .expect("work line")
        .get();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);

    state.views[view].cursor = crate::editor::text::ByteOffset(work_start.saturating_add(1));
    let outer = crate::ui::render::render_editor_view(&state, view, &ui, &config, 30, 8);
    assert_eq!(
        outer.grid.get(0, 3).expect("outer guide").style,
        crate::ui::Style::Muted
    );
    assert_eq!(
        outer.grid.get(4, 3).expect("middle guide").style,
        crate::ui::Style::Border
    );
    assert_eq!(
        outer.grid.get(8, 3).expect("inner guide").style,
        crate::ui::Style::Border
    );

    state.views[view].cursor = crate::editor::text::ByteOffset(work_start.saturating_add(4));
    let middle = crate::ui::render::render_editor_view(&state, view, &ui, &config, 30, 8);
    assert_eq!(
        middle.grid.get(0, 3).expect("outer guide").style,
        crate::ui::Style::Border
    );
    assert_eq!(
        middle.grid.get(4, 3).expect("middle guide").style,
        crate::ui::Style::Muted
    );
    assert_eq!(
        middle.grid.get(8, 3).expect("inner guide").style,
        crate::ui::Style::Border
    );

    state.views[view].cursor = crate::editor::text::ByteOffset(work_start.saturating_add(8));
    let inner = crate::ui::render::render_editor_view(&state, view, &ui, &config, 30, 8);
    assert_eq!(
        inner.grid.get(0, 3).expect("outer guide").style,
        crate::ui::Style::Border
    );
    assert_eq!(
        inner.grid.get(4, 3).expect("middle guide").style,
        crate::ui::Style::Border
    );
    assert_eq!(
        inner.grid.get(8, 3).expect("inner guide").style,
        crate::ui::Style::Muted
    );
}

#[test]
fn closing_delimiter_scope_uses_cursor_column_before_the_delimiter() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state.open_text(
            crate::editor::text::TextBuffer::from_text(
                "fn run() {\n    if outer {\n        if inner {\n            work();\n        }\n    }\n}\n",
            ),
            Some(PathBuf::from("/workspace/file.rs")),
        ).unwrap();
    let closing_start = state.buffers[state.views[view].buffer]
        .text
        .line_to_byte(4)
        .expect("inner closing brace")
        .get();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);

    state.views[view].cursor = crate::editor::text::ByteOffset(closing_start);
    let before_delimiter = crate::ui::render::render_editor_view(&state, view, &ui, &config, 30, 8);
    assert_eq!(
        before_delimiter
            .grid
            .get(0, 4)
            .expect("outer guide before delimiter")
            .style,
        crate::ui::Style::Muted
    );
    assert_eq!(
        before_delimiter
            .grid
            .get(4, 4)
            .expect("middle guide before delimiter")
            .style,
        crate::ui::Style::Border
    );

    state.views[view].cursor = crate::editor::text::ByteOffset(closing_start.saturating_add(8));
    let on_delimiter = crate::ui::render::render_editor_view(&state, view, &ui, &config, 30, 8);
    assert_eq!(
        on_delimiter
            .grid
            .get(8, 3)
            .expect("inner guide on delimiter")
            .style,
        crate::ui::Style::Muted
    );
}

#[test]
fn git_signs_do_not_paint_over_text_when_the_gutter_is_disabled() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text("\ntext"), None)
        .unwrap();
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.editor.git_signs.enabled = true;
    config.ui.status_line.enabled = false;
    let frame =
        crate::ui::render::render_editor_view(&state, view, &UiState::default(), &config, 12, 2);
    assert_eq!(frame.grid.row_text(0).trim_end(), "");
    assert_eq!(frame.grid.row_text(1).trim_end(), "text");
    assert_eq!(
        crate::ui::render::visual_rows::git_sign_column(0, &config.editor),
        None
    );
}

#[test]
fn neutral_git_sign_column_continues_through_wraps_and_end_of_buffer_rows() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text("a deliberately wrapped line"),
            Some(PathBuf::from("/workspace/file.rs")),
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = true;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 10, 8);
    let gutter = crate::ui::render::editor_state_gutter_width(
        &state,
        &state.buffers[state.views[view].buffer].text,
        &config.editor,
        10,
    );

    let sign_column =
        crate::ui::render::visual_rows::git_sign_column(gutter, &config.editor).unwrap();
    assert!(
        (0..frame.grid.height()).all(|row| {
            frame
                .grid
                .get(sign_column, row)
                .is_some_and(|cell| cell.symbol == "│" && cell.style == crate::ui::Style::Muted)
        }),
        "the neutral sign rule must span content, wrapped, and end-of-buffer rows"
    );
}

#[test]
fn end_of_buffer_markers_right_align_with_line_number_digits() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let source = (0..1_000).map(|_| "line").collect::<Vec<_>>().join("\n");
    let (_, view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text(&source),
            Some(PathBuf::from("/workspace/file.rs")),
        )
        .unwrap();
    state.views[view].scroll_line = 999;
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 20, 4);
    let gutter = crate::ui::render::editor_state_gutter_width(
        &state,
        &state.buffers[state.views[view].buffer].text,
        &config.editor,
        20,
    );

    assert_eq!(
        crate::ui::render::visual_rows::editor_line_number_digits(&state),
        4
    );
    let sign_column =
        crate::ui::render::visual_rows::git_sign_column(gutter, &config.editor).unwrap();
    for row in 1..frame.grid.height() {
        assert_eq!(frame.grid.get(0, row).unwrap().symbol, " ");
        assert_eq!(frame.grid.get(3, row).unwrap().symbol, "~");
        assert_eq!(frame.grid.get(sign_column, row).unwrap().symbol, "│");
    }
}

#[test]
fn concrete_gutter_reserves_content_at_zero_one_and_two_cell_widths() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text("a\tX"), None)
        .unwrap();
    state.views[view].cursor = crate::editor::text::ByteOffset("a\t".len());
    state.views[view].scroll_column = 4;
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = false;
    config.editor.line_numbers = crate::config::LineNumbers::Absolute;
    config.ui.status_line.enabled = false;

    assert_eq!(
        crate::ui::render::editor_gutter_width(&state.buffers[buffer].text, &config.editor, 0),
        0
    );
    assert_eq!(
        crate::ui::render::editor_content_width(&state.buffers[buffer].text, &config.editor, 0),
        0
    );
    let ui = ui_with_editor_layout(&state);
    let zero = render(&state, &ui, &config, &loaded.dashboard, 0, 2);
    assert_eq!(zero.grid.width(), 0);
    assert!(zero.cursor.is_none());

    let mut split_state = EditorState::new(PathBuf::from("/tmp"));
    split_state
        .open_text(crate::editor::text::TextBuffer::from_text("x"), None)
        .unwrap();
    split_state
        .split_active(crate::windows::TileAxis::Horizontal, 100)
        .expect("minimum-ratio split");
    split_state.focus_pane(-1, false).expect("zero-width leaf");
    let mut split_ui = ui_with_editor_layout(&split_state);
    attach_test_compositor(&split_state, &mut split_ui, &config, 6, 2);
    let zero_leaf = crate::ui::render::active_pane_viewport(&split_state, &split_ui, &config, 6, 2)
        .expect("focused tiny-layout fallback");
    assert_eq!(zero_leaf.width, 6);
    let split_frame = render(&split_state, &split_ui, &config, &loaded.dashboard, 6, 2);
    assert!(split_frame.cursor.is_some());

    for (width, expected_gutter) in [(1, 0), (2, 1)] {
        assert_eq!(
            crate::ui::render::editor_gutter_width(
                &state.buffers[buffer].text,
                &config.editor,
                width,
            ),
            expected_gutter
        );
        assert_eq!(
            crate::ui::render::editor_content_width(
                &state.buffers[buffer].text,
                &config.editor,
                width,
            ),
            1
        );
        let ui = ui_with_editor_layout(&state);
        let frame = render(&state, &ui, &config, &loaded.dashboard, width, 2);
        assert_eq!(
            frame
                .grid
                .get(expected_gutter, 0)
                .expect("reserved content cell")
                .symbol,
            "X"
        );
        assert!(
            frame
                .cursor
                .is_some_and(|(x, y, _)| { x == expected_gutter && x < width && y == 0 })
        );
    }

    config.editor.line_numbers = crate::config::LineNumbers::Off;
    assert_eq!(
        crate::ui::render::editor_gutter_width(&state.buffers[buffer].text, &config.editor, 2),
        0
    );
    assert_eq!(
        crate::ui::render::editor_content_width(&state.buffers[buffer].text, &config.editor, 2),
        2
    );
}

#[test]
fn a_long_notification_wraps_so_its_end_stays_readable() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let _ = state
        .open_text(crate::editor::text::TextBuffer::from_text(""), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    ui.notification = Some(format!("{} says why at the very end", "failed ".repeat(20)));
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 20);
    let text: Vec<String> = (0..20).map(|row| frame.grid.row_text(row)).collect();
    assert!(text.iter().any(|row| row.contains("very end")), "{text:#?}");
}

#[test]
fn plain_text_wrapping_preserves_large_messages_and_graphemes() {
    let message = "a  b`c 界 e\u{301} ".repeat(10_000);
    for ambiguous in [
        crate::config::AmbiguousWidth::Narrow,
        crate::config::AmbiguousWidth::Wide,
    ] {
        let lines = crate::ui::render::wrap_plain_text(&message, 40, ambiguous);
        assert_eq!(lines.concat(), message);
        assert!(
            lines
                .iter()
                .all(|line| crate::ui::geometry::text_width(line, ambiguous) <= 40)
        );
    }
    assert_eq!(
        crate::ui::render::wrap_plain_text("\n", 40, crate::config::AmbiguousWidth::Narrow),
        ["", ""]
    );
}

#[test]
fn notification_newlines_and_literal_paths_are_preserved() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(crate::editor::text::TextBuffer::from_text(""), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    ui.notification = Some("Build failed\nSee log".into());
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 20);
    let first = (0..20)
        .find(|y| frame.grid.row_text(*y).contains("Build failed"))
        .unwrap();
    assert!(frame.grid.row_text(first + 1).contains("See log"));
    assert!(!(0..20).any(|y| frame.grid.row_text(y).contains('�')));

    let message = format!(
        "cannot save /workspace/a  b`c.rs {}",
        "long-path/".repeat(6)
    );
    ui.notification = Some(message.clone());
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 20);
    assert!((0..20).any(|y| frame.grid.row_text(y).contains("/workspace/a  b`c.rs")));
    for width in [1, 2, 8, 40] {
        let lines = crate::ui::render::wrap_plain_text(
            &message,
            width,
            loaded.config.editor.ambiguous_width,
        );
        assert_eq!(lines.concat(), message, "width {width}");
    }
    ui.notification = None;
    let mut prompt = PromptState::new(PromptKind::Command);
    prompt.error = Some("cannot open /workspace/a  b`c.rs".into());
    ui.prompt = Some(prompt);
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 24);
    assert!((0..24).any(|y| frame.grid.row_text(y).contains("/workspace/a  b`c.rs")));
}

#[test]
fn a_long_path_in_a_notification_breaks_so_its_name_stays_readable() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let _ = state
        .open_text(crate::editor::text::TextBuffer::from_text(""), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    ui.notification = Some(format!(
        "cannot save /{}/name.txt",
        "long-directory/".repeat(8)
    ));
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 20);
    let text: Vec<String> = (0..20).map(|row| frame.grid.row_text(row)).collect();
    assert!(text.iter().any(|row| row.contains("name.txt")), "{text:#?}");
}

#[test]
fn wrapping_ignores_horizontal_offset() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (_, view) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text("abcdefghij"),
            None,
        )
        .unwrap();
    state.views[view].scroll_column = 7;
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = true;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);
    let frame = render(&state, &ui, &config, &loaded.dashboard, 5, 3);
    assert!(frame.grid.row_text(0).starts_with("abcde"));
    assert!(frame.grid.row_text(1).starts_with("fghij"));
    assert!(frame.cursor.is_some_and(|(x, y, _)| x == 0 && y == 0));
}

#[test]
fn active_pane_viewport_uses_exact_focused_split_rectangle() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(crate::editor::text::TextBuffer::from_text("one\ntwo"), None)
        .unwrap();
    state
        .split_active(crate::windows::TileAxis::Horizontal, 400)
        .expect("horizontal split");
    state
        .split_active(crate::windows::TileAxis::Vertical, 600)
        .expect("vertical split");
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    attach_test_compositor(&state, &mut ui, &loaded.config, 79, 24);
    let viewport = crate::ui::render::active_pane_viewport(&state, &ui, &loaded.config, 79, 23)
        .expect("focused viewport");
    assert_eq!(Some(viewport.view), state.active_view());
    assert!(viewport.x > 0 && viewport.y > 0);
    assert!(viewport.width < 79 && viewport.height < 23);
    assert!(viewport.y.saturating_add(viewport.height) <= 23);
}

#[test]
fn private_surface_cells_and_cursor_are_local_and_strictly_clipped() {
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new_with_policy(10, 4, "�", "◌", true),
        cursor: None,
    };
    let surface = crate::ui::render::PrivateSurfaceUiState {
        handle: 3,
        window: crate::windows::WindowId(7),
        dimensions_revision: 2,
        surface_revision: 1,
        width: 4,
        height: 2,
        cells: vec![
            crate::ui::render::PrivateSurfaceCell {
                x: 0,
                y: 0,
                grapheme: "界".into(),
                continuation: false,
                style: crate::ui::Style::Accent,
            },
            crate::ui::render::PrivateSurfaceCell {
                x: 1,
                y: 0,
                grapheme: String::new(),
                continuation: true,
                style: crate::ui::Style::Accent,
            },
            crate::ui::render::PrivateSurfaceCell {
                x: 0,
                y: 1,
                grapheme: "·".into(),
                continuation: false,
                style: crate::ui::Style::Accent,
            },
            crate::ui::render::PrivateSurfaceCell {
                x: 1,
                y: 1,
                grapheme: "a".into(),
                continuation: false,
                style: crate::ui::Style::Accent,
            },
            crate::ui::render::PrivateSurfaceCell {
                x: 3,
                y: 1,
                grapheme: "e\u{301}".into(),
                continuation: false,
                style: crate::ui::Style::Selection,
            },
        ],
        cursor: Some((3, 1)),
        focused: true,
    };
    crate::ui::render::render_private_surface(
        &mut frame,
        &surface,
        crate::windows::WindowRect {
            x: 2,
            y: 1,
            width: 4,
            height: 2,
        },
    );
    assert_eq!(frame.grid.get(2, 1).expect("wide lead").symbol, "界");
    assert!(
        frame
            .grid
            .get(3, 1)
            .expect("wide continuation")
            .continuation
    );
    assert_eq!(
        frame.grid.get(2, 2).expect("narrow private cell").symbol,
        "·"
    );
    assert_eq!(
        frame.grid.get(3, 2).expect("adjacent private cell").symbol,
        "a"
    );
    assert!(!frame.grid.get(3, 2).unwrap().continuation);
    assert_eq!(frame.grid.get(6, 2).expect("outside right").symbol, " ");
    assert!(frame.cursor.is_some_and(|(x, y, _)| (x, y) == (5, 2)));

    let mut stale = surface.clone();
    stale.width = 5;
    let before = (frame.grid.row_text(1), frame.grid.row_text(2), frame.cursor);
    crate::ui::render::render_private_surface(
        &mut frame,
        &stale,
        crate::windows::WindowRect {
            x: 2,
            y: 1,
            width: 4,
            height: 2,
        },
    );
    assert_eq!(
        (frame.grid.row_text(1), frame.grid.row_text(2), frame.cursor),
        before,
        "stale dimensions draw nothing"
    );
}

#[test]
fn multiline_diagnostics_style_continuations_but_only_annotate_the_start() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::editor::text::TextBuffer;
    use crate::ui::Style;
    let path = PathBuf::from("/workspace/file.rs");
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state
        .open_text(
            TextBuffer::from_text("abc\ndef\nghi\njkl"),
            Some(path.clone()),
        )
        .unwrap();
    let mut config = crate::config::load_bundled_config().config;
    config.ui.status_line.enabled = false;
    let ui = UiState {
        diagnostics: vec![crate::ui::render::UiDiagnostic {
            buffer: None,
            path,
            range: ByteRange::new(ByteOffset(1), ByteOffset(12)).unwrap(),
            severity: crate::workspace::DiagnosticSeverity::Error,
            source: String::new(),
            code: None,
            message: "problem".into(),
        }],
        ..Default::default()
    };
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 40, 4);
    let gutter = crate::ui::render::editor_state_gutter_width(
        &state,
        &state.buffers[state.views[view].buffer].text,
        &config.editor,
        40,
    );
    assert_eq!(frame.grid.get(gutter, 0).unwrap().style, Style::Default);
    assert_eq!(
        frame.grid.get(gutter + 1, 0).unwrap().style,
        Style::DiagnosticError
    );
    for row in [1, 2] {
        assert_eq!(
            frame.grid.get(gutter, row).unwrap().style,
            Style::DiagnosticError
        );
        assert!(!frame.grid.row_text(row).contains("problem"));
    }
    assert_eq!(frame.grid.get(gutter, 3).unwrap().style, Style::Default);
    assert!(frame.grid.row_text(0).contains("problem"));
    // Starting the viewport inside the diagnostic still paints its continuation.
    state.views[view].cursor = ByteOffset(8);
    state.views[view].scroll_line = 1;
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 40, 2);
    assert_eq!(
        frame.grid.get(gutter, 0).unwrap().style,
        Style::DiagnosticError
    );
    assert!(!frame.grid.row_text(0).contains("problem"));
}

#[test]
fn diagnostic_continuations_handle_unicode_ends_and_collapsed_lines() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::ByteRange;
    use crate::editor::text::TextBuffer;
    use crate::ui::Style;
    let path = PathBuf::from("/workspace/file.rs");
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state
        .open_text(
            TextBuffer::from_text("abc\ndef\nghé\njkl"),
            Some(path.clone()),
        )
        .unwrap();
    let mut config = crate::config::load_bundled_config().config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    for end in [12, 13] {
        let ui = UiState {
            diagnostics: vec![crate::ui::render::UiDiagnostic {
                buffer: None,
                path: path.clone(),
                range: ByteRange::new(ByteOffset(1), ByteOffset(end)).unwrap(),
                severity: crate::workspace::DiagnosticSeverity::Error,
                source: String::new(),
                code: None,
                message: "problem".into(),
            }],
            ..Default::default()
        };
        let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 40, 4);
        assert_eq!(
            frame.grid.get(2, 2).unwrap().style,
            Style::DiagnosticError,
            "end {end}"
        );
        assert_eq!(
            frame.grid.get(0, 3).unwrap().style,
            Style::Default,
            "end {end}"
        );
    }
    let source = format!("start\n{}last", "line\n".repeat(10_000));
    let buffer = state.views[view].buffer;
    state.buffers[buffer].text = TextBuffer::from_text(&source);
    let ui = UiState {
        diagnostics: vec![crate::ui::render::UiDiagnostic {
            buffer: None,
            path,
            range: ByteRange::new(ByteOffset(0), ByteOffset(source.len())).unwrap(),
            severity: crate::workspace::DiagnosticSeverity::Error,
            source: String::new(),
            code: None,
            message: "problem".into(),
        }],
        syntax_folds: vec![crate::ui::render::UiFold {
            view,
            start_line: 0,
            end_line: 10_000,
            collapsed: true,
        }],
        ..Default::default()
    };
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &config, 40, 2);
    assert!(frame.grid.row_text(1).starts_with("last"));
    assert_eq!(frame.grid.get(0, 1).unwrap().style, Style::DiagnosticError);
}

#[test]
fn status_line_names_the_symbols_around_the_cursor() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, _) = state
        .open_text(
            crate::editor::text::TextBuffer::from_text("one\ntwo\nthree\n"),
            Some(PathBuf::from("/tmp/main.rs")),
        )
        .unwrap();
    let symbol = |name: &str, start, end| crate::ui::render::UiBreadcrumb {
        name: name.into(),
        range: crate::editor::text::ByteRange::new(
            crate::editor::text::ByteOffset(start),
            crate::editor::text::ByteOffset(end),
        )
        .unwrap(),
    };
    let ui = UiState {
        breadcrumbs: Some((
            buffer,
            vec![
                symbol("Outer", 0, 14),
                symbol("inner", 0, 3),
                symbol("sibling", 2, 3),
                symbol("elsewhere", 8, 13),
            ],
        )),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 10);
    let status = frame.grid.row_text(9);
    assert!(status.contains("\u{203a} Outer \u{203a} inner"), "{status}");
    assert!(
        !status.contains("sibling"),
        "same line, but the cursor is not in it: {status}"
    );
    assert!(!status.contains("elsewhere"), "{status}");
}

#[test]
fn wrapped_rows_end_at_words_and_break_only_long_ones() {
    let rows = crate::ui::render::debug::wrap_rows(
        &["one two three".into(), "abcdefghijkl".into()],
        8,
        &crate::ui::CellGrid::new(80, 24),
    );
    assert_eq!(rows, ["one two", "  three", "abcdefgh", "  ijkl"]);
}

#[test]
fn a_line_with_a_diagnostic_is_marked_in_the_gutter_with_its_worst_severity() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let path = PathBuf::from("/tmp/main.rs");
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("bad\nfine\nodd\n"),
            Some(path.clone()),
        )
        .unwrap();
    let diagnostic = |severity, start, end| crate::ui::render::UiDiagnostic {
        buffer: None,
        path: path.clone(),
        severity,
        range: crate::editor::text::ByteRange::new(
            crate::editor::text::ByteOffset(start),
            crate::editor::text::ByteOffset(end),
        )
        .expect("range"),
        source: "rustc".into(),
        code: None,
        message: "problem".into(),
    };
    use crate::workspace::DiagnosticSeverity::Error;
    use crate::workspace::DiagnosticSeverity::Warning;
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        diagnostics: vec![
            diagnostic(Warning, 0, 1),
            diagnostic(Error, 1, 2),
            diagnostic(Warning, 9, 12),
        ],
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 10);
    let row = |text: &str| {
        (0..10)
            .find(|y| frame.grid.row_text(*y).contains(text))
            .unwrap_or_else(|| panic!("{text} is shown"))
    };
    let mark = |y| {
        (0..6)
            .filter_map(|x| frame.grid.get(x, y))
            .find(|cell| cell.symbol == "●")
            .map(|cell| cell.style)
    };
    assert_eq!(
        mark(row("bad")),
        Some(crate::ui::Style::Error),
        "the worst severity on the line"
    );
    assert_eq!(mark(row("fine")), None);
    assert_eq!(mark(row("odd")), Some(crate::ui::Style::Warning));
}

#[test]
fn focused_diagnostic_expands_all_matching_messages_as_virtual_lines() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let path = PathBuf::from("/tmp/main.rs");
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("bad\nnext\n"),
            Some(path.clone()),
        )
        .unwrap();
    let range = crate::editor::text::ByteRange::new(
        crate::editor::text::ByteOffset(0),
        crate::editor::text::ByteOffset(3),
    )
    .expect("range");
    let diagnostic = |message: &str, code: &str| crate::ui::render::UiDiagnostic {
        buffer: None,
        path: path.clone(),
        range,
        severity: crate::workspace::DiagnosticSeverity::Error,
        source: "rustc".into(),
        code: Some(code.into()),
        message: message.into(),
    };
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        diagnostics: vec![
            diagnostic("first error", "E0001"),
            diagnostic("second error", "E0002"),
        ],
        expanded_diagnostic: Some(crate::ui::render::ExpandedDiagnosticState {
            buffer: None,
            path,
            line: 0,
            severities: vec![crate::workspace::DiagnosticSeverity::Error],
        }),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 10);
    assert!(frame.grid.row_text(0).contains("●● first error [E0001]"));
    assert!(frame.grid.row_text(1).contains("Diagnostics:"));
    assert!(frame.grid.row_text(2).contains("1. first error [E0001]"));
    assert!(frame.grid.row_text(3).contains("2. second error [E0002]"));
    assert!(frame.grid.row_text(4).contains("next"));
}

#[test]
fn key_hints_anchor_clip_and_preserve_protected_rows() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    let loaded = crate::config::load_bundled_config();
    let config = loaded.config;
    let ui = UiState {
        dashboard: true,
        key_hints: Some(crate::ui::render::KeyHintUiState {
            prefix: "space f".into(),
            rows: (0..30)
                .map(|index| crate::ui::render::KeyHintUiRow {
                    next_key: char::from(b'a' + index).to_string(),
                    label: format!("Action {index}"),
                    icon: (index == 0).then(|| "F".into()),
                    group: index == 0,
                })
                .collect(),
        }),
        ..UiState::default()
    };
    let right = render(&state, &ui, &config, &loaded.dashboard, 80, 20);
    let right_prefix_x = (0..80)
        .find(|x| {
            (0..19).any(|y| {
                right
                    .grid
                    .get(*x, y)
                    .is_some_and(|cell| cell.style == crate::ui::Style::KeyHintPrefix)
            })
        })
        .expect("right anchored prefix");
    assert!(right_prefix_x > 30);
    assert!(!(0..80).any(|x| {
        right.grid.get(x, 19).is_some_and(|cell| {
            matches!(
                cell.style,
                crate::ui::Style::KeyHintBackground
                    | crate::ui::Style::KeyHintBorder
                    | crate::ui::Style::KeyHintKey
                    | crate::ui::Style::KeyHintGroup
                    | crate::ui::Style::KeyHintCommand
                    | crate::ui::Style::KeyHintPrefix
            )
        })
    }));
    assert!(!(0..20).any(|row| right.grid.row_text(row).contains("Action 29")));

    let tiny = render(&state, &ui, &config, &loaded.dashboard, 10, 3);
    assert!(!(0..3).any(|row| tiny.grid.row_text(row).contains("space f")));
}

#[test]
fn key_hints_use_available_rows_and_stay_inside_a_lower_workspace() {
    let hints = crate::ui::render::KeyHintUiState {
        prefix: "space".into(),
        rows: (0..8)
            .map(|index| crate::ui::render::KeyHintUiRow {
                next_key: index.to_string(),
                label: format!("Action {index}"),
                icon: None,
                group: false,
            })
            .collect(),
    };
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(80, 40),
        cursor: None,
    };
    for y in 0..40 {
        frame.grid.fill_row(y, crate::ui::Style::TerminalBackground);
    }
    crate::ui::render::terminal::render_key_hints(
        &mut frame,
        &hints,
        crate::windows::WindowRect {
            x: 0,
            y: 30,
            width: 80,
            height: 8,
        },
        crate::config::AmbiguousWidth::Narrow,
    );
    for index in 0..8 {
        assert!((30..37).any(|y| frame.grid.row_text(y).contains(&format!("Action {index}"))));
    }
    for y in (0..30).chain(37..40) {
        assert!(
            (0..80).all(
                |x| frame.grid.get(x, y).unwrap().style == crate::ui::Style::TerminalBackground
            )
        );
    }
}

#[test]
fn key_hints_measure_wide_labels_and_keep_borders_single_cell() {
    let hints = crate::ui::render::KeyHintUiState {
        prefix: "space".into(),
        rows: vec![crate::ui::render::KeyHintUiRow {
            next_key: "a".into(),
            label: "·".repeat(20),
            icon: None,
            group: false,
        }],
    };
    for (width, clipped) in [(60, false), (24, true)] {
        let mut frame = crate::ui::grid::Frame {
            grid: crate::ui::CellGrid::new_with_policy(width, 10, "?", "?", true),
            cursor: None,
        };
        crate::ui::render::terminal::render_key_hints(
            &mut frame,
            &hints,
            crate::windows::WindowRect {
                x: 0,
                y: 0,
                width,
                height: 10,
            },
            crate::config::AmbiguousWidth::Wide,
        );
        let body = (0..10)
            .find(|y| frame.grid.row_text(*y).contains("[a]"))
            .unwrap();
        let text = frame.grid.row_text(body);
        assert_eq!(text.contains('…'), clipped);
        if !clipped {
            assert!(text.contains(&"·".repeat(20)));
        }
        for y in 0..10 {
            for x in 0..width {
                let cell = frame.grid.get(x, y).unwrap();
                if cell.style == crate::ui::Style::KeyHintBorder {
                    assert!(!cell.continuation);
                    assert!(matches!(cell.symbol.as_str(), "+" | "-" | "|"));
                }
            }
        }
    }
}

#[test]
fn the_operation_bar_renders_the_phase_at_width_and_never_loses_cancellation() {
    use crate::ui::OperationBarState;
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    // The layout needs an open buffer, which is also the ordinary state a paused operation is
    // seen in.
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("fn main() {}"),
            Some(PathBuf::from("/workspace/main.rs")),
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let config = loaded.config.clone();
    let dashboard = loaded.dashboard.clone();

    let bar = OperationBarState {
        operation: "merge".into(),
        phase: "merge: 1 conflicted path(s)".into(),
        blockers: vec!["shared.txt".into()],
        actions: vec!["resolve".into(), "abort".into()],
        abort_binding: Some("Space g m a".into()),
    };

    // WIDE: the bar is drawn on its own row and names the operation, the phase and the actions.
    let mut ui = UiState {
        operation_bar: Some(bar.clone()),
        ..UiState::default()
    };
    ui.editor_window_layout = Some(editor_layout(&state));
    let frame = render(&state, &ui, &config, &dashboard, 80, 12);
    let wide: Vec<String> = (0..12).map(|y| frame.grid.row_text(y)).collect();
    assert!(
        wide.iter()
            .any(|row| row.contains("merge") && row.contains("conflicted")),
        "the bar names the operation and its phase: {wide:?}"
    );
    assert!(
        wide.iter()
            .any(|row| row.contains("resolve") && row.contains("abort")),
        "and only the actions valid in this state: {wide:?}"
    );
    assert!(
        wide.iter().any(|row| row.contains("shared.txt")),
        "and the blocker is named by PATH: {wide:?}"
    );

    // NARROW: the operation state SURVIVES, and the cancellation is still exposed.
    let frame = render(&state, &ui, &config, &dashboard, 24, 12);
    let narrow: Vec<String> = (0..12).map(|y| frame.grid.row_text(y)).collect();
    assert!(
        narrow.iter().any(|row| row.contains("merge")),
        "at a narrow width the operation still shows: {narrow:?}"
    );
    assert!(
        narrow.iter().any(|row| row.contains("Space g m a abort")),
        "and a bar that cannot fit still exposes cancellation: {narrow:?}"
    );
    assert!(
        !narrow.iter().any(|row| row.contains("conflicted path")),
        "the lengthy phase is what gets shortened first: {narrow:?}"
    );

    // ABSENT: with nothing paused the bar is not drawn at all, so its presence means something.
    let quiet = UiState {
        operation_bar: None,
        ..UiState::default()
    };
    let mut quiet = quiet;
    quiet.editor_window_layout = Some(editor_layout(&state));
    let frame = render(&state, &quiet, &config, &dashboard, 80, 12);
    let none: Vec<String> = (0..12).map(|y| frame.grid.row_text(y)).collect();
    assert!(
        !none
            .iter()
            .any(|row| row.contains("resolve") || row.contains("cancel")),
        "with no operation paused there is no bar: {none:?}"
    );
}

#[test]
fn operation_bar_keeps_actions_before_blockers_and_fits_display_cells() {
    use crate::config::AmbiguousWidth;
    let mut bar = crate::ui::render::OperationBarState {
        operation: "merge".into(),
        phase: "paused".into(),
        blockers: vec!["界".repeat(30)],
        actions: vec!["resolve".into(), "abort".into()],
        abort_binding: Some("ctrl-a".into()),
    };
    assert_eq!(
        bar.row(70, AmbiguousWidth::Narrow),
        "merge · paused · resolve/ctrl-a abort"
    );
    for ambiguous in [AmbiguousWidth::Narrow, AmbiguousWidth::Wide] {
        bar.phase = "e\u{301}界·".repeat(10);
        for width in 0..90 {
            let row = bar.row(width, ambiguous);
            assert!(
                crate::ui::geometry::text_width(&row, ambiguous) <= usize::from(width),
                "{row:?} at {width}"
            );
            assert!(!row.contains("esc cancel"));
            if width >= 12 {
                assert!(row.contains("ctrl-a abort"));
            }
        }
        bar.phase = "paused".into();
        bar.blockers = vec!["·".repeat(25)];
        let row = bar.row(70, ambiguous);
        assert!(row.contains("resolve/ctrl-a abort"));
        assert!(crate::ui::geometry::text_width(&row, ambiguous) <= 70);
    }
}

#[test]
fn a_conflict_free_pause_still_shows_the_bar() {
    use crate::ui::OperationBarState;
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
    // ui.md requires the bar for a paused operation even when NO textual conflict remains.
    let mut ui = UiState {
        operation_bar: Some(OperationBarState {
            operation: "merge".into(),
            phase: "merge: no textual conflicts remain".into(),
            blockers: Vec::new(),
            actions: vec!["continue".into(), "abort".into()],
            abort_binding: Some("Space g m a".into()),
        }),
        ..UiState::default()
    };
    ui.editor_window_layout = Some(editor_layout(&state));
    let frame = render(&state, &ui, &config, &dashboard, 80, 12);
    let rows: Vec<String> = (0..12).map(|y| frame.grid.row_text(y)).collect();
    assert!(
        rows.iter()
            .any(|row| row.contains("no textual conflicts remain")),
        "a conflict-free pause still shows the operation and its phase: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("continue")),
        "and offers the action that is now valid: {rows:?}"
    );
}

#[test]
fn the_operation_bar_takes_no_focus_and_opens_nothing_by_itself() {
    use crate::ui::OperationBarState;
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

    // A frame with NO operation, for comparison.
    let mut quiet = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        ..UiState::default()
    };
    let quiet_frame = render(&state, &quiet, &config, &dashboard, 80, 12);
    let quiet_cursor = quiet_frame.cursor;

    // The same frame WITH a paused operation.
    quiet.operation_bar = Some(OperationBarState {
        operation: "rebase".into(),
        phase: "rebase: no textual conflicts remain".into(),
        blockers: Vec::new(),
        actions: vec!["continue".into(), "abort".into()],
        abort_binding: Some("Space g m a".into()),
    });
    let frame = render(&state, &quiet, &config, &dashboard, 80, 12);

    // NO FOCUS STEALING: the bar does not move the focus.
    assert_eq!(
        frame.cursor, quiet_cursor,
        "the bar leaves the focus exactly where it was: {:?} vs {:?}",
        frame.cursor, quiet_cursor
    );
    // Only the bar's row changes: no popup or picker may cover the editor or status line.
    for y in 0..12 {
        if y == 10 {
            continue;
        }
        for x in 0..80 {
            assert_eq!(
                frame.grid.get(x, y),
                quiet_frame.grid.get(x, y),
                "cell {x},{y}"
            );
        }
    }

    // The bar's own text carries NOTHING beyond the operation, phase, blockers and actions: the
    // row is asserted exactly rather than by substring.
    let bar_row = (0..12)
        .map(|y| frame.grid.row_text(y))
        .find(|row| row.contains("rebase"))
        .expect("the bar row is drawn");
    assert_eq!(
        bar_row.trim_end(),
        "rebase · rebase: no textual conflicts remain · continue/Space g m a abort",
        "the bar explains nothing unprompted: {bar_row:?}"
    );
}

#[test]
fn the_selected_inspector_decides_the_narrow_strip_content_not_only_its_marker() {
    // "Selectable inspectors" is only real if the CONTENT follows the selection: an assertion that
    // the marker moved would pass even if the strip kept showing the previous inspector.
    use crate::ui::render::debug::DebugPane;
    use crate::ui::render::debug::DebugPanesUiState;
    use crate::ui::render::debug::debug_layout;
    use crate::ui::render::debug::render_debug_panes;
    let layout = debug_layout(60, 24).expect("a narrow layout");
    let strip = layout.console.expect("the narrow strip");
    let base = DebugPanesUiState {
        selected: DebugPane::Watches,
        target: "owned: attached to processId 4242".into(),
        status: String::new(),
        variables: Vec::new(),
        threads: vec!["main (1)".into()],
        stack: vec!["main:1".into()],
        breakpoints: vec!["src/main.rs:3".into()],
        watches: vec!["count".into()],
        console: vec!["count = 1".into()],
    };
    let mut watches = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(60, 24),
        cursor: None,
    };
    render_debug_panes(&mut watches, layout, &base);
    let mut breakpoints = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(60, 24),
        cursor: None,
    };
    render_debug_panes(
        &mut breakpoints,
        layout,
        &DebugPanesUiState {
            selected: DebugPane::Breakpoints,
            ..base.clone()
        },
    );
    let rows_of = |frame: &crate::ui::grid::Frame| -> String {
        (strip.y..strip.y + strip.height)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let watches_rows = rows_of(&watches);
    let breakpoint_rows = rows_of(&breakpoints);
    assert_ne!(
        watches_rows, breakpoint_rows,
        "the CONTENT changes with the selection, not just the marker"
    );
    assert!(
        watches_rows.contains("count"),
        "the watches inspector shows the watch: {watches_rows:?}"
    );
    assert!(
        breakpoint_rows.contains("src/main.rs:3"),
        "the breakpoints inspector shows the breakpoint: {breakpoint_rows:?}"
    );
}

#[test]
fn a_breakpoint_condition_wraps_and_a_long_frame_ends_in_an_ellipsis() {
    use crate::ui::render::debug::DebugPanesUiState;
    use crate::ui::render::debug::debug_layout;
    use crate::ui::render::debug::render_debug_panes;
    let layout = debug_layout(120, 40).unwrap();
    let inner = layout.left.unwrap().width as usize - 2;
    let panes = DebugPanesUiState {
        selected: crate::ui::render::debug::DebugPane::Stack,
        target: String::new(),
        status: String::new(),
        variables: Vec::new(),
        threads: Vec::new(),
        stack: vec!["f".repeat(inner + 5)],
        breakpoints: vec![format!(
            "main.py:6 if {} == \"Ada\"",
            "person.name".repeat(2)
        )],
        watches: Vec::new(),
        console: Vec::new(),
    };
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(120, 40),
        cursor: None,
    };
    render_debug_panes(&mut frame, layout, &panes);
    let text = (0..40)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("\"Ada\""), "{text}");
    assert!(
        text.contains(&format!("{}…", "f".repeat(inner - 1))),
        "{text}"
    );
}

/// The column at which `needle` starts on the row where it is drawn, and that row.
pub(super) fn drawn_at(frame: &super::Frame, needle: &str) -> Option<(u16, u16)> {
    for y in 0..frame.grid.height() {
        let text = frame.grid.row_text(y);
        if let Some(byte) = text.find(needle) {
            let mut offset = 0;
            for column in 0..frame.grid.width() {
                let cell = frame.grid.get(column, y).expect("grid cell");
                if cell.continuation {
                    continue;
                }
                if offset == byte {
                    return Some((column, y));
                }
                offset += cell.symbol.len();
            }
        }
    }
    None
}

#[test]
fn drawn_at_locates_display_cells_after_wide_and_combining_graphemes() {
    for wide in [false, true] {
        let mut grid =
            crate::ui::CellGrid::new_with_policy(20, 1, CONTROL_CHARACTER_REPLACEMENT, "◌", wide);
        grid.put_text(0, 0, "界e\u{301}·target", crate::ui::Style::Default);
        let frame = super::Frame { grid, cursor: None };
        let expected = if wide { 5 } else { 4 };
        assert_eq!(drawn_at(&frame, "target"), Some((expected, 0)));
        assert_eq!(frame.grid.get(expected, 0).unwrap().symbol, "t");
    }
}

#[test]
fn key_help_breaks_between_items_and_keeps_its_end() {
    let ambiguous = crate::config::load_bundled_config()
        .config
        .editor
        .ambiguous_width;
    let rows = crate::ui::geometry::wrapped_key_help(
        "j/k move · Enter install · r refresh · Esc close",
        26,
        ambiguous,
    );
    assert_eq!(rows, ["j/k move · Enter install", "r refresh · Esc close"]);
}

#[test]
fn a_notification_too_long_to_show_points_at_the_messages_list() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let _ = state
        .open_text(crate::editor::text::TextBuffer::from_text(""), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    ui.notification = Some("conflict ".repeat(200));
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 20);
    let text: Vec<String> = (0..20).map(|row| frame.grid.row_text(row)).collect();
    assert!(
        text.iter().any(|row| row.contains("… :messages")),
        "{text:#?}"
    );
}

#[test]
fn the_status_line_puts_the_server_and_root_on_the_right() {
    let mut state = EditorState::new(PathBuf::from("/tmp/some-project"));
    let (_, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text("x"), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    ui.lsp_server = Some("rust-analyzer".into());
    let wide = crate::ui::render::render_editor_view(&state, view, &ui, &loaded.config, 80, 5);
    let status = wide.grid.row_text(4);
    assert!(status.contains("rust-analyzer  some-project"), "{status:?}");
    assert!(
        status.trim_end().ends_with("Top \u{e0b2} 1:1"),
        "{status:?}"
    );
    assert_eq!(
        wide.grid.get(1, 4).expect("mode").style,
        crate::ui::Style::StatusAccent
    );

    // The server goes first when the room runs out, then the progress; the root stays.
    let narrow = crate::ui::render::render_editor_view(&state, view, &ui, &loaded.config, 42, 5);
    let status = narrow.grid.row_text(4);
    assert!(
        status.trim_end().ends_with("some-project  \u{e0b2} 1:1"),
        "{status:?}"
    );
    assert!(
        !status.contains("rust-analyzer") && !status.contains("Top"),
        "{status:?}"
    );

    // Each mode shows in its own colour.
    state.views[view].mode = crate::editor::EditorMode::Insert;
    let insert = crate::ui::render::render_editor_view(&state, view, &ui, &loaded.config, 80, 5);
    assert_eq!(
        insert.grid.get(1, 4).expect("mode").style,
        crate::ui::Style::StatusInsert
    );
}

#[test]
fn the_status_line_names_a_worktree_with_its_repository() {
    let mut state = EditorState::new(PathBuf::from("/tmp/repo/.worktree/feature"));
    state.workspace_repository = Some("repo".into());
    let (_, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text("x"), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let ui = ui_with_editor_layout(&state);
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &loaded.config, 80, 5);
    assert!(frame.grid.row_text(4).contains("repo › feature"));
    state.workspace_repository = Some("repo{worktree}{repository}".into());
    state.workspace_root = PathBuf::from("/tmp/repo/.worktree/feature{repository}");
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &loaded.config, 120, 5);
    assert!(
        frame
            .grid
            .row_text(4)
            .contains("repo{worktree}{repository} › feature{repository}")
    );
}

#[test]
fn the_status_line_keeps_fields_from_running_into_the_position() {
    let mut state = EditorState::new(PathBuf::from("/tmp/a-rather-long-project-name"));
    let (_, view) = state
        .open_text(crate::editor::text::TextBuffer::from_text("x"), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let ui = ui_with_editor_layout(&state);
    let frame = crate::ui::render::render_editor_view(&state, view, &ui, &loaded.config, 22, 5);
    let status = frame.grid.row_text(4);
    assert!(status.trim_end().ends_with(" 1:1"), "{status:?}");
}

#[test]
fn lsp_progress_is_animated_right_aligned_and_hides_transport_identity_slot5() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("fn main() {}"),
            Some(PathBuf::from("/workspace/main.rs")),
        )
        .unwrap();
    let mut ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        explorer: Some(crate::ui::render::ExplorerUiState {
            title: "Explorer".into(),
            rows: Vec::new(),
            scroll: 0,
            loading: false,
            total: 0,
            matches: 0,
            query: String::new(),
            searching: false,
        }),
        lsp_progress: vec![
            crate::ui::render::UiLspProgress {
                server_key: "root:1:rust-analyzer".into(),
                token: "roots".into(),
                server: "rust-analyzer".into(),
                title: "Roots Scanned".into(),
                message: String::new(),
                percentage: None,
                done: true,
                completed_at: Some(std::time::Instant::now()),
            },
            crate::ui::render::UiLspProgress {
                server_key: "root:1:rust-analyzer".into(),
                token: "cargo-check".into(),
                server: "rust-analyzer".into(),
                title: "Building".into(),
                message: "ring (lib)".into(),
                percentage: Some(72),
                done: false,
                completed_at: None,
            },
        ],
        lsp_progress_frame: 8,
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 10);
    let panel =
        crate::ui::render::explorer_panel_width(&loaded.config, 80).expect("explorer width");
    let row = (0..10)
        .find(|row| frame.grid.row_text(*row).contains("ring (lib)"))
        .unwrap_or_else(|| {
            panic!(
                "progress row: {:?}",
                (0..10)
                    .map(|row| frame.grid.row_text(row))
                    .collect::<Vec<_>>()
            )
        });
    assert!(!frame.grid.row_text(row).contains("root:1"));
    assert!((panel.saturating_add(1)..79).any(|x| {
        frame
            .grid
            .get(x, row)
            .is_some_and(|cell| cell.symbol == "⠇" && cell.style == crate::ui::Style::Accent)
    }));
    assert_eq!(frame.grid.get(78, row).unwrap().symbol, "r");
    for x in 66..79 {
        assert_eq!(
            frame.grid.get(x, row).unwrap().style,
            crate::ui::Style::Accent
        );
    }
    assert_eq!(frame.grid.get(79, row).unwrap().symbol, " ");
    let completed_row = (0..10)
        .find(|row| frame.grid.row_text(*row).contains("Roots Scanned"))
        .expect("completed progress row");
    assert!((panel.saturating_add(1)..79).any(|x| {
        frame
            .grid
            .get(x, completed_row)
            .is_some_and(|cell| cell.symbol == "✓")
    }));

    ui.lsp_progress_frame = 4;
    let next = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 10);
    assert!(
        (panel.saturating_add(1)..79)
            .any(|x| next.grid.get(x, row).is_some_and(|cell| cell.symbol == "⠼"))
    );
}
