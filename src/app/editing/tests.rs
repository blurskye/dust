//! Editing behavior and application integration tests.

use crate::app::App;
use crate::app::AppEffect;
use crate::app::configuration::compile_keymaps;
use crate::app::startup::StartupTarget;
use crate::app::tests::*;
use crate::editor::CommandValue;
use crate::editor::InvocationSource;
use crate::editor::input::KeyEvent;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::TextBuffer;
use crate::ui::UiEvent;
use crate::ui::UiFold;
use crate::ui::render;
use crate::workspace::DiagnosticSeverity;
use std::collections::BTreeMap;
use std::fs;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::tempdir;

#[test]
fn first_frame_renders_before_startup_file_is_queued_and_vfs_then_opens_it() {
    let root = tempdir().unwrap();
    let state = tempdir().unwrap();
    let path = root.path().join("startup.txt");
    fs::write(&path, b"startup\n").expect("startup file");
    let mut app = App::new(
        StartupTarget::File {
            workspace_root: root.path().into(),
            path: path.clone(),
            additional_files: Vec::new(),
        },
        crate::config::load_bundled_config(),
        false,
        state.path().into(),
        false,
        false,
        std::sync::Arc::new(crate::clock::SystemClock),
    )
    .unwrap();
    app.window_session.observe_size((80, 24));
    assert!(app.documents.has_startup_work());
    assert!(!app.ui.dashboard);
    // This is the same draw/request boundary used by run_interactive.
    let painted = app
        .draw_before_startup_reads(|app| {
            assert_eq!(app.documents.pending_count(), 0);
            assert!(app.state.buffers.is_empty());
            let frame = crate::ui::render(
                &app.state,
                &app.ui,
                app.configuration.settings(),
                app.configuration.dashboard(),
                80,
                24,
            );
            assert_eq!(frame.grid.width(), 80);
            Ok(true)
        })
        .unwrap();
    assert!(painted);
    assert_eq!(app.documents.startup_pending(), 1);
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("startup buffer");
    assert_eq!(
        app.state.buffers[buffer].path.as_deref(),
        Some(path.as_path())
    );
    assert_eq!(app.state.buffers[buffer].text.to_string(), "startup\n");
    assert!(!app.documents.has_startup_work());
    assert_eq!(app.documents.startup_pending(), 0);
}

#[test]
fn deferred_startup_additional_files_restore_the_primary_buffer() {
    let (_app_dirs, mut app) = app();
    let primary = app.state.workspace_root.join("primary.txt");
    let additional = app.state.workspace_root.join("additional.txt");
    fs::write(&primary, b"primary").expect("primary");
    fs::write(&additional, b"additional").expect("additional");
    app.documents.enqueue_startup_fixture(primary.clone(), true);
    app.documents
        .enqueue_startup_fixture(additional.clone(), false);
    app.queue_startup_reads();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers.len(), 2);
    let active = app.state.active_buffer_id().expect("active primary");
    assert_eq!(
        app.state.buffers[active].path.as_deref(),
        Some(primary.as_path())
    );
    assert!(
        app.state
            .buffers
            .iter()
            .any(|(_, buffer)| { buffer.path.as_deref() == Some(additional.as_path()) })
    );
    app.state.validate_layout().expect("valid startup layout");
}

#[test]
fn marks_follow_ordinary_edits_before_any_window_transition() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("marks.txt");
    fs::write(&path, "abc target").unwrap();
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("abc target"), Some(path))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.views[view].cursor = ByteOffset(4);
    app.process_effect(AppEffect::MarkSet("a".into()));
    app.state.views[view].cursor = ByteOffset(0);
    for key in ["i", "X", "escape"] {
        app.handle_configured_key(key.parse().unwrap());
    }
    assert_eq!(app.state.buffers[buffer].text.to_string(), "Xabc target");
    assert_eq!(app.persistence.marks()[0].location.byte, 5);
    app.process_effect(AppEffect::MarkJump("a".into()));
    wait_vfs(&mut app);
    assert_eq!(app.state.views[view].cursor, ByteOffset(5));
}

#[test]
fn path_completion_is_bounded_async_and_rejects_stale_requests() {
    let (_app_dirs, mut app) = app();
    fs::write(app.state.workspace_root.join("alpha.txt"), b"a").expect("alpha");
    wait_vfs(&mut app);
    app.open_prompt(crate::ui::PromptKind::Command, "e a".into());
    assert_eq!(app.documents.pending_count(), 1);
    // Leave the old listing outstanding while invalidating its query.
    app.prompt_insert("z");
    wait_vfs(&mut app);
    assert!(
        app.ui
            .prompt
            .as_ref()
            .expect("prompt")
            .completions
            .is_empty()
    );
    app.open_prompt(crate::ui::PromptKind::Command, "e a".into());
    wait_vfs(&mut app);
    assert!(
        app.ui
            .prompt
            .as_ref()
            .expect("prompt")
            .completions
            .iter()
            .any(|item| item.label == "alpha.txt")
    );
}

#[test]
fn path_completion_limits_matching_entries() {
    use crate::app::input::COMPLETION_LIMIT;
    let (_app_dirs, mut app) = app();
    for index in 0..COMPLETION_LIMIT + 10 {
        fs::write(
            app.state
                .workspace_root
                .join(format!("match-{index:04}.txt")),
            b"",
        )
        .expect("completion entry");
    }
    app.open_prompt(crate::ui::PromptKind::Command, "e match-".into());
    wait_vfs(&mut app);
    let completions = &app.ui.prompt.as_ref().expect("prompt").completions;
    assert_eq!(completions.len(), COMPLETION_LIMIT);
    assert!(
        completions
            .iter()
            .all(|item| item.label.starts_with("match-"))
    );
}

#[test]
fn path_completion_keeps_open_buffers_and_lists_directory_children() {
    let (_app_dirs, mut app) = app();
    let directory = app.state.workspace_root.join("src");
    fs::create_dir(&directory).expect("src");
    let path = directory.join("needle.rs");
    fs::write(&path, b"").expect("needle");
    app.state
        .open_text(TextBuffer::from_text(""), Some(path))
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_prompt(crate::ui::PromptKind::Command, "e needle".into());
    wait_vfs(&mut app);
    let completions = &app.ui.prompt.as_ref().expect("prompt").completions;
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].label, "src/needle.rs");

    app.open_prompt(crate::ui::PromptKind::Command, "e src/".into());
    wait_vfs(&mut app);
    let completions = &app.ui.prompt.as_ref().expect("prompt").completions;
    assert_eq!(
        completions.len(),
        1,
        "buffer and directory results are deduplicated"
    );
    assert_eq!(completions[0].label, "src/needle.rs");
    fs::write(directory.join("other.rs"), b"").expect("other");
    app.prompt_insert("o");
    wait_vfs(&mut app);
    let completions = &app.ui.prompt.as_ref().expect("prompt").completions;
    assert_eq!(completions.len(), 1);
    assert_eq!(completions[0].label, "src/other.rs");
}

#[test]
fn typing_a_path_keeps_one_listing_in_flight_and_completes_the_latest_input() {
    let (_app_dirs, mut app) = app();
    fs::write(app.state.workspace_root.join("alpha.txt"), b"a").expect("alpha");
    fs::write(app.state.workspace_root.join("beta.txt"), b"b").expect("beta");
    wait_vfs(&mut app);
    app.open_prompt(crate::ui::PromptKind::Command, "e ".into());
    for character in ["a", "l", "p", "h", "a"] {
        app.prompt_insert(character);
    }
    assert!(
        app.documents.pending_count() <= 1,
        "keystrokes must not queue one directory listing each"
    );
    wait_vfs(&mut app);
    let completions = &app.ui.prompt.as_ref().expect("prompt").completions;
    assert!(completions.iter().any(|item| item.label == "alpha.txt"));
    assert!(!completions.iter().any(|item| item.label == "beta.txt"));
}

#[test]
fn command_completion_accepts_registry_items_and_preserves_invalid_input() {
    let (_app_dirs, mut app) = app();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture
            .settings
            .command_labels
            .insert("health.open".into(), "Vitals".into());
        app.configuration.install_fixture(fixture);
    }
    app.open_prompt(crate::ui::PromptKind::Command, "hea".into());
    let index = app
        .ui
        .prompt
        .as_ref()
        .expect("prompt")
        .completions
        .iter()
        .position(|item| item.label == "health" && item.detail == "Vitals")
        .expect("health completion");
    for _ in 0..app.ui.prompt.as_ref().expect("prompt").completions.len() {
        if app.ui.prompt.as_ref().expect("prompt").selected_completion == Some(index) {
            break;
        }
        app.prompt_complete(1);
    }
    assert_eq!(
        app.ui.prompt.as_ref().expect("prompt").selected_completion,
        Some(index)
    );
    // Enter on a chosen command runs it.
    app.submit_prompt();
    assert!(app.ui.prompt.is_none());
    assert!(!app.ui.notification_error);
    assert!(
        app.ui.notification.as_deref().is_some_and(|message| {
            message.starts_with("Dust health —") && message.contains("terminal test")
        }),
        "health command result: {:?}",
        app.ui.notification
    );
    app.open_prompt(crate::ui::PromptKind::Command, "health-not-real".into());
    app.submit_prompt();
    let prompt = app.ui.prompt.as_ref().expect("invalid prompt stays open");
    assert_eq!(prompt.input, "health-not-real");
    assert!(prompt.error.is_some());
}

#[test]
fn dot_repeats_semantic_insert_session() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("one"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in ['i', 'X'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.handle_configured_key(KeyEvent::character('.'));
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "XXone");
    assert_eq!(app.input.repeat_plan(3, None).unwrap().len(), 3);
}

#[test]
fn a_yank_lights_up_the_yanked_text_for_a_moment() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(
            crate::editor::text::TextBuffer::from_text("one\ntwo\n"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    press_keys(&mut app, &["y", "y"]);
    let yanked = app.ui.yanked.clone().expect("the line lights up");
    assert_eq!(
        yanked.ranges,
        vec![crate::editor::text::ByteRange::new(ByteOffset(0), ByteOffset(4)).unwrap()]
    );
    app.handle_event(UiEvent::Resize(40, 6));
    app.reconcile_editor_windows();
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        40,
        6,
    );
    let lit = (0..6)
        .flat_map(|y| (0..40).map(move |x| (x, y)))
        .filter(|(x, y)| frame.grid.get(*x, *y).unwrap().style == crate::ui::Style::Search)
        .count();
    assert_eq!(lit, 3, "the three letters of `one`");
    app.update_yank_highlight();
    assert!(app.ui.yanked.is_some(), "it stays for a moment");
    std::thread::sleep(
        yanked
            .until
            .saturating_duration_since(std::time::Instant::now()),
    );
    app.update_yank_highlight();
    assert!(app.ui.yanked.is_none());
}

#[test]
fn counted_dot_overrides_the_recorded_count_and_keeps_it_for_later_repeats() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("abcdefghij"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in "2x3.".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert_eq!(app.state.buffers[buffer].text.to_string(), "fghij");
    app.handle_configured_key(KeyEvent::character('.'));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "ij");
    for key in "1.".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert_eq!(app.state.buffers[buffer].text.to_string(), "j");
}

