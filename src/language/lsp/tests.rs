use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use super::{
    model::{
        LspDocument, LspEvent, LspFeature, LspFeatureResult, LspMarkupKind, LspPolicy,
        LspProgressKind, SyncKind,
    },
    paths::{file_uri, language_id_for_path, path_from_file_uri},
    protocol::{
        completion_items, hover_result, incremental_change, initialize_capabilities,
        initialize_message, inlay_hints, lsp_progress_event, normalize_result, workspace_edit,
    },
    session::{LspService, requested_executable_name},
};
use crate::{
    editor::text::PositionEncoding,
    tools::{ToolDefinition, ToolKind, tool_launch_environment},
};

#[test]
fn language_and_uri_mapping_are_deterministic() {
    assert_eq!(language_id_for_path(Path::new("src/main.rs")), Some("rust"));
    assert_eq!(
        language_id_for_path(Path::new("ui.tsx")),
        Some("typescriptreact")
    );
    assert_eq!(
        language_id_for_path(Path::new("ui.jsx")),
        Some("javascriptreact")
    );
    assert_eq!(language_id_for_path(Path::new("main.go")), Some("go"));
    for (path, language) in [
        ("script.sh", "shellscript"),
        ("index.html", "html"),
        ("style.scss", "scss"),
        ("data.jsonc", "jsonc"),
        ("workflow.yml", "yaml"),
        ("README.mdx", "markdown.mdx"),
        ("init.lua", "lua"),
        ("index.php", "php"),
        ("App.svelte", "svelte"),
        ("App.vue", "vue"),
        ("index.astro", "astro"),
        ("Dockerfile", "dockerfile"),
    ] {
        assert_eq!(
            language_id_for_path(Path::new(path)),
            Some(language),
            "{path}"
        );
    }
    assert_eq!(language_id_for_path(Path::new("README")), Some("text"));
    let path = Path::new("/tmp/a b#%.rs");
    let uri = file_uri(path);
    assert!(uri.ends_with("/tmp/a%20b%23%25.rs"));
    assert_eq!(path_from_file_uri(&uri).as_deref(), Some(path));
}

#[test]
fn representative_mason_servers_are_discoverable_from_real_paths() {
    let catalog = crate::tools::bundled_tool_catalog();
    for (path, language, server) in [
        ("flake.nix", "nix", "nil_ls"),
        ("main.zig", "zig", "zls"),
        ("taplo.toml", "toml", "taplo"),
        ("Main.java", "java", "jdtls"),
        ("Main.kt", "kotlin", "kotlin_language_server"),
        ("main.rb", "ruby", "ruby_lsp"),
        ("main.ex", "elixir", "elixirls"),
        ("Main.hs", "haskell", "hls"),
        ("main.ml", "ocaml", "ocamllsp"),
        ("main.fs", "fsharp", "fsautocomplete"),
        ("main.clj", "clojure", "clojure_lsp"),
        ("main.jl", "julia", "julials"),
        ("main.R", "r", "r_language_server"),
        ("main.tf", "terraform", "terraformls"),
        ("query.sql", "sql", "sqls"),
        ("schema.graphql", "graphql", "graphql"),
        ("schema.proto", "proto", "buf_ls"),
        ("contract.sol", "solidity", "solidity"),
        ("main.asm", "asm", "asm_lsp"),
        ("paper.tex", "plaintex", "texlab"),
        ("document.xml", "xml", "lemminx"),
        ("CMakeLists.txt", "cmake", "cmake"),
        ("config.fish", "fish", "fish_lsp"),
    ] {
        assert_eq!(
            language_id_for_path(Path::new(path)),
            Some(language),
            "{path}"
        );
        assert!(
            catalog.iter().any(|definition| {
                definition.id == server && definition.supports_language(language)
            }),
            "missing {server} for {path}"
        );
    }
}

#[test]
fn every_managed_catalog_lsp_language_has_a_reachable_document_mapping() {
    let samples = [
        ("rust", "main.rs"),
        ("typescript", "main.ts"),
        ("typescriptreact", "App.tsx"),
        ("javascript", "main.js"),
        ("javascriptreact", "App.jsx"),
        ("python", "main.py"),
        ("c", "main.c"),
        ("cpp", "main.cpp"),
        ("go", "main.go"),
        ("gomod", "go.mod"),
        ("gowork", "go.work"),
        ("gotmpl", "main.gotmpl"),
        ("shellscript", "main.sh"),
        ("html", "index.html"),
        ("css", "style.css"),
        ("scss", "style.scss"),
        ("less", "style.less"),
        ("sass", "style.sass"),
        ("toml", "Cargo.toml"),
        ("json", "data.json"),
        ("jsonc", "data.jsonc"),
        ("yaml", "data.yaml"),
        ("markdown", "README.md"),
        ("markdown.mdx", "README.mdx"),
        ("lua", "init.lua"),
        ("php", "index.php"),
        ("svelte", "App.svelte"),
        ("vue", "App.vue"),
        ("dockerfile", "Dockerfile"),
        ("astro", "index.astro"),
    ]
    .into_iter()
    .collect::<std::collections::BTreeMap<_, _>>();
    let servers = crate::tools::bundled_tool_catalog()
        .into_iter()
        .filter(|tool| tool.kind == ToolKind::Lsp && tool.install.is_some())
        .collect::<Vec<_>>();
    assert_eq!(servers.len(), 24);
    for server in servers {
        assert!(!server.project_root_markers.is_empty(), "{}", server.id);
        for language in &server.languages {
            let sample = samples
                .get(language.as_str())
                .unwrap_or_else(|| panic!("{} has unreachable language {language}", server.id));
            assert_eq!(
                language_id_for_path(Path::new(sample)),
                Some(language.as_str()),
                "{}",
                server.id
            );
        }
    }
}

