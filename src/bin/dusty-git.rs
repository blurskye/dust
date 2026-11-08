//! Standalone host for the same Git screen the editor opens.
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
}
fn main() -> std::process::ExitCode {
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
    let mut terminal = TerminalSession::enter(options, config.theme.clone())?;
    let mut dirty = true;
    loop {
        dirty |= screen.poll();
        if dirty {
            let (w, h) = terminal.size()?;
            let (rect, content) = screen.window_frame(w, h, &config.config.ui.git);
            let mut frame = dust::ui::Frame {
                grid: dust::ui::CellGrid::new(w, h),
                cursor: None,
            };
            GitScreen::compose(&mut frame, rect, &content);
            terminal.draw(&frame)?;
            dirty = false;
        }
        if let Some(event) = terminal.poll_event(Duration::from_millis(30))? {
            dirty = true;
            match screen.event(event) {
                ScreenOutcome::Continue => {}
                ScreenOutcome::Close => break,
                ScreenOutcome::OpenRepository(path) => screen = GitScreen::open(&path)?,
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
