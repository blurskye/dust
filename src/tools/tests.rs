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
    assert_eq!(
        catalog
            .iter()
            .filter(|tool| tool.kind == ToolKind::Agent)
            .count(),
        3
    );
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
        24
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
    assert_eq!(servers, 291);
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
    for id in ["pyright", "pi-acp"] {
        let definition = bundled_tool_catalog()
            .into_iter()
            .find(|tool| tool.id == id)
            .unwrap();
        let directory = tempfile::tempdir().expect("tool store");
        let bin = directory.path().join(id).join("bin");
        std::fs::create_dir_all(&bin).expect("tool bin");
        std::fs::write(bin.join("node"), b"test runtime").expect("private node runtime");
        let environment = tool_launch_environment_at(directory.path(), &definition);
        let path = environment.get("PATH").expect("JavaScript PATH");
        assert_eq!(std::env::split_paths(path).next(), Some(bin));
    }
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
    let version = probe_isolated_tool_version(ToolVersionRequest {
        id: definition.id,
        executable: installed,
        arguments: vec!["--version".into()],
        environment: runtime.environment(),
        current_directory: Some(directory.path().into()),
        timeout: std::time::Duration::from_secs(10),
    })
    .expect("launch patched upstream executable");
    assert!(version.contains("0.3.3016"), "{version}");
}

