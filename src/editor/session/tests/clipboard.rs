use super::*;
use crate::editor::session::clipboard::ClipboardSession;
use crate::editor::{CommandInvocation, RegisterShape, RegisterValue};

fn press(app: &mut InteractiveApplication, key: char) {
    app.handle_event(UiEvent::Key {
        key: KeyEvent::character(key),
        kind: crate::ui::KeyInputKind::Press,
    });
}

#[test]
fn clipboard_paste_finishes_before_later_input_or_macro_commands() {
    for macro_commands in [false, true] {
        let mut app = app();
        app.set_dashboard_visibility(false);
        let (buffer, view) = app.state.open_text(TextBuffer::from_text("abc"), None);
        app.state.views[view].cursor = ByteOffset(1);
        let invocation = CommandInvocation::new("edit.put", InvocationSource::Test).unwrap()
            .with_argument("before", CommandValue::Bool(false));
        let (session, reply) = ClipboardSession::pending_read_fixture(
            invocation, app.state.window_version(), app.state.registers.generation(),
        );
        app.clipboard = session;
        if macro_commands {
            app.invoke(CommandInvocation::new("view.cursor.move", InvocationSource::Macro).unwrap()
                .with_argument("motion", CommandValue::String("left".into())));
            app.invoke(CommandInvocation::new("edit.delete_char", InvocationSource::Macro).unwrap());
        } else {
            press(&mut app, 'h');
            press(&mut app, 'x');
        }
        assert_eq!(app.state.views[view].cursor, ByteOffset(1));
        assert_eq!(app.state.buffers[buffer].text.to_string(), "abc");
        app.process_clipboard_paste();
        assert_eq!(app.state.views[view].cursor, ByteOffset(1));
        reply.send(Some("XY".into())).unwrap();
        app.process_clipboard_paste();
        assert_eq!(app.state.buffers[buffer].text.to_string(), "aXYc");
        assert_eq!(app.state.views[view].cursor, ByteOffset(1));
        assert!(!app.clipboard.waiting());
    }
}

#[test]
fn clipboard_failed_paste_reports_error_and_releases_later_input() {
    let mut app = app();
    app.set_dashboard_visibility(false);
    let (buffer, view) = app.state.open_text(TextBuffer::from_text("abc"), None);
    app.state.registers.set('"', RegisterValue {
        text: "stale".into(), shape: RegisterShape::Character,
    }, true, false);
    let invocation = CommandInvocation::new("edit.put", InvocationSource::Test).unwrap()
        .with_argument("before", CommandValue::Bool(false));
    let (session, reply) = ClipboardSession::pending_read_fixture(
        invocation, app.state.window_version(), app.state.registers.generation(),
    );
    app.clipboard = session;
    press(&mut app, 'l');
    reply.send(None).unwrap();
    app.process_clipboard_paste();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "abc");
    assert_eq!(app.state.views[view].cursor, ByteOffset(1));
    assert!(app.ui.notification_error);
    assert!(app.ui.notification.as_ref().unwrap().contains("clipboard"));
}

#[test]
fn clipboard_imported_selection_is_used_by_the_requested_paste_register() {
    for register in [None, Some('+'), Some('*')] {
        let mut app = app();
        app.set_dashboard_visibility(false);
        if register.is_some() {
            let mut fixture = app.configuration.fixture_snapshot();
            fixture.settings.editor.registers.clipboard = crate::config::ClipboardSync::Off;
            app.configuration.install_fixture(fixture);
        }
        let (buffer, _) = app.state.open_text(TextBuffer::from_text("ab"), None);
        for name in ['"', '+', '*'] {
            app.state.registers.set(name, RegisterValue {
                text: "stale".into(), shape: RegisterShape::Character,
            }, true, false);
        }
        let mut invocation = CommandInvocation::new("edit.put", InvocationSource::Test).unwrap()
            .with_argument("before", CommandValue::Bool(false));
        invocation.register = register;
        let (session, reply) = ClipboardSession::pending_read_fixture(
            invocation, app.state.window_version(), app.state.registers.generation(),
        );
        app.clipboard = session;
        reply.send(Some("selection".into())).unwrap();
        app.process_clipboard_paste();
        assert_eq!(app.state.buffers[buffer].text.to_string(), "aselectionb");
    }
}
