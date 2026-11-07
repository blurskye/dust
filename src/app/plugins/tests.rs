//! Plugins behavior and application integration tests.

use crate::app::App;
use crate::app::AppEffect;
use crate::app::picker::tests::palette_row_for;
use crate::app::tests::*;
use crate::editor::CommandSource;
use crate::editor::CommandValue;
use crate::editor::InvocationSource;
use crate::editor::input::InputContextLayer;
use crate::editor::text::ByteOffset;
use crate::editor::text::TextBuffer;
use crate::persistence::PersistedSession;
use crate::persistence::PersistedState;
use crate::persistence::PersistenceWorker;
use crate::plugins::PluginLoadId;
use crate::ui::ContentTabKind;
use crate::ui::PickerSurfaceKind;
use crate::ui::render::overlays::SCRATCH_ICON;
use crate::ui::render::overlays::VIRTUAL_ICON;
use crate::windows::DockEdge;
use crate::windows::WindowPlacement;
use crate::windows::WindowRect;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::tempdir;

#[test]
fn plugin_confirmation_is_typed_one_shot_and_source_owned() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);

    let request = |token| crate::plugins::PluginConfirmationRequest {
        token,
        title: "Apply plugin state".into(),
        message: "Change the custom retained view?".into(),
        default_answer: crate::plugins::PluginConfirmationAnswer::No,
    };
    let invoke = |app: &mut App| {
        app.process_effect(AppEffect::PluginExecute {
            plugin: "demo".into(),
            command: "run".into(),
            arguments: Vec::new(),
        });
    };

    invoke(&mut app);
    let initial = fixture.take();
    fixture.reply_confirmation_request(&initial, request(7));
    app.process_plugin_events();
    let prompt = app.input.prompt().expect("plugin confirmation");
    assert_eq!(prompt.kind, crate::ui::PromptKind::PluginConfirmation);
    let confirmation = prompt.confirmation.as_ref().expect("typed choice");
    assert!(confirmation.title.starts_with("Plugin demo ·"));
    assert_eq!(confirmation.selected, crate::ui::ConfirmationChoice::No);

    app.handle_configured_key("enter".parse().unwrap());
    assert!(app.input.prompt().is_none());
    let no = fixture.take();
    assert!(!no.confirmation_available());
    assert_eq!(
        no.confirmation_results(),
        [crate::plugins::PluginConfirmationResult {
            request: 7,
            outcome: crate::plugins::PluginConfirmationOutcome::Answered(
                crate::plugins::PluginConfirmationAnswer::No,
            ),
        }]
    );
    let count = app.extension.confirmation_count();
    app.submit_prompt();
    assert_eq!(app.extension.confirmation_count(), count);
    fixture.reply(&no, Vec::new());
    app.process_plugin_events();
    assert_eq!(app.extension.confirmation_count(), 0);

    invoke(&mut app);
    let initial = fixture.take();
    fixture.reply_confirmation_request(&initial, request(8));
    app.process_plugin_events();
    // y answers at once, without Enter.
    app.handle_configured_key("y".parse().unwrap());
    assert!(app.input.prompt().is_none());
    let yes = fixture.take();
    assert!(matches!(
        yes.confirmation_results()[0].outcome,
        crate::plugins::PluginConfirmationOutcome::Answered(
            crate::plugins::PluginConfirmationAnswer::Yes
        )
    ));
    fixture.reply(&yes, Vec::new());
    app.process_plugin_events();

    invoke(&mut app);
    let initial = fixture.take();
    fixture.reply_confirmation_request(&initial, request(9));
    app.process_plugin_events();
    app.handle_configured_key("escape".parse().unwrap());
    app.process_plugin_events();
    let cancelled = fixture.take();
    assert!(matches!(
        cancelled.confirmation_results()[0].outcome,
        crate::plugins::PluginConfirmationOutcome::Answered(
            crate::plugins::PluginConfirmationAnswer::Cancelled
        )
    ));
    fixture.reply(&cancelled, Vec::new());
    app.process_plugin_events();

    invoke(&mut app);
    let initial = fixture.take();
    fixture.reply_confirmation_request(&initial, request(10));
    app.process_plugin_events();
    assert!(app.input.prompt().is_some());
    let accepted = app.configuration.fixture_snapshot();
    app.configuration.install_fixture(accepted);
    app.process_plugin_events();
    assert!(app.input.prompt().is_none());
    let invalidated = fixture.take();
    assert!(matches!(
        invalidated.confirmation_results()[0].outcome,
        crate::plugins::PluginConfirmationOutcome::Failed(
            crate::plugins::PluginConfirmationError {
                code: crate::plugins::PluginConfirmationErrorCode::StaleSource,
                ..
            }
        )
    ));
    fixture.reply(&invalidated, Vec::new());
    app.process_plugin_events();
    assert_eq!(app.extension.confirmation_count(), 0);

    invoke(&mut app);
    let initial = fixture.take();
    fixture.reply_confirmation_request(&initial, request(11));
    app.process_plugin_events();
    assert!(app.input.prompt().is_some());
    app.process_effect(AppEffect::PluginDisable("demo".into()));
    assert!(app.input.prompt().is_none());
    assert_eq!(app.extension.confirmation_count(), 0);
}

