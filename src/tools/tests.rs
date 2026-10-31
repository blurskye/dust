use std::io::Write;

use reqwest::Url;

use super::{
    catalog::{
        ToolArchiveFormat, ToolInstall, ToolKind, bundled_tool_catalog, discover_executable,
        managed_tool_installed_version_at, nix_lsp_launch_environment_at, nix_lsp_runtime_at,
        nix_lsp_shell, nixos_os_release_at, tool_launch_environment_at, write_managed_tool_version,
    },
    compatibility::{elf_interpreter, runtime_artifact},
    install::install_tool,
    install_prepare::{
        archive_entry_matches, prepare_nix_lsp_workspace_at, relative_catalog_path,
        validate_tool_id,
    },
    installer_service::{ToolInstaller, ToolOperation},
    mason_catalog::mason_snapshot_counts,
    package_lock::valid_digest,
    runtimes::{install_nix_lsp_profile, uninstall_managed_tool, uninstall_nix_lsp_profile},
    version::{ToolVersionRequest, probe_tool_version},
};

#[test]
fn archive_entry_matching_accepts_only_an_optional_leading_current_directory() {
    assert!(archive_entry_matches(
        std::path::Path::new("./emmylua_ls"),
        std::path::Path::new("emmylua_ls")
    ));
    assert!(archive_entry_matches(
        std::path::Path::new("bin/server"),
        std::path::Path::new("bin/server")
    ));
    assert!(!archive_entry_matches(
        std::path::Path::new("../emmylua_ls"),
        std::path::Path::new("emmylua_ls")
    ));
    assert!(!archive_entry_matches(
        std::path::Path::new("/emmylua_ls"),
        std::path::Path::new("emmylua_ls")
    ));
}

#[test]
fn bundled_catalog_is_typed_and_has_real_language_and_debug_tools() {
    let catalog = bundled_tool_catalog();
    assert!(catalog.iter().any(|tool| tool.id == "rust-analyzer"));
    assert!(catalog.iter().any(|tool| tool.kind == ToolKind::Dap));
    // Agent adapters serve the agent panel, not a language.
    assert!(
        catalog
            .iter()
            .all(|tool| (tool.kind == ToolKind::Agent) == tool.languages.is_empty())
    );
    assert_eq!(catalog.iter().filter(|tool| tool.kind == ToolKind::Agent).count(), 3);
    assert!(
        catalog
            .iter()
            .filter(|tool| tool.kind == ToolKind::Lsp && tool.install.is_some())
            .all(|tool| nix_lsp_shell(tool).is_some())
    );
    assert_eq!(
        catalog
            .iter()
            .filter(|tool| tool.kind == ToolKind::Lsp && tool.install.is_some())
            .count(),
        19
    );
    assert!(
        catalog
            .iter()
            .filter(|tool| tool.kind == ToolKind::Lsp)
            .count()
            > 250
    );
    assert!(catalog.iter().any(|tool| matches!(
        tool.install,
        Some(ToolInstall::Archive { .. } | ToolInstall::NpmPackage { .. })
    )));
    assert!(catalog.iter().all(|tool| {
        tool.install.is_some()
            || tool.kind == ToolKind::Lsp
            || matches!(tool.id.as_str(), "lldb-dap" | "js-debug-adapter")
    }));
}

#[test]
fn pinned_mason_snapshot_has_complete_server_and_filetype_inventories() {
    let (servers, extensions, filenames) = mason_snapshot_counts();
    assert_eq!(servers, 292);
    assert_eq!(extensions, 1_231);
    assert_eq!(filenames, 407);
    assert!(
        bundled_tool_catalog()
            .iter()
            .filter(|tool| tool.kind == ToolKind::Lsp)
            .count()
            > 250
    );
}

#[test]
fn wildcard_mason_servers_are_explicit_only() {
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "efm")
        .expect("generic EFM definition");
    assert!(definition.supports_language("rust"));
    assert!(definition.supports_language("markdown"));
    assert!(!definition.automatically_supports_language("rust"));
}