#[test]
fn counted_dot_repeats_inserted_text_and_open_lines() {
    for (key, original, expected) in [
        ('i', "ABone", "ABABABABone"),
        ('a', "oABne", "oABABABABne"),
        ('o', "one\nAB", "one\nAB\nAB\nAB\nAB"),
        ('O', "AB\none", "AB\nAB\nAB\nAB\none"),
    ] {
        let (_app_dirs, mut app) = app();
        let (buffer, view) = app
            .state
            .open_text(TextBuffer::from_text("one"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        for key in [key, 'A', 'B'] {
            app.handle_configured_key(KeyEvent::character(key));
        }
        app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
        assert_eq!(app.state.buffers[buffer].text.to_string(), original);
        if key == 'i' || key == 'a' {
            app.state.views[view].cursor = ByteOffset(0);
        }
        for key in "3.".chars() {
            app.handle_configured_key(KeyEvent::character(key));
        }
        assert_eq!(
            app.state.buffers[buffer].text.to_string(),
            expected,
            "{key}"
        );
    }
}

#[test]
fn counted_search_commands_move_to_the_requested_match() {
    for (keys, cursor, expected, selected) in [
        ("3n", 0, 12, false),
        ("3N", 20, 8, false),
        ("3*", 0, 12, false),
        ("3#", 20, 8, false),
        ("3gn", 0, 10, true),
    ] {
        let (_app_dirs, mut app) = app();
        let (_, view) = app
            .state
            .open_text(TextBuffer::from_text("foo foo foo foo foo foo"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.ui.search_query = Some("foo".into());
        app.state.views[view].cursor = ByteOffset(cursor);
        for key in keys.chars() {
            app.handle_configured_key(KeyEvent::character(key));
        }
        assert_eq!(app.state.views[view].cursor, ByteOffset(expected), "{keys}");
        assert_eq!(
            app.state.views[view].selection.is_some(),
            selected,
            "{keys}"
        );
    }
}

#[test]
fn counted_diagnostic_commands_move_the_requested_number_of_problems() {
    use crate::app::diagnostics::DiagnosticBatch;
    use crate::app::diagnostics::DiagnosticDocument;
    use crate::app::diagnostics::DiagnosticValidity;
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("problems.rs");
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("a\nb\nc\nd\ne\n"), Some(path.clone()))
        .unwrap();
    app.set_dashboard_visibility(false);
    let producer = crate::workspace::DiagnosticProducer::new(
        crate::workspace::DiagnosticOwner::new("fixture").unwrap(),
    );
    app.diagnostics.activate(producer.clone());
    let uri = crate::language::lsp::file_uri(&path);
    let items = [2, 4, 6, 8]
        .into_iter()
        .map(|start| crate::workspace::Diagnostic {
            uri: uri.clone(),
            version: None,
            range: ByteRange::new(ByteOffset(start), ByteOffset(start + 1)).unwrap(),
            severity: DiagnosticSeverity::Error,
            source: "fixture".into(),
            code: None,
            message: "problem".into(),
            related: Vec::new(),
            tags: Vec::new(),
            fixes: Vec::new(),
            data: None,
            timestamp: std::time::SystemTime::UNIX_EPOCH,
        })
        .collect();
    app.diagnostics
        .publish(
            DiagnosticBatch {
                producer,
                uri,
                document: DiagnosticDocument::capture(&app.state, buffer),
                validity: DiagnosticValidity::RequestedSnapshot,
                items,
            },
            &app.state,
        )
        .unwrap();
    for key in "3]d".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert_eq!(app.state.views[view].cursor, ByteOffset(6));
    for key in "2[d".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert_eq!(app.state.views[view].cursor, ByteOffset(2));
    for key in "1000000]d".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert_eq!(app.state.views[view].cursor, ByteOffset(2));
}

#[test]
fn counted_buffer_tab_and_pane_commands_keep_their_counts() {
    let (_app_dirs, mut app) = app();
    let buffers = (0..5)
        .map(|_| {
            app.state
                .open_text(TextBuffer::from_text("text"), None)
                .unwrap()
                .0
        })
        .collect::<Vec<_>>();
    app.state.show_buffer(buffers[0]).unwrap();
    app.set_dashboard_visibility(false);
    for key in "3]b".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert_eq!(app.state.active_buffer_id(), Some(buffers[3]));
    for _ in 0..4 {
        app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::NewTab));
    }
    let tabs = app
        .state
        .editor_tabs()
        .map(|tab| tab.id)
        .collect::<Vec<_>>();
    for key in "3gt".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert_eq!(app.state.active_tab(), Some(tabs[2]));
    for key in "2gT".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert_eq!(app.state.active_tab(), Some(tabs[0]));
    app.handle_configured_key(KeyEvent::character('g'));
    app.handle_configured_key(KeyEvent::character('t'));
    assert_eq!(app.state.active_tab(), Some(tabs[1]));
    for key in "1gt".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    assert_eq!(app.state.active_tab(), Some(tabs[0]));
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    )));
    let mut invocation =
        crate::editor::CommandInvocation::new("pane.resize.grow", InvocationSource::Test).unwrap();
    invocation.count = std::num::NonZeroU64::new(3);
    app.invoke(invocation);
    let crate::windows::TiledLayoutNode::Split { ratio, .. } =
        &app.state.tab_layout(tabs[0]).unwrap()
    else {
        panic!("split")
    };
    assert_eq!(*ratio, 350);
}

#[test]
fn empty_insert_sessions_preserve_the_previous_dot_change() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in ["x", "i", "escape", "."] {
        app.handle_configured_key(key.parse().unwrap());
    }
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "c");
}

#[test]
fn insert_cursor_motion_splits_dot_recording() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("one"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in ["i", "a", "left", "b", "escape", "."] {
        app.handle_configured_key(key.parse().unwrap());
    }
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "bbaone");
    let view = app.state.active_view().unwrap();
    assert_eq!(app.state.views[view].cursor, ByteOffset(0));
    assert_eq!(
        app.state.views[view].mode,
        crate::editor::EditorMode::Normal
    );
    assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
}

#[test]
fn normalized_multiline_character_repeat_preserves_lines_and_end_column() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(
            crate::editor::text::TextBuffer::from_text("ab\ncd\n0123456789\nuvwxyz"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in ["l", "v", "j", "h", "d", "j", "."] {
        app.handle_configured_key(key.parse().unwrap());
    }
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "ad\n0vwxyz");
}

#[test]
fn multiline_visual_repeat_at_document_end_deletes_the_remaining_text() {
    for terminated in [false, true] {
        let (_app_dirs, mut app) = app();
        let source = format!("ab\ncd\n0123456789{}", if terminated { "\n" } else { "" });
        app.state
            .open_text(crate::editor::text::TextBuffer::from_text(&source), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        for key in ["l", "v", "j", "h", "d", "G", "$", "."] {
            app.handle_configured_key(key.parse().unwrap());
        }
        let buffer = app.state.active_buffer_id().unwrap();
        assert_eq!(app.state.buffers[buffer].text.to_string(), "ad\n012345678");
        assert!(!app.ui.notification_error, "{:?}", app.ui.notification);
    }
}

#[test]
fn normalized_visual_comment_and_surround_keep_operator_arguments() {
    for (operator, argument, value, expected) in [
        ("surround_add", "character", ")", "(ab) (cd)"),
        ("comment", "comment", "//", "// ab\n// cd"),
    ] {
        let (_app_dirs, mut app) = app();
        let source = if operator == "comment" {
            "ab\ncd"
        } else {
            "ab cd"
        };
        app.state
            .open_text(crate::editor::text::TextBuffer::from_text(source), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        for key in ["v", "l"] {
            app.handle_configured_key(key.parse().unwrap());
        }
        app.invoke(
            crate::editor::CommandInvocation::new("edit.operator", InvocationSource::Test)
                .unwrap()
                .with_argument("operator", CommandValue::String(operator.into()))
                .with_argument("motion", CommandValue::String("visual".into()))
                .with_argument("linewise", CommandValue::Bool(false))
                .with_argument(argument, CommandValue::String(value.into())),
        );
        let view = app.state.active_view().unwrap();
        app.state.views[view].cursor = ByteOffset(if operator == "comment" { 6 } else { 5 });
        app.state.views[view].selection = None;
        app.state.views[view].mode = crate::editor::EditorMode::Normal;
        app.handle_configured_key(".".parse().unwrap());
        let buffer = app.state.active_buffer_id().unwrap();
        assert_eq!(app.state.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn normalized_visual_indent_repeats_the_level_count() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("a\nb"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in ["V", "3", ">", "j", "."] {
        app.handle_configured_key(key.parse().unwrap());
    }
    let buffer = app.state.active_buffer_id().unwrap();
    let indent = " ".repeat(app.state.policy.indent_width * 3);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        format!("{indent}a\n{indent}b")
    );
}

#[test]
fn counted_insert_and_dot_preserve_the_insertion_count() {
    let (_app_dirs, mut app) = app();
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("z"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    press_keys(&mut app, &["3", "i", "a", "b", "c", "escape"]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "abcabcabcz");
    assert_eq!(app.state.views[view].cursor, ByteOffset(8));
    press_keys(&mut app, &["$", "."]);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "abcabcabcabcabcabcz"
    );
    press_keys(&mut app, &["u"]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "abcabcabcz");
}

#[test]
fn visual_comment_and_surround_dot_retain_operator_arguments() {
    let (_comment_dirs, mut comment) = app();
    let path = comment.state.workspace_root.join("repeat.rs");
    let (buffer, _) = comment
        .state
        .open_text(TextBuffer::from_text("a\nb\n"), Some(path))
        .unwrap();
    comment.set_dashboard_visibility(false);
    press_keys(&mut comment, &["V", "g", "c", "j", "."]);
    assert_eq!(
        comment.state.buffers[buffer].text.to_string(),
        "// a\n// b\n"
    );

    let (_surround_dirs, mut surround) = app();
    let (buffer, _) = surround
        .state
        .open_text(TextBuffer::from_text("ab\ncd\n"), None)
        .unwrap();
    surround.set_dashboard_visibility(false);
    press_keys(
        &mut surround,
        &["v", "l", "g", "s", "a", ")", "j", "0", "."],
    );
    assert_eq!(
        surround.state.buffers[buffer].text.to_string(),
        "(ab)\n(cd)\n"
    );
}

#[test]
fn visual_indent_dot_retains_the_number_of_levels() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("a\nb\n"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    press_keys(&mut app, &["V", "2", ">", "j", "."]);
    let level = if app.state.policy.expand_tabs {
        " ".repeat(app.state.policy.indent_width)
    } else {
        "\t".into()
    };
    let indent = level.repeat(2);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        format!("{indent}a\n{indent}b\n")
    );
}

#[test]
fn normalized_characterwise_and_linewise_changes_repeat_without_selection() {
    let (_character_dirs, mut character) = app();
    character
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("abcdef"), None)
        .unwrap();
    character.set_dashboard_visibility(false);
    for key in ['v', 'l', 'd', 'l', '.'] {
        character.handle_configured_key(KeyEvent::character(key));
    }
    let buffer = character
        .state
        .active_buffer_id()
        .expect("character buffer");
    assert_eq!(character.state.buffers[buffer].text.to_string(), "cf");

    let (_line_dirs, mut line) = app();
    line.state
        .open_text(
            crate::editor::text::TextBuffer::from_text("a\nb\nc\nd\n"),
            None,
        )
        .unwrap();
    line.set_dashboard_visibility(false);
    for key in ['V', 'j', 'd', '.'] {
        line.handle_configured_key(KeyEvent::character(key));
    }
    let buffer = line.state.active_buffer_id().expect("line buffer");
    assert_eq!(line.state.buffers[buffer].text.to_string(), "");
}

#[test]
fn normalized_multiline_character_repeat_keeps_its_last_line_extent() {
    for reverse in [false, true] {
        let (_app_dirs, mut app) = app();
        let (buffer, view) = app
            .state
            .open_text(
                crate::editor::text::TextBuffer::from_text(
                    "abc\ne\u{301}fg\n0123456789\ne\u{301}xyz\n",
                ),
                None,
            )
            .unwrap();
        app.set_dashboard_visibility(false);
        let (anchor, head) = if reverse { (4, 1) } else { (1, 4) };
        app.state.views[view].cursor = ByteOffset(head);
        app.state.views[view].mode = crate::editor::EditorMode::VisualChar;
        app.state.views[view].selection = Some(crate::editor::Selection {
            anchor: ByteOffset(anchor),
            head: ByteOffset(head),
            kind: crate::editor::SelectionKind::Character,
        });
        app.handle_configured_key(KeyEvent::character('d'));
        assert_eq!(
            app.state.buffers[buffer].text.to_string(),
            "afg\n0123456789\ne\u{301}xyz\n"
        );
        app.state.views[view].cursor = ByteOffset(4);
        app.handle_configured_key(KeyEvent::character('.'));
        assert_eq!(app.state.buffers[buffer].text.to_string(), "afg\nxyz\n");
        app.state.views[view].cursor = ByteOffset(6);
        app.handle_configured_key(KeyEvent::character('.'));
        assert_eq!(app.state.buffers[buffer].text.to_string(), "afg\nxy");
    }
}

#[test]
fn normalized_block_repeat_does_not_insert_on_the_trailing_phantom_line() {
    let (_app_dirs, mut app) = app();
    let (buffer, view) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("ab\ncd\nef\n"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.handle_configured_key("ctrl-v".parse().unwrap());
    for key in ['j', 'c', 'Z'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.state.views[view].cursor = ByteOffset(6);
    app.handle_configured_key(KeyEvent::character('.'));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "Zb\nZd\nZf\n");
}

#[test]
fn normalized_blockwise_change_repeats_without_a_stale_selection() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(
            crate::editor::text::TextBuffer::from_text("abc\ndef\nghi\njkl"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.handle_configured_key("ctrl-v".parse().expect("block key"));
    app.handle_configured_key(KeyEvent::character('l'));
    app.handle_configured_key(KeyEvent::character('j'));
    app.handle_configured_key(KeyEvent::character('c'));
    app.handle_configured_key(KeyEvent::character('Z'));
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.handle_configured_key(KeyEvent::character('j'));
    app.handle_configured_key(KeyEvent::character('j'));
    app.handle_configured_key(KeyEvent::character('.'));
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "Zc\nZf\nZi\nZl");
}

#[test]
fn normalized_block_repeat_pads_new_ragged_and_empty_rows_by_display_column() {
    let (_app_dirs, mut app) = app();
    let (buffer, view) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("abcd\nx\npqrs\né---\n\n界--"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.views[view].cursor = ByteOffset(10);
    app.state.views[view].mode = crate::editor::EditorMode::VisualBlock;
    app.state.views[view].selection = Some(crate::editor::Selection {
        anchor: ByteOffset(2),
        head: ByteOffset(10),
        kind: crate::editor::SelectionKind::Block,
    });
    for key in [
        KeyEvent::character('c'),
        KeyEvent::character('Z'),
        KeyEvent::plain(crate::editor::input::KeyCode::Escape),
    ] {
        app.handle_configured_key(key);
    }
    // Repeat the two-column block at display column two, after é and one dash.
    let destination = app.state.buffers[buffer]
        .text
        .to_string()
        .find("é-")
        .unwrap()
        + "é-".len();
    app.state.views[view].cursor = ByteOffset(destination);
    app.state.views[view].selection = None;
    app.handle_configured_key(KeyEvent::character('.'));
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "abZ\nx Z\npqZ\né-Z\n  Z\n界Z"
    );
}

#[test]
fn replace_mode_session_is_semantically_repeatable() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.handle_configured_key(KeyEvent::character('R'));
    app.handle_configured_key(KeyEvent::character('X'));
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    app.handle_configured_key(KeyEvent::character('l'));
    app.handle_configured_key(KeyEvent::character('.'));
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "XXc");
}

#[test]
fn surround_find_keeps_the_target_separate_from_the_delimiter() {
    for (keys, expected) in [("gsafX)", "(abcX)def"), ("gsatX]", "[abc]Xdef")] {
        let (_app_dirs, mut app) = app();
        app.state
            .open_text(TextBuffer::from_text("abcXdef"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        for key in keys.chars() {
            app.handle_configured_key(KeyEvent::character(key));
        }
        let buffer = app.state.active_buffer_id().unwrap();
        assert_eq!(app.state.buffers[buffer].text.to_string(), expected);
    }
}

#[test]
fn command_line_macro_recording_is_stopped_by_the_normal_toggle() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("abcde"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.open_prompt(
        crate::ui::PromptKind::Command,
        "macro.record.start register=a".into(),
    );
    app.submit_prompt();
    for key in ['x', 'q', '@', 'a'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "cde");
}

#[test]
fn blockwise_visual_commands_record_and_replay_as_typed_macro_invocations() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(
            crate::editor::text::TextBuffer::from_text("abc\ndef\nghi\njkl"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    app.handle_configured_key(KeyEvent::character('q'));
    app.handle_configured_key(KeyEvent::character('a'));
    app.handle_configured_key("ctrl-v".parse().expect("block key"));
    for key in ['l', 'j', 'd', 'q', 'j', 'j', '@', 'a'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "c\nf\ni\nl");
}

#[test]
fn explicit_macro_commands_and_q_share_recording_state() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let start = crate::editor::CommandInvocation::new("macro.record.start", InvocationSource::Test)
        .unwrap()
        .with_argument("register", CommandValue::String("a".into()));
    app.invoke(start.clone());
    app.handle_configured_key(KeyEvent::character('x'));
    app.handle_configured_key(KeyEvent::character('q'));
    assert_eq!(app.state.registers.get_macro('a').unwrap().len(), 1);
    assert!(app.input.pending_context().is_none());
    app.invoke(start);
    app.invoke(
        crate::editor::CommandInvocation::new("macro.record.stop", InvocationSource::Test).unwrap(),
    );
    app.handle_configured_key(KeyEvent::character('q'));
    assert_eq!(
        app.input.pending_context(),
        Some(crate::editor::input::PendingInputContext::MacroRecord)
    );
    app.handle_configured_key(KeyEvent::character('b'));
    app.handle_configured_key(KeyEvent::character('q'));
    assert!(app.state.registers.get_macro('b').is_some());
}

#[test]
fn macros_record_command_line_edits_once() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcde"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in ["q", "a", ":"] {
        app.handle_configured_key(key.parse().unwrap());
    }
    for character in "edit.delete_char".chars() {
        app.handle_configured_key(KeyEvent::character(character));
    }
    for key in ["enter", "q", "@", "a"] {
        app.handle_configured_key(key.parse().unwrap());
    }
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "cde");
    let recorded = app.state.registers.get_macro('a').unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].id.as_str(), "edit.delete_char");
}