#[test]
#[ignore = "operator qualification: requires Nix and all catalog LSP upstream networks"]
fn nixos_runtime_installs_and_probes_every_managed_catalog_lsp() {
    if !super::is_nixos_host() {
        return;
    }
    let nix = discover_executable("nix").expect("Nix executable");
    let definitions = managed_lsp_definitions();
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
            lsp_protocol_smoke(&definition, &installed, directory.path());
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

const MANAGED_LSP_FIXTURES: [(&str, &str, &str, &str); 24] = [
    ("rust-analyzer", "rust", "src/main.rs", "fn main() { let value = 1; value; }\n"),
    ("typescript-language-server", "typescript", "main.ts", "const value = 1;\nvalue;\n"),
    ("pyright", "python", "main.py", "value = 1\nvalue\n"),
    ("clangd", "c", "main.c", "int main(void) { return 0; }\n"),
    ("gopls", "go", "main.go", "package main\nfunc main() { var value = 1; _ = value }\n"),
    ("bash-language-server", "shellscript", "main.sh", "#!/bin/sh\necho value\n"),
    ("vscode-html-language-server", "html", "index.html", "<main>value</main>\n"),
    ("tailwindcss-language-server", "html", "index.html", "<main class=\"flex\">value</main>\n"),
    ("taplo", "toml", "config.toml", "value = 1\n"),
    ("vscode-css-language-server", "css", "style.css", "main { color: red; }\n"),
    ("vscode-json-language-server", "json", "config.json", "{\"value\": 1}\n"),
    ("yaml-language-server", "yaml", "config.yaml", "value: 1\n"),
    ("marksman", "markdown", "README.md", "# Value\n\ntext\n"),
    ("emmylua-ls", "lua", "main.lua", "local value = 1\nprint(value)\n"),
    ("intelephense", "php", "main.php", "<?php\n$value = 1;\necho $value;\n"),
    ("svelte-language-server", "svelte", "App.svelte", "<script>let value = 1;</script>\n{value}\n"),
    ("vue-language-server", "vue", "App.vue", "<script setup>const value = 1</script>\n<template>{{ value }}</template>\n"),
    ("dockerfile-language-server", "dockerfile", "Dockerfile", "FROM scratch\n"),
    ("astro-language-server", "astro", "index.astro", "---\nconst value = 1;\n---\n<main>{value}</main>\n"),
    ("biome", "javascript", "main.js", "const value = 1;\nvalue;\n"),
    ("oxlint", "javascript", "main.js", "const value = 1;\nvalue;\n"),
    ("oxfmt", "javascript", "main.js", "const value = 1;\nvalue;\n"),
    ("tsgo", "typescript", "main.ts", "const value = 1;\nvalue;\n"),
    ("ruff", "python", "main.py", "value = 1\nvalue\n"),
];

fn managed_lsp_definitions() -> Vec<super::catalog::ToolDefinition> {
    let definitions = bundled_tool_catalog()
        .into_iter()
        .filter(|tool| tool.kind == ToolKind::Lsp && tool.install.is_some())
        .collect::<Vec<_>>();
    let mut actual = definitions.iter().map(|tool| tool.id.as_str()).collect::<Vec<_>>();
    let mut expected = MANAGED_LSP_FIXTURES.iter().map(|fixture| fixture.0).collect::<Vec<_>>();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(actual, expected, "managed LSP fixtures must match the catalog before installation");
    definitions
}

#[test]
fn managed_lsp_protocol_fixtures_cover_the_exact_catalog_inventory() {
    managed_lsp_definitions();
}

// Keep qualification launches independent of the developer's environment while
// using the same bounded version probe as the tool manager. `env` execs the tool.
fn probe_isolated_tool_version(mut request: ToolVersionRequest) -> Result<String, String> {
    let env = discover_executable("env").ok_or("env executable unavailable")?;
    let env = std::path::absolute(env).map_err(|error| error.to_string())?;
    let mut arguments = vec!["-i".into()];
    arguments.extend(request.environment.iter().map(|(key, value)| format!("{key}={value}")));
    arguments.push(request.executable.to_str().ok_or("non-UTF-8 tool path")?.into());
    arguments.append(&mut request.arguments);
    request.executable = env;
    request.arguments = arguments;
    request.environment.clear();
    probe_tool_version(request, &std::sync::atomic::AtomicBool::new(false))
}

#[cfg(unix)]
#[test]
fn isolated_version_probe_does_not_inherit_the_developer_environment() {
    let directory = tempfile::tempdir().unwrap();
    let version = probe_isolated_tool_version(ToolVersionRequest {
        id: "isolated".into(),
        executable: "/bin/sh".into(),
        arguments: vec!["-c".into(), "test -z \"${HOME+x}\" && printf '%s' \"$DUST_PROBE_FIXTURE\"".into()],
        environment: std::collections::BTreeMap::from([("DUST_PROBE_FIXTURE".into(), "isolated".into())]),
        current_directory: Some(directory.path().into()),
        timeout: std::time::Duration::from_secs(10),
    }).unwrap();
    assert_eq!(version, "isolated");
}

fn write_lsp_message(
    stdin: &mut std::process::ChildStdin,
    message: &serde_json::Value,
) -> Result<(), String> {
    crate::process::write_content_length_message(stdin, message)
        .map_err(|error| error.to_string())
}

fn lsp_reader(
    stdout: impl std::io::Read,
    messages: std::sync::mpsc::Sender<Result<serde_json::Value, String>>,
) {
    let mut reader = std::io::BufReader::new(stdout);
    loop {
        match crate::process::read_content_length_message(&mut reader) {
            Ok(Some(message)) => {
                if messages.send(Ok(message)).is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                let _ = messages.send(Err(error.to_string()));
                return;
            }
        }
    }
}

struct LspSmokeProcess {
    child: std::process::Child,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl LspSmokeProcess {
    fn spawn(command: &mut std::process::Command) -> (
        Self,
        std::sync::mpsc::Receiver<Result<serde_json::Value, String>>,
    ) {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let child = command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn().expect("launch LSP smoke");
        let mut process = Self {
            child,
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reader: None,
        };
        let stdout = crate::process::CancellablePipe::new(
            process.child.stdout.take().expect("LSP stdout"),
            process.cancelled.clone(),
        ).expect("cancellable LSP stdout");
        let (sender, receiver) = std::sync::mpsc::channel();
        process.reader = Some(std::thread::spawn(move || lsp_reader(stdout, sender)));
        (process, receiver)
    }
}

impl Drop for LspSmokeProcess {
    fn drop(&mut self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::Release);
        #[cfg(unix)]
        crate::process::terminate_process_group(self.child.id());
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn lsp_smoke_reader_accepts_lowercase_headers_and_shared_framing() {
    let message = serde_json::json!({"id": 1, "result": {}});
    let body = serde_json::to_string(&message).unwrap();
    let mut frames = format!("content-length: {}\r\n\r\n{body}", body.len()).into_bytes();
    crate::process::write_content_length_message(&mut frames, &message).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    lsp_reader(frames.as_slice(), sender);
    let received = receiver.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(received, [message.clone(), message]);
}

#[cfg(unix)]
#[test]
fn lsp_smoke_cleans_up_after_an_assertion_failure() {
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", "sleep 30"]);
    let (process, _messages) = LspSmokeProcess::spawn(&mut command);
    let pid = nix::unistd::Pid::from_raw(process.child.id() as i32);
    let started = std::time::Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _process = process;
        panic!("fixture protocol assertion failure");
    }));
    assert!(result.is_err());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(nix::sys::signal::kill(pid, None), Err(nix::errno::Errno::ESRCH));
}

