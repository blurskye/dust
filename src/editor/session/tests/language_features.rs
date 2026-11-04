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
fn interactive_lsp_results_follow_the_request_cursor() {
    use crate::language::lsp::{LspFeature, LspFeatureResult, LspHover, LspMarkupContent, LspMarkupKind};
    for moved in [false, true] {
        let mut app = app();
        let path = app.state.workspace_root.join("hover.rs");
        let (buffer, view) = app.state.open_text(TextBuffer::from_text("first\nsecond"), Some(path));
        app.set_dashboard_visibility(false);
        let transport = super::language::fixture_owned_lsp(&mut app, "server", buffer);
        app.language.select("server");
        let document = app.active_lsp_document().unwrap().1;
        app.request_lsp(LspFeature::Hover);
        if moved { app.state.views[view].cursor = ByteOffset(6); }
        transport.publish_result(app.language.generation(), "textDocument/hover", &document,
            LspFeatureResult::Hover(LspHover {
                range: None,
                sections: vec![LspMarkupContent { kind: LspMarkupKind::PlainText, value: "first symbol".into() }],
            }));
        app.process_lsp_events();
        assert_eq!(app.ui.documentation_popup.is_some(), !moved);
    }
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
    app.lsp_asked_generation = Some(7);
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
fn stale_content_modified_failures_do_not_retry_the_latest_user_feature() {
    let mut app = app();
    app.lsp_asked = Some(crate::language::lsp::LspFeature::Definition);
    app.lsp_asked_generation = Some(7);
    app.ui.lsp_loading = Some("definition loading".into());
    for (generation, method) in [(6, "textDocument/hover"), (7, "textDocument/semanticTokens/full"), (6, "textDocument/definition")] {
        app.handle_lsp_request_failure("server".into(), generation, method.into(), "-32801 content modified".into());
        assert!(app.lsp_waiting.is_none());
        assert!(app.lsp_retry_at.is_none());
        assert_eq!(app.ui.lsp_loading.as_deref(), Some("definition loading"));
    }
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
        text: app.state.buffers[buffer].text.source_version(),
        uri: "file:///inlay-failure.rs".into(),
        language: "rust".into(),
    };
    {
        let mut fixture = app.configuration.fixture_snapshot();
        fixture.settings.lsp.inlay_hint_level = crate::config::InlayHintLevel::Max;
        app.configuration.install_fixture(fixture);
    }
    app.notify_message("stable notification".into(), false);
    app.ui.lsp_loading = Some("completion loading".into());
    app.language.fixture_inlay_source(Some((42, owner.clone())));

    app.handle_lsp_request_failure(
        "rust-analyzer".into(),
        41,
        "textDocument/inlayHint".into(),
        r#"{"code":-32801,"message":"content modified"}"#.into(),
    );
    assert_eq!(app.ui.lsp_loading.as_deref(), Some("completion loading"));
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
    assert_eq!(app.ui.lsp_loading.as_deref(), Some("completion loading"));
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
        text: app.state.buffers[buffer].text.source_version(),
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

fn app_waiting_to_save_formatted(
    name: &str,
) -> (
    InteractiveApplication,
    crate::editor::BufferId,
    std::path::PathBuf,
    String,
) {
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
        request: Some(("formatter".into(), 1)),
        buffer,
        source: app.state.buffers[buffer].text.source_version(),
        path: app.state.buffers[buffer].path.clone(),
        formatter: None,
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
    let transport = super::language::fixture_owned_lsp(&mut app, "formatter", buffer);
    let position = |character| LspPosition { line: 0, character };
    app.present_lsp_result_owned(
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
        Some(transport.context()),
    );
    assert!(app.format_on_save.is_none());
    assert_eq!(app.state.buffers[buffer].text.to_string(), "a b\n");
    super::wait_vfs(&mut app);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a b\n");
}

#[test]
fn formatting_syncs_unsent_edits_before_requesting_the_server() {
    use crate::language::lsp::LspFeature;
    let mut app = app();
    let path = app.state.workspace_root.join("unsynced.rs");
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("old"), Some(path));
    let transport = super::language::fixture_owned_lsp(&mut app, "formatter", buffer);
    app.language.select("formatter");
    app.state.buffers[buffer].text.restore_text("new unsent text").unwrap();
    app.request_lsp(LspFeature::Formatting);
    let synced = transport.synced_document().expect("sync precedes formatting");
    assert_eq!(synced.text, "new unsent text");
    assert_eq!(synced.version, app.state.buffers[buffer].text.revision().0);
    assert_eq!(transport.requested_feature(), Some(LspFeature::Formatting));
}