#[test]
fn macro_edits_replace_the_previous_dot_change() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in ["q", "a", "i", "X", "escape", "q", "x", "@", "a", "."] {
        app.handle_configured_key(key.parse().unwrap());
    }
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "XXabc");
}

#[test]
fn direct_macro_commands_keep_the_recording_grammar_in_sync() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcd"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in ["q", "a", "x"] {
        app.handle_configured_key(key.parse().unwrap());
    }
    app.invoke(
        crate::editor::CommandInvocation::new("macro.record.stop", InvocationSource::Test).unwrap(),
    );
    for key in ["q", "b", "x", "q"] {
        app.handle_configured_key(key.parse().unwrap());
    }
    assert_eq!(app.state.registers.get_macro('a').unwrap().len(), 1);
    assert_eq!(app.state.registers.get_macro('b').unwrap().len(), 1);
    app.invoke(
        crate::editor::CommandInvocation::new("macro.record.start", InvocationSource::Test)
            .unwrap()
            .with_argument("register", CommandValue::String("c".into())),
    );
    for key in ["x", "q"] {
        app.handle_configured_key(key.parse().unwrap());
    }
    assert_eq!(app.state.registers.get_macro('c').unwrap().len(), 1);
}

#[test]
fn macro_replay_honors_configured_counts() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("abcde"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in ['q', 'a', 'x', 'q'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.show_error("an unrelated earlier error");
    for key in ['3', '@', 'a'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    let buffer = app.state.active_buffer_id().expect("buffer");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "e");
}

#[test]
fn counted_macro_replay_stops_when_a_nested_macro_fails() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("abcdef"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    // b calls a, which does not exist. Neither b's deletion nor its next repetition may run.
    let replay = crate::editor::CommandInvocation::new("macro.replay", InvocationSource::Test)
        .unwrap()
        .with_argument("register", CommandValue::String("a".into()));
    let delete =
        crate::editor::CommandInvocation::new("edit.delete_char", InvocationSource::Test).unwrap();
    app.state
        .registers
        .set_macro('b', vec![delete.clone(), replay, delete]);
    assert!(!app.process_effect_checked(AppEffect::MacroReplay {
        register: 'b',
        count: 3
    }));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "bcdef");
    assert_eq!(app.input.replay_depth(), 0);
    assert!(app.ui.notification_error);
}

#[test]
fn key_hint_lifecycle_tracks_exact_prefix_delay_and_cancellation() {
    let (_app_dirs, mut app) = app();
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(
        std::time::UNIX_EPOCH,
        Instant::now(),
    ));
    app.clock = clock.clone();
    app.state
        .open_text(crate::editor::text::TextBuffer::from_text("hint"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.key_hints.delay_ms = 0;
        app.configuration.install_fixture(fixture);
    }
    app.handle_configured_key("space".parse().expect("leader"));
    assert_eq!(
        app.input
            .mapping_prefix()
            .map(|prefix| (prefix, app.input.mapping_deadline())),
        Some(("space", None))
    );
    app.update_key_hint_ui();
    assert_eq!(
        app.ui.key_hints.as_ref().map(|hints| hints.prefix.as_str()),
        Some("space")
    );
    app.flush_keymap_timeout();
    assert!(app.input.mapping_pending());
    assert!(app.ui.key_hints.is_some());
    app.handle_configured_key(KeyEvent::character('f'));
    assert_eq!(app.input.mapping_prefix(), Some("space f"));
    app.update_key_hint_ui();
    assert_eq!(
        app.ui.key_hints.as_ref().map(|hints| hints.prefix.as_str()),
        Some("space f")
    );
    app.handle_configured_key(KeyEvent::character('b'));
    app.update_key_hint_ui();
    assert!(!app.input.mapping_pending());
    assert!(app.ui.key_hints.is_none());

    app.close_picker();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.key_hints.delay_ms = 500;
        app.configuration.install_fixture(fixture);
    }
    app.handle_configured_key("space".parse().expect("leader"));
    app.update_key_hint_ui();
    assert!(app.ui.key_hints.is_none());
    clock.advance(Duration::from_millis(600));
    app.update_key_hint_ui();
    assert!(app.ui.key_hints.is_some());
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.key_hints.enabled = false;
        app.configuration.install_fixture(fixture);
    }
    app.update_key_hint_ui();
    assert!(app.ui.key_hints.is_none());
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.key_hints.enabled = true;
        app.configuration.install_fixture(fixture);
    }
    app.handle_configured_paste("x".into());
    assert!(!app.input.mapping_pending());
    assert!(app.ui.key_hints.is_none());

    app.handle_configured_key("space".parse().expect("leader"));
    app.open_prompt(crate::ui::PromptKind::WorkspaceTrust, String::new());
    assert!(!app.input.mapping_pending());
    assert!(app.ui.key_hints.is_none());
    app.close_prompt();
    app.handle_configured_key("space".parse().expect("leader"));
    app.handle_event(crate::ui::UiEvent::FocusLost);
    assert!(!app.input.mapping_pending());
    assert!(app.ui.key_hints.is_none());
}

#[test]
fn key_hint_timeout_reload_and_pass_through_never_leave_stale_actions() {
    let (_app_dirs, mut app) = app();
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(
        std::time::UNIX_EPOCH,
        Instant::now(),
    ));
    app.clock = clock.clone();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    loaded.entries.extend([
        crate::config::KeymapEntry {
            id: "user.test.ambiguous-exact".into(),
            context: Some("dashboard".into()),
            mode: String::new(),
            keys: Some("x".into()),
            command: Some("health.open".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
        },
        crate::config::KeymapEntry {
            id: "user.test.ambiguous-long".into(),
            context: Some("dashboard".into()),
            mode: String::new(),
            keys: Some("x y".into()),
            command: Some("file.new".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
        },
        crate::config::KeymapEntry {
            id: "user.test.hidden-pass-through".into(),
            context: Some("editor".into()),
            mode: "normal".into(),
            keys: Some("<leader> f z".into()),
            command: None,
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: true,
            desc: None,
        },
        crate::config::KeymapEntry {
            id: "user.test.lower-command-pass-through".into(),
            context: Some("editor".into()),
            mode: "normal".into(),
            keys: Some("<leader> f p".into()),
            command: None,
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: true,
            desc: None,
        },
    ]);
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    app.configuration.install_bindings(keymaps, false);
    assert!(
        app.configuration
            .keymaps()
            .hints_for_prefix(
                "editor.normal",
                "space f",
                &app.configuration.settings().command_labels,
            )
            .is_some_and(|projection| {
                !projection.rows.iter().any(|row| row.next_key == "z")
                    && projection.rows.iter().any(|row| {
                        row.next_key == "p"
                            && row.mappings.iter().any(|mapping| {
                                mapping.command.as_deref() == Some("workspace.root.list")
                            })
                    })
            })
    );

    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.ui.key_hints.delay_ms = 0;
        app.configuration.install_fixture(fixture);
    }
    app.handle_key_in_context("dashboard", KeyEvent::character('x'));
    app.update_key_hint_ui();
    assert!(app.input.mapping_pending());
    assert!(app.ui.key_hints.is_some());
    clock.advance(Duration::from_millis(
        app.configuration.settings().input.mapping_timeout_ms + 1,
    ));
    app.flush_keymap_timeout();
    assert!(!app.input.mapping_pending());
    assert!(app.ui.key_hints.is_none());

    app.handle_key_in_context("dashboard", KeyEvent::character('x'));
    assert!(app.input.mapping_pending());
    let mut candidate = crate::config::load_bundled_config();
    candidate.config.input.keymaps.leader = ",".into();
    app.apply_configuration_candidate(candidate, false)
        .expect("accepted configuration");
    assert!(!app.input.mapping_pending());
    assert!(app.ui.key_hints.is_none());
}

