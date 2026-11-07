//! Tools tests for editor surfaces and frame composition.

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
            for_project: true,
            name: "Rust Analyzer".into(),
            kind: "lsp".into(),
            languages: "rust".into(),
            version: String::new(),
            release: String::new(),
            runtime: "PATH".into(),
            lifecycle: "failed".into(),
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
fn tool_menu_explains_empty_filters_and_keeps_long_progress_inside_its_box() {
    let config = crate::config::load_bundled_config().config;
    for (width, height) in [(80, 24), (100, 30), (160, 45)] {
        let mut menu = tool_menu_for_render();
        menu.rows.clear();
        menu.query = "no-such-tool".into();
        let mut frame = crate::ui::grid::Frame {
            grid: crate::ui::CellGrid::new(width, height),
            cursor: None,
        };
        crate::ui::render::tools::render_tool_menu(&mut frame, &menu, &config, width, height);
        let text = (0..height)
            .map(|y| frame.grid.row_text(y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("No matching tools"), "{text}");
        assert!(text.contains("Esc clears the search"), "{text}");

        menu = tool_menu_for_render();
        menu.rows[0].lifecycle =
            "installing (downloading 123456789 / 987654321 bytes, verifying checksum)".into();
        menu.rows[0].name = "A language server with a very long display name".repeat(3);
        let mut frame = crate::ui::grid::Frame {
            grid: crate::ui::CellGrid::new(width, height),
            cursor: None,
        };
        crate::ui::render::tools::render_tool_menu(&mut frame, &menu, &config, width, height);
        let box_width = width * 82 / 100;
        let left = (width - box_width) / 2;
        let right = left + box_width - 1;
        let top = (height - height * 84 / 100) / 2;
        let bottom = top + height * 84 / 100 - 1;
        for y in top + 1..bottom {
            for x in [left, right] {
                assert_eq!(
                    frame.grid.get(x, y).unwrap().symbol,
                    "│",
                    "{width}x{height} row {y}"
                );
            }
            assert_eq!(frame.grid.get(left - 1, y).unwrap().symbol, " ");
            assert_eq!(frame.grid.get(right + 1, y).unwrap().symbol, " ");
        }
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
        assert!(screen.contains("failed"));
        assert!(screen.contains("For this project"));
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
fn project_group_headers_keep_selected_tools_visible_across_the_boundary() {
    let config = crate::config::load_bundled_config().config;
    let mut menu = tool_menu_for_render();
    let row = menu.rows[0].clone();
    menu.rows = (0..30)
        .map(|index| {
            let mut row = row.clone();
            row.name = format!("tool-{index:03}-end");
            row.for_project = index < 3;
            row.lifecycle = "not installed".into();
            row.note = "Not installed".into();
            row
        })
        .collect();
    for (width, height) in [(80, 24), (160, 45)] {
        for selected in 0..menu.rows.len() {
            menu.selected = selected;
            let mut frame = crate::ui::grid::Frame {
                grid: crate::ui::CellGrid::new(width, height),
                cursor: None,
            };
            crate::ui::render::tools::render_tool_menu(&mut frame, &menu, &config, width, height);
            let selected_rows = (0..height)
                .filter(|y| {
                    (0..width).any(|x| {
                        frame.grid.get(x, *y).unwrap().style == crate::ui::Style::Selection
                    })
                })
                .collect::<Vec<_>>();
            assert!(
                selected_rows
                    .iter()
                    .any(|y| frame.grid.row_text(*y).contains(&menu.rows[selected].name)),
                "selected tool {selected} missing at {width}x{height}"
            );
        }
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