#[cfg(unix)]
#[test]
fn lsp_smoke_reader_shutdown_is_bounded_after_the_child_exits() {
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", "sleep 30 & exit 0"]);
    let (mut process, _messages) = LspSmokeProcess::spawn(&mut command);
    assert!(process.child.wait().unwrap().success());
    let (done, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(process);
        done.send(()).unwrap();
    });
    receiver.recv_timeout(std::time::Duration::from_secs(2)).expect("reader shutdown");
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
    let definitions = managed_lsp_definitions();
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
        lsp_protocol_smoke(&definition, &installed, directory.path());
    }
}

fn lsp_protocol_smoke(
    definition: &super::catalog::ToolDefinition,
    installed: &std::path::Path,
    store: &std::path::Path,
) {
        let project = store.join("projects").join(&definition.id);
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
        let &(_, language, source_name, text) = MANAGED_LSP_FIXTURES
            .iter()
            .find(|fixture| fixture.0 == definition.id)
            .expect("catalog LSP fixture");
        let source = project.join(source_name);
        if let Some(parent) = source.parent() {
            std::fs::create_dir_all(parent).expect("source parent");
        }
        std::fs::write(&source, text).expect("source fixture");
        let root_uri = Url::from_file_path(&project).expect("root URI").to_string();
        let source_uri = Url::from_file_path(&source)
            .expect("source URI")
            .to_string();
        let runtime = nix_lsp_runtime_at(store, definition).expect("private runtime");
        let mut environment = runtime.environment();
        let path = std::env::join_paths(
            std::iter::once(store.join(&definition.id).join("bin"))
                .chain(std::env::split_paths(&runtime.runtime_path)),
        ).expect("private runtime PATH");
        environment.insert("PATH".into(), path.to_str().unwrap().into());
        for (variable, name) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("TMPDIR", "tmp"),
        ] {
            let path = store.join("environment").join(&definition.id).join(name);
            std::fs::create_dir_all(&path).expect("isolated server directory");
            environment.insert(variable.into(), path.to_str().unwrap().into());
        }
        if matches!(definition.install, Some(ToolInstall::NpmPackage { .. })) {
            let node =
                std::fs::canonicalize(store.join(&definition.id).join("bin/node"))
                    .expect("private Node runtime");
            assert!(
                node.starts_with("/nix/store") && node.to_string_lossy().contains("nodejs"),
                "{} did not select pinned Nix Node: {}",
                definition.id,
                node.display()
            );
        }
        let mut command = std::process::Command::new(installed);
        command.args(&definition.launch_args).current_dir(&project).env_clear().envs(environment);
        let (mut process, messages_rx) = LspSmokeProcess::spawn(&mut command);
        let mut stdin = process.child.stdin.take().expect("LSP stdin");
        let configuration =
            crate::language::lsp::LspServerConfiguration::for_server(&definition.id, installed);
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
        assert!(initialize.pointer("/result/capabilities").is_some_and(serde_json::Value::is_object));
        // Linters and formatters need not implement completion.
        if initialize.pointer("/result/capabilities/completionProvider")
            .is_some_and(|provider| provider.is_object() || provider == true)
        {
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
            if process.child.try_wait().expect("LSP status").is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                panic!("{} did not exit after shutdown", definition.id);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        drop(process);
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
    let version = probe_isolated_tool_version(ToolVersionRequest {
        id: definition.id.clone(),
        executable: installed.clone(),
        arguments: vec!["--version".into()],
        environment: std::collections::BTreeMap::from([
            ("HOME".into(), directory.path().to_str().unwrap().into()),
            ("PATH".into(), bin.to_str().unwrap().into()),
        ]),
        current_directory: Some(directory.path().into()),
        timeout: std::time::Duration::from_secs(10),
    })
    .expect("launch Bun-backed JavaScript executable");
    assert_eq!(Some(version.as_str()), definition.version.as_deref());
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
fn installer_worker_queues_requests_made_while_one_is_running() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("tool store");
    let catalog = bundled_tool_catalog();
    let installer = ToolInstaller::start_at(directory.path().to_path_buf());
    let rustup = directory.path().join("rustup");
    std::fs::write(&rustup, "#!/bin/sh\n: > .ready\nn=0\nwhile [ ! -f .release ]; do\n n=$((n+1))\n [ \"$n\" -lt 1000 ] || exit 1\n sleep 0.01\ndone\n").unwrap();
    std::fs::set_permissions(&rustup, std::fs::Permissions::from_mode(0o755)).unwrap();
    let proxy = directory.path().join("rust-analyzer");
    std::fs::hard_link(&rustup, &proxy).unwrap();
    let first = catalog.iter().find(|tool| tool.id == "rust-analyzer").unwrap().clone();
    installer.install_rustup_component(
        first, rustup, proxy, directory.path().into(), "rust-analyzer".into(),
    ).expect("queue blocking install");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !directory.path().join(".ready").exists() {
        assert!(std::time::Instant::now() < deadline, "installer did not start");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let ids = ["claude-agent-acp", "codex-acp"];
    for id in ids {
        let selected = directory.path().join(id).join("bin");
        std::fs::create_dir_all(&selected).expect("selected tool");
        let definition = catalog
            .iter()
            .find(|tool| tool.id == id)
            .expect("catalog tool")
            .clone();
        installer.uninstall(definition).expect("queue uninstall");
    }

    assert!(installer.try_result().unwrap().is_none(), "first operation must still be running");
    std::fs::write(directory.path().join(".release"), b"release").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut finished = Vec::new();
    while finished.len() < ids.len() + 1 {
        if let Some(result) = installer.try_result().expect("installer result") {
            assert!(result.result.is_ok(), "{result:?}");
            finished.push(result.id);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "queued work timed out"
        );
        std::thread::yield_now();
    }
    assert_eq!(finished, ["rust-analyzer", "claude-agent-acp", "codex-acp"]);
    assert!(ids.iter().all(|id| !directory.path().join(id).exists()));
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
fn wait_for_descendant(path: &std::path::Path) -> nix::unistd::Pid {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(contents) = std::fs::read_to_string(path)
            && let Ok(pid) = contents.trim().parse::<i32>()
            && pid > 0
        {
            let pid = nix::unistd::Pid::from_raw(pid);
            assert_eq!(nix::sys::signal::kill(pid, None), Ok(()), "descendant must be observable before cancellation");
            return pid;
        }
        assert!(std::time::Instant::now() < deadline, "descendant PID was not published");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[cfg(unix)]
fn assert_descendant_stopped(pid: nix::unistd::Pid) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match nix::sys::signal::kill(pid, None) {
            Err(nix::errno::Errno::ESRCH) => return,
            Ok(()) => {
                // A killed orphan can remain a zombie until the host's init reaps it.
                #[cfg(target_os = "linux")]
                if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
                    && stat.rsplit_once(") ").and_then(|(_, state)| state.chars().next()) == Some('Z')
                {
                    return;
                }
            }
            Err(error) => panic!("cannot inspect descendant {pid}: {error}"),
        }
        assert!(std::time::Instant::now() < deadline, "descendant {pid} is still running");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[cfg(unix)]
#[test]
fn version_probe_terminates_descendants_on_every_exit_path() {
    use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, thread, time::Duration};

    for (action, expected_error) in [
        ("wait", Some("cancelled")),
        ("wait", Some("timed out")),
        ("while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done", Some("exceeded its limit")),
        ("printf version", Some("remained open")),
        ("printf version; exec 1>&- 2>&-; exit 1", Some("exited with")),
        ("printf version; exec 1>&- 2>&-", None),
    ] {
        let root = tempfile::tempdir().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let request = ToolVersionRequest {
            id: "descendant".into(),
            executable: "/bin/sh".into(),
            arguments: vec!["-c".into(), format!(
                "sleep 30 {} & echo $! > descendant; while [ ! -f release ]; do sleep 0.01; done; {action}",
                if action.contains("exec") { ">/dev/null 2>&1" } else { "" },
            )],
            environment: Default::default(),
            current_directory: Some(root.path().into()),
            timeout: Duration::from_secs(10),
        };
        let worker_cancelled = cancelled.clone();
        let worker = thread::spawn(move || probe_tool_version(request, &worker_cancelled));
        let pid = wait_for_descendant(&root.path().join("descendant"));
        std::fs::write(root.path().join("release"), b"").unwrap();
        if expected_error == Some("cancelled") {
            cancelled.store(true, Ordering::Release);
        }
        let result = worker.join().unwrap();
        assert_descendant_stopped(pid);
        if let Some(error) = expected_error {
            assert!(result.as_ref().is_err_and(|result| result.contains(error)), "{result:?}");
        } else {
            assert_eq!(result.unwrap(), "version");
        }
    }
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

#[cfg(unix)]
#[test]
fn version_service_shutdown_with_full_results_is_bounded() {
    use std::{sync::mpsc, thread, time::Duration};
    let service = super::ToolVersionService::start();
    let directory = tempfile::tempdir().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    for id in 0..40 {
        let request = ToolVersionRequest {
            id: id.to_string(),
            executable: "/bin/sh".into(),
            arguments: vec!["-c".into(), if id == 32 {
                // This cannot execute until all 32 preceding results have been queued.
                "printf version; : > full".into()
            } else {
                "printf version".into()
            }],
            environment: Default::default(),
            current_directory: Some(directory.path().into()),
            timeout: Duration::from_secs(1),
        };
        loop {
            match service.probe(&request) {
                Ok(()) => break,
                Err(std::sync::mpsc::TrySendError::Full(())) => {
                    assert!(std::time::Instant::now() < deadline, "probe admission timed out");
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("probe worker stopped: {error}"),
            }
        }
    }
    while !directory.path().join("full").exists() {
        assert!(std::time::Instant::now() < deadline, "result queue did not fill");
        thread::sleep(Duration::from_millis(2));
    }
    let (done, receiver) = mpsc::channel();
    thread::spawn(move || {
        drop(service);
        done.send(()).unwrap();
    });
    receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("version worker shutdown");
}

#[cfg(unix)]
#[test]
fn version_probe_stops_continuous_oversized_output() {
    let started = std::time::Instant::now();
    let result = probe_tool_version(
        ToolVersionRequest {
            id: "continuous".into(),
            executable: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                "while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done".into(),
            ],
            environment: Default::default(),
            current_directory: None,
            timeout: std::time::Duration::from_secs(10),
        },
        &std::sync::atomic::AtomicBool::new(false),
    );
    assert!(result.unwrap_err().contains("exceeded its limit"));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

pub(super) fn package_fixture(
    name: &str,
) -> (
    super::PackageManifest,
    crate::extensions::PluginManifest,
    Vec<u8>,
) {
    use sha2::{Digest, Sha256};
    let bytes = b"\0asm\x0d\0\x01\0".to_vec();
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let manifest = super::PackageManifest {
        name: name.into(),
        version: "1.0.0".into(),
        component: "plugin.wasm".into(),
        source: super::PackageSource::Registry {
            registry: "https://example.invalid".into(),
            version: "1.0.0".into(),
            archive_sha256: digest.clone(),
        },
        dependencies: Vec::new(),
    };
    let plugin = crate::extensions::PluginManifest {
        id: name.into(),
        version: "1.0.0".into(),
        component_sha256: digest,
        capabilities: Default::default(),
        commands: Default::default(),
        contributions: Default::default(),
    };
    (manifest, plugin, bytes)
}

#[test]
fn package_gc_preserves_removed_package_for_rollback() {
    let root = tempfile::tempdir().unwrap();
    let manager = super::PackageManager::new(root.path().into(), 1024 * 1024, 1024).unwrap();
    let (manifest, plugin, bytes) = package_fixture("demo");
    let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
    manager
        .commit_prepared(&manifest, &plugin, &object)
        .unwrap();
    manager.remove("demo").unwrap();
    assert_eq!(manager.gc(256).unwrap(), 0);
    manager.rollback().unwrap();
    assert_eq!(manager.activations().unwrap()[0].0.id, "demo");
}

#[test]
fn package_activation_identity_is_independent_of_component() {
    let root = tempfile::tempdir().unwrap();
    let manager = super::PackageManager::new(root.path().into(), 1024 * 1024, 1024).unwrap();
    for name in ["one", "two"] {
        let (manifest, plugin, bytes) = package_fixture(name);
        let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
        manager
            .commit_prepared(&manifest, &plugin, &object)
            .unwrap();
    }
    manager.gc(256).unwrap();
    let plugins = manager.activations().unwrap();
    assert_eq!(
        plugins
            .iter()
            .map(|(plugin, _)| plugin.id.as_str())
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    let mut updated = package_fixture("two");
    updated.1.version = "2.0.0".into();
    let object = manager
        .stage_verified_object(&updated.0, &updated.2)
        .unwrap();
    manager
        .commit_prepared(&updated.0, &updated.1, &object)
        .unwrap();
    manager.gc(256).unwrap();
    manager.rollback().unwrap();
    assert_eq!(manager.activations().unwrap()[1].0.version, "1.0.0");
}

#[test]
fn package_commit_accepts_and_normalizes_uppercase_digest() {
    let root = tempfile::tempdir().unwrap();
    let manager = super::PackageManager::new(root.path().into(), 1024 * 1024, 1024).unwrap();
    let (mut manifest, mut plugin, bytes) = package_fixture("demo");
    if let super::PackageSource::Registry { archive_sha256, .. } = &mut manifest.source {
        *archive_sha256 = archive_sha256.to_ascii_uppercase();
    }
    plugin.component_sha256 = plugin.component_sha256.to_ascii_uppercase();
    let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
    let lock = manager
        .commit_prepared(&manifest, &plugin, &object)
        .unwrap();
    assert_eq!(
        lock.packages[0].content_sha256,
        plugin.component_sha256.to_ascii_lowercase()
    );
    manager.activations().unwrap();
}

#[cfg(unix)]
#[test]
fn package_managers_serialize_concurrent_lock_updates() {
    let root = tempfile::tempdir().unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let threads = (0..8)
        .map(|id| {
            let root = root.path().to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let manager = super::PackageManager::new(root, 1024 * 1024, 1024).unwrap();
                let (manifest, plugin, bytes) = package_fixture(&format!("package-{id}"));
                let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
                barrier.wait();
                manager
                    .commit_prepared(&manifest, &plugin, &object)
                    .unwrap();
                manager.gc(256).unwrap();
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    let manager = super::PackageManager::new(root.path().into(), 1024 * 1024, 1024).unwrap();
    assert_eq!(manager.activations().unwrap().len(), 8);
}

#[test]
fn catalog_filetypes_preserve_exact_extension_case_before_fallback() {
    use super::mason_filetype_for_path;
    use std::path::Path;
    assert_eq!(
        mason_filetype_for_path(Path::new("module.EX")),
        Some("euphoria3")
    );
    assert_eq!(
        mason_filetype_for_path(Path::new("module.ex")),
        Some("elixir")
    );
    assert_eq!(
        mason_filetype_for_path(Path::new("rules.BUILD")),
        Some("bzl")
    );
    assert_eq!(
        mason_filetype_for_path(Path::new("source.RS")),
        Some("rust")
    );
}

#[test]
fn package_service_keeps_every_completion_under_backpressure() {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let root = tempfile::tempdir().unwrap();
    let mut service = super::PackageService::start(root.path().into(), 1, 1024, 1024);
    let mut admitted = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    for _ in 0..3 {
        loop {
            match service.request(super::PackageOperation::List) {
                Ok(id) => {
                    admitted.push(id);
                    break;
                }
                Err(super::PackageError::QueueFull) => {
                    assert!(Instant::now() < deadline, "package admission timed out");
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("package admission failed: {error}"),
            }
        }
    }
    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while service.pending_count() != 0 {
        for event in service.poll().unwrap() {
            match event {
                super::PackageServiceEvent::Completed(result) => received.push(result.id),
                event => panic!("unexpected {event:?}"),
            }
        }
        assert!(
            Instant::now() < deadline,
            "missing admitted package completion"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(received, admitted);
}

#[test]
fn registry_service_keeps_every_result_under_backpressure() {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let root = tempfile::tempdir().unwrap();
    let service = super::RegistryService::start(
        super::RegistryPolicy {
            offline: true,
            queue_capacity: 1,
            ..Default::default()
        },
        root.path().into(),
    )
    .unwrap();
    let mut admitted = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    for _ in 0..3 {
        loop {
            match service.request(super::RegistryRequest::Search {
                query: "fixture".into(),
            }) {
                Ok(id) => {
                    admitted.push(id);
                    break;
                }
                Err(super::RegistryError::QueueFull) => {
                    assert!(Instant::now() < deadline, "registry admission timed out");
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("registry admission failed: {error}"),
            }
        }
    }
    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while received.len() < admitted.len() {
        received.extend(service.poll().unwrap().into_iter().map(|event| event.id));
        assert!(
            Instant::now() < deadline,
            "missing admitted registry completion"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(received, admitted);
}

#[cfg(unix)]
#[test]
fn failed_uv_update_restores_previous_environment_and_shebang_paths() {
    use std::{fs, os::unix::fs::PermissionsExt, sync::mpsc};
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join(".runtimes/uv");
    fs::create_dir_all(runtime.join("bin")).unwrap();
    fs::write(runtime.join("bin/uv"), "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(runtime.join("bin/uv"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        runtime.join(super::compatibility::MANAGED_RUNTIME_MARKER),
        runtime_artifact("uv").unwrap().sha256,
    )
    .unwrap();
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| matches!(tool.install, Some(ToolInstall::UvTool { .. })))
        .unwrap();
    let old = root.path().join(&definition.id);
    fs::create_dir_all(old.join("bin")).unwrap();
    let script = format!("#!{}/uv-tools/python\nprevious tool\n", old.display());
    fs::write(old.join("bin").join(&definition.executable), &script).unwrap();
    let (_cancel, cancellations) = mpsc::channel();
    let result = super::runtimes::install_uv_tool(
        &definition,
        root.path(),
        &["invalid-fixture".into()],
        &cancellations,
        &mut |_, _, _| {},
        None,
    );
    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(old.join("bin").join(&definition.executable)).unwrap(),
        script
    );
}

#[cfg(unix)]
#[test]
fn installer_cancellation_terminates_descendant_processes() {
    use std::{sync::mpsc, thread};
    let root = tempfile::tempdir().unwrap();
    let (cancel, cancellations) = mpsc::channel();
    let cwd = root.path().to_path_buf();
    let worker = thread::spawn(move || {
        super::runtimes::run_managed(
            std::path::Path::new("/bin/sh"),
            &[
                "-c".into(),
                "trap '' TERM; sleep 30 & echo $! > descendant; wait".into(),
            ],
            &cwd,
            &std::collections::BTreeMap::from([("PATH".into(), std::env::var("PATH").unwrap())]),
            &cancellations,
            None,
        )
    });
    let pid = wait_for_descendant(&root.path().join("descendant"));
    cancel.send(()).unwrap();
    assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
    assert_descendant_stopped(pid);
}

#[test]
fn rustfmt_is_available_as_a_stdin_formatter() {
    let tool = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rustfmt")
        .unwrap();
    assert_eq!(tool.kind, ToolKind::Formatter);
    assert_eq!(tool.format_arguments, ["--emit", "stdout"]);
}
