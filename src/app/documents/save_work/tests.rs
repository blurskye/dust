//! Save-work behavior through ordinary save commands and editor transactions.

use super::*;
use crate::app::tests::app;

fn step(name: &str) -> (SaveStep, Receiver<SaveWorkRequest>) {
    let (requests, receiver) = std::sync::mpsc::sync_channel(8);
    (
        SaveStep {
            name: name.into(),
            requests,
        },
        receiver,
    )
}

fn edit(app: &mut App, buffer: BufferId, text: &str) {
    app.apply_save_text(buffer, text.into()).unwrap();
}

#[test]
fn editable_document_save_delivers_edited_text_and_dirty_quit_uses_normal_policy() {
    let (_dirs, mut app) = app();
    let (save, requests) = step("directory");
    let buffer = app
        .open_editable_document("Files".into(), "directory".into(), "old".into(), save)
        .unwrap();
    edit(&mut app, buffer, "renamed");
    assert!(app.state.buffers[buffer].dirty);
    app.process_effect(crate::app::commands::AppEffect::Quit { force: false });
    assert!(!app.should_quit);
    app.start_save(false, None, false);
    let request = requests.try_recv().unwrap();
    assert_eq!(request.buffer, buffer);
    assert_eq!(request.text.to_string(), "renamed");
    assert!(app.state.buffers[buffer].dirty);
    request.reply.send(Ok(None)).unwrap();
    app.process_save_work();
    assert!(!app.state.buffers[buffer].dirty);
    assert!(app.state.buffers[buffer].path.is_none());
    app.process_effect(crate::app::commands::AppEffect::Quit { force: false });
    assert!(app.should_quit);
}

#[test]
fn failed_or_late_provider_write_keeps_unsaved_edits_and_never_writes_a_file() {
    let (_dirs, mut app) = app();
    let (save, requests) = step("directory");
    let buffer = app
        .open_editable_document("Files".into(), "directory".into(), "old".into(), save)
        .unwrap();
    edit(&mut app, buffer, "new");
    app.start_save(false, None, false);
    requests
        .try_recv()
        .unwrap()
        .reply
        .send(Err("write refused".into()))
        .unwrap();
    app.process_save_work();
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(app.ui.notification.as_deref(), Some("write refused"));
    app.start_save(false, None, false);
    let late = requests.try_recv().unwrap();
    edit(&mut app, buffer, "newer");
    late.reply.send(Ok(None)).unwrap();
    app.process_save_work();
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "newer");
    assert!(app.state.buffers[buffer].path.is_none());
}

#[test]
fn before_save_steps_change_text_in_order_and_can_cancel_the_provider_save() {
    let (_dirs, mut app) = app();
    let (first, a) = step("first");
    let (second, b) = step("second");
    let (save, writes) = step("directory");
    app.documents
        .set_before_save("directory".into(), vec![first, second]);
    let buffer = app
        .open_editable_document("Files".into(), "directory".into(), "old".into(), save)
        .unwrap();
    edit(&mut app, buffer, "new");
    app.start_save(false, None, false);
    assert!(b.try_recv().is_err());
    a.try_recv()
        .unwrap()
        .reply
        .send(Ok(Some("first edit".into())))
        .unwrap();
    app.process_save_work();
    let second = b.try_recv().unwrap();
    assert_eq!(second.text.to_string(), "first edit");
    second.reply.send(Err("validation failed".into())).unwrap();
    app.process_save_work();
    assert!(writes.try_recv().is_err());
    assert!(app.state.buffers[buffer].dirty);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "first edit");
}

#[test]
fn formatter_chain_passes_each_result_to_the_next_before_save_steps() {
    let (_dirs, mut app) = app();
    let (first, a) = step("formatter one");
    let (second, b) = step("formatter two");
    let (before, checks) = step("validate");
    let (save, writes) = step("directory");
    app.documents.set_formatters(
        "directory".into(),
        vec![Formatter::Text(first), Formatter::Text(second)],
    );
    app.documents
        .set_before_save("directory".into(), vec![before]);
    let buffer = app
        .open_editable_document("Files".into(), "directory".into(), "old".into(), save)
        .unwrap();
    edit(&mut app, buffer, "new");
    // Invoke the same entry point as file.save with format-on-save enabled.
    assert!(app.configuration.settings().lsp.format_on_save);
    assert!(app.begin_format_on_save(false));
    a.try_recv()
        .unwrap()
        .reply
        .send(Ok(Some("one".into())))
        .unwrap();
    app.process_save_work();
    let second = b.try_recv().unwrap();
    assert_eq!(second.text.to_string(), "one");
    second.reply.send(Ok(Some("two".into()))).unwrap();
    app.process_save_work();
    let check = checks.try_recv().unwrap();
    assert_eq!(check.text.to_string(), "two");
    check.reply.send(Ok(None)).unwrap();
    app.process_save_work();
    let write = writes.try_recv().unwrap();
    assert_eq!(write.text.to_string(), "two");
    write.reply.send(Ok(None)).unwrap();
    app.process_save_work();
    assert!(!app.state.buffers[buffer].dirty);
    assert!(app.format_on_save.is_empty());
}

#[test]
fn before_save_updates_an_ordinary_file_only_after_all_steps_succeed() {
    let (_dirs, mut app) = app();
    let path = app.state.workspace_root.join("file.rs");
    std::fs::write(&path, "original").unwrap();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("original"), Some(path.clone()))
        .unwrap();
    let (validate, checks) = step("validate");
    app.documents.set_before_save("rust".into(), vec![validate]);
    edit(&mut app, buffer, "edited");
    app.start_save(false, None, false);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    checks
        .try_recv()
        .unwrap()
        .reply
        .send(Ok(Some("checked".into())))
        .unwrap();
    app.process_save_work();
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.state.buffers[buffer].dirty {
        app.process_vfs_results();
        assert!(Instant::now() < deadline, "save did not finish");
        std::thread::yield_now();
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "checked");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "checked");
}

#[test]
fn manual_format_uses_the_same_chain_without_saving() {
    let (_dirs, mut app) = app();
    let (format, requests) = step("format");
    let (save, writes) = step("directory");
    let buffer = app
        .open_editable_document("Files".into(), "directory".into(), "old".into(), save)
        .unwrap();
    app.documents
        .set_formatters("directory".into(), vec![Formatter::Text(format)]);
    app.request_lsp(crate::language::lsp::LspFeature::Formatting {
        tab_size: 4,
        insert_spaces: true,
    });
    requests
        .try_recv()
        .unwrap()
        .reply
        .send(Ok(Some("formatted".into())))
        .unwrap();
    app.process_save_work();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "formatted");
    assert!(app.state.buffers[buffer].dirty);
    assert!(writes.try_recv().is_err());
    assert!(app.format_on_save.is_empty());
}

#[test]
fn timeout_cancels_before_save_and_save_all_includes_editable_documents() {
    let (_dirs, mut app) = app();
    let (save, writes) = step("directory");
    let buffer = app
        .open_editable_document("Files".into(), "directory".into(), "old".into(), save)
        .unwrap();
    edit(&mut app, buffer, "new");
    app.save_all(false);
    let request = writes.try_recv().unwrap();
    app.documents
        .save_work
        .pending
        .get_mut(&buffer)
        .unwrap()
        .deadline = Instant::now();
    app.process_save_work();
    assert!(request.reply.send(Ok(None)).is_err());
    assert!(app.state.buffers[buffer].dirty);
}