#[test]
fn rust_analyzer_catalog_exposes_distinct_exact_installable_pins() {
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("rust-analyzer catalog entry");
    let versions = definition.managed_versions();
    assert_eq!(
        versions
            .iter()
            .map(|version| (
                version.version.as_deref().unwrap_or_default(),
                version.release.as_deref().unwrap_or_default(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("0.3.3025", "2026-08-24"),
            ("0.3.3016", "2026-08-17.4"),
            ("0.3.3008", "2026-08-10.1"),
            ("0.3.2997", "2026-08-03"),
            ("0.3.2989", "2026-07-27"),
        ]
    );
    assert!(versions.iter().all(|version| matches!(
        version.install,
        Some(ToolInstall::Archive { ref url, ref sha256, .. })
            if url.contains(version.release.as_deref().unwrap_or_default())
                && valid_digest(sha256)
    )));
}

#[test]
fn managed_tool_records_the_exact_activated_catalog_version() {
    let root = tempfile::tempdir().expect("tool store");
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("rust-analyzer catalog entry")
        .managed_version("2026-08-10.1")
        .expect("exact pin");
    let destination = root.path().join(&definition.id);
    std::fs::create_dir(&destination).expect("managed tool directory");
    write_managed_tool_version(&destination, &definition).expect("version marker");
    assert_eq!(definition.version.as_deref(), Some("0.3.3008"));
    assert_eq!(
        managed_tool_installed_version_at(root.path(), &definition).as_deref(),
        Some("0.3.3008")
    );
}

#[test]
fn catalog_uses_dust_managed_installation_methods_not_system_programs() {
    let catalog = bundled_tool_catalog();
    assert!(catalog.iter().all(|tool| !matches!(
        tool.install,
        Some(ToolInstall::Archive { ref url, .. }) if !url.starts_with("https://")
    )));
    assert!(
        catalog
            .iter()
            .all(|tool| tool.install.as_ref().is_none_or(|install| !matches!(
                install,
                ToolInstall::Archive { .. }
            ) || !matches!(
                tool.id.as_str(),
                "rustfmt" | "clippy"
            )))
    );
}

#[test]
fn javascript_tools_prepend_their_private_bun_backed_node_runtime() {
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "pyright")
        .expect("pyright catalog entry");
    let directory = tempfile::tempdir().expect("tool store");
    let bin = directory.path().join("pyright/bin");
    std::fs::create_dir_all(&bin).expect("tool bin");
    std::fs::write(bin.join("node"), b"test runtime").expect("private node runtime");
    let environment = tool_launch_environment_at(directory.path(), &definition);
    let path = environment.get("PATH").expect("JavaScript PATH");
    assert_eq!(
        std::env::split_paths(path).next(),
        Some(bin),
        "the package shebang must resolve node to Dust's Bun runtime"
    );
}

#[test]
fn nixos_host_detection_accepts_only_the_exact_os_release_id() {
    let directory = tempfile::tempdir().expect("os release directory");
    let os_release = directory.path().join("os-release");
    std::fs::write(&os_release, "NAME=NixOS\nID=\"nixos\"\n").expect("NixOS release");
    assert!(nixos_os_release_at(&os_release));
    std::fs::write(&os_release, "ID_LIKE=nixos\nID=ubuntu\n").expect("other release");
    assert!(!nixos_os_release_at(&os_release));
}

#[test]
fn private_nix_workspace_contains_only_dust_pinned_flake_inputs() {
    let directory = tempfile::tempdir().expect("tool store");
    let workspace = prepare_nix_lsp_workspace_at(&directory.path().join("nix-workspace"))
        .expect("Nix workspace");
    let flake = std::fs::read_to_string(workspace.join("flake.nix")).expect("flake source");
    let lock = std::fs::read_to_string(workspace.join("flake.lock")).expect("flake lock");
    assert!(workspace.join("profiles").is_dir());
    assert!(flake.contains("Dust-owned Linux executable compatibility runtime"));
    assert!(flake.contains("compat-runtime"));
    assert!(!flake.contains("rust-analyzer ="));
    assert!(lock.contains("b6018f87da91d19d0ab4cf979885689b469cdd41"));
}

#[test]
fn bun_bootstrap_is_the_pinned_glibc_baseline_artifact() {
    let bun = runtime_artifact("bun").expect("Bun runtime artifact");
    assert_eq!(
        bun.url,
        "https://github.com/oven-sh/bun/releases/download/bun-v1.3.14/bun-linux-x64-baseline.zip"
    );
    assert_eq!(
        bun.sha256,
        "a063908ae08b7852ca10939bbdc6ceed3ddabce8fb9402dce83d65d73b36e6c7"
    );
    assert_eq!(bun.archive, ToolArchiveFormat::Zip);
    assert_eq!(bun.executable_path, "bun-linux-x64-baseline/bun");
}

#[test]
#[ignore = "operator qualification: requires Nix plus upstream artifact network access"]
fn nixos_runtime_installs_patches_and_runs_the_verified_upstream_artifact() {
    if !super::is_nixos_host() {
        return;
    }
    let nix = discover_executable("nix").expect("Nix executable");
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("rust-analyzer catalog entry")
        .managed_version("0.3.3016")
        .expect("exact upstream pin");
    let directory = tempfile::tempdir().expect("tool store");
    let (_, cancellations) = std::sync::mpsc::channel();
    let mut report = |_: &str, _: u64, _: Option<u64>| {};
    let installed = install_nix_lsp_profile(
        &definition,
        directory.path(),
        &nix,
        &cancellations,
        &mut report,
    )
    .expect("Dust via Nix install")
    .expect("installed executable");
    let runtime =
        nix_lsp_runtime_at(directory.path(), &definition).expect("private compatibility runtime");
    assert_eq!(
        elf_interpreter(&installed).expect("ELF interpreter"),
        Some(runtime.loader.display().to_string())
    );
    let output = std::process::Command::new(&installed)
        .arg("--version")
        .env_clear()
        .envs(runtime.environment())
        .output()
        .expect("launch patched upstream executable");
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("0.3.3016"));
}

