use crate::app::tests::*;




#[test]
pub(in crate::app) fn prompt_mouse_context_is_independently_configurable() {
    let (_app_dirs, mut app) = app();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.entries.push(crate::config::KeymapEntry {
        id: "user.test.prompt-mouse-context-is-independently-configurable".into(),
        context: Some("prompt_mouse".into()),
        mode: String::new(),
        keys: Some("mouse-right-down".into()),
        command: Some("command_line.cancel".into()),
        arguments: Default::default(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    });
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none());
    app.configuration.install_bindings(keymaps, false);
    app.open_prompt(crate::ui::PromptKind::Command, String::new());
    app.handle_event(crate::ui::UiEvent::Mouse {
        key: crate::editor::input::KeyEvent::plain(crate::editor::input::KeyCode::MouseRightDown),
        column: 1,
        row: 1,
    });
    assert!(app.ui.prompt.is_none());
}


#[test]
pub(in crate::app) fn protected_prompt_region_blocks_ordinary_compositor_hit_testing() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcdef"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.open_prompt(crate::ui::PromptKind::WorkspaceTrust, String::new());
    app.reconcile_editor_windows();
    let geometry = crate::ui::prompt_geometry(
        app.window_session.width(),
        app.workspace_render_height(),
        false,
    );
    let point = (
        geometry.left.saturating_add(geometry.width / 2),
        geometry.top.saturating_add(geometry.height / 2),
    );
    let projection = app.compositor_projection().expect("protected projection");
    assert_eq!(projection.protected_regions.len(), 1);
    assert_eq!(projection.hit_test(point.0, point.1), None);

    app.close_prompt();
    app.reconcile_editor_windows();
    assert!(
        app.compositor_projection()
            .expect("ordinary projection")
            .hit_test(point.0, point.1)
            .is_some()
    );
}
use crate::editor::input::KeyEvent;
use crate::editor::text::TextBuffer;
use crate::ui::PromptKind;
use crate::ui::UiEvent;
use crate::workspace::WorkspaceTrust;
use crate::app::compile_keymaps;