#[test]
fn hidden_confirmation_suffix_cannot_be_submitted() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    fixture.reply_confirmation_request(
        &invocation,
        crate::plugins::PluginConfirmationRequest {
            token: 40,
            title: "Review all".into(),
            message: "target  界/".repeat(400),
            default_answer: crate::plugins::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    assert!(app.input.prompt().is_some());
    app.handle_configured_key("left".parse().unwrap());
    app.handle_configured_key("enter".parse().unwrap());
    assert!(app.input.prompt().is_some());
    assert_eq!(app.extension.confirmation_count(), 1);
}

#[test]
fn selected_yes_refuses_one_row_and_one_column_visibility_boundaries() {
    for (width, height, token) in [(100, 4, 44), (1, 10, 45)] {
        let (_app_dirs, mut app) = app();
        app.state
            .open_text(TextBuffer::from_text("x"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.handle_event(crate::ui::UiEvent::Resize(width, height));
        app.reconcile_editor_windows();
        let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
        app.extension.fixture_manager(manager);
        app.process_effect(AppEffect::PluginExecute {
            plugin: "demo".into(),
            command: "run".into(),
            arguments: Vec::new(),
        });
        let invocation = fixture.take();
        fixture.reply_confirmation_request(
            &invocation,
            crate::plugins::PluginConfirmationRequest {
                token,
                title: "Boundary".into(),
                message: "/tmp/a".into(),
                default_answer: crate::plugins::PluginConfirmationAnswer::No,
            },
        );
        app.process_plugin_events();
        if (width, height) == (100, 4) {
            let protected = app
                .compositor_projection()
                .unwrap()
                .protected_regions
                .first()
                .copied()
                .expect("protected confirmation bounds");
            assert_eq!(protected.height, 2);
            let frame = crate::ui::render(
                &app.state,
                &app.ui,
                app.configuration.settings(),
                app.configuration.dashboard(),
                width,
                height,
            );
            let screen = (0..height)
                .map(|row| frame.grid.row_text(row))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!screen.contains("[ Yes ]"));
        }
        app.handle_configured_key("left".parse().unwrap());
        app.handle_configured_key("enter".parse().unwrap());
        assert!(
            app.input.prompt().is_some(),
            "{width}x{height} submitted Yes"
        );
        app.handle_configured_key("escape".parse().unwrap());
        assert!(
            app.input.prompt().is_none(),
            "Escape failed at {width}x{height}"
        );
    }
}

#[test]
fn docked_layout_render_and_protection_share_confirmation_bounds() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("x"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.handle_event(crate::ui::UiEvent::Resize(100, 20));
    app.process_effect(AppEffect::ExplorerOpen);
    app.reconcile_editor_windows();
    assert!(
        app.compositor_projection().unwrap().workspace_bounds.width < 100,
        "explorer must consume horizontal workspace"
    );
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    fixture.reply_confirmation_request(
        &invocation,
        crate::plugins::PluginConfirmationRequest {
            token: 46,
            title: "Docked".into(),
            message: "/tmp/a".into(),
            default_answer: crate::plugins::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    let prompt = app.input.prompt().unwrap();
    let dimensions = crate::ui::prompt_surface_dimensions(
        &app.state,
        &app.ui,
        app.window_session.width(),
        app.window_session.height(),
    );
    let expected = crate::ui::prompt_geometry_for_state(
        dimensions.0,
        dimensions.1,
        app.configuration.settings(),
        prompt,
    );
    let protected = app
        .compositor_projection()
        .unwrap()
        .protected_regions
        .first()
        .copied()
        .expect("protected prompt");
    assert_eq!(
        protected,
        crate::windows::WindowRect {
            x: expected.left,
            y: expected.top,
            width: expected.width,
            height: expected.height,
        }
    );
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        100,
        20,
    );
    assert_eq!(
        frame.grid.get(expected.left, expected.top).unwrap().style,
        crate::ui::Style::Border
    );
}

#[test]
fn delayed_confirmation_cannot_replace_a_newer_native_interaction() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(8);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    app.open_prompt(crate::ui::PromptKind::Command, "newer".into());
    fixture.reply_confirmation_request(
        &invocation,
        crate::plugins::PluginConfirmationRequest {
            token: 41,
            title: "Delayed".into(),
            message: "Must not steal focus".into(),
            default_answer: crate::plugins::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    assert_eq!(
        app.input.prompt().unwrap().kind,
        crate::ui::PromptKind::Command
    );
    assert_eq!(app.input.prompt().unwrap().input, "newer");
    app.process_plugin_events();
    let invalidated = fixture.take();
    assert!(matches!(
        invalidated.confirmation_results()[0].outcome,
        crate::plugins::PluginConfirmationOutcome::Failed(
            crate::plugins::PluginConfirmationError {
                code: crate::plugins::PluginConfirmationErrorCode::StaleSource,
                ..
            }
        )
    ));
}

#[test]
fn delayed_initial_confirmation_reply_gets_terminal_failure_after_config_reload() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(8);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    let accepted = app.configuration.fixture_snapshot();
    app.configuration.install_fixture(accepted);
    app.process_plugin_events();
    fixture.reply_confirmation_request(
        &invocation,
        crate::plugins::PluginConfirmationRequest {
            token: 50,
            title: "Delayed".into(),
            message: "Original configuration was replaced".into(),
            default_answer: crate::plugins::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    assert!(app.input.prompt().is_none());
    app.process_plugin_events();
    let invalidated = fixture.take();
    assert_eq!(invalidated.confirmation_results()[0].request, 50);
    assert!(matches!(
        invalidated.confirmation_results()[0].outcome,
        crate::plugins::PluginConfirmationOutcome::Failed(
            crate::plugins::PluginConfirmationError {
                code: crate::plugins::PluginConfirmationErrorCode::StaleSource,
                ..
            }
        )
    ));
}

#[test]
fn accepted_confirmation_cannot_wait_past_a_newer_prompt_capture() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(8);
    app.extension.fixture_manager(manager);
    for _ in 0..2 {
        app.process_effect(AppEffect::PluginExecute {
            plugin: "demo".into(),
            command: "run".into(),
            arguments: Vec::new(),
        });
    }
    let first = fixture.take();
    let second = fixture.take();
    let request = |token| crate::plugins::PluginConfirmationRequest {
        token,
        title: format!("Request {token}"),
        message: "Do not outlive a newer capture".into(),
        default_answer: crate::plugins::PluginConfirmationAnswer::No,
    };
    fixture.reply_confirmation_request(&first, request(51));
    app.process_plugin_events();
    assert_eq!(
        app.input.prompt().unwrap().kind,
        crate::ui::PromptKind::PluginConfirmation
    );
    fixture.reply_confirmation_request(&second, request(52));
    app.process_plugin_events();
    assert_eq!(
        app.input.prompt().unwrap().kind,
        crate::ui::PromptKind::PluginConfirmation
    );
    app.process_plugin_events();
    let invalidated = fixture.take();
    assert_eq!(invalidated.confirmation_results()[0].request, 52);
    assert!(matches!(
        invalidated.confirmation_results()[0].outcome,
        crate::plugins::PluginConfirmationOutcome::Failed(
            crate::plugins::PluginConfirmationError {
                code: crate::plugins::PluginConfirmationErrorCode::StaleSource,
                ..
            }
        )
    ));
}

#[test]
fn tool_menu_blocks_background_and_invalidates_delayed_confirmation() {
    for delayed in [false, true] {
        let (_app_dirs, mut app) = app();
        let (manager, fixture) = crate::plugins::ExecutionFixture::new(8);
        app.extension.fixture_manager(manager);
        let invoke = |app: &mut App| {
            app.process_effect(AppEffect::PluginExecute {
                plugin: "demo".into(),
                command: "run".into(),
                arguments: Vec::new(),
            });
        };
        let invocation = if delayed {
            invoke(&mut app);
            let invocation = fixture.take();
            app.open_tool_menu("lsp".into());
            invocation
        } else {
            app.open_tool_menu("lsp".into());
            invoke(&mut app);
            fixture.take()
        };
        assert!(app.ui.tool_menu.is_some());
        fixture.reply_confirmation_request(
            &invocation,
            crate::plugins::PluginConfirmationRequest {
                token: 70 + u64::from(delayed),
                title: "Background".into(),
                message: "Must not overlay the tool manager".into(),
                default_answer: crate::plugins::PluginConfirmationAnswer::No,
            },
        );
        app.process_plugin_events();
        assert!(app.input.prompt().is_none());
        assert!(app.ui.tool_menu.is_some());
        app.process_plugin_events();
        let invalidated = fixture.take();
        assert!(matches!(
            invalidated.confirmation_results()[0].outcome,
            crate::plugins::PluginConfirmationOutcome::Failed(
                crate::plugins::PluginConfirmationError {
                    code: crate::plugins::PluginConfirmationErrorCode::StaleSource,
                    ..
                }
            )
        ));
    }
}

#[test]
fn pending_key_capture_blocks_background_and_invalidates_delayed_confirmation() {
    for delayed in [false, true] {
        let (_app_dirs, mut app) = app();
        let (manager, fixture) = crate::plugins::ExecutionFixture::new(8);
        app.extension.fixture_manager(manager);
        let invoke = |app: &mut App| {
            app.process_effect(AppEffect::PluginExecute {
                plugin: "demo".into(),
                command: "run".into(),
                arguments: Vec::new(),
            });
        };
        let invocation = if delayed {
            invoke(&mut app);
            let invocation = fixture.take();
            app.handle_configured_key("space".parse().expect("leader"));
            invocation
        } else {
            app.handle_configured_key("space".parse().expect("leader"));
            invoke(&mut app);
            fixture.take()
        };
        assert!(app.input.mapping_pending());
        fixture.reply_confirmation_request(
            &invocation,
            crate::plugins::PluginConfirmationRequest {
                token: 72 + u64::from(delayed),
                title: "Background".into(),
                message: "Must not consume a pending key gesture".into(),
                default_answer: crate::plugins::PluginConfirmationAnswer::No,
            },
        );
        app.process_plugin_events();
        assert!(app.input.prompt().is_none());
        assert!(app.input.mapping_pending());
        app.process_plugin_events();
        let invalidated = fixture.take();
        assert!(matches!(
            invalidated.confirmation_results()[0].outcome,
            crate::plugins::PluginConfirmationOutcome::Failed(
                crate::plugins::PluginConfirmationError {
                    code: crate::plugins::PluginConfirmationErrorCode::StaleSource,
                    ..
                }
            )
        ));
    }
}

#[test]
fn unfinished_modal_grammar_blocks_and_survives_confirmation_rejection() {
    for keys in [vec!['2'], vec!['d'], vec!['"', 'a'], vec!['r']] {
        for delayed in [false, true] {
            let (_app_dirs, mut app) = app();
            app.state
                .open_text(TextBuffer::from_text("abc"), None)
                .unwrap();
            app.set_dashboard_visibility(false);
            app.reconcile_editor_windows();
            let (manager, fixture) = crate::plugins::ExecutionFixture::new(8);
            app.extension.fixture_manager(manager);
            let invoke = |app: &mut App| {
                app.process_effect(AppEffect::PluginExecute {
                    plugin: "demo".into(),
                    command: "run".into(),
                    arguments: Vec::new(),
                });
            };
            let invocation = if delayed {
                invoke(&mut app);
                let invocation = fixture.take();
                for key in &keys {
                    app.handle_configured_key(crate::editor::input::KeyEvent::character(*key));
                }
                invocation
            } else {
                for key in &keys {
                    app.handle_configured_key(crate::editor::input::KeyEvent::character(*key));
                }
                invoke(&mut app);
                fixture.take()
            };
            assert!(
                app.input.grammar_pending(),
                "gesture {keys:?} was not pending"
            );
            fixture.reply_confirmation_request(
                &invocation,
                crate::plugins::PluginConfirmationRequest {
                    token: 80,
                    title: "Background".into(),
                    message: "Must not consume unfinished modal grammar".into(),
                    default_answer: crate::plugins::PluginConfirmationAnswer::No,
                },
            );
            app.process_plugin_events();
            assert!(app.input.prompt().is_none());
            assert!(
                app.input.grammar_pending(),
                "confirmation rejection erased gesture {keys:?}"
            );
            app.process_plugin_events();
            let invalidated = fixture.take();
            assert!(matches!(
                invalidated.confirmation_results()[0].outcome,
                crate::plugins::PluginConfirmationOutcome::Failed(
                    crate::plugins::PluginConfirmationError {
                        code: crate::plugins::PluginConfirmationErrorCode::StaleSource,
                        ..
                    }
                )
            ));
        }
    }
}

#[test]
fn background_confirmation_during_documentation_fails_without_overlay() {
    let (_app_dirs, mut app) = app();
    let policy = app.configuration.settings().clone();
    app.input.present_documentation(
        crate::ui::DocumentationPopupKind::Hover,
        "Hover".into(),
        "test".into(),
        None,
        [(
            crate::ui::DocumentationMarkupKind::PlainText,
            "newer".into(),
        )],
        &policy,
        200,
    );
    app.project_input_documentation();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(8);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    fixture.reply_confirmation_request(
        &invocation,
        crate::plugins::PluginConfirmationRequest {
            token: 53,
            title: "Background".into(),
            message: "Must not overlay documentation".into(),
            default_answer: crate::plugins::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    assert!(app.input.prompt().is_none());
    assert!(app.input.documentation().is_some());
    app.process_plugin_events();
    let invalidated = fixture.take();
    assert!(matches!(
        invalidated.confirmation_results()[0].outcome,
        crate::plugins::PluginConfirmationOutcome::Failed(
            crate::plugins::PluginConfirmationError {
                code: crate::plugins::PluginConfirmationErrorCode::StaleSource,
                ..
            }
        )
    ));
}

#[test]
fn background_confirmation_during_picker_fails_without_overlay() {
    let (_app_dirs, mut app) = app();
    app.install_picker("Newer picker".into(), Vec::new());
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(8);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    fixture.reply_confirmation_request(
        &invocation,
        crate::plugins::PluginConfirmationRequest {
            token: 54,
            title: "Background".into(),
            message: "Must not overlay picker".into(),
            default_answer: crate::plugins::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    assert!(app.input.prompt().is_none());
    assert_eq!(app.ui.picker.as_ref().unwrap().title, "Newer picker");
    app.process_plugin_events();
    let invalidated = fixture.take();
    assert!(matches!(
        invalidated.confirmation_results()[0].outcome,
        crate::plugins::PluginConfirmationOutcome::Failed(
            crate::plugins::PluginConfirmationError {
                code: crate::plugins::PluginConfirmationErrorCode::StaleSource,
                ..
            }
        )
    ));
}

#[test]
fn confirmation_result_retains_document_a_without_materializing_later_b() {
    let (_app_dirs, mut app) = app();
    let a_path = app.state.workspace_root.join("a.txt");
    let b_path = app.state.workspace_root.join("b.txt");
    app.state
        .open_text(TextBuffer::from_text("A"), Some(a_path.clone()))
        .unwrap();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(8);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    assert_eq!(invocation.snapshot().unwrap().utf8, "A");
    fixture.reply_confirmation_request(
        &invocation,
        crate::plugins::PluginConfirmationRequest {
            token: 42,
            title: "A".into(),
            message: "Answer for A".into(),
            default_answer: crate::plugins::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    app.state
        .open_text(TextBuffer::from_text("B"), Some(b_path))
        .unwrap();
    app.handle_configured_key("enter".parse().unwrap());
    let answer = fixture.take();
    assert!(answer.snapshot().is_none());
    assert!(matches!(
        answer.confirmation_results()[0].outcome,
        crate::plugins::PluginConfirmationOutcome::Answered(
            crate::plugins::PluginConfirmationAnswer::No
        )
    ));

    fixture.reply(&answer, Vec::new());
    app.process_plugin_events();
    let active = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[active].text.to_string(), "B");
}

#[test]
fn changed_confirmation_document_gets_a_terminal_failure() {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("a.txt");
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("A"), Some(path.clone()))
        .unwrap();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(8);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    fixture.reply_confirmation_request(
        &invocation,
        crate::plugins::PluginConfirmationRequest {
            token: 43,
            title: "A".into(),
            message: "Answer for unchanged A".into(),
            default_answer: crate::plugins::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    assert!(
        app.state
            .replace_buffer_from_disk(buffer, TextBuffer::from_text("changed A"), path,)
    );
    app.handle_configured_key("enter".parse().unwrap());
    let invalidated = fixture.take();
    assert!(invalidated.snapshot().is_none());
    assert!(matches!(
        invalidated.confirmation_results()[0].outcome,
        crate::plugins::PluginConfirmationOutcome::Failed(
            crate::plugins::PluginConfirmationError {
                code: crate::plugins::PluginConfirmationErrorCode::StaleSource,
                ..
            }
        )
    ));
}

#[test]
fn stale_ordinary_reply_releases_only_its_exact_confirmation_reservation() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(8);
    app.extension.fixture_manager(manager);
    for _ in 0..2 {
        app.process_effect(AppEffect::PluginExecute {
            plugin: "demo".into(),
            command: "run".into(),
            arguments: Vec::new(),
        });
    }
    let first = fixture.take();
    let second = fixture.take();
    assert_eq!(app.extension.confirmation_count(), 2);

    let accepted = app.configuration.fixture_snapshot();
    app.configuration.install_fixture(accepted);
    app.process_plugin_events();
    fixture.reply(&first, Vec::new());
    app.process_plugin_events();
    assert_eq!(
        app.extension.confirmation_count(),
        1,
        "only the rejected invocation reservation must be released"
    );

    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let retry = fixture.take();
    assert!(retry.confirmation_available());
    assert_eq!(app.extension.confirmation_count(), 2);

    fixture.reply_confirmation_request(
        &second,
        crate::plugins::PluginConfirmationRequest {
            token: 61,
            title: "Still reserved".into(),
            message: "The second invocation retained its exact slot".into(),
            default_answer: crate::plugins::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    let invalidated = fixture.take();
    assert_eq!(invalidated.confirmation_results()[0].request, 61);
    assert!(matches!(
        invalidated.confirmation_results()[0].outcome,
        crate::plugins::PluginConfirmationOutcome::Failed(
            crate::plugins::PluginConfirmationError {
                code: crate::plugins::PluginConfirmationErrorCode::StaleSource,
                ..
            }
        )
    ));
}

#[test]
fn confirmation_capacity_is_reserved_before_the_guest_call() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(128);
    app.extension.fixture_manager(manager);
    let mut first = None;
    for index in 0..64 {
        app.process_effect(AppEffect::PluginExecute {
            plugin: "demo".into(),
            command: "run".into(),
            arguments: Vec::new(),
        });
        let invocation = fixture.take();
        assert!(invocation.confirmation_available());
        if index == 0 {
            first = Some(invocation);
        }
    }
    assert_eq!(app.extension.confirmation_count(), 64);

    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let full = fixture.take();
    assert!(!full.confirmation_available());
    assert_eq!(app.extension.confirmation_count(), 64);

    fixture.reply(&first.unwrap(), Vec::new());
    fixture.reply(&full, Vec::new());
    app.process_plugin_events();
    assert_eq!(app.extension.confirmation_count(), 63);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    assert!(fixture.take().confirmation_available());
}

#[test]
fn real_component_observes_queue_full_from_aggregate_confirmation_ownership() {
    use sha2::Digest;
    use sha2::Sha256;
    let core = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/retained-guest/retained-guest.core.wasm"
    ));
    let component = wit_component::ComponentEncoder::default()
        .module(core)
        .expect("fixture metadata")
        .validate(true)
        .encode()
        .expect("component");
    let store = tempfile::tempdir().expect("plugin store");
    fs::write(store.path().join("fixture.wasm"), &component).unwrap();
    let manifest =
        crate::app::plugins::fixture_manifest(format!("{:x}", Sha256::digest(&component)));
    write_plugin_manifest(store.path(), &manifest);

    let (_app_dirs, mut app) = app_with_plugin_queue(64);
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let load = app
        .extension
        .index_from_store(
            "demo".into(),
            store.path().to_path_buf(),
            PathBuf::from("manifest.toml"),
            PathBuf::from("fixture.wasm"),
            app.plugin_limits(),
        )
        .expect("index request");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !app.extension.is_active_load("demo", load) {
        app.process_plugin_events();
        assert!(
            Instant::now() < deadline,
            "fixture activation timed out: notification={:?}, statuses={:?}",
            app.ui.notification,
            app.extension.statuses()
        );
        thread::yield_now();
    }

    let enqueue_deadline = Instant::now() + Duration::from_secs(15);
    for expected in 1..=64 {
        loop {
            app.process_effect(AppEffect::PluginExecute {
                plugin: "demo".into(),
                command: "confirmationprobe".into(),
                arguments: Vec::new(),
            });
            if app.extension.confirmation_count() == expected {
                break;
            }
            assert!(
                Instant::now() < enqueue_deadline,
                "production execution queue did not accept reservation {expected}"
            );
            thread::yield_now();
        }
    }
    assert_eq!(app.extension.confirmation_count(), 64);

    // Transfer reservations to pending confirmations before opening a modal changes capture.
    // This keeps the ordinary probe separate from stale-source result-only callbacks.
    let reply_deadline = Instant::now() + Duration::from_secs(15);
    while !app
        .notices
        .history()
        .any(|(_, text)| text == "confirmationprobe:64:accepted")
    {
        for event in app.extension.poll_owned().unwrap() {
            if let crate::plugins::OwnedPluginEvent::Executed(reply) = event {
                app.apply_plugin_execution(reply);
            }
        }
        assert!(
            !matches!(
                app.extension.statuses().get("demo"),
                Some(crate::plugins::PluginStatus::Failed(_))
            ),
            "real Component failed before returning a reserved reply: {:?}",
            app.extension.statuses()
        );
        assert!(
            Instant::now() < reply_deadline,
            "reserved real reply timed out: count={}, notification={:?}, statuses={:?}",
            app.extension.confirmation_count(),
            app.ui.notification,
            app.extension.statuses()
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        app.extension.confirmation_count(),
        64,
        "aggregate lane must be full immediately before the ordinary probe"
    );
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "confirmationprobe".into(),
        arguments: Vec::new(),
    });
    assert_eq!(
        app.extension.confirmation_count(),
        64,
        "the intended 65th ordinary invocation must receive no reservation"
    );
    let full_deadline = Instant::now() + Duration::from_secs(15);
    while !app
        .notices
        .history()
        .any(|(_, text)| text == "confirmationprobe:65:queue-full")
    {
        for event in app.extension.poll_owned().unwrap() {
            if let crate::plugins::OwnedPluginEvent::Executed(reply) = event {
                app.apply_plugin_execution(reply);
            }
        }
        assert!(
            app.extension.confirmation_count() <= 64,
            "aggregate confirmation ownership exceeded its bound"
        );
        assert!(
            Instant::now() < full_deadline,
            "real Component did not report the exact ordinary probe queue-full: {:?}",
            app.ui.notification
        );
        thread::yield_now();
    }
    app.process_plugin_events();
    assert!(app.input.prompt().is_some());
    // Opening the first prompt changes capture. Pending requests may now finish through bounded
    // stale-source result-only callbacks, so ownership can legitimately decline after polling.
    assert!(app.extension.confirmation_count() <= 64);
}

#[test]
fn packaged_production_component_clone_refusal_never_enqueues_a_guest_callback() {
    use sha2::Digest;
    use sha2::Sha256;
    let core = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/retained-guest/retained-guest.core.wasm"
    ));
    assert_eq!(
        format!("{:x}", Sha256::digest(core)),
        "4a81f4665c8cee1e88191b5223030ee7821b79683734120ad0a770126c3501b1"
    );
    let component = wit_component::ComponentEncoder::default()
        .module(core)
        .expect("fixture metadata")
        .validate(true)
        .encode()
        .expect("component");
    let store = tempfile::tempdir().expect("plugin store");
    fs::write(store.path().join("fixture.wasm"), &component).unwrap();
    let manifest =
        crate::app::plugins::fixture_manifest(format!("{:x}", Sha256::digest(&component)));
    write_plugin_manifest(store.path(), &manifest);

    for (command, private) in [("drive", false), ("private", true)] {
        let (_app_dirs, mut app) = app();
        app.state
            .open_text(TextBuffer::from_text("editor"), None)
            .unwrap();
        app.set_dashboard_visibility(false);
        app.reconcile_editor_windows();
        let load = app
            .extension
            .index_from_store(
                "demo".into(),
                store.path().to_path_buf(),
                PathBuf::from("manifest.toml"),
                PathBuf::from("fixture.wasm"),
                app.plugin_limits(),
            )
            .expect("index request");
        let deadline = Instant::now() + Duration::from_secs(15);
        while !app.extension.is_active_load("demo", load) {
            app.process_plugin_events();
            assert!(
                Instant::now() < deadline,
                "fixture activation timed out: {:?}; {:?}",
                app.ui.notification,
                app.extension.statuses()
            );
            thread::yield_now();
        }
        app.process_effect(AppEffect::PluginExecute {
            plugin: "demo".into(),
            command: command.into(),
            arguments: Vec::new(),
        });
        if private {
            while !app
                .window_session
                .graph()
                .content_snapshots()
                .iter()
                .any(|content| {
                    content.adapter == crate::windows::ContentAdapterKind::PrivateCellSurface
                })
            {
                app.process_plugin_events();
                assert!(Instant::now() < deadline, "private open timed out");
                thread::yield_now();
            }
            app.process_effect(AppEffect::PluginExecute {
                plugin: "demo".into(),
                command: command.into(),
                arguments: Vec::new(),
            });
        }
        while if private {
            app.ui.private_surfaces.is_empty()
        } else {
            app.ui.plugin_surfaces.is_empty()
        } {
            app.process_plugin_events();
            assert!(
                Instant::now() < deadline,
                "{command} window timed out: notice={:?}, statuses={:?}, retained={:?}, private={:?}, delivery={}",
                app.ui.notification,
                app.extension.statuses(),
                app.ui.plugin_surfaces,
                app.ui.private_surfaces,
                app.extension.fixture_delivery_state()
            );
            thread::yield_now();
        }
        let window = if private {
            app.ui
                .private_surfaces
                .last()
                .expect("private surface")
                .window
        } else {
            app.ui
                .plugin_surfaces
                .last()
                .expect("retained surface")
                .window
        };
        let target = format!("window:{}", window.0);
        let graph = app.window_session.graph().clone();
        let editor = app.state.window_version();
        let catalogue = app.extension.contributions().version();
        let retained = app.ui.plugin_surfaces.clone();
        let private_surfaces = app.ui.private_surfaces.clone();
        let delivery = app.extension.fixture_delivery_state();

        let error = app
            .clone_graph_window(&target, crate::windows::TileAxis::Horizontal)
            .expect_err("production presenter clone must be rejected");
        assert!(error.contains("clone-view"), "{error}");
        assert_eq!(app.window_session.graph(), &graph);
        assert_eq!(app.state.window_version(), editor);
        assert_eq!(app.extension.contributions().version(), catalogue);
        assert_eq!(app.ui.plugin_surfaces, retained);
        assert_eq!(app.ui.private_surfaces, private_surfaces);
        assert_eq!(app.extension.fixture_delivery_state(), delivery);

        app.extension
            .disable_load("demo", load)
            .expect("disable actual component load");
        app.remove_plugin_contributions("demo", load);
        while app.extension.fixture_queued() != 0 {
            app.process_plugin_events();
            assert!(Instant::now() < deadline, "fixture unload timed out");
            thread::yield_now();
        }
        assert!(app.extension.contributions().get("demo").is_none());
        let graph = app.window_session.graph().clone();
        let delivery = app.extension.fixture_delivery_state();
        assert!(
            app.clone_graph_window(&target, crate::windows::TileAxis::Horizontal)
                .is_err()
        );
        assert_eq!(app.window_session.graph(), &graph);
        assert_eq!(app.extension.fixture_delivery_state(), delivery);

        let replacement = app
            .extension
            .index_from_store(
                "demo".into(),
                store.path().to_path_buf(),
                PathBuf::from("manifest.toml"),
                PathBuf::from("fixture.wasm"),
                app.plugin_limits(),
            )
            .expect("replacement index request");
        assert_ne!(replacement, load);
        let replacement_deadline = Instant::now() + Duration::from_secs(15);
        while !app.extension.is_active_load("demo", replacement) {
            app.process_plugin_events();
            assert!(
                Instant::now() < replacement_deadline,
                "replacement activation timed out"
            );
            thread::yield_now();
        }
        let graph = app.window_session.graph().clone();
        let delivery = app.extension.fixture_delivery_state();
        assert!(
            app.clone_graph_window(&target, crate::windows::TileAxis::Horizontal)
                .is_err()
        );
        assert_eq!(app.window_session.graph(), &graph);
        assert_eq!(app.extension.fixture_delivery_state(), delivery);
    }
}

#[test]
fn packaged_real_guest_confirmation_updates_its_custom_view() {
    use sha2::Digest;
    use sha2::Sha256;
    let core = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/retained-guest/retained-guest.core.wasm"
    ));
    let component = wit_component::ComponentEncoder::default()
        .module(core)
        .expect("fixture metadata")
        .validate(true)
        .encode()
        .expect("component");
    let store = tempfile::tempdir().expect("plugin store");
    fs::write(store.path().join("fixture.wasm"), &component).unwrap();
    let manifest =
        crate::app::plugins::fixture_manifest(format!("{:x}", Sha256::digest(&component)));
    write_plugin_manifest(store.path(), &manifest);

    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    assert!(app.window_session.graph().revision().is_some());
    let load = app
        .extension
        .index_from_store(
            "demo".into(),
            store.path().to_path_buf(),
            PathBuf::from("manifest.toml"),
            PathBuf::from("fixture.wasm"),
            app.plugin_limits(),
        )
        .expect("index request");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !app.extension.is_active_load("demo", load) {
        app.process_plugin_events();
        assert!(
            Instant::now() < deadline,
            "fixture activation timed out: {:?}; {:?}",
            app.ui.notification,
            app.extension.statuses()
        );
        thread::yield_now();
    }
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "confirmation".into(),
        arguments: Vec::new(),
    });
    while app.ui.plugin_surfaces.iter().all(|surface| {
        surface
            .rows
            .iter()
            .all(|row| row.text != "Request confirmation")
    }) {
        app.process_plugin_events();
        assert!(Instant::now() < deadline, "confirmation view timed out");
        thread::yield_now();
    }
    let confirmation_window = app.ui.plugin_surfaces[0].window;
    let focus = app
        .window_session
        .plan_focus(
            confirmation_window,
            &app.state.window_version(),
            app.extension.contributions().version(),
        )
        .expect("plan confirmation view focus");
    app.apply_window_plan(focus)
        .expect("focus confirmation view");
    let focused_before_answer = app.window_session.graph().focused();
    assert_eq!(
        app.window_session.graph().focused(),
        Some(confirmation_window)
    );
    app.extension
        .focus_window_row(confirmation_window, 0)
        .expect("focus confirmation action");
    let action = app
        .extension
        .window_interaction(
            app.window_session.graph(),
            crate::app::plugins::PluginUiInteraction::Activate,
        )
        .expect("activate retained confirmation action")
        .expect("confirmation action command");
    app.invoke_mapped_command(&action);
    while app.input.prompt().is_none() {
        app.process_plugin_events();
        assert!(
            Instant::now() < deadline,
            "confirmation request timed out: count={}, notice={:?}",
            app.extension.confirmation_count(),
            app.ui.notification
        );
        thread::yield_now();
    }
    app.handle_configured_key("left".parse().unwrap());
    app.handle_configured_key("enter".parse().unwrap());
    assert!(
        app.input.prompt().is_none(),
        "confirmation not submitted: {:?}",
        app.input.prompt()
    );
    assert_eq!(app.window_session.graph().focused(), focused_before_answer);
    while app
        .ui
        .plugin_surfaces
        .iter()
        .all(|surface| surface.rows.iter().all(|row| row.text != "Confirmed: Yes"))
    {
        app.process_plugin_events();
        assert!(
            Instant::now() < deadline,
            "Yes result timed out: notice={:?}, status={:?}",
            app.ui.notification,
            app.extension.statuses()
        );
        thread::yield_now();
    }

    for (key, expected) in [
        ("enter", "Confirmed: No"),
        ("escape", "Confirmed: Cancelled"),
    ] {
        let outcome_deadline = Instant::now() + Duration::from_secs(4);
        app.apply_retained_ui_interaction(crate::app::plugins::PluginUiInteraction::Activate);
        while app.input.prompt().is_none() {
            app.process_plugin_events();
            assert!(Instant::now() < outcome_deadline, "later request timed out");
            thread::yield_now();
        }
        app.handle_configured_key(key.parse().unwrap());
        while app
            .ui
            .plugin_surfaces
            .iter()
            .all(|surface| surface.rows.iter().all(|row| row.text != expected))
        {
            app.process_plugin_events();
            assert!(
                Instant::now() < outcome_deadline,
                "{expected} result timed out"
            );
            thread::yield_now();
        }
    }
}

#[test]
fn retained_plugin_window_projection_is_idle_and_load_owned() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    let index = crate::plugins::index_manifest_contributions(
        &manifest,
        77,
        "manifest.toml:bytes 0..1".into(),
    )
    .expect("index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    let request = crate::plugins::PluginUiRequest {
        token: 1,
        kind: crate::plugins::PluginUiRequestKind::OpenWindow {
            expected_graph_revision: app.window_session.graph().revision().expect("revision"),
            content_type: "plugin.demo.panel".into(),
            title: "Demo".into(),
            placement: crate::plugins::PluginRequestedPlacement::Floating {
                x: 5,
                y: 3,
                width: 24,
                height: 7,
            },
            tree: Some(crate::plugins::PluginUiTree {
                revision: 1,
                root: 1,
                nodes: vec![crate::plugins::PluginUiNode {
                    id: 1,
                    kind: crate::plugins::PluginUiNodeKind::Text,
                    text: "retained plugin".into(),
                    style: crate::plugins::PluginUiStyle::Accent,
                    action: None,
                    children: Vec::new(),
                }],
            }),
        },
    };
    let crate::app::plugins::WindowUpdate::Publish(source, transition) =
        app.extension.prepare_window(
            &index,
            request.clone(),
            &app.state,
            &app.window_session,
            false,
        )
    else {
        panic!("expected staged window");
    };
    app.window_session.invalidate_native();
    let accepted = app.window_session.accept_plugin_transition(
        *transition,
        &app.state.window_version(),
        source.catalog(),
        &mut app.state,
    );
    assert!(
        source
            .finish(accepted, app.window_session.graph().revision())
            .error
            .is_some()
    );
    assert!(app.extension.window_snapshots().is_empty());
    let crate::app::plugins::WindowUpdate::Publish(source, transition) =
        app.extension.prepare_window(
            &index,
            request.clone(),
            &app.state,
            &app.window_session,
            false,
        )
    else {
        panic!("expected fresh window proposal");
    };
    let (other, _) = app
        .window_session
        .begin_plugin(
            &index.owner,
            index.plugin_load,
            (&app.state.window_version(), source.catalog()),
        )
        .unwrap()
        .seal();
    let accepted = app.window_session.accept_plugin_transition(
        other,
        &app.state.window_version(),
        source.catalog(),
        &mut app.state,
    );
    assert!(
        source
            .finish(accepted, app.window_session.graph().revision())
            .error
            .is_some()
    );
    drop(transition);
    assert!(app.extension.window_snapshots().is_empty());
    let event = app.apply_plugin_ui_request(&index, request);
    assert!(event.error.is_none(), "{:?}", event.error);
    let window = app.extension.window_snapshots()[0].window;
    app.ui.plugin_surfaces = app.extension.window_snapshots();
    app.window_session.invalidate_native();
    app.reconcile_editor_windows();
    assert!(
        app.compositor_projection()
            .expect("projection")
            .window(window)
            .is_some()
    );
    let reconciliation_count = app.window_session.test_publications();
    let graph = app.window_session.graph().clone();
    for _ in 0..256 {
        app.reconcile_editor_windows();
    }
    assert_eq!(app.window_session.test_publications(), reconciliation_count);
    assert_eq!(app.window_session.graph(), &graph);
    app.remove_plugin_contributions("demo", PluginLoadId(77));
    assert!(app.ui.plugin_surfaces.is_empty());
    assert!(app.window_session.graph().window(window).is_none());
}

