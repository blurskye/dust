use std::path::PathBuf;
use crate::editor::EditorState;
use crate::ui::render::CompletionItem;
use crate::ui::render::DocumentationMarkupKind;
use crate::ui::render::DocumentationPopupKind;
use crate::ui::render::DocumentationPopupState;
use crate::ui::render::DocumentationSection;

use crate::ui::render::composition_tests::ui_with_editor_layout;
use crate::ui::render::composition_tests::attach_test_compositor;
use crate::ui::render::composition_tests::render;


#[test]
fn editor_popups_are_suppressed_while_a_modal_owns_input() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(crate::editor::text::TextBuffer::from_text("code"), None)
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    for modal in 0..3 {
        let mut ui = ui_with_editor_layout(&state);
        match modal {
            0 => ui.settings = Some(crate::ui::render::SettingsPageState::default()),
            1 => {
                ui.tool_menu = Some(crate::ui::render::ToolMenuState {
                    category: "lsp".into(),
                    selected: 0,
                    scroll: 0,
                    rows: Vec::new(),
                    runtime_picker: None,
                    query: String::new(),
                    searching: false,
                })
            }
            _ => {
                ui.picker = Some(crate::ui::render::PickerUiState {
                    preview_side_scroll: (0, 0),
                    footer: None,
                    input_hint: String::new(),
                    surface: crate::ui::render::PickerSurfaceKind::Generic,
                    title: "Picker".into(),
                    query: String::new(),
                    rows: Vec::new(),
                    selected: None,
                    scroll: 0,
                    total: 0,
                    loading: false,
                    preview: None,
                })
            }
        }
        let expected = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 24);
        ui.language_completion = Some(crate::ui::render::LanguageCompletionState {
            typed: 0,
            items: vec![CompletionItem {
                icon: None,
                color: None,
                label: "candidate".into(),
                detail: String::new(),
            }],
            selected: 0,
            scroll: 0,
        });
        ui.documentation_popup = Some(DocumentationPopupState {
            kind: DocumentationPopupKind::Hover,
            title: "Hover".into(),
            provider: String::new(),
            source_range: None,
            scroll: 0,
            sections: vec![DocumentationSection {
                heading: None,
                markup: DocumentationMarkupKind::PlainText,
                lines: vec!["old documentation".into()],
                spans: Vec::new(),
            }],
        });
        ui.lsp_loading = Some("Loading".into());
        let actual = render(&state, &ui, &loaded.config, &loaded.dashboard, 80, 24);
        assert_eq!(actual.grid, expected.grid, "modal {modal}");
    }
}


#[test]
fn workspace_editor_cursor_follows_compositor_focus() {
    use crate::windows::CompositorLayer;
    use crate::windows::ProjectedCompositorWindow;
    use crate::windows::WindowId;
    use crate::windows::WindowRect;

    let loaded = crate::config::load_bundled_config();
    for split in [false, true] {
        let mut state = EditorState::new(PathBuf::from("/workspace"));
        state
            .open_text(crate::editor::text::TextBuffer::from_text("editor"), None)
            .unwrap();
        if split {
            state
                .split_active(crate::editor::Axis::Horizontal, 500)
                .unwrap();
        }
        let mut ui = ui_with_editor_layout(&state);
        attach_test_compositor(&state, &mut ui, &loaded.config, 60, 8);
        let frame = crate::ui::render::render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 8);
        assert!(frame.cursor.is_some());

        let windows = &mut ui.compositor_layout.as_mut().unwrap().projection.windows;
        for window in windows.iter_mut() {
            window.focused = false;
        }
        windows.push(ProjectedCompositorWindow {
            window: WindowId(99),
            rect: WindowRect {
                x: 50,
                y: 0,
                width: 10,
                height: 3,
            },
            layer: CompositorLayer::Overlay,
            z_index: 1,
            focused: true,
            focusable: true,
        });
        ui.plugin_surfaces.push(crate::ui::render::PluginSurfaceUiState {
            handle: 9,
            window: WindowId(99),
            tree_revision: 1,
            scroll: 0,
            rows: vec![crate::ui::render::PluginSurfaceRow {
                node: 1,
                depth: 0,
                text: "plugin".into(),
                style: crate::ui::Style::Accent,
                actionable: true,
                focused: true,
            }],
        });
        let frame = crate::ui::render::render(&state, &ui, &loaded.config, &loaded.dashboard, 60, 8);
        assert!(frame.grid.row_text(0).contains("plugin"));
        assert!(frame.cursor.is_none());
    }
}