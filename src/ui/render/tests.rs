use std::path::PathBuf;

use crate::editor::EditorState;

use super::{
    CompletionItem, DocumentationMarkupKind, DocumentationPopupKind, DocumentationPopupState,
    DocumentationSection, EditorWindowBinding, EditorWindowLayoutState, PromptKind, PromptState,
    UiState, prompt_geometry,
};

fn editor_layout(state: &EditorState) -> EditorWindowLayoutState {
    fn convert(
        node: &crate::editor::LayoutNode,
        state: &EditorState,
        next: &mut u64,
        bindings: &mut Vec<EditorWindowBinding>,
    ) -> crate::ui::window::TiledLayoutNode {
        match node {
            crate::editor::LayoutNode::Pane(pane) => {
                *next += 1;
                let window = crate::ui::window::WindowId(*next);
                bindings.push(EditorWindowBinding {
                    window,
                    pane: *pane,
                    view: state.panes[*pane].view,
                });
                crate::ui::window::TiledLayoutNode::Window(window)
            }
            crate::editor::LayoutNode::Split {
                axis,
                ratio,
                first,
                second,
            } => crate::ui::window::TiledLayoutNode::Split {
                axis: match axis {
                    crate::editor::Axis::Horizontal => crate::ui::window::TileAxis::Horizontal,
                    crate::editor::Axis::Vertical => crate::ui::window::TileAxis::Vertical,
                },
                ratio: *ratio,
                first: Box::new(convert(first, state, next, bindings)),
                second: Box::new(convert(second, state, next, bindings)),
            },
        }
    }

    let tab = &state.tabs[state.active_tab.expect("active tab")];
    let mut bindings = Vec::new();
    let root = convert(&tab.root, state, &mut 0, &mut bindings);
    let focused = bindings
        .iter()
        .find(|binding| binding.pane == tab.focused)
        .map(|binding| binding.window);
    EditorWindowLayoutState {
        source_revision: state.window_revision().expect("test revision"),
        layout: crate::ui::window::TiledLayoutSnapshot { root, focused },
        bindings,
    }
}

fn ui_with_editor_layout(state: &EditorState) -> UiState {
    UiState {
        editor_window_layout: Some(editor_layout(state)),
        ..UiState::default()
    }
}