#[test]
fn plugin_host_events_are_snapshot_owned_coalesced_and_idle() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::plugins::PluginCapability::EventSubscriptions);
    manifest.contributions.subscriptions = vec![crate::plugins::PluginSubscriptionContribution {
        id: "plugin.demo.subscription.buffer".into(),
        kind: crate::plugins::PluginSubscriptionKind::BufferSummaryChanged,
        maximum_queued: 8,
        maximum_rate_per_second: 120,
        coalesce: true,
        provenance: None,
    }];
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 77, "event manifest".into())
            .expect("event index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    app.extension.install_events(&index).expect("event owner");
    app.refresh_plugin_host_events();
    assert_eq!(app.extension.events().pending("demo", PluginLoadId(77)), 1);
    let stable_queue = app.extension.events().clone();
    let stable_observation = app.extension.observation().clone();
    for _ in 0..256 {
        app.refresh_plugin_host_events();
    }
    assert_eq!(*app.extension.events(), stable_queue);
    assert_eq!(*app.extension.observation(), stable_observation);

    let view = app.state.active_view().expect("view");
    app.state.views[view].cursor = ByteOffset(1);
    app.refresh_plugin_host_events();
    let event = app
        .extension
        .deliver("demo", PluginLoadId(77))
        .pop()
        .expect("coalesced buffer event");
    assert_eq!(event.coalesced, 1);
    assert!(event.payload.contains("cursor=1"));
    app.extension
        .acknowledge("demo", PluginLoadId(77), &[event.id]);
    while app.state.active_view().is_some() {
        app.state.close_active_view().unwrap();
    }
    app.refresh_plugin_host_events();
    assert_eq!(app.extension.events().pending("demo", PluginLoadId(77)), 1);
    // The earlier delivery is rate-limited, but the disappearance is queued now.
    thread::sleep(Duration::from_millis(10));
    let empty = app
        .extension
        .deliver("demo", PluginLoadId(77))
        .pop()
        .expect("empty buffer event");
    assert_eq!(
        empty.kind,
        crate::plugins::PluginSubscriptionKind::BufferSummaryChanged
    );
    assert_eq!(empty.payload, "buffer=none");
    app.refresh_plugin_host_events();
    assert_eq!(app.extension.events().pending("demo", PluginLoadId(77)), 1);
    app.remove_plugin_contributions("demo", PluginLoadId(77));
    assert_eq!(app.extension.events().pending("demo", PluginLoadId(77)), 0);
}

#[test]
fn plugin_picker_provider_uses_the_generic_presenter_and_cancels_exactly() {
    let (_app_dirs, mut app) = app();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::plugins::PluginCapability::PickerProviders);
    manifest.contributions.providers = vec![crate::plugins::PluginProviderContribution {
        id: "plugin.demo.provider.files".into(),
        kind: crate::plugins::PluginProviderKind::PickerSource,
        command: "plugin.demo.run".into(),
        maximum_concurrent_requests: 1,
        maximum_items: 16,
        maximum_bytes: 8_192,
        deadline_milliseconds: 30_000,
        provenance: None,
    }];
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 77, "provider manifest".into())
            .expect("provider index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    app.extension
        .fixture_provider_install(&index)
        .expect("provider owner");
    let request = app
        .extension
        .fixture_provider_start(
            crate::plugins::PluginProviderStart {
                plugin: "demo".into(),
                load: PluginLoadId(77),
                provider: "plugin.demo.provider.files".into(),
                query: "be".into(),
                context: "editor".into(),
                workspace_revision: Some(app.workspace.root_revision()),
                buffer_revision: None,
            },
            &app.state,
        )
        .expect("request");
    app.extension
        .provider_chunk(
            "demo",
            PluginLoadId(77),
            (Some(app.workspace.root_revision()), None),
            crate::plugins::PluginPickerChunk {
                request,
                sequence: 1,
                items: vec![
                    crate::plugins::PluginPickerItem {
                        id: "z".into(),
                        primary: "Zebra".into(),
                        secondary: None,
                        action_data: vec![1],
                    },
                    crate::plugins::PluginPickerItem {
                        id: "b".into(),
                        primary: "Beta".into(),
                        secondary: Some("ranked first".into()),
                        action_data: vec![2],
                    },
                ],
                terminal: crate::plugins::PluginProviderTerminal::More,
            },
        )
        .expect("chunk");
    app.extension.picker_installed(Some(request));
    app.present_plugin_picker(request);
    assert_eq!(app.search.picker_surface(), PickerSurfaceKind::Generic);
    assert_eq!(
        app.search
            .picker()
            .and_then(crate::workspace::PickerState::selected_item)
            .map(|item| item.label.as_str()),
        Some("Beta")
    );
    app.picker_insert("e");
    app.input.set_picker_normal(true);
    let selected = app
        .search
        .picker()
        .unwrap()
        .selected_item()
        .unwrap()
        .id
        .clone();
    app.invoke(
        crate::editor::CommandInvocation::new("overlay.help.contextual", InvocationSource::Test)
            .unwrap(),
    );
    assert!(app.search.help_is_open());
    let help_ui = app.ui.picker.clone();
    app.extension
        .provider_chunk(
            "demo",
            PluginLoadId(77),
            (Some(app.workspace.root_revision()), None),
            crate::plugins::PluginPickerChunk {
                request,
                sequence: 2,
                items: vec![crate::plugins::PluginPickerItem {
                    id: "better".into(),
                    primary: "Better".into(),
                    secondary: None,
                    action_data: Vec::new(),
                }],
                terminal: crate::plugins::PluginProviderTerminal::Complete,
            },
        )
        .unwrap();
    app.present_plugin_picker(request);
    assert!(
        app.search.help_is_open(),
        "a stream refresh must not dismiss help"
    );
    assert_eq!(app.ui.picker, help_ui);
    assert!(app.extension.provider_visible(request));
    app.close_picker();
    assert!(!app.search.help_is_open());
    assert!(
        app.ui
            .picker
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .any(|row| row.label == "Better")
    );
    assert_eq!(app.ui.picker.as_ref().unwrap().query, "e");
    assert_eq!(
        app.search.picker().unwrap().selected_item().unwrap().id,
        selected
    );
    assert!(app.input.picker_normal());
    let stable = app.extension.provider_projection(request);
    let stable_ui = app.ui.picker.clone();
    for _ in 0..256 {
        app.present_plugin_picker(request);
        assert_eq!(app.extension.provider_projection(request), stable);
        assert_eq!(app.ui.picker, stable_ui);
        assert_eq!(
            app.search.picker().unwrap().selected_item().unwrap().id,
            selected
        );
        assert!(app.input.picker_normal());
    }
    app.install_picker("Another picker".into(), Vec::new());
    let replacement_ui = app.ui.picker.clone();
    assert!(!app.extension.provider_visible(request));
    app.extension.picker_installed(Some(request)); // cancelled sources cannot reactivate
    app.present_plugin_picker(request);
    assert_eq!(app.ui.picker, replacement_ui);
    assert!(!app.extension.provider_visible(request));
    app.close_picker();
    assert_eq!(
        app.extension
            .provider_chunk(
                "demo",
                PluginLoadId(77),
                (Some(app.workspace.root_revision()), None),
                crate::plugins::PluginPickerChunk {
                    request,
                    sequence: 2,
                    items: Vec::new(),
                    terminal: crate::plugins::PluginProviderTerminal::Complete,
                },
            )
            .unwrap_err()
            .code,
        crate::plugins::PluginProviderErrorCode::UnknownRequest
    );
    // Reopening a one-request provider reserves new metadata without destroying
    // the displayed source if subsequent picker acceptance is refused.
    let mut broker = crate::plugins::PluginProviderBroker::default();
    broker.install(&index).unwrap();
    let start = || crate::plugins::PluginProviderStart {
        plugin: "demo".into(),
        load: PluginLoadId(77),
        provider: "plugin.demo.provider.files".into(),
        query: String::new(),
        context: "editor".into(),
        workspace_revision: None,
        buffer_revision: None,
    };
    let old = broker.start(start()).unwrap();
    let next = broker.reserve_replacing(start(), Some(old)).unwrap();
    assert!(broker.presentable(old) && broker.presentable(next));
    assert_eq!(
        broker
            .reserve_replacing(start(), Some(old))
            .unwrap_err()
            .code,
        crate::plugins::PluginProviderErrorCode::QueueFull
    );
    broker.cancel(next);
    assert!(broker.presentable(old));
    let next = broker.reserve_replacing(start(), Some(old)).unwrap();
    broker.cancel(old);
    assert!(broker.presentable(next));
}

#[test]
fn private_plugin_surface_geometry_snapshot_and_idle_projection_are_application_owned() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::plugins::PluginCapability::PrivateCells);
    let content = &mut manifest.contributions.content_types[0];
    content.presenter = crate::plugins::PluginPresenterKind::PrivateCells;
    content.private_cells = Some(crate::plugins::PluginPrivateCellPolicy {
        maximum_cells: 64,
        maximum_patch_cells: 32,
        maximum_dirty_rectangles: 8,
        maximum_styles: 8,
        maximum_hit_regions: 8,
        maximum_bytes: 4_096,
        cursor: true,
    });
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 77, "private manifest".into())
            .expect("index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    let revision = app.window_session.graph().revision().expect("revision");
    let opened = app.apply_plugin_ui_request(
        &index,
        crate::plugins::PluginUiRequest {
            token: 1,
            kind: crate::plugins::PluginUiRequestKind::OpenWindow {
                expected_graph_revision: revision,
                content_type: "plugin.demo.panel".into(),
                title: "Cells".into(),
                placement: crate::plugins::PluginRequestedPlacement::Floating {
                    x: 4,
                    y: 2,
                    width: 12,
                    height: 4,
                },
                tree: None,
            },
        },
    );
    assert!(opened.error.is_none(), "{:?}", opened.error);
    let handle = opened.handle.expect("handle");
    app.window_session.invalidate_native();
    app.reconcile_editor_windows();
    let geometry = app
        .extension
        .ui_events()
        .get(&("demo".into(), PluginLoadId(77)))
        .and_then(|events| events.iter().find_map(|event| event.geometry.clone()))
        .expect("local geometry event");
    assert_eq!((geometry.width, geometry.height), (12, 4));
    let replaced = app.apply_plugin_ui_request(
        &index,
        crate::plugins::PluginUiRequest {
            token: 2,
            kind: crate::plugins::PluginUiRequestKind::ReplacePrivate {
                handle,
                snapshot: crate::plugins::PluginPrivateSnapshot {
                    revision: 1,
                    dimensions_revision: geometry.dimensions_revision,
                    styles: vec![crate::plugins::PluginPrivateStyle {
                        id: 1,
                        style: crate::plugins::PluginUiStyle::Accent,
                    }],
                    cells: vec![crate::plugins::PluginPrivateCell {
                        x: 0,
                        y: 0,
                        grapheme: "X".into(),
                        continuation: false,
                        style: 1,
                    }],
                    cursor: Some(crate::plugins::PluginPrivateCursor { x: 0, y: 0 }),
                    hit_regions: vec![crate::plugins::PluginPrivateHitRegion {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                        action: "plugin.demo.run".into(),
                    }],
                },
            },
        },
    );
    assert!(replaced.error.is_none(), "{:?}", replaced.error);
    app.window_session.invalidate_native();
    app.reconcile_editor_windows();
    assert_eq!(app.ui.private_surfaces.len(), 1);
    assert_eq!(app.ui.private_surfaces[0].cells[0].grapheme, "X");
    let graph = app.window_session.graph().clone();
    let ui = app.ui.private_surfaces.clone();
    let reconciliations = app.window_session.test_publications();
    for _ in 0..256 {
        app.reconcile_editor_windows();
    }
    assert_eq!(app.window_session.graph(), &graph);
    assert_eq!(app.ui.private_surfaces, ui);
    assert_eq!(app.window_session.test_publications(), reconciliations);
}