#[test]
#[ignore = "operator qualification: requires Nix and all catalog LSP upstream networks"]
fn nixos_runtime_installs_and_probes_every_managed_catalog_lsp() {
    if !super::is_nixos_host() {
        return;
    }
    let nix = discover_executable("nix").expect("Nix executable");
    let definitions = bundled_tool_catalog()
        .into_iter()
        .filter(|tool| tool.kind == ToolKind::Lsp && tool.install.is_some())
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 19);
    let directory = tempfile::tempdir().expect("tool store");
    let (_, cancellations) = std::sync::mpsc::channel();
    let mut report = |_: &str, _: u64, _: Option<u64>| {};
    for definition in definitions {
        let installed = install_nix_lsp_profile(
            &definition,
            directory.path(),
            &nix,
            &cancellations,
            &mut report,
        )
        .unwrap_or_else(|error| panic!("install {}: {error}", definition.id))
        .expect("installed executable");
        let environment = nix_lsp_launch_environment_at(directory.path(), &definition)
            .expect("private launch environment");
        if definition.probe_args.is_empty() {
            let mut child = std::process::Command::new(&installed)
                .args(&definition.launch_args)
                .current_dir(directory.path())
                .env_clear()
                .envs(&environment)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap_or_else(|error| panic!("launch {}: {error}", definition.id));
            std::thread::sleep(std::time::Duration::from_millis(250));
            assert!(
                child.try_wait().expect("launch status").is_none(),
                "{} exited during launch smoke",
                definition.id
            );
            child.kill().expect("stop launch smoke");
            child.wait().expect("reap launch smoke");
        } else {
            let version = probe_tool_version(
                ToolVersionRequest {
                    id: definition.id.clone(),
                    executable: installed,
                    arguments: definition.probe_args.clone(),
                    environment,
                    current_directory: Some(directory.path().to_path_buf()),
                    timeout: std::time::Duration::from_secs(10),
                },
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap_or_else(|error| panic!("probe {}: {error}", definition.id));
            assert!(!version.is_empty(), "{} version", definition.id);
        }
    }
}

fn write_lsp_message(
    stdin: &mut std::process::ChildStdin,
    message: &serde_json::Value,
) -> Result<(), String> {
    let body = serde_json::to_vec(message).map_err(|error| error.to_string())?;
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len())
        .and_then(|()| stdin.write_all(&body))
        .and_then(|()| stdin.flush())
        .map_err(|error| format!("write LSP frame: {error}"))
}

