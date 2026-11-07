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
        &[],
        0,
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
        &[],
        0,
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
use std::path::Path;

#[test]
fn dashboard_hides_the_entire_logo_when_the_terminal_leaves_a_short_workspace() {
    let loaded = crate::config::load_bundled_config();
    let actions = loaded
        .dashboard
        .action
        .iter()
        .map(|action| crate::ui::render::DashboardAction {
            command: action.command.clone(),
            label: action.label.clone(),
            icon: action.icon.clone(),
            key: None,
        })
        .collect::<Vec<_>>();
    // Each header line needs one more row than the entries alone; with the bundled
    // entries a 16-row terminal leaves a workspace about this tall.
    let entries = actions.len() as u16;
    let identity_fits = entries + 7;
    let version_fits = entries + 5;
    for height in [entries + 20, identity_fits, version_fits, entries + 2] {
        let frame = crate::ui::render::render_dashboard(
            Path::new("/workspace"),
            &actions,
            0,
            &loaded.dashboard,
            &loaded.config,
            160,
            height,
        );
        let rows = (0..height)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>();
        assert!(rows.iter().all(|row| !row.contains('█')), "height {height}");
        for action in &actions {
            assert!(
                rows.iter().any(|row| row.contains(&action.label)),
                "height {height}: {}",
                action.label
            );
        }
        let version = format!("v{}", env!("CARGO_PKG_VERSION"));
        assert_eq!(
            rows.iter().any(|row| row.contains(&version)),
            height >= version_fits
        );
        assert_eq!(
            rows.iter()
                .any(|row| row.contains(&loaded.dashboard.identity)),
            height >= identity_fits
        );
        let first = rows.iter().position(|row| !row.trim().is_empty()).unwrap();
        let last = rows.iter().rposition(|row| !row.trim().is_empty()).unwrap();
        assert!(
            first.abs_diff(rows.len() - last - 1) <= 1,
            "height {height}"
        );
    }
    // Once the complete header fits, every logo row is present with breathing room.
    let render = |height| {
        crate::ui::render::render_dashboard(
            Path::new("/workspace"),
            &actions,
            0,
            &loaded.dashboard,
            &loaded.config,
            160,
            height,
        )
    };
    let frame = (entries..80)
        .map(render)
        .find(|frame| (0..frame.grid.height()).any(|y| frame.grid.row_text(y).contains('█')))
        .expect("the logo fits on a tall dashboard");
    assert!(frame.grid.row_text(0).trim().is_empty());
    for (index, line) in loaded.dashboard.logo.lines().enumerate() {
        assert_eq!(frame.grid.row_text(index as u16 + 1).trim(), line);
    }
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