#[test]
fn retained_interaction_commands_use_host_focus_scroll_and_mouse_state() {
    let (_app_dirs, mut app) = app();
    app.state
        .open_text(TextBuffer::from_text("editor"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    let index = crate::plugins::index_manifest_contributions(&manifest, 77, "manifest.toml".into())
        .expect("index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    let revision = app.window_session.graph().revision().expect("revision");
    let event = app.apply_plugin_ui_request(
        &index,
        crate::plugins::PluginUiRequest {
            token: 1,
            kind: crate::plugins::PluginUiRequestKind::OpenWindow {
                expected_graph_revision: revision,
                content_type: "plugin.demo.panel".into(),
                title: "Demo".into(),
                placement: crate::plugins::PluginRequestedPlacement::Floating {
                    x: 4,
                    y: 2,
                    width: 24,
                    height: 7,
                },
                tree: Some(crate::plugins::PluginUiTree {
                    revision: 1,
                    root: 1,
                    nodes: vec![
                        crate::plugins::PluginUiNode {
                            id: 1,
                            kind: crate::plugins::PluginUiNodeKind::Container,
                            text: String::new(),
                            style: crate::plugins::PluginUiStyle::Default,
                            action: None,
                            children: vec![2, 3],
                        },
                        crate::plugins::PluginUiNode {
                            id: 2,
                            kind: crate::plugins::PluginUiNodeKind::List,
                            text: "List".into(),
                            style: crate::plugins::PluginUiStyle::Default,
                            action: None,
                            children: Vec::new(),
                        },
                        crate::plugins::PluginUiNode {
                            id: 3,
                            kind: crate::plugins::PluginUiNodeKind::Button,
                            text: "Run".into(),
                            style: crate::plugins::PluginUiStyle::Accent,
                            action: Some("plugin.demo.run".into()),
                            children: Vec::new(),
                        },
                    ],
                }),
            },
        },
    );
    assert!(event.error.is_none(), "{:?}", event.error);
    app.ui.plugin_surfaces = app.extension.window_snapshots();
    assert_eq!(
        app.ui.plugin_surfaces[0]
            .rows
            .iter()
            .find(|row| row.focused)
            .map(|row| row.node),
        Some(2)
    );
    app.window_session.invalidate_native();
    app.reconcile_editor_windows();
    app.invoke_mapped_command("surface.retained.focus.next");
    assert_eq!(
        app.ui.plugin_surfaces[0]
            .rows
            .iter()
            .find(|row| row.focused)
            .map(|row| row.node),
        Some(3)
    );
    app.invoke_mapped_command("surface.retained.focus.previous");
    assert_eq!(
        app.ui.plugin_surfaces[0]
            .rows
            .iter()
            .find(|row| row.focused)
            .map(|row| row.node),
        Some(2)
    );
    app.invoke_mapped_command("surface.retained.scroll.down");
    assert_eq!(app.ui.plugin_surfaces[0].scroll, 1);
    let window = app.ui.plugin_surfaces[0].window;
    let rect = app
        .compositor_projection()
        .and_then(|projection| projection.window(window).map(|window| window.rect))
        .expect("plugin rectangle");
    app.input.observe_mouse(rect.x, rect.y);
    app.invoke_mapped_command("surface.retained.mouse.select");
    assert_eq!(
        app.ui.plugin_surfaces[0]
            .rows
            .iter()
            .find(|row| row.focused)
            .map(|row| row.node),
        Some(3),
        "scroll-adjusted mouse row selects the visible button"
    );
}

#[test]
fn invalid_plugin_arguments_leave_pending_deliveries_intact() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let key = ("demo".to_owned(), PluginLoadId(1));
    app.extension.queue_ui(
        key.0.clone(),
        key.1,
        [crate::plugins::PluginUiEvent {
            token: 7,
            handle: None,
            graph_revision: None,
            tree_revision: None,
            surface_revision: None,
            geometry: None,
            action: None,
            error: None,
        }],
    );
    app.extension
        .queue_storage(crate::plugins::PluginStorageResult {
            plugin: key.0.clone(),
            load: key.1,
            token: 8,
            value: None,
            revision: None,
            entries: Vec::new(),
            truncated: false,
            error: None,
        });
    for arguments in [vec!["x".repeat(65_537)], vec![String::new(); 257]] {
        app.process_effect(AppEffect::PluginExecute {
            plugin: "demo".into(),
            command: "run".into(),
            arguments,
        });
        assert_eq!(
            app.extension
                .ui_events()
                .get(&key)
                .and_then(|events| events.first())
                .map(|event| event.token),
            Some(7)
        );
        assert_eq!(
            app.extension
                .storage_results()
                .get(&key)
                .and_then(|results| results.first())
                .map(|result| result.token),
            Some(8)
        );
        assert_eq!(app.extension.confirmation_count(), 0);
    }
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    assert!(fixture.take().arguments().is_empty());
    assert!(!app.extension.ui_events().contains_key(&key));
    assert!(!app.extension.storage_results().contains_key(&key));
}

#[test]
fn runtime_reset_discards_pending_invocations_without_disabling_new_calls() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let invoke = |app: &mut App| {
        app.process_effect(AppEffect::PluginExecute {
            plugin: "demo".into(),
            command: "run".into(),
            arguments: Vec::new(),
        });
    };
    invoke(&mut app);
    let old = fixture.take();
    app.extension.reset_runtime();
    fixture.reply(&old, Vec::new());
    let crate::plugins::OwnedPluginEvent::Executed(reply) =
        app.extension.poll_owned().unwrap().pop().unwrap()
    else {
        panic!("execution reply");
    };
    let error = app
        .extension
        .receive(
            reply,
            &app.workspace.version(),
            app.configuration.binding_revision(),
        )
        .err()
        .expect("pre-reset invocation must be discarded");
    assert!(error.contains("discarded"), "{error}");
    assert_eq!(app.extension.confirmation_count(), 0);
    invoke(&mut app);
    let current = fixture.take();
    fixture.reply(&current, Vec::new());
    let crate::plugins::OwnedPluginEvent::Executed(reply) =
        app.extension.poll_owned().unwrap().pop().unwrap()
    else {
        panic!("execution reply");
    };
    assert!(
        app.extension
            .receive(
                reply,
                &app.workspace.version(),
                app.configuration.binding_revision()
            )
            .is_ok()
    );
}

#[test]
fn stale_load_cleanup_preserves_replacement_ui_events() {
    let (_app_dirs, mut app) = app();
    let event = |token| crate::plugins::PluginUiEvent {
        token,
        handle: None,
        graph_revision: None,
        tree_revision: None,
        surface_revision: None,
        geometry: None,
        action: None,
        error: None,
    };
    app.extension
        .queue_ui("demo".into(), PluginLoadId(77), [event(1)]);
    app.extension
        .queue_ui("demo".into(), PluginLoadId(78), [event(2)]);
    let storage_result = |load, token| crate::plugins::PluginStorageResult {
        plugin: "demo".into(),
        load: PluginLoadId(load),
        token,
        value: None,
        revision: None,
        entries: Vec::new(),
        truncated: false,
        error: None,
    };
    app.extension.queue_storage(storage_result(77, 1));
    app.extension.queue_storage(storage_result(78, 2));
    app.extension.install_storage("demo", PluginLoadId(78));
    let manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    let replacement =
        crate::plugins::index_manifest_contributions(&manifest, 78, "replacement manifest".into())
            .expect("replacement index");
    app.extension
        .fixture_contribution(replacement)
        .expect("replacement catalog");
    app.remove_plugin_contributions("demo", PluginLoadId(77));
    assert!(
        !app.extension
            .ui_events()
            .contains_key(&("demo".into(), PluginLoadId(77)))
    );
    assert_eq!(
        app.extension
            .ui_events()
            .get(&("demo".into(), PluginLoadId(78)))
            .and_then(|events| events.first())
            .map(|event| event.token),
        Some(2)
    );
    assert_eq!(
        app.extension
            .contributions()
            .get("demo")
            .map(|index| index.plugin_load),
        Some(78)
    );
    assert!(
        !app.extension
            .storage_results()
            .contains_key(&("demo".into(), PluginLoadId(77)))
    );
    assert_eq!(
        app.extension
            .storage_results()
            .get(&("demo".into(), PluginLoadId(78)))
            .and_then(|results| results.first())
            .map(|result| result.token),
        Some(2)
    );
    app.extension
        .queue_ui("demo".into(), PluginLoadId(78), (10..80).map(event));
    assert_eq!(
        app.extension.ui_events()[&("demo".into(), PluginLoadId(78))].len(),
        71
    );
    assert!(
        app.extension
            .push_action("demo".into(), PluginLoadId(78), event(999))
            .is_err()
    );
    let delivered = app.extension.take_ui("demo", PluginLoadId(78));
    assert_eq!(delivered.len(), 71);
    assert_eq!(delivered.last().unwrap().token, 79);
    assert!(app.extension.take_ui("demo", PluginLoadId(78)).is_empty());
}

#[test]
fn expected_index_failure_before_sibling_index_aborts_the_complete_batch() {
    let (_app_dirs, mut app) = app();
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.deferred_plugin_keymaps = true;
        fixture.expected_plugin_indexes =
            ["bad".to_owned(), "good".to_owned()].into_iter().collect();
        app.configuration.install_fixture(fixture);
    }
    let bad = tempfile::tempdir().expect("bad plugin store");
    fs::write(bad.path().join("manifest.toml"), "not = [valid").expect("bad manifest");
    let good = tempfile::tempdir().expect("good plugin store");
    write_plugin_manifest(
        good.path(),
        &plugin_manifest("good", "plugin.good.keymap.dashboard", "x y"),
    );
    let limits = app.plugin_limits();
    let bad_load = app
        .extension
        .index_from_store(
            "bad".into(),
            bad.path().to_path_buf(),
            PathBuf::from("manifest.toml"),
            PathBuf::from("missing.wasm"),
            limits.clone(),
        )
        .expect("bad index request");
    let good_load = app
        .extension
        .index_from_store(
            "good".into(),
            good.path().to_path_buf(),
            PathBuf::from("manifest.toml"),
            PathBuf::from("missing.wasm"),
            limits,
        )
        .expect("good index request");
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.extension.load_id("good").is_some()
        || app.extension.load_id("bad").is_some()
        || !app.extension.pending_indexes().is_empty()
    {
        app.process_plugin_events();
        assert!(Instant::now() < deadline, "batch rejection timed out");
        thread::yield_now();
    }
    app.process_plugin_events();
    assert!(app.extension.contributions().get("bad").is_none());
    assert!(app.extension.contributions().get("good").is_none());
    assert!(
        app.configuration
            .keymaps()
            .inspect_id("plugin.good.keymap.dashboard")
            .is_none()
    );
    assert!(app.extension.pending_indexes().is_empty());
    assert!(!app.extension.is_active_load("bad", bad_load));
    assert!(!app.extension.is_active_load("good", good_load));
}