#[test]
fn an_unfinished_insert_mapping_types_its_keys() {
    for (snippet, context, mode) in [
        (false, "editor", "insert"),
        (true, "editor", "insert"),
        (true, "snippet", ""),
    ] {
        let (_app_dirs, mut app) = app();
        let clock = std::sync::Arc::new(crate::clock::ManualClock::new(
            std::time::UNIX_EPOCH,
            Instant::now(),
        ));
        app.clock = clock.clone();
        let mut loaded = crate::config::load_bundled_config().keymaps;
        loaded.entries.push(crate::config::KeymapEntry {
            id: "user.test.jk".into(),
            context: Some(context.into()),
            mode: mode.into(),
            keys: Some("j k".into()),
            command: Some("input.cancel".into()),
            arguments: BTreeMap::new(),
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
        assert!(diagnostic.is_none(), "{diagnostic:?}");
        app.configuration.install_bindings(keymaps, false);
        let (buffer, view) = app
            .state
            .open_text(TextBuffer::from_text(""), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        if snippet {
            app.input.install_snippet(
                crate::app::language::snippets::SnippetDocument {
                    editor: &app.state.window_version(),
                    buffer,
                    text: &app.state.buffers[buffer].text,
                    view: Some(view),
                },
                ByteOffset(0),
                crate::language::lsp::parse_snippet("${1:}$0", 1024)
                    .unwrap()
                    .tabstops,
            );
        }
        let text = |app: &App| {
            app.state.buffers[app.state.views[view].buffer]
                .text
                .to_string()
        };

        app.handle_configured_key(KeyEvent::character('i'));
        app.handle_configured_key(KeyEvent::character('j'));
        app.handle_configured_key(KeyEvent::character('a'));
        assert_eq!(text(&app), "ja");
        if snippet {
            assert_eq!(app.input.snippet_buffer(), Some(buffer));
        }

        app.handle_configured_key(KeyEvent::character('j'));
        app.update_key_hint_ui();
        assert!(app.input.mapping_pending());
        clock.advance(Duration::from_millis(
            app.configuration.settings().input.mapping_timeout_ms + 1,
        ));
        app.flush_keymap_timeout();
        assert_eq!(text(&app), "jaj");

        app.handle_configured_key(KeyEvent::character('j'));
        app.handle_configured_paste("foo".into());
        assert_eq!(text(&app), "jajjfoo");
        assert!(!app.input.mapping_pending());
        if snippet {
            assert_eq!(app.input.snippet_buffer(), Some(buffer));
        }

        app.handle_configured_key(KeyEvent::character('j'));
        app.handle_configured_key(KeyEvent::character('k'));
        assert_eq!(text(&app), "jajjfoo");
        assert_eq!(
            app.state.views[view].mode,
            crate::editor::EditorMode::Normal
        );
    }
}

#[test]
fn snippet_keys_only_route_in_the_owning_buffer() {
    let (_app_dirs, mut app) = app();
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("one two"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.input.install_snippet(
        crate::app::language::snippets::SnippetDocument {
            editor: &app.state.window_version(),
            buffer,
            text: &app.state.buffers[buffer].text,
            view: Some(view),
        },
        ByteOffset(0),
        crate::language::lsp::parse_snippet("${1:one} ${2:two}", 1024)
            .unwrap()
            .tabstops,
    );
    let (other, _) = app
        .state
        .open_text(TextBuffer::from_text(""), None)
        .unwrap();
    press_keys(&mut app, &["i", "tab"]);
    assert!(!app.state.buffers[other].text.to_string().is_empty());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "one two");
    assert_eq!(app.state.active_buffer_id(), Some(other));
}

#[test]
fn timeout_executes_an_earlier_exact_mapping_and_replays_its_suffix() {
    let (_app_dirs, mut app) = app();
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(
        std::time::UNIX_EPOCH,
        Instant::now(),
    ));
    app.clock = clock.clone();
    let mut loaded = crate::config::load_bundled_config().keymaps;
    for (id, keys, command) in [
        ("short", "x", "edit.delete_char"),
        ("long", "x y z", "file.new"),
    ] {
        loaded.entries.push(crate::config::KeymapEntry {
            id: format!("user.test.{id}"),
            context: Some("editor".into()),
            mode: "normal".into(),
            keys: Some(keys.into()),
            command: Some(command.into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
        });
    }
    let (keymaps, diagnostic) = compile_keymaps(
        &app.commands,
        &loaded,
        &app.configuration.settings().input.keymaps,
    );
    assert!(diagnostic.is_none(), "{diagnostic:?}");
    app.configuration.install_bindings(keymaps, false);
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    press_keys(&mut app, &["x", "y"]);
    clock.advance(Duration::from_millis(
        app.configuration.settings().input.mapping_timeout_ms + 1,
    ));
    app.flush_keymap_timeout();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "bc");
    // Replayed y starts the ordinary yank operator.
    assert!(app.input.grammar_pending());
}

#[test]
fn delayed_open_preserves_newer_unsaved_canonical_buffer_and_closed_reload_cannot_reopen_it() {
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    let state_directory = app.workspace.state_path().parent().unwrap().to_path_buf();
    let path = app.state.workspace_root.join("late.txt");
    fs::write(&path, b"disk").unwrap();
    app.start_open(path.clone());
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("new unsaved"), Some(path.clone()))
        .unwrap();
    app.state.set_buffer_dirty(buffer, true);
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "new unsaved");
    assert!(app.state.buffers[buffer].dirty);
    app.start_reload(true);
    app.close_active_buffer(true);
    wait_vfs(&mut app);
    assert!(!app.state.buffers.contains_key(buffer));
    assert!(
        app.state
            .buffers
            .values()
            .all(|b| b.path.as_ref() != Some(&path))
    );
    drop(app);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(state_directory).unwrap();
}

/// Includes startup VFS submission/read/decode, editor+window publication and
/// final primary selection, but not rendering or general process startup.
#[test]
#[ignore = "explicit startup lifecycle cost probe"]
pub(in crate::app) fn startup_lifecycle_cost_probe() {
    use crate::app::plugins::Plugins;
    let (_app_dirs, mut app) = app();
    let root = app.state.workspace_root.clone();
    let state_directory = app.workspace.state_path().parent().unwrap().to_path_buf();
    let bytes = "0123456789abcdef\n".repeat(3856);
    for n in 0..4 {
        let path = app.state.workspace_root.join(format!("file-{n}.txt"));
        fs::write(&path, &bytes).unwrap();
        app.documents.enqueue_startup_fixture(path, n == 0);
    }
    let baseline = (
        crate::windows::WindowGraph::test_clone_count(),
        crate::editor::EditorState::test_clone_count(),
        Plugins::fixture_window_clone_count(),
        TextBuffer::test_materializations(),
    );
    eprintln!("DUST_ALLOC_START");
    let start = Instant::now();
    app.queue_startup_reads();
    wait_vfs(&mut app);
    let elapsed = start.elapsed().as_nanos();
    eprintln!("DUST_ALLOC_END");
    let materializations = TextBuffer::test_materializations();
    println!(
        "startup_files=4 bytes_per_file={} elapsed_ns={elapsed} graph_copies={} editor_copies={} host_copies={} text_decodes={} text_displays={}",
        bytes.len(),
        crate::windows::WindowGraph::test_clone_count() - baseline.0,
        crate::editor::EditorState::test_clone_count() - baseline.1,
        Plugins::fixture_window_clone_count() - baseline.2,
        materializations.0 - baseline.3.0,
        materializations.1 - baseline.3.1
    );
    assert_eq!(app.state.buffers.len(), 4);
    assert!(
        app.state.buffers[app.state.active_buffer_id().unwrap()]
            .path
            .as_ref()
            .unwrap()
            .ends_with("file-0.txt")
    );
    drop(app);
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(state_directory).unwrap();
}

#[test]
fn pending_open_cannot_resurrect_closed_path_or_steal_newer_navigation() {
    crate::app::tests::driver::TestSession::new(crate::config::load_bundled_config())
        .verify_open_lifetimes();
}

#[test]
fn force_save_as_does_not_overwrite_another_open_document() {
    crate::app::tests::driver::TestSession::new(crate::config::load_bundled_config())
        .verify_save_collisions();
}

#[test]
fn expired_shutdown_does_not_accept_or_report_an_exit_snapshot() {
    crate::app::tests::driver::TestSession::new(crate::config::load_bundled_config())
        .verify_expired_shutdown_does_not_write();
}

#[test]
fn the_line_after_a_final_newline_is_never_a_normal_mode_line() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(
            crate::editor::text::TextBuffer::from_text("one\n  two\n"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    let view = app.state.active_view().expect("view");
    let line = |app: &App| {
        app.state.buffers[buffer]
            .text
            .byte_to_line(app.state.views[view].cursor)
            .unwrap()
    };
    assert_eq!(app.state.buffers[buffer].text.visible_lines(), 2);
    for _ in 0..3 {
        app.handle_configured_key(KeyEvent::character('j'));
    }
    assert_eq!(line(&app), 1, "j stops at the last real line");
    app.handle_configured_key(KeyEvent::character('g'));
    app.handle_configured_key(KeyEvent::character('g'));
    app.handle_configured_key(KeyEvent::character('G'));
    assert_eq!(
        app.state.views[view].cursor.get(),
        6,
        "G lands on the last line's first non-blank"
    );
    app.handle_configured_key(KeyEvent::character('d'));
    app.handle_configured_key(KeyEvent::character('d'));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "one\n");
    assert_eq!(line(&app), 0, "dd on the last line lands on the line above");
}

#[test]
fn tutor_moves_on_when_each_step_is_done() {
    fn press(app: &mut App, key: &str) {
        self::press(app, key);
        app.tutor_tick();
    }
    pub(in crate::app) fn title(app: &App) -> String {
        app.ui
            .tutor
            .as_ref()
            .map(|bar| bar.title.clone())
            .unwrap_or_default()
    }
    fn cursor_line(app: &App) -> String {
        let view = &app.state.views[app.state.active_view().unwrap()];
        let text = &app.state.buffers[view.buffer].text;
        text.line(text.byte_to_line(view.cursor).unwrap())
            .unwrap()
            .trim()
            .to_owned()
    }
    fn wait_root(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while app.workspace.root_operation_pending() {
            app.process_trust_events();
            assert!(Instant::now() < deadline, "root switch timed out");
            thread::yield_now();
        }
        app.tutor_tick();
        wait_vfs(app);
        app.tutor_tick();
    }
    /// A done step says so for a moment before the next one comes.
    fn next_step(app: &mut App) {
        assert!(
            app.ui.tutor.as_ref().is_some_and(|bar| bar.praised),
            "{}",
            title(app)
        );
        let before = title(app);
        let deadline = Instant::now() + Duration::from_secs(3);
        while title(app) == before {
            assert!(Instant::now() < deadline, "{before} never moved on");
            thread::sleep(Duration::from_millis(10));
            app.tutor_tick();
        }
        wait_vfs(app);
        app.tutor_tick();
    }
    let (_app_dirs, mut app) = app();
    let home = app.state.workspace_root.canonicalize().unwrap();
    // The first start asks for a language; Rust comes first.
    app.process_effect(AppEffect::Tutor);
    assert!(app.ui.tutor.is_none());
    press(&mut app, "enter");
    wait_root(&mut app);
    assert!(title(&app).starts_with("Tutor 1/"), "{}", title(&app));
    assert!(app.ui.tutor.as_ref().unwrap().press.contains("`j`"));
    // The tutor moved the editor into its own practice repository.
    let practice = app.state.workspace_root.clone();
    assert_ne!(practice, home);
    assert!(practice.join(".git").is_dir() && practice.join("src/greet.rs").is_file());
    assert!(practice.join("practice.rs").is_file());
    press(&mut app, "j");
    next_step(&mut app);
    assert!(title(&app).starts_with("Tutor 2/"), "{}", title(&app));
    assert!(
        cursor_line(&app).starts_with("h  left"),
        "{}",
        cursor_line(&app)
    );
    assert!(
        app.ui.tutor.as_ref().unwrap().target.is_some(),
        "the X is marked"
    );
    for _ in 0..2 {
        press(&mut app, "j");
    }
    assert_eq!(cursor_line(&app), "The X marks the spot.");
    // `j` kept the column of `h` through the blank line, so `w` lands on the X.
    press(&mut app, "w");
    next_step(&mut app);
    assert!(title(&app).starts_with("Tutor 3/"), "{}", title(&app));
    // A step that checks text waits for the practice line to read right.
    app.process_effect(AppEffect::TutorBack);
    for _ in 0..10 {
        app.process_effect(AppEffect::TutorNext);
    }
    app.tutor_tick();
    wait_vfs(&mut app);
    app.tutor_tick();
    assert!(title(&app).starts_with("Tutor 12/"), "{}", title(&app));
    assert_eq!(cursor_line(&app), "Thhe cow jumpped over thee moon.");
    for key in ["l", "x", "f", "p", "x", "$", "6", "h", "x"] {
        press(&mut app, key);
    }
    let view = &app.state.views[app.state.active_view().unwrap()];
    assert!(
        app.state.buffers[view.buffer]
            .text
            .to_string()
            .contains("    The cow jumped over the moon.\n")
    );
    // The next step put the cursor on its own practice line.
    next_step(&mut app);
    assert!(title(&app).starts_with("Tutor 13/"), "{}", title(&app));
    assert_eq!(cursor_line(&app), "Dust is a editor");
    // The card says which line, and the gutter marks it.
    let card = app.ui.tutor.as_ref().unwrap();
    let line = card.line.expect("the step's line").1;
    assert_eq!(card.place, format!("practice.rs, line {}", line + 1));
    assert!(!card.away);
    // Going back to a step already done in the text stays there.
    app.process_effect(AppEffect::TutorBack);
    app.tutor_tick();
    assert!(title(&app).starts_with("Tutor 12/"), "{}", title(&app));
    app.process_effect(AppEffect::TutorNext);
    app.tutor_tick();
    // A file of the user's own pauses the tutor, whose card says how to get back; :Tutor goes
    // back too.
    let own = home.join("notes.txt");
    fs::write(&own, "mine\n").unwrap();
    app.start_open(own);
    wait_vfs(&mut app);
    app.tutor_tick();
    let card = app.ui.tutor.as_ref().expect("the card stays");
    assert!(
        card.away && card.place.contains("`:e practice.rs`"),
        "{}",
        card.place
    );
    // Back in the practice file, the cursor is on the step's line again.
    app.start_open(practice.join("practice.rs"));
    wait_vfs(&mut app);
    app.tutor_tick();
    assert_eq!(cursor_line(&app), "Dust is a editor");
    app.process_effect(AppEffect::Tutor);
    wait_vfs(&mut app);
    app.tutor_tick();
    assert!(title(&app).starts_with("Tutor 13/"), "{}", title(&app));
    assert_eq!(cursor_line(&app), "Dust is a editor");
    // Quitting moves back home and deletes the practice project.
    app.process_effect(AppEffect::TutorQuit);
    wait_root(&mut app);
    assert!(app.ui.tutor.is_none());
    assert_eq!(app.state.workspace_root, home);
    assert!(!practice.exists());
    assert!(app.state.buffers.values().all(|buffer| {
        buffer
            .path
            .as_deref()
            .is_none_or(|path| !path.starts_with(&practice))
    }));
    // The next start asks first; continuing is a fresh practice project at the step the user
    // stopped at.
    app.process_effect(AppEffect::Tutor);
    assert!(app.ui.tutor.is_none());
    press(&mut app, "enter");
    wait_root(&mut app);
    assert!(title(&app).starts_with("Tutor 13/"), "{}", title(&app));
    assert_ne!(app.state.workspace_root, practice);
}

