//! Terminal tests for editor surfaces and frame composition.

use crate::editor::EditorState;
use crate::ui::render::UiState;
use std::path::PathBuf;

use crate::ui::render::composition_tests::attach_test_compositor;
use crate::ui::render::composition_tests::editor_layout;
use crate::ui::render::composition_tests::render;
use crate::ui::render::composition_tests::ui_with_editor_layout;

#[test]
fn terminal_is_a_full_width_bottom_dock_with_direct_cells_and_focus_cursor() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("editor content\nsecond line"),
            Some(PathBuf::from("/workspace/main.rs")),
        )
        .unwrap();
    state
        .split_active(crate::windows::TileAxis::Horizontal, 400)
        .expect("nested editor split");
    let loaded = crate::config::load_bundled_config();
    let mut config = loaded.config;
    config.ui.terminal_panel.height = 6;
    let mut screen = crate::ui::PtyScreen::new(80, 4, 100);
    screen.feed_bytes("\u{1b}[31;1mRED\u{1b}[0m 界".as_bytes());
    let (cursor_column, cursor_row) = screen.cursor();
    let terminal = crate::ui::render::TerminalPanelState {
        id: 7,
        profile: "shell".into(),
        tabs: vec![
            crate::ui::render::TerminalTabState {
                phase: crate::ui::render::TerminalPhase::Running,
                id: 7,
                profile: "shell".into(),
                running: true,
                exit_code: None,
            },
            crate::ui::render::TerminalTabState {
                phase: crate::ui::render::TerminalPhase::Exited,
                id: 8,
                profile: "logs".into(),
                running: false,
                exit_code: Some(2),
            },
        ],
        mode: crate::ui::render::TerminalPanelMode::Input,
        running: true,
        exit_code: None,
        truncated: false,
        focused: true,
        input_hint: None,
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
    let tab_geometry = crate::ui::render::terminal_panel_geometry(19, 6).unwrap();
    let tab_geometry = crate::ui::render::TerminalPanelGeometry {
        width: 80,
        ..tab_geometry
    };
    assert_eq!(
        crate::ui::render::terminal_tab_at_column(&terminal, &config, tab_geometry, 2),
        Some(7)
    );
    assert_eq!(
        crate::ui::render::terminal_tab_at_column(&terminal, &config, tab_geometry, 13),
        Some(8)
    );
    let mut ui = ui_with_editor_layout(&state);
    ui.explorer = Some(crate::ui::render::ExplorerUiState {
        title: "Explorer".into(),
        rows: Vec::new(),
        scroll: 0,
        loading: false,
        total: 0,
        matches: 0,
        query: String::new(),
        searching: false,
    });
    ui.terminal_panel = Some(terminal.clone());
    // The status line keeps the screen's last row, so the panel ends one row above it.
    let geometry = crate::ui::render::terminal_panel_geometry(19, 6).expect("terminal geometry");
    assert_eq!(geometry.top, 13);
    assert_eq!(geometry.body_height, 5);
    let panel_width = crate::ui::render::explorer_panel_width(&config, 80).expect("explorer width");
    let editor_width = 80_u16.saturating_sub(panel_width).saturating_sub(1);
    attach_test_compositor(&state, &mut ui, &config, 80, 20);
    let viewport =
        crate::ui::render::active_pane_viewport(&state, &ui, &config, editor_width, geometry.top)
            .expect("reduced editor viewport");
    assert!(viewport.height <= 13);

    let frame = render(&state, &ui, &config, &loaded.dashboard, 80, 20);
    // Typing in the terminal is its own mode, not the editor's Normal mode.
    assert!(frame.grid.row_text(19).contains("TERMINAL"));
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

    ui.terminal_panel.as_mut().unwrap().input_hint = Some("Esc Esc → Normal mode".into());
    let hinted = render(&state, &ui, &config, &loaded.dashboard, 80, 20);
    assert!(
        hinted
            .grid
            .row_text(geometry.top)
            .contains("Esc Esc → Normal mode")
    );
    let mut editor_ui = ui;
    editor_ui.terminal_panel.as_mut().expect("terminal").focused = false;
    let editor_frame = render(&state, &editor_ui, &config, &loaded.dashboard, 80, 20);
    assert!(editor_frame.grid.row_text(19).contains("NORMAL"));
    assert!(
        editor_frame
            .cursor
            .is_some_and(|(_, y, _)| y < geometry.top)
    );
    let tab_row = editor_frame.grid.row_text(geometry.top);
    assert!(tab_row.contains("shell"));
    assert!(tab_row.contains("logs [exit 2]"));
    assert!(!tab_row.contains("Normal mode"));
}