#[test]
fn deferred_user_plugin_move_compiles_after_index_then_falls_back_after_activation_failure() {
    let (_app_dirs, mut app) = app();
    let root = tempfile::tempdir().expect("plugin store");
    write_plugin_manifest(
        root.path(),
        &plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y"),
    );
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.plugins.entries.insert(
        "demo".into(),
        crate::config::PluginEntryConfig {
            manifest: PathBuf::from("manifest.toml"),
            component: PathBuf::from("missing.wasm"),
            enabled: true,
        },
    );
    loaded.config.windows.rules = vec![crate::config::WindowRuleConfig {
        content_type: "plugin.demo.panel".into(),
        placement: crate::config::WindowPlacementConfig::Floating {
            x: 4,
            y: 3,
            width: 50,
            height: 12,
        },
    }];
    loaded.keymaps.entries.extend([
        crate::config::KeymapEntry {
            id: "plugin.demo.keymap.dashboard".into(),
            context: Some("dashboard".into()),
            mode: String::new(),
            keys: Some("z".into()),
            command: Some("plugin.demo.run".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
        },
        crate::config::KeymapEntry {
            id: "user.test.demo-retained-next".into(),
            context: Some("plugin.demo.chat".into()),
            mode: String::new(),
            keys: Some("j".into()),
            command: Some("surface.retained.focus.next".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
        },
    ]);
    let (configuration, diagnostic) =
        crate::app::configuration::resolve_configuration(&app.commands, loaded, true)
            .expect("deferred config");
    assert!(diagnostic.is_none());
    assert!(configuration.deferred_plugin_keymaps);
    assert!(configuration.settings.windows.rules.is_empty());
    assert_eq!(configuration.raw_windows.rules.len(), 1);
    app.configuration.install_fixture(configuration);
    let load = app
        .extension
        .index_from_store(
            "demo".into(),
            root.path().to_path_buf(),
            PathBuf::from("manifest.toml"),
            PathBuf::from("missing.wasm"),
            app.plugin_limits(),
        )
        .expect("index request");
    let deadline = Instant::now() + Duration::from_secs(2);
    while app.extension.contributions().get("demo").is_none() {
        app.process_plugin_events();
        assert!(Instant::now() < deadline, "static acceptance timed out");
        thread::yield_now();
    }
    let moved = app
        .configuration
        .keymaps()
        .inspect_id("plugin.demo.keymap.dashboard")
        .expect("user replacement");
    assert_eq!(moved.keys, "z");
    assert_eq!(moved.contribution_layer, InputContextLayer::User);
    assert_eq!(
        app.configuration
            .keymaps()
            .inspect_id("user.test.demo-retained-next")
            .map(|mapping| mapping.context),
        Some("plugin.demo.chat".into())
    );
    assert_eq!(
        app.configuration.settings().windows.rules[0].content_type,
        "plugin.demo.panel"
    );
    assert!(
        app.configuration
            .keymaps()
            .inspect_key("dashboard", "x y")
            .is_none()
    );
    while app.extension.contributions().get("demo").is_some() {
        app.process_plugin_events();
        assert!(Instant::now() < deadline, "activation rollback timed out");
        thread::yield_now();
    }
    assert!(!app.extension.is_active_load("demo", load));
    assert!(
        app.configuration
            .keymaps()
            .inspect_id("plugin.demo.keymap.dashboard")
            .is_none()
    );
    assert!(
        app.configuration
            .keymaps()
            .inspect_id("user.test.demo-retained-next")
            .is_none()
    );
    assert!(app.configuration.settings().windows.rules.is_empty());
}

#[test]
fn enabled_plugin_references_are_deferred_but_unknown_namespaces_still_reject() {
    let (_app_dirs, app) = app();
    let mut loaded = crate::config::load_bundled_config();
    loaded.config.plugins.entries.insert(
        "demo".into(),
        crate::config::PluginEntryConfig {
            manifest: PathBuf::from("demo.toml"),
            component: PathBuf::from("demo.wasm"),
            enabled: true,
        },
    );
    loaded
        .config
        .windows
        .rules
        .push(crate::config::WindowRuleConfig {
            content_type: "plugin.demo.panel".into(),
            placement: crate::config::WindowPlacementConfig::Automatic,
        });
    loaded.keymaps.entries.extend([
        crate::config::KeymapEntry {
            id: "plugin.demo.keymap.default".into(),
            context: None,
            mode: String::new(),
            keys: None,
            command: None,
            arguments: BTreeMap::new(),
            unmap: false,
            disable: true,
            pass_through: false,
            desc: None,
        },
        crate::config::KeymapEntry {
            id: "user.test.demo-command".into(),
            context: Some("dashboard".into()),
            mode: String::new(),
            keys: Some("z".into()),
            command: Some("plugin.demo.run".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
        },
        crate::config::KeymapEntry {
            id: "user.test.demo-retained-context".into(),
            context: Some("plugin.demo.chat".into()),
            mode: String::new(),
            keys: Some("j".into()),
            command: Some("surface.retained.focus.next".into()),
            arguments: BTreeMap::new(),
            unmap: false,
            disable: false,
            pass_through: false,
            desc: None,
        },
    ]);
    let (resolved, diagnostic) =
        crate::app::configuration::resolve_configuration(&app.commands, loaded, true)
            .expect("deferred plugin references");
    assert!(diagnostic.is_none());
    assert!(resolved.deferred_plugin_keymaps);
    assert!(resolved.expected_plugin_indexes.contains("demo"));
    assert!(
        resolved
            .settings
            .windows
            .rules
            .iter()
            .all(|rule| !rule.content_type.starts_with("plugin."))
    );
    assert!(
        resolved
            .raw_windows
            .rules
            .iter()
            .any(|rule| rule.content_type == "plugin.demo.panel")
    );
    assert!(
        resolved
            .keymaps
            .inspect_id("plugin.demo.keymap.default")
            .is_none()
    );
    assert!(
        resolved
            .keymaps
            .inspect_id("user.test.demo-command")
            .is_none()
    );

    let mut unknown = crate::config::load_bundled_config();
    unknown.keymaps.entries.push(crate::config::KeymapEntry {
        id: "user.test.unknown-plugin".into(),
        context: Some("dashboard".into()),
        mode: String::new(),
        keys: Some("z".into()),
        command: Some("plugin.unknown.run".into()),
        arguments: BTreeMap::new(),
        unmap: false,
        disable: false,
        pass_through: false,
        desc: None,
    });
    assert!(
        crate::app::configuration::resolve_configuration(&app.commands, unknown, true).is_err()
    );
    let mut unknown_window = crate::config::load_bundled_config();
    unknown_window
        .config
        .windows
        .rules
        .push(crate::config::WindowRuleConfig {
            content_type: "plugin.unknown.panel".into(),
            placement: crate::config::WindowPlacementConfig::Hidden,
        });
    assert!(
        crate::app::configuration::resolve_configuration(&app.commands, unknown_window, true)
            .is_err()
    );

    let (_running_dirs, mut running) = self::app();
    let previous = running.configuration.settings().plugins.clone();
    let mut changed = crate::config::load_bundled_config();
    changed.config.plugins.entries.insert(
        "demo".into(),
        crate::config::PluginEntryConfig {
            manifest: PathBuf::from("demo.toml"),
            component: PathBuf::from("demo.wasm"),
            enabled: true,
        },
    );
    let error = running
        .apply_configuration_candidate(changed, false)
        .expect_err("plugin topology reload must stage before mutation");
    assert!(error.contains("plugin topology"));
    assert_eq!(running.configuration.settings().plugins, previous);
}

#[test]
fn deferred_batch_abort_and_worker_loss_clear_static_ownership() {
    pub(in crate::app) fn index(
        owner: &str,
        plugin_load: u64,
    ) -> crate::plugins::PluginContributionIndex {
        crate::plugins::PluginContributionIndex {
            capabilities: Default::default(),
            owner: owner.into(),
            version: "1.0.0".into(),
            checksum: "0".repeat(64),
            plugin_load,
            source: format!("test:{owner}"),
            commands: BTreeMap::new(),
            contexts: Vec::new(),
            keymaps: Vec::new(),
            key_groups: Vec::new(),
            content_types: Vec::new(),
            subscriptions: Vec::new(),
            providers: Vec::new(),
            virtual_documents: Vec::new(),
            storage: None,
        }
    }

    let (_app_dirs, mut app) = app();
    for (owner, load) in [("one", PluginLoadId(41)), ("two", PluginLoadId(42))] {
        app.extension
            .fixture_contribution(index(owner, load.0))
            .expect("provisional index");
        app.extension
            .fixture_pending_index(owner.into(), (load, "0".repeat(64)));
    }
    app.abort_pending_index_batch("expected index failed");
    assert!(app.extension.pending_indexes().is_empty());
    assert!(app.extension.contributions().get("one").is_none());
    assert!(app.extension.contributions().get("two").is_none());
    assert!(
        app.extension
            .rejected_indexes()
            .contains(&("one".into(), PluginLoadId(41)))
    );
    assert!(
        app.extension
            .rejected_indexes()
            .contains(&("two".into(), PluginLoadId(42)))
    );

    app.extension
        .fixture_contribution(index("three", 43))
        .expect("accepted index");
    app.extension
        .fixture_pending_index("three".into(), (PluginLoadId(43), "0".repeat(64)));
    app.handle_plugin_worker_stopped(crate::plugins::PluginError::Stopped);
    assert!(app.extension.pending_indexes().is_empty());
    assert!(app.extension.rejected_indexes().is_empty());
    assert!(app.extension.contributions().get("three").is_none());
    assert!(
        app.commands
            .specs()
            .all(|spec| !matches!(spec.source, CommandSource::Plugin(_)))
    );
}

#[test]
fn provider_chunks_use_the_revision_after_virtual_publication() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let mut manifest = plugin_manifest("demo", "plugin.demo.test", "x y");
    manifest.capabilities.extend([
        crate::plugins::PluginCapability::VirtualDocuments,
        crate::plugins::PluginCapability::PickerProviders,
    ]);
    manifest.contributions.virtual_documents =
        vec![crate::plugins::PluginVirtualDocumentContribution {
            id: "plugin.demo.virtual.help".into(),
            title: "Help".into(),
            language: "markdown".into(),
            mode: crate::plugins::PluginVirtualDocumentMode::ReadOnly,
            maximum_concurrent_documents: 1,
            maximum_document_bytes: 4096,
            maximum_document_lines: 128,
            maximum_patch_bytes: 256,
            maximum_patches_per_request: 8,
            deadline_milliseconds: 30_000,
            command: "plugin.demo.run".into(),
            context: "dashboard".into(),
            provenance: None,
        }];
    manifest.contributions.providers = vec![crate::plugins::PluginProviderContribution {
        id: "plugin.demo.provider.files".into(),
        kind: crate::plugins::PluginProviderKind::PickerSource,
        command: "plugin.demo.run".into(),
        maximum_concurrent_requests: 1,
        maximum_items: 16,
        maximum_bytes: 8192,
        deadline_milliseconds: 30_000,
        provenance: None,
    }];
    let index = crate::plugins::index_manifest_contributions(&manifest, 1, "test".into()).unwrap();
    app.extension.fixture_contribution(index.clone()).unwrap();
    app.extension.fixture_provider_install(&index).unwrap();
    let virtual_request = app.extension.fixture_virtual_request(&index);
    let request = app
        .extension
        .fixture_provider_start(
            crate::plugins::PluginProviderStart {
                plugin: "demo".into(),
                load: PluginLoadId(1),
                provider: "plugin.demo.provider.files".into(),
                query: String::new(),
                context: "dashboard".into(),
                workspace_revision: Some(app.workspace.root_revision()),
                buffer_revision: None,
            },
            &app.state,
        )
        .unwrap();
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    fixture.reply_with(&fixture.take(), |execution| {
        execution
            .virtual_document_proposals
            .push(crate::plugins::PluginVirtualDocumentProposal {
                request: virtual_request.request,
                document: virtual_request.document,
                cancellation_id: virtual_request.cancellation_id,
                kind: crate::plugins::PluginVirtualDocumentProposalKind::Full(
                    crate::plugins::PluginVirtualDocumentFull {
                        expected_revision: None,
                        revision: 1,
                        title: "Help".into(),
                        language: "markdown".into(),
                        text: "new virtual text".into(),
                    },
                ),
            });
        execution
            .provider_chunks
            .push(crate::plugins::PluginPickerChunk {
                request,
                sequence: 1,
                items: vec![crate::plugins::PluginPickerItem {
                    id: "old".into(),
                    primary: "Stale row".into(),
                    secondary: None,
                    action_data: Vec::new(),
                }],
                terminal: crate::plugins::PluginProviderTerminal::Complete,
            });
    });
    app.process_plugin_events();
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "new virtual text"
    );
    assert!(
        app.extension
            .provider_projection(request)
            .is_none_or(|projection| projection.items.is_empty())
    );
    assert!(app.ui.notification_error);
}

#[test]
fn virtual_publication_keeps_original_request_until_checked_window_acceptance() {
    use crate::app::plugins::VirtualContext;
    let (_app_dirs, mut app) = app();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::plugins::PluginCapability::VirtualDocuments);
    manifest.contributions.virtual_documents =
        vec![crate::plugins::PluginVirtualDocumentContribution {
            id: "plugin.demo.virtual.help".into(),
            title: "Help".into(),
            language: "markdown".into(),
            mode: crate::plugins::PluginVirtualDocumentMode::ReadOnly,
            maximum_concurrent_documents: 1,
            maximum_document_bytes: 4096,
            maximum_document_lines: 128,
            maximum_patch_bytes: 256,
            maximum_patches_per_request: 8,
            deadline_milliseconds: 30_000,
            command: "plugin.demo.run".into(),
            context: "dashboard".into(),
            provenance: None,
        }];
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 7, "virtual fixture".into())
            .unwrap();
    app.extension.fixture_contribution(index.clone()).unwrap();
    let request = app.extension.fixture_virtual_request(&index);
    let proposal = crate::plugins::PluginVirtualDocumentProposal {
        request: request.request,
        document: request.document,
        cancellation_id: request.cancellation_id,
        kind: crate::plugins::PluginVirtualDocumentProposalKind::Full(
            crate::plugins::PluginVirtualDocumentFull {
                expected_revision: None,
                revision: 1,
                title: "Help".into(),
                language: "markdown".into(),
                text: "# Original source\n".into(),
            },
        ),
    };
    let before = app.state.version();
    assert!(
        app.extension
            .prepare_virtual(
                "other",
                PluginLoadId(7),
                proposal.clone(),
                true,
                VirtualContext {
                    editor: &app.state,
                    windows: &app.window_session,
                    scratch: "scratch"
                }
            )
            .is_err()
    );
    let (source, candidate) = app
        .extension
        .prepare_virtual(
            "demo",
            PluginLoadId(7),
            proposal.clone(),
            true,
            VirtualContext {
                editor: &app.state,
                windows: &app.window_session,
                scratch: "scratch",
            },
        )
        .unwrap();
    // Retiring the window candidate cannot consume the original broker request.
    app.window_session.invalidate_native();
    assert!(
        app.window_session
            .accept_editor_transition(candidate, source.before(), source.catalog())
            .is_err()
    );
    drop(source);
    assert_eq!(app.state.version(), before);
    let (source, candidate) = app
        .extension
        .prepare_virtual(
            "demo",
            PluginLoadId(7),
            proposal.clone(),
            true,
            VirtualContext {
                editor: &app.state,
                windows: &app.window_session,
                scratch: "scratch",
            },
        )
        .unwrap();
    let accepted = app
        .window_session
        .accept_editor_transition(candidate, source.before(), source.catalog())
        .unwrap();
    *app.state = source.finish(accepted).unwrap();
    let virtual_buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(
        app.state.buffers[virtual_buffer].text.to_string(),
        "# Original source\n"
    );
    assert!(
        app.extension
            .prepare_virtual(
                "demo",
                PluginLoadId(7),
                proposal,
                true,
                VirtualContext {
                    editor: &app.state,
                    windows: &app.window_session,
                    scratch: "scratch"
                }
            )
            .is_err()
    );
    let (ordinary, view) = app
        .state
        .open_text(TextBuffer::from_text("ordinary"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let focus = app.window_session.graph().focused();
    app.remove_plugin_contributions("demo", PluginLoadId(7));
    assert!(!app.state.buffers.contains_key(virtual_buffer));
    assert_eq!(app.state.active_buffer_id(), Some(ordinary));
    assert_eq!(app.state.active_view(), Some(view));
    assert_eq!(app.window_session.graph().focused(), focus);
    assert_eq!(app.state.buffers[ordinary].text.to_string(), "ordinary");
}

#[test]
fn virtual_document_titles_and_languages_reach_windows_and_syntax() {
    let (_app_dirs, mut app) = app();
    let before = app.state.version();
    let mut draft = app.state.draft();
    let mut snapshot = crate::plugins::PluginVirtualDocumentSnapshot {
        plugin: "demo".into(),
        load: PluginLoadId(7),
        document: 3,
        source: "plugin.demo.virtual.help".into(),
        key: "help".into(),
        title: "Help".into(),
        language: "rust".into(),
        text: "fn help() {}\n".into(),
        revision: 1,
        closed: false,
    };
    let buffer = app
        .extension
        .fixture_virtual_publish(&mut draft, &snapshot)
        .unwrap();
    *app.state = draft.accept(&before).unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    assert!(app.ui.content_tabs.iter().any(|tab| tab.title == "Help"));
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.syntax.result(buffer).is_none() {
        app.process_syntax();
        assert!(
            Instant::now() < deadline,
            "declared Rust language was not parsed"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        app.syntax.result(buffer).unwrap().language,
        crate::language::syntax::SyntaxLanguage::Rust
    );
    snapshot.revision = 2;
    snapshot.title = "Updated help".into();
    snapshot.language = "python".into();
    snapshot.text = "def help(): pass\n".into();
    let before = app.state.version();
    let mut draft = app.state.draft();
    assert_eq!(
        app.extension
            .fixture_virtual_publish(&mut draft, &snapshot)
            .unwrap(),
        buffer
    );
    *app.state = draft.accept(&before).unwrap();
    app.reconcile_editor_windows();
    assert!(
        app.ui
            .content_tabs
            .iter()
            .any(|tab| tab.title == "Updated help")
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        app.process_syntax();
        if app.syntax.result(buffer).is_some_and(|result| {
            result.language == crate::language::syntax::SyntaxLanguage::Python
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "updated language was not parsed");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn provider_selection_delivers_request_item_and_action_data() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::plugins::PluginCapability::PickerProviders);
    manifest.contributions.providers = vec![crate::plugins::PluginProviderContribution {
        id: "plugin.demo.provider.files".into(),
        kind: crate::plugins::PluginProviderKind::PickerSource,
        command: "plugin.demo.run".into(),
        maximum_concurrent_requests: 1,
        maximum_items: 16,
        maximum_bytes: 8192,
        deadline_milliseconds: 30_000,
        provenance: None,
    }];
    let index = crate::plugins::index_manifest_contributions(&manifest, 1, "picker fixture".into())
        .unwrap();
    app.extension.fixture_contribution(index.clone()).unwrap();
    app.extension.fixture_provider_install(&index).unwrap();
    let request = app
        .extension
        .fixture_provider_start(
            crate::plugins::PluginProviderStart {
                plugin: "demo".into(),
                load: PluginLoadId(1),
                provider: "plugin.demo.provider.files".into(),
                query: String::new(),
                context: String::new(),
                workspace_revision: None,
                buffer_revision: None,
            },
            &app.state,
        )
        .unwrap();
    app.extension
        .provider_chunk(
            "demo",
            PluginLoadId(1),
            (None, None),
            crate::plugins::PluginPickerChunk {
                request,
                sequence: 1,
                items: vec![crate::plugins::PluginPickerItem {
                    id: "selected".into(),
                    primary: "Selected item".into(),
                    secondary: None,
                    action_data: vec![0, 255],
                }],
                terminal: crate::plugins::PluginProviderTerminal::Complete,
            },
        )
        .unwrap();
    app.extension.picker_installed(Some(request));
    app.present_plugin_picker(request);
    app.accept_picker();
    assert_eq!(
        fixture.take().arguments(),
        &[request.to_string(), "selected".into(), "[0,255]".into()]
    );
    assert!(app.extension.provider_projection(request).is_none());
}

#[test]
fn plugin_surface_observations_include_counts_and_geometry_preserves_results() {
    let (_app_dirs, mut app) = app();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::plugins::PluginCapability::EventSubscriptions);
    manifest.contributions.subscriptions = vec![crate::plugins::PluginSubscriptionContribution {
        id: "plugin.demo.subscription.surface".into(),
        kind: crate::plugins::PluginSubscriptionKind::PluginSurfaceChanged,
        maximum_queued: 1,
        maximum_rate_per_second: 30,
        coalesce: true,
        provenance: None,
    }];
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 1, "surface fixture".into())
            .unwrap();
    app.extension.fixture_contribution(index.clone()).unwrap();
    app.extension.install_events(&index).unwrap();
    let observation = (1, "editor".into(), None, 1);
    app.extension.observe(observation.clone(), 1, 0, 0);
    app.extension.observe(observation, 1, 0, 1);
    assert_eq!(
        app.extension.deliver("demo", PluginLoadId(1))[0].payload,
        "retained=0;private=1"
    );
    let event = |token, width: Option<u16>| crate::plugins::PluginUiEvent {
        token,
        handle: Some(5),
        graph_revision: None,
        tree_revision: None,
        surface_revision: None,
        action: None,
        error: None,
        geometry: width.map(|width| crate::plugins::PluginSurfaceGeometry {
            handle: 5,
            width,
            height: 3,
            dimensions_revision: 1,
        }),
    };
    app.extension.queue_ui(
        "demo".into(),
        PluginLoadId(1),
        (1..=64).map(|token| event(token, None)),
    );
    app.extension.queue_ui(
        "demo".into(),
        PluginLoadId(1),
        [event(0, Some(20)), event(0, Some(30))],
    );
    let events = app.extension.take_ui("demo", PluginLoadId(1));
    assert_eq!(events.len(), 65);
    assert_eq!(events[0].token, 1);
    assert_eq!(events.last().unwrap().geometry.as_ref().unwrap().width, 30);
}

#[test]
fn conflicting_plugin_defaults_resolve_after_user_key_and_group_overrides() {
    let (_app_dirs, mut app) = app();
    let mut catalog = crate::plugins::PluginContributionCatalog::default();
    for owner in ["demo", "other"] {
        let mut manifest = plugin_manifest(owner, &format!("plugin.{owner}.keymap.open"), "x y");
        manifest
            .contributions
            .key_groups
            .push(crate::plugins::PluginKeyGroupContribution {
                id: format!("plugin.{owner}.group.open"),
                context: "dashboard".into(),
                mode: String::new(),
                prefix: "x".into(),
                label: owner.into(),
                icon: None,
                order: 1,
            });
        let index =
            crate::plugins::index_manifest_contributions(&manifest, 1, "keys fixture".into())
                .unwrap();
        app.register_indexed_plugin_commands(&index).unwrap();
        catalog.install(index).unwrap();
    }
    let mut loaded = crate::config::load_bundled_config();
    let compile = |loaded: &crate::config::LoadedConfig| {
        crate::app::configuration::compile_keymaps_with_plugin_catalog(
            &app.commands,
            &loaded.keymaps,
            &catalog,
            &loaded.config.input.keymaps,
        )
    };
    assert!(compile(&loaded).is_err());
    loaded
        .keymaps
        .entries
        .push(toml::from_str("id='plugin.other.keymap.open'\ncontext='dashboard'\nkeys='z y'\ncommand='plugin.other.run'").unwrap());
    loaded.keymaps.groups.push(
        toml::from_str(
            "id='plugin.other.group.open'\ncontext='dashboard'\nprefix='z'\nlabel='Other'",
        )
        .unwrap(),
    );
    compile(&loaded).expect("user replacements resolve the default conflicts");
}

#[test]
fn virtual_open_deduplicates_requests_and_cancels_after_navigation() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::plugins::PluginCapability::VirtualDocuments);
    manifest.contributions.virtual_documents =
        vec![crate::plugins::PluginVirtualDocumentContribution {
            id: "plugin.demo.virtual.help".into(),
            title: "Help".into(),
            language: "markdown".into(),
            mode: crate::plugins::PluginVirtualDocumentMode::ReadOnly,
            maximum_concurrent_documents: 2,
            maximum_document_bytes: 4096,
            maximum_document_lines: 128,
            maximum_patch_bytes: 256,
            maximum_patches_per_request: 8,
            deadline_milliseconds: 30_000,
            command: "plugin.demo.run".into(),
            context: "dashboard".into(),
            provenance: None,
        }];
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 1, "virtual fixture".into())
            .unwrap();
    app.extension.fixture_contribution(index.clone()).unwrap();
    let request = app.extension.fixture_virtual_request(&index);
    app.state
        .open_text(TextBuffer::from_text("original"), None)
        .unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let navigation = app
        .window_session
        .navigation_stamp(app.state.window_version(), app.state.active_view());
    for _ in 0..2 {
        app.process_effect(AppEffect::VirtualDocumentOpen {
            source: "plugin.demo.virtual.help".into(),
            key: "help".into(),
        });
        fixture.assert_no_requests();
    }
    app.state.new_tab().unwrap();
    app.reconcile_editor_windows();
    let before = app.state.version();
    let active = app.state.active_tab();
    let navigation_current = app.window_session.navigation_current(
        &navigation,
        &app.state.window_version(),
        app.state.active_view(),
    );
    assert!(!navigation_current);
    let proposal = crate::plugins::PluginVirtualDocumentProposal {
        request: request.request,
        document: request.document,
        cancellation_id: request.cancellation_id,
        kind: crate::plugins::PluginVirtualDocumentProposalKind::Full(
            crate::plugins::PluginVirtualDocumentFull {
                expected_revision: None,
                revision: 1,
                title: "Help".into(),
                language: "rust".into(),
                text: "fn help() {}".into(),
            },
        ),
    };
    assert!(
        app.extension
            .prepare_virtual(
                "demo",
                PluginLoadId(1),
                proposal,
                navigation_current,
                crate::app::plugins::VirtualContext {
                    editor: &app.state,
                    windows: &app.window_session,
                    scratch: "scratch",
                }
            )
            .is_err()
    );
    assert_eq!(app.state.version(), before);
    assert_eq!(app.state.active_tab(), active);
    assert!(
        app.state
            .buffers
            .values()
            .all(|buffer| !buffer.is_virtual_read_only())
    );
    assert!(matches!(
        app.extension
            .open_virtual("plugin.demo.virtual.help", "help".into(), &app.state),
        Ok(crate::app::plugins::VirtualOpen::Execute { .. })
    ));
}

