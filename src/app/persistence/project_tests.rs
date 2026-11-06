use crate::app::tests::*;

#[test]
pub(in crate::app) fn help_restores_the_visible_palette_and_keeps_file_search_alive() {
    let (_app_dirs, mut app) = app();
    app.open_builtin_picker(crate::app::commands::PickerKind::Commands);
    app.picker_insert("file");
    let before = app.ui.picker.as_ref().unwrap().query.clone();
    app.open_contextual_help();
    app.close_picker();
    assert_eq!(app.ui.picker.as_ref().unwrap().query, before);
    let path = app.state.workspace_root.join("surviving-search.txt");
    fs::write(&path, "found after help\n").unwrap();
    app.start_workspace_file_picker();
    app.open_contextual_help();
    app.process_workspace_file_events();
    assert!(app.search.help_is_open());
    app.close_picker();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.search.files_pending() {
        app.process_workspace_file_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    app.picker_insert("surviving-search");
    let picker = app.ui.picker.as_ref().expect("restored file picker");
    assert_eq!(picker.title, "Files");
    assert!(
        picker
            .rows
            .iter()
            .any(|row| row.label == "surviving-search.txt")
    );
    app.reconcile_editor_windows();
    app.accept_picker();
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("opened search result");
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "found after help\n"
    );
}
use std::fs;
use std::thread;
use std::time::Duration;
use std::time::Instant;