#[test]
fn terminal_tabs_keep_the_active_title_visible_and_hit_targets_match_borders() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(crate::editor::text::TextBuffer::from_text("editor"), None)
        .unwrap();
    let mut screen = crate::ui::PtyScreen::new(40, 2, 10);
    screen.feed_bytes("·x界y".as_bytes());
    let mut terminal = crate::ui::render::TerminalPanelState {
        id: 1,
        profile: "shell".into(),
        tabs: (1..=6)
            .map(|id| crate::ui::render::TerminalTabState {
                id,
                profile: "shell".into(),
                phase: crate::ui::render::TerminalPhase::Running,
                running: true,
                exit_code: None,
            })
            .collect(),
        mode: crate::ui::render::TerminalPanelMode::Input,
        running: true,
        exit_code: None,
        truncated: false,
        focused: true,
        input_hint: None,
        columns: 40,
        rows: 2,
        cells: screen.history_window(0, 2),
        cells_start: 0,
        cursor: None,
        selection: None,
        scrollback_lines: 0,
        scroll_offset: 0,
        requested_height: 3,
    };
    let mut config = crate::config::load_bundled_config().config;
    for policy in [
        crate::config::AmbiguousWidth::Narrow,
        crate::config::AmbiguousWidth::Wide,
    ] {
        config.editor.ambiguous_width = policy;
        for layer in [
            crate::windows::CompositorLayer::Docked,
            crate::windows::CompositorLayer::Floating,
        ] {
            let geometry = crate::ui::render::terminal_window_geometry(
                crate::windows::ProjectedCompositorWindow {
                    window: crate::windows::WindowId(1),
                    rect: crate::windows::WindowRect {
                        x: 10,
                        y: 1,
                        width: 40,
                        height: 3,
                    },
                    layer,
                    z_index: 0,
                    focused: true,
                    focusable: true,
                },
            )
            .unwrap();
            for active in [1, 6, 3] {
                terminal.id = active;
                let frame = crate::ui::render::terminal::dock_terminal_panel(
                    crate::ui::grid::Frame {
                        grid: crate::ui::CellGrid::new(80, 8),
                        cursor: None,
                    },
                    &terminal,
                    geometry,
                    &config,
                    80,
                    8,
                );
                let title = crate::ui::render::terminal::terminal_tab_label(
                    &terminal.tabs,
                    &terminal.tabs[(active - 1) as usize],
                );
                assert!(frame.grid.row_text(geometry.top).contains(&title));
                let mut title_cells = 0;
                for x in geometry.x..geometry.x + geometry.width {
                    let cell = frame.grid.get(x, geometry.top).unwrap();
                    if cell.style == crate::ui::Style::TerminalTitle {
                        title_cells += 1;
                        assert_eq!(
                            crate::ui::render::terminal_tab_at_column(
                                &terminal, &config, geometry, x
                            ),
                            Some(active)
                        );
                    }
                }
                assert_eq!(title_cells, title.len());
                assert_eq!(
                    crate::ui::render::terminal_tab_at_column(
                        &terminal, &config, geometry, geometry.x
                    ),
                    None
                );
                for (offset, symbol, continuation) in [
                    (0, "·", false),
                    (1, "x", false),
                    (2, "界", false),
                    (3, " ", true),
                    (4, "y", false),
                ] {
                    let cell = frame
                        .grid
                        .get(geometry.x + offset, geometry.body_top)
                        .unwrap();
                    assert_eq!(cell.symbol, symbol);
                    assert_eq!(cell.continuation, continuation);
                }
            }
        }
    }
    config.editor.ambiguous_width = crate::config::AmbiguousWidth::Narrow;
    terminal.tabs.truncate(2);
    terminal.tabs[0].profile = format!("/{}first-shell", "long-directory/".repeat(20));
    terminal.tabs[1].profile = format!("/{}second-shell", "long-directory/".repeat(20));
    terminal.tabs[1].phase = crate::ui::TerminalPhase::Exited;
    terminal.tabs[1].exit_code = Some(7);
    terminal.id = terminal.tabs[1].id;
    terminal.mode = crate::ui::TerminalPanelMode::Normal;
    for width in [80, 100, 160] {
        let geometry = crate::ui::TerminalPanelGeometry {
            x: 0,
            width,
            left_border: false,
            top: 1,
            height: 3,
            body_top: 2,
            body_height: 2,
        };
        let frame = crate::ui::render::terminal::dock_terminal_panel(
            crate::ui::grid::Frame {
                grid: crate::ui::CellGrid::new(width, 8),
                cursor: None,
            },
            &terminal,
            geometry,
            &config,
            width,
            8,
        );
        let row = frame.grid.row_text(1);
        for expected in ["first-shell", "second-shell", "[exit 7]", "NORMAL"] {
            assert!(row.contains(expected), "{width}: {row}");
        }
        let second = row.find("second-shell").unwrap();
        // The title is ASCII, except for the one-cell shortening marks.
        let column = row[..second].chars().count() as u16;
        assert_eq!(
            crate::ui::terminal_tab_at_column(&terminal, &config, geometry, column),
            Some(terminal.id)
        );
    }
}

