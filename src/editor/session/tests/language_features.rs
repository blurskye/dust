use super::*;

#[test]
fn lsp_documentation_popup_preserves_multiline_kind_and_cancels_without_notification() {
    let mut app = app();
    app.notify_message("keep unrelated notification".into(), false);
    app.present_lsp_result(
        "rust-analyzer",
        1,
        "textDocument/hover",
        "file:///tmp/test.rs",
        crate::language::lsp::LspFeatureResult::Hover(crate::language::lsp::LspHover {
            sections: vec![crate::language::lsp::LspMarkupContent {
                kind: crate::language::lsp::LspMarkupKind::Markdown,
                value: "first\nsecond 界".into(),
            }],
            range: None,
        }),
    );
    let popup = app.ui.documentation_popup.as_ref().expect("hover popup");
    assert_eq!(popup.kind, DocumentationPopupKind::Hover);
    assert_eq!(popup.sections[0].lines, ["first", "second 界"]);
    assert_eq!(popup.sections[0].markup, DocumentationMarkupKind::Markdown);
    assert_eq!(popup.provider, "rust-analyzer");
    assert!(popup.title.contains("rust-analyzer"));
    assert_eq!(
        app.ui.notification.as_deref(),
        Some("keep unrelated notification")
    );

    app.present_lsp_result(
        "rust-analyzer",
        2,
        "textDocument/signatureHelp",
        "file:///tmp/test.rs",
        crate::language::lsp::LspFeatureResult::Signature(crate::language::lsp::LspMarkupContent {
            kind: crate::language::lsp::LspMarkupKind::PlainText,
            value: "fn demo(\n  value: usize\n)".into(),
        }),
    );
    let popup = app
        .ui
        .documentation_popup
        .as_ref()
        .expect("signature popup");
    assert_eq!(popup.kind, DocumentationPopupKind::Signature);
    assert_eq!(popup.line_count(), 3);

    app.present_documentation_popup(
        DocumentationPopupKind::Hover,
        "bounded".into(),
        "provider".into(),
        Some(ByteRange::new(ByteOffset(0), ByteOffset(1)).unwrap()),
        vec![
            crate::language::lsp::LspMarkupContent {
                kind: crate::language::lsp::LspMarkupKind::PlainText,
                value: "first\nsecond".into(),
            },
            crate::language::lsp::LspMarkupContent {
                kind: crate::language::lsp::LspMarkupKind::Markdown,
                value: "third".into(),
            },
        ],
    );
    let bounded = app.ui.documentation_popup.as_ref().unwrap();
    assert_eq!(bounded.sections.len(), 2);
    assert_eq!(bounded.line_count(), 3);
    assert_eq!(bounded.source_range.unwrap().end, ByteOffset(1));
    app.handle_key_in_context("dashboard", KeyEvent::character('q'));
    assert!(app.ui.documentation_popup.is_none());
}