#[test]
fn j_and_k_keep_their_column_across_shorter_lines() {
    fn press(app: &mut App, key: &str) {
        app.handle_event(UiEvent::Key {
            key: key.parse().unwrap(),
            kind: crate::ui::KeyInputKind::Press,
        });
    }
    let (_app_dirs, mut app) = app();
    app.set_dashboard_visibility(false);
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("abcdef\n\nab\nabcdef\n"), None)
        .unwrap();
    app.state.views[view].cursor = ByteOffset(4);
    for _ in 0..3 {
        press(&mut app, "j");
    }
    assert_eq!(app.state.views[view].cursor, ByteOffset(15));
    press(&mut app, "h");
    press(&mut app, "k");
    assert_eq!(app.state.views[view].cursor, ByteOffset(9));
}

pub(in crate::app) fn press(app: &mut App, key: &str) {
    app.handle_event(UiEvent::Key {
        key: key.parse().unwrap(),
        kind: crate::ui::KeyInputKind::Press,
    });
}

pub(in crate::app) fn press_keys(app: &mut App, keys: &[&str]) {
    for key in keys {
        press(app, key);
    }
}

#[test]
fn vim_edits_that_lazyvim_users_reach_for() {
    let (_app_dirs, mut app) = app();
    app.set_dashboard_visibility(false);
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("The cat sat.\n"), None)
        .unwrap();
    app.state.views[view].cursor = ByteOffset(4);
    // `cw` keeps the space after the word.
    press_keys(&mut app, &["c", "w", "d", "o", "g", "escape"]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "The dog sat.\n");

    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("a\nb\n\nc\n"), None)
        .unwrap();
    app.state.views[view].cursor = ByteOffset(0);
    // Visual `>` with a count indents that many levels and leaves blank lines alone.
    press_keys(&mut app, &["V", "j", "j", "2", ">"]);
    let level = if app.state.policy.expand_tabs {
        " ".repeat(app.state.policy.indent_width)
    } else {
        "\t".into()
    };
    let indent = level.repeat(2);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        format!("{indent}a\n{indent}b\n\nc\n")
    );

    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("one two\nthree\n"), None)
        .unwrap();
    app.state.views[view].cursor = ByteOffset(0);
    // Visual `U`, `J` and `p`.
    press_keys(&mut app, &["v", "e", "U"]);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "ONE two\nthree\n"
    );
    press_keys(&mut app, &["V", "j", "J"]);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "ONE two three\n"
    );
    app.state.views[view].cursor = ByteOffset(0);
    press_keys(&mut app, &["y", "i", "w"]);
    app.state.views[view].cursor = ByteOffset(4);
    press_keys(&mut app, &["v", "e", "p"]);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "ONE ONE three\n"
    );
}

#[test]
fn paragraph_moves_are_jumps_that_ctrl_o_returns_from() {
    let (_app_dirs, mut app) = app();
    app.set_dashboard_visibility(false);
    let path = app.state.workspace_root.join("jumps.txt");
    fs::write(&path, b"a\nb\n\nc\n").expect("jumps");
    app.start_open(path);
    wait_vfs(&mut app);
    let view = app.state.active_view().expect("view");
    app.state.views[view].cursor = ByteOffset(0);
    press_keys(&mut app, &["}"]);
    assert_eq!(app.state.views[view].cursor, ByteOffset(4));
    press_keys(&mut app, &["G", "ctrl-o"]);
    wait_vfs(&mut app);
    let view = app.state.active_view().expect("view");
    assert_eq!(app.state.views[view].cursor, ByteOffset(4));
    press_keys(&mut app, &["ctrl-o"]);
    wait_vfs(&mut app);
    let view = app.state.active_view().expect("view");
    assert_eq!(app.state.views[view].cursor, ByteOffset(0));
    press_keys(&mut app, &["tab"]);
    wait_vfs(&mut app);
    let view = app.state.active_view().expect("view");
    assert_eq!(app.state.views[view].cursor, ByteOffset(4));
}

#[test]
fn markdown_lines_comment_with_html_comments() {
    let (_app_dirs, mut app) = app();
    app.set_dashboard_visibility(false);
    let path = app.state.workspace_root.join("notes.md");
    fs::write(&path, b"hello\n").expect("notes");
    app.start_open(path);
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("notes buffer");
    press_keys(&mut app, &["g", "c", "c"]);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "<!-- hello -->\n"
    );
    press_keys(&mut app, &["g", "c", "c"]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "hello\n");
}

#[test]
fn several_cursors_change_every_match_and_type_on_every_line() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("foo bar food foo\nx\nyy\n"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let press = |app: &mut App, key: &str| {
        let event = match key.chars().collect::<Vec<_>>()[..] {
            [character] => KeyEvent::character(character),
            _ => key.parse().expect("key"),
        };
        app.handle_configured_key(event);
    };
    let text = |app: &App| {
        app.state.buffers[app.state.views[view].buffer]
            .text
            .to_string()
    };

    // Ctrl-N takes whole words only: `food` is not a match.
    press(&mut app, "ctrl-n");
    press(&mut app, "ctrl-n");
    assert_eq!(app.state.views[view].mode, crate::editor::EditorMode::Multi);
    assert_eq!(app.state.views[view].multi_marks.len(), 2);
    press(&mut app, "c");
    for key in ["z", "z"] {
        press(&mut app, key);
    }
    press(&mut app, "escape");
    assert_eq!(text(&app), "zz bar food zz\nx\nyy\n");
    assert!(app.state.views[view].multi_marks.is_empty());

    // Cursors on the lines below, typing at each.
    app.state.views[view].cursor = ByteOffset("zz bar food zz\n".len());
    press(&mut app, "ctrl-alt-j");
    assert_eq!(app.state.views[view].multi_marks.len(), 2);
    press(&mut app, "i");
    press(&mut app, "-");
    press(&mut app, "escape");
    assert_eq!(text(&app), "zz bar food zz\n-x\n-yy\n");

    // `d` removes each match.
    app.state.views[view].cursor = ByteOffset(0);
    press(&mut app, "ctrl-n");
    press(&mut app, "ctrl-n");
    press(&mut app, "d");
    assert_eq!(
        app.state.views[view].mode,
        crate::editor::EditorMode::Normal
    );
    assert_eq!(text(&app), " bar food \n-x\n-yy\n");
}

#[test]
fn dot_stops_before_inserting_after_a_failed_search_operator() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("cat cat"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.ui.search_query = Some("cat".into());
    press_keys(&mut app, &["c", "g", "n", "d", "o", "g", "escape"]);
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("unrelated"), None)
        .unwrap();
    press_keys(&mut app, &["."]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "unrelated");
    assert!(app.ui.notification_error);
}

#[test]
fn word_search_projects_its_boundary_policy_until_an_ordinary_search_replaces_it() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("foo foobar foo"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    press_keys(&mut app, &["*"]);
    assert!(app.ui.search_whole_word);
    assert_eq!(app.ui.search_query.as_deref(), Some("foo"));
    let view = app.state.active_view().unwrap();
    assert_eq!(app.state.views[view].cursor, ByteOffset(11));
    press_keys(&mut app, &["/", "f", "o", "o", "enter"]);
    assert!(!app.ui.search_whole_word);
    assert!(!app.input.whole_word_search());
}

#[test]
fn cgn_repeats_with_dot_substitute_confirms_each_match_and_global_deletes_lines() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(
            TextBuffer::from_text("cat one\ncat two\n// cat three\ncat four\n"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    let press = |app: &mut App, key: &str| {
        let event = match key.chars().collect::<Vec<_>>()[..] {
            [character] => KeyEvent::character(character),
            _ => key.parse().expect("key"),
        };
        app.handle_configured_key(event);
    };
    let typed = |app: &mut App, keys: &str| {
        for character in keys.chars() {
            app.handle_configured_key(KeyEvent::character(character));
        }
    };
    let text = |app: &App| {
        app.state.buffers[app.state.views[view].buffer]
            .text
            .to_string()
    };

    // `/cat` goes to the second cat; `cgn` changes it and `.` the one after.
    typed(&mut app, "/cat");
    press(&mut app, "enter");
    typed(&mut app, "cgndog");
    press(&mut app, "escape");
    assert_eq!(text(&app), "cat one\ndog two\n// cat three\ncat four\n");
    press(&mut app, ".");
    assert_eq!(text(&app), "cat one\ndog two\n// dog three\ncat four\n");

    // `:s///c` asks about each match: skip the first, replace the second.
    typed(&mut app, ":%s/cat/cow/c");
    press(&mut app, "enter");
    assert_eq!(
        app.input.prompt().map(|prompt| prompt.kind),
        Some(crate::ui::PromptKind::SubstituteConfirm)
    );
    press(&mut app, "n");
    press(&mut app, "y");
    assert_eq!(text(&app), "cat one\ndog two\n// dog three\ncow four\n");
    assert!(app.input.prompt().is_none());
    assert_eq!(
        app.state.views[view].mode,
        crate::editor::EditorMode::Normal
    );
    press(&mut app, "u");
    assert_eq!(text(&app), "cat one\ndog two\n// dog three\ncat four\n");

    // `a` replaces every match left.
    typed(&mut app, ":%s/cat/cow/c");
    press(&mut app, "enter");
    press(&mut app, "a");
    assert_eq!(text(&app), "cow one\ndog two\n// dog three\ncow four\n");

    // `:g` deletes the lines that match, `:v` the ones that don't.
    typed(&mut app, ":g#^//#d");
    press(&mut app, "enter");
    assert_eq!(text(&app), "cow one\ndog two\ncow four\n");
    typed(&mut app, ":v/cow/d");
    press(&mut app, "enter");
    assert_eq!(text(&app), "cow one\ncow four\n");
}

#[test]
fn substitute_rejects_out_of_range_addresses_without_editing() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("foo\nfoo"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.run_substitute("999", "foo", "bar", "");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "foo\nfoo");
    app.run_global_delete("1,999", "foo", false);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "foo\nfoo");
}

