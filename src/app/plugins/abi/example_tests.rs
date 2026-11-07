//! The copyable example guests loaded into the real editor and driven through its keys.

use super::*;
use crate::editor::text::TextBuffer;
use sha2::{Digest, Sha256};

fn example(names: &[(&str, &str)], text: &str) -> (Vec<tempfile::TempDir>, App) {
    let (dirs, mut app) = crate::app::tests::app_with_plugin_queue(32);
    let mut roots = vec![dirs.0, dirs.1];
    let path = app.state.workspace_root.join("example.rs");
    app.state
        .open_text(TextBuffer::from_text(text), Some(path))
        .unwrap();
    app.state.policy.autopairs = false;
    crate::app::tests::prepare_trust_identity(&mut app);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    for (name, owner) in names {
        let root = tempfile::tempdir().unwrap();
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/plugins")
            .join(name);
        let core = std::fs::read(source.join("guest.core.wasm")).unwrap();
        let component = wit_component::ComponentEncoder::default()
            .module(&core)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap();
        std::fs::write(root.path().join("guest.wasm"), &component).unwrap();
        let mut manifest: crate::plugins::PluginManifest =
            toml::from_str(&std::fs::read_to_string(source.join("manifest.toml")).unwrap())
                .unwrap();
        manifest.id = (*owner).into();
        manifest.component_sha256 = format!("{:x}", Sha256::digest(&component));
        crate::app::tests::write_plugin_manifest(root.path(), &manifest);
        let load = app
            .extension
            .index_from_store(
                (*owner).into(),
                root.path().into(),
                "manifest.toml".into(),
                "guest.wasm".into(),
                app.plugin_limits(),
            )
            .unwrap();
        wait(&mut app, |app| app.extension.is_active_load(owner, load));
        roots.push(root);
    }
    (roots, app)
}
fn wait(app: &mut App, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        app.process_syntax();
        app.process_plugin_events();
        app.process_save_work();
        if done(app) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out: {:?}",
            app.ui.notification
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn settle(app: &mut App) {
    wait(app, |app| {
        !app.input.command_pending() && app.extension.abi.waiting.is_empty()
    });
}
fn command(app: &mut App, command: &str) {
    assert!(app.invoke_checked(CommandInvocation::new(command, InvocationSource::Test).unwrap()));
    settle(app);
}
fn keys(app: &mut App, keys: &str) {
    for key in keys.split_whitespace() {
        app.handle_configured_key(key.parse().unwrap());
    }
    settle(app);
}
fn contents(app: &App) -> String {
    app.state.buffers[app.state.active_buffer_id().unwrap()]
        .text
        .to_string()
}
fn replace(app: &mut App, text: &str) {
    keys(app, "g g V G d i");
    for c in text.chars() {
        app.handle_configured_key(crate::editor::input::KeyEvent::character(c));
    }
    keys(app, "escape");
}

#[test]
fn example_autopairs_types_skips_erases_undoes_repeats_and_records_macros() {
    let (_roots, mut app) = example(&[("autopairs", "autopairs")], "");
    command(&mut app, "plugin.autopairs.enable");
    keys(&mut app, "i ( x ) escape");
    assert_eq!(contents(&app), "(x)");
    keys(&mut app, "u");
    assert_eq!(contents(&app), "");
    keys(&mut app, ".");
    assert_eq!(contents(&app), "(x)");
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()].cursor.0,
        2
    );
    keys(&mut app, "u i [ backspace escape");
    assert_eq!(contents(&app), "");
    keys(&mut app, "i \" λ \" escape");
    assert_eq!(contents(&app), "\"λ\"");
    keys(&mut app, "u i ' backspace escape");
    assert_eq!(contents(&app), "");
    keys(&mut app, "q a A { z } escape q");
    assert_eq!(contents(&app), "{z}");
    keys(&mut app, "@ a");
    assert_eq!(contents(&app), "{z}{z}");
}

#[test]
fn example_surround_function_objects_keep_counts_registers_undo_dot_and_macros() {
    let source = "fn outer() {fn inner() {word}}";
    let (_roots, mut app) = example(&[("surround", "surround")], source);
    let buffer = app.state.active_buffer_id().unwrap();
    wait(&mut app, |app| app.syntax.result(buffer).is_some());
    command(&mut app, "plugin.surround.enable");
    wait(&mut app, |app| app.extension.abi.marks.len() == 2);
    keys(&mut app, "2 4 l");
    keys(&mut app, "\" a y i f");
    assert!(
        app.state.registers.get('a').is_some(),
        "{:?}; marks: {:?}",
        app.ui.notification.clone(),
        app.plugin_delivery("surround").unwrap().marks
    );
    assert_eq!(app.state.registers.get('a').unwrap().text, "word");
    keys(&mut app, "2 y a f");
    assert_eq!(app.state.registers.get('"').unwrap().text, source);
    keys(&mut app, "g s a i f (");
    assert_eq!(contents(&app), "fn outer() {fn inner() {(word)}}");
    keys(&mut app, "u .");
    assert_eq!(contents(&app), "fn outer() {fn inner() {(word)}}");
    keys(&mut app, "u");
    assert_eq!(contents(&app), source, "{:?}", app.ui.notification);
    keys(&mut app, "q a g s a i f [ q");
    assert_eq!(
        contents(&app),
        "fn outer() {fn inner() {[word]}}",
        "{:?}",
        app.ui.notification
    );
    keys(&mut app, "u @ a");
    assert_eq!(contents(&app), "fn outer() {fn inner() {[word]}}");
    keys(&mut app, "u d i f");
    assert_eq!(contents(&app), "fn outer() {fn inner() {}}");
    keys(&mut app, "u");
    assert_eq!(contents(&app), source);
    replace(&mut app, &format!("{source}\nfn next() {{fresh}}"));
    wait(&mut app, |app| {
        app.extension
            .abi
            .marks
            .iter()
            .any(|((_, _, name), marks)| name == "outer" && marks.len() == 3)
    });
    keys(&mut app, "G 0 y i f");
    assert_eq!(app.state.registers.get('"').unwrap().text, "fresh");
}