#[test]
fn inlay_hint_results_are_revision_owned_and_projected_at_exact_unicode_offsets() {
    let mut app = app();
    let path = app.state.workspace_root.join("hints.rs");
    let (buffer, _) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("let 界 = build();"),
        Some(path.clone()),
    );
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.lsp.inlay_hint_level = crate::config::InlayHintLevel::Max;
        app.configuration.install_fixture(fixture);
    }
    app.present_lsp_result(
        "rust-analyzer",
        6,
        "textDocument/inlayHint",
        &crate::language::lsp::file_uri(&path),
        crate::language::lsp::LspFeatureResult::InlayHints(Vec::new()),
    );
    assert_eq!(app.language.inlay_retries(), 1);
    assert!(app.language.inlay_pending());
    app.present_lsp_result(
        "rust-analyzer",
        7,
        "textDocument/inlayHint",
        &crate::language::lsp::file_uri(&path),
        crate::language::lsp::LspFeatureResult::InlayHints(vec![
            crate::language::lsp::LspInlayHint {
                position: crate::language::lsp::LspPosition {
                    line: 0,
                    character: 5,
                },
                label: " -> {unknown}".into(),
            },
        ]),
    );
    assert_eq!(app.language.inlay_retries(), 2);
    assert!(app.language.inlay_pending());
    assert_eq!(app.ui.lsp_inlay_hints[0].label, " -> {unknown}");
    app.present_lsp_result(
        "rust-analyzer",
        8,
        "textDocument/inlayHint",
        &crate::language::lsp::file_uri(&path),
        crate::language::lsp::LspFeatureResult::InlayHints(vec![
            crate::language::lsp::LspInlayHint {
                position: crate::language::lsp::LspPosition {
                    line: 0,
                    character: 5,
                },
                label: ": Value".into(),
            },
        ]),
    );
    assert_eq!(app.ui.lsp_inlay_hints.len(), 1);
    let hint = &app.ui.lsp_inlay_hints[0];
    assert_eq!(hint.buffer, buffer);
    assert_eq!(hint.revision, app.state.buffers[buffer].text.revision());
    assert_eq!(hint.position, ByteOffset("let 界".len()));
    assert_eq!(hint.label, ": Value");
    assert_eq!(app.language.inlay_retries(), 0);
    assert!(!app.language.inlay_pending());

    // A server answers with nothing while it loads, at either hint level.
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.lsp.inlay_hint_level = crate::config::InlayHintLevel::Standard;
        app.configuration.install_fixture(fixture);
    }
    for retry in 1..=5 {
        assert!(app.schedule_lsp_inlay_warmup_retry());
        assert_eq!(app.language.inlay_retries(), retry);
    }
    assert!(!app.schedule_lsp_inlay_warmup_retry());
}

#[test]
fn content_modified_before_any_loading_is_asked_again_a_few_times() {
    let mut app = app();
    let failure = |app: &mut InteractiveApplication| {
        app.handle_lsp_request_failure(
            "rust-analyzer".into(),
            7,
            "textDocument/definition".into(),
            r#"{"code":-32801,"message":"content modified"}"#.into(),
        );
    };
    for _ in 0..10 {
        app.lsp_asked = Some(crate::language::lsp::LspFeature::Definition);
        failure(&mut app);
        assert!(!app.ui.notification_error);
        assert!(app.lsp_waiting.is_some());
        assert!(app.lsp_retry_at.is_some());
        // Sending it again needs a server, so the test only lets the retry come due.
        app.lsp_waiting = None;
        app.lsp_retry_at = Some(Instant::now());
        app.process_lsp_retry();
        assert!(app.lsp_retry_at.is_none());
    }
    app.lsp_asked = Some(crate::language::lsp::LspFeature::Definition);
    failure(&mut app);
    assert!(app.ui.notification_error, "the eleventh answer is shown");
}

#[test]
fn background_inlay_failures_never_replace_the_user_notification() {
    let mut app = app();
    let path = app.state.workspace_root.join("inlay-failure.rs");
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("fn value() {}"), Some(path));
    let owner = super::LspProjectionOwner {
        server: "rust-analyzer".into(),
        buffer,
        view,
        revision: app.state.buffers[buffer].text.revision(),
        uri: "file:///inlay-failure.rs".into(),
        language: "rust".into(),
    };
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.lsp.inlay_hint_level = crate::config::InlayHintLevel::Max;
        app.configuration.install_fixture(fixture);
    }
    app.notify_message("stable notification".into(), false);
    app.language.fixture_inlay_source(Some((42, owner.clone())));

    app.handle_lsp_request_failure(
        "rust-analyzer".into(),
        41,
        "textDocument/inlayHint".into(),
        r#"{"code":-32801,"message":"content modified"}"#.into(),
    );
    assert_eq!(app.language.inlay_source(), Some((42, owner)));
    assert_eq!(app.language.inlay_retries(), 0);
    assert_eq!(app.ui.notification.as_deref(), Some("stable notification"));
    assert!(!app.ui.notification_error);

    app.handle_lsp_request_failure(
        "rust-analyzer".into(),
        42,
        "textDocument/inlayHint".into(),
        r#"{"code":-32801,"message":"content modified"}"#.into(),
    );
    assert!(app.language.inlay_source().is_none());
    assert_eq!(app.language.inlay_retries(), 1);
    assert!(app.language.inlay_pending());
    assert_eq!(app.ui.notification.as_deref(), Some("stable notification"));
    assert!(!app.ui.notification_error);
    assert!(app.language.logs().back().is_some_and(|line| {
        line.contains("inlay hint refresh failed") && line.contains("content modified")
    }));
}