#[test]
fn formatting_rejects_missing_and_stale_document_sources() {
    use crate::language::lsp::*;
    for ownership in ["missing context", "missing source", "stale source"] {
        let mut app = app();
        let path = app.state.workspace_root.join("owned.rs");
        let (buffer, _) = app.state.open_text(TextBuffer::from_text("current"), Some(path.clone()));
        let source_buffer = if ownership == "missing source" {
            app.state.open_text(TextBuffer::from_text("other"), Some(path.with_file_name("other.rs"))).0
        } else { buffer };
        let transport = super::language::fixture_owned_lsp(&mut app, "formatter", source_buffer);
        if ownership == "stale source" {
            app.state.buffers[buffer].text.restore_text("newer").unwrap();
        }
        let before = app.state.buffers[buffer].text.to_string();
        app.present_lsp_result_owned("formatter", 1, "textDocument/formatting", &file_uri(&path),
            LspFeatureResult::WorkspaceEdit(LspWorkspaceEdit {
                resources: Vec::new(),
                documents: vec![LspDocumentEdit { uri: file_uri(&path), version: None,
                    edits: vec![LspTextEdit { range: LspRange {
                        start: LspPosition { line: 0, character: 0 },
                        end: LspPosition { line: 0, character: 5 },
                    }, new_text: "stale".into() }],
                }],
            }),
            (ownership != "missing context").then(|| transport.context()));
        assert_eq!(app.state.buffers[buffer].text.to_string(), before, "{ownership}");
    }
}

#[test]
fn debugger_navigation_converts_character_columns_for_open_and_unopened_files() {
    for already_open in [true, false] {
        let mut app = app();
        let path = app.state.workspace_root.join("debug.rs");
        fs::write(&path, "first\né😀value\n").unwrap();
        if already_open { app.state.open_text(TextBuffer::from_text("first\né😀value\n"), Some(path.clone())); }
        app.navigate_debug_location(crate::editor::session::debugger::DebugLocation { path: path.clone(), line: 2, column: 2 });
        wait_vfs(&mut app);
        let view = &app.state.views[app.state.active_view.unwrap()];
        assert_eq!(view.cursor, ByteOffset("first\né".len()));
        app.navigate_debug_location(crate::editor::session::debugger::DebugLocation { path, line: 2, column: u64::MAX });
        assert_eq!(app.state.views[app.state.active_view.unwrap()].cursor, ByteOffset("first\né😀value".len()));
    }
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
    app.present_lsp_result(
        "server",
        1,
        "textDocument/hover",
        &uri,
        LspFeatureResult::Empty,
    );
    assert!(!app.ui.notification_error);
    assert!(
        app.ui
            .lsp_loading
            .as_deref()
            .is_some_and(|text| text.contains("Loading"))
    );
    assert!(app.lsp_waiting.is_none(), "nothing was asked for");
    app.update_lsp_progress(
        "server".into(),
        "token".into(),
        LspProgressKind::End,
        String::new(),
        String::new(),
        None,
    );

    app.present_lsp_result(
        "other",
        1,
        "textDocument/hover",
        &uri,
        LspFeatureResult::Empty,
    );
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
        edit: Some(LspWorkspaceEdit {
            resources: Vec::new(),
            documents: Vec::new(),
        }),
        command: None,
    };
    app.language
        .offer_actions("types", vec![action("from types")], None);
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
fn null_code_actions_finish_aggregation_after_another_server_offers_actions() {
    use crate::language::lsp::*;
    let mut app = app();
    let path = app.state.workspace_root.join("actions.rs");
    let (buffer, _) = app.state.open_text(TextBuffer::from_text("fn main() {}"), Some(path));
    app.set_dashboard_visibility(false);
    let first = super::language::fixture_owned_lsp(&mut app, "first", buffer);
    let second = super::language::fixture_owned_lsp(&mut app, "second", buffer);
    app.language.select("first");
    let document = app.active_lsp_document().unwrap().1;
    let position = LspPosition { line: 0, character: 0 };
    app.request_lsp(LspFeature::CodeAction { range: LspRange { start: position.clone(), end: position }, diagnostics: Vec::new() });
    let generation = app.language.generation();
    first.publish_result(generation, "textDocument/codeAction", &document,
        LspFeatureResult::CodeActions(vec![LspCodeAction {
            title: "available action".into(), kind: None, preferred: false,
            edit: Some(LspWorkspaceEdit::default()), command: None,
        }]));
    app.process_lsp_events();
    assert!(app.ui.picker.is_none());
    assert!(app.language.request_pending());
    second.publish_result(generation, "textDocument/codeAction", &document, LspFeatureResult::Empty);
    app.process_lsp_events();
    assert!(!app.language.request_pending());
    assert!(app.ui.picker.as_ref().unwrap().rows.iter().any(|row| row.label == "available action"));
}

