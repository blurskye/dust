//! Dashboard tests for editor surfaces and frame composition.

use crate::editor::EditorState;
use crate::ui::render::DocumentationMarkupKind;
use crate::ui::render::DocumentationPopupKind;
use crate::ui::render::DocumentationPopupState;
use crate::ui::render::DocumentationSection;
use crate::ui::render::UiState;
use std::path::PathBuf;

use crate::ui::render::composition_tests::attach_test_compositor;
use crate::ui::render::composition_tests::render;

#[test]
fn notice_popup_shows_on_the_dashboard_without_a_cursor() {
    let state = EditorState::new(PathBuf::from("/tmp/dusty-workspace"));
    let ui = UiState {
        documentation_popup: Some(DocumentationPopupState {
            kind: DocumentationPopupKind::Notice,
            title: "Notification".into(),
            provider: "application".into(),
            source_range: None,
            sections: vec![DocumentationSection {
                heading: None,
                markup: DocumentationMarkupKind::PlainText,
                spans: Vec::new(),
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
fn dashboard_respects_top_docks_and_floating_window_order() {
    use crate::windows::CompositorLayer;
    use crate::windows::WindowRect;
    let state = EditorState::new(PathBuf::from("/workspace"));
    let loaded = crate::config::load_bundled_config();
    let mut ui = UiState {
        dashboard: true,
        terminal_panel: Some(crate::ui::render::TerminalPanelState {
            id: 7,
            profile: "shell".into(),
            tabs: Vec::new(),
            mode: crate::ui::render::TerminalPanelMode::Input,
            running: true,
            exit_code: None,
            truncated: false,
            focused: false,
            input_hint: None,
            columns: 80,
            rows: 7,
            cells: Vec::new(),
            cells_start: 0,
            cursor: None,
            selection: None,
            scrollback_lines: 0,
            scroll_offset: 0,
            requested_height: 8,
        }),
        ..UiState::default()
    };
    attach_test_compositor(&state, &mut ui, &loaded.config, 80, 30);
    let compositor = ui.compositor_layout.as_mut().unwrap();
    let terminal = compositor.surfaces.terminal.unwrap().1;
    let dashboard = compositor.surfaces.dashboard.unwrap();
    let workspace = WindowRect {
        x: 0,
        y: 8,
        width: 80,
        height: 22,
    };
    compositor.projection.workspace_bounds = workspace;
    compositor
        .projection
        .windows
        .iter_mut()
        .find(|w| w.window == terminal)
        .unwrap()
        .rect = WindowRect {
        x: 0,
        y: 0,
        width: 80,
        height: 8,
    };
    let actual = crate::ui::render::render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 30);
    let expected = crate::ui::render::render_dashboard(
        &state.workspace_root,
        &ui,
        &loaded.dashboard,
        &loaded.config,
        workspace.width,
        workspace.height,
    );
    assert!((0..workspace.height).any(|y| !expected.grid.row_text(y).trim().is_empty()));
    for y in 0..workspace.height {
        for x in 0..workspace.width {
            assert_eq!(actual.grid.get(x, workspace.y + y), expected.grid.get(x, y));
        }
    }

    // A tiled dashboard also reflows inside the space left by the terminal dock.
    ui.compositor_layout
        .as_mut()
        .unwrap()
        .projection
        .windows
        .iter_mut()
        .find(|w| w.window == dashboard)
        .unwrap()
        .layer = CompositorLayer::Tiled;
    let tiled = crate::ui::render::render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 30);
    for y in 0..workspace.height {
        assert_eq!(
            tiled.grid.row_text(workspace.y + y),
            expected.grid.row_text(y)
        );
    }

    let compositor = ui.compositor_layout.as_mut().unwrap();
    compositor
        .projection
        .windows
        .iter_mut()
        .find(|w| w.window == dashboard)
        .unwrap()
        .layer = CompositorLayer::Docked;
    compositor
        .projection
        .windows
        .iter_mut()
        .find(|w| w.window == dashboard)
        .unwrap()
        .rect = workspace;
    compositor.projection.workspace_bounds = WindowRect {
        x: 80,
        width: 0,
        ..workspace
    };
    let docked = crate::ui::render::render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 30);
    for y in 0..workspace.height {
        assert_eq!(
            docked.grid.row_text(workspace.y + y),
            expected.grid.row_text(y)
        );
    }

    let rect = WindowRect {
        x: 10,
        y: 5,
        width: 60,
        height: 20,
    };
    let compositor = ui.compositor_layout.as_mut().unwrap();
    for window in &mut compositor.projection.windows {
        window.layer = CompositorLayer::Floating;
        window.rect = rect;
        window.z_index = u16::from(window.window == dashboard);
    }
    compositor
        .projection
        .windows
        .sort_by_key(|window| window.z_index);
    assert_eq!(
        compositor.projection.hit_test(rect.x + 1, rect.y + 1),
        Some(dashboard)
    );
    let actual = crate::ui::render::render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 30);
    let expected = crate::ui::render::render_dashboard(
        &state.workspace_root,
        &ui,
        &loaded.dashboard,
        &loaded.config,
        rect.width,
        rect.height,
    );
    for y in 0..rect.height {
        for x in 0..rect.width {
            assert_eq!(
                actual.grid.get(rect.x + x, rect.y + y),
                expected.grid.get(x, y)
            );
        }
    }
    let compositor = ui.compositor_layout.as_mut().unwrap();
    compositor.projection.windows.reverse();
    assert_eq!(
        compositor.projection.hit_test(rect.x + 1, rect.y + 1),
        Some(terminal)
    );
    let terminal_on_top =
        crate::ui::render::render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 30);
    assert_ne!(
        terminal_on_top.grid.get(rect.x, rect.y),
        actual.grid.get(rect.x, rect.y)
    );
}

use crate::config::DashboardConfig;
use crate::ui::SelectedText;
use crate::ui::Style;
use crate::ui::render::DashboardAction;
use crate::ui::render::DashboardChange;
use crate::ui::render::DashboardChanges;
use crate::ui::render::DashboardFile;
use crate::ui::render::DashboardPanel;
use crate::ui::render::DashboardProject;
use crate::ui::render::DashboardWorkspace;
use std::path::Path;

fn bundled_actions() -> Vec<DashboardAction> {
    let keys = ["f", "g", "e", "r", "t", "s", "c", "p", "q"];
    let actions = crate::config::load_bundled_config().dashboard.action;
    assert_eq!(actions.len(), keys.len());
    actions
        .iter()
        .zip(keys)
        .map(|(action, key)| DashboardAction {
            command: action.command.clone(),
            label: action.label.clone(),
            icon: action.icon.clone(),
            key: Some(key.into()),
        })
        .collect()
}

fn workspace_ui() -> UiState {
    let file = |label: &str, project: Option<&str>, age: &str| DashboardFile {
        path: PathBuf::from(format!("/work/tally/{label}")),
        label: label.into(),
        project: project.map(Into::into),
        age: age.into(),
    };
    let change = |label: &str, code, lines| DashboardChange {
        path: PathBuf::from(format!("/work/tally/{label}")),
        label: label.into(),
        code,
        lines,
    };
    let project = |name: &str, branch: &str| DashboardProject {
        path: PathBuf::from(format!("/work/{name}")),
        name: name.into(),
        branch: Some(branch.into()),
    };
    UiState {
        dashboard: true,
        dashboard_actions: bundled_actions(),
        git_branch: Some("master".into()),
        dashboard_workspace: DashboardWorkspace {
            recent_files: vec![
                file("src/count.rs", None, "2m"),
                file("README.md", None, "1h"),
                file("src/lib.rs", Some("dusty"), "3d"),
            ],
            changes: Some(DashboardChanges {
                files: vec![
                    change("src/count.rs", 'M', Some((12, 3))),
                    change("notes.txt", '?', None),
                ],
                changed: 1,
                untracked: 1,
            }),
            projects: vec![project("tally", "master"), project("dusty", "main")],
            last_commit: Some(("Count characters too".into(), "3h ago".into())),
        },
        ..UiState::default()
    }
}

fn draw_with(
    ui: &UiState,
    configured: &DashboardConfig,
    width: u16,
    height: u16,
) -> (crate::ui::Frame, Vec<String>) {
    let loaded = crate::config::load_bundled_config();
    let frame = crate::ui::render::render_dashboard(
        Path::new("/work/tally"),
        ui,
        configured,
        &loaded.config,
        width,
        height,
    );
    let rows = (0..height).map(|y| frame.grid.row_text(y)).collect();
    (frame, rows)
}

fn draw(ui: &UiState, width: u16, height: u16) -> (crate::ui::Frame, Vec<String>) {
    draw_with(
        ui,
        &crate::config::load_bundled_config().dashboard,
        width,
        height,
    )
}

/// The column `text` starts at in `row`.
fn column(row: &str, text: &str) -> usize {
    let at = row
        .find(text)
        .unwrap_or_else(|| panic!("{text:?} missing from {row:?}"));
    row[..at].chars().count()
}

fn style(frame: &crate::ui::Frame, x: u16, y: u16) -> Style {
    frame.grid.get(x, y).unwrap().style
}

fn version() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

#[test]
fn a_large_dashboard_shows_the_wordmark_beside_the_workspace_over_four_boxes() {
    let (frame, rows) = draw(&workspace_ui(), 160, 39);
    // The wordmark takes rows 1 to 4, with the workspace beside it.
    assert!(
        rows[1].starts_with("    ██▀▀▀▀▀▀▄▄  ██      ██"),
        "{rows:#?}"
    );
    assert!(rows[4].starts_with("    ██▄▄▄▄▄▄▀▀"), "{rows:#?}");
    assert_eq!(style(&frame, 4, 1), Style::Accent);
    assert_eq!(column(&rows[2], "tally · "), 58);
    for text in ["master", " · 1 changed · 1 new"] {
        assert!(rows[2].contains(text), "{}", rows[2]);
    }
    assert_eq!(rows[2].trim_end().chars().count(), 156);
    assert!(rows[2].trim_end().ends_with(&version()));
    assert_eq!(column(&rows[3], "/work/tally"), 58);
    assert_eq!(
        column(&rows[5], "last commit Count characters too 3h ago"),
        58
    );

    // Recent files and changes on row 7, projects and the actions below them.
    assert_eq!(column(&rows[7], "╭─ Recent files "), 2);
    assert!(rows[7].contains(" 1-9 ─╮"), "{}", rows[7]);
    assert_eq!(column(&rows[7], "╮"), 78);
    assert_eq!(column(&rows[7], "╭─ Changes "), 81);
    assert!(rows[7].trim_end().ends_with(" master ─╮"), "{}", rows[7]);
    assert_eq!(column(&rows[22], "╭─ Projects "), 2);
    assert_eq!(column(&rows[22], "╭─ Start "), 81);
    assert_eq!(column(&rows[36], "╰"), 2);

    // The focused box has an accent border and its selected row the selection colours.
    assert_eq!(style(&frame, 2, 7), Style::Accent);
    assert_eq!(style(&frame, 81, 7), Style::Border);
    assert_eq!(column(&rows[9], "1  src/count.rs"), 4);
    assert!(rows[9].contains("2m │"), "{}", rows[9]);
    assert_eq!(style(&frame, 3, 9), Style::Selection);
    assert_eq!(style(&frame, 4, 9), Style::QueryMatch { selected: true });
    // On the selected row the folder stays muted and the name takes the selection's text.
    assert_eq!(
        style(&frame, 8, 9),
        Style::SelectionText(SelectedText::Muted)
    );
    assert_eq!(style(&frame, 11, 9), Style::Selection);
    assert_eq!(style(&frame, 4, 10), Style::Accent);
    assert_eq!(column(&rows[10], "2  README.md"), 4);
    assert!(rows[11].contains("3  src/lib.rs  dusty"), "{}", rows[11]);
    assert_eq!(style(&frame, 7, 11), Style::Muted);
    assert_eq!(style(&frame, 11, 11), Style::Default);

    assert_eq!(column(&rows[9], "M src/count.rs"), 83);
    assert!(rows[9].contains("+12 -3 │"), "{}", rows[9]);
    assert!(rows[10].contains("? notes.txt"), "{}", rows[10]);
    assert!(rows[10].contains("new │"), "{}", rows[10]);

    assert_eq!(column(&rows[24], "tally"), 4);
    assert!(rows[24].contains("/work/tally"), "{}", rows[24]);
    assert_eq!(column(&rows[25], "dusty"), 4);
    assert!(rows[25].contains("main │"), "{}", rows[25]);

    // The actions run in three columns, each key before its label.
    for (y, labels) in [
        (24, ["f  find file", "g  find text", "e  explorer"]),
        (25, ["r  recent files", "t  tutor", "s  restore session"]),
        (26, ["c  config", "p  plugins", "q  quit"]),
    ] {
        let columns = labels.map(|label| column(&rows[y], label));
        assert_eq!(columns, [83, 107, 131], "{}", rows[y]);
    }
    assert_eq!(style(&frame, 83, 24), Style::Accent);
    assert_eq!(style(&frame, 86, 24), Style::Muted);

    assert!(rows[38].starts_with(" ctrl-h/j/k/l boxes · j/k move · enter open · 1-9 recent file"));
    assert_eq!(style(&frame, 0, 38), Style::StatusMuted);
    assert!(frame.cursor.is_none());
}

#[test]
fn an_80_by_24_dashboard_shows_one_header_line_recent_files_and_the_actions() {
    let (_, rows) = draw(&workspace_ui(), 80, 24);
    assert_eq!(
        rows[0].trim(),
        format!("dusty {} · tally · master", version())
    );
    assert_eq!(column(&rows[2], "╭─ Recent files "), 2);
    assert_eq!(rows[2].trim_end().chars().count(), 78);
    assert_eq!(column(&rows[4], "1  src/count.rs"), 4);
    assert!(rows[6].contains("3  src/lib.rs  dusty"));
    let start = rows
        .iter()
        .position(|row| row.contains("─ Start "))
        .unwrap();
    for label in [
        "find file",
        "find text",
        "explorer",
        "recent files",
        "tutor",
        "restore session",
        "config",
        "plugins",
        "quit",
    ] {
        assert!(
            rows[start..].iter().any(|row| row.contains(label)),
            "{label}: {rows:#?}"
        );
    }
    for text in ["██", "Changes", "Projects", "last commit"] {
        assert!(!rows.iter().any(|row| row.contains(text)), "{rows:#?}");
    }
    assert!(rows[23].contains("ctrl-h/j/k/l boxes"));
}

#[test]
fn shorter_windows_drop_the_wordmark_before_the_boxes() {
    let ui = workspace_ui();
    // The pty smoke test's size still fits the wordmark.
    let (_, rows) = draw(&ui, 100, 30);
    assert!(rows[1].contains("██"), "{rows:#?}");
    let (_, rows) = draw(&ui, 120, 24);
    assert!(!rows.iter().any(|row| row.contains("██")), "{rows:#?}");
    assert!(rows[1].starts_with("    tally · "), "{}", rows[1]);
    assert!(rows[1].contains("master · 1 changed"), "{}", rows[1]);
    assert!(
        rows[1]
            .trim_end()
            .ends_with(&format!("dusty {}", version()))
    );
    assert!(rows[3].contains("last commit Count characters too"));
    for title in ["Recent files", "Changes", "Projects", "Start"] {
        assert!(rows.iter().any(|row| row.contains(title)), "{rows:#?}");
    }
    // Below the boxes' height only recent files and the actions are left.
    let (_, rows) = draw(&ui, 120, 18);
    assert!(rows.iter().any(|row| row.contains("Recent files")));
    assert!(!rows.iter().any(|row| row.contains("Changes")));
}

#[test]
fn a_configured_logo_replaces_the_wordmark() {
    let mut configured = crate::config::load_bundled_config().dashboard;
    configured.logo = "TALLY\nworks".into();
    let (_, rows) = draw_with(&workspace_ui(), &configured, 160, 39);
    assert!(rows[1].starts_with("    TALLY"), "{rows:#?}");
    assert!(rows[2].starts_with("    works"), "{rows:#?}");
    assert!(!rows.iter().any(|row| row.contains("██")));
    // The workspace sits beside the logo.
    assert_eq!(column(&rows[2], "tally · "), 4 + 5 + 8);
}

#[test]
fn the_selection_follows_the_focused_box_and_falls_back_when_it_is_hidden() {
    let mut ui = workspace_ui();
    ui.dashboard_focus = Some(DashboardPanel::Projects);
    ui.dashboard_row = 1;
    let (frame, rows) = draw(&ui, 160, 39);
    assert_eq!(style(&frame, 2, 22), Style::Accent);
    assert_eq!(style(&frame, 2, 7), Style::Border);
    assert_eq!(style(&frame, 3, 25), Style::Selection);
    assert_ne!(style(&frame, 3, 24), Style::Selection);
    assert_ne!(style(&frame, 3, 9), Style::Selection);
    assert_eq!(column(&rows[25], "dusty"), 4);

    // At 80x24 there is no projects box, so recent files hold the selection.
    let (frame, _) = draw(&ui, 80, 24);
    assert_eq!(style(&frame, 2, 2), Style::Accent);
    assert_eq!(style(&frame, 3, 5), Style::Selection);

    // With no recent files the first action is selected.
    ui.dashboard_focus = None;
    ui.dashboard_workspace.recent_files.clear();
    let (frame, rows) = draw(&ui, 160, 39);
    assert!(rows[9].contains("Files you open show up here"));
    assert!(!rows[7].contains("1-9"));
    assert_eq!(style(&frame, 81, 22), Style::Accent);
    assert_eq!(style(&frame, 83, 24), Style::QueryMatch { selected: true });
    assert_eq!(
        style(&frame, 86, 24),
        Style::SelectionText(SelectedText::Muted)
    );

    // A selected change keeps its letter's and counts' colours on the selection background.
    ui.dashboard_focus = Some(DashboardPanel::Changes);
    ui.dashboard_row = 0;
    let (frame, rows) = draw(&ui, 160, 39);
    assert_eq!(column(&rows[9], "M src/count.rs"), 83);
    let plus = u16::try_from(column(&rows[9], "+12")).unwrap();
    let minus = u16::try_from(column(&rows[9], "-3")).unwrap();
    for (x, expected) in [
        (82, Style::Selection),
        (83, Style::SelectionText(SelectedText::Changed)),
        (85, Style::SelectionText(SelectedText::Muted)),
        (89, Style::Selection),
        (plus, Style::SelectionText(SelectedText::Added)),
        (minus, Style::SelectionText(SelectedText::Deleted)),
    ] {
        assert_eq!(style(&frame, x, 9), expected, "column {x}");
    }
    assert_eq!(style(&frame, 83, 10), Style::GitSignAdded);
}

#[test]
fn the_changes_box_says_when_the_tree_is_clean_or_not_a_repository() {
    let mut ui = workspace_ui();
    ui.dashboard_workspace.changes = Some(DashboardChanges::default());
    let (_, rows) = draw(&ui, 160, 39);
    assert!(rows[2].contains("master · clean"), "{}", rows[2]);
    assert_eq!(column(&rows[9], "clean"), 83);
    ui.dashboard_workspace.changes = None;
    ui.git_branch = None;
    let (_, rows) = draw(&ui, 160, 39);
    assert_eq!(column(&rows[9], "Not a Git repository"), 83);
    // Without a branch or a status the name stands alone before the version.
    assert_eq!(column(&rows[2], "tally"), 58);
    assert!(!rows[2].contains('·'), "{}", rows[2]);
}

#[test]
fn every_size_draws_without_overflowing() {
    let ui = workspace_ui();
    for width in (1..=220).step_by(7) {
        for height in 1..=50 {
            let (frame, rows) = draw(&ui, width, height);
            assert!(frame.cursor.is_none());
            if width < 20 || height < 7 {
                continue;
            }
            assert!(
                rows.iter().any(|row| row.contains("Start")),
                "{width}x{height}: {rows:#?}"
            );
        }
    }
}

#[test]
fn user_actions_without_keys_or_with_icons_line_up_their_labels() {
    let ui = UiState {
        dashboard_actions: vec![
            DashboardAction {
                command: "command_line.open".into(),
                label: "Console".into(),
                icon: "»".into(),
                key: None,
            },
            DashboardAction {
                command: "editor.quit".into(),
                label: "Exit".into(),
                icon: String::new(),
                key: Some("z".into()),
            },
        ],
        ..UiState::default()
    };
    let (_, rows) = draw(&ui, 160, 39);
    let row = rows.iter().find(|row| row.contains("Console")).unwrap();
    assert!(row.contains("   » Console"), "{row}");
    assert!(row.contains("z    Exit"), "{row}");
    assert_eq!(
        column(row, "Console") - column(row, "   »"),
        column(row, "Exit") - column(row, "z ")
    );
}

#[test]
fn a_leader_key_keeps_two_columns_so_every_action_shows() {
    let mut ui = UiState {
        dashboard_actions: bundled_actions(),
        ..UiState::default()
    };
    ui.dashboard_actions.last_mut().unwrap().key = Some(", q".into());
    let (_, rows) = draw(&ui, 100, 30);
    let row = rows.iter().find(|row| row.contains("find file")).unwrap();
    assert!(row.contains("find text"), "{row}");
    assert!(
        rows.iter().any(|row| row.contains(", q  quit")),
        "{rows:#?}"
    );
}

#[test]
fn a_long_branch_name_gives_way_to_the_project_name_and_path() {
    let mut ui = workspace_ui();
    ui.dashboard_workspace.projects[0].branch = Some("worktree-agent-a87150a942f7389cd".into());
    let (_, rows) = draw(&ui, 100, 30);
    let row = rows.iter().find(|row| row.contains("│ tally")).unwrap();
    assert!(row.contains("│ tally   /work/tally"), "{row}");
    assert!(row.contains('…'), "{row}");
}
