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
        .and_then(|picker| {
            crate::ui::geometry::picker_window_geometry(
                picker,
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
    let top = super::workspace_top_rows(state, ui, config);
    let screen_bottom = super::screen_foot_rows(state, ui, config, height);
    ui.window_graph_revision = Some(1);
    ui.compositor_layout = Some(super::CompositorLayoutState {
        source_revision: state.window_revision().expect("test revision"),
        graph_revision: 1,
        projection: snapshot.project_with_tiled_insets(
            bounds,
            config.ui.panes.minimum_width,
            config.ui.panes.minimum_height,
            crate::ui::window::TiledInsets {
                screen_bottom,
                top,
                ..Default::default()
            },
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
            phase: super::TerminalPhase::Running,
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
fn workspace_trust_prompt_is_one_centered_confirmation_panel() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    let loaded = crate::config::load_bundled_config();
    let mut prompt = PromptState::new(PromptKind::WorkspaceTrust);
    prompt.help = Some("Allows project commands.".into());
    let frame = render(
        &state,
        &UiState {
            dashboard: true,
            prompt: Some(prompt),
            ..UiState::default()
        },
        &loaded.config,
        &loaded.dashboard,
        100,
        20,
    );
    let geometry = prompt_geometry(100, 20, &loaded.config.ui.command_line, true);

    assert_eq!(geometry.left, 20);
    assert_eq!(geometry.top, loaded.config.ui.command_line.top_rows);
    assert_eq!(geometry.height, 4);
    assert!(
        frame
            .grid
            .row_text(geometry.top)
            .contains("Do you want to trust this workspace?")
    );
    assert!(
        frame
            .grid
            .row_text(geometry.top + 1)
            .contains("Allows project commands.")
    );
    assert_eq!(
        frame
            .grid
            .get(geometry.left + 2, geometry.top + 1)
            .expect("help text")
            .style,
        crate::ui::Style::Muted
    );
    assert!(frame.grid.row_text(geometry.top + 2).contains("> "));
    assert!(frame.grid.row_text(geometry.top + 3).contains('└'));
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
    state.open_text(crate::editor::text::TextBuffer::from_text("x"), None);
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
                roles: Vec::new(),
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
fn notice_popup_shows_on_the_dashboard_without_a_cursor() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    let ui = UiState {
        documentation_popup: Some(DocumentationPopupState {
            kind: DocumentationPopupKind::Notice,
            title: "Notification".into(),
            provider: "application".into(),
            source_range: None,
            sections: vec![DocumentationSection {
                heading: None,
                markup: DocumentationMarkupKind::PlainText,
                roles: Vec::new(),
                lines: vec!["the whole message".into()],
            }],
            scroll: 0,
        }),
        ..UiState::default()
    };
    let config = crate::config::load_bundled_config().config;
    let dashboard = crate::config::load_bundled_config().dashboard;
    let frame = render(&state, &ui, &config, &dashboard, 80, 24);
    assert!(frame.cursor.is_none());
    let screen = (0..24)
        .map(|y| {
            (0..80)
                .map(|x| frame.grid.get(x, y).map_or("", |cell| cell.symbol.as_str()))
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert!(screen.iter().any(|line| line.contains("Notification")));
    assert!(screen.iter().any(|line| line.contains("the whole message")));
}

#[test]
fn completion_attaches_below_globally_centered_prompt() {
    let state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    let mut prompt = PromptState::new(PromptKind::Command);
    prompt.input = "e".into();
    prompt.cursor = 1;
    prompt.completions = vec![
        CompletionItem {
            color: None,
            label: "e".into(),
            detail: "Open file".into(),
        },
        CompletionItem {
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
            color: None,
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
            matches: 0,
            query: String::new(),
            searching: false,
            preview_lines: Vec::new(),
        }),
        explorer_focused: true,
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
            matches: 0,
            query: String::new(),
            searching: false,
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
            staged: false,
        },
        super::UiGitSign {
            buffer,
            buffer_revision: revision,
            root_revision: 1,
            status_revision: 1,
            repository_id: crate::workspace::git::GitRepositoryId(1),
            line: 1,
            kind: super::UiGitSignKind::Changed,
            staged: false,
        },
        super::UiGitSign {
            buffer,
            buffer_revision: revision,
            root_revision: 1,
            status_revision: 1,
            repository_id: crate::workspace::git::GitRepositoryId(1),
            line: 2,
            kind: super::UiGitSignKind::Deleted,
            staged: false,
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
fn a_long_notification_wraps_so_its_end_stays_readable() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let _ = state.open_text(crate::editor::text::TextBuffer::from_text(""), None);
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    ui.notification = Some(format!("{} says why at the very end", "failed ".repeat(20)));
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 20);
    let text: Vec<String> = (0..20).map(|row| frame.grid.row_text(row)).collect();
    assert!(text.iter().any(|row| row.contains("very end")), "{text:#?}");
}

#[test]
fn a_long_path_in_a_notification_breaks_so_its_name_stays_readable() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let _ = state.open_text(crate::editor::text::TextBuffer::from_text(""), None);
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    ui.notification = Some(format!("cannot save /{}/name.txt", "long-directory/".repeat(8)));
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 20);
    let text: Vec<String> = (0..20).map(|row| frame.grid.row_text(row)).collect();
    assert!(text.iter().any(|row| row.contains("name.txt")), "{text:#?}");
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
fn theme_geometry_is_compact_and_bounded_without_resizing_other_pickers() {
    use crate::ui::{PickerRow, PickerSurfaceKind, PickerUiState, window::WindowRect};
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
            if let Some(rect) =
                crate::ui::geometry::picker_window_geometry(&picker, &config, bounds)
            {
                assert!(rect.width <= 64 && rect.height <= 12);
                assert!(rect.x >= bounds.x && rect.y >= bounds.y);
                assert!(rect.x + rect.width <= bounds.x + width);
                assert!(rect.y + rect.height <= bounds.y + height);
                let content =
                    crate::ui::geometry::picker_content_geometry(&picker, &config, rect).unwrap();
                assert_eq!(content.outer, rect);
                assert_eq!(content.list.height, rect.height - 4);
            } else {
                assert!(width < 4 || height < 6);
            }
            picker.surface = PickerSurfaceKind::Generic;
            assert_eq!(
                crate::ui::geometry::picker_window_geometry(&picker, &config, bounds),
                super::picker_geometry(&config, bounds)
            );
        }
    }
}

#[test]
fn picker_sources_share_a_right_column_and_do_not_overwrite_labels_or_borders() {
    use crate::ui::{
        CellGrid, Frame, PickerRow, PickerSurfaceKind, PickerUiState, Style, window::WindowRect,
    };
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
                loading: false,
                preview: None,
            };
            let mut frame = Frame {
                grid: CellGrid::new(72, 12),
                cursor: None,
            };
            super::surfaces::render_picker(&mut frame, &picker, &config, rect, None);
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
        picker: Some(super::PickerUiState {
            preview_side_scroll: (0, 0),
            footer: None,
            input_hint: "Insert · escape normal".into(),
            surface: super::PickerSurfaceKind::Generic,
            title: "Commands".into(),
            query: "health".into(),
            rows: vec![super::PickerRow::text("health", "health.open", "Show health", "core")],
            selected: Some(0),
            scroll: 0,
            loading: false,
            preview: Some(super::PickerPreview::text("health.rs", super::PickerTextPreview { lines: vec!["fn health() {}".to_string()], first_line: 0, highlight_line: Some(0), language: None })),
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
fn picker_stacks_preview_when_two_readable_columns_do_not_fit() {
    let state = EditorState::new(PathBuf::from("/tmp"));
    let long = "x".repeat(80);
    let ui = UiState {
        dashboard: true,
        picker: Some(super::PickerUiState {
            preview_side_scroll: (0, 0),
            footer: None,
            input_hint: "Insert · escape normal".into(),
            surface: super::PickerSurfaceKind::Generic,
            title: "Grep".into(),
            query: "needle".into(),
            rows: vec![super::PickerRow::text("match", format!("src/long.rs:309:5 {long}"), long.clone(), String::new())],
            selected: Some(0),
            scroll: 0,
            loading: false,
            preview: Some(super::PickerPreview::text("src/long.rs", super::PickerTextPreview { lines: (300..320)
                    .map(|line| format!("preview line {line} {long}"))
                    .collect(), first_line: 300, highlight_line: Some(308), language: None })),
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
        super::active_pane_viewport(&state, &ui, &loaded.config, 79, 23).expect("focused viewport");
    assert_eq!(Some(viewport.view), state.active_view);
    assert!(viewport.x > 0 && viewport.y > 0);
    assert!(viewport.width < 79 && viewport.height < 23);
    assert!(viewport.y.saturating_add(viewport.height) <= 23);
}

#[test]
fn completion_is_clipped_to_the_focused_split() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state.open_text(
        crate::editor::text::TextBuffer::from_text("left and right"),
        Some(PathBuf::from("/workspace/main.rs")),
    );
    state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .expect("split");
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    ui.language_completion = Some(super::LanguageCompletionState {
        items: vec![CompletionItem {
            color: None,
            label: "selected-completion".into(),
            detail: "Function · fake".into(),
        }],
        selected: 0,
        scroll: 0,
    });
    attach_test_compositor(&state, &mut ui, &loaded.config, 80, 20);
    let pane =
        super::active_pane_viewport(&state, &ui, &loaded.config, 80, 19).expect("active pane");
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 20);
    for row in 0..20 {
        for column in 0..80 {
            if frame
                .grid
                .get(column, row)
                .is_some_and(|cell| cell.style == crate::ui::Style::Selection)
            {
                assert!(column >= pane.x && column < pane.x.saturating_add(pane.width));
                assert!(row >= pane.y && row < pane.y.saturating_add(pane.height));
            }
        }
    }
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
            matches: 2,
            query: String::new(),
            searching: false,
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
        frame.grid.get(panel, 1).expect("dock separator").symbol,
        "⎸"
    );
    assert_eq!(
        frame
            .grid
            .get(panel, 2)
            .expect("preserved separator")
            .symbol,
        "⎸"
    );
    assert_eq!(
        frame
            .grid
            .get(panel + 1, 2)
            .expect("editor-side cell")
            .style,
        crate::ui::Style::Muted
    );
    assert!(frame.cursor.is_some_and(|(x, y, _)| x == 0 && y == 4));
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
            source: "rustc".into(),
            code: Some("E0001".into()),
            message: "problem".into(),
        }],
        git_branch: Some("lean".into()),
        running_tasks: 2,
        language_completion: Some(super::LanguageCompletionState {
            items: vec![CompletionItem {
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
    let view = state.active_view.expect("active view");
    let gutter = super::editor_state_gutter_width(
        &state,
        &state.buffers[state.views[view].buffer].text,
        &loaded.config.editor,
        100,
    );
    assert_eq!(
        frame.grid.get(gutter, 0).expect("diagnostic text").style,
        crate::ui::Style::Error
    );
    assert!(frame.grid.row_text(0).contains("●● problem [E0001]"));
    assert!((0..10).any(|row| frame.grid.row_text(row).contains("better")));
    let status = frame.grid.row_text(9);
    assert!(status.contains("\u{f057} 1") && status.contains("lean") && status.contains("Tasks 2"));
}

#[test]
fn status_line_names_the_symbols_around_the_cursor() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let (buffer, _) = state.open_text(
        crate::editor::text::TextBuffer::from_text("one\ntwo\nthree\n"),
        Some(PathBuf::from("/tmp/main.rs")),
    );
    let symbol = |name: &str, start, end| super::UiBreadcrumb {
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
    assert!(!status.contains("sibling"), "same line, but the cursor is not in it: {status}");
    assert!(!status.contains("elsewhere"), "{status}");
}

#[test]
fn wrapped_rows_end_at_words_and_break_only_long_ones() {
    let rows = super::debug::wrap_rows(&["one two three".into(), "abcdefghijkl".into()], 8);
    assert_eq!(rows, ["one two", "  three", "abcdefgh", "  ijkl"]);
}

#[test]
fn a_line_with_a_diagnostic_is_marked_in_the_gutter_with_its_worst_severity() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let path = PathBuf::from("/tmp/main.rs");
    state.open_text(
        crate::editor::text::TextBuffer::from_text("bad\nfine\nodd\n"),
        Some(path.clone()),
    );
    let diagnostic = |severity, start, end| super::UiDiagnostic {
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
    use crate::workspace::DiagnosticSeverity::{Error, Warning};
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
    assert_eq!(mark(row("bad")), Some(super::Style::Error), "the worst severity on the line");
    assert_eq!(mark(row("fine")), None);
    assert_eq!(mark(row("odd")), Some(super::Style::Warning));
}

#[test]
fn focused_diagnostic_expands_all_matching_messages_as_virtual_lines() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let path = PathBuf::from("/tmp/main.rs");
    state.open_text(
        crate::editor::text::TextBuffer::from_text("bad\nnext\n"),
        Some(path.clone()),
    );
    let range = crate::editor::text::ByteRange::new(
        crate::editor::text::ByteOffset(0),
        crate::editor::text::ByteOffset(3),
    )
    .expect("range");
    let diagnostic = |message: &str, code: &str| super::UiDiagnostic {
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
        expanded_diagnostic: Some(super::ExpandedDiagnosticState {
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
                phase: super::TerminalPhase::Running,
                id: 7,
                profile: "shell".into(),
                running: true,
                exit_code: None,
            },
            super::TerminalTabState {
                phase: super::TerminalPhase::Exited,
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
        matches: 0,
        query: String::new(),
        searching: false,
        preview_lines: Vec::new(),
    });
    ui.terminal_panel = Some(terminal.clone());
    // The status line keeps the screen's last row, so the panel ends one row above it.
    let geometry = super::terminal_panel_geometry(&config, 19, 6).expect("terminal geometry");
    assert_eq!(geometry.top, 13);
    assert_eq!(geometry.body_height, 5);
    let panel_width = super::explorer_panel_width(&config, 80).expect("explorer width");
    let editor_width = 80_u16.saturating_sub(panel_width).saturating_sub(1);
    attach_test_compositor(&state, &mut ui, &config, 80, 20);
    let viewport = super::active_pane_viewport(&state, &ui, &config, editor_width, geometry.top)
        .expect("reduced editor viewport");
    assert!(viewport.height <= 13);

    let frame = render(&state, &ui, &config, &loaded.dashboard, 80, 20);
    assert!(frame.grid.row_text(19).contains("NORMAL"));
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
    let tab_row = frame.grid.row_text(geometry.top);
    assert!(tab_row.contains("shell"));
    assert!(!tab_row.contains("TERMINAL"));
    assert!(!frame.grid.row_text(19).contains("TERMINAL"));

    let mut editor_ui = ui;
    editor_ui.terminal_panel.as_mut().expect("terminal").focused = false;
    let editor_frame = render(&state, &editor_ui, &config, &loaded.dashboard, 80, 20);
    assert!(
        editor_frame
            .cursor
            .is_some_and(|(_, y, _)| y < geometry.top)
    );
    let tab_row = editor_frame.grid.row_text(geometry.top);
    assert!(tab_row.contains("shell"));
    assert!(tab_row.contains("logs [exit 2]"));
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
        matches: 0,
        query: String::new(),
        searching: false,
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

#[test]
fn the_operation_bar_renders_the_phase_at_width_and_never_loses_cancellation() {
    use crate::ui::OperationBarState;
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    // The layout needs an open buffer, which is also the ordinary state a paused operation is
    // seen in.
    state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(PathBuf::from("/workspace/main.rs")),
    );
    let loaded = crate::config::load_bundled_config();
    let config = loaded.config.clone();
    let dashboard = loaded.dashboard.clone();

    let bar = OperationBarState {
        operation: "merge".into(),
        phase: "merge: 1 conflicted path(s)".into(),
        blockers: vec!["shared.txt".into()],
        actions: vec!["resolve".into(), "abort".into()],
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
        wide.iter().any(|row| row.contains("merge") && row.contains("conflicted")),
        "the bar names the operation and its phase: {wide:?}"
    );
    assert!(
        wide.iter().any(|row| row.contains("resolve") && row.contains("abort")),
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
        narrow.iter().any(|row| row.contains("cancel")),
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
        !none.iter().any(|row| row.contains("resolve") || row.contains("cancel")),
        "with no operation paused there is no bar: {none:?}"
    );
}

#[test]
fn a_conflict_free_pause_still_shows_the_bar() {
    use crate::ui::OperationBarState;
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(PathBuf::from("/workspace/main.rs")),
    );
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
    state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(PathBuf::from("/workspace/main.rs")),
    );
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
    });
    let frame = render(&state, &quiet, &config, &dashboard, 80, 12);

    // NO FOCUS STEALING: the bar does not move the focus.
    assert_eq!(
        frame.cursor, quiet_cursor,
        "the bar leaves the focus exactly where it was: {:?} vs {:?}",
        frame.cursor, quiet_cursor
    );
    // NO AUTOMATIC EXPLANATORY CONTENT: rendering opened no prompt, popup or picker of its own.
    assert!(
        quiet.prompt.is_none(),
        "rendering a paused operation opens no prompt"
    );
    assert!(
        quiet.documentation_popup.is_none(),
        "and no documentation popup"
    );
    assert!(quiet.picker.is_none(), "and no picker");

    // The bar's own text carries NOTHING beyond the operation, phase, blockers and actions: the
    // row is asserted exactly rather than by substring.
    let bar_row = (0..12)
        .map(|y| frame.grid.row_text(y))
        .find(|row| row.contains("rebase"))
        .expect("the bar row is drawn");
    assert_eq!(
        bar_row.trim_end(),
        "rebase · rebase: no textual conflicts remain · continue/abort",
        "the bar explains nothing unprompted: {bar_row:?}"
    );
}

#[test]
fn a_debug_view_survives_a_narrow_width_in_the_shared_picker() {
    use crate::ui::PickerUiState;
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state.open_text(
        crate::editor::text::TextBuffer::from_text("fn main() {}"),
        Some(PathBuf::from("/workspace/main.rs")),
    );
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
        rows: vec![crate::ui::render::PickerRow::text("variables:0", "count = 3", "i32", "locals")],
        selected: Some(0),
        scroll: 0,
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

#[test]
fn the_debug_arrangement_is_coordinated_and_the_source_is_never_squeezed_out() {
    // W-25: "sessions, threads, stack and breakpoints on one side, the dominant source in the
    // centre, lazily loaded variables and watches on the other, and an interactive console bound
    // to the selected frame. Narrow terminals replace panes with selectable inspectors rather
    // than compressing the source below a readable minimum."
    use super::debug::{
        debug_layout, render_debug_panes, DebugLayoutMode, DebugPane, DebugPanesUiState,
        DEBUG_SOURCE_MIN_WIDTH,
    };
    let panes = DebugPanesUiState {
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

    // WIDE: inspectors on BOTH sides, console BELOW, and the source dominant in the centre.
    let layout = debug_layout(120, 40).expect("a wide terminal has room for the arrangement");
    assert_eq!(layout.mode, DebugLayoutMode::Wide);
    let left = layout.left.expect("sessions/threads/stack/breakpoints on one side");
    let right = layout.right.expect("variables/watches on the other");
    let console = layout.console.expect("the console below");
    assert_eq!(left.x, 0, "the left inspector starts at the left edge");
    assert_eq!(right.x, layout.source.x + layout.source.width, "the right inspector follows the source");
    assert_eq!(console.y, layout.source.y + layout.source.height, "the console is BELOW the source");
    assert_eq!((console.x, console.width), (0, 120), "the console spans the terminal");
    assert!(
        layout.source.width > left.width && layout.source.width > right.width,
        "the source is the DOMINANT region: {} vs {} and {}",
        layout.source.width,
        left.width,
        right.width
    );
    assert!(
        layout.source.width >= DEBUG_SOURCE_MIN_WIDTH,
        "the source keeps the readable minimum"
    );

    // And the panels are DRAWN in the regions the arrangement reserved, not over the source.
    let mut frame = super::Frame {
        grid: super::CellGrid::new(120, 40),
        cursor: None,
    };
    render_debug_panes(&mut frame, layout, &panes);
    assert!(frame.grid.row_text(0).contains("Debug"), "the left inspector is titled");
    assert!(frame.grid.row_text(0).contains("Watches"), "the right inspector is titled");
    assert!(
        frame.grid.row_text(console.y).contains("Console"),
        "the console is titled below the source"
    );
    let source_row = frame.grid.row_text(0);
    let source_cell = source_row
        .chars()
        .nth(layout.source.x as usize)
        .expect("the source region exists");
    assert_eq!(
        source_cell, ' ',
        "the source region is empty of panel content: the panels cannot cover it"
    );

    // NARROW: the side panes are REPLACED, the source keeps its width, one inspector at a time.
    let narrow = debug_layout(60, 24).expect("a narrow terminal still has a layout");
    assert_eq!(narrow.mode, DebugLayoutMode::Narrow);
    assert!(
        narrow.left.is_none() && narrow.right.is_none(),
        "panes are replaced rather than compressed in"
    );
    assert_eq!(narrow.source.width, 60, "the source keeps its FULL width");
    assert!(narrow.source.width >= DEBUG_SOURCE_MIN_WIDTH);
    let strip = narrow.console.expect("one selectable inspector remains");
    let mut frame = super::Frame {
        grid: super::CellGrid::new(60, 24),
        cursor: None,
    };
    render_debug_panes(&mut frame, narrow, &panes);
    // The selector may WRAP, so every row of the strip counts as the selector.
    let selector: String = (strip.y..strip.y + strip.height)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join(" ");
    for inspector in DebugPane::ALL {
        assert!(
            selector.contains(inspector.label()),
            "narrow terminals make inspectors SELECTABLE: {} is missing from {selector:?}",
            inspector.label()
        );
    }
    assert!(
        selector.contains("[Watches]"),
        "the selected inspector is marked: {selector:?}"
    );
    assert!(
        !frame.grid.row_text(0).contains("Debug"),
        "no side pane is drawn in a narrow terminal"
    );
}

#[test]
fn the_selected_inspector_decides_the_narrow_strip_content_not_only_its_marker() {
    // "Selectable inspectors" is only real if the CONTENT follows the selection: an assertion that
    // the marker moved would pass even if the strip kept showing the previous inspector.
    use super::debug::{debug_layout, render_debug_panes, DebugPane, DebugPanesUiState};
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
    let mut watches = super::Frame {
        grid: super::CellGrid::new(60, 24),
        cursor: None,
    };
    render_debug_panes(&mut watches, layout, &base);
    let mut breakpoints = super::Frame {
        grid: super::CellGrid::new(60, 24),
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
    let rows_of = |frame: &super::Frame| -> String {
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
    use super::debug::{debug_layout, render_debug_panes, DebugPanesUiState};
    let layout = debug_layout(120, 40).unwrap();
    let inner = layout.left.unwrap().width as usize - 2;
    let panes = DebugPanesUiState {
        selected: super::debug::DebugPane::Stack,
        target: String::new(),
        status: String::new(),
        variables: Vec::new(),
        threads: Vec::new(),
        stack: vec!["f".repeat(inner + 5)],
        breakpoints: vec![format!("main.py:6 if {} == \"Ada\"", "person.name".repeat(2))],
        watches: Vec::new(),
        console: Vec::new(),
    };
    let mut frame = super::Frame {
        grid: super::CellGrid::new(120, 40),
        cursor: None,
    };
    render_debug_panes(&mut frame, layout, &panes);
    let text = (0..40).map(|y| frame.grid.row_text(y)).collect::<Vec<_>>().join("\n");
    assert!(text.contains("\"Ada\""), "{text}");
    assert!(text.contains(&format!("{}…", "f".repeat(inner - 1))), "{text}");
}

#[test]
fn the_debug_console_and_the_terminal_panel_are_planned_together_rather_than_colliding() {
    // W-25 keeps the debug console and the program's terminal output as SEPARATE content; separate
    // content also needs separate ROOM. This renders through the real path with BOTH a debug
    // arrangement and a docked terminal panel, so the assertion can fail when the two docks are
    // planned independently.
    let loaded = crate::config::load_bundled_config();
    let config = loaded.config.clone();
    let dashboard = crate::config::DashboardConfig::default();
    let (width, height) = (120u16, 40u16);
    let mut state = EditorState::new(std::path::PathBuf::from("/tmp/dust-workspace"));
    state.open_text(crate::editor::text::TextBuffer::from_text("fn main() {}\n"), None);
    let mut ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        ..UiState::default()
    };
    ui.debug_panes = Some(super::debug::DebugPanesUiState {
        selected: super::debug::DebugPane::Sessions,
        target: "owned: launched debuggee".into(),
        status: String::new(),
        variables: Vec::new(),
        threads: vec!["main (1)".into()],
        stack: vec!["main:1".into()],
        breakpoints: vec!["src/main.rs:3".into()],
        watches: vec!["count".into()],
        console: (1..=7).map(|n| format!("c{n}")).collect(),
    });
    ui.terminal_panel = Some(super::TerminalPanelState {
        id: 1,
        profile: "side".into(),
        tabs: vec![super::TerminalTabState {
            phase: super::TerminalPhase::Running,
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
        columns: 40,
        rows: 8,
        cells: Vec::new(),
        cells_start: 0,
        cursor: None,
        selection: None,
        scrollback_lines: 0,
        scroll_offset: 0,
        requested_height: 5,
    });

    let frame = render(&state, &ui, &config, &dashboard, width, height);

    // (b) The panel IS present: rendering no panel must not satisfy this test.
    let panel_row = (0..height)
        .find(|y| frame.grid.row_text(*y).contains("side"))
        .expect("the terminal panel is docked and drawn");
    // (a) The debug console is ABOVE it: the two docks do not intersect.
    let console_row = (0..height)
        .find(|y| frame.grid.row_text(*y).contains("Console"))
        .expect("the debug console is drawn");
    assert!(
        console_row < panel_row,
        "the console row {console_row} must be above the terminal panel row {panel_row}, not \
         inside it"
    );
    for y in 0..height {
        let row = frame.grid.row_text(y);
        assert!(
            !(row.contains("Console") && row.contains("side")),
            "row {y} mixes the debug console with the program's terminal output: {row:?}"
        );
    }
    // (c) The co-planning did not squeeze the source: it still keeps the readable minimum.
    let available = super::debug::debug_layout(width, panel_row).expect("the arrangement still fits above the panel");
    assert!(
        available.source.width >= super::debug::DEBUG_SOURCE_MIN_WIDTH,
        "the source keeps its minimum width above the panel: {}",
        available.source.width
    );
    let console = available.console.expect("the arrangement has a console");
    assert!(
        console.y + console.height <= panel_row,
        "the arrangement's console ends at or before the panel's top row"
    );
    // NOTHING THE CONSOLE HOLDS MAY BE HIDDEN BEHIND THE PANEL. The console body is filled to its
    // whole height above the panel, so if the arrangement had been computed against the FULL
    // height these lines would sit under the panel and disappear - which is exactly the defect
    // this test exists to catch.
    for n in 1..=7 {
        let line = format!("c{n}");
        assert!(
            (0..height).any(|y| frame.grid.row_text(y).contains(&line)),
            "console line {line} was not visible: it was covered by the terminal panel"
        );
    }
    assert!(
        frame.grid.row_text(panel_row - 1).contains("c7"),
        "the console's LAST line sits directly above the panel: {:?}",
        frame.grid.row_text(panel_row - 1)
    );
}

/// The column at which `needle` starts on the row where it is drawn, and that row.
fn drawn_at(frame: &super::Frame, needle: &str) -> Option<(u16, u16)> {
    for y in 0..frame.grid.height() {
        let text = frame.grid.row_text(y);
        if let Some(byte) = text.find(needle) {
            let column = u16::try_from(text[..byte].chars().count()).unwrap_or(u16::MAX);
            return Some((column, y));
        }
    }
    None
}

/// U-15: a row carries per-cell text, per-cell style and glyph cells, and the columns line
/// up because each cell declares its own width rather than being concatenated into a string.
///
/// The assertions read the rendered grid: the text at each column position AND the style at
/// each cell, so a rendering that drew the right characters in the wrong styles fails here.
#[test]
fn a_row_with_cells_draws_each_column_at_its_own_width_and_style() {
    use crate::ui::{CellAlign, PickerCell, Style};

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
    assert_eq!(path_x, glyph_x + 1, "the path column starts after the one-wide glyph");
    assert_eq!(state_x, path_x + 16, "the provenance column follows a 16-wide path column");
    assert_eq!(
        count_x + 2,
        state_x + 10 + 6,
        "a right-aligned cell ends at its column edge"
    );

    // Each cell keeps its own style, so a status column and a lane can be coloured apart.
    let style_at = |x: u16, y: u16| frame.grid.get(x, y).expect("a drawn cell").style;
    assert_eq!(style_at(glyph_x, y), Style::Error, "the glyph cell carries its own style");
    assert_eq!(style_at(path_x, y), Style::Default, "the path cell carries its own style");
    assert_eq!(style_at(state_x, y), Style::Muted, "the provenance cell carries its own style");
    assert_eq!(style_at(count_x, y), Style::Warning, "the count cell carries its own style");
}

/// U-15/U-16 (base form): a diff carried as structure renders its provenance, its hunk
/// headers, the exact count of a collapsed run, and each line with both numbers and a
/// classification glyph. Intra-line emphasis, side-by-side mode and syntax highlighting are
/// the diff renderer's own work and are not claimed by this test.
#[test]
fn a_structured_diff_preview_draws_provenance_hunks_and_classified_lines() {
    use crate::ui::{DiffHunk, DiffLine, DiffLineKind, DiffPreview, PickerPreviewBody, Style};

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
                line(DiffLineKind::Context, Some(31), Some(31), "let raw = read(path)?;"),
                line(DiffLineKind::Added, None, Some(33), "ensure!(gen > self.gen);"),
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
            rows: vec![super::PickerRow::text("row", "src/main.rs", "unstaged", "git")],
            selected: None,
            scroll: 0,
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
    let rendered: Vec<String> = (0..frame.grid.height()).map(|y| frame.grid.row_text(y)).collect();
    let has = |needle: &str| {
        rendered.iter().any(|row| row.contains(needle))
    };

    assert!(has("staged = HEAD -> index"), "the provenance names the comparison");
    assert!(has("@@ -31,6 +31,9 @@ impl Status {"), "the hunk header is drawn");
    assert!(has("4 unchanged lines"), "a collapsed run states its exact count");
    // A line the other side does not have still occupies its number column, so the
    // classification glyphs form one column instead of drifting with the text.
    assert!(has("31 31   let raw = read(path)?;"), "a context line shows both numbers");
    assert!(has("  33 + ensure!(gen > self.gen);"), "an added line keeps its old column blank");
    assert!(has("32    - let gen = self.gen;"), "a removed line keeps its new column blank");
    let (context_x, _) = drawn_at(&frame, "let raw = read(path)?;").expect("the context line");
    let (added_x, _) = drawn_at(&frame, "ensure!(gen > self.gen);").expect("the added line");
    let (removed_x, _) = drawn_at(&frame, "let gen = self.gen;").expect("the removed line");
    assert_eq!(context_x, added_x, "the added line's text starts where the context line's does");
    assert_eq!(context_x, removed_x, "the removed line's text starts where the context line's does");

    // Classification is carried by style as well as by the glyph, so the two agree.
    let (added_x, added_y) = drawn_at(&frame, "ensure!(gen > self.gen);")
        .expect("the added line is drawn");
    assert_eq!(frame.grid.get(added_x, added_y).expect("cell").style, Style::StatusAccent);
    let (removed_x, removed_y) = drawn_at(&frame, "let gen = self.gen;")
        .expect("the removed line is drawn");
    assert_eq!(frame.grid.get(removed_x, removed_y).expect("cell").style, Style::Error);
}

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

/// The same picker with the preview's two sides scrolled by their own offsets.
fn diff_picker_scrolled(diff: super::DiffPreview, scroll: (usize, usize)) -> UiState {
    let mut state = diff_picker(diff);
    if let Some(picker) = state.picker.as_mut() {
        picker.preview_side_scroll = scroll;
    }
    state
}

/// A picker whose preview carries a parsed diff.
fn diff_picker(diff: super::DiffPreview) -> UiState {
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
    let rendered: Vec<String> = (0..frame.grid.height()).map(|y| frame.grid.row_text(y)).collect();
    let has = |needle: &str| rendered.iter().any(|row| row.contains(needle));

    assert!(has("unstaged = index -> disk"), "the provenance names the comparison");
    assert!(has("@@ -1,6 +1,6 @@"), "the first hunk header is drawn");
    assert!(has("@@ -32,7 +32,7 @@ line thirty-one"), "the second hunk header is drawn");
    assert!(has("25 unchanged lines"), "the collapsed run states its exact count");
    // Both numbers are drawn in their own columns, and the side a line does not have is
    // left blank so the classification glyphs form one column.
    assert!(has(" 1  1   line one"), "a context line shows both of its numbers");
    assert!(has("   3 + line three changed"), "an added line blanks its old-number column");
    assert!(has(" 3    - line three"), "a removed line blanks its new-number column");
    assert!(has("32 32   line thirty-two"), "the second hunk starts at the line its header names");
    assert!(has("   35 + line thirty-five modified"), "the second hunk is numbered from its own header");
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
    use crate::ui::{parse_unified_patch, Style};

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
        frame.grid.get(unchanged_x, same_y).expect("a drawn cell").style,
        Style::StatusAccent,
        "the unchanged part of the added line keeps the addition style"
    );
}

/// U-16: side by side, the old and new sides occupy their own columns with a divider between.
#[test]
fn side_by_side_puts_the_two_sides_in_their_own_columns() {
    use crate::ui::{DiffLayout, parse_unified_patch, Style};

    let mut diff = parse_unified_patch(RENDER_PATCH, "unstaged = index -> disk");
    diff.layout = DiffLayout::SideBySide;
    let state = EditorState::new(PathBuf::from("/tmp"));
    let ui = diff_picker(diff);
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 120, 30);

    let (removed_x, y) = drawn_at(&frame, "line three").expect("the old side is drawn");
    let (added_x, added_y) = drawn_at(&frame, "line three changed").expect("the new side is drawn");
    assert_eq!(y, added_y, "both sides of one change are drawn on the same row");
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
    use crate::ui::{DiffHunk, DiffLine, DiffLineKind, DiffLayout, DiffPreview};

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
    let rendered: Vec<String> = (0..frame.grid.height()).map(|y| frame.grid.row_text(y)).collect();
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
    use crate::ui::{DiffLayout, parse_unified_patch};

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
    assert!(at_top.contains("old1"), "the old side starts at its first line");
    assert!(at_top.contains("new1"), "the new side starts at its first line");

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
fn indent_guides_inside_a_multiline_string_keep_the_guide_colour() {
    let text = "fn f() {\n    let s = r\"a\n        b\";\n}\n";
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    let (buffer, view) = state.open_text(
        crate::editor::text::TextBuffer::from_text(text),
        Some(PathBuf::from("/workspace/file.rs")),
    );
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.editor.line_numbers = crate::config::LineNumbers::Off;
    config.ui.status_line.enabled = false;
    let start = text.find("r\"").expect("string start");
    let end = text.find("\";").expect("string end") + 1;
    let mut ui = ui_with_editor_layout(&state);
    ui.syntax_spans = vec![super::UiSyntaxSpan {
        buffer,
        revision: state.buffers[buffer].text.revision(),
        range: crate::editor::text::ByteRange::new(
            crate::editor::text::ByteOffset(start),
            crate::editor::text::ByteOffset(end),
        )
        .expect("string range"),
        role: crate::language::syntax::SyntaxRole::String,
    }];
    let frame = super::render_editor_view(&state, view, &ui, &config, 30, 5);
    let guide = frame.grid.get(4, 2).expect("guide inside the string");
    assert_ne!(guide.symbol, " ");
    assert!(matches!(guide.style, crate::ui::Style::Border | crate::ui::Style::Muted), "{:?}", guide.style);
}

#[test]
fn a_long_path_in_a_picker_row_keeps_its_end() {
    let ambiguous = crate::config::load_bundled_config().config.editor.ambiguous_width;
    let path = "/tmp/a/very/long/prefix/that/repeats/on/every/row/src/main.py:12";
    let (label, detail) =
        super::surfaces::fitted_picker_row(path, "print(name)", 40, ambiguous);
    assert!(label.starts_with('…') && label.ends_with("main.py:12"), "{label}");
    assert_eq!(detail, "print(name)");
    let (label, detail) =
        super::surfaces::fitted_picker_row("main", "/tmp/a/very/long/prefix/main.go:7:2", 24, ambiguous);
    assert_eq!(label, "main");
    assert!(detail.starts_with('…') && detail.ends_with("main.go:7:2"), "{detail}");
    let (label, _) = super::surfaces::fitted_picker_row("Git: stage/unstage all", "", 12, ambiguous);
    assert_eq!(label, "Git: stage/unstage all", "ordinary text is not a path");
}

#[test]
fn key_help_breaks_between_items_and_keeps_its_end() {
    let ambiguous = crate::config::load_bundled_config().config.editor.ambiguous_width;
    let rows = super::surfaces::wrapped_key_help("j/k move · Enter install · r refresh · Esc close", 26, ambiguous);
    assert_eq!(rows, ["j/k move · Enter install", "r refresh · Esc close"]);
}

#[test]
fn a_notification_too_long_to_show_points_at_the_messages_list() {
    let mut state = EditorState::new(PathBuf::from("/tmp"));
    let _ = state.open_text(crate::editor::text::TextBuffer::from_text(""), None);
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    ui.notification = Some("conflict ".repeat(200));
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 20);
    let text: Vec<String> = (0..20).map(|row| frame.grid.row_text(row)).collect();
    assert!(text.iter().any(|row| row.contains("… :messages")), "{text:#?}");
}

#[test]
fn terminal_tabs_are_named_by_profile_and_numbered_when_repeated() {
    let tab = |id, profile: &str| super::TerminalTabState {
        phase: super::TerminalPhase::Running,
        id,
        profile: profile.into(),
        running: true,
        exit_code: None,
    };
    let tabs = vec![tab(3, "shell"), tab(4, "lazygit"), tab(9, "shell")];
    let names = tabs
        .iter()
        .map(|current| super::terminal::terminal_tab_label(&tabs, current))
        .collect::<Vec<_>>();
    assert_eq!(names, ["shell", "lazygit", "shell 2"]);
}

#[test]
fn the_status_line_puts_the_server_and_root_on_the_right() {
    let mut state = EditorState::new(PathBuf::from("/tmp/some-project"));
    let (_, view) = state.open_text(crate::editor::text::TextBuffer::from_text("x"), None);
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    ui.lsp_server = Some("rust-analyzer".into());
    let wide = super::render_editor_view(&state, view, &ui, &loaded.config, 80, 5);
    let status = wide.grid.row_text(4);
    assert!(status.contains("rust-analyzer  some-project"), "{status:?}");
    assert!(status.trim_end().ends_with("Top \u{e0b2} 1:1"), "{status:?}");
    assert_eq!(wide.grid.get(1, 4).expect("mode").style, crate::ui::Style::StatusAccent);

    // The server goes first when the room runs out, then the progress; the root stays.
    let narrow = super::render_editor_view(&state, view, &ui, &loaded.config, 42, 5);
    let status = narrow.grid.row_text(4);
    assert!(status.trim_end().ends_with("some-project  \u{e0b2} 1:1"), "{status:?}");
    assert!(!status.contains("rust-analyzer") && !status.contains("Top"), "{status:?}");

    // Each mode shows in its own colour.
    state.views[view].mode = crate::editor::EditorMode::Insert;
    let insert = super::render_editor_view(&state, view, &ui, &loaded.config, 80, 5);
    assert_eq!(insert.grid.get(1, 4).expect("mode").style, crate::ui::Style::StatusInsert);
}

#[test]
fn the_status_line_names_a_worktree_with_its_repository() {
    let mut state = EditorState::new(PathBuf::from("/tmp/repo/.worktree/feature"));
    state.workspace_repository = Some("repo".into());
    let (_, view) = state.open_text(crate::editor::text::TextBuffer::from_text("x"), None);
    let loaded = crate::config::load_bundled_config();
    let ui = ui_with_editor_layout(&state);
    let frame = super::render_editor_view(&state, view, &ui, &loaded.config, 80, 5);
    assert!(frame.grid.row_text(4).contains("repo › feature"));
}

#[test]
fn the_status_line_keeps_fields_from_running_into_the_position() {
    let mut state = EditorState::new(PathBuf::from("/tmp/a-rather-long-project-name"));
    let (_, view) = state.open_text(crate::editor::text::TextBuffer::from_text("x"), None);
    let loaded = crate::config::load_bundled_config();
    let ui = ui_with_editor_layout(&state);
    let frame = super::render_editor_view(&state, view, &ui, &loaded.config, 22, 5);
    let status = frame.grid.row_text(4);
    assert!(status.trim_end().ends_with(" 1:1"), "{status:?}");
}
