//! Compiler checks against the actual owner implementation files, not mock structs.
//! Fast owner-file probes plus an explicitly invoked complete-source compiler check.
//! Neither proves the remaining canonical service dependency/alias contracts.
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn compiled_library(dependencies: &Path) -> PathBuf {
    fs::read_dir(dependencies)
        .expect("Cargo dependency artifacts")
        .map(|entry| entry.expect("artifact"))
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("libdust-") && name.ends_with(".rlib")
        })
        .max_by_key(|entry| {
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
        })
        .expect("built Dust library")
        .path()
}

fn compile_probe(directory: &Path, name: &str, code: &str) -> (bool, String) {
    let dependencies = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let library = compiled_library(&dependencies);
    let source = directory.join(format!("{name}.rs"));
    let diagnostics = directory.join(format!("{name}.stderr"));
    fs::write(&source, code).unwrap();
    let mut child = Command::new("rustc")
        .args([
            "--edition=2024",
            "--crate-type=lib",
            "--emit=metadata",
            "--error-format=json",
        ])
        .arg(&source)
        .arg("--extern")
        .arg(format!("dust={}", library.display()))
        .arg("-L")
        .arg(format!("dependency={}", dependencies.display()))
        .arg("-o")
        .arg(directory.join(format!("{name}.rmeta")))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(File::create(&diagnostics).unwrap())
        .spawn()
        .expect("installed rustc; no toolchain installation is permitted");
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("compiler privacy probe timed out: {name}");
        }
        thread::sleep(Duration::from_millis(10));
    };
    (status.success(), fs::read_to_string(diagnostics).unwrap())
}

#[test]
fn real_feature_fields_are_private_but_domain_operations_and_public_editor_api_compile() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture = tempfile::tempdir().unwrap();
    let source = |body: &str| {
        format!(
            r#"
#![allow(dead_code, unused_imports)]
use dust::{{config, editor, language, telemetry}};
mod session {{
    #[path = {notice:?}] mod notifications;
    #[path = {syntax:?}] mod syntax;
    mod dispatch {{ {body} }}
}}
// Positive control: public EditorState editing remains a legitimate client API.
fn public_client(state: &mut editor::EditorState) {{ state.workspace_root.clear(); }}
"#,
            notice = root.join("src/editor/session/notifications.rs"),
            syntax = root.join("src/editor/session/syntax.rs"),
        )
    };
    let positive = source(
        r#"
fn domain_operations(notices: &mut super::notifications::NoticeSession, syntax: &super::syntax::SyntaxSession) {
    notices.notify("message".into(), false, std::time::Instant::now(), std::time::Duration::ZERO);
    notices.clear();
    let _ = syntax.results().count();
}
"#,
    );
    let (success, diagnostic) = compile_probe(fixture.path(), "positive", &positive);
    assert!(
        success,
        "positive control did not compile; privacy is NOT proved: {diagnostic}"
    );
    for (name, body, field) in [
        (
            "notice_mutation",
            "fn bypass(owner: &mut super::notifications::NoticeSession) { owner.deadline = None; }",
            "deadline",
        ),
        (
            "syntax_mutation",
            "fn bypass(owner: &mut super::syntax::SyntaxSession) { owner.accepted.clear(); }",
            "accepted",
        ),
        (
            "aliased_owner",
            "use super::notifications::NoticeSession as Alias; fn bypass(owner: &mut Alias) { owner.current = None; }",
            "current",
        ),
    ] {
        let (success, diagnostic) = compile_probe(fixture.path(), name, &source(body));
        assert!(!success, "private state became writable: {name}");
        assert!(
            diagnostic.contains("E0616")
                && diagnostic.contains(field)
                && diagnostic.contains("private"),
            "wrong failure in {name}: {diagnostic}"
        );
    }
}

#[cfg(unix)]
fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            assert!(
                kind.is_file(),
                "fixture does not follow source symlinks: {:?}",
                entry.path()
            );
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[cfg(unix)]
fn check_complete_tree(
    tree: &Path,
    target: &Path,
    name: &str,
    deadline: Instant,
) -> (bool, Vec<serde_json::Value>, String) {
    use std::os::unix::process::CommandExt;
    let stdout = tree.join(format!("{name}.jsonl"));
    let stderr = tree.join(format!("{name}.stderr"));
    let mut child = Command::new("cargo")
        .args([
            "check",
            "--locked",
            "--offline",
            "--lib",
            "--message-format=json",
            "--jobs",
            "2",
        ])
        .arg("--manifest-path")
        .arg(tree.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(target)
        .env_remove("CARGO_MAKEFLAGS")
        .current_dir(tree)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(File::create(&stdout).unwrap())
        .stderr(File::create(&stderr).unwrap())
        .spawn()
        .expect("installed Cargo and already cached locked dependencies");
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap());
            let _ = nix::sys::signal::killpg(pid, nix::sys::signal::Signal::SIGKILL);
            let _ = child.wait();
            panic!("copied-tree compiler deadline reached in {name}");
        }
        thread::sleep(Duration::from_millis(20));
    };
    let output = fs::read_to_string(stdout).unwrap();
    let messages = output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| {
            value["reason"] == "compiler-message" && value["message"]["level"] == "error"
        })
        .map(|value| value["message"].clone())
        .collect();
    (
        status.success(),
        messages,
        format!("{output}\n{}", fs::read_to_string(stderr).unwrap()),
    )
}

