//! Standalone host for the same Git screen the editor opens, and `dusty-git forge …`, the
//! forge layer as JSON for other editors (FORGE.md §5.3).
use clap::Parser;
use dust::git::screen::{GitScreen, ScreenOutcome};
use dust::ui::{TerminalOptions, TerminalSession};
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
}

fn main() -> std::process::ExitCode {
    let args = std::env::args_os().collect::<Vec<_>>();
    if args.get(1).is_some_and(|arg| arg == "forge") {
        return std::process::ExitCode::from(dust::forge::command::main(args[1..].to_vec()));
    }
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dusty-git: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let config = if cli.clean {
        dust::config::load_bundled_config()
    } else {
        dust::config::load_user_config()
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
    // `--page prs --open 13`: the screen's own deep-link entry point.
    if let Some(page) = cli.page.as_deref() {
        screen.open_target(page, cli.open);
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
                ScreenOutcome::OpenRepository(path) => {
                    screen = GitScreen::open(&path)?;
                    screen.set_icon_mode(config.config.ui.explorer.icon_mode);
                }
                ScreenOutcome::OpenFile(path) => {
                    drop(terminal);
                    let editor = std::env::var("VISUAL")
                        .or_else(|_| std::env::var("EDITOR"))
                        .unwrap_or_else(|_| if cfg!(windows) { "notepad" } else { "vi" }.into());
                    #[cfg(unix)]
                    let status = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(format!("exec {editor} \"$1\""))
                        .arg("dusty-git-editor")
                        .arg(&path)
                        .status();
                    #[cfg(not(unix))]
                    let status = std::process::Command::new(editor).arg(&path).status();
                    terminal = TerminalSession::enter(options, config.theme.clone())?;
                    if let Err(e) = status {
                        return Err(e.into());
                    }
                    screen.event(dust::ui::UiEvent::Key {
                        key: dust::editor::input::KeyEvent::character('r'),
                        kind: dust::ui::KeyInputKind::Press,
                    });
                }
            }
        }
    }
    Ok(())
}
