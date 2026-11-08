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

use crate::ui::render::DashboardAction;
use crate::ui::render::DashboardWorkspace;
use std::path::Path;

fn bundled_actions() -> Vec<DashboardAction> {
    // The bundled keymaps give Plugins no dashboard key.
    let keys = [
        Some("f"),
        Some("n"),
        Some("g"),
        Some("r"),
        Some("e"),
        Some("s"),
        Some("t"),
        None,
        Some("R"),
        Some("q"),
    ];
    crate::config::load_bundled_config()
        .dashboard
        .action
        .iter()
        .zip(keys)
        .map(|(action, key)| DashboardAction {
            command: action.command.clone(),
            label: action.label.clone(),
            icon: action.icon.clone(),
            key: key.map(Into::into),
        })
        .collect()
}

fn workspace_ui() -> UiState {
    UiState {
        dashboard: true,
        dashboard_actions: bundled_actions(),
        git_branch: Some("main".into()),
        dashboard_workspace: DashboardWorkspace {
            recent_files: vec!["src/ui/render/overlays.rs".into(), "README.md".into()],
            recent_projects: vec![PathBuf::from("/srv/blurskye-site")],
            git_changes: Some((3, 1)),
        },
        ..UiState::default()
    }
}

fn draw(ui: &UiState, width: u16, height: u16) -> (crate::ui::Frame, Vec<String>) {
    let loaded = crate::config::load_bundled_config();
    let frame = crate::ui::render::render_dashboard(
        Path::new("/tmp/dust-workspace"),
        ui,
        &loaded.dashboard,
        &loaded.config,
        width,
        height,
    );
    let rows = (0..height).map(|y| frame.grid.row_text(y)).collect();
    (frame, rows)
}

#[test]
fn the_dashboard_shows_the_workspace_actions_in_columns_and_recent_lists() {
    let (frame, rows) = draw(&workspace_ui(), 120, 35);
    let row = |text: &str| {
        rows.iter()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("{text} missing: {rows:#?}"))
    };
    let header = row("⠿ dust");
    assert!(rows[header].contains(&format!("v{}", env!("CARGO_PKG_VERSION"))));
    assert!(rows[header + 1].trim().chars().all(|c| c == '─'));
    let workspace = row("dust-workspace   ");
    assert!(header < workspace);
    for text in ["main", "3 changed", "1 untracked"] {
        assert!(rows[workspace].contains(text), "{}", rows[workspace]);
    }
    assert!(rows[workspace + 1].contains("dust-workspace"));
    // Ten actions fill three columns top to bottom, each key before its label.
    let first = row("f  Find File");
    assert!(workspace < first);
    for (offset, labels) in [
        (
            0,
            &["f  Find File", "e  Explorer", "R  Recover Unsaved Work"][..],
        ),
        (1, &["n  New File", "s  Restore Session", "q  Quit"]),
        (2, &["g  Find Text", "t  Tutor — learn by editing"]),
        (3, &["r  Recent Files", "   Plugins"]),
    ] {
        for label in labels {
            assert!(rows[first + offset].contains(*label), "{rows:#?}");
        }
    }
    let lists = row("Recent files");
    assert!(first + 3 < lists);
    assert!(rows[lists].contains("Projects"));
    assert!(rows[lists + 1].contains("src/ui/render/overlays.rs"));
    assert!(rows[lists + 1].contains("blurskye-site"));
    assert!(rows[lists + 2].contains("README.md"));
    // The cursor rests on the first label.
    let (x, y, _) = frame.cursor.expect("dashboard cursor");
    assert_eq!(usize::from(y), first);
    assert_eq!(frame.grid.get(x, y).unwrap().symbol, "F");
}