#[test]
fn a_server_can_register_formatting_after_it_starts() {
    let registered = super::server_message::effective_capabilities(
        &super::LspCapabilities::default(),
        ["textDocument/formatting", "textDocument/rangeFormatting"].into_iter(),
    );
    assert!(registered.formatting && registered.range_formatting);
    assert!(!registered.rename);
}

#[test]
fn work_done_progress_preserves_token_kind_and_bounded_one_line_content() {
    let event = lsp_progress_event(
        "root:1:rust-analyzer",
        &json!({
            "method":"$/progress",
            "params":{
                "token":"cargo-check",
                "value":{
                    "kind":"report",
                    "title":"Building\ncrate",
                    "message":"ring\u{001b}[31m (lib)",
                    "percentage":72
                }
            }
        }),
    )
    .expect("progress event");
    assert!(matches!(
        event,
        LspEvent::Progress {
            server,
            token,
            kind: LspProgressKind::Report,
            title,
            message,
            percentage: Some(72),
        } if server == "root:1:rust-analyzer"
            && token == "cargo-check"
            && title == "Building crate"
            && message == "ring[31m (lib)"
    ));
    assert!(
        lsp_progress_event(
            "server",
            &json!({"params":{"token":{},"value":{"kind":"begin"}}})
        )
        .is_none()
    );
}

#[test]
fn hover_markup_kind_sections_and_range_are_preserved_and_sanitized() {
    let hover = hover_result(&json!({
        "contents":{"kind":"markdown","value":"**type**\u{001b}[31m"},
        "range":{"start":{"line":1,"character":2},"end":{"line":1,"character":6}}
    }));
    assert_eq!(hover.sections.len(), 1);
    assert_eq!(hover.sections[0].kind, LspMarkupKind::Markdown);
    assert_eq!(hover.sections[0].value, "**type**");
    assert_eq!(hover.range.unwrap().start.line, 1);
}

#[test]
fn initialization_advertises_inlay_hints_and_parsing_preserves_inline_padding() {
    let initialize = initialize_message(Path::new("/tmp/project"), None);
    assert_eq!(
        initialize.pointer("/params/capabilities/textDocument/inlayHint/dynamicRegistration"),
        Some(&json!(true))
    );
    let hints = inlay_hints(&json!([{
        "position":{"line":2,"character":7},
        "label":[{"value":":"},{"value":" Result"}],
        "paddingLeft":true,
        "paddingRight":true
    }]));
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].position.line, 2);
    assert_eq!(hints[0].label, " : Result ");
}

#[test]
fn astro_and_vue_initialization_own_the_typescript_sdk_beside_the_server() {
    let directory = tempfile::tempdir().expect("language server prefix");
    let node_modules = directory.path().join("node_modules");
    let executable = node_modules
        .join("@astrojs/language-server/bin")
        .join("astro-ls");
    std::fs::create_dir_all(executable.parent().expect("server bin")).unwrap();
    std::fs::write(&executable, b"server").unwrap();
    let tsdk = node_modules.join("typescript/lib");
    std::fs::create_dir_all(&tsdk).unwrap();
    std::fs::write(tsdk.join("typescript.js"), b"module").unwrap();

    for server in ["astro-language-server", "vue-language-server"] {
        let configuration = super::LspServerConfiguration::for_server(server, &executable);
        let initialize = initialize_message(
            directory.path(),
            configuration.initialization_options.as_ref(),
        );
        assert_eq!(
            initialize
                .pointer("/params/initializationOptions/typescript/tsdk")
                .and_then(Value::as_str),
            std::fs::canonicalize(&tsdk).unwrap().to_str()
        );
    }
}

