//! Tests for tool catalogs, installation and package storage.

use reqwest::Url;

use crate::tools::catalog::ToolArchiveFormat;

use crate::tools::catalog::ToolInstall;

use crate::tools::catalog::ToolKind;

use crate::tools::catalog::bundled_tool_catalog;

use crate::tools::catalog::discover_executable;

use crate::tools::catalog::managed_tool_installed_version_at;

use crate::tools::catalog::nix_lsp_launch_environment_at;

use crate::tools::catalog::nix_lsp_runtime_at;

use crate::tools::catalog::nix_lsp_shell;

use crate::tools::catalog::nixos_os_release_at;

use crate::tools::catalog::tool_launch_environment_at;

use crate::tools::catalog::write_managed_tool_version;

use crate::tools::compatibility::elf_interpreter;

use crate::tools::compatibility::runtime_artifact;

use crate::tools::install::install_tool;

use crate::tools::install_prepare::archive_entry_matches;

use crate::tools::install_prepare::prepare_nix_lsp_workspace_at;

use crate::tools::install_prepare::relative_catalog_path;

use crate::tools::install_prepare::validate_tool_id;

use crate::tools::installer::ToolInstaller;

use crate::tools::installer::ToolOperation;

use crate::tools::mason_catalog::mason_snapshot_counts;

use crate::tools::package_lock::valid_digest;

use crate::tools::runtimes::install_nix_lsp_profile;

use crate::tools::runtimes::uninstall_managed_tool;

use crate::tools::runtimes::uninstall_nix_lsp_profile;

use crate::tools::version::ToolVersionRequest;

use crate::tools::version::probe_tool_version;

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
    for value in ["nixos", "\"nixos\"", "'nixos'"] {
        std::fs::write(&os_release, format!("NAME=NixOS\nID={value}\n")).expect("NixOS release");
        assert!(nixos_os_release_at(&os_release));
    }
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
fn cached_runtime_refreshes_when_the_nix_compatibility_loader_changes() {
    use crate::tools::catalog::NixCompatibilityRuntime;
    use crate::tools::compatibility::MANAGED_RUNTIME_MARKER;
    use std::fs;
    use std::sync::mpsc;

    let directory = tempfile::tempdir().unwrap();
    let runtime = directory.path().join(".runtimes/bun");
    fs::create_dir_all(runtime.join("bin")).unwrap();
    let executable = runtime.join("bin/bun");
    fs::write(
        runtime.join(MANAGED_RUNTIME_MARKER),
        runtime_artifact("bun").unwrap().sha256,
    )
    .unwrap();
    let compatibility = NixCompatibilityRuntime {
        loader: "/nix/store/new-glibc/lib/ld-linux-x86-64.so.2".into(),
        nix_ld: "/unused/nix-ld".into(),
        patchelf: "/unused/patchelf".into(),
        library_path: String::new(),
        runtime_path: String::new(),
    };
    for (loader, refresh) in [
        (compatibility.loader.to_str().unwrap(), false),
        ("/nix/store/old-glibc/lib/ld-linux-x86-64.so.2", true),
    ] {
        // A minimal ELF64 with one PT_INTERP header; it is inspected, never executed.
        let mut elf = vec![0; 120];
        elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&1u16.to_le_bytes());
        elf[64..68].copy_from_slice(&3u32.to_le_bytes());
        elf[72..80].copy_from_slice(&120u64.to_le_bytes());
        elf[96..104].copy_from_slice(&(loader.len() as u64 + 1).to_le_bytes());
        elf.extend_from_slice(loader.as_bytes());
        elf.push(0);
        fs::write(&executable, &elf).unwrap();
        let (cancel, cancellations) = mpsc::channel();
        let result = crate::tools::runtimes::managed_runtime(
            directory.path(),
            "bun",
            &cancellations,
            &mut move |phase, _, _| {
                assert_eq!(phase, "refreshing runtime compatibility");
                // Stop before downloading, keeping this test offline.
                cancel.send(()).unwrap();
            },
            Some(&compatibility),
        );
        if refresh {
            assert_eq!(result.unwrap_err(), "tool installation cancelled");
        } else {
            assert_eq!(result.unwrap(), executable);
        }
        assert_eq!(fs::read(&executable).unwrap(), elf);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}

#[test]
#[ignore = "operator qualification: requires Nix plus upstream artifact network access"]
fn nixos_runtime_installs_patches_and_runs_the_verified_upstream_artifact() {
    if !crate::tools::is_nixos_host() {
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
        environment: runtime
            .environment()
            .into_iter()
            .map(|(key, value)| (key, value.into()))
            .collect(),
        current_directory: Some(directory.path().into()),
        timeout: std::time::Duration::from_secs(10),
        cancelled: Default::default(),
    })
    .expect("launch patched upstream executable");
    assert!(version.contains("0.3.3016"), "{version}");
}

#[test]
#[ignore = "operator qualification: requires Nix and all catalog LSP upstream networks"]
fn nixos_runtime_installs_and_probes_every_managed_catalog_lsp() {
    if !crate::tools::is_nixos_host() {
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
                    cancelled: Default::default(),
                },
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap_or_else(|error| panic!("probe {}: {error}", definition.id));
            assert!(!version.is_empty(), "{} version", definition.id);
        }
    }
}

