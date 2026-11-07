//! Tests for language-server connections and protocol data.

use std::path::Path;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use serde_json::Value;

use serde_json::json;

use crate::language::lsp::messages::LspDocument;

use crate::language::lsp::messages::LspEvent;

use crate::language::lsp::messages::LspFeature;

use crate::language::lsp::messages::LspFeatureResult;

use crate::language::lsp::messages::LspMarkupKind;

use crate::language::lsp::server::LspPolicy;

use crate::language::lsp::messages::LspProgressKind;

use crate::language::lsp::server::SyncKind;

use crate::language::lsp::paths::file_uri;

use crate::language::lsp::paths::language_id_for_path;

use crate::language::lsp::paths::path_from_file_uri;

use crate::language::lsp::protocol::completion_items;

use crate::language::lsp::protocol::hover_result;

use crate::language::lsp::protocol::incremental_change;

use crate::language::lsp::protocol::initialize_capabilities;

use crate::language::lsp::protocol::initialize_message;

use crate::language::lsp::protocol::inlay_hints;

use crate::language::lsp::protocol::lsp_progress_event;

use crate::language::lsp::protocol::normalize_result;

use crate::language::lsp::protocol::server_status_event;

use crate::language::lsp::protocol::workspace_edit;

use crate::language::lsp::session::LspClient;

use crate::editor::text::PositionEncoding;
use crate::language::lsp::session::requested_executable_name;
use crate::tools::ToolDefinition;
use crate::tools::ToolKind;
use crate::tools::tool_launch_environment;

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
fn language_mapping_preserves_case_sensitive_cpp_extensions() {
    for (path, language) in [
        ("main.C", "cpp"),
        ("main.H", "cpp"),
        ("main.c", "c"),
        ("main.h", "c"),
        ("main.CPP", "cpp"),
        ("main.RS", "rust"),
    ] {
        assert_eq!(
            language_id_for_path(Path::new(path)),
            Some(language),
            "{path}"
        );
    }
}

#[test]
fn file_uris_validate_authorities() {
    for uri in [
        "file:///home/user/a%20b.rs",
        "file://localhost/home/user/a%20b.rs",
        "file://LOCALHOST/home/user/a%20b.rs",
    ] {
        assert_eq!(
            path_from_file_uri(uri).as_deref(),
            Some(Path::new("/home/user/a b.rs"))
        );
    }
    for uri in [
        #[cfg(unix)]
        "file://remote/home/user/main.rs",
        "file://localhost:80/home/user/main.rs",
        "file://user@localhost/home/user/main.rs",
        "file://relative.rs",
        "file:///home/user/bad%GG.rs",
    ] {
        assert_eq!(path_from_file_uri(uri), None, "{uri}");
    }
}

#[cfg(windows)]
#[test]
fn windows_file_uris_match_canonical_buffer_paths() {
    for path in [r"C:\src\a b.rs", r"\\?\C:\src\a b.rs"] {
        let uri = file_uri(Path::new(path));
        assert_eq!(uri, "file:///C:/src/a%20b.rs");
        assert_eq!(
            path_from_file_uri(&uri).unwrap(),
            Path::new(r"C:\src\a b.rs")
        );
    }
    for path in [r"\\server\share\a b.rs", r"\\?\UNC\server\share\a b.rs"] {
        let uri = file_uri(Path::new(path));
        assert_eq!(uri, "file://server/share/a%20b.rs");
        assert_eq!(
            path_from_file_uri(&uri).unwrap(),
            Path::new(r"\\server\share\a b.rs")
        );
    }
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
    let registered = crate::language::lsp::server_message::effective_capabilities(
        &crate::language::lsp::LspCapabilities::default(),
        ["textDocument/formatting", "textDocument/rangeFormatting"]
            .into_iter()
            .map(|method| (method, &Value::Null)),
    );
    assert!(registered.formatting && registered.range_formatting);
    assert!(!registered.rename);
}

fn server_message_replies(
    message: Value,
    base: &crate::language::lsp::LspCapabilities,
    capabilities: &mut crate::language::lsp::LspCapabilities,
    registrations: &mut std::collections::HashMap<String, (String, Value)>,
    documents: &std::collections::HashMap<String, LspDocument>,
) -> Vec<Value> {
    use std::sync::Arc;
    use std::sync::mpsc;
    let (channel, _events) = mpsc::sync_channel(8);
    let (commands, _requests) = mpsc::sync_channel(1);
    let events = crate::language::lsp::origins::EventSender {
        channel,
        commands,
        context: crate::language::lsp::origins::EditContext {
            session: Arc::new(()),
            server: Arc::from("fixture"),
            encoding: PositionEncoding::Utf16,
            diagnostic_ambiguous: false,
            sources: Arc::new(Default::default()),
            disk: None,
        },
        cancelled: Arc::new(false.into()),
        stop_requested: Arc::new(false.into()),
    };
    let mut output = Vec::new();
    crate::language::lsp::server_message::handle_server_message(
        crate::language::lsp::server_message::ServerMessageContext {
            server: "fixture",
            root: Path::new("/fixture"),
            stdin: &mut output,
            events: &events,
            logs: &mut Default::default(),
            base_capabilities: base,
            capabilities,
            registrations,
            configuration: &crate::language::lsp::LspServerConfiguration::standard(),
            sources: &Default::default(),
            diagnostic_lineage: &Default::default(),
            documents,
            apply_requests: &mut Default::default(),
        },
        &message,
    );
    let mut input = std::io::Cursor::new(output);
    std::iter::from_fn(|| crate::process::read_content_length_message(&mut input).unwrap())
        .collect()
}