#[test]
fn deferred_activation_queue_failure_aborts_unsubmitted_siblings() {
    let (_app_dirs, mut app) = app();
    let (mut manager, fixture) = crate::plugins::ExecutionFixture::new(1);
    manager
        .execute(crate::plugins::empty_execution_request())
        .unwrap();
    app.extension.fixture_manager(manager);
    let mut config = app.configuration.fixture_snapshot();
    config.deferred_plugin_keymaps = true;
    config.expected_plugin_indexes = ["demo".into(), "zeta".into()].into_iter().collect();
    app.configuration.install_fixture(config);
    let sibling = crate::plugins::index_manifest_contributions(
        &plugin_manifest("zeta", "plugin.zeta.test", "z z"),
        2,
        "sibling".into(),
    )
    .unwrap();
    app.register_indexed_plugin_commands(&sibling).unwrap();
    app.extension.fixture_contribution(sibling).unwrap();
    app.extension
        .fixture_pending_index("zeta".into(), (PluginLoadId(2), "0".repeat(64)));
    fixture.indexed(
        crate::plugins::index_manifest_contributions(
            &plugin_manifest("demo", "plugin.demo.test", "x y"),
            1,
            "test".into(),
        )
        .unwrap(),
    );
    app.process_plugin_events();
    assert!(app.extension.pending_indexes().is_empty());
    assert!(app.extension.contributions().get("zeta").is_none());
    assert!(
        app.extension
            .rejected_indexes()
            .contains(&("zeta".into(), PluginLoadId(2)))
    );
    assert!(
        app.ui
            .notification
            .as_deref()
            .unwrap()
            .contains("queue is full"),
        "{:?}",
        app.ui.notification
    );
}

#[test]
fn rejected_package_commands_schedule_the_installed_version_for_reload() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let manifest = plugin_manifest("demo", "plugin.demo.test", "x y");
    app.extension
        .fixture_contribution(
            crate::plugins::index_manifest_contributions(&manifest, 1, "test".into()).unwrap(),
        )
        .unwrap();
    app.extension.fixture_prepared("demo", PluginLoadId(1), 41);
    fixture.loaded(
        PluginLoadId(1),
        crate::plugins::LoadedComponent {
            canonical_path: "replacement.wasm".into(),
            bytes: Vec::new(),
            manifest,
            instantiated: true,
            unavailable_host: None,
            activated_commands: Vec::new(),
        },
    );
    app.process_plugin_events();
    app.process_package_events();
    assert!(app.extension.fixture_package_reload_pending());
    assert!(
        app.extension
            .take_prepared("demo", PluginLoadId(1))
            .is_none()
    );
    assert!(!app.extension.is_active_load("demo", PluginLoadId(1)));
    assert!(app.extension.contributions().get("demo").is_none());
}

#[test]
fn active_plugin_failure_does_not_abort_a_siblings_index() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let mut config = app.configuration.fixture_snapshot();
    config.deferred_plugin_keymaps = true;
    config.expected_plugin_indexes = ["demo".into(), "sibling".into()].into_iter().collect();
    app.configuration.install_fixture(config);
    app.configuration.complete_plugin_indexes();
    app.extension
        .fixture_pending_index("sibling".into(), (PluginLoadId(2), "0".repeat(64)));
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    fixture.fail(&fixture.take());
    app.process_plugin_events();
    assert!(app.extension.pending_indexes().contains_key("sibling"));
    assert_eq!(
        app.ui.notification.as_deref(),
        Some("Plugin demo: plugin is not active")
    );
    assert!(app.ui.notification_error);
}

#[test]
fn synchronous_plugin_load_refusal_cancels_the_deferred_batch() {
    let (_app_dirs, mut app) = app_with_plugin_queue(1);
    let root = tempfile::tempdir().unwrap();
    for owner in ["one", "two"] {
        fs::create_dir(root.path().join(owner)).unwrap();
        write_plugin_manifest(
            &root.path().join(owner),
            &plugin_manifest(owner, &format!("plugin.{owner}.test"), "x y"),
        );
    }
    let mut config = app.configuration.fixture_snapshot();
    config.deferred_plugin_keymaps = true;
    config.expected_plugin_indexes = ["one".into(), "two".into()].into_iter().collect();
    config.settings.plugins.store = root.path().into();
    for owner in ["one", "two"] {
        config.settings.plugins.entries.insert(
            owner.into(),
            crate::config::PluginEntryConfig {
                enabled: true,
                manifest: format!("{owner}/manifest.toml").into(),
                component: format!("{owner}/missing.wasm").into(),
            },
        );
    }
    app.configuration.install_fixture(config);
    app.load_configured_plugins();
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.extension.load_id("one").is_some() {
        app.process_plugin_events();
        assert!(Instant::now() < deadline);
        thread::yield_now();
    }
    assert!(app.extension.pending_indexes().is_empty());
    assert!(app.extension.contributions().get("one").is_none());
}

#[test]
fn plugin_command_arguments_reach_the_existing_execution_request() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let mut manifest = plugin_manifest("demo", "plugin.demo.test", "x y");
    manifest.commands.get_mut("run").unwrap().arguments.insert(
        "value".into(),
        crate::plugins::PluginArgumentDeclaration {
            kind: crate::plugins::PluginArgumentKind::String,
            required: true,
        },
    );
    manifest.contributions.keymaps[0]
        .arguments
        .insert("value".into(), toml::Value::String("configured".into()));
    let index = crate::plugins::index_manifest_contributions(&manifest, 1, "test".into()).unwrap();
    app.register_indexed_plugin_commands(&index).unwrap();
    app.invoke(
        crate::editor::CommandInvocation::new("plugin.demo.run", InvocationSource::Test)
            .unwrap()
            .with_argument("value", CommandValue::String("configured".into())),
    );
    let captured = fixture.take();
    assert_eq!(
        crate::app::plugins::abi::decode_value(captured.call().arguments.clone()).unwrap(),
        CommandValue::Record(BTreeMap::from([(
            "value".into(),
            CommandValue::String("configured".into())
        )]))
    );
}

#[test]
fn repeated_virtual_open_does_not_dispatch_an_empty_callback() {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::plugins::PluginCapability::VirtualDocuments);
    manifest.contributions.virtual_documents =
        vec![crate::plugins::PluginVirtualDocumentContribution {
            id: "plugin.demo.virtual.help".into(),
            title: "Help".into(),
            language: "markdown".into(),
            mode: crate::plugins::PluginVirtualDocumentMode::ReadOnly,
            maximum_concurrent_documents: 2,
            maximum_document_bytes: 4096,
            maximum_document_lines: 128,
            maximum_patch_bytes: 256,
            maximum_patches_per_request: 8,
            deadline_milliseconds: 30_000,
            command: "plugin.demo.run".into(),
            context: "dashboard".into(),
            provenance: None,
        }];
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 1, "virtual fixture".into())
            .unwrap();
    app.extension.fixture_contribution(index.clone()).unwrap();
    app.extension.fixture_virtual_request(&index);
    for _ in 0..2 {
        app.process_effect(AppEffect::VirtualDocumentOpen {
            source: "plugin.demo.virtual.help".into(),
            key: "help".into(),
        });
        fixture.assert_no_requests();
    }
}

#[test]
fn plugin_picker_provider_uses_the_generic_presenter_and_cancels_exactly_slot5() {
    let (_app_dirs, mut app) = app();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::plugins::PluginCapability::PickerProviders);
    manifest.contributions.providers = vec![crate::plugins::PluginProviderContribution {
        id: "plugin.demo.provider.files".into(),
        kind: crate::plugins::PluginProviderKind::PickerSource,
        command: "plugin.demo.run".into(),
        maximum_concurrent_requests: 1,
        maximum_items: 16,
        maximum_bytes: 8_192,
        deadline_milliseconds: 30_000,
        provenance: None,
    }];
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 77, "provider manifest".into())
            .expect("provider index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    app.extension
        .fixture_provider_install(&index)
        .expect("provider owner");
    let request = app
        .extension
        .fixture_provider_start(
            crate::plugins::PluginProviderStart {
                plugin: "demo".into(),
                load: PluginLoadId(77),
                provider: "plugin.demo.provider.files".into(),
                query: "be".into(),
                context: "editor".into(),
                workspace_revision: Some(app.workspace.root_revision()),
                buffer_revision: None,
            },
            &app.state,
        )
        .expect("request");
    app.extension
        .provider_chunk(
            "demo",
            PluginLoadId(77),
            (Some(app.workspace.root_revision()), None),
            crate::plugins::PluginPickerChunk {
                request,
                sequence: 1,
                items: vec![
                    crate::plugins::PluginPickerItem {
                        id: "z".into(),
                        primary: "Zebra".into(),
                        secondary: None,
                        action_data: vec![1],
                    },
                    crate::plugins::PluginPickerItem {
                        id: "b".into(),
                        primary: "Beta".into(),
                        secondary: Some("ranked first".into()),
                        action_data: vec![2],
                    },
                ],
                terminal: crate::plugins::PluginProviderTerminal::More,
            },
        )
        .expect("chunk");
    app.extension.picker_installed(Some(request));
    app.present_plugin_picker(request);
    assert_eq!(app.search.picker_surface(), PickerSurfaceKind::Generic);
    assert_eq!(
        app.search
            .picker()
            .and_then(crate::workspace::PickerState::selected_item)
            .map(|item| item.label.as_str()),
        Some("Beta")
    );
    app.picker_insert("e");
    app.input.set_picker_normal(true);
    let selected = app
        .search
        .picker()
        .unwrap()
        .selected_item()
        .unwrap()
        .id
        .clone();
    app.extension
        .provider_chunk(
            "demo",
            PluginLoadId(77),
            (Some(app.workspace.root_revision()), None),
            crate::plugins::PluginPickerChunk {
                request,
                sequence: 2,
                items: vec![crate::plugins::PluginPickerItem {
                    id: "better".into(),
                    primary: "Better".into(),
                    secondary: None,
                    action_data: Vec::new(),
                }],
                terminal: crate::plugins::PluginProviderTerminal::Complete,
            },
        )
        .unwrap();
    app.present_plugin_picker(request);
    assert_eq!(app.ui.picker.as_ref().unwrap().query, "e");
    assert_eq!(
        app.search.picker().unwrap().selected_item().unwrap().id,
        selected
    );
    assert!(app.input.picker_normal());
    let stable = app.extension.provider_projection(request);
    let stable_ui = app.ui.picker.clone();
    for _ in 0..256 {
        assert_eq!(app.extension.provider_projection(request), stable);
        assert_eq!(app.ui.picker, stable_ui);
    }
    app.install_picker("Another picker".into(), Vec::new());
    let replacement_ui = app.ui.picker.clone();
    assert!(!app.extension.provider_visible(request));
    app.extension.picker_installed(Some(request)); // cancelled sources cannot reactivate
    app.present_plugin_picker(request);
    assert_eq!(app.ui.picker, replacement_ui);
    assert!(!app.extension.provider_visible(request));
    app.close_picker();
    assert_eq!(
        app.extension
            .provider_chunk(
                "demo",
                PluginLoadId(77),
                (Some(app.workspace.root_revision()), None),
                crate::plugins::PluginPickerChunk {
                    request,
                    sequence: 2,
                    items: Vec::new(),
                    terminal: crate::plugins::PluginProviderTerminal::Complete,
                },
            )
            .unwrap_err()
            .code,
        crate::plugins::PluginProviderErrorCode::UnknownRequest
    );
    // Reopening a one-request provider reserves new metadata without destroying
    // the displayed source if subsequent picker acceptance is refused.
    let mut broker = crate::plugins::PluginProviderBroker::default();
    broker.install(&index).unwrap();
    let start = || crate::plugins::PluginProviderStart {
        plugin: "demo".into(),
        load: PluginLoadId(77),
        provider: "plugin.demo.provider.files".into(),
        query: String::new(),
        context: "editor".into(),
        workspace_revision: None,
        buffer_revision: None,
    };
    let old = broker.start(start()).unwrap();
    let next = broker.reserve_replacing(start(), Some(old)).unwrap();
    assert!(broker.presentable(old) && broker.presentable(next));
    assert_eq!(
        broker
            .reserve_replacing(start(), Some(old))
            .unwrap_err()
            .code,
        crate::plugins::PluginProviderErrorCode::QueueFull
    );
    broker.cancel(next);
    assert!(broker.presentable(old));
    let next = broker.reserve_replacing(start(), Some(old)).unwrap();
    broker.cancel(old);
    assert!(broker.presentable(next));
}

pub(in crate::app) fn provider_selection_fixture() -> (
    (tempfile::TempDir, tempfile::TempDir),
    App,
    crate::plugins::ExecutionFixture,
    u64,
) {
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("provider.txt");
    app.state
        .open_text(TextBuffer::from_text("original"), Some(path))
        .unwrap();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::plugins::PluginCapability::PickerProviders);
    manifest.contributions.providers = vec![crate::plugins::PluginProviderContribution {
        id: "plugin.demo.provider.files".into(),
        kind: crate::plugins::PluginProviderKind::PickerSource,
        command: "plugin.demo.run".into(),
        maximum_concurrent_requests: 1,
        maximum_items: 16,
        maximum_bytes: 8192,
        deadline_milliseconds: 30_000,
        provenance: None,
    }];
    let index = crate::plugins::index_manifest_contributions(&manifest, 1, "picker fixture".into())
        .unwrap();
    app.extension.fixture_contribution(index.clone()).unwrap();
    app.extension.fixture_provider_install(&index).unwrap();
    let request = app
        .extension
        .fixture_provider_start(
            crate::plugins::PluginProviderStart {
                plugin: "demo".into(),
                load: PluginLoadId(1),
                provider: "plugin.demo.provider.files".into(),
                query: String::new(),
                context: String::new(),
                workspace_revision: None,
                buffer_revision: None,
            },
            &app.state,
        )
        .unwrap();
    app.extension
        .provider_chunk(
            "demo",
            PluginLoadId(1),
            (None, None),
            crate::plugins::PluginPickerChunk {
                request,
                sequence: 1,
                items: vec![crate::plugins::PluginPickerItem {
                    id: "selected".into(),
                    primary: "Selected item".into(),
                    secondary: None,
                    action_data: vec![0, 255],
                }],
                terminal: crate::plugins::PluginProviderTerminal::Complete,
            },
        )
        .unwrap();
    app.extension.picker_installed(Some(request));
    app.present_plugin_picker(request);
    (_app_dirs, app, fixture, request)
}

#[test]
fn provider_selection_delivers_request_item_and_action_data_slot5() {
    let (_app_dirs, mut app, fixture, request) = provider_selection_fixture();
    app.accept_picker();
    assert_eq!(
        fixture.take().arguments(),
        &[request.to_string(), "selected".into(), "[0,255]".into()]
    );
    assert!(app.extension.provider_projection(request).is_none());
}

#[test]
fn provider_selection_survives_a_discarded_replacement() {
    let (_app_dirs, mut app, fixture, request) = provider_selection_fixture();
    let replacement = app
        .extension
        .start_provider(
            "plugin.demo.provider.files",
            String::new(),
            app.workspace.root_revision(),
            &app.state,
        )
        .unwrap();
    assert_ne!(replacement.request, request);
    app.extension
        .discard_unpresented_provider(replacement.request);
    assert!(
        app.extension
            .provider_projection(replacement.request)
            .is_none()
    );
    assert!(app.extension.provider_visible(request));
    app.accept_picker();
    assert_eq!(
        fixture.take().arguments(),
        &[request.to_string(), "selected".into(), "[0,255]".into()]
    );
}

#[test]
fn provider_selection_rejects_reloaded_text_at_the_same_numeric_revision() {
    let (_app_dirs, mut app, fixture, _) = provider_selection_fixture();
    let buffer = app.state.active_buffer_id().unwrap();
    let path = app.state.buffers[buffer].path.clone().unwrap();
    assert_eq!(app.state.buffers[buffer].text.revision().0, 0);
    assert!(
        app.state
            .replace_buffer_from_disk(buffer, TextBuffer::from_text("reloaded"), path)
    );
    assert_eq!(app.state.buffers[buffer].text.revision().0, 0);
    app.accept_picker();
    assert!(
        fixture.is_empty(),
        "stale action data must not reach the plugin"
    );
}

#[test]
fn provider_selection_rejects_a_different_buffer_with_the_same_revision() {
    let (_app_dirs, mut app, fixture, _) = provider_selection_fixture();
    app.state
        .open_text(TextBuffer::from_text("other"), None)
        .unwrap();
    app.accept_picker();
    assert!(
        fixture.is_empty(),
        "a different buffer cannot use the old provider items"
    );
}