fn lsp_reader(
    stdout: std::process::ChildStdout,
    messages: std::sync::mpsc::Sender<Result<serde_json::Value, String>>,
) {
    let mut reader = std::io::BufReader::new(stdout);
    loop {
        let mut content_length = None;
        loop {
            let mut line = String::new();
            match std::io::BufRead::read_line(&mut reader, &mut line) {
                Ok(0) => return,
                Ok(_) if line == "\r\n" || line == "\n" => break,
                Ok(_) => {
                    if let Some(value) = line
                        .trim()
                        .strip_prefix("Content-Length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                    {
                        content_length = Some(value);
                    }
                }
                Err(error) => {
                    let _ = messages.send(Err(format!("read LSP header: {error}")));
                    return;
                }
            }
        }
        let Some(length) = content_length.filter(|length| *length <= 8 * 1024 * 1024) else {
            let _ = messages.send(Err("invalid LSP Content-Length".into()));
            return;
        };
        let mut body = vec![0_u8; length];
        if let Err(error) = std::io::Read::read_exact(&mut reader, &mut body) {
            let _ = messages.send(Err(format!("read LSP body: {error}")));
            return;
        }
        let message =
            serde_json::from_slice(&body).map_err(|error| format!("decode LSP JSON: {error}"));
        if messages.send(message).is_err() {
            return;
        }
    }
}

fn await_lsp_response(
    id: u64,
    stdin: &mut std::process::ChildStdin,
    messages: &std::sync::mpsc::Receiver<Result<serde_json::Value, String>>,
) -> Result<serde_json::Value, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut observed = Vec::new();
    for _ in 0..2048 {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!("LSP response {id} timed out after {observed:?}"));
        }
        let message = match messages.recv_timeout(remaining) {
            Ok(message) => message?,
            Err(error) => {
                return Err(format!(
                    "LSP response {id} unavailable after {observed:?}: {error}"
                ));
            }
        };
        if let Some(method) = message.get("method").and_then(serde_json::Value::as_str) {
            observed.push(method.to_owned());
        }
        if message.get("id").and_then(serde_json::Value::as_u64) == Some(id)
            && message.get("method").is_none()
        {
            return Ok(message);
        }
        if let (Some(request_id), Some(method)) = (
            message.get("id"),
            message.get("method").and_then(serde_json::Value::as_str),
        ) {
            let result = if method == "workspace/configuration" {
                let count = message
                    .pointer("/params/items")
                    .and_then(serde_json::Value::as_array)
                    .map_or(0, Vec::len);
                serde_json::Value::Array(vec![serde_json::Value::Null; count])
            } else {
                serde_json::Value::Null
            };
            write_lsp_message(
                stdin,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "result": result,
                }),
            )?;
        }
    }
    Err(format!("LSP response {id} exceeded the message ceiling"))
}