#[test]
fn null_completion_preserves_other_servers_items_without_an_error() {
    use crate::language::lsp::*;
    let mut app = app();
    let path = app.state.workspace_root.join("completion.rs");
    let (buffer, _) = app.state.open_text(TextBuffer::from_text(""), Some(path));
    app.set_dashboard_visibility(false);
    let first = super::language::fixture_owned_lsp(&mut app, "first", buffer);
    let second = super::language::fixture_owned_lsp(&mut app, "second", buffer);
    app.language.select("first");
    let document = app.active_lsp_document().unwrap().1;
    app.request_lsp(LspFeature::Completion);
    let generation = app.language.generation();
    first.publish_result(generation, "textDocument/completion", &document,
        LspFeatureResult::Completion { incomplete: false, items: vec![LspCompletionItem {
            label: "available".into(), insert_text: "available".into(),
            filter_text: None, sort_text: None, detail: String::new(), text_edit: None,
            additional_edits: Vec::new(), kind: None, snippet: false,
            documentation: None, resolve_item: None, resolved: true,
        }] });
    app.process_lsp_events();
    assert!(app.ui.language_completion.is_some());
    second.publish_result(generation, "textDocument/completion", &document, LspFeatureResult::Empty);
    app.process_lsp_events();
    assert_eq!(app.ui.language_completion.as_ref().unwrap().items[0].label, "available");
    assert!(!app.ui.notification_error);
}

#[test]
fn null_projection_results_clear_previous_ranges_and_commands_succeed_quietly() {
    use crate::language::lsp::*;
    let mut app = app();
    let path = app.state.workspace_root.join("highlights.rs");
    app.state.open_text(TextBuffer::from_text("symbol"), Some(path.clone()));
    let uri = file_uri(&path);
    app.present_lsp_result("server", 1, "textDocument/documentHighlight", &uri,
        LspFeatureResult::Ranges(vec![LspRange {
            start: LspPosition { line: 0, character: 0 },
            end: LspPosition { line: 0, character: 6 },
        }]));
    assert_eq!(app.ui.lsp_highlight_spans.len(), 1);
    app.present_lsp_result("server", 2, "textDocument/documentHighlight", &uri, LspFeatureResult::Empty);
    assert!(app.ui.lsp_highlight_spans.is_empty());
    app.notify_message("keep this".into(), false);
    app.present_lsp_result("server", 3, "workspace/executeCommand", &uri, LspFeatureResult::Empty);
    assert_eq!(app.ui.notification.as_deref(), Some("keep this"));
    assert!(!app.ui.notification_error);
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
    let transport = super::language::fixture_owned_lsp(&mut app, "formatter", buffer);
    app.present_lsp_result_owned(
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
                        start: LspPosition {
                            line: 0,
                            character: 0,
                        },
                        end: LspPosition {
                            line: 3,
                            character: 0,
                        },
                    },
                    new_text: "x = 1\ny = 2\nz = 3\n".into(),
                }],
            }],
        }),
        Some(transport.context()),
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

#[test]
fn formatter_cursor_restoration_keeps_the_visual_head_synchronized() {
    let mut app = app();
    let (buffer, view) = app.state.open_text(TextBuffer::from_text("a   bé\n"), None);
    app.state.views[view].cursor = ByteOffset(4);
    app.state.views[view].selection = Some(crate::editor::Selection {
        anchor: ByteOffset(0),
        head: ByteOffset(4),
        kind: crate::editor::SelectionKind::Character,
    });
    app.state.views[view].mode = crate::editor::EditorMode::VisualChar;
    let positions = app.view_positions(|_| true);
    app.state.buffers[buffer]
        .text
        .restore_text("a bé\n")
        .unwrap();
    app.state.views[view].cursor = ByteOffset(6);
    app.state.views[view].selection.as_mut().unwrap().head = ByteOffset(6);
    app.restore_view_positions(positions);
    assert_eq!(app.state.views[view].cursor.get(), 2);
    assert_eq!(
        app.state.views[view].selection.unwrap().head,
        app.state.views[view].cursor
    );
}