const MANAGED_LSP_FIXTURES: [(&str, &str, &str, &str); 24] = [
    (
        "rust-analyzer",
        "rust",
        "src/main.rs",
        "fn main() { let value = 1; value; }\n",
    ),
    (
        "typescript-language-server",
        "typescript",
        "main.ts",
        "const value = 1;\nvalue;\n",
    ),
    ("pyright", "python", "main.py", "value = 1\nvalue\n"),
    ("clangd", "c", "main.c", "int main(void) { return 0; }\n"),
    (
        "gopls",
        "go",
        "main.go",
        "package main\nfunc main() { var value = 1; _ = value }\n",
    ),
    (
        "bash-language-server",
        "shellscript",
        "main.sh",
        "#!/bin/sh\necho value\n",
    ),
    (
        "vscode-html-language-server",
        "html",
        "index.html",
        "<main>value</main>\n",
    ),
    (
        "tailwindcss-language-server",
        "html",
        "index.html",
        "<main class=\"flex\">value</main>\n",
    ),
    ("taplo", "toml", "config.toml", "value = 1\n"),
    (
        "vscode-css-language-server",
        "css",
        "style.css",
        "main { color: red; }\n",
    ),
    (
        "vscode-json-language-server",
        "json",
        "config.json",
        "{\"value\": 1}\n",
    ),
    ("yaml-language-server", "yaml", "config.yaml", "value: 1\n"),
    ("marksman", "markdown", "README.md", "# Value\n\ntext\n"),
    (
        "emmylua-ls",
        "lua",
        "main.lua",
        "local value = 1\nprint(value)\n",
    ),
    (
        "intelephense",
        "php",
        "main.php",
        "<?php\n$value = 1;\necho $value;\n",
    ),
    (
        "svelte-language-server",
        "svelte",
        "App.svelte",
        "<script>let value = 1;</script>\n{value}\n",
    ),
    (
        "vue-language-server",
        "vue",
        "App.vue",
        "<script setup>const value = 1</script>\n<template>{{ value }}</template>\n",
    ),
    (
        "dockerfile-language-server",
        "dockerfile",
        "Dockerfile",
        "FROM scratch\n",
    ),
    (
        "astro-language-server",
        "astro",
        "index.astro",
        "---\nconst value = 1;\n---\n<main>{value}</main>\n",
    ),
    (
        "biome",
        "javascript",
        "main.js",
        "const value = 1;\nvalue;\n",
    ),
    (
        "oxlint",
        "javascript",
        "main.js",
        "const value = 1;\nvalue;\n",
    ),
    (
        "oxfmt",
        "javascript",
        "main.js",
        "const value = 1;\nvalue;\n",
    ),
    (
        "tsgo",
        "typescript",
        "main.ts",
        "const value = 1;\nvalue;\n",
    ),
    ("ruff", "python", "main.py", "value = 1\nvalue\n"),
];

fn managed_lsp_definitions() -> Vec<crate::tools::catalog::ToolDefinition> {
    let definitions = bundled_tool_catalog()
        .into_iter()
        .filter(|tool| tool.kind == ToolKind::Lsp && tool.install.is_some())
        .collect::<Vec<_>>();
    let mut actual = definitions
        .iter()
        .map(|tool| tool.id.as_str())
        .collect::<Vec<_>>();
    let mut expected = MANAGED_LSP_FIXTURES
        .iter()
        .map(|fixture| fixture.0)
        .collect::<Vec<_>>();
    actual.sort_unstable();
    expected.sort_unstable();
    assert_eq!(
        actual, expected,
        "managed LSP fixtures must match the catalog before installation"
    );
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
    arguments.extend(
        request
            .environment
            .iter()
            .map(|(key, value)| {
                value
                    .to_str()
                    .map(|value| format!("{key}={value}"))
                    .ok_or("non-UTF-8 fixture environment")
            })
            .collect::<Result<Vec<_>, _>>()?,
    );
    arguments.push(
        request
            .executable
            .to_str()
            .ok_or("non-UTF-8 tool path")?
            .into(),
    );
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
        arguments: vec![
            "-c".into(),
            "test -z \"${HOME+x}\" && printf '%s' \"$DUST_PROBE_FIXTURE\"".into(),
        ],
        environment: std::collections::BTreeMap::from([(
            "DUST_PROBE_FIXTURE".into(),
            "isolated".into(),
        )]),
        current_directory: Some(directory.path().into()),
        timeout: std::time::Duration::from_secs(10),
        cancelled: Default::default(),
    })
    .unwrap();
    assert_eq!(version, "isolated");
}

fn write_lsp_message(
    stdin: &mut std::process::ChildStdin,
    message: &serde_json::Value,
) -> Result<(), String> {
    crate::process::write_content_length_message(stdin, message).map_err(|error| error.to_string())
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
    fn spawn(
        command: &mut std::process::Command,
    ) -> (
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
            .spawn()
            .expect("launch LSP smoke");
        let mut process = Self {
            child,
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            reader: None,
        };
        let stdout = crate::process::CancellablePipe::new(
            process.child.stdout.take().expect("LSP stdout"),
            process.cancelled.clone(),
        )
        .expect("cancellable LSP stdout");
        let (sender, receiver) = std::sync::mpsc::channel();
        process.reader = Some(std::thread::spawn(move || lsp_reader(stdout, sender)));
        (process, receiver)
    }
}

impl Drop for LspSmokeProcess {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
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
    assert_eq!(
        nix::sys::signal::kill(pid, None),
        Err(nix::errno::Errno::ESRCH)
    );
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
    receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("reader shutdown");
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
    Err(format!("LSP response {id} exceeded the message limit"))
}

