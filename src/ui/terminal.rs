use std::{
    fs::File,
    io::{self, IsTerminal, Stdout, Write},
    os::fd::AsFd,
    panic,
    sync::Once,
    time::Duration,
};

use crossterm::{
    cursor::{Hide, MoveTo, SetCursorStyle, Show},
    event::{
        self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, Event, KeyCode as CtKeyCode, KeyEventKind,
        KeyModifiers, MouseButton, MouseEventKind,
    },
    execute, queue,
    style::{
        Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    },
    terminal::{
        self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate,
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    },
};

use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
};

use crate::config::{ColorMode, ThemeAttribute, ThemeColor, ThemeConfig};
use crate::editor::input::{KeyCode, KeyEvent, Modifiers};
use thiserror::Error;

use crate::ui::{Frame, Style};

static PANIC_HOOK: Once = Once::new();

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = chunk
            .iter()
            .enumerate()
            .fold(0u32, |value, (index, byte)| value | u32::from(*byte) << (16 - 8 * index));
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(ALPHABET[(value >> (18 - 6 * index) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn install_panic_restore_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |information| {
            let _ = restore_terminal(&mut io::stderr());
            previous(information);
        }));
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalOptions {
    pub color_mode: ColorMode,
    pub alternate_screen: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub mouse: bool,
}

pub struct TerminalSession {
    stdout: Stdout,
    /// The terminal crossterm reads keys from when stdin is not one.
    input: Option<File>,
    previous: Option<Frame>,
    options: TerminalOptions,
    theme: ThemeConfig,
    color_tier: ColorTier,
    color_source: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ColorTier {
    Ansi16,
    Ansi256,
    TrueColor,
}

impl TerminalSession {
    pub fn enter(options: TerminalOptions, theme: ThemeConfig) -> Result<Self, TerminalError> {
        install_panic_restore_hook();
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        let enter_result = (|| -> io::Result<()> {
            if options.alternate_screen {
                execute!(stdout, EnterAlternateScreen)?;
            }
            if options.bracketed_paste {
                execute!(stdout, EnableBracketedPaste)?;
            }
            if options.focus_events {
                execute!(stdout, EnableFocusChange)?;
            }
            if options.mouse {
                execute!(stdout, EnableMouseCapture)?;
            }
            execute!(stdout, Hide, Clear(ClearType::All))
        })();
        if let Err(error) = enter_result {
            let _ = restore_terminal(&mut stdout);
            return Err(TerminalError::Io(error));
        }
        let (color_tier, color_source) = configured_color_tier(options.color_mode);
        Ok(Self {
            stdout,
            input: if io::stdin().is_terminal() {
                None
            } else {
                File::open("/dev/tty").ok()
            },
            previous: None,
            options,
            theme,
            color_tier,
            color_source,
        })
    }

    /// Put text on the system clipboard through the terminal (OSC 52), for when no clipboard
    /// program is installed, such as over SSH.
    pub fn copy_to_clipboard(&mut self, text: &str) -> Result<(), TerminalError> {
        // Terminals cap the sequence; very large yanks stay internal.
        if text.len() > 1024 * 1024 {
            return Ok(());
        }
        write!(self.stdout, "\x1b]52;c;{}\x07", base64(text.as_bytes()))?;
        self.stdout.flush()?;
        Ok(())
    }

    pub fn invalidate(&mut self) {
        self.previous = None;
    }
    pub fn set_color_mode(&mut self, mode: ColorMode) {
        if self.options.color_mode == mode {
            return;
        }
        self.options.color_mode = mode;
        (self.color_tier, self.color_source) = configured_color_tier(mode);
        self.previous = None;
    }

    #[must_use]
    pub fn capability_summary(&self) -> String {
        let tier = match self.color_tier {
            ColorTier::Ansi16 => "ansi16",
            ColorTier::Ansi256 => "ansi256",
            ColorTier::TrueColor => "truecolor",
        };
        format!("color={tier} source={}", self.color_source)
    }

    pub fn set_mouse(&mut self, enabled: bool) -> Result<(), TerminalError> {
        if enabled == self.options.mouse {
            return Ok(());
        }
        if enabled {
            execute!(self.stdout, EnableMouseCapture)?;
        } else {
            execute!(self.stdout, DisableMouseCapture)?;
        }
        self.options.mouse = enabled;
        Ok(())
    }

    pub fn set_theme(&mut self, theme: &ThemeConfig) {
        if &self.theme != theme {
            self.theme = theme.clone();
            self.previous = None;
        }
    }

    /// Whether the next frame must be drawn in full (first frame, theme, colors or resize).
    #[must_use]
    pub const fn needs_full_draw(&self) -> bool {
        self.previous.is_none()
    }

    pub fn size(&self) -> Result<(u16, u16), TerminalError> {
        terminal::size().map_err(TerminalError::Io)
    }

    pub fn poll_event(&self, timeout: Duration) -> Result<Option<UiEvent>, TerminalError> {
        // crossterm waits in whole milliseconds and then spins on zero-timeout polls for the
        // rest of the timeout, about a millisecond of busy work on every idle wake. So the
        // wait happens here and crossterm only reads what is already there. A timeout this
        // short still makes crossterm hand out events it parsed but has not returned yet.
        const READY: Duration = Duration::from_micros(1);
        if !event::poll(READY)? && !(self.wait_for_input(timeout)? && event::poll(READY)?) {
            return Ok(None);
        }
        loop {
            if let Some(event) = normalize_event(event::read()?) {
                return Ok(Some(event));
            }
            if !event::poll(READY)? {
                return Ok(None);
            }
        }
    }

    /// Wait until the terminal has input or a signal arrives; a resize comes as SIGWINCH,
    /// which crossterm turns into an event.
    fn wait_for_input(&self, timeout: Duration) -> io::Result<bool> {
        let stdin = io::stdin();
        let terminal = match &self.input {
            Some(tty) => tty.as_fd(),
            None => stdin.as_fd(),
        };
        let timeout =
            PollTimeout::try_from(timeout.as_micros().div_ceil(1000)).unwrap_or(PollTimeout::MAX);
        match poll(&mut [PollFd::new(terminal, PollFlags::POLLIN)], timeout) {
            Ok(ready) => Ok(ready > 0),
            Err(Errno::EINTR) => Ok(true),
            Err(error) => Err(error.into()),
        }
    }

    /// Paint what changed since the last frame, and say whether anything did.
    pub fn draw(&mut self, frame: &Frame) -> Result<bool, TerminalError> {
        let mut output = Vec::new();
        if !queue_frame(
            &mut output,
            frame,
            self.previous.as_ref(),
            &self.theme,
            self.color_tier,
        )? {
            return Ok(false);
        }
        self.stdout.write_all(&output)?;
        self.stdout.flush()?;
        self.previous = Some(frame.clone());
        Ok(true)
    }
}

// Paint with the hardware cursor hidden, then publish only its final location.
// Synchronized updates also prevent supported terminals from exposing partial frames.
fn queue_frame(
    writer: &mut impl Write,
    frame: &Frame,
    previous: Option<&Frame>,
    theme: &ThemeConfig,
    color_tier: ColorTier,
) -> io::Result<bool> {
    if previous.is_some_and(|old| old.grid == frame.grid && old.cursor == frame.cursor) {
        return Ok(false);
    }
    let size_changed = previous.is_none_or(|old| {
        old.grid.width() != frame.grid.width() || old.grid.height() != frame.grid.height()
    });
    queue!(writer, BeginSynchronizedUpdate, Hide)?;
    if size_changed {
        queue!(writer, Clear(ClearType::All))?;
    }
    // Where the terminal cursor is known to be, and the style last sent. A run of changed
    // cells is written with one move and one style; the move is only skipped after plain
    // ASCII, whose width every terminal agrees on.
    let mut at = None;
    let mut style = None;
    for y in 0..frame.grid.height() {
        for x in 0..frame.grid.width() {
            let cell = frame.grid.get(x, y).expect("grid bounds");
            if cell.continuation {
                continue;
            }
            let unchanged = previous
                .and_then(|old| old.grid.get(x, y))
                .is_some_and(|old| old == cell);
            if unchanged && !size_changed {
                continue;
            }
            if at != Some((x, y)) {
                queue!(writer, MoveTo(x, y))?;
            }
            if style != Some(cell.style) {
                queue_style(writer, cell.style, theme, color_tier)?;
                style = Some(cell.style);
            }
            queue!(writer, Print(&cell.symbol))?;
            let plain = cell.symbol.len() == 1 && cell.symbol.as_bytes()[0].is_ascii_graphic()
                || cell.symbol == " ";
            at = plain.then(|| (x + 1, y));
        }
    }
    queue!(writer, ResetColor, SetAttribute(Attribute::Reset))?;
    if let Some((x, y, shape)) = frame.cursor {
        queue!(writer, MoveTo(x, y), shape, Show)?;
    }
    queue!(writer, EndSynchronizedUpdate)?;
    Ok(true)
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = execute!(self.stdout, EndSynchronizedUpdate);
        if self.options.mouse {
            let _ = execute!(self.stdout, DisableMouseCapture);
        }
        if self.options.focus_events {
            let _ = execute!(self.stdout, DisableFocusChange);
        }
        if self.options.bracketed_paste {
            let _ = execute!(self.stdout, DisableBracketedPaste);
        }
        if self.options.alternate_screen {
            let _ = execute!(self.stdout, LeaveAlternateScreen);
        }
        let _ = restore_terminal(&mut self.stdout);
    }
}

fn restore_terminal(writer: &mut impl Write) -> io::Result<()> {
    let _ = disable_raw_mode();
    execute!(
        writer,
        EndSynchronizedUpdate,
        ResetColor,
        SetAttribute(Attribute::Reset),
        SetCursorStyle::DefaultUserShape,
        Show,
        DisableBracketedPaste,
        DisableFocusChange,
        LeaveAlternateScreen
    )
}

fn queue_style(
    writer: &mut impl Write,
    style: Style,
    theme: &ThemeConfig,
    tier: ColorTier,
) -> io::Result<()> {
    if let Style::Terminal(pty) = style {
        let text = &theme.styles.terminal_text;
        let background = &theme.styles.terminal_background;
        let (pty_foreground, pty_background) = if pty.reverse {
            (pty.background, pty.foreground)
        } else {
            (pty.foreground, pty.background)
        };
        let foreground = pty_foreground.map_or_else(
            || {
                terminal_color(tier_color(
                    text.foreground,
                    text.foreground_256,
                    text.foreground_16,
                    tier,
                ))
            },
            |color| terminal_pty_color(color, tier),
        );
        let background = pty_background.map_or_else(
            || {
                terminal_color(tier_color(
                    background.background,
                    background.background_256,
                    background.background_16,
                    tier,
                ))
            },
            |color| terminal_pty_color(color, tier),
        );
        queue!(
            writer,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(foreground),
            SetBackgroundColor(background)
        )?;
        if pty.bold {
            queue!(writer, SetAttribute(Attribute::Bold))?;
        }
        if pty.dim {
            queue!(writer, SetAttribute(Attribute::Dim))?;
        }
        if pty.italic {
            queue!(writer, SetAttribute(Attribute::Italic))?;
        }
        if pty.underline {
            queue!(writer, SetAttribute(Attribute::Underlined))?;
        }
        if pty.strikethrough {
            queue!(writer, SetAttribute(Attribute::CrossedOut))?;
        }
        return Ok(());
    }
    let configured = match style {
        Style::Default => &theme.styles.default,
        Style::Muted => &theme.styles.muted,
        Style::Accent => &theme.styles.accent,
        Style::Status => &theme.styles.status,
        Style::StatusAccent => &theme.styles.status_accent,
        Style::Selection => &theme.styles.selection,
        Style::Search => &theme.styles.search,
        Style::Error => &theme.styles.error,
        Style::Warning => &theme.styles.warning,
        Style::Border => &theme.styles.border,
        Style::BufferLineBackground => &theme.styles.buffer_line_background,
        Style::BufferLineActive => &theme.styles.buffer_line_active,
        Style::BufferLineInactive => &theme.styles.buffer_line_inactive,
        Style::BufferLineDirty => &theme.styles.buffer_line_dirty,
        Style::BufferLineIcon => &theme.styles.buffer_line_icon,
        Style::BufferLinePin => &theme.styles.buffer_line_pin,
        Style::BufferLineDiagnostic => &theme.styles.buffer_line_diagnostic,
        Style::BufferLineClose => &theme.styles.buffer_line_close,
        Style::BufferLineSeparator => &theme.styles.buffer_line_separator,
        Style::KeyHintBackground => &theme.styles.key_hint_background,
        Style::KeyHintBorder => &theme.styles.key_hint_border,
        Style::KeyHintKey => &theme.styles.key_hint_key,
        Style::KeyHintGroup => &theme.styles.key_hint_group,
        Style::KeyHintCommand => &theme.styles.key_hint_command,
        Style::KeyHintPrefix => &theme.styles.key_hint_prefix,
        Style::DocumentationBackground => &theme.styles.documentation_background,
        Style::DocumentationBorder => &theme.styles.documentation_border,
        Style::DocumentationTitle => &theme.styles.documentation_title,
        Style::DocumentationText => &theme.styles.documentation_text,
        Style::GitSignAdded => &theme.styles.git_sign_added,
        Style::GitSignChanged => &theme.styles.git_sign_changed,
        Style::GitSignDeleted => &theme.styles.git_sign_deleted,
        Style::ExplorerBackground => &theme.styles.explorer_background,
        Style::ExplorerHeader => &theme.styles.explorer_header,
        Style::ExplorerCount => &theme.styles.explorer_count,
        Style::ExplorerTree => &theme.styles.explorer_tree,
        Style::ExplorerIcon => &theme.styles.explorer_icon,
        Style::ExplorerDirectory => &theme.styles.explorer_directory,
        Style::ExplorerFile => &theme.styles.explorer_file,
        Style::ExplorerSymlink => &theme.styles.explorer_symlink,
        Style::ExplorerGit => &theme.styles.explorer_git,
        Style::ExplorerDiagnostic => &theme.styles.explorer_diagnostic,
        Style::ExplorerDecoration => &theme.styles.explorer_decoration,
        Style::ExplorerSelection => &theme.styles.explorer_selection,
        Style::ExplorerBorder => &theme.styles.explorer_border,
        Style::TerminalBackground => &theme.styles.terminal_background,
        Style::TerminalBorder => &theme.styles.terminal_border,
        Style::TerminalTitle => &theme.styles.terminal_title,
        Style::TerminalStatus => &theme.styles.terminal_status,
        Style::TerminalStatusAccent => &theme.styles.terminal_status_accent,
        Style::Terminal(_) => unreachable!("terminal cells are handled above"),
        Style::SyntaxComment => &theme.styles.syntax_comment,
        Style::SyntaxString => &theme.styles.syntax_string,
        Style::SyntaxNumber => &theme.styles.syntax_number,
        Style::SyntaxKeyword => &theme.styles.syntax_keyword,
        Style::SyntaxType => &theme.styles.syntax_type,
        Style::SyntaxVariable => &theme.styles.syntax_variable,
    };
    queue!(
        writer,
        SetAttribute(Attribute::Reset),
        SetForegroundColor(terminal_color(tier_color(
            configured.foreground,
            configured.foreground_256,
            configured.foreground_16,
            tier,
        ))),
        SetBackgroundColor(terminal_color(tier_color(
            configured.background,
            configured.background_256,
            configured.background_16,
            tier,
        )))
    )?;
    for attribute in &configured.attributes {
        queue!(writer, SetAttribute(terminal_attribute(*attribute)))?;
    }
    Ok(())
}

fn configured_color_tier(mode: ColorMode) -> (ColorTier, &'static str) {
    match mode {
        ColorMode::Auto => (detect_color_tier(), "environment-fallback"),
        ColorMode::Ansi16 => (ColorTier::Ansi16, "configured"),
        ColorMode::Ansi256 => (ColorTier::Ansi256, "configured"),
        ColorMode::TrueColor => (ColorTier::TrueColor, "configured"),
    }
}

fn detect_color_tier() -> ColorTier {
    let color_term = std::env::var("COLORTERM").unwrap_or_default();
    if color_term.eq_ignore_ascii_case("truecolor") || color_term.eq_ignore_ascii_case("24bit") {
        ColorTier::TrueColor
    } else if std::env::var("TERM").is_ok_and(|term| term.contains("256color")) {
        ColorTier::Ansi256
    } else {
        ColorTier::Ansi16
    }
}

fn tier_color(
    primary: ThemeColor,
    ansi_256: Option<ThemeColor>,
    ansi_16: Option<ThemeColor>,
    tier: ColorTier,
) -> ThemeColor {
    match tier {
        ColorTier::TrueColor => primary,
        ColorTier::Ansi256 => ansi_256.or(ansi_16).unwrap_or(primary),
        ColorTier::Ansi16 => ansi_16.unwrap_or(primary),
    }
}

fn terminal_pty_color(color: crate::ui::PtyColor, tier: ColorTier) -> Color {
    match (color, tier) {
        (crate::ui::PtyColor::Indexed(index), ColorTier::Ansi16) => terminal_ansi_color(index % 16),
        (crate::ui::PtyColor::Indexed(index), _) if index > 15 => Color::AnsiValue(index),
        (crate::ui::PtyColor::Indexed(index), _) => terminal_ansi_color(index),
        (crate::ui::PtyColor::Rgb(r, g, b), ColorTier::TrueColor) => Color::Rgb { r, g, b },
        (crate::ui::PtyColor::Rgb(r, g, b), ColorTier::Ansi256) => {
            Color::AnsiValue(16 + 36 * (r / 51) + 6 * (g / 51) + (b / 51))
        }
        (crate::ui::PtyColor::Rgb(r, g, b), ColorTier::Ansi16) => terminal_ansi_color(
            ((r > 127) as u8) | (((g > 127) as u8) << 1) | (((b > 127) as u8) << 2),
        ),
    }
}
fn terminal_ansi_color(index: u8) -> Color {
    match index.min(15) {
        0 => Color::Black,
        1 => Color::DarkRed,
        2 => Color::DarkGreen,
        3 => Color::DarkYellow,
        4 => Color::DarkBlue,
        5 => Color::DarkMagenta,
        6 => Color::DarkCyan,
        7 => Color::Grey,
        8 => Color::DarkGrey,
        9 => Color::Red,
        10 => Color::Green,
        11 => Color::Yellow,
        12 => Color::Blue,
        13 => Color::Magenta,
        14 => Color::Cyan,
        _ => Color::White,
    }
}

fn terminal_color(color: ThemeColor) -> Color {
    match color {
        ThemeColor::Reset => Color::Reset,
        ThemeColor::Black => Color::Black,
        ThemeColor::DarkGrey => Color::DarkGrey,
        ThemeColor::Red => Color::Red,
        ThemeColor::DarkRed => Color::DarkRed,
        ThemeColor::Green => Color::Green,
        ThemeColor::DarkGreen => Color::DarkGreen,
        ThemeColor::Yellow => Color::Yellow,
        ThemeColor::DarkYellow => Color::DarkYellow,
        ThemeColor::Blue => Color::Blue,
        ThemeColor::DarkBlue => Color::DarkBlue,
        ThemeColor::Magenta => Color::Magenta,
        ThemeColor::DarkMagenta => Color::DarkMagenta,
        ThemeColor::Cyan => Color::Cyan,
        ThemeColor::DarkCyan => Color::DarkCyan,
        ThemeColor::White => Color::White,
        ThemeColor::Grey => Color::Grey,
        ThemeColor::Indexed(value) => Color::AnsiValue(value),
        ThemeColor::Rgb(red, green, blue) => Color::Rgb {
            r: red,
            g: green,
            b: blue,
        },
    }
}

fn terminal_attribute(attribute: ThemeAttribute) -> Attribute {
    match attribute {
        ThemeAttribute::Bold => Attribute::Bold,
        ThemeAttribute::Dim => Attribute::Dim,
        ThemeAttribute::Italic => Attribute::Italic,
        ThemeAttribute::Underlined => Attribute::Underlined,
        ThemeAttribute::Reverse => Attribute::Reverse,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyInputKind {
    Press,
    Repeat,
    Release,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UiEvent {
    Key {
        key: KeyEvent,
        kind: KeyInputKind,
    },
    Paste(String),
    Resize(u16, u16),
    FocusGained,
    FocusLost,
    Mouse {
        key: KeyEvent,
        column: u16,
        row: u16,
    },
}

#[must_use]
pub fn normalize_event(event: Event) -> Option<UiEvent> {
    match event {
        Event::Key(key) => {
            let code = match key.code {
                // Legacy terminal protocols encode Ctrl+\\ as 0x1c and
                // Ctrl+/ (Ctrl+_) as 0x1f. Crossterm exposes those bytes as
                // Ctrl+4 and Ctrl+7; restore the conventional logical keys so
                // configurable bindings behave the same with and without an
                // enhanced keyboard protocol.
                CtKeyCode::Char('4') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    KeyCode::Char('\\')
                }
                CtKeyCode::Char('7') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    KeyCode::Char('/')
                }
                CtKeyCode::Char(character) => KeyCode::Char(character),
                CtKeyCode::Enter => KeyCode::Enter,
                CtKeyCode::Esc => KeyCode::Escape,
                CtKeyCode::Backspace => KeyCode::Backspace,
                CtKeyCode::Tab | CtKeyCode::BackTab => KeyCode::Tab,
                CtKeyCode::Left => KeyCode::Left,
                CtKeyCode::Right => KeyCode::Right,
                CtKeyCode::Up => KeyCode::Up,
                CtKeyCode::Down => KeyCode::Down,
                CtKeyCode::Home => KeyCode::Home,
                CtKeyCode::End => KeyCode::End,
                CtKeyCode::Delete => KeyCode::Delete,
                _ => return None,
            };
            // Crossterm's Char carries the translated text identity. Uppercase
            // and punctuation therefore already encode the ordinary Shift
            // transformation (for example `R` and `!`). Keep Shift for
            // lowercase/digit/space enhanced-protocol events where it is an
            // independently meaningful modifier.
            let shift_is_encoded_in_character = matches!(code, KeyCode::Char(character)
                if character.is_uppercase() || (!character.is_alphanumeric() && !character.is_whitespace()));
            Some(UiEvent::Key {
                key: KeyEvent {
                    code,
                    modifiers: Modifiers {
                        control: key.modifiers.contains(KeyModifiers::CONTROL),
                        alt: key.modifiers.contains(KeyModifiers::ALT),
                        shift: key.modifiers.contains(KeyModifiers::SHIFT)
                            && !shift_is_encoded_in_character,
                    },
                },
                kind: match key.kind {
                    KeyEventKind::Press => KeyInputKind::Press,
                    KeyEventKind::Repeat => KeyInputKind::Repeat,
                    KeyEventKind::Release => KeyInputKind::Release,
                },
            })
        }
        Event::Paste(text) => Some(UiEvent::Paste(text)),
        Event::Resize(width, height) => Some(UiEvent::Resize(width, height)),
        Event::FocusGained => Some(UiEvent::FocusGained),
        Event::FocusLost => Some(UiEvent::FocusLost),
        Event::Mouse(mouse) => {
            let code = match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => KeyCode::MouseLeftDown,
                MouseEventKind::Up(MouseButton::Left) => KeyCode::MouseLeftUp,
                MouseEventKind::Down(MouseButton::Right) => KeyCode::MouseRightDown,
                MouseEventKind::Up(MouseButton::Right) => KeyCode::MouseRightUp,
                MouseEventKind::Down(MouseButton::Middle) => KeyCode::MouseMiddleDown,
                MouseEventKind::Up(MouseButton::Middle) => KeyCode::MouseMiddleUp,
                MouseEventKind::Drag(MouseButton::Left) => KeyCode::MouseLeftDrag,
                MouseEventKind::Drag(MouseButton::Right) => KeyCode::MouseRightDrag,
                MouseEventKind::Drag(MouseButton::Middle) => KeyCode::MouseMiddleDrag,
                MouseEventKind::Moved => KeyCode::MouseMove,
                MouseEventKind::ScrollUp => KeyCode::MouseScrollUp,
                MouseEventKind::ScrollDown => KeyCode::MouseScrollDown,
                MouseEventKind::ScrollLeft => KeyCode::MouseScrollLeft,
                MouseEventKind::ScrollRight => KeyCode::MouseScrollRight,
            };
            Some(UiEvent::Mouse {
                key: KeyEvent::mouse(
                    code,
                    Modifiers {
                        control: mouse.modifiers.contains(KeyModifiers::CONTROL),
                        alt: mouse.modifiers.contains(KeyModifiers::ALT),
                        shift: mouse.modifiers.contains(KeyModifiers::SHIFT),
                    },
                ),
                column: mouse.column,
                row: mouse.row,
            })
        }
    }
}

#[derive(Debug, Error)]
pub enum TerminalError {
    #[error("terminal I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use crate::editor::input::{KeyCode as DustKeyCode, Modifiers};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

    use super::{
        ColorTier, KeyInputKind, UiEvent, configured_color_tier, normalize_event, queue_style,
    };

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foo"), "Zm9v");
        assert_eq!(super::base64(b"hello\n"), "aGVsbG8K");
    }

    #[test]
    fn frame_painting_hides_intermediate_cursor_moves_and_publishes_one_final_cursor() {
        use crate::ui::{CellGrid, Frame, Style};
        use crossterm::cursor::SetCursorStyle;

        let theme = crate::config::load_bundled_config().theme;
        let mut frame = Frame {
            grid: CellGrid::new(12, 4),
            cursor: Some((2, 1, SetCursorStyle::SteadyBar)),
        };
        frame.grid.put_text(0, 0, "top", Style::Default);
        frame.grid.put_text(0, 3, "status", Style::Status);
        let mut output = Vec::new();
        assert!(
            super::queue_frame(&mut output, &frame, None, &theme, ColorTier::TrueColor).unwrap()
        );
        let emitted = String::from_utf8(output).unwrap();
        assert!(emitted.starts_with("\x1b[?2026h\x1b[?25l\x1b[2J"));
        assert!(emitted.ends_with("\x1b[2;3H\x1b[6 q\x1b[?25h\x1b[?2026l"));
        assert_eq!(emitted.matches("\x1b[?25h").count(), 1);
        // A run of same-styled text is one move, one style and the text.
        assert!(emitted.contains("top") && emitted.contains("status"));

        // An unchanged frame must not move/show the cursor or restart its blink/trail.
        let mut output = Vec::new();
        assert!(
            !super::queue_frame(
                &mut output,
                &frame,
                Some(&frame),
                &theme,
                ColorTier::TrueColor
            )
            .unwrap()
        );
        assert!(output.is_empty());

        // A background/status-only update keeps the real cursor in place.
        let previous = frame.clone();
        frame.grid.put_text(0, 3, "update", Style::Default);
        super::queue_frame(
            &mut output,
            &frame,
            Some(&previous),
            &theme,
            ColorTier::TrueColor,
        )
        .unwrap();
        let emitted = String::from_utf8(output).unwrap();
        assert!(emitted.starts_with("\x1b[?2026h\x1b[?25l"));
        assert!(!emitted.contains("\x1b[2J"));
        assert!(emitted.ends_with("\x1b[2;3H\x1b[6 q\x1b[?25h\x1b[?2026l"));

        // Cursor-only moves are still delivered, and hidden-cursor surfaces stay hidden.
        let previous = frame.clone();
        frame.cursor = Some((3, 2, SetCursorStyle::SteadyBar));
        let mut output = Vec::new();
        super::queue_frame(
            &mut output,
            &frame,
            Some(&previous),
            &theme,
            ColorTier::TrueColor,
        )
        .unwrap();
        let emitted = String::from_utf8(output).unwrap();
        assert!(emitted.ends_with("\x1b[3;4H\x1b[6 q\x1b[?25h\x1b[?2026l"));
        assert!(!emitted.contains("\x1b[2J"));
        let previous = frame.clone();
        frame.cursor = None;
        let mut output = Vec::new();
        super::queue_frame(
            &mut output,
            &frame,
            Some(&previous),
            &theme,
            ColorTier::TrueColor,
        )
        .unwrap();
        let emitted = String::from_utf8(output).unwrap();
        assert!(emitted.starts_with("\x1b[?2026h\x1b[?25l"));
        assert!(emitted.ends_with("\x1b[?2026l"));
        assert!(!emitted.contains("\x1b[?25h"));

        // Invalidated/resize frames still clear and redraw under the same protection.
        frame.grid = CellGrid::new(8, 3);
        let mut output = Vec::new();
        super::queue_frame(
            &mut output,
            &frame,
            Some(&previous),
            &theme,
            ColorTier::TrueColor,
        )
        .unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .starts_with("\x1b[?2026h\x1b[?25l\x1b[2J")
        );
    }

    #[test]
    fn configured_theme_controls_emitted_terminal_style() {
        let mut theme = crate::config::load_bundled_config().theme;
        theme.styles.accent.foreground = crate::config::ThemeColor::Indexed(201);
        theme.styles.accent.background = crate::config::ThemeColor::Rgb(1, 2, 3);
        let mut output = Vec::new();
        queue_style(
            &mut output,
            crate::ui::Style::Accent,
            &theme,
            ColorTier::TrueColor,
        )
        .expect("queue configured style");
        let emitted = String::from_utf8(output).expect("ANSI is UTF-8");
        assert!(emitted.contains("38;5;201"));
        assert!(emitted.contains("48;2;1;2;3"));

        theme.styles.accent.foreground = crate::config::ThemeColor::Rgb(1, 2, 3);
        theme.styles.accent.foreground_256 = Some(crate::config::ThemeColor::Indexed(42));
        theme.styles.accent.foreground_16 = Some(crate::config::ThemeColor::Cyan);
        let mut fallback = Vec::new();
        queue_style(
            &mut fallback,
            crate::ui::Style::Accent,
            &theme,
            ColorTier::Ansi256,
        )
        .expect("queue fallback style");
        assert!(
            String::from_utf8(fallback)
                .expect("ANSI is UTF-8")
                .contains("38;5;42")
        );
        let mut ansi16 = Vec::new();
        queue_style(
            &mut ansi16,
            crate::ui::Style::Accent,
            &theme,
            ColorTier::Ansi16,
        )
        .expect("queue ANSI fallback");
        assert!(
            String::from_utf8(ansi16)
                .expect("ANSI is UTF-8")
                .contains("38;5;14")
        );
        let mut pty = Vec::new();
        queue_style(
            &mut pty,
            crate::ui::Style::Terminal(crate::ui::PtyStyle {
                foreground: Some(crate::ui::PtyColor::Indexed(1)),
                background: Some(crate::ui::PtyColor::Indexed(4)),
                bold: true,
                ..crate::ui::PtyStyle::default()
            }),
            &theme,
            ColorTier::Ansi256,
        )
        .expect("queue PTY-owned style");
        let pty = String::from_utf8(pty).expect("PTY ANSI is UTF-8");
        assert!(pty.contains("38;5;1"));
        assert!(pty.contains("48;5;4"));
        assert!(pty.contains("\u{1b}[1m"));
        assert_eq!(
            configured_color_tier(crate::config::ColorMode::Ansi256),
            (ColorTier::Ansi256, "configured")
        );
    }

    #[test]
    fn mouse_normalization_preserves_gesture_coordinates_and_modifiers() {
        let event = normalize_event(Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 12,
            row: 7,
            modifiers: KeyModifiers::SHIFT,
        }));
        assert_eq!(
            event,
            Some(UiEvent::Mouse {
                key: crate::editor::input::KeyEvent::mouse(
                    crate::editor::input::KeyCode::MouseScrollDown,
                    Modifiers {
                        shift: true,
                        ..Modifiers::default()
                    },
                ),
                column: 12,
                row: 7,
            })
        );
    }

    #[test]
    fn shifted_uppercase_character_uses_the_character_as_its_logical_identity() {
        let event = normalize_event(Event::Key(KeyEvent::new(
            KeyCode::Char('R'),
            KeyModifiers::SHIFT,
        )));
        assert_eq!(
            event,
            Some(UiEvent::Key {
                key: crate::editor::input::KeyEvent::character('R'),
                kind: KeyInputKind::Press,
            })
        );
    }

    #[test]
    fn legacy_terminal_control_bytes_normalize_to_slash_and_backslash() {
        let slash = normalize_event(Event::Key(KeyEvent::new(
            KeyCode::Char('7'),
            KeyModifiers::CONTROL,
        )));
        let backslash = normalize_event(Event::Key(KeyEvent::new(
            KeyCode::Char('4'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(
            slash,
            Some(UiEvent::Key {
                key: crate::editor::input::KeyEvent {
                    code: DustKeyCode::Char('/'),
                    modifiers: Modifiers {
                        control: true,
                        ..Modifiers::default()
                    },
                },
                kind: KeyInputKind::Press,
            })
        );
        assert_eq!(
            backslash,
            Some(UiEvent::Key {
                key: crate::editor::input::KeyEvent {
                    code: DustKeyCode::Char('\\'),
                    modifiers: Modifiers {
                        control: true,
                        ..Modifiers::default()
                    },
                },
                kind: KeyInputKind::Press,
            })
        );
    }

    #[test]
    fn non_character_shift_and_control_modifiers_remain_distinct() {
        let shifted_left = normalize_event(Event::Key(KeyEvent::new(
            KeyCode::Left,
            KeyModifiers::SHIFT,
        )));
        assert!(matches!(
            shifted_left,
            Some(UiEvent::Key {
                key: crate::editor::input::KeyEvent {
                    modifiers: Modifiers { shift: true, .. },
                    ..
                },
                ..
            })
        ));
        let control_upper = normalize_event(Event::Key(KeyEvent::new(
            KeyCode::Char('R'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        )));
        assert!(matches!(
            control_upper,
            Some(UiEvent::Key {
                key: crate::editor::input::KeyEvent {
                    code: DustKeyCode::Char('R'),
                    modifiers: Modifiers {
                        control: true,
                        shift: false,
                        ..
                    },
                },
                ..
            })
        ));
    }

    #[test]
    fn shifted_symbol_identity_does_not_gain_a_spurious_modifier() {
        let symbol = normalize_event(Event::Key(KeyEvent::new(
            KeyCode::Char('!'),
            KeyModifiers::SHIFT,
        )));
        assert!(matches!(
            symbol,
            Some(UiEvent::Key {
                key: crate::editor::input::KeyEvent {
                    code: DustKeyCode::Char('!'),
                    modifiers: Modifiers { shift: false, .. },
                },
                ..
            })
        ));
        let intentional_shift_digit = normalize_event(Event::Key(KeyEvent::new(
            KeyCode::Char('1'),
            KeyModifiers::SHIFT,
        )));
        assert!(matches!(
            intentional_shift_digit,
            Some(UiEvent::Key {
                key: crate::editor::input::KeyEvent {
                    code: DustKeyCode::Char('1'),
                    modifiers: Modifiers { shift: true, .. },
                },
                ..
            })
        ));
    }

    #[test]
    fn normalizes_crossterm_key_without_terminal_bytes() {
        let event = normalize_event(Event::Key(KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(
            event,
            Some(UiEvent::Key {
                key: crate::editor::input::KeyEvent {
                    code: DustKeyCode::Char('w'),
                    modifiers: Modifiers {
                        control: true,
                        alt: false,
                        shift: false,
                    },
                },
                kind: KeyInputKind::Press,
            })
        );
    }
}
