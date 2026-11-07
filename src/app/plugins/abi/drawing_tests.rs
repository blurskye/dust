//! Real guest drawing, diagnostics and authority at the application boundary.

use crate::app::App;
use crate::app::plugins::abi::tests::{action, pump, real_app};
use crate::app::plugins::abi::{
    CommandAccess, CommandAuthority, CommandInvocation, CommandValue, InvocationSource,
};
use crate::editor::text::TextBuffer;

fn frame(app: &mut App) -> crate::ui::Frame {
    app.handle_event(crate::ui::UiEvent::Resize(100, 24));
    app.reconcile_editor_windows();
    crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        100,
        24,
    )
}

fn screen(app: &mut App) -> String {
    let frame = frame(app);
    (0..24)
        .map(|row| frame.grid.row_text(row))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn guest_decorations_draw_follow_edits_and_round_trip_through_read_marks() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "drawing");
    let shown = screen(&mut app);
    assert!(shown.contains("before abZ after"), "{shown}");
    assert!(shown.contains('!'));
    let delivery = app.plugin_delivery("demo").unwrap();
    assert!(
        delivery.marks[0].marks[0]
            .decoration
            .as_ref()
            .unwrap()
            .sign
            .is_some()
    );
    app.set_mode(crate::editor::EditorMode::Insert);
    app.invoke(
        CommandInvocation::new("edit.insert_text", InvocationSource::Test)
            .unwrap()
            .with_argument("text", CommandValue::String("λ".into())),
    );
    assert_eq!(
        app.plugin_delivery("demo").unwrap().marks[0].marks[0].start,
        2
    );
    assert!(screen(&mut app).contains("λbefore abZ after"));
    app.invoke(CommandInvocation::new("history.undo", InvocationSource::Test).unwrap());
    assert!(screen(&mut app).contains("before abZ after"));
    action(&mut app, "demo", "drawing-clear");
    assert!(!screen(&mut app).contains("before"));
    action(&mut app, "demo", "drawing");
    let load = app.extension.load_id("demo").unwrap();
    app.extension.disable("demo").unwrap();
    app.remove_plugin_contributions("demo", load);
    assert!(!screen(&mut app).contains("before"));
}

#[test]
fn guest_diagnostics_join_native_navigation_picker_and_clear_on_unload() {
    let (_roots, mut app) = real_app(&["demo", "other"]);
    let path = app.state.workspace_root.join("drawing.rs");
    app.state
        .open_text(TextBuffer::from_text("abc"), Some(path))
        .unwrap();
    action(&mut app, "demo", "diagnostics");
    action(&mut app, "other", "diagnostics");
    assert_eq!(app.diagnostics.choices().count(), 2);
    assert!(screen(&mut app).contains("check this value"));
    app.invoke(CommandInvocation::new("diagnostic.next", InvocationSource::Test).unwrap());
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()].cursor.0,
        1
    );
    action(&mut app, "demo", "diagnostics-clear");
    assert_eq!(app.diagnostics.choices().count(), 1);
    let load = app.extension.load_id("other").unwrap();
    app.extension.disable("other").unwrap();
    app.remove_plugin_contributions("other", load);
    assert_eq!(app.diagnostics.choices().count(), 0);
    assert!(app.ui.diagnostics.is_empty());
}

