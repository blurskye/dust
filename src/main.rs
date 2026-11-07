//! Command-line options and application startup.

#![forbid(unsafe_code)]

use crossterm::cursor::Show;

use crossterm::execute;

use crossterm::terminal::LeaveAlternateScreen;

use crossterm::terminal::disable_raw_mode;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use dust::app::run_interactive;
use dust::app::startup::resolve_startup;
use dust::config::load_bundled_config;
use dust::config::load_user_config;

#[derive(Debug, Parser)]
#[command(name = "dust", version, about = "Dust modal terminal IDE")]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,

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

#[derive(Debug, clap::Subcommand)]
enum CliCommand {
    /// Create a WebAssembly component plugin.
    Plugin {
        #[command(subcommand)]
        command: PluginCommand,
    },
}

#[derive(Debug, clap::Subcommand)]
enum PluginCommand {
    /// Create a ready-to-build Rust plugin in a new NAME directory.
    New { name: String },
}

fn main() -> ExitCode {
    #[cfg(unix)]
    {
        let mut arguments = std::env::args_os().skip(1);
        let first = arguments.next();
        if first.as_deref()
            == Some(std::ffi::OsStr::new(
                dust::debugger::ADAPTER_BRIDGE_ARGUMENT,
            ))
        {
            let Some(program) = arguments.next() else {
                return ExitCode::FAILURE;
            };
            let rest: Vec<_> = arguments.collect();
            return match dust::debugger::bridge_adapter_socket(&program, &rest) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("dust: {}", error.to_string().escape_debug());
                    ExitCode::FAILURE
                }
            };
        }
        if first.as_deref()
            == Some(std::ffi::OsStr::new(
                dust::debugger::ADAPTER_SESSION_ARGUMENT,
            ))
        {
            let Some(program) = arguments.next() else {
                return ExitCode::FAILURE;
            };
            let rest: Vec<_> = arguments.collect();
            let error = dust::debugger::exec_in_own_session(&program, &rest);
            eprintln!("dust: {}", error.to_string().escape_debug());
            return ExitCode::FAILURE;
        }
        if let Ok(binary) = std::env::current_exe() {
            dust::debugger::use_adapter_session_launcher(binary);
        }
    }
    install_crash_hook();
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("dust: {}", error.to_string().escape_debug());
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(CliCommand::Plugin {
        command: PluginCommand::New { name },
    }) = cli.command
    {
        let directory = dust::plugins::scaffold::create(&std::env::current_dir()?, &name)?;
        println!(
            "Created {} — see README.md for build, pack and load instructions",
            directory.display()
        );
        return Ok(());
    }
    if let Some(tab) = cli.agent_tools {
        let tab = tab.or_else(|| {
            std::env::var(dust::agent::mcp::TAB_VARIABLE)
                .ok()?
                .parse()
                .ok()
        });
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
            dust::tools::PackageStore::new(root.clone(), 16 * 1024 * 1024, 64 * 1024 * 1024)?;
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
        let count =
            dust::tools::verify_lock_store(lock, store, 16 * 1024 * 1024, 64 * 1024 * 1024)?;
        println!("verified {count} package(s)");
        return Ok(());
    }
    if let Some(path) = &cli.migrate_config {
        let path = dust::files::canonicalize(path)?;
        if !fs::metadata(&path)?.is_file() {
            return Err("configuration must be a regular file".into());
        }
        let file = fs::File::open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err("configuration must be a regular file".into());
        }
        const MAXIMUM: u64 = 16 * 1024 * 1024;
        if metadata.len() > MAXIMUM {
            return Err("configuration exceeds migration limit".into());
        }
        let mut bytes = Vec::new();
        file.take(MAXIMUM + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAXIMUM {
            return Err("configuration exceeds migration limit".into());
        }
        let source = String::from_utf8(bytes)?;
        let (migrated, changed) = dust::config::migrate_config_source(&source)
            .map_err(|error| format!("migration failed: {error}"))?;
        if cli.apply_migration && changed {
            dust::config::apply_config_migration(&path, &source, &migrated)?;
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
        dust::app::validate_loaded_configuration(config.clone())?;
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
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_file() {
        return 0;
    }
    let Ok(file) = fs::File::open(path) else {
        return 0;
    };
    let Ok(metadata) = file.metadata() else {
        return 0;
    };
    const MAXIMUM: u64 = 1024 * 1024;
    if !metadata.is_file() || metadata.len() > MAXIMUM {
        return 0;
    }
    let mut bytes = Vec::new();
    if file.take(MAXIMUM + 1).read_to_end(&mut bytes).is_err() || bytes.len() as u64 > MAXIMUM {
        return 0;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_subcommand_preserves_file_arguments() {
        assert!(
            matches!(Cli::try_parse_from(["dust", "plugin", "new", "words"]).unwrap().command, Some(CliCommand::Plugin { command: PluginCommand::New { name } }) if name == "words")
        );
        assert_eq!(
            Cli::try_parse_from(["dust", "file.rs"]).unwrap().paths,
            vec![PathBuf::from("file.rs")]
        );
        assert_eq!(
            Cli::try_parse_from(["dust", "--", "plugin"]).unwrap().paths,
            vec![PathBuf::from("plugin")]
        );
    }

    #[test]
    fn crash_counts_accept_bounded_regular_reports() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("last-crash.json");
        assert_eq!(previous_crash_count(&path), 0);
        fs::write(&path, r#"{"crash_count":3}"#).unwrap();
        assert_eq!(previous_crash_count(&path), 3);
        fs::write(&path, "invalid report").unwrap();
        assert_eq!(previous_crash_count(&path), 1);
        fs::File::create(&path)
            .unwrap()
            .set_len(1024 * 1024 + 1)
            .unwrap();
        assert_eq!(previous_crash_count(&path), 0);
        assert_eq!(previous_crash_count(root.path()), 0);
    }

    #[cfg(unix)]
    #[test]
    fn crash_counts_reject_special_files_and_symlinks() {
        use nix::sys::stat::Mode;
        use nix::unistd::mkfifo;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("last-crash.json");
        std::os::unix::fs::symlink("/dev/zero", &path).unwrap();
        assert_eq!(previous_crash_count(&path), 0);
        fs::remove_file(&path).unwrap();
        mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        assert_eq!(previous_crash_count(&path), 0);
        assert_eq!(previous_crash_count(Path::new("/dev/zero")), 0);
    }
}