#[test]
fn example_oil_confirms_rename_delete_create_and_keeps_cancelled_changes_dirty() {
    let (_roots, mut app) = example(&[("oil", "oil")], "");
    let root = app.state.workspace_root.clone();
    std::fs::write(root.join("old.txt"), "kept").unwrap();
    std::fs::write(root.join("delete.txt"), "gone").unwrap();
    command(&mut app, "plugin.oil.open");
    wait(&mut app, |app| contents(app).contains("old.txt"));
    settle(&mut app);
    let old_line = contents(&app)
        .lines()
        .find(|line| line.ends_with("old.txt"))
        .unwrap()
        .replace("old.txt", "renamed.txt");
    replace(&mut app, &format!("{old_line}\ncreated.txt\n"));
    let draft = contents(&app);
    keys(&mut app, "u");
    assert_ne!(contents(&app), draft);
    keys(&mut app, "ctrl-r");
    assert_eq!(contents(&app), draft);
    keys(&mut app, "ctrl-s");
    wait(&mut app, |app| app.ui.picker.is_some());
    keys(&mut app, "escape");
    assert!(root.join("old.txt").is_file());
    assert!(app.state.buffers[app.state.active_buffer_id().unwrap()].dirty);
    keys(&mut app, "ctrl-s");
    wait(&mut app, |app| app.ui.picker.is_some());
    keys(&mut app, "down enter");
    wait(&mut app, |app| {
        !app.state.buffers[app.state.active_buffer_id().unwrap()].dirty
    });
    assert_eq!(
        std::fs::read_to_string(root.join("renamed.txt")).unwrap(),
        "kept"
    );
    assert!(!root.join("delete.txt").exists());
    assert!(!root.join("old.txt").exists());
    assert_eq!(std::fs::read(root.join("created.txt")).unwrap(), b"");
}

#[test]
fn example_completion_uses_native_menu_inline_acceptance_and_undo() {
    let (_roots, mut app) = example(&[("completion", "completion")], "alphabet alpine\nal");
    command(&mut app, "plugin.completion.enable");
    keys(&mut app, "G A");
    wait(&mut app, |app| app.language.completion_items().len() == 2);
    assert_eq!(
        app.language.inline_completion(&app.state).as_deref(),
        Some("phabet")
    );
    keys(&mut app, "tab escape");
    assert_eq!(contents(&app), "alphabet alpine\nalphabet");
    keys(&mut app, "u");
    assert_eq!(contents(&app), "alphabet alpine\nal");
}

#[test]
fn example_two_wrappers_are_ordered_and_hold_typing_until_both_finish() {
    let (_roots, mut app) = example(&[("wrappers", "wrappersa"), ("wrappers", "wrappersb")], "");
    command(&mut app, "plugin.wrappersb.enable");
    command(&mut app, "plugin.wrappersa.enable");
    app.handle_configured_key("i".parse().unwrap());
    app.handle_configured_key("x".parse().unwrap());
    assert!(app.input.command_pending());
    app.handle_configured_key("y".parse().unwrap());
    assert_eq!(contents(&app), "");
    settle(&mut app);
    assert_eq!(contents(&app), "xy");
    let history: Vec<_> = app
        .notices
        .history()
        .map(|(_, message)| message.to_owned())
        .filter(|s| s.starts_with("wrappers"))
        .collect();
    assert_eq!(
        history,
        [
            "wrappersa:before",
            "wrappersb:before",
            "wrappersb:after",
            "wrappersa:after"
        ]
        .repeat(2)
    );
    keys(&mut app, "escape u .");
    assert_eq!(contents(&app), "xy");
}

#[test]
fn example_named_mode_moves_with_its_layer_and_leaves_without_leaking_chrome() {
    let (_roots, mut app) = example(&[("mode", "mode")], "abc");
    command(&mut app, "plugin.mode.enter");
    assert_eq!(app.editor_input_mode().unwrap().label(), "REVIEW");
    assert!(
        app.ui
            .chrome
            .status_text
            .values()
            .any(|(text, _)| text.contains("h/l"))
    );
    keys(&mut app, "l l h q");
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()].cursor.0,
        1
    );
    assert_ne!(app.editor_input_mode().unwrap().label(), "REVIEW");
    assert!(app.ui.chrome.status_text.is_empty());
    keys(&mut app, "i X escape u");
    assert_eq!(contents(&app), "abc");
}
