//! Window tests for editor surfaces and frame composition.

use crate::editor::EditorState;
use crate::ui::render::UiState;
use std::path::PathBuf;

use crate::ui::render::composition_tests::attach_test_compositor;
use crate::ui::render::composition_tests::ui_with_editor_layout;

#[test]
fn missing_application_window_snapshot_fails_closed() {
    let mut state = EditorState::new(PathBuf::from("/workspace"));
    state
        .open_text(
            crate::editor::text::TextBuffer::from_text("not rendered"),
            None,
        )
        .unwrap();
    let loaded = crate::config::load_bundled_config();
    let ui = UiState::default();
    assert!(crate::ui::render::pane_viewports(&state, &ui, &loaded.config, 40, 8).is_empty());
    let frame = crate::ui::render::render(&state, &ui, &loaded.config, &loaded.dashboard, 40, 8);
    assert!(frame.cursor.is_none());
    assert!(!(0..8).any(|row| frame.grid.row_text(row).contains("not rendered")));

    let mut stale = ui_with_editor_layout(&state);
    attach_test_compositor(&state, &mut stale, &loaded.config, 40, 8);
    assert_eq!(
        crate::ui::render::pane_viewports(&state, &stale, &loaded.config, 40, 7).len(),
        1
    );
    let valid = crate::ui::render::render(&state, &stale, &loaded.config, &loaded.dashboard, 40, 8);
    assert!(valid.cursor.is_some());
    assert!((0..8).any(|row| valid.grid.row_text(row).contains("not rendered")));
    state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .expect("change source layout");
    assert!(crate::ui::render::pane_viewports(&state, &stale, &loaded.config, 40, 7).is_empty());
    let frame = crate::ui::render::render(&state, &stale, &loaded.config, &loaded.dashboard, 40, 8);
    assert!(frame.cursor.is_none());
    assert!(!(0..8).any(|row| frame.grid.row_text(row).contains("not rendered")));
}
