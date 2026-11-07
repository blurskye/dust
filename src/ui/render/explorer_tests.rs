//! Explorer tests for editor surfaces and frame composition.

use crate::editor::EditorState;
use crate::ui::render::UiState;
use std::path::PathBuf;

use crate::ui::render::composition_tests::editor_layout;
use crate::ui::render::composition_tests::render;

#[test]
fn buffer_line_begins_after_the_explorer_dock() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("one"),
            Some(PathBuf::from("/workspace/one.rs")),
        )
        .unwrap();
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("two"),
            Some(PathBuf::from("/workspace/two.rs")),
        )
        .unwrap();
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        content_tabs: vec![
            crate::ui::render::ContentTabEntry {
                identity: crate::ui::render::ContentTabIdentity(crate::windows::ContentId(1)),
                kind: crate::ui::render::ContentTabKind::Text,
                title: "one.rs".into(),
                active: false,
                dirty: false,
                pinned: false,
                diagnostic: None,
                icon: "R".into(),
                closeable: true,
                segments: None,
            },
            crate::ui::render::ContentTabEntry {
                identity: crate::ui::render::ContentTabIdentity(crate::windows::ContentId(2)),
                kind: crate::ui::render::ContentTabKind::Text,
                title: "two.rs".into(),
                active: true,
                dirty: false,
                pinned: false,
                diagnostic: None,
                icon: "R".into(),
                closeable: true,
                segments: None,
            },
        ],
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
        explorer_focused: true,
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let frame = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 10);
    let panel =
        crate::ui::render::explorer_panel_width(&loaded.config, 80).expect("explorer width");
    assert!(frame.grid.row_text(0).contains("Explorer"));
    let editor_line = (panel.saturating_add(1)..80)
        .map(|x| frame.grid.get(x, 0).map_or("", |cell| cell.symbol.as_str()))
        .collect::<String>();
    assert!(editor_line.contains("one.rs"));
    assert!(editor_line.contains("two.rs"));
}

#[test]
fn explorer_is_a_persistent_left_dock_that_reflows_the_editor() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("editor content"),
            Some(PathBuf::from("/workspace/main.rs")),
        )
        .unwrap();
    let ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        explorer: Some(crate::ui::render::ExplorerUiState {
            title: "Explorer".into(),
            rows: vec![
                crate::ui::render::ExplorerUiRow {
                    node_id: 1,
                    selected: false,
                    segments: vec![
                        crate::ui::render::ExplorerUiSegment {
                            text: "├─".into(),
                            role: crate::ui::render::ExplorerSegmentRole::Tree,
                        },
                        crate::ui::render::ExplorerUiSegment {
                            text: "▾".into(),
                            role: crate::ui::render::ExplorerSegmentRole::Icon,
                        },
                        crate::ui::render::ExplorerUiSegment {
                            text: " ".into(),
                            role: crate::ui::render::ExplorerSegmentRole::Tree,
                        },
                        crate::ui::render::ExplorerUiSegment {
                            text: "src".into(),
                            role: crate::ui::render::ExplorerSegmentRole::Directory,
                        },
                    ],
                    right_segments: vec![
                        crate::ui::render::ExplorerUiSegment {
                            text: "M".into(),
                            role: crate::ui::render::ExplorerSegmentRole::Git,
                        },
                        crate::ui::render::ExplorerUiSegment {
                            text: "E".into(),
                            role: crate::ui::render::ExplorerSegmentRole::Diagnostic,
                        },
                    ],
                },
                crate::ui::render::ExplorerUiRow {
                    node_id: 2,
                    selected: true,
                    segments: vec![
                        crate::ui::render::ExplorerUiSegment {
                            text: "│ └─".into(),
                            role: crate::ui::render::ExplorerSegmentRole::Tree,
                        },
                        crate::ui::render::ExplorerUiSegment {
                            text: "·".into(),
                            role: crate::ui::render::ExplorerSegmentRole::Icon,
                        },
                        crate::ui::render::ExplorerUiSegment {
                            text:
                                " main.rs-with-an-intentionally-overlong-name-that-must-not-spill"
                                    .into(),
                            role: crate::ui::render::ExplorerSegmentRole::File,
                        },
                    ],
                    right_segments: vec![crate::ui::render::ExplorerUiSegment {
                        text: "?".into(),
                        role: crate::ui::render::ExplorerSegmentRole::Diagnostic,
                    }],
                },
            ],
            scroll: 0,
            loading: false,
            total: 2,
            matches: 2,
            query: String::new(),
            searching: false,
        }),
        explorer_focused: true,
        ..UiState::default()
    };
    let loaded = crate::config::load_bundled_config();
    let panel = crate::ui::render::explorer_panel_width(&loaded.config, 100).expect("dock width");
    assert_eq!(panel, 40);
    assert_eq!(
        crate::ui::render::explorer_panel_width(&loaded.config, 30),
        Some(14)
    );
    assert_eq!(
        crate::ui::render::explorer_panel_width(&loaded.config, 2),
        None
    );
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
        "│"
    );
    assert_eq!(
        frame
            .grid
            .get(panel, 2)
            .expect("preserved separator")
            .symbol,
        "│"
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
        "│"
    );
}