#[test]
#[ignore = "operator qualification: installs all catalog LSPs and performs live protocol I/O"]
fn nixos_runtime_completes_protocol_smoke_for_every_managed_catalog_lsp() {
    if !super::is_nixos_host() {
        return;
    }
    let nix = discover_executable("nix").expect("Nix executable");
    let definitions = bundled_tool_catalog()
        .into_iter()
        .filter(|tool| tool.kind == ToolKind::Lsp && tool.install.is_some())
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 19);
    let directory = tempfile::tempdir().expect("tool store");
    let (_, cancellations) = std::sync::mpsc::channel();
    let mut report = |_: &str, _: u64, _: Option<u64>| {};
    for definition in definitions {
        let installed = install_nix_lsp_profile(
            &definition,
            directory.path(),
            &nix,
            &cancellations,
            &mut report,
        )
        .unwrap_or_else(|error| panic!("install {}: {error}", definition.id))
        .expect("installed executable");
        let project = directory.path().join("projects").join(&definition.id);
        std::fs::create_dir_all(project.join("src")).expect("project directory");
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname='dust_protocol_smoke'\nversion='0.1.0'\nedition='2024'\n",
        )
        .expect("Cargo marker");
        std::fs::write(project.join("package.json"), "{}\n").expect("Node marker");
        std::fs::write(project.join("tsconfig.json"), "{}\n").expect("TS marker");
        std::fs::write(
            project.join("pyproject.toml"),
            "[project]\nname='smoke'\nversion='0.1.0'\n",
        )
        .expect("Python marker");
        std::fs::write(
            project.join("go.mod"),
            "module example.com/dust-smoke\ngo 1.24\n",
        )
        .expect("Go marker");
        std::fs::write(project.join(".clangd"), "CompileFlags: {}\n").expect("clangd marker");
        let (language, source_name, text) = match definition.id.as_str() {
            "rust-analyzer" => (
                "rust",
                "src/main.rs",
                "fn main() { let value = 1; value; }\n",
            ),
            "typescript-language-server" => ("typescript", "main.ts", "const value = 1;\nvalue;\n"),
            "pyright" => ("python", "main.py", "value = 1\nvalue\n"),
            "clangd" => ("c", "main.c", "int main(void) { return 0; }\n"),
            "gopls" => (
                "go",
                "main.go",
                "package main\nfunc main() { var value = 1; _ = value }\n",
            ),
            "bash-language-server" => ("shellscript", "main.sh", "#!/bin/sh\necho value\n"),
            "vscode-html-language-server" => ("html", "index.html", "<main>value</main>\n"),
            "vscode-css-language-server" => ("css", "style.css", "main { color: red; }\n"),
            "vscode-json-language-server" => ("json", "config.json", "{\"value\": 1}\n"),
            "yaml-language-server" => ("yaml", "config.yaml", "value: 1\n"),
            "marksman" => ("markdown", "README.md", "# Value\n\ntext\n"),
            "emmylua-ls" => ("lua", "main.lua", "local value = 1\nprint(value)\n"),
            "intelephense" => ("php", "main.php", "<?php\n$value = 1;\necho $value;\n"),
            "svelte-language-server" => (
                "svelte",
                "App.svelte",
                "<script>let value = 1;</script>\n{value}\n",
            ),
            "vue-language-server" => (
                "vue",
                "App.vue",
                "<script setup>const value = 1</script>\n<template>{{ value }}</template>\n",
            ),
            "dockerfile-language-server" => ("dockerfile", "Dockerfile", "FROM scratch\n"),
            "astro-language-server" => (
                "astro",
                "index.astro",
                "---\nconst value = 1;\n---\n<main>{value}</main>\n",
            ),
            other => panic!("unexpected catalog LSP {other}"),
        };
        let source = project.join(source_name);
        if let Some(parent) = source.parent() {
            std::fs::create_dir_all(parent).expect("source parent");
        }
        std::fs::write(&source, text).expect("source fixture");
        let root_uri = Url::from_file_path(&project).expect("root URI").to_string();
        let source_uri = Url::from_file_path(&source)
            .expect("source URI")
            .to_string();
        let environment = nix_lsp_launch_environment_at(directory.path(), &definition)
            .expect("private launch environment");
        if matches!(definition.install, Some(ToolInstall::NpmPackage { .. })) {
            let node =
                std::fs::canonicalize(directory.path().join(&definition.id).join("bin/node"))
                    .expect("private Node runtime");
            assert!(
                node.starts_with("/nix/store") && node.to_string_lossy().contains("nodejs"),
                "{} did not select pinned Nix Node: {}",
                definition.id,
                node.display()
            );
        }
        let mut child = std::process::Command::new(&installed)
            .args(&definition.launch_args)
            .current_dir(&project)
            .envs(environment)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap_or_else(|error| panic!("launch {}: {error}", definition.id));
        let mut stdin = child.stdin.take().expect("LSP stdin");
        let stdout = child.stdout.take().expect("LSP stdout");
        let (messages_tx, messages_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || lsp_reader(stdout, messages_tx));
        let configuration =
            crate::language::lsp::LspServerConfiguration::for_server(&definition.id, &installed);
        let mut initialize_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "textDocument": { "completion": { "completionItem": {} } },
                    "workspace": { "configuration": true, "workspaceFolders": true }
                },
                "workspaceFolders": [{ "uri": root_uri, "name": "smoke" }]
            }
        });
        if let Some(options) = configuration.initialization_options() {
            initialize_request["params"]["initializationOptions"] = options.clone();
        }
        write_lsp_message(&mut stdin, &initialize_request).expect("initialize frame");
        let initialize = await_lsp_response(1, &mut stdin, &messages_rx)
            .unwrap_or_else(|error| panic!("initialize {}: {error}", definition.id));
        assert!(
            initialize.get("error").is_none(),
            "{}: {initialize}",
            definition.id
        );
        write_lsp_message(
            &mut stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
        )
        .expect("initialized frame");
        write_lsp_message(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "workspace/didChangeConfiguration",
                "params": { "settings": {} }
            }),
        )
        .expect("initial configuration frame");
        write_lsp_message(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": { "textDocument": {
                    "uri": source_uri, "languageId": language, "version": 1, "text": text
                }}
            }),
        )
        .expect("didOpen frame");
        if definition.id != "vue-language-server" {
            write_lsp_message(
                &mut stdin,
                &serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "textDocument/completion",
                    "params": {
                        "textDocument": { "uri": source_uri },
                        "position": { "line": 0, "character": 0 },
                        "context": { "triggerKind": 1 }
                    }
                }),
            )
            .expect("completion frame");
            let completion = await_lsp_response(2, &mut stdin, &messages_rx)
                .unwrap_or_else(|error| panic!("completion {}: {error}", definition.id));
            assert!(
                completion.get("error").is_none(),
                "{}: {completion}",
                definition.id
            );
            assert!(
                completion.get("result").is_some(),
                "{}: {completion}",
                definition.id
            );
        }
        write_lsp_message(
            &mut stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": null }),
        )
        .expect("shutdown frame");
        let shutdown = await_lsp_response(3, &mut stdin, &messages_rx)
            .unwrap_or_else(|error| panic!("shutdown {}: {error}", definition.id));
        assert!(
            shutdown.get("error").is_none(),
            "{}: {shutdown}",
            definition.id
        );
        write_lsp_message(
            &mut stdin,
            &serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
        )
        .expect("exit frame");
        drop(stdin);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if child.try_wait().expect("LSP status").is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().expect("kill LSP after exit timeout");
                child.wait().expect("reap LSP");
                panic!("{} did not exit after shutdown", definition.id);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        reader.join().expect("LSP reader");
    }
}

