//! Standalone host for the same Git screen the editor opens, and `dusty-git forge …`, the
//! forge layer as JSON for other editors. `dusty-git open <link>` starts on
//! the pull request, issue or run a web address or `#123` names.
use clap::Parser;
use dusty_git::screen::{GitScreen, ScreenOutcome};
use dusty_ui::{TerminalOptions, TerminalSession};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "dusty-git",
    version,
    about = "Dusty Git — native Git workspace"
)]
struct Cli {
    /// Repository or a directory inside it.
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Use bundled configuration instead of the editor's user theme/settings.
    #[arg(long)]
    clean: bool,
    /// Start on a forge tab: git, prs (mrs), issues or actions.
    #[arg(long, value_parser = ["git", "prs", "mrs", "issues", "actions"])]
    page: Option<String>,
    /// Open this pull request or issue once the list shows it (with --page prs|issues).
    #[arg(long, value_name = "N")]
    open: Option<u64>,
    /// Open a pull request, merge request, issue or run by its web address or #number
    /// (`dusty-git open <link> [PATH]` is the same).
    #[arg(long, value_name = "URL|#N")]
    link: Option<String>,
}

/// `dusty-git open <link> [PATH] [--clean]` → `dusty-git --link <link> [PATH] [--clean]`.
fn open_arguments(args: Vec<std::ffi::OsString>) -> Result<Vec<std::ffi::OsString>, String> {
    if args.get(1).is_none_or(|arg| arg != "open") {
        return Ok(args);
    }
    let Some(link) = args
        .get(2)
        .filter(|arg| !arg.to_string_lossy().starts_with("--"))
    else {
        return Err("usage: dusty-git open <url|#number> [PATH]".into());
    };
    let mut rewritten = vec![args[0].clone(), "--link".into(), link.clone()];
    rewritten.extend(args[3..].iter().cloned());
    Ok(rewritten)
}

fn main() -> std::process::ExitCode {
    let args = std::env::args_os().collect::<Vec<_>>();
    if args.get(1).is_some_and(|arg| arg == "forge") {
        return std::process::ExitCode::from(dusty_forge::command::main(args[1..].to_vec()));
    }
    // A reopened Git tab paints its last status, History and branches before git answers.
    dusty_git::screen::enable_warm_cache();
    let args = match open_arguments(args) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("dusty-git: {error}");
            return std::process::ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dusty-git: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
fn run(args: Vec<std::ffi::OsString>) -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse_from(args);
    let config = if cli.clean {
        dusty_ui::config::load_bundled_config()
    } else {
        dusty_ui::config::load_user_config()
    };
    let options = TerminalOptions {
        color_mode: config.config.ui.color_mode,
        alternate_screen: true,
        bracketed_paste: true,
        focus_events: true,
        mouse: false,
    };
    let mut screen = GitScreen::open(&cli.path)?;
    screen.set_icon_mode(config.config.ui.explorer.icon_mode);
    screen.configure_input(&config.config);
    // `--page prs --open 13`: the screen's own deep-link entry point.
    if let Some(page) = cli.page.as_deref() {
        screen.open_target(page, cli.open);
    }
    // `open <link>`: checked before the terminal is taken, so a bad link is a plain error.
    if let Some(link) = cli.link.as_deref() {
        screen.open_link(link)?;
    }
    let mut terminal = TerminalSession::enter(options, config.theme.clone())?;
    let mut dirty = true;
    loop {
        dirty |= screen.poll();
        if dirty {
            let (w, h) = terminal.size()?;
            let frame = screen.frame(w, h);
            terminal.draw(&frame)?;
            dirty = false;
        }
        if let Some(event) = terminal.poll_event(Duration::from_millis(30))? {
            dirty = true;
            match screen.event(event) {
                ScreenOutcome::Continue => {}
                ScreenOutcome::Close => break,
                ScreenOutcome::Copy(text) => terminal.copy_to_clipboard(&text)?,
                ScreenOutcome::OpenRepository(path) => {
                    screen = GitScreen::open(&path)?;
                    screen.set_icon_mode(config.config.ui.explorer.icon_mode);
                    screen.configure_input(&config.config);
                }
                outcome @ (ScreenOutcome::OpenFile(_) | ScreenOutcome::OpenFileAt(..)) => {
                    let (path, line) = match outcome {
                        ScreenOutcome::OpenFileAt(path, line) => (path, Some(line)),
                        ScreenOutcome::OpenFile(path) => (path, None),
                        _ => unreachable!("matched above"),
                    };
                    drop(terminal);
                    let editor = std::env::var("VISUAL")
                        .or_else(|_| std::env::var("EDITOR"))
                        .unwrap_or_else(|_| if cfg!(windows) { "notepad" } else { "vi" }.into());
                    // `+N` puts vi, Vim, Neovim, nano, Emacs, Helix and Kakoune on that line.
                    #[cfg(unix)]
                    let status = match line {
                        Some(line) => std::process::Command::new("sh")
                            .arg("-c")
                            .arg(format!("exec {editor} \"+$2\" \"$1\""))
                            .arg("dusty-git-editor")
                            .arg(&path)
                            .arg(line.to_string())
                            .status(),
                        None => std::process::Command::new("sh")
                            .arg("-c")
                            .arg(format!("exec {editor} \"$1\""))
                            .arg("dusty-git-editor")
                            .arg(&path)
                            .status(),
                    };
                    #[cfg(not(unix))]
                    let status = {
                        let _ = line;
                        std::process::Command::new(editor).arg(&path).status()
                    };
                    terminal = TerminalSession::enter(options, config.theme.clone())?;
                    if let Err(e) = status {
                        return Err(e.into());
                    }
                    screen.event(dusty_ui::UiEvent::Key {
                        key: dusty_core::editor::input::KeyEvent::character('r'),
                        kind: dusty_ui::KeyInputKind::Press,
                    });
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_becomes_the_link_flag() {
        let args = |list: &[&str]| {
            list.iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            open_arguments(args(&["dusty-git", "open", "#12", "repo", "--clean"])).unwrap(),
            args(&["dusty-git", "--link", "#12", "repo", "--clean"])
        );
        assert_eq!(
            open_arguments(args(&["dusty-git", "repo"])).unwrap(),
            args(&["dusty-git", "repo"])
        );
        assert!(open_arguments(args(&["dusty-git", "open"])).is_err());
        let cli = Cli::parse_from(args(&[
            "dusty-git",
            "--link",
            "https://github.com/o/r/pull/3",
            "repo",
        ]));
        assert_eq!(cli.link.as_deref(), Some("https://github.com/o/r/pull/3"));
        assert_eq!(cli.path, std::path::PathBuf::from("repo"));
    }
}
