//! Managed language runtimes and tool launch paths.

use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use crate::tools::NIX_COMPATIBILITY_FLAKE_PACKAGE;

use crate::tools::catalog::NixCompatibilityRuntime;

use crate::tools::catalog::ToolArchiveFormat;

use crate::tools::catalog::ToolDefinition;

use crate::tools::catalog::discover_executable;

use crate::tools::catalog::nix_lsp_shell;

use crate::tools::catalog::parse_nix_runtime_environment;

use crate::tools::catalog::rust_analyzer_is_rustup_proxy;

use crate::tools::catalog::write_managed_tool_version;

use crate::tools::compatibility::MANAGED_RUNTIME_MARKER;

use crate::tools::compatibility::elf_interpreter;

use crate::tools::compatibility::patch_nix_compatibility_tree;

use crate::tools::compatibility::patch_nix_compatible_executable;

use crate::tools::compatibility::runtime_artifact;

use crate::tools::download::ToolProgressReporter;

use crate::tools::download::download_checked;

use crate::tools::download::extract_one;

use crate::tools::download::extract_tar_gzip_tree;

use crate::tools::download::prepare_executable;

use crate::tools::download::replace_managed_directory;

use crate::tools::install::install_tool;

use crate::tools::install_prepare::TOOL_INSTALL_TIMEOUT;

use crate::tools::install_prepare::ensure_private_directory;

use crate::tools::install_prepare::prepare_nix_lsp_workspace_at;

use crate::tools::install_prepare::validate_tool_id;

use crate::tools::package_lock::next_temp_suffix;

pub(super) fn fhs_glibc_loader_available() -> bool {
    [
        "/lib64/ld-linux-x86-64.so.2",
        "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
    ]
    .iter()
    .any(|path| Path::new(path).is_file())
}