#[test]
#[ignore = "operator qualification: requires Bun and npm-registry network access"]
fn managed_bun_installs_and_launches_an_exact_javascript_tool_without_node_or_npm() {
    if discover_executable("bun").is_none() {
        return;
    }
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "prettier")
        .expect("prettier catalog entry");
    let directory = tempfile::tempdir().expect("tool store");
    let (_, cancellations) = std::sync::mpsc::channel();
    let mut report = |_: &str, _: u64, _: Option<u64>| {};
    let installed = install_tool(
        &definition,
        directory.path(),
        &cancellations,
        &mut report,
        None,
    )
    .expect("managed Bun install")
    .expect("tool executable");
    let bin = installed.parent().expect("tool bin");
    assert!(bin.join("node").is_file());
    let status = std::process::Command::new(&installed)
        .arg("--version")
        .env_clear()
        .env("HOME", directory.path())
        .env("PATH", bin)
        .status()
        .expect("launch Bun-backed JavaScript executable");
    assert!(status.success());
}

#[test]
fn archive_paths_reject_absolute_parent_and_platform_escape_forms() {
    for path in ["", "../tool", "/tool", "nested/../tool", "nested\\tool"] {
        assert!(relative_catalog_path(path).is_err(), "{path}");
    }
    assert_eq!(
        relative_catalog_path("release/bin/tool").unwrap(),
        std::path::PathBuf::from("release/bin/tool")
    );
    for id in [
        "",
        "../server",
        "nested/server",
        ".",
        "server.tool",
        "server tool",
    ] {
        assert!(validate_tool_id(id).is_err(), "unsafe tool ID: {id}");
    }
    assert!(validate_tool_id("typescript-language-server").is_ok());
}

#[test]
fn installer_worker_routes_real_managed_uninstall_and_reports_its_operation() {
    let directory = tempfile::tempdir().expect("tool store");
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("catalog tool");
    let selected = directory.path().join("rust-analyzer/bin");
    std::fs::create_dir_all(&selected).expect("selected tool");
    std::fs::write(selected.join("rust-analyzer"), b"tool").expect("selected executable");
    let installer = ToolInstaller::start_at(directory.path().to_path_buf());

    installer.uninstall(definition).expect("queue uninstall");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let result = loop {
        if let Some(result) = installer.try_result().expect("installer result") {
            break result;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "uninstall worker timed out"
        );
        std::thread::yield_now();
    };

    assert_eq!(result.operation, ToolOperation::UninstallManaged);
    assert!(result.result.is_ok());
    assert!(!directory.path().join("rust-analyzer").exists());
}