fn attach_test_compositor(
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
        .map(|binding| crate::ui::window::CompositorWindowSnapshot {
            window: binding.window,
            placement: crate::ui::window::WindowPlacement::Tiled,
            adapter: crate::ui::window::ContentAdapterKind::TextBuffer,
            focusable: true,
        })
        .collect::<Vec<_>>();
    let explorer_window = ui
        .explorer
        .as_ref()
        .map(|_| crate::ui::window::WindowId(10_001));
    if let Some(window) = explorer_window {
        let size =
            super::explorer_panel_width(config, width).map_or(1, |panel| panel.saturating_add(1));
        windows.push(crate::ui::window::CompositorWindowSnapshot {
            window,
            placement: crate::ui::window::WindowPlacement::Docked {
                edge: crate::ui::window::DockEdge::Left,
                size,
            },
            adapter: crate::ui::window::ContentAdapterKind::DeclarativeUi,
            focusable: true,
        });
    }
    let terminal_window = ui
        .terminal_panel
        .as_ref()
        .map(|terminal| (terminal.id, crate::ui::window::WindowId(10_002)));
    if let Some((_, window)) = terminal_window {
        let size = super::terminal_panel_geometry(
            config,
            height,
            ui.terminal_panel
                .as_ref()
                .expect("terminal")
                .requested_height,
        )
        .map_or(1, |geometry| geometry.height);
        windows.push(crate::ui::window::CompositorWindowSnapshot {
            window,
            placement: crate::ui::window::WindowPlacement::Docked {
                edge: crate::ui::window::DockEdge::Bottom,
                size,
            },
            adapter: crate::ui::window::ContentAdapterKind::TerminalSession,
            focusable: true,
        });
    }
    let dashboard_window = ui.dashboard.then_some(crate::ui::window::WindowId(10_003));
    if let Some(window) = dashboard_window {
        windows.push(crate::ui::window::CompositorWindowSnapshot {
            window,
            placement: crate::ui::window::WindowPlacement::Overlay(crate::ui::window::WindowRect {
                x: 0,
                y: 0,
                width,
                height,
            }),
            adapter: crate::ui::window::ContentAdapterKind::DeclarativeUi,
            focusable: true,
        });
    }
    let picker_window = ui
        .picker
        .as_ref()
        .and_then(|_| {
            super::picker_geometry(
                config,
                crate::ui::window::WindowRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                },
            )
        })
        .map(|rect| (crate::ui::window::WindowId(10_004), rect));
    if let Some((window, rect)) = picker_window {
        windows.push(crate::ui::window::CompositorWindowSnapshot {
            window,
            placement: crate::ui::window::WindowPlacement::Overlay(rect),
            adapter: crate::ui::window::ContentAdapterKind::DeclarativeUi,
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
    let snapshot = crate::ui::window::CompositorSnapshot {
        tab: crate::ui::window::WorkspaceTabId(1),
        tiled: layout.map(|layout| layout.layout.clone()),
        windows,
        focused,
    };
    let bounds = crate::ui::window::WindowRect {
        x: 0,
        y: 0,
        width,
        height,
    };
    let preliminary = snapshot.project(
        bounds,
        config.ui.panes.minimum_width,
        config.ui.panes.minimum_height,
    );
    let (top, bottom) =
        super::editor_workspace_insets(state, ui, config, preliminary.workspace_bounds.height);
    ui.window_graph_revision = Some(1);
    ui.compositor_layout = Some(super::CompositorLayoutState {
        source_revision: state.window_revision().expect("test revision"),
        graph_revision: 1,
        projection: snapshot.project_with_tiled_insets(
            bounds,
            config.ui.panes.minimum_width,
            config.ui.panes.minimum_height,
            top,
            bottom,
        ),
        surfaces: super::NativeSurfaceWindowBinding {
            explorer: explorer_window,
            terminal: terminal_window,
            dashboard: dashboard_window,
            picker: picker_window.map(|(window, _)| window),
        },
    });
}

fn render(
    state: &EditorState,
    ui: &UiState,
    config: &crate::config::Config,
    dashboard: &crate::config::DashboardConfig,
    width: u16,
    height: u16,
) -> super::Frame {
    let mut ui = ui.clone();
    attach_test_compositor(state, &mut ui, config, width, height);
    super::render(state, &ui, config, dashboard, width, height)
}

#[test]
fn workspace_frame_respects_nonzero_compositor_origin_for_side_and_top_docks() {
    let config = crate::config::Config::default();
    let mut local = crate::ui::CellGrid::new(6, 3);
    local.set_cell(0, 0, "X", crate::ui::Style::Default);
    local.set_cell(5, 2, "Y", crate::ui::Style::Default);
    let frame = super::place_workspace_frame(
        super::Frame {
            grid: local,
            cursor: Some((1, 1, crossterm::cursor::SetCursorStyle::SteadyBlock)),
        },
        crate::ui::window::WindowRect {
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

    let terminal = super::TerminalPanelState {
        id: 1,
        profile: "side".into(),
        tabs: vec![super::TerminalTabState {
            id: 1,
            profile: "side".into(),
            running: true,
            exit_code: None,
        }],
        mode: super::TerminalPanelMode::Input,
        running: true,
        exit_code: None,
        truncated: false,
        unsupported_sequences: 0,
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
    let composed = super::dock_terminal_panel(
        frame,
        &terminal,
        super::TerminalPanelGeometry {
            x: 0,
            width: 5,
            top: 0,
            height: 10,
            body_top: 1,
            body_height: 8,
            status_y: 9,
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
fn prompt_is_globally_centered_and_clamped() {
    let configured = crate::config::CommandLineConfig::default();
    let geometry = prompt_geometry(120, 40, &configured, false);
    assert_eq!(geometry.left, 30);
    assert_eq!(geometry.top, 2);
    assert_eq!(geometry.width, 60);
    let narrow = prompt_geometry(30, 8, &configured, false);
    assert_eq!(narrow.left, 1);
    assert_eq!(narrow.width, 28);
}

#[test]
fn prompt_clears_and_clips_the_editor_layer_beneath_its_box() {
    let mut state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    state.open_text(
        crate::editor::text::TextBuffer::from_text(&vec!["X".repeat(100); 10].join("\n")),
        None,
    );
    let loaded = crate::config::load_bundled_config();
    let base_ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        ..UiState::default()
    };
    let geometry = prompt_geometry(100, 20, &loaded.config.ui.command_line, false);
    let probe_x = geometry.left + geometry.width - 2;
    let probe_y = geometry.top + 1;
    let base = render(&state, &base_ui, &loaded.config, &loaded.dashboard, 100, 20);
    assert_eq!(
        base.grid.get(probe_x, probe_y).expect("editor cell").symbol,
        "X"
    );

    let ui = UiState {
        prompt: Some(PromptState::new(PromptKind::Command)),
        ..base_ui.clone()
    };
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 20);
    assert_eq!(
        frame
            .grid
            .get(probe_x, probe_y)
            .expect("cleared cell")
            .symbol,
        " "
    );
    assert_eq!(
        frame
            .grid
            .get(geometry.left + geometry.width - 1, probe_y)
            .expect("right border")
            .style,
        crate::ui::Style::Border
    );

    let mut long_prompt = PromptState::new(PromptKind::Command);
    long_prompt.input = "x".repeat(200);
    long_prompt.cursor = long_prompt.input.len();
    let clipped = render(
        &state,
        &UiState {
            prompt: Some(long_prompt),
            ..base_ui
        },
        &loaded.config,
        &loaded.dashboard,
        100,
        20,
    );
    let outside_x = geometry.left + geometry.width;
    assert_eq!(
        clipped.grid.get(outside_x, probe_y),
        base.grid.get(outside_x, probe_y)
    );
}

#[test]
fn documentation_popup_preserves_lines_clamps_and_protects_status_row() {
    let mut state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    state.open_text(
        crate::editor::text::TextBuffer::from_text("cursor\nline\nline\nline\nline\nline"),
        None,
    );
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
                lines: vec!["first line".into(), "wide 界界 clipped safely".into()],
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
    assert!(screen.last().is_some_and(|line| line.contains("NORMAL")));
}

#[test]
fn completion_attaches_below_globally_centered_prompt() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    let mut prompt = PromptState::new(PromptKind::Command);
    prompt.input = "e".into();
    prompt.cursor = 1;
    prompt.completions = vec![
        CompletionItem {
            label: "e".into(),
            detail: "Open file".into(),
        },
        CompletionItem {
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
    let geometry = prompt_geometry(100, 20, &config.ui.command_line, false);
    assert_eq!(geometry.left, 20);
    assert!(frame.grid.row_text(geometry.top + 3).contains("Open file"));
}

#[test]
fn prompt_completion_viewport_follows_a_selected_item_beyond_its_first_page() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    let mut prompt = PromptState::new(PromptKind::Command);
    prompt.completions = (0..6)
        .map(|index| CompletionItem {
            label: format!("candidate-{index}"),
            detail: format!("detail-{index}"),
        })
        .collect();
    prompt.selected_completion = Some(5);
    let ui = UiState {
        dashboard: true,
        prompt: Some(prompt),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.ui.command_line.completion_max_rows = 3;
    let frame = render(&state, &ui, &config, &loaded.dashboard, 100, 20);
    let screen = (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>();

    assert!(screen.iter().any(|line| line.contains("candidate-5")));
    assert!(!screen.iter().any(|line| line.contains("candidate-0")));
}

#[test]
fn block_selection_projection_uses_shared_display_cell_ranges() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state.open_text(
        crate::editor::text::TextBuffer::from_text("\tZ\n界ab"),
        None,
    );
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
    let gutter = super::editor_state_gutter_width(
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
fn missing_application_window_snapshot_fails_closed() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state.open_text(
        crate::editor::text::TextBuffer::from_text("not rendered"),
        None,
    );
    let loaded = crate::config::load_bundled_config();
    let ui = UiState::default();
    assert!(super::pane_viewports(&state, &ui, &loaded.config, 40, 8).is_empty());
    let frame = super::render(&state, &ui, &loaded.config, &loaded.dashboard, 40, 8);
    assert!(frame.cursor.is_none());
    assert!(!(0..8).any(|row| frame.grid.row_text(row).contains("not rendered")));

    let stale = ui_with_editor_layout(&state);
    state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("change source layout");
    assert!(super::pane_viewports(&state, &stale, &loaded.config, 40, 8).is_empty());
    let frame = super::render(&state, &stale, &loaded.config, &loaded.dashboard, 40, 8);
    assert!(frame.cursor.is_none());
}

#[test]
fn split_tree_and_tab_line_render_real_independent_views() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state.open_text(
        crate::editor::text::TextBuffer::from_text("left\nright"),
        None,
    );
    state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("split");
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.ui.tab_line.hide_when_single = false;
    let ui = ui_with_editor_layout(&state);
    let frame = render(&state, &ui, &config, &loaded.dashboard, 20, 8);
    assert!(frame.grid.row_text(0).contains("[scratch]"));
    assert_eq!(frame.grid.get(9, 2).expect("split border").symbol, "│");
    assert!(frame.cursor.is_some_and(|(x, y, _)| x > 9 && y > 0));
}

#[test]
fn buffer_line_lists_unique_dirty_content_and_follows_active_overflow() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (first, _) = state.open_text(
        crate::editor::text::TextBuffer::from_text("first"),
        Some(PathBuf::from("/workspace/src/main.rs")),
    );
    state.open_text(
        crate::editor::text::TextBuffer::from_text("second"),
        Some(PathBuf::from("/workspace/tests/main.rs")),
    );
    state.open_text(
        crate::editor::text::TextBuffer::from_text("third"),
        Some(PathBuf::from("/workspace/very-long-active-buffer-name.rs")),
    );
    state.buffers[first].dirty = true;
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.ui.tab_line.enabled = false;
    config.ui.buffer_line.maximum_name_width = 18;
    let entries = vec![
        super::ContentTabEntry {
            identity: super::ContentTabIdentity(crate::ui::window::ContentId(1)),
            kind: super::ContentTabKind::Text,
            title: "src/main.rs ·1".into(),
            active: false,
            dirty: true,
            pinned: false,
            diagnostic: None,
            icon: "R".into(),
            closeable: true,
        },
        super::ContentTabEntry {
            identity: super::ContentTabIdentity(crate::ui::window::ContentId(2)),
            kind: super::ContentTabKind::Text,
            title: "tests/main.rs ·2".into(),
            active: false,
            dirty: false,
            pinned: false,
            diagnostic: None,
            icon: "R".into(),
            closeable: true,
        },
        super::ContentTabEntry {
            identity: super::ContentTabIdentity(crate::ui::window::ContentId(3)),
            kind: super::ContentTabKind::Text,
            title: "very-long-active-buffer-name.rs".into(),
            active: true,
            dirty: false,
            pinned: false,
            diagnostic: None,
            icon: "R".into(),
            closeable: true,
        },
    ];
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        content_tabs: entries,
        ..UiState::default()
    };

    let frame = render(&state, &ui, &config, &loaded.dashboard, 24, 7);
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
    let mut config = loaded.config;
    config.ui.buffer_line.active_format = "{title}{dirty}".into();
    config.ui.buffer_line.inactive_format = "{title}{dirty}".into();
    let entry = super::ContentTabEntry {
        identity: super::ContentTabIdentity(crate::ui::window::ContentId(9)),
        kind: super::ContentTabKind::Text,
        title: "a.rs".into(),
        active: true,
        dirty: true,
        pinned: true,
        diagnostic: Some(super::ContentTabDiagnostic {
            severity: super::ContentTabDiagnosticSeverity::Error,
            count: 2,
        }),
        icon: "R".into(),
        closeable: true,
    };
    let width = super::content_tab_width(&entry, &config);
    let mut grid = crate::ui::CellGrid::new(u16::try_from(width).unwrap(), 1);
    super::render_content_tab_line(&mut grid, std::slice::from_ref(&entry), &config, 0);
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
            super::content_tab_at_column(
                std::slice::from_ref(&entry),
                &config,
                u16::try_from(width).unwrap(),
                column
            ),
            Some(entry.identity)
        );
    }
    assert_eq!(
        super::content_tab_at_column(
            std::slice::from_ref(&entry),
            &config,
            u16::try_from(width).unwrap(),
            u16::try_from(width).unwrap()
        ),
        None
    );
    let mut entries = vec![entry.clone(), entry.clone(), entry.clone()];
    for (index, entry) in entries.iter_mut().enumerate() {
        entry.identity = super::ContentTabIdentity(crate::ui::window::ContentId(index as u64 + 1));
        entry.title = char::from(b'a' + index as u8).to_string();
        entry.active = index == 2;
    }
    let left = super::text_width(
        &config.ui.buffer_line.overflow_left,
        config.editor.ambiguous_width,
    );
    for column in 0..u16::try_from(left).unwrap() {
        assert_eq!(
            super::content_tab_at_column(&entries, &config, 8, column),
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
    config.ui.buffer_line.show_icons = false;
    config.ui.buffer_line.show_pins = false;
    config.ui.buffer_line.show_diagnostics = false;
    config.ui.buffer_line.show_close = false;
    let first = super::content_tab_width(&right_entries[0], &config);
    assert_eq!(
        super::content_tab_at_column(&right_entries, &config, 5, u16::try_from(first).unwrap()),
        None
    );
}

#[test]
fn buffer_line_begins_after_the_explorer_dock() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state.open_text(
        crate::editor::text::TextBuffer::from_text("one"),
        Some(PathBuf::from("/workspace/one.rs")),
    );
    state.open_text(
        crate::editor::text::TextBuffer::from_text("two"),
        Some(PathBuf::from("/workspace/two.rs")),
    );
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        content_tabs: vec![
            super::ContentTabEntry {
                identity: super::ContentTabIdentity(crate::ui::window::ContentId(1)),
                kind: super::ContentTabKind::Text,
                title: "one.rs".into(),
                active: false,
                dirty: false,
                pinned: false,
                diagnostic: None,
                icon: "R".into(),
                closeable: true,
            },
            super::ContentTabEntry {
                identity: super::ContentTabIdentity(crate::ui::window::ContentId(2)),
                kind: super::ContentTabKind::Text,
                title: "two.rs".into(),
                active: true,
                dirty: false,
                pinned: false,
                diagnostic: None,
                icon: "R".into(),
                closeable: true,
            },
        ],
        explorer: Some(super::ExplorerUiState {
            title: "Explorer".into(),
            rows: Vec::new(),
            scroll: 0,
            loading: false,
            total: 0,
            preview_lines: Vec::new(),
        }),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 10);
    let panel = super::explorer_panel_width(&loaded.config, 80).expect("explorer width");
    assert!(frame.grid.row_text(0).contains("Explorer"));
    let editor_line = (panel.saturating_add(1)..80)
        .map(|x| frame.grid.get(x, 0).map_or("", |cell| cell.symbol.as_str()))
        .collect::<String>();
    assert!(editor_line.contains("one.rs"));
    assert!(editor_line.contains("two.rs"));
}

#[test]
fn lsp_progress_is_animated_right_aligned_and_hides_transport_identity() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(PathBuf::from("/workspace/main.rs")),
    );
    let mut ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        explorer: Some(super::ExplorerUiState {
            title: "Explorer".into(),
            rows: Vec::new(),
            scroll: 0,
            loading: false,
            total: 0,
            preview_lines: Vec::new(),
        }),
        lsp_progress: vec![
            super::UiLspProgress {
                server_key: "root:1:rust-analyzer".into(),
                token: "roots".into(),
                server: "rust-analyzer".into(),
                title: "Roots Scanned".into(),
                message: String::new(),
                percentage: None,
                done: true,
                completed_at: Some(std::time::Instant::now()),
            },
            super::UiLspProgress {
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
    let panel = super::explorer_panel_width(&loaded.config, 80).expect("explorer width");
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
    let (_, view) = state.open_text(
        crate::editor::text::TextBuffer::from_text(
            "a\t界e\u{301}zz\nfold\nhidden one\nhidden two\ntail tail",
        ),
        None,
    );
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = true;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    let ui = UiState {
        lsp_folds: vec![super::UiFold {
            view,
            start_line: 1,
            end_line: 3,
            collapsed: true,
        }],
        ..ui_with_editor_layout(&state)
    };
    let buffer = state.views[view].buffer;
    let model =
        super::VisualRowModel::new(&state.buffers[buffer].text, view, &ui, &config, 4, true);
    assert_eq!(model.rows_for_line(0), 3);
    assert_eq!(
        model.normalize(super::VisualRowAnchor {
            line: 3,
            wrap_row: 8
        }),
        super::VisualRowAnchor {
            line: 1,
            wrap_row: 0
        }
    );
    let start = super::VisualRowAnchor::default();
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
    let (buffer, view) =
        state.open_text(crate::editor::text::TextBuffer::from_text("aa界界"), None);
    let ui = ui_with_editor_layout(&state);
    let model =
        super::VisualRowModel::new(&state.buffers[buffer].text, view, &ui, &config, 3, true);
    assert_eq!(model.rows_for_line(0), 3);
    assert_eq!(
        model.line_rows(0),
        vec![
            super::VisualRowSlice {
                byte_start: 0,
                byte_end: 2,
                source_column_start: 0,
                used_cells: 2,
            },
            super::VisualRowSlice {
                byte_start: 2,
                byte_end: 5,
                source_column_start: 2,
                used_cells: 2,
            },
            super::VisualRowSlice {
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
    let tiny_model =
        super::VisualRowModel::new(&state.buffers[buffer].text, view, &ui, &config, 1, true);
    assert_eq!(tiny_model.rows_for_line(0), 2);
    let tiny = render(&state, &ui, &config, &loaded.dashboard, 1, 2);
    assert_eq!(
        tiny.grid.get(0, 0).unwrap().symbol,
        config.editor.control_character_replacement
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
        for width in 1..=4 {
            let mut state = EditorState::new(PathBuf::from("/tmp"));
            let (buffer, view) =
                state.open_text(crate::editor::text::TextBuffer::from_text(sample), None);
            let ui = ui_with_editor_layout(&state);
            let model = super::VisualRowModel::new(
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
                assert!(sample.is_char_boundary(row.byte_start));
                assert!(sample.is_char_boundary(row.byte_end));
                if let Some(next) = rows.get(index + 1) {
                    assert_eq!(row.byte_end, next.byte_start);
                    let current = super::VisualRowAnchor {
                        line: 0,
                        wrap_row: index,
                    };
                    let following = model.next(current);
                    assert_eq!(following.wrap_row, index + 1);
                    assert_eq!(model.previous(following), current);
                }
                let anchor = super::VisualRowAnchor {
                    line: 0,
                    wrap_row: index,
                };
                let bytes = (0..usize::from(width))
                    .map(|cell| model.byte_at_cell(anchor, cell))
                    .collect::<Vec<_>>();
                assert!(bytes.windows(2).all(|pair| pair[0] <= pair[1]));
                assert!(bytes.iter().all(|byte| {
                    *byte >= row.byte_start
                        && *byte <= row.byte_end
                        && sample.is_char_boundary(*byte)
                }));
            }
            let last = super::VisualRowAnchor {
                line: 0,
                wrap_row: rows.len() - 1,
            };
            assert_eq!(
                model.backward(model.forward(super::VisualRowAnchor::default(), 999), 999),
                super::VisualRowAnchor::default()
            );
            assert_eq!(model.forward(last, 1), last);
        }
    }
}

#[test]
fn overlapping_collapsed_folds_normalize_to_one_reversible_interval() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state.open_text(
        crate::editor::text::TextBuffer::from_text("zero\none\ntwo\nthree\nfour\nfive"),
        None,
    );
    let loaded = crate::config::load_bundled_config();
    let ui = UiState {
        lsp_folds: vec![super::UiFold {
            view,
            start_line: 1,
            end_line: 3,
            collapsed: true,
        }],
        syntax_folds: vec![super::UiFold {
            view,
            start_line: 2,
            end_line: 4,
            collapsed: true,
        }],
        ..ui_with_editor_layout(&state)
    };
    let model = super::VisualRowModel::new(
        &state.buffers[buffer].text,
        view,
        &ui,
        &loaded.config,
        20,
        true,
    );
    let fold_start = super::VisualRowAnchor {
        line: 1,
        wrap_row: 0,
    };
    let after = model.next(fold_start);
    assert_eq!(after.line, 5);
    assert_eq!(model.previous(after), fold_start);
    for hidden in 2..=4 {
        assert_eq!(
            model.normalize(super::VisualRowAnchor {
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
    state.open_text(
        crate::editor::text::TextBuffer::from_text("abcdefghij"),
        None,
    );
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = true;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);
    let frame = render(&state, &ui, &config, &loaded.dashboard, 5, 3);
    assert!(frame.grid.row_text(0).starts_with("abcde"));
    assert!(frame.grid.row_text(1).starts_with("fghij"));

    let view = state.active_view.expect("view");
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
    let (buffer, view) = state.open_text(crate::editor::text::TextBuffer::from_text(text), None);
    state.views[view].cursor = crate::editor::text::ByteOffset("let value".len());
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = false;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        lsp_inlay_hints: vec![super::UiInlayHint {
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
fn non_wrapped_horizontal_projection_snaps_graphemes_and_preserves_source_tabs() {
    let text = "a\t界e\u{301}bcdef";
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state.open_text(crate::editor::text::TextBuffer::from_text(text), None);
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
        syntax_spans: vec![super::UiSyntaxSpan {
            buffer,
            revision: state.buffers[buffer].text.revision(),
            range: crate::editor::text::ByteRange::new(
                crate::editor::text::ByteOffset("a\t界".len()),
                crate::editor::text::ByteOffset("a\t界e\u{301}".len()),
            )
            .expect("combining syntax range"),
            role: crate::language::syntax::SyntaxRole::String,
        }],
        lsp_inlay_hints: vec![super::UiInlayHint {
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
        crate::ui::Style::SyntaxString
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
        super::snap_display_column("a·b", 2, 4, config.editor.ambiguous_width),
        2
    );
    config.editor.ambiguous_width = crate::config::AmbiguousWidth::Wide;
    assert_eq!(
        super::snap_display_column("a·b", 2, 4, config.editor.ambiguous_width),
        1
    );
}

#[test]
fn configurable_indent_guides_follow_leading_space_and_tab_display_columns() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state.open_text(
        crate::editor::text::TextBuffer::from_text("root\n    one\n        two\n\t\ttab\n"),
        Some(PathBuf::from("/workspace/file.rs")),
    );
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);
    let frame = super::render_editor_view(&state, view, &ui, &config, 20, 5);

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
    let disabled = super::render_editor_view(&state, view, &ui, &config, 20, 5);
    assert_eq!(disabled.grid.get(0, 1).unwrap().symbol, " ");
    assert_eq!(disabled.grid.get(4, 2).unwrap().symbol, " ");
    assert_eq!(disabled.grid.get(4, 3).unwrap().symbol, " ");

    config.editor.indent_guides.enabled = true;
    config.editor.ambiguous_width = crate::config::AmbiguousWidth::Wide;
    let wide = super::render_editor_view(&state, view, &ui, &config, 20, 5);
    assert_eq!(wide.grid.get(4, 2).unwrap().symbol, "|");
    assert!(!wide.grid.get(4, 2).unwrap().continuation);
}

#[test]
fn active_indent_scope_guide_is_subtly_elevated_from_normal_guides() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state.open_text(
        crate::editor::text::TextBuffer::from_text(
            "fn first() {\n    first();\n}\nfn second() {\n    second();\n}\n",
        ),
        Some(PathBuf::from("/workspace/file.rs")),
    );
    state.views[view].cursor = state.buffers[state.views[view].buffer]
        .text
        .line_to_byte(5)
        .expect("closing brace line");
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);
    let frame = super::render_editor_view(&state, view, &ui, &config, 30, 7);

    assert_eq!(
        frame.grid.get(0, 1).expect("inactive guide").style,
        crate::ui::Style::Border
    );
    assert_eq!(
        frame.grid.get(0, 4).expect("active scope guide").style,
        crate::ui::Style::Muted
    );
    assert_eq!(frame.grid.get(0, 5).expect("closing brace").symbol, "}");

    config.editor.indent_guides.active_scope = false;
    let inactive_only = super::render_editor_view(&state, view, &ui, &config, 30, 7);
    assert_eq!(
        inactive_only
            .grid
            .get(0, 4)
            .expect("inactive-only guide")
            .style,
        crate::ui::Style::Border
    );
}

#[test]
fn active_indent_scope_tracks_the_cursor_display_column() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state.open_text(
            crate::editor::text::TextBuffer::from_text(
                "fn run() {\n    if outer {\n        if inner {\n            work();\n        }\n    }\n}\n",
            ),
            Some(PathBuf::from("/workspace/file.rs")),
        );
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
    let outer = super::render_editor_view(&state, view, &ui, &config, 30, 8);
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
    let middle = super::render_editor_view(&state, view, &ui, &config, 30, 8);
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
    let inner = super::render_editor_view(&state, view, &ui, &config, 30, 8);
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
        );
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
    let before_delimiter = super::render_editor_view(&state, view, &ui, &config, 30, 8);
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
    let on_delimiter = super::render_editor_view(&state, view, &ui, &config, 30, 8);
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
fn immutable_git_signs_use_the_reserved_gutter_and_exact_buffer_revision() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, _) = state.open_text(
        crate::editor::text::TextBuffer::from_text("added\nchanged\ndeleted\n"),
        Some(PathBuf::from("/workspace/file.rs")),
    );
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    let revision = state.buffers[buffer].text.revision().0;
    ui.git_signs = vec![
        super::UiGitSign {
            buffer,
            buffer_revision: revision,
            root_revision: 1,
            status_revision: 1,
            repository_id: crate::workspace::git::GitRepositoryId(1),
            line: 0,
            kind: super::UiGitSignKind::Added,
        },
        super::UiGitSign {
            buffer,
            buffer_revision: revision,
            root_revision: 1,
            status_revision: 1,
            repository_id: crate::workspace::git::GitRepositoryId(1),
            line: 1,
            kind: super::UiGitSignKind::Changed,
        },
        super::UiGitSign {
            buffer,
            buffer_revision: revision,
            root_revision: 1,
            status_revision: 1,
            repository_id: crate::workspace::git::GitRepositoryId(1),
            line: 2,
            kind: super::UiGitSignKind::Deleted,
        },
    ];
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 20, 5);
    let gutter = super::editor_gutter_width(&state.buffers[buffer].text, &loaded.config.editor, 20);
    let sign_column = super::git_sign_column(gutter, &loaded.config.editor).unwrap();
    assert_eq!(
        (
            frame.grid.get(sign_column, 0).unwrap().symbol.as_str(),
            frame.grid.get(sign_column, 0).unwrap().style,
        ),
        ("│", crate::ui::Style::GitSignAdded)
    );
    assert_eq!(
        (
            frame.grid.get(sign_column, 1).unwrap().symbol.as_str(),
            frame.grid.get(sign_column, 1).unwrap().style,
        ),
        ("│", crate::ui::Style::GitSignChanged)
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
fn neutral_git_sign_column_continues_through_wraps_and_end_of_buffer_rows() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (_, view) = state.open_text(
        crate::editor::text::TextBuffer::from_text("a deliberately wrapped line"),
        Some(PathBuf::from("/workspace/file.rs")),
    );
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = true;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);
    let frame = super::render_editor_view(&state, view, &ui, &config, 10, 8);
    let gutter = super::editor_state_gutter_width(
        &state,
        &state.buffers[state.views[view].buffer].text,
        &config.editor,
        10,
    );

    let sign_column = super::git_sign_column(gutter, &config.editor).unwrap();
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
    let (_, view) = state.open_text(
        crate::editor::text::TextBuffer::from_text(&source),
        Some(PathBuf::from("/workspace/file.rs")),
    );
    state.views[view].scroll_line = 999;
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.ui.status_line.enabled = false;
    let ui = ui_with_editor_layout(&state);
    let frame = super::render_editor_view(&state, view, &ui, &config, 20, 4);
    let gutter = super::editor_state_gutter_width(
        &state,
        &state.buffers[state.views[view].buffer].text,
        &config.editor,
        20,
    );

    assert_eq!(super::editor_line_number_digits(&state), 4);
    let sign_column = super::git_sign_column(gutter, &config.editor).unwrap();
    for row in 1..frame.grid.height() {
        assert_eq!(frame.grid.get(0, row).unwrap().symbol, " ");
        assert_eq!(frame.grid.get(3, row).unwrap().symbol, "~");
        assert_eq!(frame.grid.get(sign_column, row).unwrap().symbol, "│");
    }
}

#[test]
fn line_number_gutter_uses_the_largest_open_buffer_across_tabs() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (short, short_view) = state.open_text(
        crate::editor::text::TextBuffer::from_text("short\n"),
        Some(PathBuf::from("/workspace/short.rs")),
    );
    let first_tab = state.active_tab.unwrap();
    state.new_tab().unwrap();
    let long_text = (0..1_000).map(|_| "long\n").collect::<String>();
    let (long, long_view) = state.open_text(
        crate::editor::text::TextBuffer::from_text(&long_text),
        Some(PathBuf::from("/workspace/long.rs")),
    );
    let loaded = crate::config::load_bundled_config();
    assert_eq!(super::editor_line_number_digits(&state), 4);
    let short_gutter = super::editor_state_gutter_width(
        &state,
        &state.buffers[short].text,
        &loaded.config.editor,
        80,
    );
    let long_gutter = super::editor_state_gutter_width(
        &state,
        &state.buffers[long].text,
        &loaded.config.editor,
        80,
    );
    assert_eq!(short_gutter, long_gutter);
    assert_eq!(short_gutter, 7);
    state.focus_tab(-1, true).unwrap();
    assert_eq!(state.active_tab, Some(first_tab));
    let ui = ui_with_editor_layout(&state);
    let frame = super::render_editor_view(&state, short_view, &ui, &loaded.config, 80, 4);
    let short_sign = super::git_sign_column(short_gutter, &loaded.config.editor).unwrap();
    let long_sign = super::git_sign_column(long_gutter, &loaded.config.editor).unwrap();
    assert_eq!(frame.grid.get(3, 0).unwrap().symbol, "1");
    assert_eq!(frame.grid.get(short_sign, 0).unwrap().symbol, "│");
    assert_eq!(
        frame.grid.get(short_sign, 0).unwrap().style,
        crate::ui::Style::Muted
    );
    assert_eq!(frame.grid.get(short_sign + 1, 0).unwrap().symbol, " ");
    assert_eq!(frame.grid.get(short_gutter, 0).unwrap().symbol, "s");
    let long_frame = super::render_editor_view(&state, long_view, &ui, &loaded.config, 80, 4);
    assert_eq!(long_frame.grid.get(long_sign, 0).unwrap().symbol, "│");
    assert_eq!(long_frame.grid.get(long_sign + 1, 0).unwrap().symbol, " ");
    assert_eq!(long_frame.grid.get(long_gutter, 0).unwrap().symbol, "l");
}

#[test]
fn concrete_gutter_reserves_content_at_zero_one_and_two_cell_widths() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, view) = state.open_text(crate::editor::text::TextBuffer::from_text("a\tX"), None);
    state.views[view].cursor = crate::editor::text::ByteOffset("a\t".len());
    state.views[view].scroll_column = 4;
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.wrap = false;
    config.editor.line_numbers = crate::config::LineNumbers::Absolute;
    config.editor.line_number_separator = "1234567890123456".into();
    config.editor.gutter_padding = 20;
    config.ui.status_line.enabled = false;

    assert_eq!(
        super::editor_gutter_width(&state.buffers[buffer].text, &config.editor, 0),
        0
    );
    assert_eq!(
        super::editor_content_width(&state.buffers[buffer].text, &config.editor, 0),
        0
    );
    let ui = ui_with_editor_layout(&state);
    let zero = render(&state, &ui, &config, &loaded.dashboard, 0, 2);
    assert_eq!(zero.grid.width(), 0);
    assert!(zero.cursor.is_none());

    let mut split_state = EditorState::new(PathBuf::from("/tmp"));
    split_state.open_text(crate::editor::text::TextBuffer::from_text("x"), None);
    split_state
        .split_active(crate::editor::Axis::Horizontal, 100)
        .expect("minimum-ratio split");
    split_state.focus_pane(-1, false).expect("zero-width leaf");
    config.ui.panes.minimum_width = 3;
    config.ui.panes.minimum_height = 2;
    let mut split_ui = ui_with_editor_layout(&split_state);
    attach_test_compositor(&split_state, &mut split_ui, &config, 6, 2);
    let zero_leaf = super::active_pane_viewport(&split_state, &split_ui, &config, 6, 2)
        .expect("focused tiny-layout fallback");
    assert_eq!(zero_leaf.width, 6);
    let split_frame = render(&split_state, &split_ui, &config, &loaded.dashboard, 6, 2);
    assert!(split_frame.cursor.is_some());

    for (width, expected_gutter) in [(1, 0), (2, 1)] {
        assert_eq!(
            super::editor_gutter_width(&state.buffers[buffer].text, &config.editor, width,),
            expected_gutter
        );
        assert_eq!(
            super::editor_content_width(&state.buffers[buffer].text, &config.editor, width,),
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
        super::editor_gutter_width(&state.buffers[buffer].text, &config.editor, 2),
        0
    );
    assert_eq!(
        super::editor_content_width(&state.buffers[buffer].text, &config.editor, 2),
        2
    );
}

#[test]
fn wrapping_ignores_horizontal_offset() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (_, view) = state.open_text(
        crate::editor::text::TextBuffer::from_text("abcdefghij"),
        None,
    );
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
fn retained_plugin_surface_rows_are_strictly_clipped_to_the_projected_window() {
    let mut frame = super::Frame {
        grid: crate::ui::CellGrid::new(12, 5),
        cursor: None,
    };
    let surface = super::PluginSurfaceUiState {
        handle: 9,
        window: crate::ui::window::WindowId(99),
        tree_revision: 1,
        scroll: 0,
        rows: vec![
            super::PluginSurfaceRow {
                node: 1,
                depth: 0,
                text: "abcdefgh".into(),
                style: crate::ui::Style::Accent,
                actionable: false,
                focused: false,
            },
            super::PluginSurfaceRow {
                node: 2,
                depth: 1,
                text: "button".into(),
                style: crate::ui::Style::Selection,
                actionable: true,
                focused: true,
            },
        ],
    };
    super::render_plugin_surface(
        &mut frame,
        &surface,
        crate::ui::window::WindowRect {
            x: 3,
            y: 1,
            width: 4,
            height: 2,
        },
    );
    assert_eq!(&frame.grid.row_text(1)[3..7], "abcd");
    assert_eq!(frame.grid.get(7, 1).expect("outside right").symbol, " ");
    assert_eq!(frame.grid.get(2, 2).expect("outside left").symbol, " ");
    assert_eq!(frame.grid.get(3, 3).expect("outside bottom").symbol, " ");
}

#[test]
fn generic_picker_is_centered_filtered_presentation_with_configured_markers() {
    let state = EditorState::new(PathBuf::from("/tmp"));
    let ui = UiState {
        dashboard: true,
        picker: Some(super::PickerUiState {
            surface: super::PickerSurfaceKind::Generic,
            title: "Commands".into(),
            query: "health".into(),
            rows: vec![super::PickerRow {
                id: "health".into(),
                label: "health.open".into(),
                detail: "Show health".into(),
                source: "core".into(),
            }],
            selected: Some(0),
            scroll: 0,
            loading: false,
        }),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 30);
    assert!((0..30).any(|row| frame.grid.row_text(row).contains("health.open")));
    assert!(frame.cursor.is_some());
}

#[test]
fn active_pane_viewport_uses_exact_focused_split_rectangle() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state.open_text(crate::editor::text::TextBuffer::from_text("one\ntwo"), None);
    state
        .split_active(crate::editor::Axis::Horizontal, 400)
        .expect("horizontal split");
    state
        .split_active(crate::editor::Axis::Vertical, 600)
        .expect("vertical split");
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    attach_test_compositor(&state, &mut ui, &loaded.config, 79, 24);
    let viewport =
        super::active_pane_viewport(&state, &ui, &loaded.config, 79, 24).expect("focused viewport");
    assert_eq!(Some(viewport.view), state.active_view);
    assert!(viewport.x > 0 && viewport.y > 0);
    assert!(viewport.width < 79 && viewport.height < 23);
    assert!(viewport.y.saturating_add(viewport.height) <= 23);
}

#[test]
fn explorer_is_a_persistent_left_dock_that_reflows_the_editor() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state.open_text(
        crate::editor::text::TextBuffer::from_text("editor content"),
        Some(PathBuf::from("/workspace/main.rs")),
    );
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        explorer: Some(super::ExplorerUiState {
            title: "Explorer".into(),
            rows: vec![
                super::ExplorerUiRow {
                    node_id: 1,
                    selected: false,
                    segments: vec![
                        super::ExplorerUiSegment {
                            text: "├─".into(),
                            role: super::ExplorerSegmentRole::Tree,
                        },
                        super::ExplorerUiSegment {
                            text: "▾".into(),
                            role: super::ExplorerSegmentRole::Icon,
                        },
                        super::ExplorerUiSegment {
                            text: " ".into(),
                            role: super::ExplorerSegmentRole::Tree,
                        },
                        super::ExplorerUiSegment {
                            text: "src".into(),
                            role: super::ExplorerSegmentRole::Directory,
                        },
                    ],
                    right_segments: vec![
                        super::ExplorerUiSegment {
                            text: "M".into(),
                            role: super::ExplorerSegmentRole::Git,
                        },
                        super::ExplorerUiSegment {
                            text: "E".into(),
                            role: super::ExplorerSegmentRole::Diagnostic,
                        },
                    ],
                },
                super::ExplorerUiRow {
                    node_id: 2,
                    selected: true,
                    segments: vec![
                        super::ExplorerUiSegment {
                            text: "│ └─".into(),
                            role: super::ExplorerSegmentRole::Tree,
                        },
                        super::ExplorerUiSegment {
                            text: "·".into(),
                            role: super::ExplorerSegmentRole::Icon,
                        },
                        super::ExplorerUiSegment {
                            text:
                                " main.rs-with-an-intentionally-overlong-name-that-must-not-spill"
                                    .into(),
                            role: super::ExplorerSegmentRole::File,
                        },
                    ],
                    right_segments: vec![super::ExplorerUiSegment {
                        text: "?".into(),
                        role: super::ExplorerSegmentRole::Diagnostic,
                    }],
                },
            ],
            scroll: 0,
            loading: false,
            total: 2,
            preview_lines: Vec::new(),
        }),
        explorer_focused: true,
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let panel = super::explorer_panel_width(&loaded.config, 100).expect("dock width");
    assert_eq!(panel, 40);
    assert_eq!(super::explorer_panel_width(&loaded.config, 30), Some(14));
    assert_eq!(super::explorer_panel_width(&loaded.config, 2), None);
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 100, 20);
    assert!(frame.grid.row_text(0).contains("Explorer"));
    assert!(frame.grid.row_text(3).contains("src"));
    assert!(frame.grid.row_text(4).contains("main.rs"));
    assert_eq!(
        frame.grid.get(1, 3).expect("tree segment").style,
        crate::ui::Style::ExplorerTree
    );
    assert_eq!(
        frame.grid.get(3, 3).expect("icon segment").style,
        crate::ui::Style::ExplorerIcon
    );
    assert_eq!(
        frame.grid.get(5, 3).expect("directory name").style,
        crate::ui::Style::ExplorerDirectory
    );
    assert_eq!(
        frame.grid.get(37, 3).expect("git decoration").style,
        crate::ui::Style::ExplorerGit
    );
    assert_eq!(
        frame.grid.get(38, 3).expect("diagnostic decoration").style,
        crate::ui::Style::ExplorerDiagnostic
    );
    assert_eq!(
        frame.grid.get(1, 4).expect("selected row").style,
        crate::ui::Style::ExplorerSelection
    );
    assert_eq!(
        frame.grid.get(panel, 3).expect("dock separator").symbol,
        "⎸"
    );
    assert_eq!(
        frame
            .grid
            .get(panel, 4)
            .expect("preserved separator")
            .symbol,
        "⎸"
    );
    assert_eq!(
        frame
            .grid
            .get(panel + 1, 4)
            .expect("editor-side cell")
            .style,
        crate::ui::Style::Muted
    );
    assert!(frame.cursor.is_some_and(|(x, y, _)| x == 1 && y == 4));
    assert!(frame.grid.row_text(19).contains("NORMAL"));
    let mut editor_ui = ui.clone();
    editor_ui.explorer_focused = false;
    let editor_frame = render(
        &state,
        &editor_ui,
        &loaded.config,
        &loaded.dashboard,
        100,
        20,
    );
    assert!(editor_frame.cursor.is_some_and(|(x, _, _)| x > panel));

    let mut ascii = loaded.config.clone();
    ascii.ui.explorer.icon_mode = crate::config::ExplorerIconMode::Ascii;
    let ascii_frame = render(&state, &ui, &ascii, &loaded.dashboard, 100, 20);
    assert_eq!(
        ascii_frame
            .grid
            .get(panel, 3)
            .expect("ASCII separator")
            .symbol,
        "|"
    );
    let mut unicode = loaded.config.clone();
    unicode.ui.explorer.icon_mode = crate::config::ExplorerIconMode::Unicode;
    let unicode_frame = render(&state, &ui, &unicode, &loaded.dashboard, 100, 20);
    assert_eq!(
        unicode_frame
            .grid
            .get(panel, 3)
            .expect("Unicode separator")
            .symbol,
        "⎸"
    );
}