#[test]
fn substitute_cursor_stays_on_a_normal_mode_grapheme() {
    for flags in ["", "c"] {
        let (_app_dirs, mut app) = app();
        let (buffer, view) = app
            .state
            .open_text(TextBuffer::from_text("a"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.run_substitute("", "a", "e\u{301}", flags);
        if flags == "c" {
            app.answer_substitution(true, false);
        }
        assert_eq!(app.state.buffers[buffer].text.to_string(), "e\u{301}");
        assert_eq!(app.state.views[view].cursor, ByteOffset(0));
        press_keys(&mut app, &["x"]);
        assert_eq!(app.state.buffers[buffer].text.to_string(), "");
    }
}

#[test]
fn substitute_cursor_does_not_split_a_grapheme_after_partial_replacement() {
    for flags in ["", "c"] {
        let (_app_dirs, mut app) = app();
        let (buffer, view) = app
            .state
            .open_text(TextBuffer::from_text("a\u{301}!"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.run_substitute("", "a", "e", flags);
        if flags == "c" {
            app.answer_substitution(true, false);
        }
        assert_eq!(app.state.buffers[buffer].text.to_string(), "e\u{301}!");
        assert_eq!(app.state.views[view].cursor, ByteOffset(0));
        press_keys(&mut app, &["x"]);
        assert_eq!(app.state.buffers[buffer].text.to_string(), "!");
    }
}

#[test]
fn substitute_and_global_delete_record_change_locations() {
    for flags in ["", "c"] {
        let (_app_dirs, mut app) = app();
        let path = app.state.workspace_root.join("substitute.txt");
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("cat"), Some(path))
            .unwrap();
        app.set_dashboard_visibility(false);
        assert!(app.persistence.changes().is_empty());
        app.run_substitute("", "cat", "cow", flags);
        if flags == "c" {
            app.answer_substitution(true, false);
        }
        assert_eq!(app.state.buffers[buffer].text.to_string(), "cow");
        assert_eq!(app.persistence.changes().len(), 1);
    }
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("global.txt");
    app.state
        .open_text(TextBuffer::from_text("drop\nkeep\n"), Some(path))
        .unwrap();
    app.set_dashboard_visibility(false);
    assert!(app.persistence.changes().is_empty());
    app.run_global_delete("", "drop", false);
    assert_eq!(app.persistence.changes().len(), 1);
}

#[test]
fn global_delete_updates_the_linewise_register_and_paste() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("drop one\nkeep\ndrop two\n"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    press_keys(&mut app, &["y", "y"]);
    app.run_global_delete("", "drop", false);
    let register = app.state.registers.get('"').unwrap();
    assert_eq!(register.text, "drop one\ndrop two\n");
    assert_eq!(register.shape, crate::editor::RegisterShape::Line);
    press_keys(&mut app, &["p"]);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "keep\ndrop one\ndrop two\n"
    );
}

#[test]
fn substitute_confirmation_refuses_reloaded_text_at_the_same_revision() {
    for finish in [false, true] {
        let (_app_dirs, mut app) = app();
        let (buffer, _) = app
            .state
            .open_text(TextBuffer::from_text("cat cat cat"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.run_substitute("%", "cat", "cow", "gc");
        app.answer_substitution(true, false);
        let revision = app.state.buffers[buffer].text.revision();
        app.state.buffers[buffer].text = TextBuffer::from_text("dog dog dog");
        assert_eq!(app.state.buffers[buffer].text.revision(), revision);
        if finish {
            app.finish_substitution();
        } else {
            app.answer_substitution(true, false);
        }
        assert!(app.substitute_confirm.is_none());
        assert_eq!(app.state.buffers[buffer].text.to_string(), "dog dog dog");
    }
}

#[test]
fn substitute_confirmation_bounds_long_match_and_replacement_previews() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text(&"a".repeat(2000)), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.window_session.observe_size((80, 24));
    app.run_substitute("", ".*", &"b".repeat(2000), "c");
    let prompt = app.input.prompt().unwrap();
    assert!(crate::ui::confirmation_fully_visible(
        80,
        24,
        app.configuration.settings(),
        prompt
    ));
    assert!(prompt.confirmation.as_ref().unwrap().message.contains('…'));
    press_keys(&mut app, &["y"]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "b".repeat(2000));
}

#[test]
fn substitute_confirmation_keeps_empty_matches_as_insertion_points() {
    let (_app_dirs, mut app) = app();
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("one\ntwo\n"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.run_substitute("%", "^", ">", "gc");
    app.answer_substitution(true, false);
    assert_eq!(app.state.views[view].cursor, ByteOffset(4));
    assert!(app.state.views[view].selection.is_none());
    app.answer_substitution(true, false);
    assert_eq!(app.state.buffers[buffer].text.to_string(), ">one\n>two\n");
}

#[test]
fn substitute_confirmation_scrolls_visual_rows_above_the_dialog() {
    for wrap in [false, true] {
        let (_app_dirs, mut app) = app();
        let text = if wrap {
            format!("{}cat\n", "padding ".repeat(150))
        } else {
            format!("{}cat\n", "padding\n".repeat(16))
        };
        let (_, view) = app
            .state
            .open_text(TextBuffer::from_text(&text), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.window_session.observe_size((80, 24));
        let mut configuration = app.configuration.fixture_snapshot();
        configuration.settings.editor.wrap = wrap;
        app.configuration.install_fixture(configuration);
        app.run_substitute("%", "cat", &"界".repeat(400), "c");
        let viewport = crate::ui::view_pane_viewport(&app.state, &app.ui, view).unwrap();
        let prompt = app.input.prompt().unwrap();
        let geometry =
            crate::ui::prompt_geometry_for_state(80, 24, app.configuration.settings(), prompt);
        assert!(geometry.height > 6);
        let target = &app.state.views[view];
        let rows = crate::ui::VisualRowModel::new_for_state(
            &app.state,
            &app.state.buffers[target.buffer].text,
            view,
            &app.ui,
            app.configuration.settings(),
            viewport.width,
            wrap,
        );
        let row = rows
            .screen_row(
                crate::ui::VisualRowAnchor {
                    line: target.scroll_line,
                    wrap_row: target.scroll_wrap_row,
                },
                rows.cursor_anchor(target.cursor),
                usize::from(viewport.height),
            )
            .expect("match is visible");
        assert!(usize::from(viewport.y) + row < usize::from(geometry.top));
    }
}

#[test]
fn multi_cursors_take_every_match_skip_remove_and_move_together() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(
            TextBuffer::from_text("let cat = 1;\nlet cat = 2;\nlet cat = 3;\nlet dog = cat;\n"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    let press = |app: &mut App, key: &str| {
        let event = match key.chars().collect::<Vec<_>>()[..] {
            [character] => KeyEvent::character(character),
            _ => key.parse().expect("key"),
        };
        app.handle_configured_key(event);
    };
    let text = |app: &App| {
        app.state.buffers[app.state.views[view].buffer]
            .text
            .to_string()
    };
    let marks = |app: &App| app.state.views[view].multi_marks.len();

    // Ctrl-Shift-L takes every match; Q drops the one the cursor is on.
    app.state.views[view].cursor = ByteOffset(4);
    press(&mut app, "ctrl-shift-l");
    assert_eq!(marks(&app), 4);
    press(&mut app, "N");
    assert_eq!(
        app.state.views[view].cursor,
        ByteOffset("let cat = 1;\nlet cat = 2;\nlet cat = 3;\nlet dog = ".len())
    );
    press(&mut app, "Q");
    assert_eq!(marks(&app), 3);
    press(&mut app, "c");
    for key in ["c", "o", "w"] {
        press(&mut app, key);
    }
    press(&mut app, "escape");
    assert_eq!(
        text(&app),
        "let cow = 1;\nlet cow = 2;\nlet cow = 3;\nlet dog = cat;\n"
    );

    // q skips the second match for the one after it.
    app.state.views[view].cursor = ByteOffset(4);
    press(&mut app, "ctrl-n");
    press(&mut app, "ctrl-n");
    press(&mut app, "q");
    let starts: Vec<usize> = app.state.views[view]
        .multi_marks
        .iter()
        .map(|mark| mark.start.get())
        .collect();
    assert_eq!(starts, vec![4, "let cow = 1;\nlet cow = 2;\nlet ".len()]);
    press(&mut app, "escape");
    // With every match already taken, q just drops the one it is on.
    app.state.views[view].cursor = ByteOffset(4);
    press(&mut app, "ctrl-shift-l");
    assert_eq!(marks(&app), 3);
    press(&mut app, "q");
    assert_eq!(marks(&app), 2);
    press(&mut app, "escape");

    // Motions move every cursor; x deletes at each, and A types at each line's end.
    app.state.views[view].cursor = ByteOffset(0);
    press(&mut app, "ctrl-alt-j");
    press(&mut app, "ctrl-alt-j");
    press(&mut app, "w");
    press(&mut app, "x");
    assert_eq!(
        text(&app),
        "let ow = 1;\nlet ow = 2;\nlet ow = 3;\nlet dog = cat;\n"
    );
    press(&mut app, "A");
    press(&mut app, "!");
    press(&mut app, "escape");
    assert_eq!(
        text(&app),
        "let ow = 1;!\nlet ow = 2;!\nlet ow = 3;!\nlet dog = cat;\n"
    );
}

#[test]
fn selecting_every_match_of_a_selected_part_of_a_word_matches_it_as_typed() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("foobar\nxfoo\n"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in ["v", "l", "l"] {
        app.handle_configured_key(KeyEvent::character(key.chars().next().unwrap()));
    }
    app.handle_configured_key("ctrl-shift-l".parse().expect("key"));
    let starts: Vec<usize> = app.state.views[view]
        .multi_marks
        .iter()
        .map(|mark| mark.start.get())
        .collect();
    assert_eq!(starts, vec![0, "foobar\nx".len()]);
}

#[test]
fn visual_block_i_and_a_type_before_and_after_every_row() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("  x = 1\n  y = 2\n  z = 3\n"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    let keys = |app: &mut App, keys: &str| {
        for character in keys.chars() {
            app.handle_configured_key(KeyEvent::character(character));
        }
    };
    let text = |app: &App| {
        app.state.buffers[app.state.views[view].buffer]
            .text
            .to_string()
    };
    app.state.views[view].cursor = ByteOffset(2);
    app.handle_configured_key("ctrl-v".parse().expect("key"));
    keys(&mut app, "2jIlet ");
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    assert_eq!(text(&app), "  let x = 1\n  let y = 2\n  let z = 3\n");
    app.state.views[view].cursor = ByteOffset("  let ".len());
    app.handle_configured_key("ctrl-v".parse().expect("key"));
    keys(&mut app, "jA:");
    app.handle_configured_key(KeyEvent::plain(crate::editor::input::KeyCode::Escape));
    assert_eq!(text(&app), "  let x: = 1\n  let y: = 2\n  let z = 3\n");
}

#[test]
fn unknown_key_after_a_prefix_ends_the_sequence_without_editing() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("prefix.txt");
    fs::write(&path, b"Header\n").expect("file");
    app.start_open(path);
    wait_vfs(&mut app);
    // Neither sequence is bound; the second key must not run as `a` or `o` would.
    for character in ['[', 'o', ']', 'a'] {
        app.handle_configured_key(KeyEvent::character(character));
    }
    assert_eq!(
        app.state.buffers[app.state.active_buffer_id().expect("buffer")]
            .text
            .to_string(),
        "Header\n"
    );
    assert_eq!(
        app.state.views[app.state.active_view().expect("view")].mode,
        crate::editor::EditorMode::Normal
    );
}

#[test]
fn conflict_accept_rejects_unknown_sides_and_preserves_malformed_blocks() {
    let (_app_dirs, mut app) = app();
    let source = "<<<<<<< orphan\n>>>>>>> early\nunrelated\n<<<<<<<content\n<<<<< ours\nours\n=====\ntheirs\n>>>>> other\n";
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text(source), None)
        .unwrap();
    let revision = app.state.buffers[buffer].text.revision();
    app.process_effect(AppEffect::GitConflictAccept { side: "our".into() });
    assert_eq!(app.state.buffers[buffer].text.to_string(), source);
    assert_eq!(app.state.buffers[buffer].text.revision(), revision);
    assert!(
        app.ui
            .notification
            .as_ref()
            .is_some_and(|message| message.contains("conflict side"))
    );
    app.process_effect(AppEffect::GitConflictAccept {
        side: "theirs".into(),
    });
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "<<<<<<< orphan\n>>>>>>> early\nunrelated\n<<<<<<<content\ntheirs\n",
    );
    assert!(
        app.ui
            .notification
            .as_ref()
            .is_some_and(|message| message.contains("no conflicts left"))
    );
}

#[test]
fn conflict_keys_move_between_markers_and_wrap() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("merged.txt");
    fs::write(&path, b"Header\n<<<<<<< ours\na\n=======\nb\n>>>>>>> theirs\nmid\n<<<<<<< ours\nc\n=======\nd\n>>>>>>> theirs\n").expect("file");
    app.start_open(path);
    wait_vfs(&mut app);
    let line = |app: &mut App| {
        wait_vfs(app);
        let view = &app.state.views[app.state.active_view().expect("view")];
        app.state.buffers[view.buffer]
            .text
            .byte_to_line(view.cursor)
            .expect("line")
    };
    app.process_effect(AppEffect::GitConflictNavigate(1));
    assert_eq!(line(&mut app), 1);
    app.process_effect(AppEffect::GitConflictNavigate(1));
    assert_eq!(line(&mut app), 7);
    app.process_effect(AppEffect::GitConflictNavigate(1));
    assert_eq!(line(&mut app), 1, "wraps to the first");
    app.process_effect(AppEffect::GitConflictNavigate(-1));
    assert_eq!(line(&mut app), 7, "wraps to the last");
}

#[test]
fn the_first_disk_poll_detects_changes_since_opening() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("changed-before-poll.txt");
    fs::write(&path, "before\n").unwrap();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().unwrap();
    fs::write(&path, "after opening\n").unwrap();
    app.next_disk_check = None;
    app.process_disk_changes();
    assert!(app.documents.file_work_pending());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "before\n");
    wait_vfs(&mut app);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "after opening\n"
    );
    assert!(!app.state.buffers[buffer].dirty);
}

#[test]
fn disk_checks_preserve_unsaved_edits_and_warn_only_when_disk_contents_differ() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("dirty-before-poll.txt");
    fs::write(&path, "disk\n").unwrap();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().unwrap();
    app.replace_with_recovered(buffer, "unsaved\n".into())
        .unwrap();
    let source = app.state.buffers[buffer].text.source_version();
    app.clear_notice();
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    assert!(app.ui.notification.is_none());
    assert_eq!(app.state.buffers[buffer].text.source_version(), source);
    assert!(app.state.buffers[buffer].dirty);
    fs::write(&path, "different disk contents\n").unwrap();
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "unsaved\n");
    assert_eq!(app.state.buffers[buffer].text.source_version(), source);
    assert_eq!(
        app.ui.notification.as_deref(),
        Some(
            app.message(
                "file_changed_on_disk",
                &[("path", app.workspace_relative_label(&path))],
            )
            .as_str()
        )
    );
}

#[test]
fn reload_normalizes_every_view_of_the_buffer_without_changing_focus() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("shortened.txt");
    fs::write(&path, "long first line\nsecond line\n").unwrap();
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().unwrap();
    let first = app.state.active_view().unwrap();
    app.state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .unwrap();
    let second = app.state.active_view().unwrap();
    app.state.views[first].cursor = ByteOffset(20);
    app.state.views[second].cursor = ByteOffset(20);
    app.state
        .split_active(crate::windows::TileAxis::Vertical, 500)
        .unwrap();
    fs::write(&path, "x\n").unwrap();
    app.start_reload(false);
    let (_, other) = app
        .state
        .open_text(TextBuffer::from_text("other"), None)
        .unwrap();
    wait_vfs(&mut app);
    assert_eq!(app.state.active_view(), Some(other));
    assert_eq!(app.state.buffers[buffer].text.to_string(), "x\n");
    assert_eq!(app.state.views[first].cursor, ByteOffset(0));
    assert_eq!(app.state.views[second].cursor, ByteOffset(0));
}