#[test]
fn maximum_inlay_hint_profiles_are_adapter_specific_and_exhaustive() {
    let rust = super::LspServerConfiguration::maximum_inlay_hints("rust-analyzer");
    let initialize = initialize_message(
        Path::new("/tmp/project"),
        rust.initialization_options.as_ref(),
    );
    assert_eq!(
        initialize
            .pointer("/params/initializationOptions/inlayHints/expressionAdjustmentHints/enable"),
        Some(&json!("always"))
    );
    assert_eq!(
        initialize.pointer("/params/initializationOptions/inlayHints/maxLength"),
        Some(&Value::Null)
    );
    assert_eq!(
        rust.settings
            .pointer("/rust-analyzer/inlayHints/closingBraceHints/minLines"),
        Some(&json!(0))
    );

    let typescript =
        super::LspServerConfiguration::maximum_inlay_hints("typescript-language-server");
    assert_eq!(
        typescript
            .initialization_options
            .as_ref()
            .and_then(|settings| settings.pointer("/preferences/includeInlayParameterNameHints")),
        Some(&json!("all"))
    );
    assert_eq!(
        typescript
            .settings
            .pointer("/typescript/inlayHints/includeInlayFunctionLikeReturnTypeHints"),
        Some(&json!(true))
    );

    let gopls = super::LspServerConfiguration::maximum_inlay_hints("gopls");
    assert_eq!(
        gopls
            .settings
            .pointer("/gopls/hints/compositeLiteralFields"),
        Some(&json!(true))
    );
    assert_eq!(
        super::LspServerConfiguration::maximum_inlay_hints("clangd"),
        super::LspServerConfiguration::standard()
    );
    assert!(matches!(
        normalize_result(
            &LspFeature::InlayHints {
                range: super::LspRange {
                    start: super::LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: super::LspPosition {
                        line: 1,
                        character: 0,
                    },
                },
            },
            &Value::Null,
        ),
        Ok(LspFeatureResult::InlayHints(items)) if items.is_empty()
    ));
}

#[test]
fn initialization_negotiates_utf32_and_incremental_sync() {
    let capabilities = initialize_capabilities(Some(&json!({"capabilities":{
        "positionEncoding":"utf-32","textDocumentSync":{"change":2},
        "hoverProvider":true,"completionProvider":{"triggerCharacters":["."]}
    }})))
    .expect("capabilities");
    assert_eq!(capabilities.position_encoding, PositionEncoding::Utf32);
    assert_eq!(capabilities.sync_kind, SyncKind::Incremental);
    assert!(capabilities.hover && capabilities.completion);
}

#[test]
fn incremental_sync_uses_the_negotiated_unicode_units() {
    let old = LspDocument {
        path: "/tmp/a.rs".into(),
        language_id: "rust".into(),
        version: 1,
        text: "a😀x".into(),
    };
    let new = LspDocument {
        version: 2,
        text: "a😀yz".into(),
        ..old.clone()
    };
    let change = incremental_change(&old, &new, PositionEncoding::Utf16).expect("change");
    assert_eq!(change.pointer("/range/start/character"), Some(&json!(3)));
    assert_eq!(change.pointer("/range/end/character"), Some(&json!(4)));
    assert_eq!(change.get("text"), Some(&json!("yz")));
}

#[test]
fn nested_document_symbols_keep_their_positions_and_children() {
    let range =
        |line: u64| json!({"start":{"line":line,"character":4},"end":{"line":line,"character":7}});
    let found = super::protocol::symbols(&json!([{
        "name":"Person","kind":23,"range":range(1),"selectionRange":range(1),
        "children":[{"name":"Greet","kind":6,"range":range(5),"selectionRange":range(6)}]
    }]))
    .expect("symbols");
    assert_eq!(found[0].name, "Person");
    assert_eq!(found[1].name, "  Greet");
    let location = found[1].location.as_ref().expect("a position");
    assert_eq!(location.uri, "");
    assert_eq!(location.range.start.line, 6);
}

#[test]
fn completion_and_workspace_edits_are_bounded_typed_data() {
    let items =
        completion_items(&json!([{"label":"println","detail":"macro","insertText":"println!"}]))
            .expect("completion");
    assert_eq!(items[0].insert_text, "println!");
    // tsserver lists auto-imports after a thousand globals, so a short list would lose them.
    let many = (0..2_000)
        .map(|index| json!({"label": format!("item{index}")}))
        .collect();
    assert_eq!(
        completion_items(&Value::Array(many)).expect("many").len(),
        2_000
    );
    let edit = workspace_edit(&json!({"changes":{"file:///tmp/a.rs":[{
        "range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"newText":"b"
    }]}}))
    .expect("workspace edit");
    assert_eq!(edit.documents.len(), 1);
    assert_eq!(edit.documents[0].edits[0].new_text, "b");
}

#[test]
fn canonical_launch_preserves_the_requested_multicall_name() {
    let definition = ToolDefinition {
        id: "rust-analyzer".into(),
        name: "Rust Analyzer".into(),
        kind: ToolKind::Lsp,
        languages: vec!["rust".into()],
        executable: "/run/current-system/sw/bin/rust-analyzer".into(),
        version: None,
        release: None,
        launch_args: Vec::new(),
        format_arguments: Vec::new(),
        probe_args: Vec::new(),
        debug_program_policy: crate::tools::DebugProgramPolicy::default(),
        project_root_markers: vec!["Cargo.toml".into()],
        project_root_strategy: crate::files::ProjectRootStrategy::Cargo,
        install: None,
        pins: Vec::new(),
    };
    assert_eq!(requested_executable_name(&definition), "rust-analyzer");
}