#[test]
#[ignore = "operator qualification: installs all catalog LSPs and performs live protocol I/O"]
fn nixos_runtime_completes_protocol_smoke_for_every_managed_catalog_lsp() {
    if !crate::tools::is_nixos_host() {
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
    definition: &crate::tools::catalog::ToolDefinition,
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
    )
    .expect("private runtime PATH");
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
        let node = std::fs::canonicalize(store.join(&definition.id).join("bin/node"))
            .expect("private Node runtime");
        assert!(
            node.starts_with("/nix/store") && node.to_string_lossy().contains("nodejs"),
            "{} did not select pinned Nix Node: {}",
            definition.id,
            node.display()
        );
    }
    let mut command = std::process::Command::new(installed);
    command
        .args(&definition.launch_args)
        .current_dir(&project)
        .env_clear()
        .envs(environment);
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
    assert!(
        initialize
            .pointer("/result/capabilities")
            .is_some_and(serde_json::Value::is_object)
    );
    // Linters and formatters need not implement completion.
    if initialize
        .pointer("/result/capabilities/completionProvider")
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
        cancelled: Default::default(),
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
    let first = catalog
        .iter()
        .find(|tool| tool.id == "rust-analyzer")
        .unwrap()
        .clone();
    installer
        .install_rustup_component(
            first,
            rustup,
            proxy,
            directory.path().into(),
            "rust-analyzer".into(),
        )
        .expect("queue blocking install");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !directory.path().join(".ready").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "installer did not start"
        );
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

    assert!(
        installer.try_result().unwrap().is_none(),
        "first operation must still be running"
    );
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
        "#!/bin/sh\n[ \"$1\" = component ] && printf '%s\\n' \"$@\" > \"$PWD/.rustup-args\"\nexit 0\n",
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
        "component\nadd\nrust-analyzer\nrust-src\nrustc\nrust-std\ncargo\n"
    );
}

#[cfg(unix)]
#[test]
fn failed_rustup_component_installation_reports_bounded_stderr() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;

    let directory = tempfile::tempdir().unwrap();
    let rustup = directory.path().join("rustup");
    fs::write(
        &rustup,
        format!(
            "#!/bin/sh\nprintf '%s\\n' '{}' 'downloading component' 'download failed' 'error: certificate verification failed' >&2\nexit 1\n",
            "x".repeat(8192)
        ),
    )
    .unwrap();
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755)).unwrap();
    let proxy = directory.path().join("rust-analyzer");
    fs::hard_link(&rustup, &proxy).unwrap();
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .unwrap();
    let (_cancel, cancellations) = mpsc::channel();
    let error = crate::tools::runtimes::install_path_rustup_component(
        &definition,
        &rustup,
        &proxy,
        directory.path(),
        "rust-analyzer",
        &cancellations,
        &mut |_, _, _| {},
    )
    .unwrap_err();
    assert!(error.contains("exited with 1"), "{error}");
    assert!(error.contains("certificate verification failed"), "{error}");
    assert!(error.len() < 512, "stderr tail must be bounded");
}

#[cfg(unix)]
#[test]
fn rustup_install_adds_the_compiler_and_fails_when_rust_analyzer_still_cannot_run() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;

    let directory = tempfile::tempdir().unwrap();
    let rustup = directory.path().join("rustup");
    // As rustup, record the arguments; as the proxy, fail like a toolchain without rustc.
    fs::write(
        &rustup,
        "#!/bin/sh\nif [ \"$1\" = component ]; then echo \"$@\" > added; exit 0; fi\necho 'cannot open shared object file' >&2\nexit 127\n",
    )
    .unwrap();
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755)).unwrap();
    let proxy = directory.path().join("rust-analyzer");
    fs::hard_link(&rustup, &proxy).unwrap();
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .unwrap();
    let (_cancel, cancellations) = mpsc::channel();
    let error = crate::tools::runtimes::install_path_rustup_component(
        &definition,
        &rustup,
        &proxy,
        directory.path(),
        "rust-analyzer",
        &cancellations,
        &mut |_, _, _| {},
    )
    .unwrap_err();
    assert_eq!(
        fs::read_to_string(directory.path().join("added")).unwrap(),
        "component add rust-analyzer rust-src rustc rust-std cargo\n"
    );
    assert!(error.contains("does not run"), "{error}");
    assert!(error.contains("cannot open shared object file"), "{error}");
}

#[cfg(unix)]
#[test]
fn rustup_cancellation_terminates_its_process_group() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use std::thread;
    let root = tempfile::tempdir().unwrap();
    let rustup = root.path().join("rustup");
    fs::write(
        &rustup,
        "#!/bin/sh\ntrap '' TERM\nsleep 30 & echo $! > descendant\nwait\n",
    )
    .unwrap();
    fs::set_permissions(&rustup, fs::Permissions::from_mode(0o755)).unwrap();
    let proxy = root.path().join("rust-analyzer");
    fs::hard_link(&rustup, &proxy).unwrap();
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .unwrap();
    let (cancel, cancellations) = mpsc::channel();
    let cwd = root.path().to_path_buf();
    let worker = thread::spawn(move || {
        crate::tools::runtimes::install_path_rustup_component(
            &definition,
            &rustup,
            &proxy,
            &cwd,
            "rust-analyzer",
            &cancellations,
            &mut |_, _, _| {},
        )
    });
    let pid = wait_for_descendant(&root.path().join("descendant"));
    cancel.send(()).unwrap();
    assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
    assert_descendant_stopped(pid);
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
    // More than the old inspection limit, including generations beyond its first page.
    for index in 3..1100 {
        symlink(
            &target,
            profiles.join(format!("rust-analyzer-{index}-link")),
        )
        .unwrap();
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
    assert_eq!(std::fs::read_dir(&profiles).unwrap().count(), 1);
}