#[test]
fn formatter_code_changes_keep_the_cursor_mapped_by_the_edits() {
    let mut app = app();
    let (buffer, view) = app.state.open_text(TextBuffer::from_text("foo()\nbar()\n"), None);
    app.state.views[view].cursor = ByteOffset(6);
    let positions = app.view_positions(|_| true);
    let revision = app.state.buffers[buffer].text.revision();
    let transaction = crate::editor::EditorTransaction::new(app.transaction_id())
        .with_operation(crate::editor::EditorOperation::ApplyEdits {
            buffer,
            edits: crate::editor::text::TextEditSet::new(revision, vec![
                crate::editor::text::TextEdit::new(ByteRange::empty(ByteOffset(5)), ";"),
                crate::editor::text::TextEdit::new(ByteRange::empty(ByteOffset(11)), ";"),
            ]),
        });
    app.state.apply_transaction(&transaction).unwrap();
    app.restore_view_positions(positions);
    assert_eq!(app.state.buffers[buffer].text.to_string(), "foo();\nbar();\n");
    assert_eq!(app.state.views[view].cursor, ByteOffset(7));
}

#[test]
fn obsolete_formatting_failures_do_not_finish_a_newer_save() {
    let (mut app, buffer, ..) = app_waiting_to_save_formatted("newer-save.txt");
    app.format_on_save.as_mut().unwrap().request = Some(("formatter".into(), 2));
    app.handle_lsp_request_failure(
        "formatter".into(),
        1,
        "textDocument/formatting".into(),
        "old failure".into(),
    );
    assert!(app.format_on_save.is_some());
    assert!(app.state.buffers[buffer].dirty);
}

#[test]
fn formatting_save_finishes_its_original_buffer_after_focus_changes() {
    let (mut app, buffer, path, ..) = app_waiting_to_save_formatted("save-in-background.txt");
    app.state.open_text(TextBuffer::from_text("other"), None);
    let other = app.state.active_buffer_id();
    app.finish_format_on_save();
    super::wait_vfs(&mut app);
    assert!(!app.state.buffers[buffer].dirty);
    assert_eq!(fs::read_to_string(path).unwrap(), "a  b\n");
    assert_eq!(app.state.active_buffer_id(), other);
}

#[test]
fn delayed_lsp_navigation_loads_without_stealing_focus() {
    use crate::language::lsp::{LspLocation, LspPosition, LspRange, file_uri};
    let mut app = app();
    app.state.open_text(TextBuffer::from_text("first"), None);
    let path = app.state.workspace_root.join("definition.txt");
    fs::write(&path, "definition").unwrap();
    app.navigate_lsp_location_encoded(
        &LspLocation {
            uri: file_uri(&path),
            range: LspRange {
                start: LspPosition {
                    line: 0,
                    character: 0,
                },
                end: LspPosition {
                    line: 0,
                    character: 0,
                },
            },
        },
        crate::editor::text::PositionEncoding::Utf16,
    );
    let (_, view) = app.state.open_text(TextBuffer::from_text("second"), None);
    super::wait_vfs(&mut app);
    assert_eq!(app.state.active_view, Some(view));
    assert!(
        app.state
            .buffers
            .values()
            .any(|buffer| buffer.path.as_ref() == Some(&path))
    );
}

#[test]
fn lsp_selection_expands_parents_and_preserves_exclusive_range_ends() {
    let mut app = app();
    let (buffer, view) = app.state.open_text(TextBuffer::from_text("abc def;"), None);
    app.set_dashboard_visibility(false);
    app.state.views[view].cursor = ByteOffset(5);
    app.language.install_selection_ranges(vec![
        ByteRange::new(ByteOffset(4), ByteOffset(7)).unwrap(),
        ByteRange::new(ByteOffset(0), ByteOffset(7)).unwrap(),
    ]);
    app.process_effect(AppEffect::LspSelectionExpand);
    assert_eq!(app.state.views[view].selection.unwrap().head, ByteOffset(6));
    app.process_effect(AppEffect::LspSelectionExpand);
    assert_eq!(
        app.state.views[view].selection.unwrap().anchor,
        ByteOffset(0)
    );
    app.handle_configured_key(KeyEvent::character('d'));
    assert_eq!(app.state.buffers[buffer].text.to_string(), ";");
}