#[cfg(unix)]
#[test]
#[ignore = "complete-source compilation; run explicitly for ownership qualification"]
fn copied_production_tree_enforces_migrated_owner_privacy() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let fixture = tempfile::tempdir().unwrap();
    let tree = fixture.path();
    copy_tree(&root.join("src"), &tree.join("src"));
    copy_tree(&root.join("assets"), &tree.join("assets"));
    copy_tree(&root.join("wit"), &tree.join("wit"));
    // The public architecture audit also checks the external-TUI contract fixture.
    fs::create_dir_all(tree.join("tests")).unwrap();
    fs::copy(
        root.join("tests/external_tui.rs"),
        tree.join("tests/external_tui.rs"),
    )
    .unwrap();
    for name in ["Cargo.toml", "Cargo.lock"] {
        fs::copy(root.join(name), tree.join(name)).unwrap();
    }
    // Separate Cargo lock/cache: never recurse into the outer test's build lock.
    let target = root.join("target/ownership-privacy");
    let deadline = Instant::now() + Duration::from_secs(600);
    let module = tree.join("src/editor/session/mod.rs");
    let original = fs::read_to_string(&module).unwrap();
    let dispatch = tree.join("src/editor/session/dispatch/mod.rs");
    let original_dispatch = fs::read_to_string(&dispatch).unwrap();
    let install = |body: &str| {
        fs::write(&module, &original).unwrap();
        fs::write(&dispatch, &original_dispatch).unwrap();
        if body.contains("ExecutionAuthorization") {
            fs::write(&dispatch,format!("{original_dispatch}\n#[allow(dead_code)] mod ownership_probe {{ use super::{{ExecutionAuthorization,ExecutionAdmission}}; {body} }}\n")).unwrap();
        } else {
            fs::write(
                &module,
                format!("{original}\n#[allow(dead_code)] mod ownership_probe {{ {body} }}\n"),
            )
            .unwrap();
        }
    };
    let authorization_positive = "fn domain(auth:ExecutionAuthorization,workspace:&crate::editor::session::workspace::WorkspaceSession) { let _=auth.consume(workspace,ExecutionAdmission::Lsp(String::new())); }";
    install(authorization_positive);
    let (success, errors, diagnostic) =
        check_complete_tree(tree, &target, "authorization_positive_before", deadline);
    assert!(
        success && errors.is_empty(),
        "authorization scope positive failed: {diagnostic}"
    );
    let positive = r#"
fn terminal_domain(owner:&mut super::terminal::TerminalSession) {let _=owner.selected();let _=owner.descriptions();}
fn lsp_domain(owner:&super::lsp::LspSession) {let _=owner.sessions();let _=owner.capabilities();}
fn extension_domain(owner:&super::extensions::ExtensionSession) {let _=owner.statuses();let _=owner.load_id("demo");let _=owner.provider_presentable(1);}
fn virtual_publication_domain(owner:&mut super::extensions::ExtensionSession,editor:&crate::editor::EditorState,windows:&super::windows::WindowSession) {drop(owner.prepare_health(false,0,super::extensions::VirtualContext{editor,windows,scratch:"scratch"}));}
fn virtual_request_domain(broker:&mut crate::extensions::PluginVirtualDocumentBroker) {drop(broker.request_lane());}
fn recovery_domain(owner:&super::recovery::RecoverySession) {let _=owner.records();}
fn tooling_domain(owner:&super::tooling::ToolingSession) {let _=owner.entries();let _=owner.metadata();}
fn recovery_io_domain(owner:&mut super::documents::DocumentSession) {drop(owner.recovery_io());}
fn git_domain(owner:&super::git::GitSession) {let _=owner.status();let _=owner.hunks();let _=owner.decorations();let _=owner.has_selection();}
fn debugger_domain(owner:&mut super::debugger::DebugSession) {let _=owner.is_running();let _=owner.persisted_breakpoints();let _=owner.reply_failures();}
fn debugger_reply_domain(reply:&crate::debugger::DapTerminalReply) {let _=reply.outcome();let _=reply.receipt().outcome();}
fn debugger_launch_domain(workspace:&mut super::workspace::WorkspaceSession) {drop(workspace.execution_lane(super::workspace::DiscoveryKind::Debugger));}
fn debugger_process_domain(service:&mut crate::debugger::DapService) {if let Ok(admission)=service.admission() {drop(admission);}}
fn task_domain(owner:&mut super::tasks::TaskSession) { let _=owner.running_count(); let _=owner.linter_count(); owner.select_output(None); }
fn diagnostic_domain(owner:&mut super::diagnostics::DiagnosticSession) { owner.set_limits(10,10); let _=owner.choices().count(); }
fn plugin_source_domain(owner:&mut super::plugin_edits::PluginEditSources) { owner.close(); }
fn edit_source_domain(owner:&mut super::lsp_edits::LspEditSources) { owner.clear_actions(); let _=owner.pending_replies(); }
fn workspace_edit_domain(owner:&mut super::workspace_edits::WorkspaceEditSession) { let _=owner.proposal(); let _=owner.toggle(0); owner.reject(); }
fn search_domain(owner:&mut super::search::SearchSession) {let _=owner.matches();owner.cancel();}
fn explorer_domain(owner:&mut super::explorer::ExplorerSession) {let _=owner.tree().selected_id();owner.navigate(1);}

