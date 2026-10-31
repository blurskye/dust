#![forbid(unsafe_code)]

use crossterm::{
    cursor::Show,
    execute,
    terminal::{LeaveAlternateScreen, disable_raw_mode},
};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::Parser;
use dust::config::{load_bundled_config, load_user_config};
use dust::editor::{resolve_startup, run_interactive};

#[derive(Debug, Parser)]
#[command(name = "dust", version, about = "Dust modal terminal IDE")]
struct Cli {
    /// Files or one directory to open.
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// Print the resolved startup surface/root without starting a UI.
    #[arg(long)]
    print_startup: bool,

    /// Ignore user configuration and use compiled defaults.
    #[arg(long)]
    clean: bool,

    /// Disable plugins, project execution, and session restoration.
    #[arg(long)]
    safe_mode: bool,

    /// Validate configuration and exit without opening the terminal UI.
    #[arg(long)]
    check_config: bool,

    /// Print bounded runtime and state-directory health information.
    #[arg(long)]
    health: bool,

    /// Print version, toolchain target, and enabled runtime features.
    #[arg(long)]
    verbose_version: bool,

    /// Print the machine-readable public API/schema manifest.
    #[arg(long)]
    api_manifest: bool,

    /// Check a saved API manifest against this build.
    #[arg(long, value_name = "MANIFEST")]
    check_api_compatibility: Option<PathBuf>,

    /// Preview migration of a configuration TOML file.
    #[arg(long, value_name = "PATH")]
    migrate_config: Option<PathBuf>,

    /// Atomically apply --migrate-config instead of printing it.
    #[arg(long, requires = "migrate_config")]
    apply_migration: bool,

    /// Verify a package lock and content-addressed store.
    #[arg(long, value_name = "LOCK")]
    verify_package_lock: Option<PathBuf>,

    /// Content-addressed package store used with --verify-package-lock.
    #[arg(long, value_name = "DIR", requires = "verify_package_lock")]
    package_store: Option<PathBuf>,

    /// Package manager state root for transactional package operations.
    #[arg(long, value_name = "DIR")]
    package_root: Option<PathBuf>,
    /// Remove a package from the active lock.
    #[arg(long, value_name = "NAME", requires = "package_root")]
    package_remove: Option<String>,
    /// Restore the previous package lock.
    #[arg(long, requires = "package_root")]
    package_rollback: bool,
    /// Print the active package lock.
    #[arg(long, requires = "package_root")]
    package_list: bool,

    /// Open files in the Dust that started this program (lazygit's edit key does this).
    #[arg(long, value_name = "FILE", num_args = 1..)]
    remote: Vec<PathBuf>,

    /// The line to open the first --remote file at.
    #[arg(long, value_name = "LINE", requires = "remote")]
    line: Option<usize>,

    /// Serve the subagent tool over MCP for the agent tab TAB, or the one in
    /// `DUST_AGENT_TAB` (Dust starts this itself, or an agent's MCP config names it).
    #[arg(long, value_name = "TAB", hide = true, num_args = 0..=1)]
    agent_tools: Option<Option<u64>>,
}

fn external_tui_fixture() -> ExitCode {
    if crossterm::terminal::enable_raw_mode().is_err() {
        return ExitCode::from(2);
    }
    let mut out = std::io::stdout();
    if out.write_all(b"PRIMARY_START\r\n\x1b[?1049hALT_READY\r\n\x1b[?1h\x1b[?2004h\x1b[?1004h\x1b[?1003h\x1b[?1006h\x1b[38;5;255;48;2;1;2;3;1;4mX\x1b[0m").and_then(|_|out.flush()).is_err(){return ExitCode::from(2)}
    let mut input = [0u8; 4096];
    let mut used = 0;
    while used < input.len() {
        match std::io::stdin().read(&mut input[used..]) {
            Ok(0) | Err(_) => return ExitCode::from(2),
            Ok(n) => {
                used += n;
                let bytes = &input[..used];
                if bytes.windows(3).any(|w| w == b"\x1bOA")
                    && bytes.windows(19).any(|w| w == b"\x1b[200~fixture\x1b[201~")
                {
                    break;
                }
            }
        }
    }
    if out.write_all(b"INPUT_OK\r\n\x1b[?1006l\x1b[?1003l\x1b[?1004l\x1b[?2004l\x1b[?1l\x1b[?1049lPRIMARY_OK\r\n").and_then(|_|out.flush()).is_err(){ExitCode::from(2)}else{ExitCode::SUCCESS}
}

