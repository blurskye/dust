//! Prompt tests for editor surfaces and frame composition.

use crate::editor::EditorState;
use crate::ui::render::PromptKind;
use crate::ui::render::PromptState;
use crate::ui::render::UiState;
use crate::ui::render::prompt_geometry;
use std::path::PathBuf;

use crate::ui::render::composition_tests::editor_layout;
use crate::ui::render::composition_tests::render;

#[test]
fn prompt_is_globally_centered_and_clamped() {
    let geometry = prompt_geometry(120, 40, false);
    assert_eq!(geometry.left, 30);
    assert_eq!(geometry.top, 2);
    assert_eq!(geometry.width, 60);
    let narrow = prompt_geometry(30, 8, false);
    assert_eq!(narrow.left, 1);
    assert_eq!(narrow.width, 28);
}

#[test]
fn prompt_help_grows_or_wraps_without_losing_the_instruction_or_input() {
    let loaded = crate::config::load_bundled_config();
    let message = "Discard hunk 2 in src/settings.txt. Type `reset this hunk`.";
    for kind in [
        PromptKind::GitReset,
        PromptKind::Text,
        PromptKind::ExplorerRename,
    ] {
        for width in [160, 80, 40, 26] {
            let mut prompt = PromptState::new(kind);
            prompt.help = Some(message.into());
            prompt.input = "draft".into();
            prompt.cursor = prompt.input.len();
            let geometry = crate::ui::prompt_geometry_for_state(width, 24, &loaded.config, &prompt);
            let mut frame = crate::ui::grid::Frame {
                grid: crate::ui::CellGrid::new(width, 24),
                cursor: None,
            };
            crate::ui::render::prompt::render_prompt(
                &mut frame,
                &prompt,
                &loaded.config,
                width,
                24,
            );
            let expected = crate::ui::render::wrap_plain_text(
                message,
                usize::from(geometry.width - 4),
                loaded.config.editor.ambiguous_width,
            );
            for (row, line) in expected.iter().enumerate() {
                let y = geometry.top + 1 + row as u16;
                assert!(
                    frame.grid.row_text(y).contains(line),
                    "{kind:?} at {width}: {line:?}"
                );
                assert_eq!(
                    frame
                        .grid
                        .get(geometry.left + geometry.width - 1, y)
                        .unwrap()
                        .style,
                    crate::ui::Style::Border
                );
            }
            let input_y = geometry.top + geometry.height - 2;
            assert!(frame.grid.row_text(input_y).contains("draft"));
            assert_eq!(frame.cursor.unwrap().1, input_y);
            assert_eq!(geometry.height, expected.len() as u16 + 3);
            assert!(geometry.left + geometry.width <= width);
        }
    }
}

#[test]
fn prompt_help_wraps_unicode_multiline_and_long_paths() {
    let loaded = crate::config::load_bundled_config();
    let message = format!(
        "界 e\u{301} instruction\n/{}\nType `reset this hunk`.",
        "long-path/".repeat(8)
    );
    let mut prompt = PromptState::new(PromptKind::Text);
    prompt.help = Some(message.clone());
    let geometry = crate::ui::prompt_geometry_for_state(30, 60, &loaded.config, &prompt);
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(30, 60),
        cursor: None,
    };
    crate::ui::render::prompt::render_prompt(&mut frame, &prompt, &loaded.config, 30, 60);
    let expected = crate::ui::render::wrap_plain_text(
        &message,
        usize::from(geometry.width - 4),
        loaded.config.editor.ambiguous_width,
    );
    for (row, line) in expected.iter().enumerate() {
        assert!(
            frame
                .grid
                .row_text(geometry.top + 1 + row as u16)
                .contains(line)
        );
    }
    assert!(
        frame
            .grid
            .row_text(geometry.top + geometry.height - 3)
            .contains("hunk`.")
    );
}

#[test]
fn short_prompt_help_and_error_layouts_remain_unchanged() {
    let loaded = crate::config::load_bundled_config();
    for (help, error) in [
        (None, None),
        (Some("Short instruction."), None),
        (Some("Hidden help"), Some("Error")),
    ] {
        let mut prompt = PromptState::new(PromptKind::Command);
        prompt.help = help.map(str::to_owned);
        prompt.error = error.map(str::to_owned);
        assert_eq!(
            crate::ui::prompt_geometry_for_state(120, 40, &loaded.config, &prompt),
            prompt_geometry(120, 40, help.is_some() || error.is_some()),
        );
    }
}