#[cfg(unix)]
#[test]
fn installer_worker_can_explicitly_add_rust_analyzer_to_the_active_rustup_toolchain() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("tool store");
    let rustup = directory.path().join("rustup");
    std::fs::write(
        &rustup,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$PWD/.rustup-args\"\n",
    )
    .expect("rustup fixture");
    std::fs::set_permissions(&rustup, std::fs::Permissions::from_mode(0o755))
        .expect("rustup fixture permissions");
    let proxy = directory.path().join("rust-analyzer");
    std::fs::hard_link(&rustup, &proxy).expect("rustup proxy");
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("catalog tool");
    let installer = ToolInstaller::start_at(directory.path().join("managed"));

    installer
        .install_rustup_component(
            definition,
            rustup,
            proxy.clone(),
            directory.path().to_path_buf(),
            "rust-analyzer".into(),
        )
        .expect("queue rustup component install");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let result = loop {
        if let Some(result) = installer.try_result().expect("installer result") {
            break result;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "rustup installer worker timed out"
        );
        std::thread::yield_now();
    };

    assert_eq!(result.operation, ToolOperation::InstallRustup);
    assert_eq!(result.result.expect("rustup install"), Some(proxy));
    assert_eq!(
        std::fs::read_to_string(directory.path().join(".rustup-args")).expect("rustup arguments"),
        "component\nadd\nrust-analyzer\n"
    );
}

#[test]
fn managed_uninstall_atomically_removes_only_the_selected_tool() {
    let directory = tempfile::tempdir().expect("tool store");
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("catalog tool");
    let selected = directory.path().join("rust-analyzer/bin");
    let sibling = directory.path().join("clangd/bin");
    std::fs::create_dir_all(&selected).expect("selected tool");
    std::fs::create_dir_all(&sibling).expect("sibling tool");
    std::fs::write(selected.join("rust-analyzer"), b"tool").expect("selected executable");
    std::fs::write(sibling.join("clangd"), b"sibling").expect("sibling executable");

    uninstall_managed_tool(&definition, directory.path()).expect("uninstall");

    assert!(!directory.path().join("rust-analyzer").exists());
    assert!(sibling.join("clangd").is_file());
}

#[cfg(unix)]
#[test]
fn pinned_nix_uninstall_removes_only_its_profile_generations() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().expect("tool store");
    let profiles = directory.path().join("nix-lsp-workspace/profiles");
    std::fs::create_dir_all(&profiles).expect("profiles");
    let target = directory.path().join("target");
    std::fs::write(&target, b"profile").expect("target");
    for name in [
        "rust-analyzer",
        "rust-analyzer-1-link",
        "rust-analyzer-2-link",
        "clangd",
    ] {
        symlink(&target, profiles.join(name)).expect("profile link");
    }
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .expect("catalog tool");

    uninstall_nix_lsp_profile(&definition, directory.path()).expect("uninstall profile");

    assert!(!profiles.join("rust-analyzer").exists());
    assert!(!profiles.join("rust-analyzer-1-link").exists());
    assert!(!profiles.join("rust-analyzer-2-link").exists());
    assert!(profiles.join("clangd").exists());
}

#[cfg(unix)]
#[test]
fn version_probe_is_bounded_when_a_descendant_keeps_output_open() {
    let shell = discover_executable("sh").expect("shell fixture");
    let started = std::time::Instant::now();
    let result = probe_tool_version(
        ToolVersionRequest {
            id: "hostile".into(),
            executable: shell,
            arguments: vec!["-c".into(), "(sleep 5) & printf 'server 1.2.3\\n'".into()],
            environment: std::collections::BTreeMap::new(),
            current_directory: None,
            timeout: std::time::Duration::from_secs(1),
        },
        &std::sync::atomic::AtomicBool::new(false),
    );
    assert!(
        result
            .as_ref()
            .is_err_and(|error| error.contains("remained open")),
        "{result:?}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn current_test_process_is_discoverable_without_a_shell() {
    let executable = std::env::current_exe().expect("current executable");
    assert_eq!(
        discover_executable(executable.to_str().expect("UTF-8 path")),
        Some(executable)
    );
}