#[test]
fn nix_profile_uninstall_reports_metadata_errors_other_than_absence() {
    let directory = tempfile::tempdir().unwrap();
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| tool.id == "rust-analyzer")
        .unwrap();
    uninstall_nix_lsp_profile(&definition, directory.path()).unwrap();
    std::fs::write(
        directory.path().join("nix-lsp-workspace"),
        b"not a directory",
    )
    .unwrap();
    let error = uninstall_nix_lsp_profile(&definition, directory.path()).unwrap_err();
    assert!(
        error.contains("cannot inspect Dust Nix environments"),
        "{error}"
    );
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
            assert_eq!(
                nix::sys::signal::kill(pid, None),
                Ok(()),
                "descendant must be observable before cancellation"
            );
            return pid;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "descendant PID was not published"
        );
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
                    && stat
                        .rsplit_once(") ")
                        .and_then(|(_, state)| state.chars().next())
                        == Some('Z')
                {
                    return;
                }
            }
            Err(error) => panic!("cannot inspect descendant {pid}: {error}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "descendant {pid} is still running"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[cfg(unix)]
#[test]
fn version_probe_terminates_descendants_on_every_exit_path() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::thread;
    use std::time::Duration;

    for (action, expected_error) in [
        ("wait", Some("cancelled")),
        ("wait", Some("timed out")),
        (
            "while :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done",
            Some("exceeded its limit"),
        ),
        ("printf version", Some("remained open")),
        (
            "printf version; exec 1>&- 2>&-; exit 1",
            Some("exited with"),
        ),
        ("printf version; exec 1>&- 2>&-", None),
    ] {
        let root = tempfile::tempdir().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let request = ToolVersionRequest {
            id: "descendant".into(),
            executable: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                format!(
                    "sleep 30 {} & echo $! > descendant; while [ ! -f release ]; do sleep 0.01; done; {action}",
                    if action.contains("exec") {
                        ">/dev/null 2>&1"
                    } else {
                        ""
                    },
                ),
            ],
            environment: Default::default(),
            current_directory: Some(root.path().into()),
            timeout: Duration::from_secs(10),
            cancelled: Default::default(),
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
            assert!(
                result.as_ref().is_err_and(|result| result.contains(error)),
                "{result:?}"
            );
        } else {
            assert_eq!(result.unwrap(), "version");
        }
    }
}

#[cfg(unix)]
#[test]
fn cancelled_queued_version_probes_do_not_spawn() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use std::time::Instant;
    let directory = tempfile::tempdir().unwrap();
    let service = crate::tools::ToolVersionWorker::start();
    let request = |id: &str, script: &str, cancelled| ToolVersionRequest {
        id: id.into(),
        executable: "/bin/sh".into(),
        arguments: vec!["-c".into(), script.into()],
        environment: Default::default(),
        current_directory: Some(directory.path().to_path_buf()),
        timeout: Duration::from_secs(2),
        cancelled,
    };
    service
        .probe(&request(
            "first",
            "touch started; while [ ! -f release ]; do sleep 0.01; done; echo version",
            Default::default(),
        ))
        .unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    service
        .probe(&request(
            "queued",
            "touch executed; echo version",
            cancelled.clone(),
        ))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !directory.path().join("started").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    cancelled.store(true, Ordering::Release);
    std::fs::write(directory.path().join("release"), "").unwrap();
    loop {
        if let Some(result) = service.try_result()
            && result.id == "queued"
        {
            assert_eq!(result.result.unwrap_err(), "version probe cancelled");
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!directory.path().join("executed").exists());
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
            cancelled: Default::default(),
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
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    let service = crate::tools::ToolVersionWorker::start();
    let directory = tempfile::tempdir().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    for id in 0..40 {
        let request = ToolVersionRequest {
            id: id.to_string(),
            executable: "/bin/sh".into(),
            arguments: vec![
                "-c".into(),
                if id == 32 {
                    // This cannot execute until all 32 preceding results have been queued.
                    "printf version; : > full".into()
                } else {
                    "printf version".into()
                },
            ],
            environment: Default::default(),
            current_directory: Some(directory.path().into()),
            timeout: Duration::from_secs(1),
            cancelled: Default::default(),
        };
        loop {
            match service.probe(&request) {
                Ok(()) => break,
                Err(std::sync::mpsc::TrySendError::Full(())) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "probe submission timed out"
                    );
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("probe worker stopped: {error}"),
            }
        }
    }
    while !directory.path().join("full").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "result queue did not fill"
        );
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
            cancelled: Default::default(),
        },
        &std::sync::atomic::AtomicBool::new(false),
    );
    assert!(result.unwrap_err().contains("exceeded its limit"));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