#[test]
fn prompt_help_reports_when_the_terminal_cannot_fit_the_message() {
    let loaded = crate::config::load_bundled_config();
    let mut prompt = PromptState::new(PromptKind::GitReset);
    prompt.help = Some("a long instruction ".repeat(20));
    let mut frame = crate::ui::grid::Frame {
        grid: crate::ui::CellGrid::new(40, 5),
        cursor: None,
    };
    crate::ui::render::prompt::render_prompt(&mut frame, &prompt, &loaded.config, 40, 5);
    assert!((0..5).any(|y| {
        frame
            .grid
            .row_text(y)
            .contains("Resize to read the full message")
    }));
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
    let geometry = prompt_geometry(100, 20, true);

    assert_eq!(geometry.left, 20);
    assert_eq!(geometry.top, 2);
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
fn prompt_clears_and_clips_the_editor_layer_beneath_its_box() {
    let mut state = EditorState::new(PathBuf::from("/tmp/dust-workspace"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text(&vec!["X".repeat(100); 10].join("\n")),
            None,
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let base_ui = UiState {
        editor_window_layout: Some(editor_layout(&state)),
        ..UiState::default()
    };
    let geometry = prompt_geometry(100, 20, false);
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
fn command_completion_details_share_the_row_background_and_selection() {
    use crate::ui::Style;
    let config = crate::config::load_bundled_config().config;
    for (width, height) in [(160, 45), (100, 30), (80, 24)] {
        let mut prompt = PromptState::new(PromptKind::Command);
        prompt.completions = vec![crate::ui::render::CompletionItem {
            icon: None,
            label: "w".into(),
            detail: "Save file".into(),
            color: None,
        }];
        for selected in [None, Some(0)] {
            prompt.selected_completion = selected;
            let mut frame = crate::ui::grid::Frame {
                grid: crate::ui::CellGrid::new(width, height),
                cursor: None,
            };
            crate::ui::render::prompt::render_prompt(&mut frame, &prompt, &config, width, height);
            let geometry = prompt_geometry(width, height, false);
            let y = geometry.top + 3;
            assert_eq!(
                frame.grid.get(geometry.left + 2, y).unwrap().style,
                if selected.is_some() {
                    Style::Selection
                } else {
                    Style::Status
                }
            );
            assert_eq!(
                frame.grid.get(geometry.left + 5, y).unwrap().style,
                if selected.is_some() {
                    Style::Selection
                } else {
                    Style::StatusMuted
                }
            );
        }
    }
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
        ui.dashboard_actions = vec![crate::ui::render::DashboardAction {
            command: "workspace.file.find".into(),
            label: "Find File".into(),
            icon: String::new(),
            key: Some("f".into()),
        }];
        let dashboard = render(&state, &ui, &config, &dashboard_config, width, height);
        assert_eq!(dashboard.grid.width(), width);
        assert!(
            dashboard.grid.row_text(height.saturating_sub(1)).len()
                >= usize::from(width.saturating_sub(2))
                || width < 3
        );
        let rows = (0..height)
            .map(|y| dashboard.grid.row_text(y))
            .collect::<Vec<_>>();
        if width >= dashboard_config.minimum_width && height >= dashboard_config.minimum_height {
            assert!(rows.iter().any(|row| row.contains("Find File")), "{rows:?}");
            assert!(
                rows.iter().any(|row| row.contains("dust-workspace")),
                "{rows:?}"
            );
            assert!(dashboard.cursor.is_some());
        } else {
            assert!(
                rows.iter().any(|row| row.contains("Dust — resize")),
                "{rows:?}"
            );
            assert!(dashboard.cursor.is_none());
        }
        ui.prompt = Some(PromptState::new(PromptKind::Command));
        let prompt = render(&state, &ui, &config, &dashboard_config, width, height);
        let geometry = prompt_geometry(width, height, false);
        if height > 2 && width > 2 {
            assert!(prompt.grid.row_text(geometry.top).contains("Command") || width < 20);
        }
    }
}