#[test]
fn the_debug_console_and_the_terminal_panel_are_planned_together_rather_than_colliding() {
    let loaded = crate::config::load_bundled_config();
    let config = loaded.config.clone();
    let dashboard = crate::config::DashboardConfig::default();
    let (width, height) = (120u16, 40u16);
    let mut state = EditorState::new(std::path::PathBuf::from("/tmp/dust-workspace"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("fn main() {}\n"),
            None,
        )
        .unwrap();
    let mut ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        ..UiState::default()
    };
    ui.debug_panes = Some(crate::ui::render::debug::DebugPanesUiState {
        selected: crate::ui::render::debug::DebugPane::Sessions,
        target: "owned: launched debuggee".into(),
        status: String::new(),
        variables: Vec::new(),
        threads: vec!["main (1)".into()],
        stack: vec!["main:1".into()],
        breakpoints: vec!["src/main.rs:3".into()],
        watches: vec!["count".into()],
        console: (1..=7).map(|n| format!("c{n}")).collect(),
    });
    ui.terminal_panel = Some(crate::ui::render::TerminalPanelState {
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
        input_hint: None,
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

    attach_test_compositor(&state, &mut ui, &config, width, height);
    let frame = crate::ui::render::render(&state, &ui, &config, &dashboard, width, height);
    let region = ui
        .compositor_layout
        .as_ref()
        .unwrap()
        .projection
        .workspace_bounds;
    let layout = crate::ui::render::debug::debug_layout_in(region).expect("debug layout");
    let source = layout.source;
    let pane =
        crate::ui::render::active_editor_pane_viewport(&state, &ui).expect("source viewport");
    assert_eq!(
        (pane.x, pane.y, pane.width, pane.height),
        (source.x, source.y, source.width, source.height)
    );
    let (x, y) = drawn_at(&frame, "fn main() {}").expect("source is visible");
    assert!(x >= source.x && x + 12 <= source.x + source.width);
    assert!(y >= source.y && y < source.y + source.height);
    assert!(frame.cursor.is_some_and(|(x, y, _)| x >= source.x
        && x < source.x + source.width
        && y >= source.y
        && y < source.y + source.height));

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
    let available = super::debug::debug_layout(width, panel_row)
        .expect("the arrangement still fits above the panel");
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

#[test]
fn terminal_tabs_are_named_by_profile_and_numbered_when_repeated() {
    let tab = |id, profile: &str| crate::ui::render::TerminalTabState {
        phase: crate::ui::render::TerminalPhase::Running,
        id,
        profile: profile.into(),
        running: true,
        exit_code: None,
    };
    let tabs = vec![tab(3, "shell"), tab(4, "lazygit"), tab(9, "shell")];
    let names = tabs
        .iter()
        .map(|current| crate::ui::render::terminal::terminal_tab_label(&tabs, current))
        .collect::<Vec<_>>();
    assert_eq!(names, ["shell", "lazygit", "shell 2"]);
}

#[test]
fn terminal_wide_cells_are_clipped_to_the_window_during_resize() {
    let mut screen = crate::ui::PtyScreen::new(60, 2, 10);
    screen.feed_bytes(format!("{}界", " ".repeat(39)).as_bytes());
    let terminal = crate::ui::render::TerminalPanelState {
        id: 1,
        profile: "shell".into(),
        tabs: Vec::new(),
        mode: crate::ui::render::TerminalPanelMode::Normal,
        running: true,
        exit_code: None,
        truncated: false,
        focused: false,
        input_hint: None,
        columns: 60,
        rows: 2,
        cells: vec![screen.history_row(0).unwrap()],
        cells_start: 0,
        cursor: None,
        selection: None,
        scrollback_lines: 0,
        scroll_offset: 0,
        requested_height: 3,
    };
    let mut workspace = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(80, 5),
        cursor: None,
    };
    workspace
        .grid
        .set_cell(50, 1, "z", crate::ui::Style::Default);
    let config = crate::config::load_bundled_config().config;
    let frame = crate::ui::render::terminal::dock_terminal_panel(
        workspace,
        &terminal,
        crate::ui::render::TerminalPanelGeometry {
            x: 10,
            width: 40,
            left_border: false,
            top: 0,
            height: 3,
            body_top: 1,
            body_height: 2,
        },
        &config,
        80,
        5,
    );
    assert_eq!(frame.grid.get(50, 1).unwrap().symbol, "z");
    assert!(!frame.grid.get(50, 1).unwrap().continuation);
}

#[test]
fn side_docked_terminal_keeps_the_debug_inspectors_visible() {
    let mut state = EditorState::new("/tmp".into());
    state
        .open_text(crate::editor::text::TextBuffer::from_text("source\n"), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let mut ui = ui_with_editor_layout(&state);
    ui.debug_panes = Some(crate::ui::render::DebugPanesUiState {
        selected: crate::ui::render::DebugPane::Console,
        target: String::new(),
        status: "Stopped".into(),
        threads: Vec::new(),
        stack: Vec::new(),
        breakpoints: Vec::new(),
        variables: Vec::new(),
        watches: Vec::new(),
        console: vec!["latest output".into()],
    });
    let mut screen = crate::ui::PtyScreen::new(40, 20, 0);
    screen.feed_bytes(b"terminal output");
    ui.terminal_panel = Some(crate::ui::render::TerminalPanelState {
        id: 1,
        profile: "shell".into(),
        tabs: Vec::new(),
        mode: crate::ui::render::TerminalPanelMode::Normal,
        running: true,
        exit_code: None,
        truncated: false,
        focused: false,
        input_hint: None,
        columns: 40,
        rows: 20,
        cells: vec![screen.history_row(0).expect("terminal output row")],
        cells_start: 0,
        cursor: None,
        selection: None,
        scrollback_lines: 0,
        scroll_offset: 0,
        requested_height: 5,
    });
    attach_test_compositor(&state, &mut ui, &loaded.config, 120, 40);
    let compositor = ui.compositor_layout.as_mut().unwrap();
    let terminal = compositor
        .projection
        .windows
        .iter_mut()
        .find(|window| window.window == crate::windows::WindowId(10_002))
        .unwrap();
    terminal.rect = crate::windows::WindowRect {
        x: 80,
        y: 0,
        width: 40,
        height: 39,
    };
    compositor.projection.workspace_bounds = crate::windows::WindowRect {
        x: 0,
        y: 0,
        width: 80,
        height: 39,
    };
    let layout = crate::ui::render::debug::debug_layout_in(compositor.projection.workspace_bounds)
        .expect("side-docked debug layout");
    // Moving the dock also changes the tiled source rectangle and debugger insets.
    let source_window = ui.editor_window_layout.as_ref().unwrap().bindings[0].window;
    compositor
        .projection
        .windows
        .iter_mut()
        .find(|window| window.window == source_window)
        .expect("source window")
        .rect = layout.source;
    let frame = crate::ui::render::render(&state, &ui, &loaded.config, &loaded.dashboard, 120, 40);
    let (x, y) = drawn_at(&frame, "source").expect("source remains visible beside the terminal");
    assert!(x >= layout.source.x && x + 6 <= layout.source.x + layout.source.width);
    assert!(y >= layout.source.y && y < layout.source.y + layout.source.height);
    assert!(frame.cursor.is_some_and(|(x, y, _)| x >= layout.source.x
        && x < layout.source.x + layout.source.width
        && y >= layout.source.y
        && y < layout.source.y + layout.source.height));
    let inspector = layout.console.expect("inspector strip");
    let (x, y) = drawn_at(&frame, "latest output").expect("selected console content");
    assert!(x >= inspector.x && x + 13 <= inspector.x + inspector.width);
    assert!(y >= inspector.y && y < inspector.y + inspector.height);
    let (x, y) = drawn_at(&frame, "terminal output").expect("terminal body is rendered");
    assert!((80..120).contains(&x) && y < 39);
}

use crate::ui::render::composition_tests::drawn_at;
