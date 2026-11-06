use crate::ui::grid::CONTROL_CHARACTER_REPLACEMENT;
use crate::ui::render::overlays::BUFFER_OVERFLOW_RIGHT;
use crate::ui::render::overlays::BUFFER_SEPARATOR;
use std::path::Path;
use std::path::PathBuf;
use crate::editor::EditorState;
use crate::ui::render::CompletionItem;
use crate::ui::render::DocumentationMarkupKind;
use crate::ui::render::DocumentationPopupKind;
use crate::ui::render::DocumentationPopupState;
use crate::ui::render::DocumentationSection;
use crate::ui::render::UiState;

use crate::ui::render::composition_tests::editor_layout;
use crate::ui::render::composition_tests::ui_with_editor_layout;
use crate::ui::render::composition_tests::attach_test_compositor;
use crate::ui::render::composition_tests::render;


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
        .split_active(crate::editor::Axis::Horizontal, 400)
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
    let viewport = crate::ui::render::active_pane_viewport(&state, &ui, &config, editor_width, geometry.top)
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
            let geometry =
                crate::ui::render::terminal_window_geometry(crate::windows::ProjectedCompositorWindow {
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
                })
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
                            crate::ui::render::terminal_tab_at_column(&terminal, &config, geometry, x),
                            Some(active)
                        );
                    }
                }
                assert_eq!(title_cells, title.len());
                assert_eq!(
                    crate::ui::render::terminal_tab_at_column(&terminal, &config, geometry, geometry.x),
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
    let pane = crate::ui::render::active_editor_pane_viewport(&state, &ui).expect("source viewport");
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

/// The column at which `needle` starts on the row where it is drawn, and that row.
fn drawn_at(frame: &super::Frame, needle: &str) -> Option<(u16, u16)> {
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
fn a_long_path_in_a_picker_row_keeps_its_end() {
    let ambiguous = crate::config::load_bundled_config()
        .config
        .editor
        .ambiguous_width;
    let path = "/tmp/a/very/long/prefix/that/repeats/on/every/row/src/main.py:12";
    let (label, detail) = crate::ui::render::picker::fitted_picker_row(path, "print(name)", 40, ambiguous);
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
fn picker_diff_geometry_does_not_count_line_numbers_twice() {
    use crate::ui::PickerPreview;
    use crate::ui::PickerPreviewBody;
    use crate::ui::PickerUiState;
    use crate::windows::WindowRect;
    let config = crate::config::load_bundled_config().config;
    let diff = crate::ui::diff::DiffPreview {
        hunks: vec![crate::ui::diff::DiffHunk {
            header: "@@".into(),
            collapsed_before: 0,
            lines: vec![crate::ui::diff::DiffLine {
                old_number: Some(100),
                new_number: Some(100),
                kind: crate::ui::diff::DiffLineKind::Context,
                text: "x".repeat(30),
                spans: Vec::new(),
            }],
        }],
        provenance: String::new(),
        truncated: false,
        total_lines: 1,
        layout: crate::ui::diff::DiffLayout::Unified,
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
    let geometry = crate::ui::render::picker::picker_content_geometry(&picker, &config, rect).unwrap();
    assert!(geometry.divider.is_some());
    assert!(geometry.stacked_divider.is_none());
    let narrow = WindowRect { width: 74, ..rect };
    let geometry = crate::ui::render::picker::picker_content_geometry(&picker, &config, narrow).unwrap();
    assert!(geometry.divider.is_none());
    assert!(geometry.stacked_divider.is_some());
}

#[test]
fn large_diff_rows_share_number_width() {
    let diff = crate::ui::diff::DiffPreview {
        hunks: vec![crate::ui::diff::DiffHunk {
            header: "@@".into(),
            collapsed_before: 0,
            lines: (1..=50_000)
                .map(|n| crate::ui::diff::DiffLine {
                    old_number: None,
                    new_number: Some(n),
                    kind: crate::ui::diff::DiffLineKind::Added,
                    text: "x".into(),
                    spans: Vec::new(),
                })
                .collect(),
        }],
        provenance: String::new(),
        truncated: false,
        total_lines: 50_000,
        layout: crate::ui::diff::DiffLayout::Unified,
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
        path: path.clone(),
        range: ByteRange::empty(ByteOffset(0)),
        severity: crate::workspace::DiagnosticSeverity::Error,
        source: String::new(),
        code: None,
        message: "bad".into(),
    }];
    ui.expanded_diagnostic = Some(crate::ui::render::ExpandedDiagnosticState {
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

fn tool_menu_for_render() -> crate::ui::render::ToolMenuState {
    crate::ui::render::ToolMenuState {
        category: "lsp".into(),
        selected: 0,
        scroll: 0,
        query: String::new(),
        searching: false,
        runtime_picker: None,
        rows: vec![crate::ui::render::ToolMenuRow {
            id: "rust-analyzer".into(),
            name: "Rust Analyzer".into(),
            kind: "lsp".into(),
            languages: "rust".into(),
            version: String::new(),
            release: String::new(),
            runtime: "PATH".into(),
            lifecycle: "not working".into(),
            session_state: String::new(),
            executable: "/bin/rust-analyzer".into(),
            note: "Version probe failed".into(),
            sessions: Vec::new(),
            lsp: true,
            installed: true,
            can_start: true,
            can_install: false,
            can_uninstall: false,
            update: None,
        }],
    }
}

#[test]
fn tool_menu_title_and_wide_categories_stay_inside_the_modal() {
    let mut config = crate::config::load_bundled_config().config;
    config.editor.ambiguous_width = crate::config::AmbiguousWidth::Wide;
    config
        .ui
        .tool_manager
        .categories
        .insert("all".into(), "语言".into());
    config
        .ui
        .tool_manager
        .categories
        .insert("lsp".into(), "服务".into());
    config
        .ui
        .tool_manager
        .categories
        .insert("dap".into(), "界".repeat(100));
    let mut menu = tool_menu_for_render();
    menu.query = "q".repeat(200);
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new_with_policy(100, 30, "�", "◌", true),
        cursor: None,
    };
    crate::ui::render::tools::render_tool_menu(&mut frame, &menu, &config, 100, 30);
    // The 82-column modal begins at x=9 and ends at x=90.
    assert_eq!(frame.grid.get(90, 2).unwrap().symbol, "+");
    assert_eq!(frame.grid.get(91, 2).unwrap().symbol, " ");
    assert_eq!(frame.grid.get(11, 5).unwrap().symbol, " ");
    assert_eq!(frame.grid.get(12, 5).unwrap().symbol, "语");
    assert_eq!(frame.grid.get(18, 5).unwrap().symbol, " ");
    assert_eq!(frame.grid.get(19, 5).unwrap().symbol, "服");
    assert_eq!(frame.grid.get(90, 5).unwrap().symbol, "|");
    assert_eq!(frame.grid.get(91, 5).unwrap().symbol, " ");
    for x in 9..=90 {
        assert!(!frame.grid.get(x, 26).unwrap().continuation);
        assert_eq!(
            frame.grid.get(x, 26).unwrap().symbol,
            if x == 9 || x == 90 { "+" } else { "-" }
        );
    }
}

#[test]
fn tool_menu_failed_probe_offers_start_instead_of_stop_and_restart() {
    let config = crate::config::load_bundled_config().config;
    let mut menu = tool_menu_for_render();
    for can_start in [true, false] {
        menu.rows[0].can_start = can_start;
        let mut frame = crate::ui::grid::Frame {
            grid: crate::ui::CellGrid::new(120, 40),
            cursor: None,
        };
        crate::ui::render::tools::render_tool_menu(&mut frame, &menu, &config, 120, 40);
        let screen = (0..40)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("not working"));
        assert!(screen.contains(if can_start {
            "Enter start"
        } else {
            "open a matching file to start"
        }));
        assert!(!screen.contains("x stop"));
        assert!(!screen.contains("R restart"));
    }
    // A failed version probe does not imply an already-running server has stopped.
    for state in ["running", "running ×2", "starting", "restarting"] {
        menu.rows[0].session_state = state.into();
        let mut frame = crate::ui::grid::Frame {
            grid: crate::ui::CellGrid::new(120, 40),
            cursor: None,
        };
        crate::ui::render::tools::render_tool_menu(&mut frame, &menu, &config, 120, 40);
        let screen = (0..40)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("x stop"), "{state}");
        assert!(screen.contains("R restart"), "{state}");
        assert!(!screen.contains("Enter start"), "{state}");
    }
}

#[test]
fn runtime_picker_keeps_selection_visible_when_navigation_used_a_taller_screen() {
    let loaded = crate::config::load_bundled_config();
    let menu = crate::ui::render::ToolMenuState {
        category: "lsp".into(),
        selected: 0,
        scroll: 0,
        rows: Vec::new(),
        query: String::new(),
        searching: false,
        runtime_picker: Some(crate::ui::render::ToolRuntimePickerState {
            server_id: "rust-analyzer".into(),
            selected: 5,
            scroll: 0,
            options: (0..8)
                .map(|n| crate::ui::render::ToolRuntimeOption {
                    source: "path".into(),
                    version_pin: None,
                    label: "PATH".into(),
                    version: format!("VERSION{n}"),
                    release: String::new(),
                    location: String::new(),
                    status: String::new(),
                    note: String::new(),
                    selected: false,
                    installed: true,
                    installable: false,
                    uninstallable: false,
                })
                .collect(),
        }),
    };
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(80, 24),
        cursor: None,
    };
    crate::ui::render::tools::render_tool_menu(&mut frame, &menu, &loaded.config, 80, 24);
    assert!((0..24).any(|y| frame.grid.row_text(y).contains("VERSION5")));
    for height in 6..=10 {
        let mut frame = crate::ui::grid::Frame {
            grid: crate::ui::CellGrid::new(80, height),
            cursor: None,
        };
        crate::ui::render::tools::render_tool_menu(&mut frame, &menu, &loaded.config, 80, height);
        assert!((0..height).any(|y| frame.grid.row_text(y).contains("Resize to choose a build")));
        assert!(!(0..height).any(|y| frame.grid.row_text(y).contains("VERSION5")));
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
        crate::ui::render::documentation::wrap_ranges(&"·".repeat(6), 10, crate::config::AmbiguousWidth::Wide).len()
            > 1
    );
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
    assert!(crate::ui::render::workspace::render_non_tiled_editor_window(
        &mut frame, &state, &ui, &config, &projected
    ));
    assert!(frame.grid.row_text(2).contains("second"));
    assert_eq!(frame.cursor.map(|(_, y, _)| y), Some(2));
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

#[test]
fn insert_selection_highlighting_is_half_open_while_visual_selection_is_inclusive() {
    use crate::editor::text::ByteOffset;
    use crate::editor::text::TextBuffer;
    use crate::editor::EditorMode;
    use crate::editor::Selection;
    use crate::editor::SelectionKind;
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
            let frame = crate::ui::render::render_editor_view(&state, view, &UiState::default(), &config, 8, 2);
            let selected = std::array::from_fn(|offset| {
                frame.grid.get(offset as u16, 0).unwrap().style == crate::ui::Style::Selection
            });
            assert_eq!(selected, expected);
        }
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

#[test]
fn content_tabs_keep_literal_placeholders_and_measure_visible_replacements() {
    let config = crate::config::load_bundled_config().config;
    let entries = [
        "a{dirty}.rs".to_owned(),
        format!("a{}.rs", "\u{200b}".repeat(10)),
        "last.rs".into(),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, title)| crate::ui::render::ContentTabEntry {
        identity: crate::ui::render::ContentTabIdentity(crate::windows::ContentId(index as u64)),
        kind: crate::ui::render::ContentTabKind::Text,
        title,
        active: index == 0,
        dirty: false,
        pinned: false,
        diagnostic: None,
        icon: "R".into(),
        closeable: false,
    })
    .collect::<Vec<_>>();
    let mut grid = crate::ui::CellGrid::new(120, 1);
    crate::ui::render::render_content_tab_line(&mut grid, &entries, &config, 0);
    assert!(grid.row_text(0).contains("a{dirty}.rs"));
    let separators = (0..120)
        .filter(|x| grid.get(*x, 0).unwrap().symbol == BUFFER_SEPARATOR)
        .collect::<Vec<_>>();
    let mut start = 0;
    for (entry, separator) in entries.iter().zip(separators) {
        assert_eq!(
            usize::from(separator + 1 - start),
            crate::ui::render::content_tab_width(entry, &config)
        );
        for column in start..=separator {
            assert_eq!(
                crate::ui::render::content_tab_at_column(&entries, &config, 120, column),
                Some(entry.identity)
            );
        }
        start = separator + 1;
    }
    // The last two cells are painted over the clipped first tab by the overflow marker.
    let mut grid = crate::ui::CellGrid::new(12, 1);
    crate::ui::render::render_content_tab_line(&mut grid, &entries, &config, 0);
    assert!(grid.row_text(0).ends_with(BUFFER_OVERFLOW_RIGHT));
    for column in 10..12 {
        assert_eq!(
            crate::ui::render::content_tab_at_column(&entries, &config, 12, column),
            None
        );
    }
}

#[test]
fn content_tabs_follow_the_last_of_many_entries() {
    let config = crate::config::load_bundled_config().config;
    let entries = (0..1000)
        .map(|index| crate::ui::render::ContentTabEntry {
            identity: crate::ui::render::ContentTabIdentity(crate::windows::ContentId(index)),
            kind: crate::ui::render::ContentTabKind::Text,
            title: format!("file-{index}.rs"),
            active: index == 999,
            dirty: false,
            pinned: false,
            diagnostic: None,
            icon: "R".into(),
            closeable: false,
        })
        .collect::<Vec<_>>();
    let mut grid = crate::ui::CellGrid::new(40, 1);
    crate::ui::render::render_content_tab_line(&mut grid, &entries, &config, 0);
    assert!(grid.row_text(0).contains("file-999.rs"));
    let column = (0..40)
        .rev()
        .find(|x| grid.get(*x, 0).unwrap().symbol == "9")
        .unwrap();
    assert_eq!(
        crate::ui::render::content_tab_at_column(&entries, &config, 40, column),
        Some(entries[999].identity)
    );
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
    let panel = crate::ui::render::explorer_panel_width(&loaded.config, 80).expect("explorer width");
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

#[test]
fn dashboard_labels_leave_a_gap_before_shortcut_keys() {
    let loaded = crate::config::load_bundled_config();
    let mut dashboard = loaded.dashboard;
    dashboard.minimum_width = 1;
    dashboard.minimum_height = 1;
    let actions = vec![crate::ui::render::DashboardAction {
        command: "recovery.open".into(),
        icon: String::new(),
        label: "Recover Unsaved Work".into(),
        key: Some("R".into()),
    }];
    let frame = crate::ui::render::overlays::render_dashboard(
        Path::new("/tmp"),
        &actions,
        0,
        &dashboard,
        &loaded.config,
        20,
        24,
    );
    let (x, y, _) = frame.cursor.unwrap();
    assert!(frame.grid.row_text(y).contains("Recover Unsave R"));
    assert_eq!(frame.grid.get(x + 14, y).unwrap().symbol, " ");
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
                let frame = crate::ui::render::editor::render_editor_view(&state, view, &ui, &config, 32, 1);
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
        path: path.clone(),
        range: ByteRange::empty(ByteOffset(0)),
        severity: crate::workspace::DiagnosticSeverity::Error,
        source: String::new(),
        code: None,
        message: "bad".into(),
    }];
    ui.expanded_diagnostic = Some(crate::ui::render::ExpandedDiagnosticState {
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
            let relative = model.byte_at_cell(crate::ui::render::VisualRowAnchor { line: 1, wrap_row }, 6);
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