#[test]
fn diagnostic_disk_snapshots_reject_nonregular_and_oversized_targets() {
    use crate::language::lsp::LspServerConfiguration;
    use crate::language::lsp::origins::EditContext;
    use crate::language::lsp::origins::EventSender;
    use crate::language::lsp::server_message::ServerMessageContext;
    use crate::language::lsp::server_message::handle_server_message;
    use std::sync::Arc;
    use std::sync::mpsc;
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    nix::unistd::mkfifo(
        &root.path().join("fifo"),
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let names = [
        #[cfg(unix)]
        "fifo",
        "directory",
        "large",
        "regular",
    ];
    std::fs::create_dir(root.path().join("directory")).unwrap();
    std::fs::write(root.path().join("large"), vec![b'x'; 4 * 1024 * 1024 + 1]).unwrap();
    std::fs::write(root.path().join("regular"), "source").unwrap();
    let (channel, receiver) = mpsc::sync_channel(1);
    let (commands, _requests) = mpsc::sync_channel(1);
    let events = EventSender {
        channel,
        commands,
        cancelled: Default::default(),
        stop_requested: Default::default(),
        context: EditContext {
            session: Arc::new(()),
            server: "fixture".into(),
            encoding: Default::default(),
            diagnostic_ambiguous: false,
            sources: Arc::new(Default::default()),
            disk: None,
        },
    };
    let worker_root = crate::files::canonicalize(root.path()).unwrap();
    let worker = std::thread::spawn(move || {
        for name in names {
            let uri = file_uri(&worker_root.join(name));
            handle_server_message(
                ServerMessageContext {
                    server: "fixture",
                    root: &worker_root,
                    stdin: &mut Vec::new(),
                    events: &events,
                    logs: &mut Default::default(),
                    base_capabilities: &Default::default(),
                    capabilities: &mut Default::default(),
                    registrations: &mut Default::default(),
                    configuration: &LspServerConfiguration::standard(),
                    sources: &Default::default(),
                    diagnostic_lineage: &Default::default(),
                    documents: &Default::default(),
                    apply_requests: &mut Default::default(),
                },
                &json!({"method":"textDocument/publishDiagnostics", "params":{"uri":uri,"diagnostics":[]}}),
            );
        }
    });
    for name in names {
        let packet = receiver
            .recv_timeout(Duration::from_secs(3))
            .expect("diagnostic snapshot read blocked");
        let uri = file_uri(&root.path().join(name));
        assert!(matches!(packet.event, LspEvent::Diagnostics { .. }));
        assert_eq!(
            packet.context.disk_document(&uri).map(ToString::to_string),
            (name == "regular").then(|| "source".to_owned())
        );
    }
    worker.join().unwrap();
}

#[test]
fn oversized_dynamic_batches_are_rejected_without_changing_registrations() {
    let base = crate::language::lsp::LspCapabilities::default();
    let mut capabilities = base.clone();
    let mut registrations = std::collections::HashMap::from([(
        "keep".into(),
        ("textDocument/hover".into(), Value::Null),
    )]);
    let original = registrations.clone();
    for (method, key) in [
        ("client/registerCapability", "registrations"),
        ("client/unregisterCapability", "unregisterations"),
        ("client/unregisterCapability", "unregistrations"),
    ] {
        let items = (0..257)
            .map(|_| json!({"id":"keep","method":"textDocument/formatting"}))
            .collect::<Vec<_>>();
        let reply = server_message_replies(
            json!({"id":1,"method":method,"params":{key:items}}),
            &base,
            &mut capabilities,
            &mut registrations,
            &Default::default(),
        )
        .remove(0);
        assert_eq!(reply["error"]["code"], -32602);
        assert_eq!(registrations, original);
        assert_eq!(capabilities, base);
    }
}

#[test]
fn configuration_replies_preserve_every_requested_position() {
    let base = crate::language::lsp::LspCapabilities::default();
    let items = (0..257)
        .map(|_| json!({"section":"missing"}))
        .collect::<Vec<_>>();
    let reply = server_message_replies(
        json!({"id":1,"method":"workspace/configuration","params":{"items":items}}),
        &base,
        &mut base.clone(),
        &mut Default::default(),
        &Default::default(),
    )
    .remove(0);
    let values = reply["result"].as_array().unwrap();
    assert_eq!(values.len(), 257);
    assert!(values.iter().all(Value::is_object));
}

#[test]
fn dynamic_providers_match_document_selectors_and_keep_static_providers() {
    let formatting = LspFeature::Formatting {
        tab_size: 2,
        insert_spaces: true,
    };
    let base = crate::language::lsp::LspCapabilities::default();
    let options =
        json!({"documentSelector":[{"language":"javascript","scheme":"file","pattern":"**/*.js"}]});
    let capabilities = crate::language::lsp::server_message::effective_capabilities(
        &base,
        [("textDocument/formatting", &options)].into_iter(),
    );
    let js = LspDocument {
        path: "/fixture/a.js".into(),
        language_id: "javascript".into(),
        version: 0,
        text: String::new(),
    };
    assert!(formatting.capability_for_document(&capabilities, &js));
    let ts = LspDocument {
        path: "/fixture/a.ts".into(),
        language_id: "typescript".into(),
        ..js.clone()
    };
    assert!(!formatting.capability_for_document(&capabilities, &ts));
    assert!(!formatting.capability_for_document(
        &capabilities,
        &LspDocument {
            path: "/fixture/a.jsx".into(),
            ..js.clone()
        }
    ));
    let static_base = crate::language::lsp::LspCapabilities {
        formatting: true,
        ..base.clone()
    };
    let capabilities = crate::language::lsp::server_message::effective_capabilities(
        &static_base,
        [("textDocument/formatting", &options)].into_iter(),
    );
    assert!(formatting.capability_for_document(&capabilities, &ts));
    for base_uri in [
        json!("file:///fixture"),
        json!({"uri":"file:///fixture","name":"fixture"}),
    ] {
        let options =
            json!({"documentSelector":[{"pattern":{"baseUri":base_uri,"pattern":"*.js"}}]});
        let capabilities = crate::language::lsp::server_message::effective_capabilities(
            &base,
            [("textDocument/formatting", &options)].into_iter(),
        );
        assert!(formatting.capability_for_document(&capabilities, &js));
        assert!(!formatting.capability_for_document(&capabilities, &ts));
        assert!(!formatting.capability_for_document(
            &capabilities,
            &LspDocument {
                path: "/elsewhere/a.js".into(),
                ..js.clone()
            }
        ));
    }
    for selector in [json!([]), json!([{"scheme":"untitled"}])] {
        let options = json!({"documentSelector":selector});
        let capabilities = crate::language::lsp::server_message::effective_capabilities(
            &base,
            [("textDocument/formatting", &options)].into_iter(),
        );
        assert!(!formatting.capability_for_document(&capabilities, &js));
    }
}

#[test]
fn dynamic_change_sync_is_document_specific_and_unregistration_restores_static_policy() {
    let base = crate::language::lsp::LspCapabilities {
        sync_kind: SyncKind::None,
        ..Default::default()
    };
    let mut capabilities = base.clone();
    let mut registrations = Default::default();
    let old = LspDocument {
        path: "/fixture/main.rs".into(),
        language_id: "rust".into(),
        version: 3,
        text: "abc".into(),
    };
    let documents = std::collections::HashMap::from([
        (old.uri(), old.clone()),
        (
            "file:///fixture/main.py".into(),
            LspDocument {
                path: "/fixture/main.py".into(),
                language_id: "python".into(),
                ..old.clone()
            },
        ),
    ]);
    let replies = server_message_replies(
        json!({"id":1,"method":"client/registerCapability","params":{"registrations":[{
            "id":"changes","method":"textDocument/didChange","registerOptions":{"documentSelector":[{"language":"rust"}],"syncKind":2}
        }]}}),
        &base,
        &mut capabilities,
        &mut registrations,
        &documents,
    );
    // Registration itself must not emit a change with an unchanged document version.
    assert_eq!(replies.len(), 1);
    let reply = &replies[0];
    assert!(reply.get("result").is_some());
    let new = LspDocument {
        version: 4,
        text: "abcd".into(),
        ..old.clone()
    };
    let mut output = Vec::new();
    crate::language::lsp::protocol::send_change(&mut output, Some(&old), &new, &capabilities)
        .unwrap();
    let change = crate::process::read_content_length_message(&mut std::io::Cursor::new(output))
        .unwrap()
        .unwrap();
    assert_eq!(change["params"]["contentChanges"][0]["text"], "d");
    assert!(change["params"]["contentChanges"][0]["range"].is_object());
    let mut output = Vec::new();
    crate::language::lsp::protocol::send_change(
        &mut output,
        None,
        &LspDocument {
            language_id: "python".into(),
            ..new.clone()
        },
        &capabilities,
    )
    .unwrap();
    assert!(output.is_empty());
    let replies = server_message_replies(
        json!({"id":2,"method":"client/unregisterCapability","params":{"unregisterations":[{"id":"changes","method":"textDocument/didChange"}]}}),
        &base,
        &mut capabilities,
        &mut registrations,
        &documents,
    );
    assert_eq!(replies.len(), 1);
    let reply = &replies[0];
    assert!(reply.get("result").is_some());
    crate::language::lsp::protocol::send_change(&mut output, Some(&old), &new, &capabilities)
        .unwrap();
    assert!(output.is_empty());
    assert_eq!(capabilities, base);
}

#[test]
fn rust_analyzer_status_reports_whether_the_workspace_loaded_fully() {
    assert_eq!(
        initialize_message(Path::new("/project"), None)
            .pointer("/params/capabilities/experimental/serverStatusNotification"),
        Some(&json!(true))
    );
    let status = |health: &str| {
        server_status_event(
            "rust-analyzer",
            &json!({
                "method":"experimental/serverStatus",
                "params":{"health":health,"quiescent":true,"message":"Failed to run build scripts of some packages.\n\ndetails"}
            }),
        )
    };
    assert!(matches!(
        status("warning"),
        Some(LspEvent::ServerStatus { healthy: false, message, .. })
            if message.starts_with("Failed to run build scripts")
    ));
    assert!(matches!(
        status("ok"),
        Some(LspEvent::ServerStatus { healthy: true, .. })
    ));
    assert!(status("unknown").is_none());
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
            && token == "\"cargo-check\""
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
fn progress_token_identity_distinguishes_numbers_from_strings_across_updates() {
    let mut identities = Vec::new();
    for kind in ["begin", "report", "end"] {
        let tokens = [json!(1), json!("1"), json!("\"1\"")]
            .into_iter()
            .map(|token| {
                let event = lsp_progress_event(
                    "server",
                    &json!({"params":{"token":token,"value":{"kind":kind}}}),
                )
                .expect("progress event");
                let LspEvent::Progress { token, .. } = event else {
                    panic!("expected progress");
                };
                token
            })
            .collect::<Vec<_>>();
        assert_ne!(tokens[0], tokens[1]);
        assert_ne!(tokens[1], tokens[2]);
        identities.push(tokens);
    }
    assert_eq!(identities[0], identities[1]);
    assert_eq!(identities[1], identities[2]);
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
fn language_tagged_hover_strings_preserve_literal_code_in_mixed_sections() {
    let hover = hover_result(&json!({"contents":[
        "**Documentation**",
        {"language":"python","value":"__init__(self)\n`literal` *code*\u{001b}[31m"}
    ]}));
    assert_eq!(hover.sections.len(), 2);
    assert_eq!(hover.sections[0].kind, LspMarkupKind::Markdown);
    assert_eq!(hover.sections[1].kind, LspMarkupKind::PlainText);
    assert_eq!(hover.sections[1].value, "__init__(self)\n`literal` *code*");

    let hover = hover_result(&json!({
        "contents":{"language":"python","value":"__init__(self)"}
    }));
    assert_eq!(hover.sections.len(), 1);
    assert_eq!(hover.sections[0].kind, LspMarkupKind::PlainText);
    assert_eq!(hover.sections[0].value, "__init__(self)");
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
        let configuration =
            crate::language::lsp::LspServerConfiguration::for_server(server, &executable);
        let initialize = initialize_message(
            directory.path(),
            configuration.initialization_options.as_ref(),
        );
        assert_eq!(
            initialize
                .pointer("/params/initializationOptions/typescript/tsdk")
                .and_then(Value::as_str),
            crate::files::canonicalize(&tsdk).unwrap().to_str()
        );
    }
}

#[test]
fn maximum_inlay_hint_profiles_are_adapter_specific_and_exhaustive() {
    let rust = crate::language::lsp::LspServerConfiguration::maximum_inlay_hints("rust-analyzer");
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

    let typescript = crate::language::lsp::LspServerConfiguration::maximum_inlay_hints(
        "typescript-language-server",
    );
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

    let gopls = crate::language::lsp::LspServerConfiguration::maximum_inlay_hints("gopls");
    assert_eq!(
        gopls
            .settings
            .pointer("/gopls/hints/compositeLiteralFields"),
        Some(&json!(true))
    );
    assert_eq!(
        crate::language::lsp::LspServerConfiguration::maximum_inlay_hints("clangd"),
        crate::language::lsp::LspServerConfiguration::standard()
    );
    assert!(matches!(
        normalize_result(
            &LspFeature::InlayHints {
                range: crate::language::lsp::LspRange {
                    start: crate::language::lsp::LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: crate::language::lsp::LspPosition {
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
fn initialization_advertises_semantic_tokens_but_not_pull_diagnostics() {
    let initialize = initialize_message(Path::new("/workspace"), None);
    let document = &initialize["params"]["capabilities"]["textDocument"];
    let semantic = &document["semanticTokens"];
    assert_eq!(
        semantic["requests"],
        json!({"full":{"delta":true},"range":false})
    );
    assert_eq!(semantic["formats"], json!(["relative"]));
    assert!(
        semantic["tokenTypes"]
            .as_array()
            .unwrap()
            .contains(&json!("function"))
    );
    assert!(semantic["tokenModifiers"].is_array());
    assert_eq!(semantic["overlappingTokenSupport"], false);
    assert_eq!(semantic["multilineTokenSupport"], false);
    // A client that advertises pull diagnostics stops receiving rust-analyzer's pushed ones.
    assert!(document["diagnostic"].is_null());
    assert_eq!(document["synchronization"]["dynamicRegistration"], false);
}

#[test]
fn absent_change_support_does_not_enable_document_changes() {
    for sync in [Value::Null, json!({"openClose":true}), json!(0)] {
        let capabilities =
            initialize_capabilities(Some(&json!({"capabilities":{"textDocumentSync":sync}})))
                .unwrap();
        assert_eq!(capabilities.sync_kind, SyncKind::None);
        let mut output = Vec::new();
        crate::language::lsp::protocol::send_change(
            &mut output,
            None,
            &LspDocument {
                path: "/workspace/main.rs".into(),
                language_id: "rust".into(),
                version: 1,
                text: "changed".into(),
            },
            &capabilities,
        )
        .unwrap();
        assert!(output.is_empty());
    }
    assert_eq!(
        initialize_capabilities(Some(&json!({"capabilities":{}})))
            .unwrap()
            .sync_kind,
        SyncKind::None
    );
}

#[test]
fn formatting_parameters_use_the_requested_indentation_for_both_methods() {
    let range = crate::language::lsp::LspRange {
        start: crate::language::lsp::LspPosition {
            line: 0,
            character: 0,
        },
        end: crate::language::lsp::LspPosition {
            line: 2,
            character: 0,
        },
    };
    for (tab_size, insert_spaces) in [(2, true), (8, false)] {
        for feature in [
            LspFeature::Formatting {
                tab_size,
                insert_spaces,
            },
            LspFeature::RangeFormatting {
                range: range.clone(),
                tab_size,
                insert_spaces,
            },
        ] {
            let params =
                crate::language::lsp::protocol::feature_params(&feature, "file:///main.rs", 0, 0);
            assert_eq!(
                params["options"],
                json!({"tabSize":tab_size,"insertSpaces":insert_spaces})
            );
        }
    }
}

#[test]
fn failed_document_sync_keeps_the_last_sent_document_and_stops_the_session() {
    use crate::editor::EditorState;
    use crate::editor::text::TextBuffer;
    use crate::language::lsp::session::SessionCommand;
    use crate::language::lsp::session::handle_command;
    use std::collections::HashMap;
    let old = LspDocument {
        path: "/workspace/main.rs".into(),
        language_id: "rust".into(),
        version: 1,
        text: "old".into(),
    };
    let new = LspDocument {
        version: 2,
        text: "x".repeat(16 * 1024 * 1024),
        ..old.clone()
    };
    let mut editor = EditorState::new("/workspace".into());
    let (buffer, _) = editor
        .open_text(TextBuffer::from_text("old"), Some(old.path.clone()))
        .unwrap();
    let mut service = LspClient::new();
    let endpoint = service.replace_edit_source_fixture(
        old.uri(),
        (buffer, editor.buffers[buffer].text.source_version()),
        PositionEncoding::Utf16,
    );
    for opened in [true, false] {
        for command in [
            SessionCommand::Sync(new.clone()),
            SessionCommand::Save(new.clone()),
        ] {
            let mut documents = HashMap::from([(old.uri(), old.clone())]);
            let mut opened_documents = if opened {
                HashMap::from([(old.uri(), old.version)])
            } else {
                HashMap::new()
            };
            let mut stopping = false;
            handle_command(
                command,
                "fixture",
                Path::new("/workspace"),
                Path::new("/server"),
                &mut std::io::sink(),
                true,
                &crate::language::lsp::LspCapabilities {
                    sync_kind: SyncKind::Incremental,
                    ..Default::default()
                },
                &mut documents,
                &mut HashMap::new(),
                &mut Default::default(),
                &mut HashMap::new(),
                &mut opened_documents,
                &mut HashMap::new(),
                &mut 2,
                Duration::from_secs(5),
                0,
                &endpoint.events,
                &mut stopping,
                &mut crate::language::lsp::LspServerConfiguration::standard(),
            )
            .unwrap_err();
            assert!(stopping);
            assert_eq!(documents.get(&old.uri()), Some(&old));
            assert_eq!(
                opened_documents.get(&old.uri()).copied(),
                opened.then_some(old.version)
            );
            assert!(
                matches!(service.try_event(), Some(LspEvent::Failed { message, .. }) if message.contains("synchronization write failed"))
            );
        }
    }
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
fn code_action_diagnostic_codes_keep_their_json_type_and_related_information() {
    let range = json!({"start":{"line":0,"character":0},"end":{"line":0,"character":1}});
    for code in [json!("001"), json!("123"), json!(123)] {
        let related = json!([{"location":{"uri":"file:///other.rs","range":range},"message":"other declaration"}]);
        let diagnostics = crate::language::lsp::protocol::diagnostics(&json!([{
            "range":range,"message":"duplicate","code":code,"relatedInformation":related,
        }]));
        assert_eq!(diagnostics[0].code, Some(code.clone()));
        assert_eq!(diagnostics[0].related[0].0.uri, "file:///other.rs");
        assert_eq!(diagnostics[0].related[0].1, "other declaration");
        let feature = LspFeature::CodeAction {
            range: diagnostics[0].range.clone(),
            diagnostics,
        };
        let params =
            crate::language::lsp::protocol::feature_params(&feature, "file:///main.rs", 0, 0);
        assert_eq!(params["context"]["diagnostics"][0]["code"], code);
    }
}

#[test]
fn signature_help_selects_the_active_overload_with_a_safe_fallback() {
    for (active, expected) in [
        (json!(1), "second"),
        (json!(99), "first"),
        (Value::Null, "first"),
        (json!(-1), "first"),
    ] {
        let result = normalize_result(
            &LspFeature::SignatureHelp,
            &json!({
                "signatures":[{"label":"first"},{"label":"second"}],"activeSignature":active,
            }),
        )
        .unwrap();
        assert!(
            matches!(result, LspFeatureResult::Signature(content) if content.value == expected)
        );
    }
    assert_eq!(
        crate::language::lsp::protocol::signature_text(&json!({"signatures":[]})),
        "no signature information"
    );
}

#[test]
fn null_results_clear_persistent_document_projections() {
    for feature in [
        LspFeature::DocumentHighlights,
        LspFeature::FoldingRanges,
        LspFeature::SelectionRanges,
    ] {
        assert!(
            matches!(normalize_result(&feature, &Value::Null).unwrap(), LspFeatureResult::Ranges(items) if items.is_empty())
        );
    }
    for feature in [
        LspFeature::SemanticTokensFull,
        LspFeature::SemanticTokensDelta {
            previous_result_id: "old".into(),
        },
    ] {
        assert!(
            matches!(normalize_result(&feature, &Value::Null).unwrap(), LspFeatureResult::SemanticTokens(tokens) if tokens.data.is_empty() && tokens.result_id.is_none())
        );
    }
}

#[test]
fn code_lenses_keep_resolve_data_and_accept_resolved_commands() {
    let item = json!({"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":1}},"data":{"id":42}});
    let lenses = crate::language::lsp::protocol::code_lenses(&json!([item]));
    assert_eq!(lenses[0].resolve_item, Some(item.clone()));
    assert!(lenses[0].command.is_none());
    let feature = LspFeature::CodeLensResolve { item: item.clone() };
    assert_eq!(feature.method(), "codeLens/resolve");
    assert_eq!(
        crate::language::lsp::protocol::feature_params(&feature, "file:///main.rs", 0, 0),
        item
    );
    let caps = initialize_capabilities(Some(
        &json!({"capabilities":{"codeLensProvider":{"resolveProvider":true}}}),
    ))
    .unwrap();
    assert!(feature.capability(&caps));
    let mut resolved = item;
    resolved["command"] = json!({"title":"Run test","command":"run.test","arguments":[42]});
    let LspFeatureResult::CodeLens(lenses) = normalize_result(&feature, &resolved).unwrap() else {
        panic!("resolved code lens must be presented as an executable choice");
    };
    assert_eq!(lenses[0].title, "Run test");
    assert_eq!(
        lenses[0].command.as_ref().unwrap().arguments,
        vec![json!(42)]
    );
}

#[test]
fn completion_fallback_inserts_the_full_label_not_its_display_preview() {
    let label = "é".repeat(5000);
    for resolved in [false, true] {
        let item =
            crate::language::lsp::protocol::completion_item(&json!({"label":label}), resolved)
                .unwrap();
        assert_eq!(item.label.chars().count(), 4096);
        assert_eq!(item.insert_text, label);
        let item = crate::language::lsp::protocol::completion_item(
            &json!({"label":label,"insertText":"explicit"}),
            resolved,
        )
        .unwrap();
        assert_eq!(item.insert_text, "explicit");
    }
}

#[test]
fn completion_commands_survive_initial_and_resolved_items() {
    let item = json!({"label":"example","command":{"title":"Import","command":"add.import","arguments":["example"]}});
    for resolved in [false, true] {
        let parsed = crate::language::lsp::protocol::completion_item(&item, resolved).unwrap();
        let command = parsed.command.unwrap();
        assert_eq!(command.command, "add.import");
        assert_eq!(command.arguments, vec![json!("example")]);
    }
}

#[test]
fn nested_document_symbols_keep_their_positions_and_children() {
    let range =
        |line: u64| json!({"start":{"line":line,"character":4},"end":{"line":line,"character":7}});
    let found = crate::language::lsp::protocol::symbols(&json!([{
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
fn workspace_edits_reject_malformed_resource_operations_without_partial_results() {
    for operation in [
        json!({"kind":"create"}),
        json!({"kind":"create","uri":42}),
        json!({"kind":"delete"}),
        json!({"kind":"delete","uri":null}),
        json!({"kind":"rename","oldUri":"file:///old"}),
        json!({"kind":"rename","newUri":"file:///new"}),
        json!({"kind":"unknown","uri":"file:///new"}),
        json!({"uri":"file:///new"}),
    ] {
        assert!(
            workspace_edit(&json!({
                "changes":{"file:///valid":[{
                    "range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},
                    "newText":"valid"
                }]},
                "documentChanges":[operation]
            }))
            .is_err(),
            "accepted {operation}"
        );
    }
    let edit = workspace_edit(&json!({"documentChanges":[
        {"kind":"create","uri":"file:///new","options":{"overwrite":true}},
        {"kind":"rename","oldUri":"file:///old","newUri":"file:///renamed"},
        {"kind":"delete","uri":"file:///deleted"}
    ]}))
    .unwrap();
    assert_eq!(edit.resources.len(), 3);
}

#[test]
fn workspace_edits_reject_invalid_non_null_document_versions() {
    for version in [json!(-1), json!(1.5), json!("1"), json!(false), json!({})] {
        assert!(
            workspace_edit(&json!({"documentChanges":[{
                "textDocument":{"uri":"file:///a","version":version},"edits":[]
            }]}))
            .is_err(),
            "accepted {version}"
        );
    }
    for (version, expected) in [
        (Value::Null, None),
        (json!(0), Some(0)),
        (json!(42), Some(42)),
    ] {
        let edit = workspace_edit(&json!({"documentChanges":[{
            "textDocument":{"uri":"file:///a","version":version},"edits":[]
        }]}))
        .unwrap();
        assert_eq!(edit.documents[0].version, expected);
    }
    let edit = workspace_edit(&json!({"documentChanges":[{
        "textDocument":{"uri":"file:///a"},"edits":[]
    }]}))
    .unwrap();
    assert_eq!(edit.documents[0].version, None);
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
fn initialization_timeout_honors_the_one_second_minimum_and_longer_policy() {
    let sleep = crate::tools::discover_executable("sleep").expect("sleep fixture is required");
    let workspace = tempfile::tempdir().expect("workspace");
    let definition = ToolDefinition {
        id: "sleeping-server".into(),
        name: "Sleeping Server".into(),
        kind: ToolKind::Lsp,
        languages: vec!["rust".into()],
        executable: sleep.display().to_string(),
        version: None,
        release: None,
        launch_args: vec!["10".into()],
        format_arguments: Vec::new(),
        probe_args: Vec::new(),
        debug_program_policy: crate::tools::DebugProgramPolicy::default(),
        project_root_markers: vec!["Cargo.toml".into()],
        project_root_strategy: crate::files::ProjectRootStrategy::Cargo,
        install: None,
        pins: Vec::new(),
    };
    for (requested, expected) in [
        (Duration::from_millis(100), Duration::from_secs(1)),
        (Duration::from_millis(1500), Duration::from_millis(1500)),
    ] {
        let mut service = LspClient::new();
        let started = Instant::now();
        service
            .start_session_absolute_with_policy(crate::language::lsp::LspSessionLaunch {
                session_key: &definition.id,
                definition: &definition,
                executable: sleep.clone(),
                arguments: definition.launch_args.clone(),
                root: workspace.path().into(),
                document: None,
                policy: LspPolicy {
                    request_timeout: requested,
                    maximum_log_lines: 20,
                },
                configuration: crate::language::lsp::LspServerConfiguration::standard(),
                environment: tool_launch_environment(&definition),
            })
            .expect("start");
        let deadline = started + expected + Duration::from_secs(2);
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
        let elapsed = started.elapsed();
        assert!(
            elapsed >= expected && elapsed < expected + Duration::from_secs(2),
            "expected initialization timeout near {expected:?}, got {elapsed:?}"
        );
        service
            .stop(&definition.id)
            .expect("stop timed-out session");
    }
}

#[cfg(target_os = "linux")]
#[cfg(unix)]
#[test]
fn stopping_does_not_wait_for_a_daemon_that_kept_the_servers_pipes() {
    let shell = crate::tools::discover_executable("sh").expect("sh fixture is required");
    let workspace = tempfile::tempdir().expect("workspace");
    let script = workspace.path().join("server.sh");
    // Removing the temporary workspace releases the daemon, even if the test panics.
    std::fs::write(&script, "(printf ready > daemon-ready; while test -d \"$PWD\"; do sleep 0.05; done) &\nexec sleep 30\n").expect("script");
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
    let mut service = LspClient::new();
    service
        .start_session_absolute_with_policy(crate::language::lsp::LspSessionLaunch {
            session_key: &definition.id,
            definition: &definition,
            executable: shell.clone(),
            arguments: definition.launch_args.clone(),
            root: workspace.path().into(),
            document: None,
            policy: LspPolicy {
                request_timeout: Duration::from_secs(10),
                maximum_log_lines: 20,
            },
            configuration: crate::language::lsp::LspServerConfiguration::standard(),
            environment: tool_launch_environment(&definition),
        })
        .expect("start");
    let deadline = Instant::now() + Duration::from_secs(3);
    let origin = loop {
        if let Some(event) = service.try_owned_event() {
            break std::sync::Arc::downgrade(&event.context.session);
        }
        assert!(Instant::now() < deadline, "session did not start");
        thread::sleep(Duration::from_millis(5));
    };
    while !workspace.path().join("daemon-ready").exists() {
        assert!(
            Instant::now() < deadline,
            "daemon did not inherit the pipes"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        origin.upgrade().is_some(),
        "session stopped before the test could stop it"
    );
    let started = Instant::now();
    service.stop("daemonizing").expect("stop");
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "stop submission blocked"
    );
    // The origin is held by the session handle and its workers until cleanup finishes.
    while origin.upgrade().is_some() {
        while service.try_event().is_some() {}
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "session workers did not finish cleanup"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn an_unanswered_feature_request_expires_at_its_configured_deadline() {
    let script = BOUNDARY_SERVER
        .split("m=read()\nsend(")
        .next()
        .unwrap()
        .to_owned()
        + r#"
m=read()
send({'jsonrpc':'2.0','id':m['id'],'result':{'capabilities':{'definitionProvider':True}}})
while read() is not None: pass
"#;
    let timeout = Duration::from_millis(100);
    let (_root, mut service, document) = boundary_service_with_policy(
        &script,
        LspPolicy {
            request_timeout: timeout,
            ..Default::default()
        },
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(Instant::now() < deadline, "server did not initialize");
        match service.try_event() {
            Some(LspEvent::Running { .. }) => break,
            Some(LspEvent::Failed { message, .. }) => panic!("{message}"),
            _ => thread::sleep(Duration::from_millis(5)),
        }
    }
    let started = Instant::now();
    service
        .request(
            "boundary",
            LspFeature::Definition,
            document,
            crate::editor::text::ByteOffset(0),
            13,
        )
        .expect("definition");
    loop {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "request did not expire"
        );
        match service.try_event() {
            Some(LspEvent::RequestFailed {
                request_id: 13,
                method,
                message,
                ..
            }) => {
                assert_eq!(method, "textDocument/definition");
                assert!(message.contains("timed out"));
                assert!(started.elapsed() >= timeout, "request expired too early");
                break;
            }
            Some(LspEvent::Failed { message, .. }) => panic!("{message}"),
            _ => thread::sleep(Duration::from_millis(5)),
        }
    }
    service.stop("boundary").expect("stop");
}

#[test]
fn deterministic_fake_server_proves_lifecycle_sync_and_features() {
    let python = crate::debugger::test_python();
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
    let mut service = LspClient::new();
    service
        .start_session_absolute_with_policy(crate::language::lsp::LspSessionLaunch {
            session_key: &definition.id,
            definition: &definition,
            executable: python.clone(),
            arguments: definition.launch_args.clone(),
            root: workspace.path().into(),
            document: Some(document.clone()),
            policy: LspPolicy {
                request_timeout: Duration::from_secs(5),
                maximum_log_lines: 20,
            },
            configuration: crate::language::lsp::LspServerConfiguration::standard(),
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
                range: crate::language::lsp::LspRange {
                    start: crate::language::lsp::LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: crate::language::lsp::LspPosition {
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
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut hover = false;
    let mut completion = false;
    let mut inlay = false;
    while Instant::now() < deadline && !(hover && completion && inlay) {
        if let Some(LspEvent::Result {
            request_id, result, ..
        }) = service.try_event()
        {
            match (request_id, result) {
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
            LspDocument {
                version: 2,
                text: "a😀z".into(),
                ..document.clone()
            },
            crate::editor::text::ByteOffset(5),
            13,
        )
        .expect("definition");
    let timeout_deadline = Instant::now() + Duration::from_secs(7);
    let mut timed_out = false;
    while Instant::now() < timeout_deadline && !timed_out {
        if let Some(LspEvent::RequestFailed {
            request_id: 13,
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
            crate::language::lsp::LspServerConfiguration::maximum_inlay_hints("rust-analyzer"),
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
    assert_eq!(
        items.iter().map(|item| item.color()).collect::<Vec<_>>(),
        [
            Some((255, 0, 0)),
            Some((0, 255, 0)),
            Some((17, 34, 51)),
            Some((14, 165, 233)),
            None,
        ]
    );
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
    use crate::language::lsp::protocol::normalize_result;
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
    let mut config = crate::language::lsp::LspServerConfiguration::for_server(
        "vscode-json-language-server",
        std::path::Path::new("/server"),
    );
    config.overlay(
        crate::language::lsp::LspServerConfiguration::maximum_inlay_hints(
            "vscode-json-language-server",
        ),
    );
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

pub(super) fn boundary_service(script: &str) -> (tempfile::TempDir, LspClient, LspDocument) {
    boundary_service_with_policy(script, LspPolicy::default())
}

fn boundary_service_with_policy(
    script: &str,
    policy: LspPolicy,
) -> (tempfile::TempDir, LspClient, LspDocument) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("server.py");
    std::fs::write(&path, script).unwrap();
    let mut definition = crate::tools::bundled_tool_catalog()
        .into_iter()
        .find(|d| d.id == "rust-analyzer")
        .unwrap();
    let python = crate::debugger::test_python();
    definition.executable = python.display().to_string();
    let document = LspDocument {
        path: root.path().join("main.rs"),
        language_id: "rust".into(),
        version: 0,
        text: "fn main() {}".into(),
    };
    let mut service = LspClient::new();
    service
        .start_session_absolute_with_policy(crate::language::lsp::LspSessionLaunch {
            session_key: "boundary",
            definition: &definition,
            executable: python,
            arguments: vec!["-u".into(), path.display().to_string()],
            root: root.path().into(),
            document: Some(document.clone()),
            policy,
            configuration: crate::language::lsp::LspServerConfiguration::standard(),
            environment: Default::default(),
        })
        .unwrap();
    (root, service, document)
}
pub(super) const BOUNDARY_SERVER: &str = r#"
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
            Some(LspEvent::Result { request_id: 7, .. }) => break,
            Some(LspEvent::Failed { message, .. } | LspEvent::RequestFailed { message, .. }) => {
                panic!("{message}")
            }
            _ => thread::sleep(Duration::from_millis(1)),
        }
    }
    service.stop("boundary").unwrap();
}
#[test]
fn stderr_keeps_logging_after_invalid_utf8_and_the_startup_allowance() {
    let script = BOUNDARY_SERVER
        .split("m=read()\nsend(")
        .next()
        .unwrap()
        .to_owned()
        + r#"
m=read()
send({'jsonrpc':'2.0','id':m['id'],'result':{'capabilities':{}}})
while True:
 m=read()
 if m is None: break
 if m.get('method')=='textDocument/didOpen':
  sys.stderr.buffer.write(b'invalid \xff\n'+b'startup\n'*201+b'runtime error\n')
  sys.stderr.buffer.flush()
"#;
    let (_root, mut service, _) = boundary_service(&script);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut invalid = false;
    loop {
        assert!(Instant::now() < deadline, "recent stderr was not forwarded");
        match service.try_event() {
            Some(LspEvent::Log { message, .. }) => {
                invalid |= message == "invalid \u{fffd}";
                if message == "runtime error" {
                    break;
                }
            }
            Some(LspEvent::Failed { message, .. }) => panic!("{message}"),
            _ => thread::sleep(Duration::from_millis(1)),
        }
    }
    assert!(invalid);
    service.stop("boundary").unwrap();
}

#[test]
fn a_server_that_stops_reading_is_failed_and_reaped_without_manual_stop() {
    let script = BOUNDARY_SERVER
        .split("m=read()\nsend(")
        .next()
        .unwrap()
        .to_owned()
        + r#"
m=read()
send({'jsonrpc':'2.0','id':m['id'],'result':{'capabilities':{'textDocumentSync':2}}})
pathlib.Path('pid').write_text(str(os.getpid()))
time.sleep(30)
"#;
    // The process checks that read `root` are Linux-only.
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    let (root, mut service, mut document) = boundary_service_with_policy(
        &script,
        LspPolicy {
            request_timeout: Duration::from_secs(1),
            ..LspPolicy::default()
        },
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(service.try_event(), Some(LspEvent::Running { .. })) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    document.version = 1;
    document.text = "x".repeat(2 * 1024 * 1024);
    service.sync("boundary", document).unwrap();
    loop {
        assert!(Instant::now() < deadline, "blocked server never failed");
        if let Some(LspEvent::Failed { message, .. }) = service.try_event() {
            assert!(message.contains("timed out"), "{message}");
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    #[cfg(target_os = "linux")]
    {
        let pid = std::fs::read_to_string(root.path().join("pid")).unwrap();
        while Path::new(&format!("/proc/{pid}")).exists() {
            assert!(Instant::now() < deadline, "timed-out server was not reaped");
            thread::sleep(Duration::from_millis(5));
        }
    }
    service.stop("boundary").unwrap();
}

// The fixture server watches stdin with `select`, which Windows supports only on sockets.
#[cfg(unix)]
#[test]
fn shutdown_waits_for_the_servers_response_before_sending_exit() {
    let script = BOUNDARY_SERVER
        .split("m=read()\nsend(")
        .next()
        .unwrap()
        .to_owned()
        + r#"
import select
m=read()
send({'jsonrpc':'2.0','id':m['id'],'result':{'capabilities':{}}})
while True:
 m=read()
 if m is None: break
 if m.get('method')=='shutdown':
  # No exit may arrive while the server is still completing shutdown.
  assert not select.select([sys.stdin.buffer],[],[],.05)[0]
  send({'jsonrpc':'2.0','id':m['id'],'result':None})
 if m.get('method')=='exit':
  pathlib.Path('shutdown-ok').write_text('ok')
  break
"#;
    let (root, mut service, _) = boundary_service(&script);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(service.try_event(), Some(LspEvent::Running { .. })) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    service.stop("boundary").unwrap();
    while !root.path().join("shutdown-ok").exists() {
        assert!(
            Instant::now() < deadline,
            "shutdown handshake was interrupted"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn stopping_a_server_terminates_its_helpers_even_after_the_server_exits() {
    let script = BOUNDARY_SERVER
        .split("m=read()\nsend(")
        .next()
        .unwrap()
        .to_owned()
        + r#"
import subprocess
helper=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'])
pathlib.Path('helper-pid').write_text(str(helper.pid))
m=read()
send({'jsonrpc':'2.0','id':m['id'],'result':{'capabilities':{}}})
while True:
 m=read()
 if m is None: break
 if m.get('method')=='shutdown':
  send({'jsonrpc':'2.0','id':m['id'],'result':None})
 if m.get('method')=='exit': break
"#;
    let (root, mut service, _) = boundary_service(&script);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(service.try_event(), Some(LspEvent::Running { .. })) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let pid = std::fs::read_to_string(root.path().join("helper-pid")).unwrap();
    service.stop("boundary").unwrap();
    loop {
        let status = std::fs::read_to_string(format!("/proc/{pid}/stat"));
        if status.is_err() || status.unwrap().split_whitespace().nth(2) == Some("Z") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "language server helper survived shutdown"
        );
        thread::sleep(Duration::from_millis(5));
    }
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
send({'jsonrpc':'2.0','id':m['id'],'result':{'capabilities':{'hoverProvider':True,'textDocumentSync':1}}})
pathlib.Path('pid').write_text(str(os.getpid()))
time.sleep(30)
"#;
    // The process checks that read `root` are Linux-only.
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
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
fn failed_request_writes_report_the_original_request_id() {
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
                request_id: 91,
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

#[cfg(unix)]
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
    let mut service = LspClient::new();
    service
        .start_session_absolute_with_policy(crate::language::lsp::LspSessionLaunch {
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
            configuration: crate::language::lsp::LspServerConfiguration::standard(),
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