#[test]
fn completion_documentation_is_typed_and_exact_request_owned() {
    let mut app = app();
    let path = app.state.workspace_root.join("completion.rs");
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("let value = 1;"), Some(path));
    app.set_dashboard_visibility(false);
    let owner = super::LspProjectionOwner {
        server: "fake-server".into(),
        buffer,
        view,
        revision: app.state.buffers[buffer].text.revision(),
        uri: "file:///completion.rs".into(),
        language: "rust".into(),
    };
    app.language.fixture_generation(7);
    app.language.fixture_projection(Some(owner.clone()));
    app.language
        .fixture_completion_source(Some((7, owner.clone())));
    app.language
        .fixture_completion_items(vec![crate::language::lsp::LspCompletionItem {
            label: "value".into(),
            filter_text: None,
            sort_text: None,
            detail: "usize".into(),
            insert_text: "value".into(),
            text_edit: None,
            additional_edits: Vec::new(),
            kind: None,
            snippet: false,
            documentation: Some(crate::language::lsp::LspMarkupContent {
                kind: crate::language::lsp::LspMarkupKind::Markdown,
                value: "**value docs**".into(),
            }),
            resolve_item: None,
            resolved: true,
        }]);
    app.ui.language_completion = Some(crate::ui::LanguageCompletionState {
        typed: 0,
        items: vec![crate::ui::CompletionItem {
            icon: None,
            color: None,
            label: "value".into(),
            detail: "usize".into(),
        }],
        selected: 0,
        scroll: 0,
    });
    app.refresh_lsp_completion_documentation();
    let popup = app
        .ui
        .documentation_popup
        .as_ref()
        .expect("selected completion documentation opens automatically");
    assert_eq!(popup.kind, DocumentationPopupKind::Completion);
    assert_eq!(popup.provider, "fake-server");
    assert_eq!(popup.sections[0].markup, DocumentationMarkupKind::Markdown);
    app.language.fixture_documentation(
        0,
        Some(crate::language::lsp::LspMarkupContent {
            kind: crate::language::lsp::LspMarkupKind::Markdown,
            value: (0..40)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        }),
    );
    app.refresh_lsp_completion_documentation();
    assert_eq!(app.active_input_context(), "completion");
    app.handle_configured_key("ctrl-d".parse::<KeyEvent>().expect("Ctrl-d"));
    assert_eq!(
        app.ui
            .documentation_popup
            .as_ref()
            .map(|popup| popup.scroll),
        Some(4)
    );
    assert!(app.ui.language_completion.is_some());

    app.language.fixture_generation(8);
    app.invoke_mapped_command("lsp.completion.documentation.open");
    assert!(app.ui.documentation_popup.is_none());
    assert!(app.ui.notification_error);

    app.language.fixture_generation(7);
    let revision = app.state.buffers[buffer].text.revision();
    app.state.buffers[buffer]
        .text
        .apply_edits(&crate::editor::text::TextEditSet::new(
            revision,
            vec![crate::editor::text::TextEdit::new(
                ByteRange::new(ByteOffset(0), ByteOffset(0)).unwrap(),
                "x",
            )],
        ))
        .unwrap();
    app.invoke_mapped_command("lsp.completion.documentation.open");
    assert!(app.ui.documentation_popup.is_none());

    let mut live_owner = owner;
    live_owner.revision = app.state.buffers[buffer].text.revision();
    app.language.fixture_projection(Some(live_owner.clone()));
    app.language
        .fixture_completion_source(Some((7, live_owner)));
    app.state
        .split_active(crate::editor::Axis::Horizontal, 500)
        .unwrap();
    app.invoke_mapped_command("lsp.completion.documentation.open");
    assert!(app.ui.documentation_popup.is_none());
    assert!(app.ui.notification_error);
}