#[test]
fn moving_through_the_dashboard_goes_down_a_column_then_to_the_next() {
    let mut ui = workspace_ui();
    let cursor = |ui: &UiState| {
        let (frame, _) = draw(ui, 120, 35);
        let (x, y, _) = frame.cursor.expect("dashboard cursor");
        let label = frame
            .grid
            .row_text(y)
            .chars()
            .skip(usize::from(x))
            .collect::<String>();
        (x, y, label)
    };
    let (first_x, first_y, label) = cursor(&ui);
    assert!(label.starts_with("Find File"));
    ui.dashboard_selected = 2;
    let (x, y, label) = cursor(&ui);
    assert_eq!((x, y), (first_x, first_y + 2));
    assert!(label.starts_with("Find Text"));
    ui.dashboard_selected = 4;
    let (x, y, label) = cursor(&ui);
    assert!(x > first_x && y == first_y);
    assert!(label.starts_with("Explorer"));
    ui.dashboard_selected = 99;
    assert!(cursor(&ui).2.starts_with("Quit"));
}

#[test]
fn the_dashboard_keeps_every_action_as_the_window_shrinks() {
    let ui = workspace_ui();
    for width in (20..=140).step_by(6) {
        for height in 7..=40 {
            let (frame, rows) = draw(&ui, width, height);
            if rows.iter().any(|row| row.contains("Dust — resize")) {
                // Only when the actions alone are taller than the window.
                assert!(height < 10, "{width}x{height}: {rows:#?}");
                continue;
            }
            for action in &ui.dashboard_actions {
                let start: String = action.label.chars().take(4).collect();
                assert!(
                    rows.iter().any(|row| row.contains(&start)),
                    "{width}x{height}: {rows:#?}"
                );
            }
            assert!(frame.cursor.is_some());
        }
    }
    // At 80x24 nothing gives way.
    let (_, rows) = draw(&ui, 80, 24);
    for text in [
        "⠿ dust",
        "3 changed",
        "Recent files",
        "Projects",
        "README.md",
    ] {
        assert!(rows.iter().any(|row| row.contains(text)), "{rows:#?}");
    }
    // A short window drops the lists before the workspace name.
    let (_, rows) = draw(&ui, 80, 10);
    assert!(rows.iter().any(|row| row.contains("dust-workspace")));
    assert!(!rows.iter().any(|row| row.contains("Recent files")));
}

#[test]
fn a_terminal_dock_shortens_the_dashboard_from_the_top_parts_down() {
    // A dock above or below leaves the dashboard a short window: the lists and header give
    // way, every action stays, and what is left stays centred.
    let ui = workspace_ui();
    for height in 7..=30 {
        let (_, rows) = draw(&ui, 160, height);
        for action in &ui.dashboard_actions {
            assert!(
                rows.iter().any(|row| row.contains(&action.label)),
                "height {height}: {rows:#?}"
            );
        }
        let first = rows.iter().position(|row| !row.trim().is_empty()).unwrap();
        let last = rows.iter().rposition(|row| !row.trim().is_empty()).unwrap();
        assert!(
            first.abs_diff(rows.len() - last - 1) <= 1,
            "height {height}: {rows:#?}"
        );
        let header = rows.iter().any(|row| row.contains("⠿ dust"));
        let lists = rows.iter().any(|row| row.contains("Recent files"));
        assert!(!lists || header, "height {height}: {rows:#?}");
        assert_eq!(header, height >= 8, "height {height}: {rows:#?}");
    }
}

#[test]
fn a_narrow_dashboard_keeps_keys_apart_from_clipped_labels() {
    let ui = UiState {
        dashboard_actions: vec![DashboardAction {
            command: "recovery.inspect".into(),
            icon: String::new(),
            label: "Recover Unsaved Work".into(),
            key: Some("R".into()),
        }],
        ..UiState::default()
    };
    let (frame, _) = draw(&ui, 20, 24);
    let (x, y, _) = frame.cursor.unwrap();
    assert!(frame.grid.row_text(y).contains("R  Recover Unsav"));
    assert_eq!(frame.grid.get(x - 1, y).unwrap().symbol, " ");
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
    let (frame, rows) = draw(&ui, 80, 24);
    let row = rows.iter().find(|row| row.contains("Console")).unwrap();
    assert!(row.contains("   » Console"), "{row}");
    assert!(row.contains("z    Exit"), "{row}");
    assert!(!rows.iter().any(|row| row.contains("Recent files")));
    let (x, y, _) = frame.cursor.unwrap();
    let label = frame
        .grid
        .row_text(y)
        .chars()
        .skip(usize::from(x))
        .collect::<String>();
    assert!(label.starts_with("Console"));
}