fn domains(config: &super::configuration::ConfigurationSession, input: &mut super::input::InputSession) {
    let _ = config.settings().input.count_limit;
    input.cancel_mapping();
    input.prompt_insert("query");
    let _ = input.prompt();
    let _ = input.documentation();
}
fn window_domain(windows: &mut super::windows::WindowSession, editor: &crate::editor::EditorState) {
    windows.invalidate();
    let _ = windows.layout();
    let _ = windows.current(Some(1), (80, 24));
    let _ = windows.content();
    let _ = windows.graph().revision();
    let _ = windows.editor_ids().buffer_for_content(crate::ui::window::ContentId(1));
    let _ = windows.native_ids().explorer();
    let draft = windows.begin_plugin("demo", 1, (&editor.window_version(), &crate::extensions::PluginCatalogVersion::default())).unwrap();
    let _ = draft.read().revision();
}
fn contribution_domain(config: &mut super::configuration::ConfigurationSession) {
    let catalog = crate::extensions::PluginContributionCatalog::default();
    let candidate = config.prepare_builtin_rules(&catalog);
    let _ = candidate.windows();
    let _ = config.check_contributions(&candidate, &catalog);
    let _ = config.commit_contributions(candidate, &catalog);
}
fn legitimate_client(editor: &mut crate::editor::EditorState) { editor.workspace_root.clear(); }
"#;
    install(positive);
    let (success, errors, diagnostic) = check_complete_tree(tree, &target, "positive", deadline);
    assert!(
        success && errors.is_empty(),
        "full-source positive control failed; privacy is NOT proved: {diagnostic}"
    );
    for (name, body, field, code) in [
        (
            "accepted",
            "fn bypass(owner: &mut super::configuration::ConfigurationSession) { owner.accepted.settings.lsp.inlay_hints = false; }",
            "accepted",
            "E0616",
        ),
        (
            "immutable_policy",
            "fn bypass(owner: &mut super::configuration::ConfigurationSession) { owner.settings().lsp.inlay_hints = false; }",
            "cannot assign",
            "E0594",
        ),
        (
            "sequence",
            "fn bypass(owner: &mut super::input::InputSession) { owner.pending = None; }",
            "pending",
            "E0616",
        ),
        (
            "aliased_prompt",
            "use super::input::InputSession as Alias; fn bypass(owner: &mut Alias) { owner.prompt = None; }",
            "prompt",
            "E0616",
        ),
        (
            "documentation",
            "fn bypass(owner: &mut super::input::InputSession) { owner.documentation = None; }",
            "documentation",
            "E0616",
        ),
        (
            "window_layout",
            "fn bypass(owner: &mut super::windows::WindowSession) { owner.layout = None; }",
            "layout",
            "E0616",
        ),
        (
            "aliased_window_content",
            "use super::windows::WindowSession as Alias; fn bypass(owner: &mut Alias) { owner.content.picker = None; }",
            "content",
            "E0616",
        ),
        (
            "configuration_candidate_stamp",
            "fn bypass(candidate: &mut super::configuration::ConfigurationCandidate) { candidate.stamp = std::sync::Arc::new(()); }",
            "stamp",
            "E0616",
        ),
        (
            "contribution_policy",
            "fn bypass(candidate: &mut super::configuration::ContributionCandidate) { candidate.windows = Default::default(); }",
            "windows",
            "E0616",
        ),
        (
            "retired_binding_installer",
            "fn bypass(config: &mut super::configuration::ConfigurationSession, keymaps: super::configuration::ResolvedKeymaps) { config.install_bindings(keymaps, false); }",
            "install_bindings",
            "E0599",
        ),
        (
            "window_core",
            "fn bypass(owner: &mut super::windows::WindowSession) { owner.core.graph.focus(crate::ui::window::WindowId(1)).unwrap(); }",
            "core",
            "E0616",
        ),
        (
            "immutable_live_graph",
            "fn bypass(owner: &mut super::windows::WindowSession) { let _ = owner.graph().focus(crate::ui::window::WindowId(1)); }",
            "cannot borrow",
            "E0596",
        ),
        (
            "editor_bridge_writer",
            "fn bypass(owner: &mut super::windows::WindowSession) { let _ = owner.editor_ids().reset(&mut crate::ui::window::WindowGraph::default()); }",
            "reset",
            "E0624",
        ),
        (
            "native_bridge_writer",
            "fn bypass(owner: &mut super::windows::WindowSession) { let _ = owner.native_ids().set_configured_placements(Default::default()); }",
            "set_configured_placements",
            "E0624",
        ),
        (
            "window_candidate",
            "fn bypass(candidate: &mut super::windows::WindowCandidate) { candidate.core.graph = crate::ui::window::WindowGraph::default(); }",
            "core",
            "E0616",
        ),
        (
            "plugin_draft_graph",
            "fn bypass(draft: &mut super::windows::PluginWindowDraft) { draft.graph = crate::ui::window::WindowGraph::default(); }",
            "graph",
            "E0616",
        ),
        (
            "plugin_draft_scope",
            "fn bypass(draft: &mut super::windows::PluginWindowDraft) { draft.generation = 42; }",
            "generation",
            "E0616",
        ),
        (
            "replay_budget",
            "fn bypass(owner: &mut super::input::InputSession) { owner.replay_steps = 0; }",
            "replay_steps",
            "E0616",
        ),
        (
            "repeat_record",
            "fn bypass(owner: &mut super::input::InputSession) { owner.last_change.clear(); }",
            "last_change",
            "E0616",
        ),
        (
            "editor_source_view",
            "fn bypass(source: &mut crate::editor::EditorTopology<'_>, other: &crate::editor::EditorState) { source.buffers = &other.buffers; }",
            "buffers",
            "E0616",
        ),
        (
            "window_focus",
            "fn bypass(windows: &mut super::windows::WindowSession) { windows.focus = Default::default(); }",
            "focus",
            "E0616",
        ),
        (
            "window_content_order",
            "fn bypass(windows: &mut super::windows::WindowSession) { windows.content_line = Default::default(); }",
            "content_line",
            "E0616",
        ),
        (
            "window_requested_height",
            "fn bypass(windows: &mut super::windows::WindowSession) { windows.terminal_height = 1; }",
            "terminal_height",
            "E0616",
        ),
        (
            "input_capture",
            "fn bypass(input: &mut super::input::InputSession) { input.capture = None; }",
            "capture",
            "E0616",
        ),
        (
            "window_plan_operations",
            "fn bypass(plan: &mut super::windows::WindowPlan) { plan.operations.clear(); }",
            "operations",
            "E0616",
        ),
        (
            "editor_transition_source",
            "fn bypass(plan: &mut super::windows::EditorWindowCandidate, proposal: crate::editor::EditorDraft) { plan.proposal = proposal; }",
            "proposal",
            "E0616",
        ),
        (
            "editor_proposal_stores",
            "fn bypass(proposal: &mut crate::editor::EditorDraft, state: crate::editor::EditorState) { proposal.state = state; }",
            "state",
            "E0616",
        ),
        (
            "editor_proposal_origin",
            "fn bypass(proposal: &mut crate::editor::EditorDraft, version: crate::editor::EditorVersion) { proposal.before = version; }",
            "before",
            "E0616",
        ),
        (
            "consumed_editor_draft",
            "fn bypass(state: &crate::editor::EditorState, tx: &crate::editor::EditorTransaction) { let draft = state.draft(); let _ = tx; let _ = draft.accept(&state.version()); let _ = draft.accept(&state.version()); }",
            "draft",
            "E0382",
        ),
        (
            "workspace_roots",
            "fn bypass(owner: &mut super::workspace::WorkspaceSession) { owner.state.set_trust(crate::workspace::WorkspaceTrust::Trusted); }",
            "state",
            "E0616",
        ),
        (
            "workspace_trust_store",
            "fn bypass(owner: &mut super::workspace::WorkspaceSession) { let _ = &mut owner.store; }",
            "store",
            "E0616",
        ),
        (
            "workspace_confirmation",
            "fn bypass(owner: &mut super::workspace::WorkspaceSession) { owner.confirmation = None; }",
            "confirmation",
            "E0616",
        ),
        (
            "workspace_permit_source",
            "fn bypass(permit: &mut super::workspace::ExecutionPermit, source: super::workspace::WorkspaceVersion) { permit.source=source; }",
            "source",
            "E0616",
        ),
        (
            "workspace_permit_consumed",
            "fn bypass(owner:&super::workspace::WorkspaceSession,permit:super::workspace::ExecutionPermit,id:super::workspace::ValidationId) { owner.consume_permit(permit,id); owner.consume_permit(permit,id); }",
            "permit",
            "E0382",
        ),
        (
            "document_file_intents",
            "fn bypass(owner:&mut super::documents::DocumentSession) { owner.pending_files.clear(); }",
            "pending_files",
            "E0616",
        ),
        (
            "document_file_reply",
            "fn bypass(reply:&mut super::documents::FileReply) { let _ = &mut reply.intent; }",
            "intent",
            "E0616",
        ),
        (
            "text_untracked_rope",
            "fn bypass(text:&mut crate::editor::text::TextBuffer) { text.rope.remove(..); }",
            "rope",
            "E0616",
        ),
        (
            "workspace_discovery",
            "fn bypass(owner:&mut super::workspace::WorkspaceSession) { let _ = &mut owner.language_roots; }",
            "language_roots",
            "E0616",
        ),
        (
            "workspace_preflight",
            "fn bypass(owner:&mut super::workspace::WorkspaceSession) { let _ = &mut owner.preflights; }",
            "preflights",
            "E0616",
        ),
        (
            "persistence_worker",
            "fn bypass(owner:&mut super::persistence::PersistenceSession) { let _ = &mut owner.worker; }",
            "worker",
            "E0616",
        ),
        (
            "persistence_terminal",
            "fn bypass(owner:&mut super::persistence::PersistenceSession) { owner.saved_on_exit = true; }",
            "saved_on_exit",
            "E0616",
        ),
        (
            "authorization_binding",
            "fn bypass(auth:&mut ExecutionAuthorization) { auth.admission=ExecutionAdmission::Lsp(String::new()); }",
            "admission",
            "E0616",
        ),
        (
            "authorization_consumed",
            "fn bypass(auth:ExecutionAuthorization,workspace:&crate::editor::session::workspace::WorkspaceSession) { auth.consume(workspace,ExecutionAdmission::Lsp(String::new())); auth.consume(workspace,ExecutionAdmission::Lsp(String::new())); }",
            "auth",
            "E0382",
        ),
        (
            "navigation_source",
            "fn bypass(stamp:&mut super::windows::NavigationStamp) { stamp.view=None; }",
            "view",
            "E0616",
        ),
        (
            "native_command_plan",
            "fn bypass(plan:&mut super::windows::WindowPlan) { plan.native=None; }",
            "native",
            "E0616",
        ),
        (
            "workspace_preparation",
            "fn bypass(owner:&mut super::workspace_edits::WorkspaceEditSession) { owner.preparing=None; }",
            "preparing",
            "E0616",
        ),
        (
            "workspace_io_origin",
            "fn bypass(reply:&mut super::documents::WorkspaceIoReply) { reply.worker=std::sync::Arc::new(()); }",
            "worker",
            "E0616",
        ),
        (
            "debugger_launch",
            "fn bypass(owner:&mut super::debugger::DebugSession) {owner.launch=None;}",
            "launch",
            "E0616",
        ),
        (
            "debugger_intent",
            "fn bypass(intent:&mut super::debugger::DebugIntent) {let _=&mut intent.nonce;}",
            "nonce",
            "E0616",
        ),
        (
            "debugger_permit",
            "fn bypass(permit:&mut super::workspace::DebugPermit) {let _=&mut permit.nonce;}",
            "nonce",
            "E0616",
        ),
        (
            "debugger_service_loan",
            "fn bypass(service:&mut crate::debugger::DapService) {let loan=service.admission().unwrap();let _=service.stop();drop(loan);}",
            "service",
            "E0499",
        ),
        (
            "workspace_execution_lane",
            "fn bypass(workspace:&mut super::workspace::WorkspaceSession) {let lane=workspace.execution_lane(super::workspace::DiscoveryKind::Debugger);let _=workspace.begin_validation();drop(lane);}",
            "workspace",
            "E0499",
        ),
        (
            "debugger_runtime",
            "fn bypass(owner:&mut super::debugger::DebugSession) {let _=&mut owner.service;}",
            "service",
            "E0616",
        ),
        (
            "debugger_choices",
            "fn bypass(owner:&mut super::debugger::DebugSession) {owner.choices=None;}",
            "choices",
            "E0616",
        ),
        (
            "debugger_breakpoints",
            "fn bypass(owner:&mut super::debugger::DebugSession) {owner.breakpoints.clear();}",
            "breakpoints",
            "E0616",
        ),
        (
            "debugger_selection",
            "fn bypass(selection:&mut super::debugger::DebugSelection) {let _=&mut selection.source;}",
            "source",
            "E0616",
        ),
        (
            "lsp_projection_state",
            "fn bypass(owner:&mut super::lsp::LspSession) {let _=&mut owner.projection;}",
            "projection",
            "E0616",
        ),
        (
            "extension_contributions",
            "fn bypass(owner:&mut super::extensions::ExtensionSession) {let _=&mut owner.plugin_contributions;}",
            "plugin_contributions",
            "E0616",
        ),
        (
            "extension_package_commits",
            "fn bypass(owner:&mut super::extensions::ExtensionSession) {owner.package_commits.clear();}",
            "package_commits",
            "E0616",
        ),
        (
            "extension_registry",
            "fn bypass(owner:&mut super::extensions::ExtensionSession) {let _=&mut owner.registry;}",
            "registry",
            "E0616",
        ),
        (
            "lsp_transport",
            "fn bypass(owner:&mut super::lsp::LspSession) {let _=&mut owner.service;}",
            "service",
            "E0616",
        ),
        (
            "lsp_edit_ledger",
            "fn bypass(owner:&mut super::lsp::LspSession) {let _=&mut owner.edits;}",
            "edits",
            "E0616",
        ),
        (
            "lsp_runtime_registry",
            "fn bypass(owner:&mut super::lsp::LspSession) {owner.sessions.clear();}",
            "sessions",
            "E0616",
        ),
        (
            "lsp_active_runtime",
            "fn bypass(owner:&mut super::lsp::LspSession) {owner.active=None;}",
            "active",
            "E0616",
        ),
        (
            "extension_providers",
            "fn bypass(owner:&mut super::extensions::ExtensionSession) {let _=&mut owner.providers;}",
            "providers",
            "E0616",
        ),
        (
            "extension_virtual_documents",
            "fn bypass(owner:&mut super::extensions::ExtensionSession) {let _=&mut owner.virtual_documents;}",
            "virtual_documents",
            "E0616",
        ),
        (
            "extension_virtual_editor",
            "fn bypass(owner:&mut super::extensions::ExtensionSession) {let _=&mut owner.virtual_editor;}",
            "virtual_editor",
            "E0616",
        ),
        (
            "extension_active_picker",
            "fn bypass(owner:&mut super::extensions::ExtensionSession) {owner.active_picker=None;}",
            "active_picker",
            "E0616",
        ),
        (
            "virtual_publication_loan",
            "fn bypass(owner:&mut super::extensions::ExtensionSession,editor:&crate::editor::EditorState,windows:&super::windows::WindowSession) {let stage=owner.prepare_health(false,0,super::extensions::VirtualContext{editor,windows,scratch:\"scratch\"}).unwrap();owner.reset_runtime();drop(stage);}",
            "owner",
            "E0499",
        ),
        (
            "virtual_publication_authority",
            "fn bypass(source:&mut super::extensions::VirtualPublication) {let _=&mut source.bridge;}",
            "bridge",
            "E0616",
        ),
        (
            "extension_manager",
            "fn bypass(owner:&mut super::extensions::ExtensionSession) {let _=&mut owner.manager;}",
            "manager",
            "E0616",
        ),
        (
            "extension_edits",
            "fn bypass(owner:&mut super::extensions::ExtensionSession) {let _=&mut owner.edits;}",
            "edits",
            "E0616",
        ),
        (
            "extension_delivery",
            "fn bypass(owner:&mut super::extensions::ExtensionSession) {owner.ui.clear();}",
            "ui",
            "E0616",
        ),
        (
            "virtual_request_loan",
            "fn bypass(broker:&mut crate::extensions::PluginVirtualDocumentBroker) {let lane=broker.request_lane();broker.reset_runtime();drop(lane);}",
            "broker",
            "E0499",
        ),
        (
            "virtual_request_authority",
            "fn bypass(lane:&mut crate::extensions::PluginVirtualRequestLane) {let _=&mut lane.broker;}",
            "broker",
            "E0616",
        ),
        (
            "recovery_pending",
            "fn bypass(owner:&mut super::recovery::RecoverySession) {owner.pending.clear();}",
            "pending",
            "E0616",
        ),
        (
            "recovery_revisions",
            "fn bypass(owner:&mut super::recovery::RecoverySession) {owner.revisions.clear();}",
            "revisions",
            "E0616",
        ),
        (
            "recovery_io_loan",
            "fn bypass(owner:&mut super::documents::DocumentSession) {let loan=owner.recovery_io();owner.close_admission();drop(loan);}",
            "owner",
            "E0499",
        ),
        (
            "tooling_installer",
            "fn bypass(owner:&mut super::tooling::ToolingSession) {let _=&mut owner.installer;}",
            "installer",
            "E0616",
        ),
        (
            "tooling_catalog",
            "fn bypass(owner:&mut super::tooling::ToolingSession) {owner.entries.clear();}",
            "entries",
            "E0616",
        ),
        (
            "git_runtime",
            "fn bypass(owner:&mut super::git::GitSession) {owner.status.cancel();}",
            "status",
            "E0616",
        ),
        (
            "git_selection",
            "fn bypass(owner:&mut super::git::GitSession) {owner.selected=None;}",
            "selected",
            "E0616",
        ),
        (
            "debugger_reply_receipts",
            "fn bypass(owner:&mut super::debugger::DebugSession) {owner.reply_receipts.clear();}",
            "reply_receipts",
            "E0616",
        ),
        (
            "debugger_terminal_policy",
            "fn bypass(owner:&mut super::debugger::DebugSession) {owner.terminal_profile=None;}",
            "terminal_profile",
            "E0616",
        ),
        (
            "debugger_reply_receipt",
            "fn bypass(receipt:&mut crate::debugger::DapReplyReceipt) {let _=&mut receipt.outcome;}",
            "outcome",
            "E0616",
        ),
        (
            "debugger_actor_context",
            "fn bypass(context:&mut crate::debugger::DapContext) {let _=&mut context.actor;}",
            "actor",
            "E0616",
        ),
        (
            "terminal_intent",
            "fn bypass(owner:&mut super::terminal::TerminalSession) {owner.intent=None;}",
            "intent",
            "E0616",
        ),
        (
            "dap_terminal_reply",
            "fn bypass(reply:&mut crate::debugger::DapTerminalReply) {let _=&mut reply.inner;}",
            "inner",
            "E0616",
        ),
        (
            "terminal_runtime",
            "fn bypass(owner:&mut super::terminal::TerminalSession) {owner.tabs.clear();}",
            "tabs",
            "E0616",
        ),
        (
            "terminal_backend",
            "fn bypass(owner:&mut super::terminal::TerminalSession) {let _=&mut owner.service;}",
            "service",
            "E0616",
        ),
        (
            "terminal_key",
            "fn bypass(key:&mut super::terminal::TerminalKey) {key.id=crate::process::PtyId(1);}",
            "id",
            "E0616",
        ),
        (
            "terminal_permit",
            "fn bypass(key:&mut super::workspace::TerminalPermit) {key.profile.clear();}",
            "profile",
            "E0616",
        ),
        (
            "pty_admission_loan",
            "fn bypass(owner:&mut crate::process::PtyService,request:crate::process::PtyRequest) {let loan=owner.reserve().unwrap();owner.begin_shutdown();let _=loan.submit(request,None);}",
            "owner",
            "E0499",
        ),
        (
            "linter_builds",
            "fn bypass(owner:&mut super::tasks::TaskSession) { owner.linters.builds.clear(); }",
            "linters",
            "E0616",
        ),
        (
            "linter_submission",
            "fn bypass(value:&mut super::documents::TaskSubmission) { let _=&mut value.result; }",
            "result",
            "E0616",
        ),
        (
            "linter_cancellation",
            "fn bypass(value:&mut super::documents::TaskCancellation) { let _=&mut value.key; }",
            "key",
            "E0616",
        ),
        (
            "linter_reply",
            "fn bypass(value:&mut super::documents::TaskReadReply) { let _=&mut value.result; }",
            "result",
            "E0616",
        ),
        (
            "linter_live_loan",
            "fn bypass(owner:&mut super::tasks::TaskSession,editor:&crate::editor::EditorState,workspace:&super::workspace::WorkspaceSession,config:&u64,diag:&super::diagnostics::DiagnosticSession,documents:&mut super::documents::DocumentSession) { let loan=owner.linter_read(editor,workspace,config,diag).unwrap(); owner.retire_workspace(); let _=documents.submit_linter_read(loan); }",
            "owner",
            "E0499",
        ),
        (
            "linter_live_editor",
            "fn bypass(owner:&mut super::tasks::TaskSession,editor:&mut crate::editor::EditorState,workspace:&super::workspace::WorkspaceSession,config:&u64,diag:&super::diagnostics::DiagnosticSession,documents:&mut super::documents::DocumentSession) { let loan=owner.linter_read(editor,workspace,config,diag).unwrap(); editor.buffers.clear(); let _=documents.submit_linter_read(loan); }",
            "editor",
            "E0502",
        ),
        (
            "task_plan",
            "fn bypass(owner:&mut super::tasks::TaskSession) { owner.plans.clear(); }",
            "plans",
            "E0616",
        ),
        (
            "task_preflight",
            "fn bypass(ready:&mut super::workspace::ExecutionReady) { ready.result.executable.clear(); }",
            "result",
            "E0616",
        ),
        (
            "task_completion_output",
            "fn bypass(task:&mut super::tasks::RunningTask) { task.output.clear(); }",
            "output",
            "E0616",
        ),
        (
            "task_running",
            "fn bypass(owner:&mut super::tasks::TaskSession) { owner.running.clear(); }",
            "running",
            "E0616",
        ),
        (
            "task_completion_adapter",
            "fn bypass(task:&mut super::tasks::RunningTask) { task.adapter=super::tasks::AdapterContext::Plain; }",
            "adapter",
            "E0616",
        ),
        (
            "task_permit",
            "fn bypass(permit:&mut super::workspace::TaskPermit) { permit.name.clear(); }",
            "name",
            "E0616",
        ),
        (
            "job_admission_loan",
            "fn bypass(jobs:&mut crate::process::JobSupervisor,request:crate::process::JobRequest) { let loan=jobs.reserve().unwrap(); let _=jobs.cancel(crate::process::JobId(1)); let _=loan.submit(request); }",
            "jobs",
            "E0499",
        ),
        (
            "diagnostic_store",
            "fn bypass(owner:&mut super::diagnostics::DiagnosticSession) { owner.store.clear_uri(\"file:///x\"); }",
            "store",
            "E0616",
        ),
        (
            "diagnostic_key",
            "fn bypass(key:&mut super::diagnostics::DiagnosticKey) { key.index=0; }",
            "index",
            "E0616",
        ),
        (
            "diagnostic_document",
            "fn bypass(document:&mut super::diagnostics::DiagnosticDocument) { document.path.clear(); }",
            "path",
            "E0616",
        ),
        (
            "workspace_submission",
            "fn bypass(submission:&mut super::documents::WorkspaceSubmission) { submission.result=Ok(crate::files::RequestId(1)); }",
            "result",
            "E0616",
        ),
        (
            "workspace_cancellation",
            "fn bypass(cancelled:&mut super::documents::WorkspaceCancellation) { cancelled.id=crate::files::RequestId(1); }",
            "id",
            "E0616",
        ),
        (
            "workspace_loan_retirement",
            "fn bypass(owner:&mut super::workspace_edits::WorkspaceEditSession, workspace:&super::workspace::WorkspaceSession, editor:&crate::editor::EditorState, documents:&mut super::documents::DocumentSession) { let loan=owner.prepare_io(0,workspace,&1,editor).unwrap(); owner.reject(); let _=documents.submit_workspace_io(loan); }",
            "owner",
            "E0499",
        ),
        (
            "workspace_loan_configuration",
            "fn bypass(owner:&mut super::workspace_edits::WorkspaceEditSession, workspace:&super::workspace::WorkspaceSession, configuration:&mut super::configuration::ConfigurationSession, editor:&crate::editor::EditorState, documents:&mut super::documents::DocumentSession) { let loan=owner.prepare_io(0,workspace,configuration.binding_revision_ref(),editor).unwrap(); let _=configuration.next_result(); let _=documents.submit_workspace_io(loan); }",
            "configuration",
            "E0502",
        ),
        (
            "workspace_loan_scope",
            "fn bypass(owner:&mut super::workspace_edits::WorkspaceEditSession, workspace:&mut super::workspace::WorkspaceSession, editor:&crate::editor::EditorState, documents:&mut super::documents::DocumentSession) { let loan=owner.prepare_io(0,workspace,&1,editor).unwrap(); workspace.cancel_confirmation(); let _=documents.submit_workspace_io(loan); }",
            "workspace",
            "E0502",
        ),
        (
            "workspace_loan_editor",
            "fn bypass(owner:&mut super::workspace_edits::WorkspaceEditSession, workspace:&super::workspace::WorkspaceSession, editor:&mut crate::editor::EditorState, documents:&mut super::documents::DocumentSession) { let loan=owner.prepare_io(0,workspace,&1,editor).unwrap(); editor.workspace_root.clear(); let _=documents.submit_workspace_io(loan); }",
            "editor",
            "E0502",
        ),
        (
            "workspace_io_permit",
            "fn bypass(permit:&mut super::workspace_edits::IoPermit<'_>) { let _=&mut permit.preparation; }",
            "preparation",
            "E0616",
        ),
        (
            "plugin_invocation_loan",
            r#"fn bypass(source:&mut super::plugin_edits::PluginEditSources, manager:&mut crate::extensions::PluginManager, editor:&crate::editor::EditorState, workspace:&super::workspace::WorkspaceSession, request:crate::extensions::PluginExecutionRequest) { let loan=source.prepare(manager,"demo","run",editor,workspace,&1,1024).unwrap(); manager.disable("demo").unwrap(); let _=loan.submit(request); }"#,
            "manager",
            "E0499",
        ),
        (
            "plugin_edit_pending",
            "fn bypass(source:&mut super::plugin_edits::PluginEditSources) { source.pending.clear(); }",
            "pending",
            "E0616",
        ),
        (
            "plugin_edit_preview",
            "fn bypass(source:&mut super::plugin_edits::PluginEditSources) { source.ready=None; }",
            "ready",
            "E0616",
        ),
        (
            "plugin_execution_origin",
            "fn bypass(origin:&mut crate::extensions::PluginInvocationOrigin) { origin.load=crate::extensions::PluginLoadId(1); }",
            "load",
            "E0616",
        ),
        (
            "plugin_execution_reply",
            "fn bypass(reply:&mut crate::extensions::PluginExecutionReply) { reply.origin=None; }",
            "origin",
            "E0616",
        ),
        (
            "lsp_edit_source",
            "fn bypass(owner:&mut super::lsp_edits::LspEditSources) { owner.active=None; }",
            "active",
            "E0616",
        ),
        (
            "lsp_edit_responses",
            "fn bypass(owner:&mut super::lsp_edits::LspEditSources) { owner.replies.clear(); }",
            "replies",
            "E0616",
        ),
        (
            "lsp_action_payloads",
            "fn bypass(owner:&mut super::lsp_edits::LspEditSources) { owner.actions=None; }",
            "actions",
            "E0616",
        ),
        (
            "workspace_edit_preview",
            "fn bypass(owner:&mut super::workspace_edits::WorkspaceEditSession) { owner.preview=None; }",
            "preview",
            "E0616",
        ),
        (
            "workspace_edit_requests",
            "fn bypass(owner:&mut super::workspace_edits::WorkspaceEditSession) { owner.applying.clear(); }",
            "applying",
            "E0616",
        ),
        (
            "search_worker",
            "fn bypass(owner:&mut super::search::SearchSession) { let _=&mut owner.grep; }",
            "grep",
            "E0616",
        ),
        (
            "search_results",
            "fn bypass(owner:&mut super::search::SearchSession) { owner.matches.clear(); }",
            "matches",
            "E0616",
        ),
        (
            "search_preview",
            "fn bypass(owner:&mut super::search::SearchSession) { owner.preview_read=None; }",
            "preview_read",
            "E0616",
        ),
        (
            "explorer_tree",
            "fn bypass(owner:&mut super::explorer::ExplorerSession) { let _=&mut owner.tree; }",
            "tree",
            "E0616",
        ),
        (
            "explorer_worker",
            "fn bypass(owner:&mut super::explorer::ExplorerSession) { let _=&mut owner.directory; }",
            "directory",
            "E0616",
        ),
        (
            "explorer_requests",
            "fn bypass(owner:&mut super::explorer::ExplorerSession) { owner.pending=None; }",
            "pending",
            "E0616",
        ),
        (
            "explorer_watch",
            "fn bypass(owner:&mut super::explorer::ExplorerSession) { let _=&mut owner.watch; }",
            "watch",
            "E0616",
        ),
        (
            "exit_request",
            "fn bypass(owner:&mut super::persistence::PersistenceSession) { owner.exit_request=None; }",
            "exit_request",
            "E0616",
        ),
        (
            "document_worker",
            "fn bypass(owner: &mut super::documents::DocumentSession) { let _ = owner.vfs.read(std::path::PathBuf::new()); }",
            "vfs",
            "E0616",
        ),
        (
            "document_startup",
            "fn bypass(owner: &mut super::documents::DocumentSession) { owner.pending_startup.clear(); }",
            "pending_startup",
            "E0616",
        ),
        (
            "document_policy",
            "fn bypass(owner: &mut super::documents::DocumentSession) { owner.overrides.clear(); }",
            "overrides",
            "E0616",
        ),
        (
            "input_snippet",
            "fn bypass(owner: &mut super::input::InputSession) { owner.snippet = None; }",
            "snippet",
            "E0616",
        ),
        (
            "input_mouse",
            "fn bypass(owner: &mut super::input::InputSession) { let _ = &mut owner.mouse; }",
            "mouse",
            "E0616",
        ),
        (
            "catalog_source",
            "fn bypass(source: &mut crate::extensions::PluginCatalogVersion) { source.0 = std::sync::Arc::new(()); }",
            "0",
            "E0616",
        ),
        (
            "sealed_prompt_reply",
            "fn bypass(key: &mut super::input::PromptCompletionKey) { key.generation = 1; }",
            "generation",
            "E0616",
        ),
    ] {
        install(body);
        let (success, errors, diagnostic) = check_complete_tree(tree, &target, name, deadline);
        assert!(!success, "full-source mutation compiled: {name}");
        assert_eq!(
            errors.len(),
            1,
            "bootstrap/unrelated errors do not prove privacy: {name}: {diagnostic}"
        );
        assert_eq!(
            errors[0]["code"]["code"], code,
            "wrong compiler error: {name}: {diagnostic}"
        );
        assert!(
            errors[0]["message"].as_str().unwrap().contains(field),
            "wrong field: {name}: {diagnostic}"
        );
        eprintln!("copied-tree privacy: {name}: {code} on {field}");
    }
    install(positive);
    dust::architecture::audit_source(tree).expect("valid production dependency control");
    let service = tree.join("src/language/lsp/mod.rs");
    let facade = tree.join("src/editor/mod.rs");
    let original_service = fs::read_to_string(&service).unwrap();
    let original_facade = fs::read_to_string(&facade).unwrap();
    for (name, body, extra) in [
        (
            "service_direct",
            "fn ownership_dependency(s: &mut crate::editor::EditorState) { s.workspace_root.clear(); }",
            "",
        ),
        (
            "service_alias",
            "use crate::editor::EditorState as SharedEditor; fn ownership_dependency(s: &mut SharedEditor) { s.workspace_root.clear(); }",
            "",
        ),
        (
            "service_reexport",
            "fn ownership_dependency(s: &mut crate::editor::OwnershipSharedEditor) { s.workspace_root.clear(); }",
            "pub use self::EditorState as OwnershipSharedEditor;",
        ),
        (
            "service_block_module",
            "fn ownership_dependency() { mod nested { pub fn mutate(s: &mut crate::editor::EditorState) { s.workspace_root.clear(); } } }",
            "",
        ),
    ] {
        fs::write(
            &service,
            format!("{original_service}\n#[allow(dead_code)] {body}\n"),
        )
        .unwrap();
        fs::write(&facade, format!("{original_facade}\n{extra}\n")).unwrap();
        let (success, errors, diagnostic) = check_complete_tree(tree, &target, name, deadline);
        assert!(
            success && errors.is_empty(),
            "dependency negative must be valid Rust: {name}: {diagnostic}"
        );
        let error = dust::architecture::audit_source(tree).expect_err(name);
        assert!(
            matches!(&error, dust::architecture::ArchitectureError::Dependency { path, detail } if path == &service && detail.contains("forbidden authority crate::editor") && detail.contains("EditorState")),
            "unrelated rejection is not dependency evidence: {name}: {error}"
        );
        eprintln!("copied-tree dependency: {name}: valid Rust rejected as live editor authority");
    }
    fs::write(&service, original_service).unwrap();
    fs::write(&facade, original_facade).unwrap();
    dust::architecture::audit_source(tree).expect("restored dependency positive control");
    let (success, errors, diagnostic) =
        check_complete_tree(tree, &target, "positive_after", deadline);
    assert!(
        success && errors.is_empty(),
        "positive control after mutations failed: {diagnostic}"
    );
    install(authorization_positive);
    let (success, errors, diagnostic) =
        check_complete_tree(tree, &target, "authorization_positive_after", deadline);
    assert!(
        success && errors.is_empty(),
        "authorization scope positive after failed: {diagnostic}"
    );
}