fn app_waiting_to_save_formatted(name: &str) -> (InteractiveApplication, crate::editor::BufferId, std::path::PathBuf, String) {
    let mut app = app();
    let path = app.state.workspace_root.join(name);
    std::fs::write(&path, "a  b\n").unwrap();
    let (buffer, _) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text("a  b\n"),
        Some(path.clone()),
    );
    app.set_dashboard_visibility(false);
    app.state.set_buffer_dirty(buffer, true);
    app.reconcile_editor_windows();
    app.format_on_save = Some(super::super::FormatOnSave {
        buffer,
        quit_after: false,
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(60),
    });
    let uri = format!("file://{}", path.display());
    (app, buffer, path, uri)
}

#[test]
fn a_format_result_applies_without_a_review_and_finishes_the_waiting_save() {
    use crate::language::lsp::{
        LspDocumentEdit, LspFeatureResult, LspPosition, LspRange, LspTextEdit, LspWorkspaceEdit,
    };
    let (mut app, buffer, path, uri) = app_waiting_to_save_formatted("formatted.txt");
    let position = |character| LspPosition { line: 0, character };
    app.present_lsp_result(
        "formatter",
        1,
        "textDocument/formatting",
        &uri,
        LspFeatureResult::WorkspaceEdit(LspWorkspaceEdit {
            resources: Vec::new(),
            documents: vec![LspDocumentEdit {
                uri: String::new(),
                version: None,
                edits: vec![LspTextEdit {
                    range: LspRange {
                        start: position(1),
                        end: position(3),
                    },
                    new_text: " ".into(),
                }],
            }],
        }),
    );
    assert!(app.format_on_save.is_none());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "a b\n");
    super::wait_vfs(&mut app);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a b\n");
}

#[test]
fn an_empty_answer_while_the_server_loads_says_it_is_loading() {
    use crate::language::lsp::{LspFeatureResult, LspProgressKind};
    let (mut app, _, _, uri) = app_waiting_to_save_formatted("loading.rs");
    app.format_on_save = None;
    app.update_lsp_progress(
        "server".into(),
        "token".into(),
        LspProgressKind::Begin,
        "Indexing".into(),
        "4/330".into(),
        Some(1),
    );
    app.present_lsp_result("server", 1, "textDocument/hover", &uri, LspFeatureResult::Empty);
    assert!(!app.ui.notification_error);
    assert!(app.ui.lsp_loading.as_deref().is_some_and(|text| text.contains("Loading")));
    assert!(app.lsp_waiting.is_none(), "nothing was asked for");
    app.update_lsp_progress(
        "server".into(),
        "token".into(),
        LspProgressKind::End,
        String::new(),
        String::new(),
        None,
    );

    app.present_lsp_result("other", 1, "textDocument/hover", &uri, LspFeatureResult::Empty);
    assert!(app.ui.notification_error);
}

#[test]
fn code_actions_wait_for_every_server_and_list_them_together() {
    use crate::language::lsp::{LspCodeAction, LspFeatureResult, LspWorkspaceEdit};
    let (mut app, _, _, uri) = app_waiting_to_save_formatted("actions.py");
    app.format_on_save = None;
    let action = |title: &str| LspCodeAction {
        title: title.into(),
        kind: Some("quickfix".into()),
        preferred: false,
        edit: Some(LspWorkspaceEdit { resources: Vec::new(), documents: Vec::new() }),
        command: None,
    };
    app.language.offer_actions("types", vec![action("from types")], None);
    app.present_lsp_result(
        "linter",
        1,
        "textDocument/codeAction",
        &uri,
        LspFeatureResult::CodeActions(vec![action("from linter")]),
    );
    let labels = app
        .ui
        .picker
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .map(|row| row.label.clone())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"from types".to_owned()), "{labels:?}");
    assert!(labels.contains(&"from linter".to_owned()), "{labels:?}");
}