pub(in crate::app) fn virtual_document_fixture(
    capacity: usize,
) -> (
    (tempfile::TempDir, tempfile::TempDir),
    App,
    crate::plugins::ExecutionFixture,
    crate::plugins::PluginContributionIndex,
) {
    let (_app_dirs, mut app) = app();
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(capacity);
    app.extension.fixture_manager(manager);
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::plugins::PluginCapability::VirtualDocuments);
    manifest.contributions.virtual_documents =
        vec![crate::plugins::PluginVirtualDocumentContribution {
            id: "plugin.demo.virtual.help".into(),
            title: "Help".into(),
            language: "markdown".into(),
            mode: crate::plugins::PluginVirtualDocumentMode::ReadOnly,
            maximum_concurrent_documents: 2,
            maximum_document_bytes: 4096,
            maximum_document_lines: 128,
            maximum_patch_bytes: 256,
            maximum_patches_per_request: 8,
            deadline_milliseconds: 30_000,
            command: "plugin.demo.run".into(),
            context: "dashboard".into(),
            provenance: None,
        }];
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 1, "virtual fixture".into())
            .unwrap();
    app.extension.install_index(index.clone()).unwrap();
    (_app_dirs, app, fixture, index)
}

#[test]
fn repeated_virtual_open_does_not_dispatch_an_empty_callback_slot5() {
    let (_app_dirs, mut app, fixture, _) = virtual_document_fixture(4);
    app.process_effect(AppEffect::VirtualDocumentOpen {
        source: "plugin.demo.virtual.help".into(),
        key: "help".into(),
    });
    assert_eq!(fixture.take().virtual_documents().len(), 1);
    for _ in 0..2 {
        assert!(matches!(
            app.extension.open_virtual("plugin.demo.virtual.help", "help".into(), &app.state),
            Err(error) if error == "virtual document is already opening"
        ));
    }
}

pub(in crate::app) fn publish_virtual_fixture(
    app: &mut App,
    request: &crate::plugins::PluginVirtualDocumentRequest,
) {
    let (publication, candidate) = app
        .extension
        .prepare_virtual(
            "demo",
            PluginLoadId(1),
            crate::plugins::PluginVirtualDocumentProposal {
                request: request.request,
                document: request.document,
                cancellation_id: request.cancellation_id,
                kind: crate::plugins::PluginVirtualDocumentProposalKind::Full(
                    crate::plugins::PluginVirtualDocumentFull {
                        expected_revision: None,
                        revision: 1,
                        title: "Help".into(),
                        language: "markdown".into(),
                        text: "# Help".into(),
                    },
                ),
            },
            true,
            crate::app::plugins::VirtualContext {
                editor: &app.state,
                windows: &app.window_session,
                scratch: "scratch",
            },
        )
        .unwrap();
    let accepted = app
        .window_session
        .accept_editor_transition(candidate, publication.before(), publication.catalog())
        .unwrap();
    *app.state = publication.finish(accepted).unwrap();
    app.reconcile_editor_windows();
}

#[test]
fn virtual_open_can_retry_immediately_after_execution_reservation_fails() {
    let (_app_dirs, mut app, fixture, _) = virtual_document_fixture(1);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let open = || AppEffect::VirtualDocumentOpen {
        source: "plugin.demo.virtual.help".into(),
        key: "help".into(),
    };
    app.process_effect(open());
    fixture.take();
    app.process_effect(open());
    let execution = fixture.take();
    assert_eq!(execution.virtual_documents().len(), 1);
    assert_eq!(
        execution.virtual_documents()[0].kind,
        crate::plugins::PluginVirtualDocumentRequestKind::Open
    );
}

#[test]
fn virtual_refresh_can_retry_immediately_after_execution_reservation_fails() {
    let (_app_dirs, mut app, fixture, _) = virtual_document_fixture(1);
    app.process_effect(AppEffect::VirtualDocumentOpen {
        source: "plugin.demo.virtual.help".into(),
        key: "help".into(),
    });
    let execution = fixture.take();
    publish_virtual_fixture(&mut app, &execution.virtual_documents()[0]);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    app.process_effect(AppEffect::VirtualDocumentRefresh);
    fixture.take();
    app.process_effect(AppEffect::VirtualDocumentRefresh);
    let execution = fixture.take();
    assert_eq!(execution.virtual_documents().len(), 1);
    assert_eq!(
        execution.virtual_documents()[0].kind,
        crate::plugins::PluginVirtualDocumentRequestKind::Refresh
    );
}

#[test]
fn closing_a_virtual_buffer_delivers_close_notice_and_accepts_its_acknowledgement() {
    let (_app_dirs, mut app, fixture, _) = virtual_document_fixture(4);
    app.process_effect(AppEffect::VirtualDocumentOpen {
        source: "plugin.demo.virtual.help".into(),
        key: "help".into(),
    });
    let execution = fixture.take();
    publish_virtual_fixture(&mut app, &execution.virtual_documents()[0]);
    let buffer = app.state.active_buffer_id().unwrap();
    // Closing also supersedes an outstanding refresh.
    app.extension.refresh_virtual(buffer, &app.state).unwrap();
    app.close_active_buffer(true);
    assert!(!app.state.buffers.contains_key(buffer));
    let execution = fixture.take();
    assert_eq!(execution.virtual_documents().len(), 1);
    let close = &execution.virtual_documents()[0];
    assert_eq!(
        close.kind,
        crate::plugins::PluginVirtualDocumentRequestKind::CloseNotice
    );
    assert_eq!(close.current_revision, Some(1));
    assert!(app.window_session.dashboard_visible());
    fixture.reply_virtual(
        &execution,
        crate::plugins::PluginVirtualDocumentProposal {
            request: close.request,
            document: close.document,
            cancellation_id: close.cancellation_id,
            kind: crate::plugins::PluginVirtualDocumentProposalKind::CloseAcknowledged,
        },
    );
    app.process_plugin_events();
    assert!(!app.state.buffers.contains_key(buffer));
    assert!(app.window_session.dashboard_visible());
    assert!(app.ui.dashboard);
    app.process_effect(AppEffect::VirtualDocumentOpen {
        source: "plugin.demo.virtual.help".into(),
        key: "help".into(),
    });
    let reopened = fixture.take();
    assert_ne!(reopened.virtual_documents()[0].document, close.document);
}

#[test]
fn virtual_error_returns_the_broker_error_and_releases_the_open_request() {
    let (_app_dirs, mut app, fixture, _) = virtual_document_fixture(4);
    app.process_effect(AppEffect::VirtualDocumentOpen {
        source: "plugin.demo.virtual.help".into(),
        key: "help".into(),
    });
    let execution = fixture.take();
    let request = &execution.virtual_documents()[0];
    let result = app.extension.prepare_virtual(
        "demo",
        PluginLoadId(1),
        crate::plugins::PluginVirtualDocumentProposal {
            request: request.request,
            document: request.document,
            cancellation_id: request.cancellation_id,
            kind: crate::plugins::PluginVirtualDocumentProposalKind::Error(
                crate::plugins::PluginVirtualDocumentError::new(
                    crate::plugins::PluginVirtualDocumentErrorCode::Internal,
                    "provider failed",
                ),
            ),
        },
        true,
        crate::app::plugins::VirtualContext {
            editor: &app.state,
            windows: &app.window_session,
            scratch: "scratch",
        },
    );
    assert!(matches!(result, Err(error) if error == "provider failed"));
    app.process_effect(AppEffect::VirtualDocumentOpen {
        source: "plugin.demo.virtual.help".into(),
        key: "help".into(),
    });
    assert_eq!(fixture.take().virtual_documents().len(), 1);
}

#[test]
fn palette_finds_plugin_titles_without_configured_labels_and_honors_overrides() {
    let (_app_dirs, mut app) = app();
    let mut manifest =
        crate::app::tests::plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x");
    manifest.commands.get_mut("run").unwrap().title = "Print Hello".into();
    let index =
        crate::plugins::index_manifest_contributions(&manifest, 1, "fixture".into()).unwrap();
    app.register_indexed_plugin_commands(&index).unwrap();
    assert!(
        !app.configuration
            .settings()
            .command_labels
            .contains_key("plugin.demo.run")
    );
    assert_eq!(
        palette_row_for(&mut app, "Print Hello"),
        "Plugins: Print Hello"
    );

    app.close_picker();
    let mut config = app.configuration.fixture_snapshot();
    config
        .settings
        .command_labels
        .insert("plugin.demo.run".into(), "Say Goodbye".into());
    app.configuration.install_fixture(config);
    assert_eq!(
        palette_row_for(&mut app, "Say Goodbye"),
        "Plugins: Say Goodbye"
    );
}

#[test]
fn health_virtual_dogfood_opens_refreshes_and_remains_read_only() {
    let (_app_dirs, mut app) = app();
    app.process_effect(AppEffect::HealthVirtualOpen);
    let buffer = app.state.active_buffer_id().expect("health buffer");
    assert!(
        app.state.buffers[buffer]
            .text
            .to_string()
            .contains("# Dust health")
    );
    assert!(app.state.buffers[buffer].is_virtual_read_only());
    app.reconcile_editor_windows();
    assert!(
        app.ui
            .content_tabs
            .iter()
            .any(|entry| entry.icon == VIRTUAL_ICON
                && entry.closeable
                && entry.kind == ContentTabKind::VirtualDocument)
    );
    let virtual_content = app
        .window_session
        .graph()
        .content_for_window(
            app.window_session
                .graph()
                .focused()
                .expect("virtual window"),
        )
        .expect("virtual content");
    assert_eq!(
        virtual_content.adapter,
        crate::windows::ContentAdapterKind::VirtualDocument
    );
    assert!(virtual_content.capabilities.persist_descriptor);
    let persisted = app.capture_session().expect("virtual descriptor session");
    assert!(persisted.tabs.is_empty());
    assert_eq!(persisted.content_descriptors.len(), 1);
    let descriptor = &persisted.content_descriptors[0];
    assert_eq!(descriptor.semantic_id, "virtual:0");
    assert_eq!(descriptor.content_type, "builtin.virtual-document");
    assert_eq!(descriptor.adapter, "virtual-document");
    assert_eq!(descriptor.provider.as_deref(), Some("builtin.health"));
    assert_eq!(descriptor.source.as_deref(), Some("builtin.health.virtual"));
    assert_eq!(descriptor.key.as_deref(), Some("health"));
    assert_eq!(
        persisted.unavailable_virtual_descriptors(),
        std::collections::BTreeSet::from(["builtin.health:builtin.health.virtual:health".into()])
    );
    let encoded = serde_json::to_value(descriptor).expect("virtual descriptor JSON");
    for forbidden in ["load_id", "load", "revision", "callback", "cells", "text"] {
        assert!(encoded.get(forbidden).is_none(), "serialized {forbidden}");
    }
    app.report_unavailable_persisted_content(&persisted);
    assert!(app.ui.config_diagnostic.as_deref().is_some_and(|message| {
        message.contains("production API 0.4")
            && message.contains("builtin.health:builtin.health.virtual:health")
            && message.contains("continuing without")
    }));
    let mut unavailable_restore = persisted.clone();
    unavailable_restore
        .window_rules
        .push(crate::persistence::PersistedWindowRule {
            content_type: "builtin.terminal".into(),
            placement: crate::persistence::PersistedWindowPlacement::Floating {
                x: 7,
                y: 5,
                width: 40,
                height: 9,
            },
        });
    app.persistence
        .fixture_restore_source(crate::app::persistence::RestoreContext {
            editor: &app.state,
            workspace: &app.workspace,
            input: &app.input,
            windows: &app.window_session,
            configuration: app.configuration.binding_revision(),
        });
    app.restore_session(unavailable_restore);
    assert!(app.terminal_session.descriptions().is_empty());
    assert!(app.window_session.native_ids().terminal(1).is_none());
    assert!(
        app.window_session
            .native_ids()
            .semantic_defaults(None)
            .contains(&(
                "builtin.terminal",
                WindowPlacement::Floating(WindowRect {
                    x: 7,
                    y: 5,
                    width: 40,
                    height: 9,
                })
            ))
    );
    let ids = (
        buffer,
        app.state.active_view(),
        app.window_session.graph().focused(),
    );
    app.process_effect(AppEffect::HealthVirtualRefresh);
    assert_eq!(
        (
            app.state.active_buffer_id().unwrap(),
            app.state.active_view(),
            app.window_session.graph().focused()
        ),
        ids
    );
    let revision = app.state.buffers[buffer].text.revision();
    let transaction = crate::editor::EditorTransaction::new(crate::editor::TransactionId(999))
        .with_operation(crate::editor::EditorOperation::ApplyEdits {
            buffer,
            edits: crate::editor::text::TextEditSet::new(
                revision,
                vec![crate::editor::text::TextEdit::new(
                    crate::editor::text::ByteRange::new(
                        crate::editor::text::ByteOffset(0),
                        crate::editor::text::ByteOffset(0),
                    )
                    .unwrap(),
                    "x",
                )],
            ),
        });
    assert!(matches!(
        app.state.apply_transaction(&transaction),
        Err(crate::editor::TransactionError::ReadOnly)
    ));
    for _ in 0..256 {
        app.reconcile_editor_windows();
        assert_eq!(app.window_session.graph().focused(), ids.2);
    }
    app.remove_plugin_contributions("builtin.health", PluginLoadId(1));
    assert!(!app.state.buffers.contains_key(buffer));
    assert!(app.state.active_view().is_none());
    assert!(app.window_session.dashboard_visible());
    app.window_session.graph().validate().unwrap();
}

#[test]
fn accepted_virtual_snapshot_publishes_through_the_tiled_editor_bridge() {
    let (_app_dirs, mut app) = app();
    let snapshot = crate::plugins::PluginVirtualDocumentSnapshot {
        plugin: "demo".into(),
        load: PluginLoadId(7),
        source: "plugin.demo.virtual.help".into(),
        document: 9,
        key: "help".into(),
        title: "Help".into(),
        language: "markdown".into(),
        text: "# Héllo\nvirtual".into(),
        revision: 1,
        closed: false,
    };
    let mut draft = app.state.draft();
    let buffer = app
        .extension
        .fixture_virtual_publish(&mut draft, &snapshot)
        .expect("publish");
    *app.state = draft.accept(&app.state.version()).unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    assert!(app.state.content_for_buffer(buffer).is_some());
    assert!(app.window_session.graph().focused().is_some());
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "# Héllo\nvirtual"
    );
    let ids = (
        app.state.active_view(),
        app.window_session.graph().focused(),
    );
    let mut refresh = snapshot.clone();
    refresh.revision = 2;
    refresh.text = "# Héllo\nrefreshed".into();
    let mut draft = app.state.draft();
    assert_eq!(
        app.extension
            .fixture_virtual_publish(&mut draft, &refresh)
            .unwrap(),
        buffer
    );
    *app.state = draft.accept(&app.state.version()).unwrap();
    app.reconcile_editor_windows();
    assert_eq!(app.state.buffers[buffer].text.to_string(), refresh.text);
    assert_eq!(
        app.state.buffers[buffer].origin,
        crate::editor::BufferOrigin::VirtualReadOnly {
            owner: refresh.plugin.clone(),
            load_id: refresh.load.0,
            document: refresh.document,
            revision: refresh.revision,
        }
    );
    assert_eq!(
        (
            app.state.active_view(),
            app.window_session.graph().focused()
        ),
        ids
    );
    assert!(matches!(
        app.window_session
            .graph()
            .window(app.window_session.graph().focused().unwrap())
            .unwrap()
            .placement,
        WindowPlacement::Tiled
    ));
}

