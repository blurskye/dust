use crate::app::tests::*;




#[test]
pub(in crate::app) fn remote_files_outside_the_workspace_open_writable_at_the_requested_line() {
    let (_app_dirs, mut app) = app();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("note.txt");
    fs::write(&path, "one\ntwo\nthree\n").unwrap();
    let server = crate::remote::RemoteServer::start(directory.path()).unwrap();
    crate::remote::send(server.path(), std::slice::from_ref(&path), Some(2)).unwrap();
    app.remote = Some(server);
    for _ in 0..200 {
        app.process_remote_opens();
        app.process_vfs_results();
        if app
            .state
            .active_buffer_id()
            .is_some_and(|buffer| app.state.buffers[buffer].path.as_ref() == Some(&path))
        {
            break;
        }
        thread::sleep(Duration::from_millis(5));
    }
    let view = &app.state.views[app.state.active_view.unwrap()];
    assert_eq!(app.state.buffers[view.buffer].path.as_ref(), Some(&path));
    assert!(!app.state.buffers[view.buffer].read_only);
    assert_eq!(view.cursor.get(), 4);
}