pub(super) fn package_fixture(
    name: &str,
) -> (
    crate::tools::PackageManifest,
    crate::plugins::PluginManifest,
    Vec<u8>,
) {
    use sha2::Digest;
    use sha2::Sha256;
    let bytes = b"\0asm\x0d\0\x01\0".to_vec();
    let digest = format!("{:x}", Sha256::digest(&bytes));
    let manifest = crate::tools::PackageManifest {
        name: name.into(),
        version: "1.0.0".into(),
        component: "plugin.wasm".into(),
        source: crate::tools::PackageSource::Registry {
            registry: "https://example.invalid".into(),
            version: "1.0.0".into(),
            archive_sha256: digest.clone(),
        },
        dependencies: Vec::new(),
    };
    let plugin = crate::plugins::PluginManifest {
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
    let manager = crate::tools::PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
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
    let manager = crate::tools::PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
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
            .map(|(plugin, _, _)| plugin.id.as_str())
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
fn package_resolver_and_verifier_accept_uppercase_digests() {
    let root = tempfile::tempdir().unwrap();
    let (manifest, _, bytes) = package_fixture("demo");
    let digest = manifest.source_digest().to_ascii_uppercase();
    for source in [
        crate::tools::PackageSource::Registry {
            registry: "https://example.invalid".into(),
            version: "1.0.0".into(),
            archive_sha256: digest.clone(),
        },
        crate::tools::PackageSource::Git {
            url: "https://example.invalid/demo.git".into(),
            commit: "a".repeat(40),
            tree_sha256: digest.clone(),
        },
    ] {
        let mut manifest = manifest.clone();
        manifest.source = source;
        let manifests = [(manifest.name.clone(), manifest)].into_iter().collect();
        let mut lock = crate::tools::resolve_manifests(&manifests).unwrap();
        assert_eq!(lock.packages[0].content_sha256, digest.to_ascii_lowercase());
        let store = root.path().join("store");
        crate::tools::commit_store_bytes(&store, &digest, &bytes, 1024).unwrap();
        let path = root.path().join("lock.json");
        crate::tools::write_lock_atomic(&path, &lock, 4096).unwrap();
        assert_eq!(
            crate::tools::verify_lock_store(&path, &store, 4096, 1024).unwrap(),
            1
        );
        // Supplied locks may still use uppercase, even though generated locks do not.
        lock.packages[0].content_sha256.make_ascii_uppercase();
        crate::tools::write_lock_atomic(&path, &lock, 4096).unwrap();
        assert_eq!(
            crate::tools::verify_lock_store(&path, &store, 4096, 1024).unwrap(),
            1
        );
    }
}

#[test]
fn package_reader_accepts_exact_limit_and_rejects_excess_and_special_files() {
    use crate::tools::package_lock::read_bounded_regular;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("object");
    std::fs::write(&path, b"1234").unwrap();
    assert_eq!(read_bounded_regular(&path, 4).unwrap(), b"1234");
    assert!(matches!(
        read_bounded_regular(&path, 3),
        Err(crate::tools::PackageError::InvalidManifest)
    ));
    assert!(read_bounded_regular(root.path(), 4).is_err());
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(read_bounded_regular(&link, 4).is_err());
    let fifo = root.path().join("fifo");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
    assert!(matches!(
        read_bounded_regular(&fifo, 4),
        Err(crate::tools::PackageError::InvalidManifest)
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn package_reader_bounds_streams_whose_metadata_understates_their_size() {
    let path = std::path::Path::new("/proc/self/status");
    assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
    assert!(matches!(
        crate::tools::package_lock::read_bounded_regular(path, 8),
        Err(crate::tools::PackageError::InvalidManifest)
    ));
}

#[test]
fn package_verification_bounds_objects_and_rejects_non_regular_entries() {
    let root = tempfile::tempdir().unwrap();
    let (manifest, _, bytes) = package_fixture("demo");
    let store = root.path().join("store");
    let object =
        crate::tools::commit_store_bytes(&store, manifest.source_digest(), &bytes, 1024).unwrap();
    let manifests = [(manifest.name.clone(), manifest)].into_iter().collect();
    let lock = crate::tools::resolve_manifests(&manifests).unwrap();
    let path = root.path().join("lock.json");
    crate::tools::write_lock_atomic(&path, &lock, 4096).unwrap();
    assert_eq!(
        crate::tools::verify_lock_store(&path, &store, 4096, bytes.len()).unwrap(),
        1
    );
    assert!(matches!(
        crate::tools::verify_lock_store(&path, &store, 4096, bytes.len() - 1),
        Err(crate::tools::PackageError::InvalidManifest)
    ));
    // Sparse objects must be rejected before allocating their contents.
    std::fs::File::create(&object)
        .unwrap()
        .set_len(1024 * 1024 * 1024)
        .unwrap();
    assert!(matches!(
        crate::tools::verify_lock_store(&path, &store, 4096, 1024),
        Err(crate::tools::PackageError::InvalidManifest)
    ));
    std::fs::remove_file(&object).unwrap();
    nix::unistd::mkfifo(&object, nix::sys::stat::Mode::S_IRUSR).unwrap();
    assert!(matches!(
        crate::tools::verify_lock_store(&path, &store, 4096, 1024),
        Err(crate::tools::PackageError::InvalidManifest)
    ));
}

#[test]
fn package_verification_rejects_content_that_differs_from_its_source_pin() {
    let root = tempfile::tempdir().unwrap();
    let (manifest, _, bytes) = package_fixture("demo");
    let store = root.path().join("store");
    crate::tools::commit_store_bytes(&store, manifest.source_digest(), &bytes, 1024).unwrap();
    let manifests = [(manifest.name.clone(), manifest)].into_iter().collect();
    let mut lock = crate::tools::resolve_manifests(&manifests).unwrap();
    for source in [
        crate::tools::PackageSource::Registry {
            registry: "https://example.invalid".into(),
            version: "1.0.0".into(),
            archive_sha256: "0".repeat(64),
        },
        crate::tools::PackageSource::Git {
            url: "https://example.invalid/demo.git".into(),
            commit: "a".repeat(40),
            tree_sha256: "0".repeat(64),
        },
    ] {
        lock.packages[0].source = source;
        let path = root.path().join("lock.json");
        crate::tools::write_lock_atomic(&path, &lock, 4096).unwrap();
        assert!(matches!(
            crate::tools::verify_lock_store(&path, &store, 4096, 1024),
            Err(crate::tools::PackageError::ChecksumMismatch)
        ));
    }
}

#[test]
fn package_store_reinstall_repairs_corrupted_objects() {
    let root = tempfile::tempdir().unwrap();
    let (manifest, _, bytes) = package_fixture("demo");
    let object =
        crate::tools::commit_store_bytes(root.path(), manifest.source_digest(), &bytes, 1024)
            .unwrap();
    for corrupt in [vec![b'x'; bytes.len()], vec![b'x'; 2048]] {
        std::fs::write(&object, corrupt).unwrap();
        let repaired =
            crate::tools::commit_store_bytes(root.path(), manifest.source_digest(), &bytes, 1024)
                .unwrap();
        assert_eq!(repaired, object);
        assert_eq!(std::fs::read(&object).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    let manager = crate::tools::PackageStore::new(root.path().join("manager"), 4096, 1024).unwrap();
    let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
    std::fs::write(&object, b"corrupt").unwrap();
    assert_eq!(
        manager.stage_verified_object(&manifest, &bytes).unwrap(),
        object
    );
}

#[test]
fn package_atomic_writes_remove_staging_files_when_rename_fails() {
    let root = tempfile::tempdir().unwrap();
    let (manifest, _, bytes) = package_fixture("demo");
    let manifests = [(manifest.name.clone(), manifest.clone())]
        .into_iter()
        .collect();
    let lock = crate::tools::resolve_manifests(&manifests).unwrap();
    // Staging succeeds in the directory, but the destination name cannot be installed.
    let path = root.path().join("x".repeat(300));
    assert!(crate::tools::write_lock_atomic(&path, &lock, 4096).is_err());
    assert!(
        crate::tools::package_lock::write_protected_atomic(&path, b"activation", 1024).is_err()
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    let target = root.path().join(manifest.source_digest());
    std::fs::create_dir(&target).unwrap();
    assert!(
        crate::tools::commit_store_bytes(root.path(), manifest.source_digest(), &bytes, 1024)
            .is_err()
    );
    assert!(target.is_dir());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn package_commits_preserve_plugin_ownership_and_identity() {
    let root = tempfile::tempdir().unwrap();
    let manager = crate::tools::PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
    let (manifest, plugin, bytes) = package_fixture("demo");
    let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
    let installed = manager
        .commit_prepared(&manifest, &plugin, &object)
        .unwrap();
    let mut renamed = plugin.clone();
    renamed.id = "other".into();
    assert!(matches!(
        manager.commit_prepared(&manifest, &renamed, &object),
        Err(crate::tools::PackageError::Activation(_))
    ));
    let mut unrelated = manifest.clone();
    unrelated.name = "other".into();
    assert!(matches!(
        manager.commit_prepared(&unrelated, &plugin, &object),
        Err(crate::tools::PackageError::Activation(_))
    ));
    assert_eq!(manager.read_lock().unwrap(), installed);
    assert_eq!(manager.activations().unwrap()[0].0.id, "demo");
    assert!(matches!(
        manager.rollback(),
        Err(crate::tools::PackageError::NoRollback)
    ));
}

#[test]
fn package_commit_accepts_and_normalizes_uppercase_digest() {
    let root = tempfile::tempdir().unwrap();
    let manager = crate::tools::PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
    let (mut manifest, mut plugin, bytes) = package_fixture("demo");
    if let crate::tools::PackageSource::Registry { archive_sha256, .. } = &mut manifest.source {
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
                let manager = crate::tools::PackageStore::new(root, 1024 * 1024, 1024).unwrap();
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
    let manager = crate::tools::PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
    assert_eq!(manager.activations().unwrap().len(), 8);
}

#[test]
fn catalog_filetypes_preserve_exact_extension_case_before_fallback() {
    use crate::tools::mason_filetype_for_path;
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
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;
    let root = tempfile::tempdir().unwrap();
    let mut service = crate::tools::PackageWorker::start(root.path().into(), 1, 1024, 1024);
    let mut accepted = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    for _ in 0..3 {
        loop {
            match service.request(crate::tools::PackageOperation::List) {
                Ok(id) => {
                    accepted.push(id);
                    break;
                }
                Err(crate::tools::PackageError::QueueFull) => {
                    assert!(Instant::now() < deadline, "package submission timed out");
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("package submission failed: {error}"),
            }
        }
    }
    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while service.pending_count() != 0 {
        for event in service.poll().unwrap() {
            match event {
                crate::tools::PackageEvent::Completed(result) => received.push(result.id),
                event => panic!("unexpected {event:?}"),
            }
        }
        assert!(
            Instant::now() < deadline,
            "missing accepted package completion"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(received, accepted);
}

#[test]
fn registry_service_keeps_every_result_under_backpressure() {
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;
    let root = tempfile::tempdir().unwrap();
    let service = crate::tools::RegistryWorker::start(
        crate::tools::RegistryPolicy {
            offline: true,
            queue_capacity: 1,
            ..Default::default()
        },
        root.path().into(),
    )
    .unwrap();
    let mut accepted = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    for _ in 0..3 {
        loop {
            match service.request(crate::tools::RegistryRequest::Search {
                query: "fixture".into(),
            }) {
                Ok(id) => {
                    accepted.push(id);
                    break;
                }
                Err(crate::tools::RegistryError::QueueFull) => {
                    assert!(Instant::now() < deadline, "registry submission timed out");
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("registry submission failed: {error}"),
            }
        }
    }
    let mut received = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while received.len() < accepted.len() {
        received.extend(service.poll().unwrap().into_iter().map(|event| event.id));
        assert!(
            Instant::now() < deadline,
            "missing accepted registry completion"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(received, accepted);
}

#[cfg(unix)]
#[test]
fn locked_uv_installation_rejects_another_process_and_uninstall() {
    use std::fs;
    use std::sync::mpsc;
    let definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| matches!(tool.install, Some(ToolInstall::UvTool { .. })))
        .unwrap();
    if let Some(root) = std::env::var_os("DUST_TEST_LOCKED_TOOL_ROOT") {
        let root = std::path::Path::new(&root);
        let (_cancel, cancellations) = mpsc::channel();
        let error = crate::tools::runtimes::install_uv_tool(
            &definition,
            root,
            &["fixture".into()],
            &cancellations,
            &mut |_, _, _| {},
            None,
        )
        .unwrap_err();
        assert!(error.contains("cannot lock tool"), "{error}");
        assert!(
            uninstall_managed_tool(&definition, root)
                .unwrap_err()
                .contains("cannot lock tool")
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let tool = root.path().join(&definition.id);
    fs::create_dir(&tool).unwrap();
    fs::write(tool.join("existing"), "keep this").unwrap();
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.path().join(format!(".install-{}.lock", definition.id)))
        .unwrap();
    let lock = nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusive).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("locked_uv_installation_rejects_another_process_and_uninstall")
        .env("DUST_TEST_LOCKED_TOOL_ROOT", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(tool.join("existing")).unwrap(),
        "keep this"
    );
    drop(lock);
    uninstall_managed_tool(&definition, root.path()).unwrap();
    assert!(!tool.exists());
}

#[cfg(unix)]
#[test]
fn failed_uv_update_restores_previous_environment_and_shebang_paths() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join(".runtimes/uv");
    fs::create_dir_all(runtime.join("bin")).unwrap();
    fs::write(runtime.join("bin/uv"), "#!/bin/sh\nexit 1\n").unwrap();
    fs::set_permissions(runtime.join("bin/uv"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        runtime.join(crate::tools::compatibility::MANAGED_RUNTIME_MARKER),
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
    let result = crate::tools::runtimes::install_uv_tool(
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
fn installer_exit_terminates_helpers_and_finishes_reading_stderr() {
    use std::fs;
    use std::sync::mpsc;
    use std::time::Duration;
    use std::time::Instant;
    for exit in [0, 1] {
        let root = tempfile::tempdir().unwrap();
        let (_cancel, cancellations) = mpsc::channel();
        let started = Instant::now();
        let result = crate::tools::runtimes::run_managed(
            std::path::Path::new("/bin/sh"),
            &[
                "-c".into(),
                format!("sleep 30 & echo $! > descendant; echo diagnostic >&2; exit {exit}"),
            ],
            root.path(),
            &std::collections::BTreeMap::from([("PATH".into(), std::env::var_os("PATH").unwrap())]),
            &cancellations,
            None,
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        if exit == 0 {
            result.unwrap();
        } else {
            assert!(result.unwrap_err().contains("diagnostic"));
        }
        let pid = fs::read_to_string(root.path().join("descendant")).unwrap();
        let state = fs::read_to_string(format!("/proc/{}/stat", pid.trim()));
        assert!(state.is_err() || state.unwrap().split_whitespace().nth(2) == Some("Z"));
    }
}

#[cfg(unix)]
#[test]
fn installer_cancellation_terminates_descendant_processes() {
    use std::sync::mpsc;
    use std::thread;
    let root = tempfile::tempdir().unwrap();
    let (cancel, cancellations) = mpsc::channel();
    let cwd = root.path().to_path_buf();
    let worker = thread::spawn(move || {
        crate::tools::runtimes::run_managed(
            std::path::Path::new("/bin/sh"),
            &[
                "-c".into(),
                "trap '' TERM; sleep 30 & echo $! > descendant; wait".into(),
            ],
            &cwd,
            &std::collections::BTreeMap::from([("PATH".into(), std::env::var_os("PATH").unwrap())]),
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

#[test]
fn package_failed_commit_preserves_rollback() {
    let root = tempfile::tempdir().unwrap();
    let manager = crate::tools::PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
    let (manifest, mut plugin, bytes) = package_fixture("demo");
    let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
    manager
        .commit_prepared(&manifest, &plugin, &object)
        .unwrap();
    plugin.version = "2.0.0".into();
    let current = manager
        .commit_prepared(&manifest, &plugin, &object)
        .unwrap();
    let backup_path = root.path().join("dust-packages.lock.previous.json");
    let backup = std::fs::read(&backup_path).unwrap();
    let limited = crate::tools::PackageStore::new(
        root.path().into(),
        serde_json::to_vec_pretty(&current).unwrap().len(),
        1024,
    )
    .unwrap();
    let (additional, plugin, _) = package_fixture("extra");
    assert!(matches!(
        limited.commit_prepared(&additional, &plugin, &object),
        Err(crate::tools::PackageError::LockTooLarge)
    ));
    assert_eq!(std::fs::read(backup_path).unwrap(), backup);
    assert_eq!(manager.activations().unwrap()[0].0.version, "2.0.0");
    manager.rollback().unwrap();
    assert_eq!(manager.activations().unwrap()[0].0.version, "1.0.0");
}

#[test]
fn package_missing_removal_preserves_rollback() {
    let root = tempfile::tempdir().unwrap();
    let manager = crate::tools::PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
    let (manifest, mut plugin, bytes) = package_fixture("demo");
    let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
    manager
        .commit_prepared(&manifest, &plugin, &object)
        .unwrap();
    plugin.version = "2.0.0".into();
    manager
        .commit_prepared(&manifest, &plugin, &object)
        .unwrap();
    assert!(matches!(
        manager.remove("typo"),
        Err(crate::tools::PackageError::PackageNotFound(_))
    ));
    manager.rollback().unwrap();
    assert_eq!(manager.activations().unwrap()[0].0.version, "1.0.0");
}

#[test]
fn package_removal_ignores_its_own_dependencies_but_not_other_packages() {
    let root = tempfile::tempdir().unwrap();
    let manager = crate::tools::PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
    let (manifest, plugin, bytes) = package_fixture("demo");
    let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
    let mut lock = manager
        .commit_prepared(&manifest, &plugin, &object)
        .unwrap();
    lock.packages[0].dependencies.push("demo".into());
    crate::tools::write_lock_atomic(
        &root.path().join("dust-packages.lock.json"),
        &lock,
        1024 * 1024,
    )
    .unwrap();
    let (other, plugin, _) = package_fixture("dependent");
    let mut lock = manager.commit_prepared(&other, &plugin, &object).unwrap();
    lock.packages[1].dependencies.push("demo".into());
    crate::tools::write_lock_atomic(
        &root.path().join("dust-packages.lock.json"),
        &lock,
        1024 * 1024,
    )
    .unwrap();
    assert!(matches!(
        manager.remove("demo"),
        Err(crate::tools::PackageError::PackageRequired(_))
    ));
    manager.remove("dependent").unwrap();
    assert!(manager.remove("demo").unwrap().packages.is_empty());
}

#[test]
fn package_commit_rejects_changed_or_shared_plugin_ids() {
    let root = tempfile::tempdir().unwrap();
    let manager = crate::tools::PackageStore::new(root.path().into(), 1024 * 1024, 1024).unwrap();
    let (manifest, mut plugin, bytes) = package_fixture("alpha");
    let object = manager.stage_verified_object(&manifest, &bytes).unwrap();
    manager
        .commit_prepared(&manifest, &plugin, &object)
        .unwrap();
    plugin.id = "new-id".into();
    assert!(
        manager
            .commit_prepared(&manifest, &plugin, &object)
            .unwrap_err()
            .to_string()
            .contains("cannot change plugin ID")
    );
    let (other, mut plugin, _) = package_fixture("beta");
    plugin.id = "alpha".into();
    assert!(
        manager
            .commit_prepared(&other, &plugin, &object)
            .unwrap_err()
            .to_string()
            .contains("already owned")
    );
    let activations = manager.activations().unwrap();
    assert_eq!(activations.len(), 1);
    assert_eq!(activations[0].0.id, "alpha");
}

#[cfg(unix)]
#[test]
fn go_install_preserves_non_utf8_environment_paths() {
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    // Some filesystems require UTF-8 names; Linux tmpfs accepts raw bytes.
    let directory = if cfg!(target_os = "linux") {
        tempfile::tempdir_in("/dev/shm")
    } else {
        tempfile::tempdir()
    }
    .unwrap();
    let root = directory.path().join(OsStr::from_bytes(b"state-\xff"));
    let runtime = root.join(".runtimes/go");
    fs::create_dir_all(runtime.join("bin")).unwrap();
    let go = runtime.join("bin/go");
    fs::write(&go, "#!/bin/sh\nset -eu\n[ \"$GOBIN\" = \"$HOME/bin\" ]\n[ \"$GOPATH\" = \"$HOME/gopath\" ]\n[ \"$GOCACHE\" = \"$HOME/gocache\" ]\nprintf '#!/bin/sh\\nexit 0\\n' > \"$GOBIN/fixture\"\n").unwrap();
    fs::set_permissions(go, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        runtime.join(crate::tools::compatibility::MANAGED_RUNTIME_MARKER),
        runtime_artifact("go").unwrap().sha256,
    )
    .unwrap();
    let mut definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| matches!(tool.install, Some(ToolInstall::GoPackage { .. })))
        .unwrap();
    definition.id = "fixture".into();
    definition.executable = "fixture".into();
    let (_cancel, cancellations) = mpsc::channel();
    let installed = install_tool(&definition, &root, &cancellations, &mut |_, _, _| {}, None)
        .unwrap()
        .unwrap();
    assert!(installed.is_file());
    assert!(installed.starts_with(&root));
}

#[cfg(unix)]
#[test]
fn npm_install_patches_native_dependencies_before_activation() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    fn elf(interpreter: &str) -> Vec<u8> {
        let mut bytes = vec![0; 120];
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[32..40].copy_from_slice(&64_u64.to_le_bytes());
        bytes[54..56].copy_from_slice(&56_u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1_u16.to_le_bytes());
        bytes[64..68].copy_from_slice(&3_u32.to_le_bytes());
        bytes[72..80].copy_from_slice(&120_u64.to_le_bytes());
        bytes[96..104].copy_from_slice(&(interpreter.len() as u64 + 1).to_le_bytes());
        bytes.extend_from_slice(interpreter.as_bytes());
        bytes.push(0);
        bytes
    }
    fn shell_bytes(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("\\{byte:03o}")).collect()
    }
    let root = tempfile::tempdir().unwrap();
    let runtime = root.path().join(".runtimes/bun");
    fs::create_dir_all(runtime.join("bin")).unwrap();
    let bun = runtime.join("bin/bun");
    fs::write(&bun, format!("#!/bin/sh\nset -eu\nmkdir -p \"$BUN_INSTALL/node_modules/native\"\nprintf '{}' > \"$BUN_INSTALL/node_modules/native/fixture\"\nchmod 700 \"$BUN_INSTALL/node_modules/native/fixture\"\nln -s ../node_modules/native/fixture \"$BUN_INSTALL/bin/fixture\"\n", shell_bytes(&elf("/lib64/ld-linux-x86-64.so.2")))).unwrap();
    fs::set_permissions(&bun, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        runtime.join(crate::tools::compatibility::MANAGED_RUNTIME_MARKER),
        runtime_artifact("bun").unwrap().sha256,
    )
    .unwrap();
    let patchelf = root.path().join("patchelf");
    let loader = "/private/ld.so";
    fs::write(
        &patchelf,
        format!(
            "#!/bin/sh\nprintf '{}' > \"$5\"\n",
            shell_bytes(&elf(loader))
        ),
    )
    .unwrap();
    fs::set_permissions(&patchelf, fs::Permissions::from_mode(0o700)).unwrap();
    let compatibility = crate::tools::catalog::NixCompatibilityRuntime {
        loader: loader.into(),
        nix_ld: "/private/nix-ld".into(),
        patchelf,
        library_path: String::new(),
        runtime_path: std::env::var("PATH").unwrap(),
    };
    let mut definition = bundled_tool_catalog()
        .into_iter()
        .find(|tool| matches!(tool.install, Some(ToolInstall::NpmPackage { .. })))
        .unwrap();
    definition.id = "fixture".into();
    definition.executable = "fixture".into();
    let (_cancel, cancellations) = mpsc::channel();
    let installed = install_tool(
        &definition,
        root.path(),
        &cancellations,
        &mut |_, _, _| {},
        Some(&compatibility),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        elf_interpreter(&installed).unwrap().as_deref(),
        Some(loader)
    );
}

#[cfg(unix)]
#[test]
fn installer_cancellation_terminates_descendant_processes_slot5() {
    use std::fs;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use std::time::Instant;
    let root = tempfile::tempdir().unwrap();
    let (cancel, cancellations) = mpsc::channel();
    let cwd = root.path().to_path_buf();
    let worker = thread::spawn(move || {
        crate::tools::runtimes::run_managed(
            std::path::Path::new("/bin/sh"),
            &[
                "-c".into(),
                "trap '' TERM; sleep 30 & echo $! > descendant; wait".into(),
            ],
            &cwd,
            &std::collections::BTreeMap::from([("PATH".into(), std::env::var_os("PATH").unwrap())]),
            &cancellations,
            None,
        )
    });
    let deadline = Instant::now() + Duration::from_secs(3);
    while !root.path().join("descendant").exists() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let pid = fs::read_to_string(root.path().join("descendant")).unwrap();
    cancel.send(()).unwrap();
    assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
    let state = fs::read_to_string(format!("/proc/{}/stat", pid.trim()));
    assert!(state.is_err() || state.unwrap().split_whitespace().nth(2) == Some("Z"));
}