#[test]
fn private_surface_cells_and_cursor_are_local_and_strictly_clipped() {
    let mut frame = super::Frame {
        grid: crate::ui::CellGrid::new(10, 4),
        cursor: None,
    };
    let surface = super::PrivateSurfaceUiState {
        handle: 3,
        window: crate::ui::window::WindowId(7),
        dimensions_revision: 2,
        surface_revision: 1,
        width: 4,
        height: 2,
        cells: vec![
            super::PrivateSurfaceCell {
                x: 0,
                y: 0,
                grapheme: "界".into(),
                continuation: false,
                style: crate::ui::Style::Accent,
            },
            super::PrivateSurfaceCell {
                x: 1,
                y: 0,
                grapheme: String::new(),
                continuation: true,
                style: crate::ui::Style::Accent,
            },
            super::PrivateSurfaceCell {
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
    super::render_private_surface(
        &mut frame,
        &surface,
        crate::ui::window::WindowRect {
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
    assert_eq!(frame.grid.get(6, 2).expect("outside right").symbol, " ");
    assert!(frame.cursor.is_some_and(|(x, y, _)| (x, y) == (5, 2)));

    let mut stale = surface.clone();
    stale.width = 5;
    let before = (frame.grid.row_text(1), frame.grid.row_text(2), frame.cursor);
    super::render_private_surface(
        &mut frame,
        &stale,
        crate::ui::window::WindowRect {
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
fn language_diagnostics_and_completion_are_projected_without_service_calls() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let path = PathBuf::from("/tmp/main.rs");
    state.open_text(
        crate::editor::text::TextBuffer::from_text("bad"),
        Some(path.clone()),
    );
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        diagnostics: vec![super::UiDiagnostic {
            path,
            severity: crate::workspace::DiagnosticSeverity::Error,
            range: crate::editor::text::ByteRange::new(
                crate::editor::text::ByteOffset(0),
                crate::editor::text::ByteOffset(3),
            )
            .expect("range"),
            message: "problem".into(),
        }],
        language_completion: Some(super::LanguageCompletionState {
            items: vec![CompletionItem {
                label: "better".into(),
                detail: "fake LSP".into(),
            }],
            selected: 0,
            scroll: 0,
        }),
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 40, 10);
    let view = state.active_view.expect("active view");
    let gutter = super::editor_state_gutter_width(
        &state,
        &state.buffers[state.views[view].buffer].text,
        &loaded.config.editor,
        40,
    );
    assert_eq!(
        frame.grid.get(gutter, 0).expect("diagnostic text").style,
        crate::ui::Style::Error
    );
    assert!((0..10).any(|row| frame.grid.row_text(row).contains("better")));
}

#[test]
fn language_completion_keeps_the_selected_item_inside_its_bounded_viewport() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    state.open_text(
        crate::editor::text::TextBuffer::from_text("value"),
        Some(PathBuf::from("/tmp/main.rs")),
    );
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        language_completion: Some(super::LanguageCompletionState {
            items: (0..20)
                .map(|index| CompletionItem {
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
    let mut config = loaded.config;
    config.ui.command_line.completion_max_rows = 4;
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

#[test]
fn terminal_is_a_full_width_bottom_dock_with_direct_cells_and_focus_cursor() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state.open_text(
        crate::editor::text::TextBuffer::from_text("editor content\nsecond line"),
        Some(PathBuf::from("/workspace/main.rs")),
    );
    state
        .split_active(crate::editor::Axis::Horizontal, 400)
        .expect("nested editor split");
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.ui.terminal_panel.height = 6;
    config.ui.terminal_panel.minimum_height = 4;
    let mut screen = crate::ui::PtyScreen::new(80, 4, 100);
    screen.feed("\u{1b}[31;1mRED\u{1b}[0m 界");
    let (cursor_column, cursor_row) = screen.cursor();
    let terminal = super::TerminalPanelState {
        id: 7,
        profile: "shell".into(),
        tabs: vec![
            super::TerminalTabState {
                id: 7,
                profile: "shell".into(),
                running: true,
                exit_code: None,
            },
            super::TerminalTabState {
                id: 8,
                profile: "logs".into(),
                running: false,
                exit_code: Some(2),
            },
        ],
        mode: super::TerminalPanelMode::Input,
        running: true,
        exit_code: None,
        truncated: false,
        unsupported_sequences: screen.unsupported_sequences(),
        focused: true,
        columns: 80,
        rows: 4,
        cells: screen.history_window(0, 4),
        cells_start: 0,
        cursor: Some((cursor_column, cursor_row)),
        selection: None,
        scrollback_lines: screen.scrollback_len(),
        scroll_offset: 0,
        requested_height: 6,
    };
    assert_eq!(
        super::terminal_tab_at_column(&terminal, &config, 80, 2),
        Some(7)
    );
    assert_eq!(
        super::terminal_tab_at_column(&terminal, &config, 80, 13),
        Some(8)
    );
    let mut ui = ui_with_editor_layout(&state);
    ui.explorer = Some(super::ExplorerUiState {
        title: "Explorer".into(),
        rows: Vec::new(),
        scroll: 0,
        loading: false,
        total: 0,
        preview_lines: Vec::new(),
    });
    ui.terminal_panel = Some(terminal.clone());
    let geometry = super::terminal_panel_geometry(&config, 20, 6).expect("terminal geometry");
    assert_eq!(geometry.top, 14);
    assert_eq!(geometry.body_height, 4);
    let panel_width = super::explorer_panel_width(&config, 80).expect("explorer width");
    let editor_width = 80_u16.saturating_sub(panel_width).saturating_sub(1);
    attach_test_compositor(&state, &mut ui, &config, 80, 20);
    let viewport = super::active_pane_viewport(&state, &ui, &config, editor_width, geometry.top)
        .expect("reduced editor viewport");
    assert!(viewport.height < 14);

    let frame = render(&state, &ui, &config, &loaded.dashboard, 80, 20);
    assert_eq!(
        frame.grid.get(0, geometry.top).expect("full divider").style,
        crate::ui::Style::TerminalBorder
    );
    assert_eq!(
        frame
            .grid
            .get(79, geometry.top)
            .expect("right divider")
            .style,
        crate::ui::Style::TerminalBorder
    );
    assert_eq!(
        frame
            .grid
            .get(0, geometry.body_top)
            .expect("red terminal cell")
            .style,
        crate::ui::Style::Terminal(crate::ui::PtyStyle {
            foreground: Some(crate::ui::PtyColor::Indexed(1)),
            bold: true,
            ..crate::ui::PtyStyle::default()
        })
    );
    assert_eq!(
        frame
            .grid
            .get(4, geometry.body_top)
            .expect("wide terminal cell")
            .symbol,
        "界"
    );
    assert!(
        frame
            .grid
            .get(5, geometry.body_top)
            .expect("wide continuation")
            .continuation
    );
    assert_ne!(
        frame
            .grid
            .get(panel_width, geometry.body_top)
            .expect("below explorer")
            .style,
        crate::ui::Style::ExplorerBorder
    );
    assert!(frame.cursor.is_some_and(|(x, y, _)| {
        x == u16::try_from(cursor_column).unwrap() && y == geometry.body_top
    }));
    assert!(
        frame
            .grid
            .row_text(geometry.status_y)
            .contains("TERMINAL  7: shell")
    );
    assert!(
        !frame
            .grid
            .row_text(geometry.status_y)
            .contains("unsupported")
    );

    let mut editor_ui = ui;
    editor_ui.terminal_panel.as_mut().expect("terminal").focused = false;
    let editor_frame = render(&state, &editor_ui, &config, &loaded.dashboard, 80, 20);
    assert!(
        editor_frame
            .cursor
            .is_some_and(|(_, y, _)| y < geometry.top)
    );
    let tab_row = editor_frame.grid.row_text(geometry.top);
    assert!(tab_row.contains("7: shell"));
    assert!(tab_row.contains("8: logs [exit 2]"));
}

#[test]
fn key_hints_anchor_clip_and_preserve_protected_rows() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.ui.key_hints.max_rows = 2;
    config.ui.key_hints.max_columns = 2;
    config.ui.key_hints.max_width = 44;
    let mut ui = UiState {
        dashboard: true,
        key_hints: Some(super::KeyHintUiState {
            prefix: "space f".into(),
            rows: (0..8)
                .map(|index| super::KeyHintUiRow {
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
    assert!(!(0..20).any(|row| right.grid.row_text(row).contains("Action 4")));

    config.ui.key_hints.anchor = crate::config::KeyHintAnchor::BottomLeft;
    ui.explorer = Some(super::ExplorerUiState {
        title: "Files".into(),
        rows: Vec::new(),
        scroll: 0,
        loading: false,
        total: 0,
        preview_lines: Vec::new(),
    });
    let left = render(&state, &ui, &config, &loaded.dashboard, 80, 20);
    let panel = super::explorer_panel_width(&config, 80).expect("panel");
    let leftmost_hint = (0..80)
        .find(|x| {
            (0..19).any(|y| {
                left.grid
                    .get(*x, y)
                    .is_some_and(|cell| cell.style == crate::ui::Style::KeyHintBorder)
            })
        })
        .expect("left anchored hint");
    assert_eq!(leftmost_hint, panel + 1);
    assert!(leftmost_hint < right_prefix_x);

    let tiny = render(&state, &ui, &config, &loaded.dashboard, 10, 3);
    assert!(!(0..3).any(|row| tiny.grid.row_text(row).contains("space f")));
}

#[test]
fn dashboard_and_prompt_render_at_multiple_sizes() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    for (width, height) in [(120, 40), (42, 10), (18, 5), (18, 1)] {
        let mut ui = UiState {
            dashboard: true,
            ..UiState::default()
        };
        let loaded = crate::config::load_bundled_config();
        let config = loaded.config;
        let dashboard_config = loaded.dashboard;
        let dashboard = render(&state, &ui, &config, &dashboard_config, width, height);
        assert_eq!(dashboard.grid.width(), width);
        assert!(
            dashboard.grid.row_text(height.saturating_sub(1)).len()
                >= usize::from(width.saturating_sub(2))
                || width < 3
        );
        ui.prompt = Some(PromptState::new(PromptKind::Command));
        let prompt = render(&state, &ui, &config, &dashboard_config, width, height);
        let geometry = prompt_geometry(width, height, &config.ui.command_line, false);
        if height > 2 && width > 2 {
            assert!(prompt.grid.row_text(geometry.top).contains("Command") || width < 20);
        }
    }
}