#[test]
fn guest_diagnostics_work_in_unnamed_buffers_and_reject_stale_replacements() {
    use crate::app::plugins::abi::wit;
    use slotmap::Key;
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "diagnostics");
    assert!(screen(&mut app).contains("check this value"));
    app.invoke(CommandInvocation::new("diagnostic.next", InvocationSource::Test).unwrap());
    let view = app.state.active_view().unwrap();
    assert_eq!(app.state.views[view].cursor.0, 1);
    let buffer = app.state.views[view].buffer;
    let revision = app.state.buffers[buffer].text.revision().0;
    let authority = app.plugin_authority("demo");
    for (revision, start, end) in [(revision + 1, 0, 1), (revision, 2, 99), (revision, 2, 1)] {
        assert!(
            app.publish_plugin_diagnostics(
                "demo",
                &authority,
                wit::DiagnosticSet {
                    buffer: buffer.data().as_ffi(),
                    revision,
                    namespace: "lint".into(),
                    diagnostics: vec![wit::Diagnostic {
                        start,
                        end,
                        severity: wit::DiagnosticSeverity::Error,
                        message: "invalid".into(),
                        source: "fixture".into()
                    }],
                }
            )
            .is_err()
        );
        assert_eq!(app.diagnostics.choices().count(), 1);
    }
    let mut marks = wit::MarkSet {
        buffer: buffer.data().as_ffi(),
        revision,
        namespace: "drawing".into(),
        marks: Vec::new(),
    };
    action(&mut app, "demo", "drawing");
    marks.marks = app.plugin_delivery("demo").unwrap().marks[0].marks.clone();
    marks.marks[0].decoration.as_mut().unwrap().highlight = Some("not-a-highlight".into());
    assert!(
        app.replace_plugin_marks("demo", &authority, marks)
            .unwrap_err()
            .contains("unknown highlight")
    );
    assert_eq!(app.plugin_delivery("demo").unwrap().marks[0].marks.len(), 3);
}

#[test]
fn guest_drawing_and_completion_cannot_amplify_the_callers_ui_grant() {
    let (_roots, mut app) = real_app(&["demo"]);
    for name in ["drawing", "diagnostics", "completion"] {
        let invocation = CommandInvocation::new("plugin.demo.drive", InvocationSource::Test)
            .unwrap()
            .with_argument("abi", CommandValue::String(name.into()));
        assert!(app.invoke_with_authority(
            invocation,
            CommandAuthority::Plugin {
                initiator: "caller".into(),
                grants: [CommandAccess::Edit].into()
            }
        ));
        pump(&mut app, |app| !app.input.command_pending());
        assert!(
            app.ui
                .notification
                .as_deref()
                .is_some_and(|notice| notice.contains("Ui")),
            "{:?}",
            app.ui.notification
        );
        assert!(app.extension.abi.marks.is_empty());
        assert!(app.extension.abi.diagnostics.is_empty());
        assert!(app.extension.abi.completions.is_empty());
    }
}

#[test]
fn guest_inline_completion_draws_and_disappears_after_acceptance_or_cursor_motion() {
    let (_roots, mut app) = real_app(&["demo"]);
    action(&mut app, "demo", "completion");
    app.set_mode(crate::editor::EditorMode::Insert);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        app.sync_lsp_document();
        app.process_plugin_events();
        if app.ui.inline_completion.is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "completion did not survive LSP synchronization: {:?}",
            app.ui.notification
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    app.sync_lsp_document();
    app.refresh_lsp_projection_owner();
    assert!(screen(&mut app).contains("plugin-valueabc"));
    let view = app.state.active_view().unwrap();
    app.state.views[view].cursor = crate::editor::text::ByteOffset(1);
    let frame = frame(&mut app);
    // The menu still contains the item; the buffer's ghost text must already be gone.
    assert!(!frame.grid.row_text(1).contains("plugin-value"));
    app.state.views[view].cursor = crate::editor::text::ByteOffset(0);
    app.accept_lsp_completion();
    assert!(app.ui.language_completion.is_none());
    assert_eq!(
        app.state.buffers[app.state.views[view].buffer]
            .text
            .to_string(),
        "plugin-valueabc"
    );
    pump(&mut app, |app| app.ui.language_completion.is_some());
    let load = app.extension.load_id("demo").unwrap();
    app.extension.disable("demo").unwrap();
    app.remove_plugin_contributions("demo", load);
    assert!(app.ui.inline_completion.is_none());
    assert!(app.ui.language_completion.is_none());
}
