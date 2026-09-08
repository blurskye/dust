use super::driver::TestSession;
use crate::config::LoadedConfig;
use std::time::Duration;

fn bindings(entries: &[(&str, &str)]) -> LoadedConfig {
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.ui.key_hints.delay_ms = 0;
    for (index, (keys, command)) in entries.iter().enumerate() {
        loaded.keymaps.entries.push(crate::config::KeymapEntry {
            id: format!("user.test.restricted-driver-{index}"),
            context: Some("dashboard".into()),
            mode: String::new(),
            keys: Some((*keys).into()),
            command: Some((*command).into()),
            arguments: Default::default(),
            unmap: false,
            disable: false,
            pass_through: false,
        });
    }
    loaded
}

#[test]
fn stable_clock_pure_prefix_rejection_replacement_and_focus_loss_use_the_real_constructor() {
    let loaded = bindings(&[("z z", "health.open")]);
    let mut session = TestSession::new(loaded.clone());
    let now = session.time();
    for _ in 0..100 {
        assert_eq!(session.time(), now);
    }
    session.key("z");
    session.advance_input(Duration::from_secs(120));
    assert_eq!(session.prefix(), Some("z"));
    assert!(session.projection().key_hints.is_some());
    let mut invalid = loaded.clone();
    invalid.config.input.count_limit = 0;
    assert!(session.apply_candidate(invalid).is_err());
    assert_eq!(session.prefix(), Some("z"));
    session.apply_candidate(loaded.clone()).unwrap();
    assert_eq!(session.prefix(), None);
    assert!(session.projection().key_hints.is_none());
    session.key("z");
    session.focus_lost();
    assert_eq!(session.prefix(), None);
    // A second session borrows equivalent immutable bindings, not the first sequence.
    let mut independent = TestSession::new(loaded);
    session.key("z");
    independent.key("z");
    assert_eq!(session.prefix(), Some("z"));
    assert_eq!(independent.prefix(), Some("z"));
}

#[test]
fn ambiguous_exact_mapping_executes_only_at_its_injected_deadline() {
    let loaded = bindings(&[("x", "file.new"), ("x y", "health.open")]);
    let timeout = loaded.config.input.mapping_timeout_ms;
    let mut session = TestSession::new(loaded);
    session.key("x");
    session.advance_input(Duration::from_millis(timeout - 1));
    assert_eq!(session.prefix(), Some("x"));
    assert!(session.projection().dashboard);
    session.advance_input(Duration::from_millis(1));
    assert_eq!(session.prefix(), None);
    assert!(!session.projection().dashboard);
}

#[test]
fn documentation_unrelated_key_replays_and_focus_preserves_the_regular_command_prompt() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    session.documentation(
        (0..40)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    session.key("j");
    assert_eq!(
        session
            .projection()
            .documentation_popup
            .as_ref()
            .unwrap()
            .scroll,
        4
    );
    session.key(":");
    assert!(session.projection().documentation_popup.is_none());
    assert!(session.projection().prompt.is_some());
    for key in ["h", "e", "a"] {
        session.key(key);
    }
    let prompt = session.projection().prompt.as_ref().unwrap();
    assert_eq!(prompt.input, "hea");
    assert!(prompt.completions.iter().any(|item| item.label == "health"));
    let mut candidate = crate::config::load_bundled_config();
    candidate
        .config
        .command_labels
        .insert("health.open".into(), "OWNER_UPDATED_LABEL".into());
    session.apply_candidate(candidate).unwrap();
    let prompt = session.projection().prompt.as_ref().unwrap();
    assert_eq!(prompt.input, "hea");
    assert!(
        prompt
            .completions
            .iter()
            .any(|item| item.label == "health" && item.detail == "OWNER_UPDATED_LABEL")
    );
    session.focus_lost();
    assert_eq!(session.projection().prompt.as_ref().unwrap().input, "hea");
    session.key("escape");
    assert!(session.projection().prompt.is_none());
}

#[test]
fn typed_manager_keys_close_each_layer_and_then_quit() {
    let mut session = TestSession::new(crate::config::load_bundled_config());
    for key in [":", "l", "s", "p", "enter"] {
        session.key(key);
    }
    assert!(session.projection().tool_menu.is_some());
    session.key("p");
    assert!(
        session
            .projection()
            .tool_menu
            .as_ref()
            .unwrap()
            .runtime_picker
            .is_some()
    );
    session.key("escape");
    assert!(
        session
            .projection()
            .tool_menu
            .as_ref()
            .unwrap()
            .runtime_picker
            .is_none()
    );
    session.key("escape");
    assert!(session.projection().tool_menu.is_none());
    for key in [":", "q", "enter"] {
        session.key(key);
    }
    assert!(session.quit_requested());
}

#[test]
fn explorer_leader_shortcut_toggles_from_dashboard_explorer_and_editor() {
    for leader in ["space", ","] {
        let mut loaded = crate::config::load_bundled_config();
        loaded.config.input.keymaps.leader = leader.into();
        let mut session = TestSession::new(loaded);
        let toggle = |session: &mut TestSession| {
            for key in [leader, "f", "e"] {
                session.key(key);
            }
        };
        assert_eq!(session.context(), "dashboard");
        toggle(&mut session);
        assert!(session.projection().explorer.is_some());
        assert_eq!(session.context(), "explorer");
        toggle(&mut session);
        assert!(
            session.projection().explorer.is_none(),
            "second {leader} f e must close explorer"
        );
        assert_eq!(session.context(), "dashboard");
        toggle(&mut session);
        session.key("escape");
        assert!(session.projection().explorer.is_none());
        session.new_buffer();
        toggle(&mut session);
        assert!(session.projection().explorer.is_some());
        session.focus_explorer(false);
        assert_eq!(session.context(), "editor.normal");
        toggle(&mut session);
        assert!(
            session.projection().explorer.is_none(),
            "visible explorer also toggles off from the editor"
        );
        toggle(&mut session);
        assert_eq!(session.context(), "explorer");
        toggle(&mut session);
        assert!(session.projection().explorer.is_none());
        assert_eq!(session.context(), "editor.normal");
    }
}