#[test]
fn location_picker_keeps_utf8_positions_after_server_capabilities_disappear() {
    let mut app = app();
    let path = app.state.workspace_root.join("encoded.rs");
    let (_, view) = app
        .state
        .open_text(TextBuffer::from_text("é def"), Some(path.clone()));
    app.set_dashboard_visibility(false);
    app.open_lsp_locations_picker(
        vec![crate::language::lsp::LspLocation {
            uri: crate::language::lsp::file_uri(&path),
            range: crate::language::lsp::LspRange {
                start: crate::language::lsp::LspPosition {
                    line: 0,
                    character: 3,
                },
                end: crate::language::lsp::LspPosition {
                    line: 0,
                    character: 6,
                },
            },
        }],
        crate::editor::text::PositionEncoding::Utf8,
    );
    app.accept_picker();
    assert_eq!(app.state.views[view].cursor, ByteOffset(3));
}

#[test]
fn an_unrelated_formatter_cannot_release_a_pending_save() {
    let (mut app, pending_buffer, path, _) = app_waiting_to_save_formatted("pending.rs");
    app.format_on_save.as_mut().unwrap().formatter = Some("wanted".into());
    let (other, _) = app.state.open_text(TextBuffer::from_text("other"), None);
    app.tasks.fixture_formatter_result(
        "unrelated",
        other,
        b"other formatted",
        &app.state,
        &app.workspace,
        app.configuration.binding_revision(),
    );
    app.process_job_events();
    assert!(app.format_on_save.is_some());
    assert_eq!(fs::read_to_string(&path).unwrap(), "a  b\n");
    app.tasks.fixture_formatter_result(
        "wanted",
        pending_buffer,
        b"a b\n",
        &app.state,
        &app.workspace,
        app.configuration.binding_revision(),
    );
    app.process_job_events();
    wait_vfs(&mut app);
    assert!(app.format_on_save.is_none());
    assert_eq!(fs::read_to_string(path).unwrap(), "a b\n");
}

#[test]
fn auto_import_completion_keeps_snippet_tabstops_after_shifted_insertion() {
    use crate::language::lsp::{LspCompletionItem, LspPosition, LspRange, LspTextEdit};
    let mut app = app();
    let path = app.state.workspace_root.join("snippet.rs");
    let (buffer, view) = app
        .state
        .open_text(TextBuffer::from_text("\nfoo"), Some(path));
    app.state.views[view].cursor = ByteOffset(4);
    app.set_dashboard_visibility(false);
    let document = app.active_lsp_document().unwrap().1;
    app.language.fixture_completion_source(Some((
        1,
        super::LspProjectionOwner {
            server: "fixture".into(),
            buffer,
            view,
            revision: app.state.buffers[buffer].text.revision(),
            text: app.state.buffers[buffer].text.source_version(),
            uri: document.uri(),
            language: document.language_id,
        },
    )));
    app.language
        .fixture_completion_items(vec![LspCompletionItem {
            label: "foo".into(),
            filter_text: None,
            sort_text: None,
            detail: String::new(),
            insert_text: String::new(),
            text_edit: Some(LspTextEdit {
                range: LspRange {
                    start: LspPosition {
                        line: 1,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 1,
                        character: 3,
                    },
                },
                new_text: "foo(${1:arg})$0".into(),
            }),
            additional_edits: vec![LspTextEdit {
                range: LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 0,
                    },
                },
                new_text: "use thing;\n".into(),
            }],
            kind: None,
            snippet: true,
            documentation: None,
            resolve_item: None,
            resolved: true,
        }]);
    app.ui.language_completion = Some(crate::ui::LanguageCompletionState {
        typed: 0,
        items: vec![crate::ui::CompletionItem {
            icon: None,
            color: None,
            label: "foo".into(),
            detail: String::new(),
        }],
        selected: 0,
        scroll: 0,
    });
    app.accept_lsp_completion();
    let text = app.state.buffers[buffer].text.to_string();
    assert_eq!(text, "use thing;\n\nfoo(arg)");
    assert_eq!(app.input.snippet_buffer(), Some(buffer));
    assert_eq!(
        app.state.views[view].selection.unwrap().anchor.get(),
        text.find("arg").unwrap()
    );
    app.navigate_snippet(1);
    assert!(app.state.views[view].selection.is_none());
}
