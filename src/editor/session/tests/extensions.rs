use super::*;

#[test]
fn plugin_confirmation_is_typed_one_shot_and_source_owned() {
    let mut app = app();
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);

    let request = |token| crate::extensions::PluginConfirmationRequest {
        token,
        title: "Apply plugin state".into(),
        message: "Change the custom retained view?".into(),
        default_answer: crate::extensions::PluginConfirmationAnswer::No,
    };
    let invoke = |app: &mut InteractiveApplication| {
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
        [crate::extensions::PluginConfirmationResult {
            request: 7,
            outcome: crate::extensions::PluginConfirmationOutcome::Answered(
                crate::extensions::PluginConfirmationAnswer::No,
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
        crate::extensions::PluginConfirmationOutcome::Answered(
            crate::extensions::PluginConfirmationAnswer::Yes
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
        crate::extensions::PluginConfirmationOutcome::Answered(
            crate::extensions::PluginConfirmationAnswer::Cancelled
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
    let retired = fixture.take();
    assert!(matches!(
        retired.confirmation_results()[0].outcome,
        crate::extensions::PluginConfirmationOutcome::Failed(
            crate::extensions::PluginConfirmationError {
                code: crate::extensions::PluginConfirmationErrorCode::SourceRetired,
                ..
            }
        )
    ));
    fixture.reply(&retired, Vec::new());
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
    let mut app = app();
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    fixture.reply_confirmation_request(
        &invocation,
        crate::extensions::PluginConfirmationRequest {
            token: 40,
            title: "Review all".into(),
            message: "target  界/".repeat(400),
            default_answer: crate::extensions::PluginConfirmationAnswer::No,
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
        let mut app = app();
        app.state.open_text(TextBuffer::from_text("x"), None);
        app.set_dashboard_visibility(false);
        app.handle_event(crate::ui::UiEvent::Resize(width, height));
        app.reconcile_editor_windows();
        let (manager, fixture) = crate::extensions::ExecutionFixture::new(4);
        app.extension.fixture_manager(manager);
        app.process_effect(AppEffect::PluginExecute {
            plugin: "demo".into(),
            command: "run".into(),
            arguments: Vec::new(),
        });
        let invocation = fixture.take();
        fixture.reply_confirmation_request(
            &invocation,
            crate::extensions::PluginConfirmationRequest {
                token,
                title: "Boundary".into(),
                message: "/tmp/a".into(),
                default_answer: crate::extensions::PluginConfirmationAnswer::No,
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
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("x"), None);
    app.set_dashboard_visibility(false);
    app.handle_event(crate::ui::UiEvent::Resize(100, 20));
    app.process_effect(AppEffect::ExplorerOpen);
    app.reconcile_editor_windows();
    assert!(
        app.compositor_projection().unwrap().workspace_bounds.width < 100,
        "explorer must consume horizontal workspace"
    );
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    fixture.reply_confirmation_request(
        &invocation,
        crate::extensions::PluginConfirmationRequest {
            token: 46,
            title: "Docked".into(),
            message: "/tmp/a".into(),
            default_answer: crate::extensions::PluginConfirmationAnswer::No,
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
        crate::ui::window::WindowRect {
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
    let mut app = app();
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(8);
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
        crate::extensions::PluginConfirmationRequest {
            token: 41,
            title: "Delayed".into(),
            message: "Must not steal focus".into(),
            default_answer: crate::extensions::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    assert_eq!(
        app.input.prompt().unwrap().kind,
        crate::ui::PromptKind::Command
    );
    assert_eq!(app.input.prompt().unwrap().input, "newer");
    app.process_plugin_events();
    let retired = fixture.take();
    assert!(matches!(
        retired.confirmation_results()[0].outcome,
        crate::extensions::PluginConfirmationOutcome::Failed(
            crate::extensions::PluginConfirmationError {
                code: crate::extensions::PluginConfirmationErrorCode::SourceRetired,
                ..
            }
        )
    ));
}

#[test]
fn delayed_initial_confirmation_reply_gets_terminal_failure_after_config_reload() {
    let mut app = app();
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(8);
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
        crate::extensions::PluginConfirmationRequest {
            token: 50,
            title: "Delayed".into(),
            message: "Original configuration was replaced".into(),
            default_answer: crate::extensions::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    assert!(app.input.prompt().is_none());
    app.process_plugin_events();
    let retired = fixture.take();
    assert_eq!(retired.confirmation_results()[0].request, 50);
    assert!(matches!(
        retired.confirmation_results()[0].outcome,
        crate::extensions::PluginConfirmationOutcome::Failed(
            crate::extensions::PluginConfirmationError {
                code: crate::extensions::PluginConfirmationErrorCode::SourceRetired,
                ..
            }
        )
    ));
}

#[test]
fn accepted_confirmation_cannot_wait_past_a_newer_prompt_capture() {
    let mut app = app();
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(8);
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
    let request = |token| crate::extensions::PluginConfirmationRequest {
        token,
        title: format!("Request {token}"),
        message: "Do not outlive a newer capture".into(),
        default_answer: crate::extensions::PluginConfirmationAnswer::No,
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
    let retired = fixture.take();
    assert_eq!(retired.confirmation_results()[0].request, 52);
    assert!(matches!(
        retired.confirmation_results()[0].outcome,
        crate::extensions::PluginConfirmationOutcome::Failed(
            crate::extensions::PluginConfirmationError {
                code: crate::extensions::PluginConfirmationErrorCode::SourceRetired,
                ..
            }
        )
    ));
}

#[test]
fn tool_menu_blocks_background_and_retires_delayed_confirmation() {
    for delayed in [false, true] {
        let mut app = app();
        let (manager, fixture) = crate::extensions::ExecutionFixture::new(8);
        app.extension.fixture_manager(manager);
        let invoke = |app: &mut InteractiveApplication| {
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
            crate::extensions::PluginConfirmationRequest {
                token: 70 + u64::from(delayed),
                title: "Background".into(),
                message: "Must not overlay the tool manager".into(),
                default_answer: crate::extensions::PluginConfirmationAnswer::No,
            },
        );
        app.process_plugin_events();
        assert!(app.input.prompt().is_none());
        assert!(app.ui.tool_menu.is_some());
        app.process_plugin_events();
        let retired = fixture.take();
        assert!(matches!(
            retired.confirmation_results()[0].outcome,
            crate::extensions::PluginConfirmationOutcome::Failed(
                crate::extensions::PluginConfirmationError {
                    code: crate::extensions::PluginConfirmationErrorCode::SourceRetired,
                    ..
                }
            )
        ));
    }
}

#[test]
fn pending_key_capture_blocks_background_and_retires_delayed_confirmation() {
    for delayed in [false, true] {
        let mut app = app();
        let (manager, fixture) = crate::extensions::ExecutionFixture::new(8);
        app.extension.fixture_manager(manager);
        let invoke = |app: &mut InteractiveApplication| {
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
            crate::extensions::PluginConfirmationRequest {
                token: 72 + u64::from(delayed),
                title: "Background".into(),
                message: "Must not consume a pending key gesture".into(),
                default_answer: crate::extensions::PluginConfirmationAnswer::No,
            },
        );
        app.process_plugin_events();
        assert!(app.input.prompt().is_none());
        assert!(app.input.mapping_pending());
        app.process_plugin_events();
        let retired = fixture.take();
        assert!(matches!(
            retired.confirmation_results()[0].outcome,
            crate::extensions::PluginConfirmationOutcome::Failed(
                crate::extensions::PluginConfirmationError {
                    code: crate::extensions::PluginConfirmationErrorCode::SourceRetired,
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
            let mut app = app();
            app.state.open_text(TextBuffer::from_text("abc"), None);
            app.set_dashboard_visibility(false);
            app.reconcile_editor_windows();
            let (manager, fixture) = crate::extensions::ExecutionFixture::new(8);
            app.extension.fixture_manager(manager);
            let invoke = |app: &mut InteractiveApplication| {
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
                crate::extensions::PluginConfirmationRequest {
                    token: 80,
                    title: "Background".into(),
                    message: "Must not consume unfinished modal grammar".into(),
                    default_answer: crate::extensions::PluginConfirmationAnswer::No,
                },
            );
            app.process_plugin_events();
            assert!(app.input.prompt().is_none());
            assert!(
                app.input.grammar_pending(),
                "confirmation rejection erased gesture {keys:?}"
            );
            app.process_plugin_events();
            let retired = fixture.take();
            assert!(matches!(
                retired.confirmation_results()[0].outcome,
                crate::extensions::PluginConfirmationOutcome::Failed(
                    crate::extensions::PluginConfirmationError {
                        code: crate::extensions::PluginConfirmationErrorCode::SourceRetired,
                        ..
                    }
                )
            ));
        }
    }
}

#[test]
fn background_confirmation_during_documentation_fails_without_overlay() {
    let mut app = app();
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
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(8);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    fixture.reply_confirmation_request(
        &invocation,
        crate::extensions::PluginConfirmationRequest {
            token: 53,
            title: "Background".into(),
            message: "Must not overlay documentation".into(),
            default_answer: crate::extensions::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    assert!(app.input.prompt().is_none());
    assert!(app.input.documentation().is_some());
    app.process_plugin_events();
    let retired = fixture.take();
    assert!(matches!(
        retired.confirmation_results()[0].outcome,
        crate::extensions::PluginConfirmationOutcome::Failed(
            crate::extensions::PluginConfirmationError {
                code: crate::extensions::PluginConfirmationErrorCode::SourceRetired,
                ..
            }
        )
    ));
}

#[test]
fn background_confirmation_during_picker_fails_without_overlay() {
    let mut app = app();
    app.install_picker("Newer picker".into(), Vec::new());
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(8);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    fixture.reply_confirmation_request(
        &invocation,
        crate::extensions::PluginConfirmationRequest {
            token: 54,
            title: "Background".into(),
            message: "Must not overlay picker".into(),
            default_answer: crate::extensions::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    assert!(app.input.prompt().is_none());
    assert_eq!(app.ui.picker.as_ref().unwrap().title, "Newer picker");
    app.process_plugin_events();
    let retired = fixture.take();
    assert!(matches!(
        retired.confirmation_results()[0].outcome,
        crate::extensions::PluginConfirmationOutcome::Failed(
            crate::extensions::PluginConfirmationError {
                code: crate::extensions::PluginConfirmationErrorCode::SourceRetired,
                ..
            }
        )
    ));
}

#[test]
fn confirmation_result_retains_document_a_without_materializing_later_b() {
    let mut app = app();
    let a_path = app.state.workspace_root.join("a.txt");
    let b_path = app.state.workspace_root.join("b.txt");
    app.state
        .open_text(TextBuffer::from_text("A"), Some(a_path.clone()));
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(8);
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
        crate::extensions::PluginConfirmationRequest {
            token: 42,
            title: "A".into(),
            message: "Answer for A".into(),
            default_answer: crate::extensions::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    app.state
        .open_text(TextBuffer::from_text("B"), Some(b_path));
    app.handle_configured_key("enter".parse().unwrap());
    let answer = fixture.take();
    assert!(answer.snapshot().is_none());
    assert!(matches!(
        answer.confirmation_results()[0].outcome,
        crate::extensions::PluginConfirmationOutcome::Answered(
            crate::extensions::PluginConfirmationAnswer::No
        )
    ));

    fixture.reply(&answer, Vec::new());
    app.process_plugin_events();
    let active = app.state.active_buffer_id().unwrap();
    assert_eq!(app.state.buffers[active].text.to_string(), "B");
}

#[test]
fn changed_confirmation_document_gets_a_terminal_failure() {
    let mut app = app();
    let path = app.state.workspace_root.join("a.txt");
    let (buffer, _) = app
        .state
        .open_text(TextBuffer::from_text("A"), Some(path.clone()));
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(8);
    app.extension.fixture_manager(manager);
    app.process_effect(AppEffect::PluginExecute {
        plugin: "demo".into(),
        command: "run".into(),
        arguments: Vec::new(),
    });
    let invocation = fixture.take();
    fixture.reply_confirmation_request(
        &invocation,
        crate::extensions::PluginConfirmationRequest {
            token: 43,
            title: "A".into(),
            message: "Answer for unchanged A".into(),
            default_answer: crate::extensions::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    assert!(
        app.state
            .replace_buffer_from_disk(buffer, TextBuffer::from_text("changed A"), path,)
    );
    app.handle_configured_key("enter".parse().unwrap());
    let retired = fixture.take();
    assert!(retired.snapshot().is_none());
    assert!(matches!(
        retired.confirmation_results()[0].outcome,
        crate::extensions::PluginConfirmationOutcome::Failed(
            crate::extensions::PluginConfirmationError {
                code: crate::extensions::PluginConfirmationErrorCode::SourceRetired,
                ..
            }
        )
    ));
}

#[test]
fn stale_ordinary_reply_releases_only_its_exact_confirmation_reservation() {
    let mut app = app();
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(8);
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
        crate::extensions::PluginConfirmationRequest {
            token: 61,
            title: "Still reserved".into(),
            message: "The second invocation retained its exact slot".into(),
            default_answer: crate::extensions::PluginConfirmationAnswer::No,
        },
    );
    app.process_plugin_events();
    let retired = fixture.take();
    assert_eq!(retired.confirmation_results()[0].request, 61);
    assert!(matches!(
        retired.confirmation_results()[0].outcome,
        crate::extensions::PluginConfirmationOutcome::Failed(
            crate::extensions::PluginConfirmationError {
                code: crate::extensions::PluginConfirmationErrorCode::SourceRetired,
                ..
            }
        )
    ));
}

#[test]
fn confirmation_capacity_is_reserved_before_the_guest_call() {
    let mut app = app();
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(128);
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
    use sha2::{Digest, Sha256};
    let core = include_bytes!("../../../../tests/fixtures/retained-guest/retained-guest.core.wasm");
    let component = wit_component::ComponentEncoder::default()
        .module(core)
        .expect("fixture metadata")
        .validate(true)
        .encode()
        .expect("component");
    let store = tempfile::tempdir().expect("plugin store");
    fs::write(store.path().join("fixture.wasm"), &component).unwrap();
    let manifest = crate::editor::session::extensions::fixture_manifest(format!(
        "{:x}",
        Sha256::digest(&component)
    ));
    write_plugin_manifest(store.path(), &manifest);

    let mut app = app_with_plugin_queue(64);
    app.state.open_text(TextBuffer::from_text("editor"), None);
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
    while !app.extension.is_active_generation("demo", load) {
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
                "production execution queue did not admit reservation {expected}"
            );
            thread::yield_now();
        }
    }
    assert_eq!(app.extension.confirmation_count(), 64);

    // Transfer reservations to pending confirmations before opening a modal changes capture.
    // This keeps the ordinary probe separate from SourceRetired result-only callbacks.
    let reply_deadline = Instant::now() + Duration::from_secs(15);
    while !app
        .notices
        .history()
        .any(|(_, text)| text == "confirmationprobe:64:accepted")
    {
        for event in app.extension.poll_owned().unwrap() {
            if let crate::extensions::OwnedPluginEvent::Executed(reply) = event {
                app.apply_plugin_execution(reply);
            }
        }
        assert!(
            !matches!(
                app.extension.statuses().get("demo"),
                Some(crate::extensions::PluginStatus::Failed(_))
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
            if let crate::extensions::OwnedPluginEvent::Executed(reply) = event {
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
    // SourceRetired result-only callbacks, so ownership can legitimately decline after polling.
    assert!(app.extension.confirmation_count() <= 64);
}

#[test]
fn packaged_production_02_component_clone_refusal_never_enqueues_a_guest_callback() {
    use sha2::{Digest, Sha256};
    let core = include_bytes!("../../../../tests/fixtures/retained-guest/retained-guest.core.wasm");
    assert_eq!(
        format!("{:x}", Sha256::digest(core)),
        "3773ae204a9b1fe0436f2252161663562be895aea2392b1b4bd187c43af74416"
    );
    let component = wit_component::ComponentEncoder::default()
        .module(core)
        .expect("fixture metadata")
        .validate(true)
        .encode()
        .expect("component");
    let store = tempfile::tempdir().expect("plugin store");
    fs::write(store.path().join("fixture.wasm"), &component).unwrap();
    let manifest = crate::editor::session::extensions::fixture_manifest(format!(
        "{:x}",
        Sha256::digest(&component)
    ));
    write_plugin_manifest(store.path(), &manifest);

    for (command, private) in [("drive", false), ("private", true)] {
        let mut app = app();
        app.state.open_text(TextBuffer::from_text("editor"), None);
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
        while !app.extension.is_active_generation("demo", load) {
            app.process_plugin_events();
            assert!(Instant::now() < deadline, "fixture activation timed out");
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
                    content.adapter == crate::ui::window::ContentAdapterKind::PrivateCellSurface
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
            .clone_graph_window(&target, crate::editor::Axis::Horizontal)
            .expect_err("production 0.2 presenter clone must be rejected");
        assert!(error.contains("clone-view"), "{error}");
        assert_eq!(app.window_session.graph(), &graph);
        assert_eq!(app.state.window_version(), editor);
        assert_eq!(app.extension.contributions().version(), catalogue);
        assert_eq!(app.ui.plugin_surfaces, retained);
        assert_eq!(app.ui.private_surfaces, private_surfaces);
        assert_eq!(app.extension.fixture_delivery_state(), delivery);

        app.extension
            .disable_generation("demo", load)
            .expect("disable actual component generation");
        app.remove_plugin_contribution_generation("demo", load);
        while app.extension.fixture_queued() != 0 {
            app.process_plugin_events();
            assert!(Instant::now() < deadline, "fixture unload timed out");
            thread::yield_now();
        }
        assert!(app.extension.contributions().get("demo").is_none());
        let graph = app.window_session.graph().clone();
        let delivery = app.extension.fixture_delivery_state();
        assert!(
            app.clone_graph_window(&target, crate::editor::Axis::Horizontal)
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
        while !app.extension.is_active_generation("demo", replacement) {
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
            app.clone_graph_window(&target, crate::editor::Axis::Horizontal)
                .is_err()
        );
        assert_eq!(app.window_session.graph(), &graph);
        assert_eq!(app.extension.fixture_delivery_state(), delivery);
    }
}

#[test]
fn packaged_real_guest_confirmation_updates_its_custom_view() {
    use sha2::{Digest, Sha256};
    let core = include_bytes!("../../../../tests/fixtures/retained-guest/retained-guest.core.wasm");
    let component = wit_component::ComponentEncoder::default()
        .module(core)
        .expect("fixture metadata")
        .validate(true)
        .encode()
        .expect("component");
    let store = tempfile::tempdir().expect("plugin store");
    fs::write(store.path().join("fixture.wasm"), &component).unwrap();
    let manifest = crate::editor::session::extensions::fixture_manifest(format!(
        "{:x}",
        Sha256::digest(&component)
    ));
    write_plugin_manifest(store.path(), &manifest);

    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
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
    while !app.extension.is_active_generation("demo", load) {
        app.process_plugin_events();
        assert!(Instant::now() < deadline, "fixture activation timed out");
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
            crate::editor::session::extensions::PluginUiInteraction::Activate,
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
        app.apply_retained_ui_interaction(
            crate::editor::session::extensions::PluginUiInteraction::Activate,
        );
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
fn retained_plugin_window_projection_is_idle_and_generation_owned() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    let index = crate::extensions::index_manifest_contributions(
        &manifest,
        77,
        "manifest.toml:bytes 0..1".into(),
    )
    .expect("index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    let request = crate::extensions::PluginUiRequest {
        token: 1,
        kind: crate::extensions::PluginUiRequestKind::OpenWindow {
            expected_graph_revision: app.window_session.graph().revision().expect("revision"),
            content_type: "plugin.demo.panel".into(),
            title: "Demo".into(),
            placement: crate::extensions::PluginRequestedPlacement::Floating {
                x: 5,
                y: 3,
                width: 24,
                height: 7,
            },
            tree: Some(crate::extensions::PluginUiTree {
                revision: 1,
                root: 1,
                nodes: vec![crate::extensions::PluginUiNode {
                    id: 1,
                    kind: crate::extensions::PluginUiNodeKind::Text,
                    text: "retained plugin".into(),
                    style: crate::extensions::PluginUiStyle::Accent,
                    action: None,
                    children: Vec::new(),
                }],
            }),
        },
    };
    let crate::editor::session::extensions::WindowUpdate::Publish(source, transition) = app
        .extension
        .prepare_window(&index, request.clone(), &app.state, &app.window_session)
    else {
        panic!("expected staged window");
    };
    app.window_session.invalidate_native();
    let receipt = app.window_session.accept_plugin_transition(
        *transition,
        &app.state.window_version(),
        source.catalog(),
    );
    assert!(
        source
            .finish(receipt, app.window_session.graph().revision())
            .error
            .is_some()
    );
    assert!(app.extension.window_snapshots().is_empty());
    let crate::editor::session::extensions::WindowUpdate::Publish(source, transition) = app
        .extension
        .prepare_window(&index, request.clone(), &app.state, &app.window_session)
    else {
        panic!("expected fresh window proposal");
    };
    let (other, _) = app
        .window_session
        .begin_plugin(
            &index.owner,
            index.generation,
            (&app.state.window_version(), source.catalog()),
        )
        .unwrap()
        .seal();
    let receipt = app.window_session.accept_plugin_transition(
        other,
        &app.state.window_version(),
        source.catalog(),
    );
    assert!(
        source
            .finish(receipt, app.window_session.graph().revision())
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
    let reconciliation_count = app.window_session.editor_ids().reconciliation_count();
    let graph = app.window_session.graph().clone();
    for _ in 0..256 {
        app.reconcile_editor_windows();
    }
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        reconciliation_count
    );
    assert_eq!(app.window_session.graph(), &graph);
    app.remove_plugin_contribution_generation("demo", PluginLoadId(77));
    assert!(app.ui.plugin_surfaces.is_empty());
    assert!(app.window_session.graph().window(window).is_none());
}

#[test]
fn plugin_host_events_are_snapshot_owned_coalesced_and_idle() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::extensions::PluginCapability::EventSubscriptions);
    manifest.contributions.subscriptions =
        vec![crate::extensions::PluginSubscriptionContribution {
            id: "plugin.demo.subscription.buffer".into(),
            kind: crate::extensions::PluginSubscriptionKind::BufferSummaryChanged,
            maximum_queued: 8,
            maximum_rate_per_second: 120,
            coalesce: true,
            provenance: None,
        }];
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 77, "event manifest".into())
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

    let view = app.state.active_view.expect("view");
    app.state.views[view].cursor = ByteOffset(1);
    app.refresh_plugin_host_events();
    let event = app
        .extension
        .deliver("demo", PluginLoadId(77))
        .pop()
        .expect("coalesced buffer event");
    assert_eq!(event.coalesced, 1);
    assert!(event.payload.contains("cursor=1"));
    app.remove_plugin_contribution_generation("demo", PluginLoadId(77));
    assert_eq!(app.extension.events().pending("demo", PluginLoadId(77)), 0);
}

#[test]
fn plugin_picker_provider_uses_the_generic_presenter_and_cancels_exactly() {
    let mut app = app();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::extensions::PluginCapability::PickerProviders);
    manifest.contributions.providers = vec![crate::extensions::PluginProviderContribution {
        id: "plugin.demo.provider.files".into(),
        kind: crate::extensions::PluginProviderKind::PickerSource,
        command: "plugin.demo.run".into(),
        maximum_concurrent_requests: 1,
        maximum_items: 16,
        maximum_bytes: 8_192,
        deadline_milliseconds: 30_000,
        provenance: None,
    }];
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 77, "provider manifest".into())
            .expect("provider index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    app.extension
        .fixture_provider_install(&index)
        .expect("provider owner");
    let request = app
        .extension
        .fixture_provider_start(crate::extensions::PluginProviderStart {
            plugin: "demo".into(),
            load: PluginLoadId(77),
            provider: "plugin.demo.provider.files".into(),
            query: "be".into(),
            context: "editor".into(),
            workspace_revision: Some(app.workspace.root_revision()),
            buffer_revision: None,
        })
        .expect("request");
    app.extension
        .provider_chunk(
            "demo",
            PluginLoadId(77),
            (Some(app.workspace.root_revision()), None),
            crate::extensions::PluginPickerChunk {
                request,
                sequence: 1,
                items: vec![
                    crate::extensions::PluginPickerItem {
                        id: "z".into(),
                        primary: "Zebra".into(),
                        secondary: None,
                        action_data: vec![1],
                    },
                    crate::extensions::PluginPickerItem {
                        id: "b".into(),
                        primary: "Beta".into(),
                        secondary: Some("ranked first".into()),
                        action_data: vec![2],
                    },
                ],
                terminal: crate::extensions::PluginProviderTerminal::More,
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
            crate::extensions::PluginPickerChunk {
                request,
                sequence: 2,
                items: vec![crate::extensions::PluginPickerItem {
                    id: "better".into(),
                    primary: "Better".into(),
                    secondary: None,
                    action_data: Vec::new(),
                }],
                terminal: crate::extensions::PluginProviderTerminal::Complete,
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
                crate::extensions::PluginPickerChunk {
                    request,
                    sequence: 2,
                    items: Vec::new(),
                    terminal: crate::extensions::PluginProviderTerminal::Complete,
                },
            )
            .unwrap_err()
            .code,
        crate::extensions::PluginProviderErrorCode::UnknownRequest
    );
    // Reopening a one-request provider reserves new metadata without destroying
    // the displayed source if subsequent picker admission is refused.
    let mut broker = crate::extensions::PluginProviderBroker::default();
    broker.install(&index).unwrap();
    let start = || crate::extensions::PluginProviderStart {
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
        crate::extensions::PluginProviderErrorCode::QueueFull
    );
    broker.cancel(next);
    assert!(broker.presentable(old));
    let next = broker.reserve_replacing(start(), Some(old)).unwrap();
    broker.cancel(old);
    assert!(broker.presentable(next));
}

#[test]
fn private_plugin_surface_geometry_snapshot_and_idle_projection_are_application_owned() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::extensions::PluginCapability::PrivateCells);
    let content = &mut manifest.contributions.content_types[0];
    content.presenter = crate::extensions::PluginPresenterKind::PrivateCells;
    content.private_cells = Some(crate::extensions::PluginPrivateCellPolicy {
        maximum_cells: 64,
        maximum_patch_cells: 32,
        maximum_dirty_rectangles: 8,
        maximum_styles: 8,
        maximum_hit_regions: 8,
        maximum_bytes: 4_096,
        cursor: true,
    });
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 77, "private manifest".into())
            .expect("index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    let revision = app.window_session.graph().revision().expect("revision");
    let opened = app.apply_plugin_ui_request(
        &index,
        crate::extensions::PluginUiRequest {
            token: 1,
            kind: crate::extensions::PluginUiRequestKind::OpenWindow {
                expected_graph_revision: revision,
                content_type: "plugin.demo.panel".into(),
                title: "Cells".into(),
                placement: crate::extensions::PluginRequestedPlacement::Floating {
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
        crate::extensions::PluginUiRequest {
            token: 2,
            kind: crate::extensions::PluginUiRequestKind::ReplacePrivate {
                handle,
                snapshot: crate::extensions::PluginPrivateSnapshot {
                    revision: 1,
                    dimensions_revision: geometry.dimensions_revision,
                    styles: vec![crate::extensions::PluginPrivateStyle {
                        id: 1,
                        style: crate::extensions::PluginUiStyle::Accent,
                    }],
                    cells: vec![crate::extensions::PluginPrivateCell {
                        x: 0,
                        y: 0,
                        grapheme: "X".into(),
                        continuation: false,
                        style: 1,
                    }],
                    cursor: Some(crate::extensions::PluginPrivateCursor { x: 0, y: 0 }),
                    hit_regions: vec![crate::extensions::PluginPrivateHitRegion {
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
    let reconciliations = app.window_session.editor_ids().reconciliation_count();
    for _ in 0..256 {
        app.reconcile_editor_windows();
    }
    assert_eq!(app.window_session.graph(), &graph);
    assert_eq!(app.ui.private_surfaces, ui);
    assert_eq!(
        app.window_session.editor_ids().reconciliation_count(),
        reconciliations
    );
}

#[test]
fn retained_interaction_commands_use_host_focus_scroll_and_mouse_state() {
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("editor"), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 77, "manifest.toml".into())
            .expect("index");
    app.extension
        .fixture_contribution(index.clone())
        .expect("catalog");
    let revision = app.window_session.graph().revision().expect("revision");
    let event = app.apply_plugin_ui_request(
        &index,
        crate::extensions::PluginUiRequest {
            token: 1,
            kind: crate::extensions::PluginUiRequestKind::OpenWindow {
                expected_graph_revision: revision,
                content_type: "plugin.demo.panel".into(),
                title: "Demo".into(),
                placement: crate::extensions::PluginRequestedPlacement::Floating {
                    x: 4,
                    y: 2,
                    width: 24,
                    height: 7,
                },
                tree: Some(crate::extensions::PluginUiTree {
                    revision: 1,
                    root: 1,
                    nodes: vec![
                        crate::extensions::PluginUiNode {
                            id: 1,
                            kind: crate::extensions::PluginUiNodeKind::Container,
                            text: String::new(),
                            style: crate::extensions::PluginUiStyle::Default,
                            action: None,
                            children: vec![2, 3],
                        },
                        crate::extensions::PluginUiNode {
                            id: 2,
                            kind: crate::extensions::PluginUiNodeKind::List,
                            text: "List".into(),
                            style: crate::extensions::PluginUiStyle::Default,
                            action: None,
                            children: Vec::new(),
                        },
                        crate::extensions::PluginUiNode {
                            id: 3,
                            kind: crate::extensions::PluginUiNodeKind::Button,
                            text: "Run".into(),
                            style: crate::extensions::PluginUiStyle::Accent,
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
    app.invoke_mapped_command("surface.retained.focus.next");
    assert_eq!(
        app.ui.plugin_surfaces[0]
            .rows
            .iter()
            .find(|row| row.focused)
            .map(|row| row.node),
        Some(3)
    );
    app.invoke_mapped_command("surface.retained.scroll.down");
    assert_eq!(app.ui.plugin_surfaces[0].scroll, 1);
    let window = app.ui.plugin_surfaces[0].window;
    app.window_session.invalidate_native();
    app.reconcile_editor_windows();
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
fn stale_generation_cleanup_preserves_replacement_ui_events() {
    let mut app = app();
    let event = |token| crate::extensions::PluginUiEvent {
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
    let storage_result = |load, token| crate::extensions::PluginStorageResult {
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
    let replacement = crate::extensions::index_manifest_contributions(
        &manifest,
        78,
        "replacement manifest".into(),
    )
    .expect("replacement index");
    app.extension
        .fixture_contribution(replacement)
        .expect("replacement catalog");
    app.remove_plugin_contribution_generation("demo", PluginLoadId(77));
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
            .map(|index| index.generation),
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
    let mut app = app();
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
    assert!(!app.extension.is_active_generation("bad", bad_load));
    assert!(!app.extension.is_active_generation("good", good_load));
}

#[test]
fn deferred_user_plugin_move_compiles_after_index_then_falls_back_after_activation_failure() {
    let mut app = app();
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
        super::resolve_configuration(&app.commands, loaded, true).expect("deferred config");
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
    assert!(!app.extension.is_active_generation("demo", load));
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
    let app = app();
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
    let (resolved, diagnostic) = super::resolve_configuration(&app.commands, loaded, true)
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
    assert!(super::resolve_configuration(&app.commands, unknown, true).is_err());
    let mut unknown_window = crate::config::load_bundled_config();
    unknown_window
        .config
        .windows
        .rules
        .push(crate::config::WindowRuleConfig {
            content_type: "plugin.unknown.panel".into(),
            placement: crate::config::WindowPlacementConfig::Hidden,
        });
    assert!(super::resolve_configuration(&app.commands, unknown_window, true).is_err());

    let mut running = self::app();
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
    fn index(owner: &str, generation: u64) -> crate::extensions::PluginContributionIndex {
        crate::extensions::PluginContributionIndex {
            owner: owner.into(),
            version: "1.0.0".into(),
            checksum: "0".repeat(64),
            generation,
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

    let mut app = app();
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
    app.handle_plugin_worker_stopped(crate::extensions::PluginError::Stopped);
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
fn virtual_publication_keeps_original_request_until_checked_window_acceptance() {
    use crate::editor::session::extensions::VirtualContext;
    let mut app = app();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::extensions::PluginCapability::VirtualDocuments);
    manifest.contributions.virtual_documents =
        vec![crate::extensions::PluginVirtualDocumentContribution {
            id: "plugin.demo.virtual.help".into(),
            title: "Help".into(),
            language: "markdown".into(),
            mode: crate::extensions::PluginVirtualDocumentMode::ReadOnly,
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
        crate::extensions::index_manifest_contributions(&manifest, 7, "virtual fixture".into())
            .unwrap();
    app.extension.fixture_contribution(index.clone()).unwrap();
    let request = app.extension.fixture_virtual_request(&index);
    let proposal = crate::extensions::PluginVirtualDocumentProposal {
        request: request.request,
        document: request.document,
        cancellation_generation: request.cancellation_generation,
        kind: crate::extensions::PluginVirtualDocumentProposalKind::Full(
            crate::extensions::PluginVirtualDocumentFull {
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
    let (ordinary, view) = app.state.open_text(TextBuffer::from_text("ordinary"), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let focus = app.window_session.graph().focused();
    app.remove_plugin_contribution_generation("demo", PluginLoadId(7));
    assert!(!app.state.buffers.contains_key(virtual_buffer));
    assert_eq!(app.state.active_buffer_id(), Some(ordinary));
    assert_eq!(app.state.active_view, Some(view));
    assert_eq!(app.window_session.graph().focused(), focus);
    assert_eq!(app.state.buffers[ordinary].text.to_string(), "ordinary");
}

#[test]
fn virtual_document_titles_and_languages_reach_windows_and_syntax() {
    let mut app = app();
    let before = app.state.version();
    let mut draft = app.state.draft();
    let mut snapshot = crate::extensions::PluginVirtualDocumentSnapshot {
        plugin: "demo".into(), load: PluginLoadId(7), document: 3,
        source: "plugin.demo.virtual.help".into(), key: "help".into(),
        title: "Help".into(), language: "rust".into(),
        text: "fn help() {}\n".into(), revision: 1, closed: false,
    };
    let buffer = app.extension.fixture_virtual_publish(&mut draft, &snapshot).unwrap();
    *app.state = draft.accept(&before).unwrap();
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    assert!(app.ui.content_tabs.iter().any(|tab| tab.title == "Help"));
    let deadline = Instant::now() + Duration::from_secs(3);
    while app.syntax.result(buffer).is_none() {
        app.process_syntax();
        assert!(Instant::now() < deadline, "declared Rust language was not parsed");
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(app.syntax.result(buffer).unwrap().language, crate::language::syntax::SyntaxLanguage::Rust);
    snapshot.revision = 2;
    snapshot.title = "Updated help".into();
    snapshot.language = "python".into();
    snapshot.text = "def help(): pass\n".into();
    let before = app.state.version();
    let mut draft = app.state.draft();
    assert_eq!(app.extension.fixture_virtual_publish(&mut draft, &snapshot).unwrap(), buffer);
    *app.state = draft.accept(&before).unwrap();
    app.reconcile_editor_windows();
    assert!(app.ui.content_tabs.iter().any(|tab| tab.title == "Updated help"));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        app.process_syntax();
        if app.syntax.result(buffer).is_some_and(|result| result.language == crate::language::syntax::SyntaxLanguage::Python) {
            break;
        }
        assert!(Instant::now() < deadline, "updated language was not parsed");
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn provider_selection_delivers_request_item_and_action_data() {
    let mut app = app();
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::extensions::PluginCapability::PickerProviders);
    manifest.contributions.providers = vec![crate::extensions::PluginProviderContribution {
        id: "plugin.demo.provider.files".into(),
        kind: crate::extensions::PluginProviderKind::PickerSource,
        command: "plugin.demo.run".into(),
        maximum_concurrent_requests: 1,
        maximum_items: 16,
        maximum_bytes: 8192,
        deadline_milliseconds: 30_000,
        provenance: None,
    }];
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 1, "picker fixture".into())
            .unwrap();
    app.extension.fixture_contribution(index.clone()).unwrap();
    app.extension.fixture_provider_install(&index).unwrap();
    let request = app
        .extension
        .fixture_provider_start(crate::extensions::PluginProviderStart {
            plugin: "demo".into(),
            load: PluginLoadId(1),
            provider: "plugin.demo.provider.files".into(),
            query: String::new(),
            context: String::new(),
            workspace_revision: None,
            buffer_revision: None,
        })
        .unwrap();
    app.extension
        .provider_chunk(
            "demo",
            PluginLoadId(1),
            (None, None),
            crate::extensions::PluginPickerChunk {
                request,
                sequence: 1,
                items: vec![crate::extensions::PluginPickerItem {
                    id: "selected".into(),
                    primary: "Selected item".into(),
                    secondary: None,
                    action_data: vec![0, 255],
                }],
                terminal: crate::extensions::PluginProviderTerminal::Complete,
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
fn plugin_surface_observations_include_counts_and_geometry_preserves_receipts() {
    let mut app = app();
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::extensions::PluginCapability::EventSubscriptions);
    manifest.contributions.subscriptions =
        vec![crate::extensions::PluginSubscriptionContribution {
            id: "plugin.demo.subscription.surface".into(),
            kind: crate::extensions::PluginSubscriptionKind::PluginSurfaceChanged,
            maximum_queued: 1,
            maximum_rate_per_second: 30,
            coalesce: true,
            provenance: None,
        }];
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 1, "surface fixture".into())
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
    let event = |token, width: Option<u16>| crate::extensions::PluginUiEvent {
        token,
        handle: Some(5),
        graph_revision: None,
        tree_revision: None,
        surface_revision: None,
        action: None,
        error: None,
        geometry: width.map(|width| crate::extensions::PluginSurfaceGeometry {
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
    let mut app = app();
    let mut catalog = crate::extensions::PluginContributionCatalog::default();
    for owner in ["demo", "other"] {
        let mut manifest = plugin_manifest(owner, &format!("plugin.{owner}.keymap.open"), "x y");
        manifest
            .contributions
            .key_groups
            .push(crate::extensions::PluginKeyGroupContribution {
                id: format!("plugin.{owner}.group.open"),
                context: "dashboard".into(),
                mode: String::new(),
                prefix: "x".into(),
                label: owner.into(),
                icon: None,
                order: 1,
            });
        let index =
            crate::extensions::index_manifest_contributions(&manifest, 1, "keys fixture".into())
                .unwrap();
        app.register_indexed_plugin_commands(&index).unwrap();
        catalog.install(index).unwrap();
    }
    let mut loaded = crate::config::load_bundled_config();
    let compile = |loaded: &crate::config::LoadedConfig| {
        crate::editor::session::configuration::compile_keymaps_with_plugin_catalog(
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
    let mut app = app();
    let (manager, _fixture) = crate::extensions::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let mut manifest = plugin_manifest("demo", "plugin.demo.keymap.dashboard", "x y");
    manifest
        .capabilities
        .insert(crate::extensions::PluginCapability::VirtualDocuments);
    manifest.contributions.virtual_documents =
        vec![crate::extensions::PluginVirtualDocumentContribution {
            id: "plugin.demo.virtual.help".into(),
            title: "Help".into(),
            language: "markdown".into(),
            mode: crate::extensions::PluginVirtualDocumentMode::ReadOnly,
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
        crate::extensions::index_manifest_contributions(&manifest, 1, "virtual fixture".into())
            .unwrap();
    app.extension.fixture_contribution(index.clone()).unwrap();
    let request = app.extension.fixture_virtual_request(&index);
    app.state.open_text(TextBuffer::from_text("original"), None);
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    let navigation = app.window_session.navigation_stamp(app.state.window_version(), app.state.active_view);
    for _ in 0..2 {
        assert!(matches!(
            app.extension.open_virtual("plugin.demo.virtual.help", "help".into(), &app.state),
            Err(error) if error == "virtual document is already opening"
        ));
    }
    app.state.new_tab().unwrap();
    app.reconcile_editor_windows();
    let before = app.state.version();
    let active = app.state.active_tab;
    let navigation_current = app.window_session.navigation_current(&navigation, &app.state.window_version(), app.state.active_view);
    assert!(!navigation_current);
    let proposal = crate::extensions::PluginVirtualDocumentProposal {
        request: request.request, document: request.document,
        cancellation_generation: request.cancellation_generation,
        kind: crate::extensions::PluginVirtualDocumentProposalKind::Full(crate::extensions::PluginVirtualDocumentFull {
            expected_revision: None, revision: 1, title: "Help".into(),
            language: "rust".into(), text: "fn help() {}".into(),
        }),
    };
    assert!(app.extension.prepare_virtual("demo", PluginLoadId(1), proposal, navigation_current,
        crate::editor::session::extensions::VirtualContext {
            editor: &app.state, windows: &app.window_session, scratch: "scratch",
        }).is_err());
    assert_eq!(app.state.version(), before);
    assert_eq!(app.state.active_tab, active);
    assert!(app.state.buffers.values().all(|buffer| !buffer.is_virtual_read_only()));
    assert!(matches!(app.extension.open_virtual("plugin.demo.virtual.help", "help".into(), &app.state),
        Ok(crate::editor::session::extensions::VirtualOpen::Execute { .. })));
}

#[test]
fn active_plugin_failure_does_not_abort_a_siblings_index() {
    let mut app = app();
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(4);
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
}

#[test]
fn synchronous_plugin_load_refusal_retires_the_deferred_batch() {
    let mut app = app_with_plugin_queue(1);
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
    let mut app = app();
    let (manager, fixture) = crate::extensions::ExecutionFixture::new(4);
    app.extension.fixture_manager(manager);
    let mut manifest = plugin_manifest("demo", "plugin.demo.test", "x y");
    manifest.commands.get_mut("run").unwrap().arguments.insert(
        "value".into(),
        crate::extensions::PluginArgumentDeclaration {
            kind: crate::extensions::PluginArgumentKind::String,
            required: true,
        },
    );
    manifest.contributions.keymaps[0]
        .arguments
        .insert("value".into(), toml::Value::String("configured".into()));
    let index =
        crate::extensions::index_manifest_contributions(&manifest, 1, "test".into()).unwrap();
    app.register_indexed_plugin_commands(&index).unwrap();
    app.invoke(
        crate::editor::CommandInvocation::new("plugin.demo.run", InvocationSource::Test)
            .unwrap()
            .with_argument("value", CommandValue::String("configured".into())),
    );
    assert_eq!(fixture.take().arguments(), ["configured"]);
}