#[test]
fn clean_buffers_follow_the_disk_and_changed_ones_keep_their_text() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("watched.txt");
    fs::write(&path, b"first\n").expect("file");
    app.start_open(path.clone());
    wait_vfs(&mut app);
    let buffer = app.state.active_buffer_id().expect("buffer");
    let source = app.state.buffers[buffer].text.source_version();
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.source_version(), source);

    // Longer content, so the length differs even if the clock did not tick.
    fs::write(&path, b"from outside\n").expect("rewrite");
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "from outside\n");

    app.state.buffers[buffer].dirty = true;
    fs::write(&path, b"outside again and longer\n").expect("rewrite");
    app.next_disk_check = None;
    app.process_disk_changes();
    wait_vfs(&mut app);
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "from outside\n",
        "unsaved work stays"
    );
}

pub(in crate::app) fn wait_syntax(app: &mut App, buffer: crate::editor::BufferId) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.process_syntax();
        if app.syntax.source(buffer) == Some(&app.state.buffers[buffer].text.source_version())
            && app.syntax.result(buffer).is_some_and(|result| {
                Some(result.language)
                    == crate::language::syntax::language_for_document(
                        app.state.buffers[buffer].path.as_deref(),
                        "",
                    )
            })
        {
            break;
        }
        assert!(Instant::now() < deadline, "syntax reply timed out");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn syntax_objects_refresh_when_only_the_document_language_changes() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("hello.rs");
    let (buffer, _) = app
        .state
        .open_text(
            TextBuffer::from_text("def hello():\n    return 1\n"),
            Some(path),
        )
        .unwrap();
    wait_syntax(&mut app, buffer);
    let revision = app.state.buffers[buffer].text.revision();
    app.state.buffers[buffer].path = Some(app.state.workspace_root.join("hello.py"));
    wait_syntax(&mut app, buffer);
    let (at, objects) = app.state.buffers[buffer].syntax_objects.as_ref().unwrap();
    assert_eq!(*at, revision);
    assert!(
        objects
            .iter()
            .any(|object| object.kind == crate::editor::SyntaxObjectKind::Function)
    );
}

#[test]
fn collapsed_syntax_folds_follow_lines_inserted_above_them() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("hello.rs");
    let (buffer, _) = app
        .state
        .open_text(
            TextBuffer::from_text("\nfn hello() {\n    let a = 1;\n}\n"),
            Some(path),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    wait_syntax(&mut app, buffer);
    let fold = app
        .ui
        .syntax_folds
        .iter_mut()
        .find(|fold| fold.start_line == 1)
        .unwrap();
    fold.collapsed = true;
    let end = fold.end_line;
    let revision = app.state.buffers[buffer].text.revision();
    app.state.buffers[buffer]
        .text
        .apply_edits(&crate::editor::text::TextEditSet::new(
            revision,
            vec![crate::editor::text::TextEdit::new(
                ByteRange::empty(ByteOffset(0)),
                "\n",
            )],
        ))
        .unwrap();
    // A view change rebuilds rows while this buffer still has its older parse.
    app.process_syntax();
    app.state
        .split_active(crate::windows::TileAxis::Horizontal, 500)
        .unwrap();
    app.process_syntax();
    assert!(
        app.ui
            .syntax_folds
            .iter()
            .any(|fold| fold.collapsed && fold.start_line == 2 && fold.end_line == end + 1)
    );
    wait_syntax(&mut app, buffer);
    assert!(
        app.ui
            .syntax_folds
            .iter()
            .any(|fold| fold.collapsed && fold.start_line == 2 && fold.end_line == end + 1)
    );
}

#[test]
fn idle_syntax_and_clipboard_polls_preserve_frame_revisions() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("let word = 1;"), None)
        .unwrap();
    app.ui.lsp_semantic_spans = vec![crate::ui::UiSyntaxSpan {
        buffer,
        revision: app.state.buffers[buffer].text.revision(),
        range: ByteRange::new(ByteOffset(4), ByteOffset(8)).unwrap(),
        role: crate::config::Highlight::VARIABLE,
    }];
    app.process_syntax();
    let before = (app.state.revision, app.ui.revision);
    for _ in 0..3 {
        app.process_syntax();
        assert!(app.process_clipboard().is_none());
        assert_eq!((app.state.revision, app.ui.revision), before);
    }
}

#[test]
fn colours_follow_typed_text_until_the_next_parse_or_token_reply() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(
            crate::editor::text::TextBuffer::from_text("let word = 1;"),
            None,
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    let buffer = app.state.active_buffer_id().expect("buffer");
    let word = ByteRange::new(ByteOffset(4), ByteOffset(8)).unwrap();
    app.ui.lsp_semantic_spans = vec![crate::ui::UiSyntaxSpan {
        buffer,
        revision: app.state.buffers[buffer].text.revision(),
        range: word,
        role: crate::config::Highlight::VARIABLE,
    }];
    for key in ['i', 'X'] {
        app.handle_configured_key(KeyEvent::character(key));
    }
    let revision = app.ui.revision;
    app.process_syntax();
    assert!(app.ui.revision > revision);
    let span = app.ui.lsp_semantic_spans.first().expect("the colour stays");
    assert_eq!(span.revision, app.state.buffers[buffer].text.revision());
    assert_eq!(
        span.range,
        ByteRange::new(ByteOffset(5), ByteOffset(9)).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn recovery_through_an_alias_preserves_the_already_open_dirty_buffer() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    let path = app.state.workspace_root.join("current.txt");
    let alias = app.state.workspace_root.join("old.txt");
    fs::write(&path, "disk").unwrap();
    std::os::unix::fs::symlink(&path, &alias).unwrap();
    let (original, _) = app
        .state
        .open_text(TextBuffer::from_text("disk"), Some(path.clone()))
        .unwrap();
    app.replace_with_recovered(original, "new unsaved work".into())
        .unwrap();
    let journal = app.state.workspace_root.join("journal.toml");
    fs::write(&journal, "journal").unwrap();
    let id = app
        .documents
        .read_scoped(alias, vec![app.state.workspace_root.clone()])
        .unwrap();
    app.recovery.register_read(
        id,
        journal,
        "recovered draft".into(),
        app.workspace.version(),
    );
    wait_vfs(&mut app);
    let recovered = app.state.active_buffer_id().unwrap();
    assert_ne!(recovered, original);
    assert_eq!(
        app.state.buffers[original].text.to_string(),
        "new unsaved work"
    );
    assert!(app.state.buffers[original].dirty);
    assert_eq!(app.state.buffers[original].path.as_ref(), Some(&path));
    assert_eq!(
        app.state.buffers[recovered].text.to_string(),
        "recovered draft"
    );
    assert!(app.state.buffers[recovered].dirty);
    assert!(app.state.buffers[recovered].path.is_none());
    assert_eq!(fs::read_to_string(path).unwrap(), "disk");
}

#[test]
fn recovery_replies_never_replace_a_buffer_opened_during_the_read() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("recover.txt");
    fs::write(&path, "disk").unwrap();
    let id = app
        .documents
        .read_scoped(path.clone(), vec![app.state.workspace_root.clone()])
        .unwrap();
    app.recovery.register_read(
        id,
        path.with_extension("journal"),
        "recovered".into(),
        app.workspace.version(),
    );
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("disk"), Some(path))
        .unwrap();
    app.replace_with_recovered(buffer, "new edits".into())
        .unwrap();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "new edits");
    let recovered = &app.state.buffers[app.state.active_buffer_id().unwrap()];
    assert_eq!(recovered.text.to_string(), "recovered");
    assert!(recovered.path.is_none());
    assert!(recovered.dirty);
}

#[test]
fn recovery_missing_original_is_bound_only_inside_the_workspace() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("missing.txt");
    let id = app
        .documents
        .read_scoped(path.clone(), vec![app.state.workspace_root.clone()])
        .unwrap();
    app.recovery.register_read(
        id,
        path.with_extension("journal"),
        "recovered".into(),
        app.workspace.version(),
    );
    wait_vfs(&mut app);
    let recovered = &app.state.buffers[app.state.active_buffer_id().unwrap()];
    assert_eq!(recovered.path.as_ref(), Some(&path));
    assert_eq!(recovered.text.to_string(), "recovered");
}

#[test]
fn recovery_missing_parent_does_not_bind_an_unvalidated_destination() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("missing-parent/original.txt");
    let id = app
        .documents
        .read_scoped(path.clone(), vec![app.state.workspace_root.clone()])
        .unwrap();
    app.recovery.register_read(
        id,
        path.with_extension("journal"),
        "recovered".into(),
        app.workspace.version(),
    );
    wait_vfs(&mut app);
    let recovered = &app.state.buffers[app.state.active_buffer_id().unwrap()];
    assert_eq!(recovered.text.to_string(), "recovered");
    assert!(recovered.path.is_none());
    assert!(recovered.dirty);
}

#[cfg(unix)]
#[test]
fn recovery_rejected_paths_and_missing_files_under_outside_symlinks_become_scratch() {
    for exists in [true, false] {
        let (_app_dirs, mut app) = app();
        let outside = tempdir().unwrap();
        if exists {
            fs::write(outside.path().join("original.txt"), "").unwrap();
        }
        let link = app.state.workspace_root.join("link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let path = link.join("original.txt");
        let id = app
            .documents
            .read_scoped(path.clone(), vec![app.state.workspace_root.clone()])
            .unwrap();
        app.recovery.register_read(
            id,
            path.with_extension("journal"),
            "recovered".into(),
            app.workspace.version(),
        );
        wait_vfs(&mut app);
        let recovered = &app.state.buffers[app.state.active_buffer_id().unwrap()];
        assert_eq!(recovered.text.to_string(), "recovered");
        assert!(recovered.path.is_none());
        assert!(recovered.dirty);
        assert_eq!(outside.path().join("original.txt").exists(), exists);
    }
}

#[test]
fn recovery_failure_and_cancelled_reads_preserve_the_journal() {
    let (_app_dirs, mut app) = app();
    app.recovery.skip_maintenance();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("saved"), None)
        .unwrap();
    app.state.buffers[buffer].read_only = true;
    assert!(app.replace_with_recovered(buffer, "draft".into()).is_err());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "saved");
    let path = app.state.workspace_root.join("recover.txt");
    fs::write(&path, "disk").unwrap();
    let journal = app.state.workspace_root.join("journal.toml");
    fs::write(&journal, "recoverable").unwrap();
    let id = app
        .documents
        .read_scoped(path, vec![app.state.workspace_root.clone()])
        .unwrap();
    app.recovery
        .register_read(id, journal.clone(), "draft".into(), app.workspace.version());
    app.workspace.fixture_safe_mode(true);
    wait_vfs(&mut app);
    assert_eq!(fs::read_to_string(journal).unwrap(), "recoverable");
    assert!(
        app.state
            .buffers
            .values()
            .all(|buffer| buffer.text.to_string() != "draft")
    );
}

#[test]
fn flash_labels_only_visible_wrapped_rows_and_horizontal_columns() {
    for wrap in [true, false] {
        let (_app_dirs, mut app) = app();
        app.window_session.observe_size((12, 8));
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = wrap;
        fixture.settings.editor.line_numbers = crate::config::LineNumbers::Off;
        fixture.settings.ui.status_line.enabled = false;
        app.configuration.install_fixture(fixture);
        let (_, view) = app
            .state
            .open_text(
                TextBuffer::from_text(&format!("{}\nx x x", "x ".repeat(200))),
                None,
            )
            .unwrap();
        app.set_dashboard_visibility(false);
        app.reconcile_editor_windows();
        let viewport = crate::ui::active_editor_pane_viewport(&app.state, &app.ui).unwrap();
        let width = crate::ui::editor_state_content_width(
            &app.state,
            &app.state.buffers[app.state.views[view].buffer].text,
            &app.configuration.settings().editor,
            viewport.width,
        );
        let (first, end) = if wrap {
            app.state.views[view].scroll_wrap_row = 1;
            app.state.views[view].cursor = ByteOffset(width);
            (width, width * (usize::from(viewport.height) + 1))
        } else {
            app.state.views[view].scroll_column = 10;
            app.state.views[view].cursor = ByteOffset(10);
            (10, 10 + width)
        };
        app.begin_flash();
        press_keys(&mut app, &["x"]);
        let flash = app.flash.as_ref().expect("several visible matches");
        let starts = flash
            .targets
            .iter()
            .map(|(mark, _)| {
                app.state.buffers[flash.buffer]
                    .decorations
                    .range(*mark)
                    .unwrap()
                    .start
                    .get()
            })
            .collect::<Vec<_>>();
        let expected = (first..end)
            .filter(|at| at % 2 == 0 && *at != first)
            .collect::<Vec<_>>();
        assert_eq!(starts, expected, "wrap={wrap}");
        assert!(flash.targets.iter().any(|(_, label)| label.is_some()));
    }
}