fn main() -> ExitCode {
    #[cfg(unix)]
    {
        let mut arguments = std::env::args_os().skip(1);
        let first = arguments.next();
        if first.as_deref() == Some(std::ffi::OsStr::new(dust::debugger::ADAPTER_BRIDGE_ARGUMENT)) {
            let Some(program) = arguments.next() else {
                return ExitCode::FAILURE;
            };
            let rest: Vec<_> = arguments.collect();
            return match dust::debugger::bridge_adapter_socket(&program, &rest) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("dust: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        if first.as_deref() == Some(std::ffi::OsStr::new(dust::debugger::ADAPTER_SESSION_ARGUMENT))
        {
            let Some(program) = arguments.next() else {
                return ExitCode::FAILURE;
            };
            let rest: Vec<_> = arguments.collect();
            let error = dust::debugger::exec_in_own_session(&program, &rest);
            eprintln!("dust: {error}");
            return ExitCode::FAILURE;
        }
        if let Ok(binary) = std::env::current_exe() {
            dust::debugger::use_adapter_session_launcher(binary);
        }
    }
    if std::env::var("DUST_TEST_EXTERNAL_TUI_FIXTURE").as_deref() == Ok("1") {
        return external_tui_fixture();
    }
    install_crash_hook();
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dust: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(tab) = cli.agent_tools {
        let tab = tab.or_else(|| std::env::var(dust::agent::mcp::TAB_VARIABLE).ok()?.parse().ok());
        let server = std::env::var_os(dust::remote::SERVER_VARIABLE).map(Into::into);
        let profiles = std::env::var(dust::agent::mcp::PROFILES_VARIABLE).unwrap_or_default();
        dust::agent::mcp::serve(server.zip(tab), &profiles)?;
        return Ok(());
    }
    if !cli.remote.is_empty() {
        let server = std::env::var_os(dust::remote::SERVER_VARIABLE)
            .ok_or("--remote only works from a terminal started by Dust")?;
        dust::remote::send(Path::new(&server), &cli.remote, cli.line)?;
        return Ok(());
    }
    if let Some(path) = &cli.check_api_compatibility {
        let report = dust::api::check_compatibility(path, 1024 * 1024)?;
        println!("{}", serde_json::to_string_pretty(&report)?);
        if !report.compatible {
            return Err("API manifest is incompatible".into());
        }
        return Ok(());
    }
    if let Some(root) = &cli.package_root {
        let manager =
            dust::tools::PackageManager::new(root.clone(), 16 * 1024 * 1024, 64 * 1024 * 1024)?;
        if let Some(name) = &cli.package_remove {
            let lock = manager.remove(name)?;
            println!("removed {name}; {} package(s) remain", lock.packages.len());
            return Ok(());
        }
        if cli.package_rollback {
            let lock = manager.rollback()?;
            println!("restored {} package(s)", lock.packages.len());
            return Ok(());
        }
        if cli.package_list {
            println!("{}", serde_json::to_string_pretty(&manager.read_lock()?)?);
            return Ok(());
        }
    }
    if let Some(lock) = &cli.verify_package_lock {
        let store = cli
            .package_store
            .as_ref()
            .ok_or("--package-store is required")?;
        let count = dust::tools::verify_lock_store(lock, store, 16 * 1024 * 1024)?;
        println!("verified {count} package(s)");
        return Ok(());
    }
    if let Some(path) = &cli.migrate_config {
        let metadata = fs::metadata(path)?;
        if metadata.len() > 16 * 1024 * 1024 {
            return Err("configuration exceeds migration ceiling".into());
        }
        let source = fs::read_to_string(path)?;
        let (migrated, changed) = dust::config::migrate_config_source(&source)
            .map_err(|error| format!("migration failed: {error}"))?;
        if cli.apply_migration && changed {
            let temp = path.with_extension(format!("migrate.{}.tmp", std::process::id()));
            let mut options = fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp)?;
            file.write_all(migrated.as_bytes())?;
            file.sync_all()?;
            fs::rename(temp, path)?;
        } else {
            print!("{migrated}");
        }
        return Ok(());
    }
    if cli.api_manifest {
        println!("{}", serde_json::to_string_pretty(&dust::api::manifest()?)?);
        return Ok(());
    }
    if cli.verbose_version {
        println!(
            "dust {} {}-{} component-model=true",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        return Ok(());
    }
    let config = if cli.clean {
        load_bundled_config()
    } else {
        load_user_config()
    };
    if cli.check_config {
        dust::editor::validate_loaded_configuration(config.clone())?;
        println!("configuration valid");
        return Ok(());
    }
    if cli.health {
        let state = dust::config::user_state_directory();
        let mut snapshot = dust::telemetry::HealthSnapshot {
            config_degraded: config.diagnostic.is_some(),
            ..Default::default()
        };
        snapshot.safe_mode =
            cli.safe_mode || previous_crash_count(&state.join("last-crash.json")) >= 2;
        snapshot
            .notes
            .push(format!("state_directory={}", state.display()));
        snapshot
            .notes
            .push(format!("state_directory_present={}", state.is_dir()));
        snapshot.notes.push(format!(
            "previous_crash={}",
            state.join("last-crash.json").is_file()
        ));
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    let current_directory = std::env::current_dir()?;
    let startup = resolve_startup(&cli.paths, &current_directory)?;
    if cli.print_startup {
        println!("{}", startup.describe());
        return Ok(());
    }
    let safe_mode = cli.safe_mode
        || previous_crash_count(&dust::config::user_state_directory().join("last-crash.json")) >= 2;
    run_interactive(startup, config, safe_mode)?;
    Ok(())
}

fn previous_crash_count(path: &Path) -> u64 {
    let Ok(metadata) = fs::metadata(path) else {
        return 0;
    };
    if metadata.len() > 1024 * 1024 {
        return 0;
    }
    let Ok(bytes) = fs::read(path) else { return 0 };
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| value.get("crash_count").and_then(serde_json::Value::as_u64))
        .unwrap_or(1)
}

fn install_crash_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stderr(), LeaveAlternateScreen, Show);
        let state = dust::config::user_state_directory();
        let _ = fs::create_dir_all(&state);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&state, fs::Permissions::from_mode(0o700));
        }
        let target = state.join("last-crash.json");
        let temp = state.join(format!(".crash-{}.tmp", std::process::id()));
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown".into());
        let crash_count = previous_crash_count(&target)
            .saturating_add(1)
            .min(1_000_000);
        let live_path = state.join("live-health.json");
        let live_state = fs::symlink_metadata(&live_path)
            .ok()
            .filter(|metadata| {
                metadata.file_type().is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.len() <= 64 * 1024
            })
            .and_then(|_| fs::read(&live_path).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let bytes=serde_json::to_vec(&serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"platform":format!("{}-{}",std::env::consts::OS,std::env::consts::ARCH),"location":location,"crash_count":crash_count,"pid":std::process::id(),"live_state":live_state})).unwrap_or_default();
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        if let Ok(mut file) = options.open(&temp) {
            if file
                .write_all(&bytes)
                .and_then(|()| file.sync_all())
                .is_ok()
            {
                let _ = fs::rename(&temp, &target);
            } else {
                let _ = fs::remove_file(&temp);
            }
        }
        previous(info);
    }));
}