pub(super) fn managed_runtime(
    root: &Path,
    name: &str,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<PathBuf, String> {
    if cancellations.try_recv().is_ok() {
        return Err("tool installation cancelled".into());
    }
    let artifact = runtime_artifact(name)?;
    let runtime = root.join(".runtimes").join(name);
    let executable = runtime.join("bin").join(name);
    let marker = runtime.join(MANAGED_RUNTIME_MARKER);
    if executable.is_file()
        && fs::read_to_string(&marker)
            .is_ok_and(|value| value.trim().eq_ignore_ascii_case(artifact.sha256))
    {
        let refresh = if let Some(compatibility) = compatibility {
            elf_interpreter(&executable)?.is_some_and(|loader| {
                loader.starts_with("/nix/store/") && Path::new(&loader) != compatibility.loader
            })
        } else {
            false
        };
        if !refresh {
            if let Some(compatibility) = compatibility {
                patch_nix_compatible_executable(
                    &executable,
                    compatibility,
                    &runtime,
                    cancellations,
                )?;
            }
            return Ok(executable);
        }
        // Re-extract the verified artifact rather than patching another profile's loader.
        report("refreshing runtime compatibility", 0, None);
    }
    fs::create_dir_all(runtime.parent().ok_or("runtime has no parent")?)
        .map_err(|error| format!("cannot create runtime directory: {error}"))?;
    let staging = root.join(format!(".runtime-{name}-{}", next_temp_suffix()));
    fs::create_dir(&staging).map_err(|error| format!("cannot stage runtime: {error}"))?;
    let result = (|| {
        let bytes = download_checked(artifact.url, artifact.sha256, cancellations, report)?;
        report("extracting runtime", 0, None);
        let bin = staging.join("bin");
        let staged_executable = bin.join(name);
        if name == "go" && artifact.archive == ToolArchiveFormat::TarGzip {
            extract_tar_gzip_tree(&bytes, "go", &staging)?;
            prepare_executable(&staged_executable)?;
        } else {
            fs::create_dir(&bin).map_err(|error| format!("cannot create runtime bin: {error}"))?;
            extract_one(
                &bytes,
                artifact.archive,
                artifact.executable_path,
                &staged_executable,
            )?;
        }
        if let Some(compatibility) = compatibility {
            patch_nix_compatibility_tree(&staging, compatibility, cancellations)?;
        }
        fs::write(staging.join(MANAGED_RUNTIME_MARKER), artifact.sha256)
            .map_err(|error| format!("cannot record managed runtime identity: {error}"))?;
        replace_managed_directory(&staging, &runtime)?;
        Ok(runtime.join("bin").join(name))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

pub(super) fn managed_bun_runtime(
    root: &Path,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<PathBuf, String> {
    // NixOS uses Dust's exact checked GNU Bun artifact so PATH cannot silently
    // change the runtime processed by the private compatibility environment.
    if compatibility.is_none()
        && let Some(bun) = discover_executable("bun")
    {
        let bun = crate::files::canonicalize(&bun)
            .map_err(|error| format!("cannot canonicalize Bun runtime: {error}"))?;
        if bun.is_file() && bun.is_absolute() {
            report("verifying Bun runtime", 0, None);
            run_managed(
                &bun,
                &["--version".into()],
                root,
                &BTreeMap::from([("HOME".into(), root.as_os_str().to_owned())]),
                cancellations,
                None,
            )
            .map_err(|error| format!("configured Bun runtime is not runnable: {error}"))?;
            return Ok(bun);
        }
    }
    if compatibility.is_none() && !fhs_glibc_loader_available() {
        return Err(
            "no directly runnable Bun runtime and this Linux host lacks the FHS glibc loader"
                .into(),
        );
    }
    managed_runtime(root, "bun", cancellations, report, compatibility)
}

pub(super) fn run_managed(
    program: &Path,
    arguments: &[String],
    cwd: &Path,
    environment: &BTreeMap<String, OsString>,
    cancellations: &Receiver<()>,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<(), String> {
    let mut effective_environment = environment.clone();
    if let Some(runtime) = compatibility {
        effective_environment.extend(
            runtime
                .environment()
                .into_iter()
                .map(|(key, value)| (key, value.into())),
        );
        effective_environment.insert("DUST_NIX_LD".into(), runtime.nix_ld.as_os_str().to_owned());
        let mut paths = env::split_paths(&runtime.runtime_path).collect::<Vec<_>>();
        if let Some(existing) = effective_environment.get("PATH") {
            paths.extend(env::split_paths(existing));
        }
        let path = env::join_paths(paths).map_err(|_| "Dust Nix compatibility PATH is invalid")?;
        effective_environment.insert("PATH".into(), path);
    }
    let mut command = Command::new(program);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .args(arguments)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env_clear()
        .envs(&effective_environment)
        .spawn()
        .map_err(|error| format!("failed to launch managed {}: {error}", program.display()))?;
    let installer_output = child.stderr.take().map(read_installer_output);
    let deadline = Instant::now() + TOOL_INSTALL_TIMEOUT;
    let result = loop {
        if cancellations.try_recv().is_ok() {
            break Err("tool installation cancelled".to_owned());
        }
        if Instant::now() >= deadline {
            break Err("tool installation exceeded the 15-minute limit".to_owned());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => break Err(format!("failed to wait for managed installer: {error}")),
        }
    };
    // Even a successful installer may leave helpers holding its output pipe open.
    crate::process::terminate_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
    let output = installer_output
        .and_then(|output| output.join().ok())
        .unwrap_or_default();
    let status = result?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "managed installer `{}` exited with {}{}",
            program.file_name().unwrap_or_default().to_string_lossy(),
            status
                .code()
                .map_or_else(|| "signal".into(), |code| code.to_string()),
            output
        ))
    }
}

/// Read what an installer prints on its own thread, so a full pipe cannot stall it, and hand
/// back the end of it: that is where installers say what went wrong.
fn read_installer_output(
    mut stream: impl std::io::Read + Send + 'static,
) -> thread::JoinHandle<String> {
    const KEPT_BYTES: usize = 4096;
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut chunk = [0; 4096];
        while let Ok(read) = stream.read(&mut chunk) {
            if read == 0 {
                break;
            }
            kept.extend_from_slice(&chunk[..read]);
            kept.drain(..kept.len().saturating_sub(KEPT_BYTES));
        }
        installer_failure_detail(&String::from_utf8_lossy(&kept))
    })
}

fn installer_failure_detail(output: &str) -> String {
    let lines = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let detail = lines[lines.len().saturating_sub(3)..]
        .join(" ")
        .chars()
        .filter(|character| !character.is_control())
        .take(400)
        .collect::<String>();
    if detail.is_empty() {
        detail
    } else {
        format!(": {detail}")
    }
}

pub(super) fn install_javascript_package(
    root: &Path,
    packages: &[String],
    staging: &Path,
    output: &Path,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<(), String> {
    let bun = managed_bun_runtime(root, cancellations, report, compatibility)?;
    let bin = staging.join("bin");
    let node = compatibility
        .and_then(|runtime| runtime.executable("node"))
        .unwrap_or_else(|| bun.clone());
    #[cfg(unix)]
    std::os::unix::fs::symlink(&node, bin.join("node"))
        .map_err(|error| format!("cannot provide private JavaScript runtime: {error}"))?;
    #[cfg(windows)]
    fs::copy(&node, bin.join("node.exe"))
        .map_err(|error| format!("cannot provide private JavaScript runtime: {error}"))?;
    let arguments = std::iter::once("install".into())
        .chain(std::iter::once("--global".into()))
        .chain(std::iter::once("--ignore-scripts".into()))
        .chain(std::iter::once("--no-progress".into()))
        .chain(std::iter::once("--no-summary".into()))
        .chain(packages.iter().cloned())
        .collect::<Vec<_>>();
    let environment = BTreeMap::from([
        ("HOME".into(), staging.as_os_str().to_owned()),
        ("BUN_INSTALL".into(), staging.as_os_str().to_owned()),
        (
            "BUN_INSTALL_CACHE_DIR".into(),
            staging.join("bun-cache").into_os_string(),
        ),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ]);
    report("resolving JavaScript packages", 0, None);
    run_managed(
        &bun,
        &arguments,
        staging,
        &environment,
        cancellations,
        compatibility,
    )?;
    if !output.is_file() {
        return Err("managed Bun installation did not produce its executable".into());
    }
    Ok(())
}

pub(super) fn install_uv_tool(
    definition: &ToolDefinition,
    root: &Path,
    packages: &[String],
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<Option<PathBuf>, String> {
    let _lock = lock_managed_tool(root, &definition.id)?;
    let uv = managed_runtime(root, "uv", cancellations, report, compatibility)?;
    let destination = root.join(&definition.id);
    let backup = root.join(format!(
        ".previous-{}-{}",
        definition.id,
        next_temp_suffix()
    ));
    let had_previous = destination.exists();
    if had_previous {
        let metadata = fs::symlink_metadata(&destination)
            .map_err(|error| format!("cannot inspect managed tool: {error}"))?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err("managed tool destination is not a real directory".into());
        }
        fs::rename(&destination, &backup)
            .map_err(|error| format!("cannot preserve previous tool: {error}"))?;
    }
    let result = (|| {
        fs::create_dir(&destination)
            .map_err(|error| format!("cannot create managed tool: {error}"))?;
        // uv links the executables into a side folder that becomes `bin` only once they are
        // patched, so a language server can't start one while patchelf still writes it.
        let staged_bin = destination.join(".bin-staging");
        let tools = destination.join("uv-tools");
        fs::create_dir(&staged_bin).map_err(|error| format!("cannot create tool bin: {error}"))?;
        let arguments = std::iter::once("tool".into())
            .chain(std::iter::once("install".into()))
            .chain(std::iter::once("--reinstall".into()))
            .chain(packages.iter().cloned())
            .collect::<Vec<_>>();
        let environment = BTreeMap::from([
            ("HOME".into(), destination.as_os_str().to_owned()),
            ("UV_TOOL_DIR".into(), tools.into_os_string()),
            ("UV_TOOL_BIN_DIR".into(), staged_bin.as_os_str().to_owned()),
            (
                "UV_CACHE_DIR".into(),
                destination.join("uv-cache").into_os_string(),
            ),
            (
                "UV_PYTHON_INSTALL_DIR".into(),
                destination.join("python").into_os_string(),
            ),
            ("UV_NO_PROGRESS".into(), "1".into()),
        ]);
        report("resolving Python tool", 0, None);
        run_managed(
            &uv,
            &arguments,
            &destination,
            &environment,
            cancellations,
            compatibility,
        )?;
        if let Some(compatibility) = compatibility {
            patch_nix_compatibility_tree(&destination, compatibility, cancellations)?;
        }
        if !staged_bin.join(&definition.executable).is_file() {
            return Err("managed uv installation did not produce its executable".into());
        }
        write_managed_tool_version(&destination, definition)?;
        let bin = destination.join("bin");
        fs::rename(&staged_bin, &bin)
            .map_err(|error| format!("cannot activate tool bin: {error}"))?;
        Ok(Some(bin.join(&definition.executable)))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&destination);
        if had_previous {
            fs::rename(&backup, &destination)
                .map_err(|error| format!("cannot restore previous tool: {error}"))?;
        }
    } else if had_previous {
        let _ = fs::remove_dir_all(&backup);
    }
    result
}

pub(super) fn valid_rustup_component(component: &str) -> bool {
    !component.is_empty()
        && component.len() <= 128
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

pub(super) fn install_path_rustup_component(
    definition: &ToolDefinition,
    rustup: &Path,
    proxy: &Path,
    cwd: &Path,
    component: &str,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
) -> Result<Option<PathBuf>, String> {
    if !valid_rustup_component(component)
        || definition.id != "rust-analyzer"
        || definition.executable != "rust-analyzer"
        || component != "rust-analyzer"
    {
        return Err("external rustup installation is restricted to rust-analyzer".into());
    }
    let rustup = crate::files::canonicalize(rustup)
        .map_err(|error| format!("cannot canonicalize rustup: {error}"))?;
    let cwd = crate::files::canonicalize(cwd)
        .map_err(|error| format!("cannot canonicalize rustup working directory: {error}"))?;
    if !rustup.is_file() || !cwd.is_dir() || !rust_analyzer_is_rustup_proxy(proxy) {
        return Err("the selected PATH runtime is not a valid rustup proxy".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let runtime =
            fs::metadata(&rustup).map_err(|error| format!("cannot inspect rustup: {error}"))?;
        let proxy =
            fs::metadata(proxy).map_err(|error| format!("cannot inspect rustup proxy: {error}"))?;
        if runtime.dev() != proxy.dev() || runtime.ino() != proxy.ino() {
            return Err("the selected rustup executable does not own the PATH proxy".into());
        }
    }
    #[cfg(not(unix))]
    if crate::files::canonicalize(proxy).ok().as_deref() != Some(rustup.as_path()) {
        return Err("the selected rustup executable does not own the PATH proxy".into());
    }
    report("installing rustup component", 0, None);
    let mut command = Command::new(&rustup);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    // A toolchain installed with only some components lacks the compiler libraries
    // rust-analyzer loads, the standard library build scripts compile against, and the
    // cargo it runs to read the workspace. Adding components already there does nothing.
    let mut child = command
        .args([
            "component",
            "add",
            component,
            "rust-src",
            "rustc",
            "rust-std",
            "cargo",
        ])
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to launch rustup: {error}"))?;
    let installer_output = child.stderr.take().map(read_installer_output);
    let deadline = Instant::now() + TOOL_INSTALL_TIMEOUT;
    let result = loop {
        if cancellations.try_recv().is_ok() {
            break Err("rustup component installation cancelled".to_owned());
        }
        if Instant::now() >= deadline {
            break Err("rustup component installation exceeded the 15-minute limit".to_owned());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => break Err(format!("failed to wait for rustup: {error}")),
        }
    };
    crate::process::terminate_process_group(child.id());
    let _ = child.kill();
    let _ = child.wait();
    let output = installer_output
        .and_then(|output| output.join().ok())
        .unwrap_or_default();
    let status = result?;
    if !status.success() {
        return Err(format!(
            "rustup component installation exited with {}{}",
            status
                .code()
                .map_or_else(|| "signal".into(), |code| code.to_string()),
            output
        ));
    }
    report("verifying rustup component", 0, None);
    if !proxy.is_file() {
        return Err(format!(
            "rustup installed the component but {} is unavailable",
            definition.executable
        ));
    }
    let environment = env::vars_os()
        .filter_map(|(key, value)| key.into_string().ok().map(|key| (key, value)))
        .collect();
    run_managed(
        proxy,
        &["--version".into()],
        &cwd,
        &environment,
        cancellations,
        None,
    )
    .map_err(|error| format!("rustup added {component}, but it does not run: {error}"))?;
    Ok(Some(proxy.to_path_buf()))
}

pub(super) fn install_rustup_component(
    definition: &ToolDefinition,
    root: &Path,
    component: &str,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<Option<PathBuf>, String> {
    if !valid_rustup_component(component) {
        return Err("tool catalog has an invalid rustup component".into());
    }
    let initializer = managed_runtime(root, "rustup", cancellations, report, compatibility)?;
    let runtime = root.join(".runtimes/rustup");
    let rustup_home = runtime.join("rustup-home");
    let cargo_home = runtime.join("cargo-home");
    let environment = BTreeMap::from([
        ("HOME".into(), runtime.as_os_str().to_owned()),
        ("RUSTUP_HOME".into(), rustup_home.as_os_str().to_owned()),
        ("CARGO_HOME".into(), cargo_home.as_os_str().to_owned()),
        ("RUSTUP_INIT_SKIP_PATH_CHECK".into(), "yes".into()),
    ]);
    let rustup = cargo_home.join("bin/rustup");
    if !rustup.is_file() {
        let arguments = vec![
            "-y".into(),
            "--no-modify-path".into(),
            "--profile".into(),
            "minimal".into(),
            "--default-toolchain".into(),
            "stable".into(),
        ];
        report("installing Rust toolchain", 0, None);
        // One binary is both programs and goes by its name, and this copy is named `rustup`.
        let mut init_environment = environment.clone();
        init_environment.insert("RUSTUP_FORCE_ARG0".into(), "rustup-init".into());
        run_managed(
            &initializer,
            &arguments,
            &runtime,
            &init_environment,
            cancellations,
            compatibility,
        )?;
    }
    if !rustup.is_file() {
        return Err("managed rustup initialization did not produce rustup".into());
    }
    let arguments = vec![
        "component".into(),
        "add".into(),
        component.into(),
        "--toolchain".into(),
        "stable".into(),
    ];
    report("installing Rust component", 0, None);
    run_managed(
        &rustup,
        &arguments,
        &runtime,
        &environment,
        cancellations,
        compatibility,
    )?;
    if let Some(compatibility) = compatibility {
        patch_nix_compatibility_tree(&runtime, compatibility, cancellations)?;
    }
    let target = rustup_home
        .join("toolchains/stable-x86_64-unknown-linux-gnu/bin")
        .join(&definition.executable);
    prepare_executable(&target)?;
    let staging = root.join(format!(".tool-{}-{}", definition.id, next_temp_suffix()));
    fs::create_dir(&staging).map_err(|error| format!("cannot stage rust component: {error}"))?;
    let result = (|| {
        let bin = staging.join("bin");
        fs::create_dir(&bin).map_err(|error| format!("cannot create tool bin: {error}"))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, bin.join(&definition.executable))
            .map_err(|error| format!("cannot link managed Rust component: {error}"))?;
        #[cfg(not(unix))]
        fs::copy(&target, bin.join(&definition.executable))
            .map_err(|error| format!("cannot copy managed Rust component: {error}"))?;
        write_managed_tool_version(&staging, definition)?;
        let destination = root.join(&definition.id);
        replace_managed_directory(&staging, &destination)?;
        Ok(Some(destination.join("bin").join(&definition.executable)))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

pub(super) fn probe_downloaded_tool(
    executable: &Path,
    arguments: &[String],
    workspace: &Path,
    cancellations: &Receiver<()>,
    compatibility: Option<&NixCompatibilityRuntime>,
) -> Result<(), String> {
    if arguments.is_empty() {
        return Ok(());
    }
    run_managed(
        executable,
        arguments,
        workspace,
        &BTreeMap::from([("HOME".into(), workspace.as_os_str().to_owned())]),
        cancellations,
        compatibility,
    )
    .map_err(|error| format!("downloaded artifact is not runnable on this Linux host: {error}"))
}

fn lock_managed_tool(root: &Path, id: &str) -> Result<crate::files::FileLock, String> {
    validate_tool_id(id)?;
    ensure_private_directory(root)?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(format!(".install-{id}.lock")))
        .map_err(|error| format!("cannot open tool installation lock: {error}"))?;
    crate::files::lock_exclusive(file, true).map_err(|(_, error)| {
        format!("cannot lock tool {id} (another installation may be running): {error}")
    })
}

pub(super) fn uninstall_managed_tool(
    definition: &ToolDefinition,
    root: &Path,
) -> Result<(), String> {
    let _lock = lock_managed_tool(root, &definition.id)?;
    let destination = root.join(&definition.id);
    let Ok(metadata) = fs::symlink_metadata(&destination) else {
        return Ok(());
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err("managed tool destination is not a real directory".into());
    }
    let removed = root.join(format!(".removed-{}-{}", definition.id, next_temp_suffix()));
    fs::rename(&destination, &removed)
        .map_err(|error| format!("cannot deactivate managed tool: {error}"))?;
    fs::remove_dir_all(&removed)
        .map_err(|error| format!("tool deactivated but cleanup failed: {error}"))?;
    Ok(())
}

pub(super) fn nix_installer_environment() -> BTreeMap<String, OsString> {
    [
        "HOME",
        "PATH",
        "USER",
        "LOGNAME",
        "NIX_REMOTE",
        "SSL_CERT_FILE",
        "NIX_SSL_CERT_FILE",
    ]
    .into_iter()
    .filter_map(|key| env::var_os(key).map(|value| (key.into(), value)))
    .collect()
}

pub(super) fn install_nix_lsp_profile(
    definition: &ToolDefinition,
    root: &Path,
    nix: &Path,
    cancellations: &Receiver<()>,
    report: &mut ToolProgressReporter,
) -> Result<Option<PathBuf>, String> {
    let profile_id = nix_lsp_shell(definition)
        .ok_or("this LSP has no verified artifact for the Dust Nix runtime")?;
    let workspace = prepare_nix_lsp_workspace_at(&root.join("nix-lsp-workspace"))?;
    let profile = workspace.join("profiles").join(profile_id);
    let compatibility = match parse_nix_runtime_environment(&profile) {
        Ok(compatibility) => compatibility,
        Err(_) => {
            // Migrate an old per-server nixpkgs/dev-shell profile before creating
            // the generic runtime out-link. The verified artifact is left untouched
            // until its replacement has passed extraction and probing.
            uninstall_nix_lsp_profile(definition, root)?;
            report("realizing Dust Nix compatibility", 0, None);
            let arguments = vec![
                "build".into(),
                "--no-update-lock-file".into(),
                "--no-write-lock-file".into(),
                "--out-link".into(),
                profile.display().to_string(),
                format!(
                    "path:{}#{NIX_COMPATIBILITY_FLAKE_PACKAGE}",
                    workspace.display()
                ),
            ];
            run_managed(
                nix,
                &arguments,
                &workspace,
                &nix_installer_environment(),
                cancellations,
                None,
            )?;
            parse_nix_runtime_environment(&profile)?
        }
    };
    report("installing verified upstream artifact", 0, None);
    install_tool(
        definition,
        root,
        cancellations,
        report,
        Some(&compatibility),
    )
}

pub(super) fn nix_profile_installed_at(profile: &Path) -> bool {
    fs::symlink_metadata(profile).is_ok_and(|metadata| metadata.file_type().is_symlink())
        && crate::files::canonicalize(profile).is_ok()
}

pub(super) fn uninstall_nix_lsp_profile(
    definition: &ToolDefinition,
    root: &Path,
) -> Result<(), String> {
    let shell =
        nix_lsp_shell(definition).ok_or("this LSP has no Dust Nix compatibility runtime")?;
    let profiles = root.join("nix-lsp-workspace/profiles");
    let metadata = match fs::symlink_metadata(&profiles) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("cannot inspect Dust Nix environments: {error}")),
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err("Dust Nix environment store is not a real directory".into());
    }
    let generation_prefix = format!("{shell}-");
    let mut removed = 0usize;
    for entry in fs::read_dir(&profiles)
        .map_err(|error| format!("cannot read Dust Nix environments: {error}"))?
    {
        let entry =
            entry.map_err(|error| format!("cannot inspect Dust Nix environment: {error}"))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != shell && !(name.starts_with(&generation_prefix) && name.ends_with("-link")) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("cannot inspect Dust Nix environment link: {error}"))?;
        if !metadata.file_type().is_symlink() {
            return Err("Dust Nix environment entry is not a symlink".into());
        }
        fs::remove_file(entry.path())
            .map_err(|error| format!("cannot remove Dust Nix environment: {error}"))?;
        removed = removed.saturating_add(1);
    }
    if removed == 0 && nix_profile_installed_at(&profiles.join(shell)) {
        return Err("Dust Nix environment could not be removed".into());
    }
    Ok(())
}

pub(super) fn uninstall_nix_lsp_tool(
    definition: &ToolDefinition,
    root: &Path,
) -> Result<(), String> {
    // Deactivate the executable first so a profile cleanup failure cannot leave a
    // partially removed tool launchable.
    uninstall_managed_tool(definition, root)?;
    uninstall_nix_lsp_profile(definition, root)
}

#[cfg(test)]
mod installer_output_tests {
    use crate::tools::runtimes::*;

    #[test]
    fn a_failed_installer_reports_the_end_of_what_it_printed() {
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        let output = read_installer_output(reader);
        std::io::Write::write_all(
            &mut writer,
            b"fetching\n\nbuilding\nerror: no such attribute\n\n",
        )
        .expect("write");
        std::io::Write::write_all(&mut writer, b"exit 1\n").expect("write");
        drop(writer);
        assert_eq!(
            output.join().expect("output"),
            ": building error: no such attribute exit 1"
        );
    }

    #[test]
    fn a_silent_installer_adds_nothing_to_the_exit_message() {
        assert_eq!(installer_failure_detail(" \n\n"), "");
    }
}