#[test]
fn flash_follows_source_scrolling_and_does_not_label_clipped_inlay_anchors() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((20, 8));
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.settings.editor.wrap = false;
    fixture.settings.editor.line_numbers = crate::config::LineNumbers::Off;
    fixture.settings.ui.status_line.enabled = false;
    app.configuration.install_fixture(fixture);
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text(&"x ".repeat(100)), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.state.views[view].scroll_column = 10;
    app.state.views[view].cursor = ByteOffset(10);
    for position in [0, 18] {
        app.ui.lsp_inlay_hints.push(crate::ui::UiInlayHint {
            buffer,
            revision: app.state.buffers[buffer].text.revision(),
            position: ByteOffset(position),
            label: "hint".repeat(20),
        });
    }
    app.begin_flash();
    press_keys(&mut app, &["x"]);
    let flash = app.flash.as_ref().expect("three visible matches");
    assert_eq!(
        flash
            .targets
            .iter()
            .map(|(mark, _)| app.state.buffers[flash.buffer]
                .decorations
                .range(*mark)
                .unwrap()
                .start
                .get())
            .collect::<Vec<_>>(),
        vec![12, 14, 16]
    );
    let frame = render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        20,
        8,
    );
    for label in flash.targets.iter().filter_map(|(_, label)| *label) {
        assert!((0..8).any(|row| frame.grid.row_text(row).contains(label)));
    }
}

#[test]
fn flash_ignores_wrapped_folded_and_horizontally_clipped_text() {
    for (source, wrap, fold, scroll) in [
        (format!("{}\nz", "a".repeat(4000)), true, false, 0),
        ("a\nhidden z\na".into(), true, true, 0),
        (format!("{}z", "a".repeat(100)), false, false, 0),
        (format!("z{}", "a".repeat(100)), false, false, 10),
    ] {
        let (_app_dirs, mut app) = app();
        let (_, view) = app
            .state
            .open_text(TextBuffer::from_text(&source), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.editor.wrap = wrap;
        app.configuration.install_fixture(fixture);
        app.reconcile_editor_windows();
        if fold {
            app.ui.lsp_folds.push(UiFold {
                view,
                start_line: 0,
                end_line: 1,
                collapsed: true,
            });
        }
        app.state.views[view].scroll_column = scroll;
        app.begin_flash();
        press_keys(&mut app, &["z"]);
        assert!(app.flash.is_none());
        assert_eq!(app.state.views[view].cursor, ByteOffset(0));
        assert_eq!(app.ui.notification.as_deref(), Some("no match for z"));
    }
}

#[test]
fn flash_targets_follow_insertions_before_the_label_is_chosen() {
    let (_dirs, mut app) = app();
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("abc abc abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.begin_flash();
    press_keys(&mut app, &["a"]);
    let (mark, label) = app.flash.as_ref().unwrap().targets[0];
    let before = app.state.buffers[buffer]
        .decorations
        .range(mark)
        .unwrap()
        .start;
    let revision = app.state.buffers[buffer].text.revision();
    app.state
        .apply_transaction(
            &crate::editor::EditorTransaction::new(crate::editor::TransactionId(1)).with_operation(
                crate::editor::EditorOperation::ApplyEdits {
                    buffer,
                    edits: crate::editor::text::TextEditSet::new(
                        revision,
                        vec![crate::editor::text::TextEdit::new(
                            crate::editor::text::ByteRange::empty(ByteOffset(0)),
                            "new ",
                        )],
                    ),
                },
            ),
        )
        .unwrap();
    app.sync_flash_ui();
    assert_eq!(
        app.ui.flash.as_ref().unwrap().matches[0].start,
        ByteOffset(before.get() + 4)
    );
    press_keys(&mut app, &[&label.unwrap().to_string()]);
    assert_eq!(app.state.views[view].cursor, ByteOffset(before.get() + 4));
    assert!(app.flash.is_none());
}

#[test]
fn flash_cancels_cached_targets_after_a_reload() {
    let (_app_dirs, mut app) = app();
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("abc abc abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.begin_flash();
    press_keys(&mut app, &["a"]);
    let label = app
        .flash
        .as_ref()
        .unwrap()
        .targets
        .iter()
        .find_map(|(_, label)| *label)
        .unwrap();
    app.state.buffers[buffer].text = TextBuffer::from_text("é");
    app.state.views[view].cursor = ByteOffset(0);
    press_keys(&mut app, &[&label.to_string()]);
    assert!(app.flash.is_none());
    assert_eq!(app.state.views[view].cursor.get(), 0);
}

#[test]
fn persisted_navigation_clamps_multibyte_offsets() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("mark.txt");
    fs::write(&path, "éx").unwrap();
    app.navigate_persisted_location(crate::persistence::PersistedLocation {
        path,
        byte: 1,
        anchor: 2,
        scroll_line: 0,
        scroll_column: 0,
    });
    wait_vfs(&mut app);
    let view = &app.state.views[app.state.active_view().unwrap()];
    assert_eq!(view.cursor.get(), 0);
    assert_eq!(view.selection.unwrap().anchor.get(), 2);
    assert_eq!(view.selection.unwrap().head, view.cursor);
}

#[test]
fn cursor_only_commands_do_not_enter_the_change_list() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("changes.txt");
    app.state
        .open_text(TextBuffer::from_text("abc def"), Some(path))
        .unwrap();
    app.set_dashboard_visibility(false);
    press_keys(&mut app, &["l", "w", "h"]);
    assert!(app.persistence.changes().is_empty());
    press_keys(&mut app, &["x"]);
    assert_eq!(app.persistence.changes().len(), 1);
}

#[test]
fn count_errors_return_operator_pending_views_to_normal() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    press_keys(
        &mut app,
        &["1", "0", "0", "1", "d", "1", "0", "0", "0", "w"],
    );
    assert_eq!(
        app.state.views[view].mode,
        crate::editor::EditorMode::Normal
    );
}

#[test]
fn count_only_and_empty_pattern_substitutions_preserve_their_meaning() {
    let (_app_dirs, mut app) = app();
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("cat cat\nkeep\n"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.run_substitute("%", "cat", "dog", "gn");
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "cat cat\nkeep\n"
    );
    app.run_substitute("%", "cat", "dog", "z");
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "cat cat\nkeep\n"
    );
    app.ui.search_query = Some("cat".into());
    app.run_substitute("%", "", "dog", "g");
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "dog dog\nkeep\n"
    );
    app.state.views[view].cursor = ByteOffset(8);
    app.run_global_delete("", "keep", false);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "dog dog\n");
    assert!(app.state.views[view].cursor.get() < app.state.buffers[buffer].text.len_bytes());
}

#[test]
fn recorded_dot_and_insert_delete_and_yank_replay_the_expected_change() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("abcdef"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    press_keys(&mut app, &["x", "q", "a", ".", "q", "@", "a"]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "def");
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("abc"), None)
        .unwrap();
    press_keys(&mut app, &["i", "X", "delete", "escape", "."]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "Xbc");
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("abc"), None)
        .unwrap();
    press_keys(&mut app, &["x", "y", "y", "."]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "c");
}

#[test]
fn macro_replay_stops_before_an_edit_after_a_failed_search_effect() {
    let (_app_dirs, mut app) = app();
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.settings.search.wrap = false;
    app.configuration.install_fixture(fixture);
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("cat cat tail"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.ui.search_query = Some("cat".into());
    press_keys(&mut app, &["q", "a", "n", "x", "q"]);
    app.state.views[view].cursor = ByteOffset(7);
    let before = app.state.buffers[buffer].text.to_string();
    app.replay_macro('a', 1);
    assert_eq!(app.state.buffers[buffer].text.to_string(), before);
    assert!(app.ui.notification_error);
}

#[test]
fn macro_replay_waits_for_clipboard_put_before_deleting() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("recording"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.state.registers.set(
        '"',
        crate::editor::RegisterValue {
            text: "old".into(),
            shape: crate::editor::RegisterShape::Character,
        },
        true,
        false,
    );
    press_keys(&mut app, &["q", "a", "p", "h", "x", "q"]);
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("abc"), None)
        .unwrap();
    let mut fixture = app.configuration.fixture_snapshot();
    fixture.settings.editor.registers.clipboard = crate::config::ClipboardSync::Unnamedplus;
    app.configuration.install_fixture(fixture);
    let reply = app.clipboard.fixture_read();
    app.replay_macro('a', 1);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "abc");
    assert_eq!(app.input.replay_depth(), 1);
    app.process_clipboard_paste();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "abc");
    reply.send(Some("XY".into())).unwrap();
    app.process_clipboard_paste();
    assert_eq!(app.state.buffers[buffer].text.to_string(), "aYbc");
    assert_eq!(app.state.registers.get('"').unwrap().text, "X");
    assert_eq!(app.input.replay_depth(), 0);
    assert!(app.macro_replays.is_empty());
}

#[test]
fn macro_replay_stops_before_an_edit_after_a_failed_find() {
    let (_app_dirs, mut app) = app();
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("az\nkeep"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    for key in "qafzxq".chars() {
        app.handle_configured_key(KeyEvent::character(key));
    }
    app.state.views[view].cursor = ByteOffset(3);
    let before = app.state.buffers[buffer].text.to_string();
    app.replay_macro('a', 1);
    assert_eq!(app.state.buffers[buffer].text.to_string(), before);
    assert!(app.ui.notification_error);
}

#[test]
fn star_and_hash_repeat_only_whole_identifiers() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("foo foobar foo"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.search_word_under_cursor(true, 1);
    assert_eq!(app.state.views[view].cursor, ByteOffset(11));
    app.perform_search(true, 1);
    assert_eq!(app.state.views[view].cursor, ByteOffset(0));
    app.search_word_under_cursor(false, 1);
    assert_eq!(app.state.views[view].cursor, ByteOffset(11));
}

#[test]
fn empty_substitution_selection_positions_the_cursor_without_selecting_text() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("abc"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    assert!(app.select_text(view, 0, 2));
    assert!(app.select_text(view, 3, 3));
    assert_eq!(app.state.views[view].cursor, ByteOffset(3));
    assert_eq!(
        app.state.views[view].mode,
        crate::editor::EditorMode::Normal
    );
    assert!(app.state.views[view].selection.is_none());
}

#[test]
fn word_search_keeps_combining_marks_and_search_selection_keeps_word_boundaries() {
    let (_app_dirs, mut app) = app();
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("e\u{301} e e\u{301}"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    assert!(app.search_word_under_cursor(true, 1));
    assert_eq!(app.ui.search_query.as_deref(), Some("e\u{301}"));
    assert_eq!(app.state.views[view].cursor, ByteOffset(6));

    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("foo foobar foo"), None)
        .unwrap();
    app.state.views[view].cursor = ByteOffset(0);
    app.search_word_under_cursor(true, 1);
    app.state.views[view].cursor = ByteOffset(5);
    assert!(app.select_next_search_match(1));
    let selection = app.state.views[view].selection.as_ref().unwrap();
    assert_eq!(selection.anchor, ByteOffset(11));
    assert_eq!(selection.head, ByteOffset(13));
    app.set_mode(crate::editor::EditorMode::Normal);
    app.state.views[view].cursor = ByteOffset(5);
    press_keys(&mut app, &["c", "g", "n", "X", "escape"]);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "foo foobar X");
}

#[test]
fn operator_arrow_motions_are_linewise_like_j_and_k() {
    for (motion, cursor) in [("down", 1), ("up", 5)] {
        let (_app_dirs, mut app) = app();
        let (buffer, view) = app
            .state
            .open_text(
                crate::editor::text::TextBuffer::from_text("abc\ndef\nghi\n"),
                None,
            )
            .unwrap();
        app.set_dashboard_visibility(false);
        app.state.views[view].cursor = crate::editor::text::ByteOffset(cursor);
        app.handle_configured_key(KeyEvent::character('d'));
        app.handle_configured_key(motion.parse().unwrap());
        assert_eq!(
            app.state.buffers[buffer].text.to_string(),
            "ghi\n",
            "{motion}"
        );
    }
}

#[test]
fn a_new_primary_startup_file_stays_selected_after_existing_files_load() {
    let (_app_dirs, mut app) = app();
    let primary = app.state.workspace_root.join("new.rs");
    let additional = app.state.workspace_root.join("existing.rs");
    fs::write(&additional, "existing").unwrap();
    let startup = StartupTarget::File {
        workspace_root: app.state.workspace_root.clone(),
        path: primary.clone(),
        additional_files: vec![additional],
    };
    app.documents = crate::app::documents::Documents::new(
        crate::files::VfsWorker::start(2),
        &startup,
        app.state.window_version(),
        app.workspace.root_revision(),
        2,
    );
    app.edit_path(primary.clone());
    let new_buffer = app.state.active_buffer_id().unwrap();
    app.queue_startup_reads();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers.len(), 2);
    assert_eq!(app.state.active_buffer_id(), Some(new_buffer));
    assert_eq!(app.state.buffers[new_buffer].path.as_ref(), Some(&primary));
}

#[test]
fn startup_reads_load_without_changing_focus_after_user_navigation() {
    let (_app_dirs, mut app) = app();
    let primary = app.state.workspace_root.join("primary.txt");
    let additional = app.state.workspace_root.join("additional.txt");
    fs::write(&primary, "primary").unwrap();
    fs::write(&additional, "additional").unwrap();
    app.documents.enqueue_startup_fixture(primary, true);
    app.documents.enqueue_startup_fixture(additional, false);
    app.queue_startup_reads();
    app.edit_path(app.state.workspace_root.join("chosen.txt"));
    let chosen = app.state.active_buffer_id();
    let focus = app.window_session.graph().focused();
    wait_vfs(&mut app);
    assert_eq!(app.state.buffers.len(), 3);
    assert_eq!(app.state.active_buffer_id(), chosen);
    assert_eq!(app.window_session.graph().focused(), focus);
    assert!(!app.documents.has_startup_work());
}
