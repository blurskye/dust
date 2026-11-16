//! Dusty Git inside the editor: the editor's own entry point opens it, `e` on a reader line
//! opens that line in an editor buffer, the same command brings the screen back where it was,
//! and Esc returns to the editor with its buffer and cursor untouched.

use crate::app::App;
use crate::app::tests::{app, wait_vfs};
use crate::ui::{KeyInputKind, UiEvent};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn key(app: &mut App, keys: &str) {
    for key in keys.split_whitespace() {
        app.handle_event(UiEvent::Key {
            key: key.parse().unwrap(),
            kind: KeyInputKind::Press,
        });
    }
}

fn screen_text(app: &mut App) -> String {
    let frame = app
        .git_screen
        .as_mut()
        .expect("Dusty Git is open")
        .frame(120, 36);
    (0..36)
        .map(|y| frame.grid.row_text(y))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Poll until the screen shows `text` and its reads have settled (a resumed screen
/// refreshes, and its reader reloads, in the background).
fn wait_screen(app: &mut App, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut changed = Instant::now();
    loop {
        if app.git_screen.as_mut().expect("Dusty Git is open").poll() {
            changed = Instant::now();
        }
        let shown = screen_text(app);
        if shown.contains(text) && changed.elapsed() > Duration::from_secs(1) {
            return;
        }
        assert!(Instant::now() < deadline, "missing {text}:\n{shown}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The active buffer's path and the cursor's 0-based line.
fn cursor(app: &App) -> (PathBuf, usize) {
    let view = &app.state.views[app.state.active_view().unwrap()];
    let buffer = &app.state.buffers[view.buffer];
    (
        buffer.path.clone().expect("a file buffer"),
        buffer.text.byte_to_line(view.cursor).unwrap(),
    )
}

/// The whole composed terminal: the editor with the Git window drawn over it.
fn composed(app: &mut App) -> crate::ui::Frame {
    app.reconcile_editor_windows();
    let mut frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        app.window_session.width(),
        app.window_session.height(),
    );
    if app.git_screen_visible {
        let config = app.configuration.settings().ui.git.clone();
        let (width, height) = (app.window_session.width(), app.window_session.height());
        let (rect, git) = app
            .git_screen
            .as_mut()
            .unwrap()
            .window_frame(width, height, &config);
        crate::git::screen::GitScreen::compose(&mut frame, rect, &git);
    }
    frame
}

fn rows(frame: &crate::ui::Frame) -> Vec<String> {
    (0..frame.grid.height())
        .map(|y| frame.grid.row_text(y))
        .collect()
}

/// Whether row `y` has a cell drawn in a live style: anything but the receded border colour.
fn live(frame: &crate::ui::Frame, y: u16) -> bool {
    (0..frame.grid.width()).any(|x| {
        frame.grid.get(x, y).is_some_and(|cell| {
            !cell.symbol.trim().is_empty()
                && !matches!(
                    cell.style,
                    crate::ui::Style::Git {
                        role: crate::ui::GitRole::Border,
                        ..
                    }
                )
        })
    })
}

fn repository(root: &Path) {
    let repo = git2::Repository::init(root).unwrap();
    let original = (1..=60)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    fs::write(root.join("a.txt"), &original).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Embedded", "embedded@example.invalid").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "base", &tree, &[])
        .unwrap();
    fs::write(
        root.join("a.txt"),
        original.replace("line 30\n", "changed 30\n"),
    )
    .unwrap();
}

#[test]
fn git_tab_e_opens_the_line_in_a_buffer_and_esc_returns_to_the_editor_where_it_was() {
    let (_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    repository(&root);
    app.handle_event(UiEvent::Resize(120, 36));
    // The editor has a buffer of its own, its cursor on line 10.
    app.edit_path_at(root.join("a.txt"), 10);
    wait_vfs(&mut app);
    assert_eq!(cursor(&app), (root.join("a.txt"), 9));

    // The editor's own binding opens Dusty Git over the editor.
    key(&mut app, "space g g");
    assert!(app.git_screen_visible, "{:?}", app.ui.notification);
    wait_screen(&mut app, "changed 30");
    let frame = composed(&mut app);
    let shown = rows(&frame);
    assert!(
        shown.iter().any(|row| row.contains("changed 30")),
        "{shown:#?}"
    );
    // The float leaves the editor around it, receded: the editor's status line (the last
    // row) keeps its text but not its colours, so Dusty Git's footer is the only live one.
    let bottom = frame.grid.height() - 1;
    assert!(shown[usize::from(bottom)].contains("NORMAL"), "{shown:#?}");
    assert!(!live(&frame, bottom), "{shown:#?}");

    // `0` focuses the reader. Its first code line is the hunk's first context line (27);
    // the deleted line opens at the line that took its place (30).
    key(&mut app, "0 e");
    assert!(!app.git_screen_visible);
    assert!(
        app.git_screen.is_some(),
        "Dusty Git waits behind the buffer"
    );
    wait_vfs(&mut app);
    assert_eq!(cursor(&app), (root.join("a.txt"), 26));

    key(&mut app, "space g g");
    assert!(app.git_screen_visible);
    wait_screen(&mut app, "changed 30");
    key(&mut app, "j j j e");
    wait_vfs(&mut app);
    assert_eq!(cursor(&app), (root.join("a.txt"), 29));

    // Esc steps out of the reader, then closes the screen; the buffer and cursor stay.
    key(&mut app, "space g g");
    assert!(app.git_screen_visible);
    wait_screen(&mut app, "changed 30");
    key(&mut app, "esc esc");
    assert!(app.git_screen.is_none() && !app.git_screen_visible);
    assert_eq!(cursor(&app), (root.join("a.txt"), 29));
    let frame = composed(&mut app);
    let shown = rows(&frame);
    assert!(
        shown
            .iter()
            .any(|row| row.contains("line 30") || row.contains("changed 30")),
        "{shown:#?}"
    );
    // Closed, the editor's status line is live again.
    assert!(live(&frame, frame.grid.height() - 1), "{shown:#?}");
}