#[test]
fn a_format_replacing_the_whole_file_keeps_the_cursor_on_its_line() {
    use crate::language::lsp::{
        LspDocumentEdit, LspFeatureResult, LspPosition, LspRange, LspTextEdit, LspWorkspaceEdit,
    };
    let mut app = app();
    let path = app.state.workspace_root.join("whole.py");
    let source = "x  =  1\ny  =  2\nz  =  3\n";
    std::fs::write(&path, source).unwrap();
    let (buffer, view) = app.state.open_text(
        crate::editor::text::TextBuffer::from_text(source),
        Some(path.clone()),
    );
    app.set_dashboard_visibility(false);
    app.reconcile_editor_windows();
    app.state.views[view].cursor = crate::editor::text::ByteOffset(8);
    app.present_lsp_result(
        "formatter",
        1,
        "textDocument/formatting",
        &format!("file://{}", path.display()),
        LspFeatureResult::WorkspaceEdit(LspWorkspaceEdit {
            resources: Vec::new(),
            documents: vec![LspDocumentEdit {
                uri: String::new(),
                version: None,
                edits: vec![LspTextEdit {
                    range: LspRange {
                        start: LspPosition { line: 0, character: 0 },
                        end: LspPosition { line: 3, character: 0 },
                    },
                    new_text: "x = 1\ny = 2\nz = 3\n".into(),
                }],
            }],
        }),
    );
    let text = &app.state.buffers[buffer].text;
    assert_eq!(text.to_string(), "x = 1\ny = 2\nz = 3\n");
    assert_eq!(text.byte_to_line(app.state.views[view].cursor).unwrap(), 1);
}

#[test]
fn a_save_still_happens_when_the_formatter_has_nothing_fails_or_is_slow() {
    let (mut app, _, path, uri) = app_waiting_to_save_formatted("nothing.txt");
    app.present_lsp_result(
        "formatter",
        1,
        "textDocument/formatting",
        &uri,
        crate::language::lsp::LspFeatureResult::Empty,
    );
    assert!(app.format_on_save.is_none());
    super::wait_vfs(&mut app);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a  b\n");
    assert!(!app.state.buffers[app.state.active_buffer_id().unwrap()].dirty);

    let (mut app, buffer, ..) = app_waiting_to_save_formatted("failed.txt");
    app.handle_lsp_request_failure(
        "formatter".into(),
        1,
        "textDocument/formatting".into(),
        "formatter crashed".into(),
    );
    assert!(app.format_on_save.is_none());
    super::wait_vfs(&mut app);
    assert!(!app.state.buffers[buffer].dirty);

    let (mut app, buffer, ..) = app_waiting_to_save_formatted("slow.txt");
    app.process_format_on_save_deadline();
    assert!(app.format_on_save.is_some());
    app.format_on_save.as_mut().unwrap().deadline = std::time::Instant::now();
    app.process_format_on_save_deadline();
    assert!(app.format_on_save.is_none());
    super::wait_vfs(&mut app);
    assert!(!app.state.buffers[buffer].dirty);
}

#[test]
fn loading_a_session_refuses_to_replace_unsaved_work() {
    let (mut app, buffer, ..) = app_waiting_to_save_formatted("unsaved.txt");
    app.load_session_on_request();
    assert!(!app.session_load_requested);
    assert!(app.state.buffers.contains_key(buffer));
    assert!(app.state.buffers[buffer].dirty);
}