#[test]
fn universal_window_clone_uses_editor_owned_independent_text_and_virtual_views() {
    let (_app_dirs, mut app) = app();
    let (buffer, original_view) = app
        .state
        .open_text(TextBuffer::from_text("αbc\nline"), None)
        .unwrap();
    {
        let view = &mut app.state.views[original_view];
        view.cursor = crate::editor::text::ByteOffset(2);
        view.selection = Some(crate::editor::Selection {
            anchor: crate::editor::text::ByteOffset(0),
            head: crate::editor::text::ByteOffset(2),
            kind: crate::editor::SelectionKind::Character,
        });
        view.scroll_line = 1;
        view.scroll_column = 3;
        view.mode = crate::editor::EditorMode::Insert;
    }
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let original_window = app.window_session.graph().focused().expect("text window");
    let original_content = app
        .window_session
        .graph()
        .content_for_window(original_window)
        .expect("text content")
        .id;
    let before_counts = app.window_session.graph().counts();

    let invocation = crate::editor::CommandInvocation::new(
        "window.clone",
        crate::editor::InvocationSource::Test,
    )
    .expect("clone command")
    .with_argument(
        "target",
        crate::editor::CommandValue::String(format!("window:{}", original_window.0)),
    )
    .with_argument(
        "axis",
        crate::editor::CommandValue::String("vertical".into()),
    );
    let outcome = app
        .commands
        .execute(&app.state, &invocation)
        .expect("typed clone command");
    let [effect] = outcome.effects.as_slice() else {
        panic!("clone command must emit one effect");
    };
    assert!(matches!(
        effect,
        AppEffect::WindowClone {
            target,
            axis: crate::windows::TileAxis::Vertical,
        } if target == &format!("window:{}", original_window.0)
    ));
    app.process_effect(outcome.effects.into_iter().next().expect("clone effect"));

    let cloned_view = app.state.active_view().expect("cloned editor view");
    let cloned_window = app.window_session.graph().focused().expect("cloned window");
    assert_ne!(cloned_view, original_view);
    assert_ne!(cloned_window, original_window);
    assert_eq!(app.state.views[cloned_view].buffer, buffer);
    assert_eq!(
        app.window_session
            .graph()
            .content_for_window(cloned_window)
            .expect("cloned text content")
            .id,
        original_content
    );
    assert_eq!(
        app.window_session.graph().counts(),
        (
            before_counts.0,
            before_counts.1 + 1,
            before_counts.2 + 1,
            before_counts.3,
        )
    );
    assert_eq!(
        app.state.views[cloned_view].cursor,
        app.state.views[original_view].cursor
    );
    assert_eq!(
        app.state.views[cloned_view].selection,
        app.state.views[original_view].selection
    );
    assert_eq!(app.state.views[cloned_view].scroll_line, 1);
    assert_eq!(app.state.views[cloned_view].scroll_column, 3);
    assert_eq!(
        app.state.views[cloned_view].mode,
        crate::editor::EditorMode::Insert
    );

    let transaction = crate::editor::EditorTransaction::new(crate::editor::TransactionId(999))
        .with_operation(crate::editor::EditorOperation::SetCursor {
            view: cloned_view,
            position: crate::editor::text::ByteOffset(3),
        })
        .with_operation(crate::editor::EditorOperation::SetScroll {
            view: cloned_view,
            first_line: 0,
        })
        .with_operation(crate::editor::EditorOperation::SetScrollColumn {
            view: cloned_view,
            first_column: 0,
        })
        .with_operation(crate::editor::EditorOperation::SetSelection {
            view: cloned_view,
            selection: None,
        })
        .with_operation(crate::editor::EditorOperation::SetMode {
            view: cloned_view,
            mode: crate::editor::EditorMode::Normal,
        });
    app.apply_authoritative_editor_transaction(&transaction)
        .expect("update only cloned view");
    assert_eq!(
        app.state.views[original_view].cursor,
        crate::editor::text::ByteOffset(2)
    );
    assert_eq!(app.state.views[original_view].scroll_line, 1);
    assert_eq!(app.state.views[original_view].scroll_column, 3);
    assert!(app.state.views[original_view].selection.is_some());
    assert_eq!(
        app.state.views[original_view].mode,
        crate::editor::EditorMode::Insert
    );

    app.process_effect(AppEffect::WindowClone {
        target: format!("window:{}", original_window.0),
        axis: crate::windows::TileAxis::Horizontal,
    });
    let exact_clone = app.state.active_view().expect("exact source clone");
    assert_ne!(exact_clone, cloned_view);
    assert_eq!(app.state.views[exact_clone].buffer, buffer);
    assert_eq!(
        app.state.views[exact_clone].cursor,
        app.state.views[original_view].cursor
    );
    assert_eq!(
        app.state.views[exact_clone].selection,
        app.state.views[original_view].selection
    );
    assert_eq!(
        app.state.views[exact_clone].scroll_line,
        app.state.views[original_view].scroll_line
    );
    assert_eq!(
        app.state.views[exact_clone].scroll_column,
        app.state.views[original_view].scroll_column
    );
    assert_eq!(
        app.state.views[exact_clone].mode,
        app.state.views[original_view].mode
    );
    assert_ne!(
        app.state.views[exact_clone].cursor,
        app.state.views[cloned_view].cursor
    );
    assert_ne!(
        app.state.views[exact_clone].selection,
        app.state.views[cloned_view].selection
    );

    let (_virtual_app_dirs, mut virtual_app) = crate::app::tests::app();
    virtual_app.process_effect(AppEffect::HealthVirtualOpen);
    let virtual_buffer = virtual_app.state.active_buffer_id().expect("health buffer");
    let virtual_view = virtual_app.state.active_view().expect("health view");
    virtual_app.reconcile_editor_windows();
    let virtual_window = virtual_app
        .window_session
        .graph()
        .focused()
        .expect("health window");
    let virtual_content = virtual_app
        .window_session
        .graph()
        .content_for_window(virtual_window)
        .expect("health content")
        .id;
    virtual_app.process_effect(AppEffect::WindowClone {
        target: format!("window:{}", virtual_window.0),
        axis: crate::windows::TileAxis::Horizontal,
    });
    let cloned_virtual_view = virtual_app.state.active_view().expect("cloned health view");
    let cloned_virtual_window = virtual_app
        .window_session
        .graph()
        .focused()
        .expect("cloned health window");
    assert_ne!(cloned_virtual_view, virtual_view);
    assert_eq!(
        virtual_app.state.views[cloned_virtual_view].buffer,
        virtual_buffer
    );
    assert!(virtual_app.state.buffers[virtual_buffer].is_virtual_read_only());
    assert_eq!(
        virtual_app
            .window_session
            .graph()
            .content_for_window(cloned_virtual_window)
            .expect("cloned health content")
            .id,
        virtual_content
    );
}

#[test]
fn virtual_read_only_origin_never_queues_save_reload_or_recovery_io() {
    let (_app_dirs, mut app) = app();
    let (buffer, _) = app
        .state
        .open_text(crate::editor::text::TextBuffer::from_text("virtual"), None)
        .unwrap();
    app.state.buffers[buffer].read_only = true;
    app.state.buffers[buffer].origin = crate::editor::BufferOrigin::VirtualReadOnly {
        owner: "demo".into(),
        load_id: 7,
        document: 9,
        revision: 1,
    };
    let path = app.state.workspace_root.join("forbidden");
    fs::write(&path, b"unchanged").unwrap();
    app.state.buffers[buffer].path = Some(path.clone());
    let before = app.documents.pending_count();
    app.start_save(false, Some(path.clone()), false);
    assert_eq!(app.documents.pending_count(), before);
    app.start_reload(true);
    assert_eq!(app.documents.pending_count(), before);
    wait_vfs(&mut app);
    assert_eq!(fs::read(&path).unwrap(), b"unchanged");
    assert_eq!(app.state.buffers[buffer].text.to_string(), "virtual");
    app.state.buffers[buffer].dirty = true;
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.recovery.enabled = true;
        app.configuration.install_fixture(fixture);
    }
    app.recovery.skip_maintenance();
    app.schedule_recovery_work();
    assert!(!app.recovery.scheduled().contains_key(&buffer));
    app.queue_dirty_recovery_before_exit();
    assert!(!app.recovery.writing(None));
    assert!(app.capture_session().is_none());
}

#[test]
fn content_line_uses_filename_icons_for_extensionless_files() {
    let (_app_dirs, mut app) = app();
    for name in ["Dockerfile", "Makefile", "README", ".gitignore"] {
        app.state
            .open_text(
                TextBuffer::from_text("file"),
                Some(PathBuf::from("/workspace").join(name)),
            )
            .unwrap();
    }
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    for name in ["Dockerfile", "Makefile", "README", ".gitignore"] {
        let entry = app
            .ui
            .content_tabs
            .iter()
            .find(|entry| entry.title == name)
            .unwrap();
        assert_eq!(entry.icon, crate::files::file_icon(name).unwrap().glyph);
        assert_ne!(entry.icon, SCRATCH_ICON);
    }
}

#[test]
fn text_and_virtual_windows_project_input_and_preserve_identity_across_ordinary_placements() {
    let (_app_dirs, mut app) = app();
    app.window_session.observe_size((80, 24));
    app.state
        .open_text(
            TextBuffer::from_text("floating editor contents"),
            Some(PathBuf::from("/workspace/floating.rs")),
        )
        .unwrap();
    app.set_dashboard_visibility(false);
    let tab = app.state.active_tab().expect("editor tab");
    let source_pane = app.state.tab_focused_window(tab).unwrap();
    let source_view = app.state.window_view(source_pane).unwrap();
    app.process_effect(AppEffect::Layout(crate::app::windows::LayoutAction::Split(
        crate::windows::TileAxis::Horizontal,
    )));
    let source_window = app
        .state
        .graph
        .window(source_pane)
        .map(|window| window.id)
        .expect("source window");
    let source_content = app
        .window_session
        .graph()
        .content_for_window(source_window)
        .expect("source content")
        .id;

    app.process_effect(AppEffect::WindowFloat {
        target: format!("window:{}", source_window.0),
        x: 7,
        y: 4,
        width: 31,
        height: 9,
    });
    let floating = app
        .compositor_projection()
        .and_then(|projection| projection.window(source_window))
        .expect("floating editor projection");
    assert_eq!(floating.layer, crate::windows::CompositorLayer::Floating);
    assert_eq!(
        floating.rect,
        WindowRect {
            x: 7,
            y: 4,
            width: 31,
            height: 9,
        }
    );
    let frame = crate::ui::render(
        &app.state,
        &app.ui,
        app.configuration.settings(),
        app.configuration.dashboard(),
        80,
        24,
    );
    assert!(
        (4..13).any(|row| (7..38).any(|column| {
            frame
                .grid
                .get(column, row)
                .is_some_and(|cell| cell.symbol == "f")
        })),
        "floating editor content is painted in its projected rectangle"
    );
    app.input.observe_mouse(12, 5);
    app.focus_mouse_pane();
    assert_eq!(app.state.tab_focused_window(tab).unwrap(), source_pane);
    assert_eq!(app.state.active_view(), Some(source_view));

    app.process_effect(AppEffect::WindowMove {
        target: format!("window:{}", source_window.0),
        x: 11,
        y: 6,
    });
    app.process_effect(AppEffect::WindowResize {
        target: format!("window:{}", source_window.0),
        amount: 3,
    });
    let moved_source = WindowPlacement::Floating(WindowRect {
        x: 11,
        y: 6,
        width: 34,
        height: 12,
    });
    assert_eq!(
        app.window_session
            .graph()
            .window(source_window)
            .expect("moved editor")
            .placement,
        moved_source
    );
    let other_pane = app
        .state
        .editor_windows()
        .map(|window| window.id)
        .find(|pane| *pane != source_pane)
        .expect("other pane");
    let other_window = app
        .state
        .graph
        .window(other_pane)
        .map(|window| window.id)
        .expect("other editor window");
    let other_placement = WindowPlacement::Floating(WindowRect {
        x: 48,
        y: 2,
        width: 25,
        height: 7,
    });
    app.place_graph_window(&format!("window:{}", other_window.0), other_placement)
        .expect("float second editor");
    app.swap_graph_windows(
        &format!("window:{}", source_window.0),
        &format!("window:{}", other_window.0),
    )
    .expect("swap non-tiled editor placements");
    assert_eq!(
        app.window_session
            .graph()
            .window(source_window)
            .expect("swapped source")
            .placement,
        other_placement
    );
    assert_eq!(
        app.window_session
            .graph()
            .window(other_window)
            .expect("swapped other")
            .placement,
        moved_source
    );

    for placement in [
        WindowPlacement::Docked {
            edge: DockEdge::Right,
            size: 18,
        },
        WindowPlacement::Fullscreen,
        WindowPlacement::Hidden,
    ] {
        app.place_graph_window(&format!("window:{}", source_window.0), placement)
            .expect("supported editor placement");
        assert_eq!(
            app.window_session
                .graph()
                .window(source_window)
                .expect("same editor window")
                .placement,
            placement
        );
        assert_eq!(
            app.window_session
                .graph()
                .content_for_window(source_window)
                .expect("same content")
                .id,
            source_content
        );
        assert_eq!(
            app.state.graph.window(source_pane).map(|window| window.id),
            Some(source_window)
        );
        if placement == WindowPlacement::Hidden {
            assert_ne!(app.state.tab_focused_window(tab).unwrap(), source_pane);
            assert_eq!(
                app.window_session.graph().focused(),
                app.state.tab_focused_window(tab)
            );
        }
    }
    app.show_graph_window(&format!("window:{}", source_window.0))
        .expect("show hidden editor");
    assert_eq!(app.state.active_view(), Some(source_view));
    assert_eq!(app.state.tab_focused_window(tab).unwrap(), source_pane);
    assert_eq!(app.window_session.graph().focused(), Some(source_window));
    assert_eq!(
        app.window_session
            .graph()
            .window(source_window)
            .expect("restored editor")
            .placement,
        WindowPlacement::Fullscreen
    );
    app.place_graph_window(
        &format!("window:{}", source_window.0),
        WindowPlacement::Tiled,
    )
    .expect("return editor to exact logical tile");
    assert!(
        app.window_session
            .graph()
            .tiled_layout_snapshot(app.window_session.graph().active_tab().expect("active tab"))
            .expect("restored tiled layout")
            .project(
                WindowRect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 24,
                },
                1,
                1,
            )
            .windows
            .iter()
            .any(|window| window.window == source_window)
    );
    app.close_graph_window(&format!("window:{}", other_window.0))
        .expect("close exact non-tiled editor view");
    assert!(app.window_session.graph().window(other_window).is_none());
    assert_eq!(
        app.window_session
            .graph()
            .content_for_window(source_window)
            .expect("shared text content survives view close")
            .id,
        source_content
    );

    let (_virtual_app_dirs, mut virtual_app) = crate::app::tests::app();
    virtual_app.process_effect(AppEffect::HealthVirtualOpen);
    virtual_app.reconcile_editor_windows();
    let virtual_window = virtual_app
        .window_session
        .graph()
        .focused()
        .expect("virtual window");
    let virtual_content = virtual_app
        .window_session
        .graph()
        .content_for_window(virtual_window)
        .expect("virtual content")
        .clone();
    assert_eq!(
        virtual_content.adapter,
        crate::windows::ContentAdapterKind::VirtualDocument
    );
    virtual_app
        .place_graph_window(
            &format!("window:{}", virtual_window.0),
            WindowPlacement::Floating(WindowRect {
                x: 4,
                y: 3,
                width: 40,
                height: 10,
            }),
        )
        .expect("float virtual document");
    virtual_app
        .move_graph_window(&format!("window:{}", virtual_window.0), 6, 5)
        .expect("move virtual document");
    virtual_app
        .resize_graph_window(&format!("window:{}", virtual_window.0), 2)
        .expect("resize virtual document");
    for placement in [
        WindowPlacement::Docked {
            edge: DockEdge::Left,
            size: 16,
        },
        WindowPlacement::Fullscreen,
        WindowPlacement::Hidden,
    ] {
        virtual_app
            .place_graph_window(&format!("window:{}", virtual_window.0), placement)
            .expect("place virtual document");
        assert_eq!(
            virtual_app
                .window_session
                .graph()
                .window(virtual_window)
                .expect("same virtual window")
                .placement,
            placement
        );
    }
    virtual_app
        .show_graph_window(&format!("window:{}", virtual_window.0))
        .expect("show virtual document");
    virtual_app
        .place_graph_window(
            &format!("window:{}", virtual_window.0),
            WindowPlacement::Tiled,
        )
        .expect("tile virtual document");
    assert_eq!(
        virtual_app
            .window_session
            .graph()
            .content_for_window(virtual_window)
            .expect("same virtual content")
            .id,
        virtual_content.id
    );
    let virtual_pane = virtual_app
        .state
        .window_view(virtual_window)
        .map(|_| virtual_window)
        .expect("virtual pane");
    virtual_app
        .clone_graph_window(
            &format!("window:{}", virtual_window.0),
            crate::windows::TileAxis::Horizontal,
        )
        .expect("clone virtual view for close qualification");
    let virtual_clone = virtual_app
        .window_session
        .graph()
        .focused()
        .expect("virtual clone window");
    assert_ne!(virtual_clone, virtual_window);
    virtual_app
        .place_graph_window(
            &format!("window:{}", virtual_clone.0),
            WindowPlacement::Floating(WindowRect {
                x: 8,
                y: 4,
                width: 30,
                height: 8,
            }),
        )
        .expect("float virtual clone");
    virtual_app
        .close_graph_window(&format!("window:{}", virtual_clone.0))
        .expect("close exact virtual view");
    assert!(
        virtual_app
            .window_session
            .graph()
            .window(virtual_clone)
            .is_none()
    );
    assert_eq!(
        virtual_app
            .state
            .graph
            .window(virtual_pane)
            .map(|window| window.id),
        Some(virtual_window)
    );
    assert_eq!(virtual_app.state.buffers.len(), 1);
}

#[test]
fn startup_plugin_publication_waits_for_session_restoration() {
    use crate::persistence::PersistedLayout;
    use crate::persistence::PersistedPane;
    use crate::persistence::PersistedTab;
    let (_app_dirs, mut app) = app();
    let path = app.state.workspace_root.join("restored.txt");
    fs::write(&path, "restored text\n").unwrap();
    let session = PersistedSession {
        roots: vec![app.state.workspace_root.clone()],
        tabs: vec![PersistedTab {
            layout: PersistedLayout::Pane {
                pane: PersistedPane {
                    path: path.clone(),
                    cursor: 3,
                    anchor: 3,
                    scroll_line: 0,
                    scroll_column: 0,
                },
            },
            focused_path: Some(path.clone()),
            focused_pane: None,
        }],
        active_tab: 0,
        window_rules: Vec::new(),
        content_descriptors: Vec::new(),
        pinned: Vec::new(),
    };
    let saved = tempdir().unwrap();
    let state_path = saved.path().join("session.json");
    fs::write(
        &state_path,
        serde_json::to_vec(&PersistedState {
            session: Some(session),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    let (worker, release) = PersistenceWorker::gated_for_test(1);
    app.persistence = crate::app::persistence::Persistence::new(worker, state_path);
    let (manager, fixture) = crate::plugins::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    app.load_persistence();
    fixture.indexed(
        crate::plugins::index_manifest_contributions(
            &plugin_manifest("demo", "plugin.demo.test", "x y"),
            1,
            "startup".into(),
        )
        .unwrap(),
    );
    let revision = app.configuration.binding_revision();
    app.process_plugin_events();
    assert!(app.extension.contributions().get("demo").is_none());
    drop(release);
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.persistence.pending() {
        app.process_plugin_events();
        app.process_persistence_events();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(app.persistence.restoring());
    app.process_plugin_events();
    assert_eq!(app.configuration.binding_revision(), revision);
    wait_vfs(&mut app);
    assert!(!app.persistence.restoring());
    let buffer = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[buffer].path.as_ref(), Some(&path));
    assert_eq!(
        app.state.buffers[buffer].text.to_string(),
        "restored text\n"
    );
    assert_eq!(
        app.state.views[app.state.active_view().unwrap()].cursor,
        ByteOffset(3)
    );
    app.process_plugin_events();
    assert!(app.extension.contributions().get("demo").is_some());
    assert!(app.configuration.binding_revision() > revision);
}