#[test]
fn server_that_never_initializes_fails_within_the_request_timeout() {
    let Some(sleep) = crate::tools::discover_executable("sleep") else {
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace");
    let definition = ToolDefinition {
        id: "sleeping-server".into(),
        name: "Sleeping Server".into(),
        kind: ToolKind::Lsp,
        languages: vec!["rust".into()],
        executable: sleep.display().to_string(),
        version: None,
        release: None,
        launch_args: vec!["2".into()],
        format_arguments: Vec::new(),
        probe_args: Vec::new(),
        debug_program_policy: crate::tools::DebugProgramPolicy::default(),
        project_root_markers: vec!["Cargo.toml".into()],
        project_root_strategy: crate::files::ProjectRootStrategy::Cargo,
        install: None,
        pins: Vec::new(),
    };
    let mut service = LspService::new();
    service
        .start_session_absolute_with_policy(super::LspSessionLaunch {
            session_key: &definition.id,
            definition: &definition,
            executable: sleep,
            arguments: definition.launch_args.clone(),
            root: workspace.path().into(),
            document: None,
            policy: LspPolicy {
                request_timeout: Duration::from_millis(100),
                maximum_log_lines: 20,
            },
            configuration: super::LspServerConfiguration::standard(),
            environment: tool_launch_environment(&definition),
        })
        .expect("start");
    let deadline = Instant::now() + Duration::from_secs(3);
    let failed = loop {
        if let Some(LspEvent::Failed { message, .. }) = service.try_event() {
            break message;
        }
        assert!(
            Instant::now() < deadline,
            "initialize timeout event missing"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert!(failed.contains("initialize request timed out"));
    service
        .stop(&definition.id)
        .expect("stop timed-out session");
}

#[cfg(target_os = "linux")]
#[cfg(unix)]
#[test]
fn stopping_does_not_wait_for_a_daemon_that_kept_the_servers_pipes() {
    let Some(shell) = crate::tools::discover_executable("sh") else {
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace");
    let script = workspace.path().join("server.sh");
    std::fs::write(&script, "sleep 6 &\nexec sleep 6\n").expect("script");
    let definition = ToolDefinition {
        id: "daemonizing".into(),
        name: "Daemonizing".into(),
        kind: ToolKind::Lsp,
        languages: vec!["rust".into()],
        executable: shell.display().to_string(),
        version: None,
        release: None,
        format_arguments: Vec::new(),
        launch_args: vec![script.display().to_string()],
        probe_args: Vec::new(),
        debug_program_policy: crate::tools::DebugProgramPolicy::default(),
        project_root_markers: Vec::new(),
        project_root_strategy: crate::files::ProjectRootStrategy::Cargo,
        install: None,
        pins: Vec::new(),
    };
    let mut service = LspService::new();
    service
        .start_session_absolute_with_policy(super::LspSessionLaunch {
            session_key: &definition.id,
            definition: &definition,
            executable: shell.clone(),
            arguments: definition.launch_args.clone(),
            root: workspace.path().into(),
            document: None,
            policy: LspPolicy {
                request_timeout: Duration::from_millis(100),
                maximum_log_lines: 20,
            },
            configuration: super::LspServerConfiguration::standard(),
            environment: tool_launch_environment(&definition),
        })
        .expect("start");
    thread::sleep(Duration::from_millis(200));
    let started = Instant::now();
    service.stop("daemonizing").expect("stop");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn deterministic_fake_server_proves_lifecycle_sync_and_features() {
    let Some(python) = crate::tools::discover_executable("python3") else {
        return;
    };
    let workspace = tempfile::tempdir().expect("workspace");
    let script = workspace.path().join("fake_lsp.py");
    let trace_path = workspace.path().join("trace.jsonl");
    std::fs::write(
            &script,
            r#"
import json, sys
trace = open(sys.argv[1], 'w', buffering=1)
def read():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line: return None
        if line in (b'\r\n', b'\n'): break
        if line.lower().startswith(b'content-length:'): length = int(line.split(b':',1)[1])
    return json.loads(sys.stdin.buffer.read(length))
def send(value):
    data=json.dumps(value,separators=(',',':')).encode()
    sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(data)).encode()+data); sys.stdout.buffer.flush()
while True:
    message=read()
    if message is None: break
    trace.write(json.dumps(message,separators=(',',':'))+'\n')
    method=message.get('method')
    if method=='initialize':
        send({'jsonrpc':'2.0','id':message['id'],'result':{'capabilities':{
          'positionEncoding':'utf-8','textDocumentSync':{'change':2},'hoverProvider':True,
          'completionProvider':{},'definitionProvider':True,'referencesProvider':True,
          'documentFormattingProvider':True,'renameProvider':True,'codeActionProvider':True,
          'inlayHintProvider':True}}})
    elif method=='textDocument/didOpen':
        d=message['params']['textDocument']; send({'jsonrpc':'2.0','method':'textDocument/publishDiagnostics','params':{
          'uri':d['uri'],'version':d['version'],'diagnostics':[{'range':{'start':{'line':0,'character':0},'end':{'line':0,'character':1}},'severity':2,'message':'fake warning'}]}})
        send({'jsonrpc':'2.0','id':99,'method':'workspace/applyEdit','params':{'label':'fake edit','edit':{'changes':{d['uri']:[{
          'range':{'start':{'line':0,'character':0},'end':{'line':0,'character':1}},'newText':'b'}]}}}})
        send({'jsonrpc':'2.0','id':98,'method':'workspace/inlayHint/refresh','params':{}})
        send({'jsonrpc':'2.0','id':97,'method':'workspace/configuration','params':{'items':[{'section':'fake.one'},{'section':'fake.two'}]}})
    elif method=='textDocument/hover':
        send({'jsonrpc':'2.0','id':message['id'],'result':{'contents':{'kind':'markdown','value':'fake hover'}}})
    elif method=='textDocument/completion':
        send({'jsonrpc':'2.0','id':message['id'],'result':[{'label':'fakeItem','insertText':'fakeItem'}]})
    elif method=='textDocument/inlayHint':
        send({'jsonrpc':'2.0','id':message['id'],'result':[{'position':{'line':0,'character':1},'label':': str','paddingRight':True}]})
    elif method=='shutdown': send({'jsonrpc':'2.0','id':message['id'],'result':None})
    elif method=='exit': break
"#,
        )
        .expect("script");
    let definition = ToolDefinition {
        id: "fake".into(),
        name: "Fake".into(),
        kind: ToolKind::Lsp,
        languages: vec!["rust".into()],
        executable: python.display().to_string(),
        version: None,
        release: None,
        format_arguments: Vec::new(),
        launch_args: vec![
            script.display().to_string(),
            trace_path.display().to_string(),
        ],
        probe_args: Vec::new(),
        debug_program_policy: crate::tools::DebugProgramPolicy::default(),
        project_root_markers: vec!["Cargo.toml".into()],
        project_root_strategy: crate::files::ProjectRootStrategy::Cargo,
        install: None,
        pins: Vec::new(),
    };
    let document = LspDocument {
        path: workspace.path().join("main.rs"),
        language_id: "rust".into(),
        version: 1,
        text: "a😀".into(),
    };
    let mut service = LspService::new();
    service
        .start_session_absolute_with_policy(super::LspSessionLaunch {
            session_key: &definition.id,
            definition: &definition,
            executable: python.clone(),
            arguments: definition.launch_args.clone(),
            root: workspace.path().into(),
            document: Some(document.clone()),
            policy: LspPolicy {
                request_timeout: Duration::from_millis(100),
                maximum_log_lines: 20,
            },
            configuration: super::LspServerConfiguration::standard(),
            environment: tool_launch_environment(&definition),
        })
        .expect("start");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut running = false;
    let mut diagnostics_seen = false;
    let mut apply_edit_seen = false;
    let mut inlay_refresh_seen = false;
    while Instant::now() < deadline
        && !(running && diagnostics_seen && apply_edit_seen && inlay_refresh_seen)
    {
        if let Some(event) = service.try_event() {
            match event {
                LspEvent::Running { capabilities, .. } => {
                    assert_eq!(capabilities.position_encoding, PositionEncoding::Utf8);
                    assert_eq!(capabilities.sync_kind, SyncKind::Incremental);
                    running = true;
                }
                LspEvent::Diagnostics { diagnostics, .. } => {
                    assert_eq!(diagnostics[0].message, "fake warning");
                    diagnostics_seen = true;
                }
                LspEvent::InlayHintsRefresh { server } => {
                    assert_eq!(server, "fake");
                    inlay_refresh_seen = true;
                }
                LspEvent::ApplyEditRequest {
                    server,
                    request_id,
                    edit,
                    ..
                } => {
                    assert_eq!(edit.documents.len(), 1);
                    service
                        .respond_apply_edit(
                            &server,
                            request_id,
                            false,
                            Some("test rejection".into()),
                        )
                        .expect("respond");
                    apply_edit_seen = true;
                }
                _ => {}
            }
        } else {
            thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(running && diagnostics_seen && apply_edit_seen && inlay_refresh_seen);
    service
        .request(
            "fake",
            LspFeature::Hover,
            document.clone(),
            crate::editor::text::ByteOffset(5),
            11,
        )
        .expect("hover");
    service
        .request(
            "fake",
            LspFeature::Completion,
            document.clone(),
            crate::editor::text::ByteOffset(5),
            12,
        )
        .expect("completion");
    service
        .request(
            "fake",
            LspFeature::InlayHints {
                range: super::LspRange {
                    start: super::LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: super::LspPosition {
                        line: 0,
                        character: 5,
                    },
                },
            },
            document.clone(),
            crate::editor::text::ByteOffset(0),
            14,
        )
        .expect("inlay hints");
    service
        .sync(
            "fake",
            LspDocument {
                version: 2,
                text: "a😀z".into(),
                ..document.clone()
            },
        )
        .expect("sync");
    let mut hover = false;
    let mut completion = false;
    let mut inlay = false;
    while Instant::now() < deadline && !(hover && completion && inlay) {
        if let Some(LspEvent::Result {
            generation, result, ..
        }) = service.try_event()
        {
            match (generation, result) {
                (11, LspFeatureResult::Hover(result)) => {
                    hover = result
                        .sections
                        .first()
                        .is_some_and(|section| section.value == "fake hover")
                }
                (12, LspFeatureResult::Completion { items, .. }) => {
                    completion = items[0].label == "fakeItem";
                }
                (14, LspFeatureResult::InlayHints(items)) => {
                    inlay = items[0].label == ": str ";
                }
                _ => {}
            }
        } else {
            thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(hover && completion && inlay);
    service
        .request(
            "fake",
            LspFeature::Definition,
            document.clone(),
            crate::editor::text::ByteOffset(5),
            13,
        )
        .expect("definition");
    let timeout_deadline = Instant::now() + Duration::from_secs(2);
    let mut timed_out = false;
    while Instant::now() < timeout_deadline && !timed_out {
        if let Some(LspEvent::RequestFailed {
            generation: 13,
            message,
            ..
        }) = service.try_event()
        {
            timed_out = message.contains("timed out");
        } else {
            thread::sleep(Duration::from_millis(10));
        }
    }
    assert!(timed_out);
    service
        .configure(
            "fake",
            super::LspServerConfiguration::maximum_inlay_hints("rust-analyzer"),
        )
        .expect("configure maximum inlay hints");
    thread::sleep(Duration::from_millis(30));
    service.close("fake", document.uri()).expect("close");
    service.stop("fake").expect("stop");
    let deadline = Instant::now() + Duration::from_secs(2);
    let trace = loop {
        let trace = std::fs::read_to_string(&trace_path).expect("trace");
        if trace.contains("textDocument/didClose") {
            break trace;
        }
        assert!(Instant::now() < deadline, "close was not delivered");
        thread::sleep(Duration::from_millis(5));
    };
    assert!(trace.contains("test rejection"));
    assert!(
        trace
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|message| {
                message.get("id") == Some(&json!(97))
                    && message.get("result") == Some(&json!([{}, {}]))
            })
    );
    assert!(
        trace
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|message| {
                message.get("method") == Some(&json!("workspace/didChangeConfiguration"))
                    && message
                        .pointer("/params/settings/rust-analyzer/inlayHints/implicitDrops/enable")
                        == Some(&json!(true))
            })
    );
    let change = trace
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|message| message.get("method") == Some(&json!("textDocument/didChange")))
        .expect("change");
    assert_eq!(
        change.pointer("/params/textDocument/version"),
        Some(&json!(2)),
        "the unsaved buffer revision must be synchronized before any file save"
    );
    assert_eq!(
        change.pointer("/params/contentChanges/0/range/start/character"),
        Some(&json!(5))
    );
    assert_eq!(
        change.pointer("/params/contentChanges/0/text"),
        Some(&json!("z"))
    );
}

#[test]
fn document_colors_and_color_completions_carry_rgb() {
    let colors = json!([
        {"range":{"start":{"line":0,"character":12},"end":{"line":0,"character":22}},
         "color":{"red":0.937,"green":0.267,"blue":0.267,"alpha":1.0}},
        {"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":1}},
         "color":{"red":"bad","green":0,"blue":0,"alpha":1}}
    ]);
    let Ok(LspFeatureResult::Colors(colors)) =
        normalize_result(&LspFeature::DocumentColors, &colors)
    else {
        panic!("colours");
    };
    assert_eq!(colors.len(), 1);
    assert_eq!(colors[0].rgb, (239, 68, 68));
    assert_eq!(colors[0].range.start.character, 12);
    assert_eq!(
        normalize_result(&LspFeature::DocumentColors, &Value::Null),
        Ok(LspFeatureResult::Colors(Vec::new()))
    );
    let capabilities =
        initialize_capabilities(Some(&json!({"capabilities":{"colorProvider":true}}))).unwrap();
    assert!(capabilities.document_colors);

    let items = completion_items(&json!([
        {"label":"bg-red-500","kind":16,"documentation":"#ef4444"},
        {"label":"text-sky-500","kind":16,"documentation":{"kind":"plaintext","value":"rgb(14, 165, 233)"}},
        {"label":"fff","kind":16,"detail":"color: #fff"},
        {"label":"red","kind":6,"documentation":"#ff0000"}
    ]))
    .unwrap();
    let swatches = items.iter().map(|item| item.color()).collect::<Vec<_>>();
    assert_eq!(
        swatches,
        [
            Some((239, 68, 68)),
            Some((14, 165, 233)),
            Some((255, 255, 255)),
            None
        ]
    );
}

#[test]
fn color_completions_find_hex_colors_after_non_color_hashes() {
    let items = completion_items(&json!([
        {"label":"red","kind":16,"documentation":{"kind":"markdown","value":"### Preview\n\n#ff0000"}},
        {"label":"short","kind":16,"documentation":"#no-color #12 #0f0"},
        {"label":"alpha","kind":16,"detail":"#invalid #11223344"},
        {"label":"rgb","kind":16,"documentation":"### Preview\nrgb(14, 165, 233)"},
        {"label":"invalid","kind":16,"documentation":"### Preview\n#12 #12345"}
    ])).unwrap();
    assert_eq!(items.iter().map(|item| item.color()).collect::<Vec<_>>(), [
        Some((255, 0, 0)),
        Some((0, 255, 0)),
        Some((17, 34, 51)),
        Some((14, 165, 233)),
        None,
    ]);
}

#[test]
fn workspace_mutations_reject_oversized_documents_and_edit_lists() {
    let edit = json!({"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"newText":"x"});
    let mut changes = serde_json::Map::new();
    for index in 0..257 {
        changes.insert(format!("file:///{index}.rs"), json!([edit.clone()]));
    }
    assert!(workspace_edit(&json!({"changes":changes})).is_err());
    assert!(
        workspace_edit(
            &json!({"documentChanges":vec![json!({"kind":"create","uri":"file:///x"});257]})
        )
        .is_err()
    );
    assert!(workspace_edit(&json!({"changes":{"file:///x":vec![edit;4097]}})).is_err());
}
#[test]
fn semantic_delta_support_and_response_modes_are_distinct() {
    use super::protocol::normalize_result;
    for (full, delta) in [
        (json!(true), false),
        (json!({"delta":true}), true),
        (json!({"delta":false}), false),
    ] {
        let caps = initialize_capabilities(Some(
            &json!({"capabilities":{"semanticTokensProvider":{"full":full}}}),
        ))
        .unwrap();
        assert!(caps.semantic_tokens);
        assert_eq!(caps.semantic_tokens_delta, delta);
    }
    let feature = LspFeature::SemanticTokensDelta {
        previous_result_id: "old".into(),
    };
    assert!(matches!(
        normalize_result(
            &feature,
            &json!({"edits":[{"start":0,"deleteCount":1,"data":[3]}]})
        )
        .unwrap(),
        LspFeatureResult::SemanticTokenDelta(_)
    ));
    assert!(matches!(
        normalize_result(&feature, &json!({"data":[0,0,1,0,0]})).unwrap(),
        LspFeatureResult::SemanticTokens(_)
    ));
}
#[test]
fn maximum_hints_preserve_server_formatting_and_validation() {
    let mut config = super::LspServerConfiguration::for_server(
        "vscode-json-language-server",
        std::path::Path::new("/server"),
    );
    config.overlay(super::LspServerConfiguration::maximum_inlay_hints(
        "vscode-json-language-server",
    ));
    assert_eq!(
        config.initialization_options().unwrap()["provideFormatter"],
        true
    );
    assert_eq!(config.settings["json"]["validate"]["enable"], true);
}
#[cfg(unix)]
#[test]
fn file_uris_preserve_non_utf8_path_bytes() {
    use std::os::unix::ffi::OsStringExt;
    let a = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/a\xff.rs".to_vec()));
    let b = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/a\xfe.rs".to_vec()));
    assert_ne!(file_uri(&a), file_uri(&b));
    assert_eq!(path_from_file_uri(&file_uri(&a)), Some(a));
    assert_eq!(path_from_file_uri(&file_uri(&b)), Some(b));
}

fn boundary_service(script: &str) -> (tempfile::TempDir, LspService, LspDocument) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("server.py");
    std::fs::write(&path, script).unwrap();
    let mut definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|d| d.id == "rust-analyzer")
        .unwrap();
    let python = crate::tools::discover_executable("python3").unwrap();
    definition.executable = python.display().to_string();
    let document = LspDocument {
        path: root.path().join("main.rs"),
        language_id: "rust".into(),
        version: 0,
        text: "fn main() {}".into(),
    };
    let mut service = LspService::new();
    service
        .start_session_absolute_with_policy(super::LspSessionLaunch {
            session_key: "boundary",
            definition: &definition,
            executable: python,
            arguments: vec!["-u".into(), path.display().to_string()],
            root: root.path().into(),
            document: Some(document.clone()),
            policy: LspPolicy::default(),
            configuration: super::LspServerConfiguration::standard(),
            environment: Default::default(),
        })
        .unwrap();
    (root, service, document)
}
const BOUNDARY_SERVER: &str = r#"
import json,sys,pathlib,time,os
def read():
 length=0
 while True:
  line=sys.stdin.buffer.readline()
  if not line: return None
  if line in (b'\r\n',b'\n'): break
  if line.lower().startswith(b'content-length:'): length=int(line.split(b':',1)[1])
 return json.loads(sys.stdin.buffer.read(length))
def send(value):
 data=json.dumps(value).encode()
 sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n'%len(data)).encode()+data);sys.stdout.buffer.flush()
m=read()
send({'jsonrpc':'2.0','id':1,'method':'window/showMessageRequest','params':{'type':3,'message':'starting'}})
reply=read()
assert reply['id']==1 and 'method' not in reply
send({'jsonrpc':'2.0','id':m['id'],'result':{'capabilities':{'hoverProvider':True}}})
while True:
 m=read()
 if m is None: break
 method=m.get('method')
 if method=='initialized':
  send({'jsonrpc':'2.0','id':2,'method':'client/registerCapability','params':{'registrations':[{'id':'completion','method':'textDocument/completion','registerOptions':{'resolveProvider':True,'triggerCharacters':['.']}}]}})
  send({'jsonrpc':'2.0','id':3,'method':'workspace/workspaceFolders','params':{}})
  for n in range(250): sys.stderr.write('line\n');sys.stderr.flush()
  time.sleep(.03);sys.stderr.write('still open\n');sys.stderr.flush()
  pathlib.Path('stderr-ok').write_text('ok')
  for n in range(600): send({'jsonrpc':'2.0','method':'$/progress','params':{'token':n,'value':{'kind':'begin','title':'progress'}}})
 if m.get('id')==3 and 'result' in m:
  pathlib.Path('folders.json').write_text(json.dumps(m['result']))
  send({'jsonrpc':'2.0','id':4,'method':'client/unregisterCapability','params':{'unregisterations':[{'id':'completion','method':'textDocument/completion'}]}})
 if method=='textDocument/hover': send({'jsonrpc':'2.0','id':m['id'],'result':{'contents':'alive'}})
"#;
#[test]
fn server_request_ids_dynamic_options_folders_and_saturated_readers_stay_healthy() {
    let (root, mut service, document) = boundary_service(BOUNDARY_SERVER);
    thread::sleep(Duration::from_millis(100));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut progress = 0;
    let mut registered = false;
    let mut unregistered = false;
    while progress < 600 || !unregistered {
        assert!(
            Instant::now() < deadline,
            "progress={progress}, registered={registered}, unregistered={unregistered}"
        );
        match service.try_event() {
            Some(LspEvent::Progress { .. }) => progress += 1,
            Some(LspEvent::CapabilitiesChanged { capabilities, .. }) => {
                if capabilities.completion {
                    registered = capabilities.completion_resolve
                        && capabilities.completion_triggers == ["."];
                } else {
                    unregistered = !capabilities.completion_resolve
                        && capabilities.completion_triggers.is_empty();
                }
            }
            Some(LspEvent::Failed { message, .. }) => panic!("{message}"),
            Some(LspEvent::Stopped { .. }) => panic!("healthy server stopped"),
            _ => thread::sleep(Duration::from_millis(1)),
        }
    }
    assert!(registered && root.path().join("stderr-ok").is_file());
    let folders: Value =
        serde_json::from_slice(&std::fs::read(root.path().join("folders.json")).unwrap()).unwrap();
    assert_eq!(folders[0]["uri"], file_uri(root.path()));
    service
        .request(
            "boundary",
            LspFeature::Hover,
            document,
            crate::editor::text::ByteOffset(0),
            7,
        )
        .unwrap();
    loop {
        assert!(Instant::now() < deadline);
        match service.try_event() {
            Some(LspEvent::Result { generation: 7, .. }) => break,
            Some(LspEvent::Failed { message, .. } | LspEvent::RequestFailed { message, .. }) => {
                panic!("{message}")
            }
            _ => thread::sleep(Duration::from_millis(1)),
        }
    }
    service.stop("boundary").unwrap();
}
#[test]
fn stop_cancels_a_blocked_write_even_with_a_full_command_queue() {
    let script = BOUNDARY_SERVER
        .split("m=read()\nsend(")
        .next()
        .unwrap()
        .to_owned()
        + r#"
m=read()
send({'jsonrpc':'2.0','id':m['id'],'result':{'capabilities':{'hoverProvider':True}}})
pathlib.Path('pid').write_text(str(os.getpid()))
time.sleep(30)
"#;
    let (root, mut service, mut document) = boundary_service(&script);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(service.try_event(), Some(LspEvent::Running { .. })) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    document.version = 1;
    document.text = "x".repeat(2 * 1024 * 1024);
    service.sync("boundary", document.clone()).unwrap();
    thread::sleep(Duration::from_millis(30));
    let mut full = false;
    for _ in 0..100 {
        if service.sync("boundary", document.clone()).is_err() {
            full = true;
            break;
        }
    }
    assert!(full);
    let started = Instant::now();
    service.stop("boundary").unwrap();
    assert!(started.elapsed() < Duration::from_millis(200));
    #[cfg(target_os = "linux")]
    {
        let pid = std::fs::read_to_string(root.path().join("pid")).unwrap();
        while Path::new(&format!("/proc/{pid}")).exists() {
            assert!(
                Instant::now() < deadline,
                "blocked language server was not reaped"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
fn failed_request_writes_report_the_original_generation() {
    let script = BOUNDARY_SERVER
        .split("m=read()\nsend(")
        .next()
        .unwrap()
        .to_owned()
        + r#"
m=read()
send({'jsonrpc':'2.0','id':m['id'],'result':{'capabilities':{'hoverProvider':True}}})
while True:
 m=read()
 if m.get('method')=='textDocument/didOpen': break
os.close(0)
pathlib.Path('closed').write_text('closed')
time.sleep(30)
"#;
    let (root, mut service, document) = boundary_service(&script);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.path().join("closed").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    service
        .request(
            "boundary",
            LspFeature::Hover,
            document,
            crate::editor::text::ByteOffset(0),
            91,
        )
        .unwrap();
    loop {
        assert!(
            Instant::now() < deadline,
            "failed write never produced a request outcome"
        );
        match service.try_event() {
            Some(LspEvent::RequestFailed {
                generation: 91,
                method,
                ..
            }) => {
                assert_eq!(method, "textDocument/hover");
                break;
            }
            _ => thread::sleep(Duration::from_millis(5)),
        }
    }
    service.stop("boundary").unwrap();
}
