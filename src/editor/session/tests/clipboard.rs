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
        let (_app_dirs, mut app) = app();
        app.set_dashboard_visibility(false);
        let (buffer, view) = app.state.open_text(TextBuffer::from_text("abc"), None).unwrap();
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
            app.invoke(CommandInvocation::new("view.cursor.move", InvocationSource::Macro).unwrap()
                .with_argument("motion", CommandValue::String("left".into())));
            app.invoke(CommandInvocation::new("edit.delete_char", InvocationSource::Macro).unwrap());
        } else {
            press(&mut app, 'h');
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

fn invocation(id: &str) -> CommandInvocation {
    CommandInvocation::new(id, InvocationSource::Test).unwrap()
}

fn paste() -> CommandInvocation {
    invocation("edit.put").with_argument("before", CommandValue::Bool(false))
}

#[test]
fn clipboard_macro_stops_after_a_failed_command_following_a_paste() {
    let (_app_dirs, mut app) = app();
    app.set_dashboard_visibility(false);
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("abc"), None).unwrap();
    app.clipboard = ClipboardSession::pending_copy_fixture(app.state.registers.generation(), "X");
    app.state.registers.set_macro('a', vec![
        paste(),
        invocation("view.cursor.move")
            .with_argument("motion", CommandValue::String("find_forward".into()))
            .with_argument("character", CommandValue::String("z".into())),
        invocation("edit.delete_char"),
    ]);
    app.replay_macro('a', 1);
    assert!(app.clipboard.waiting());
    assert_eq!(app.input.replay_depth(), 1);
    app.process_clipboard_paste();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "aXbc");
    assert!(app.ui.notification_error);
    assert_eq!(app.input.replay_depth(), 0);
}

#[test]
fn clipboard_failed_reads_stop_suspended_macros_but_release_later_keys() {
    let (_app_dirs, mut app) = app();
    app.set_dashboard_visibility(false);
    let (buffer, view) = app.state.open_text(TextBuffer::from_text("abc"), None).unwrap();
    app.clipboard = ClipboardSession::pending_copy_fixture(app.state.registers.generation(), "X");
    app.state.registers.set_macro('a', vec![paste(), invocation("edit.delete_char")]);
    app.replay_macro('a', 1);
    let continuation = app.clipboard.take_deferred();
    let (session, reply) = ClipboardSession::pending_read_fixture(
        paste(), app.state.window_version(), app.state.registers.generation(),
    );
    app.clipboard = session;
    app.clipboard.append_deferred(continuation);
    press(&mut app, 'l');
    reply.send(None).unwrap();
    app.process_clipboard_paste();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "abc");
    assert_eq!(app.state.views[view].cursor, ByteOffset(1));
    assert_eq!(app.input.replay_depth(), 0);
    assert!(app.ui.notification_error);
}

#[test]
fn clipboard_recursive_macros_keep_the_recursion_limit_across_pastes() {
    let (_app_dirs, mut app) = app();
    app.set_dashboard_visibility(false);
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("a"), None).unwrap();
    app.clipboard = ClipboardSession::pending_copy_fixture(app.state.registers.generation(), "X");
    app.state.registers.set_macro('a', vec![
        paste(),
        invocation("macro.replay").with_argument("register", CommandValue::String("a".into())),
    ]);
    app.replay_macro('a', 1);
    for _ in 0..=crate::editor::session::dispatch::MACRO_RECURSION_LIMIT {
        app.process_clipboard_paste();
    }
    assert!(!app.clipboard.waiting());
    assert_eq!(app.input.replay_depth(), 0);
    assert!(app.ui.notification_error);
    assert_eq!(app.state.buffers[buffer].text.to_string(), format!("a{}", "X".repeat(crate::editor::session::dispatch::MACRO_RECURSION_LIMIT)));
}

#[test]
fn clipboard_deferred_macros_expand_before_later_input() {
    let (_app_dirs, mut app) = app();
    app.set_dashboard_visibility(false);
    let (buffer, view) = app.state.open_text(TextBuffer::from_text("abc"), None).unwrap();
    app.clipboard = ClipboardSession::pending_copy_fixture(app.state.registers.generation(), "X");
    app.invoke(paste());
    app.state.registers.set_macro('a', vec![
        paste(),
        invocation("view.cursor.move").with_argument("motion", CommandValue::String("right".into())),
        invocation("edit.delete_char"),
    ]);
    app.invoke(invocation("macro.replay").with_argument("register", CommandValue::String("a".into())));
    press(&mut app, 'h');
    app.process_clipboard_paste();
    app.process_clipboard_paste();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "aXXc");
    assert_eq!(app.state.views[view].cursor, ByteOffset(2));
    assert_eq!(app.input.replay_depth(), 0);
}

#[test]
fn clipboard_counted_macros_finish_each_replay_before_starting_the_next() {
    let (_app_dirs, mut app) = app();
    app.set_dashboard_visibility(false);
    let (buffer, view) = app.state.open_text(TextBuffer::from_text("abc"), None).unwrap();
    app.clipboard = ClipboardSession::pending_copy_fixture(app.state.registers.generation(), "X");
    app.state.registers.set_macro('a', vec![paste()]);
    for key in "20@ah".chars() {
        press(&mut app, key);
    }
    for _ in 0..20 {
        app.process_clipboard_paste();
    }
    assert!(!app.clipboard.waiting());
    assert_eq!(app.state.buffers[buffer].text.to_string(), format!("a{}bc", "X".repeat(20)));
    assert_eq!(app.state.views[view].cursor, ByteOffset(19));
    assert_eq!(app.input.replay_depth(), 0);
    assert!(!app.ui.notification_error);
}

#[test]
fn clipboard_failed_paste_reports_error_and_releases_later_input() {
    let (_app_dirs, mut app) = app();
    app.set_dashboard_visibility(false);
    let (buffer, view) = app.state.open_text(TextBuffer::from_text("abc"), None).unwrap();
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
        let (_app_dirs, mut app) = app();
        app.set_dashboard_visibility(false);
        if register.is_some() {
            let mut fixture = app.configuration.fixture_snapshot();
            fixture.settings.editor.registers.clipboard = crate::config::ClipboardSync::Off;
            app.configuration.install_fixture(fixture);
        }
        let (buffer, _) = app.state.open_text(TextBuffer::from_text("ab"), None).unwrap();
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
