//! Terminal event decoding, rendering and restoration.

use std::cell::Cell;
use std::fs::File;
use std::io;
use std::io::IsTerminal;
use std::io::Stdout;
use std::io::Write;
use std::os::fd::AsFd;
use std::panic;
use std::sync::Once;
use std::time::Duration;

use crossterm::cursor::Hide;

use crossterm::cursor::MoveTo;

use crossterm::cursor::SetCursorStyle;

use crossterm::cursor::Show;

use crossterm::event;

use crossterm::event::DisableBracketedPaste;

use crossterm::event::DisableFocusChange;

use crossterm::event::DisableMouseCapture;

use crossterm::event::EnableBracketedPaste;

use crossterm::event::EnableFocusChange;

use crossterm::event::EnableMouseCapture;

use crossterm::event::Event;

use crossterm::event::KeyCode as CtKeyCode;

use crossterm::event::KeyEventKind;

use crossterm::event::KeyModifiers;

use crossterm::event::KeyboardEnhancementFlags;

use crossterm::event::MouseButton;

use crossterm::event::MouseEventKind;

use crossterm::event::PopKeyboardEnhancementFlags;

use crossterm::event::PushKeyboardEnhancementFlags;

use crossterm::execute;

use crossterm::queue;

use crossterm::style::Attribute;

use crossterm::style::Color;

use crossterm::style::Print;

use crossterm::style::ResetColor;

use crossterm::style::SetAttribute;

use crossterm::style::SetBackgroundColor;

use crossterm::style::SetForegroundColor;

use crossterm::terminal;

use crossterm::terminal::BeginSynchronizedUpdate;

use crossterm::terminal::Clear;

use crossterm::terminal::ClearType;

use crossterm::terminal::EndSynchronizedUpdate;

use crossterm::terminal::EnterAlternateScreen;

use crossterm::terminal::LeaveAlternateScreen;

use crossterm::terminal::disable_raw_mode;

use crossterm::terminal::enable_raw_mode;

use nix::errno::Errno;

use nix::poll::PollFd;

use nix::poll::PollFlags;

use nix::poll::PollTimeout;

use nix::poll::poll;

use crate::config::ColorMode;

use crate::config::ThemeAttribute;

use crate::config::ThemeColor;

use crate::config::ThemeConfig;

use crate::config::Highlight;
use crate::config::ThemeStyle;
use crate::editor::input::KeyCode;
use crate::editor::input::KeyEvent;
use crate::editor::input::Modifiers;
use thiserror::Error;

use crate::ui::Frame;

use crate::ui::Style;

static PANIC_HOOK: Once = Once::new();

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |value, (index, byte)| {
            value | u32::from(*byte) << (16 - 8 * index)
        });
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(
                    ALPHABET[(value >> (18 - 6 * index) & 63) as usize],
                ));
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
            let _ = restore_terminal(
                &mut io::stderr(),
                TerminalOptions {
                    color_mode: ColorMode::Auto,
                    alternate_screen: true,
                    bracketed_paste: true,
                    focus_events: true,
                    mouse: true,
                },
                true,
            );
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
    /// crossterm may still hold keys it parsed in its last read.
    buffered: Cell<bool>,
    enhanced_keyboard: bool,
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
        let mut keyboard_pushed = false;
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
            // Terminals with the kitty keyboard protocol then send keys like ctrl-, that have no
            // legacy code; the others ignore the request.
            execute!(
                stdout,
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )?;
            keyboard_pushed = true;
            execute!(stdout, Hide, Clear(ClearType::All))?;
            event::poll(Duration::ZERO)?;
            Ok(())
        })();
        if let Err(error) = enter_result {
            let _ = restore_terminal(&mut stdout, options, keyboard_pushed);
            return Err(TerminalError::Io(error));
        }
        let enhanced_keyboard = terminal::supports_keyboard_enhancement().unwrap_or(false);
        let (color_tier, color_source) = configured_color_tier(options.color_mode);
        Ok(Self {
            stdout,
            input: if io::stdin().is_terminal() {
                None
            } else {
                File::open("/dev/tty").ok()
            },
            buffered: Cell::new(false),
            enhanced_keyboard,
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
        // rest of the timeout, so the idle wait happens here on the terminal. crossterm also
        // hands out keys it already parsed only while its timeout has time left, so it gets a
        // full millisecond, and is asked again after every key: several keys can arrive in
        // one read, and the rest would otherwise wait for the next keypress.
        const READ: Duration = Duration::from_millis(1);
        if !self.buffered.get() {
            self.wait_for_input(timeout)?;
        }
        // SIGWINCH may be delivered to another thread, leaving crossterm's signal pipe
        // ready without interrupting the stdin wait. Check it even after a timeout.
        loop {
            if !event::poll(READ)? {
                self.buffered.set(false);
                return Ok(None);
            }
            self.buffered.set(true);
            if let Some(event) = normalize_keyboard_event(event::read()?, !self.enhanced_keyboard) {
                return Ok(Some(event));
            }
        }
    }

    /// Wait for terminal input; crossterm's pending signals are checked afterwards.
    fn wait_for_input(&self, timeout: Duration) -> io::Result<()> {
        let stdin = io::stdin();
        let terminal = match &self.input {
            Some(tty) => tty.as_fd(),
            None => stdin.as_fd(),
        };
        let timeout =
            PollTimeout::try_from(timeout.as_micros().div_ceil(1000)).unwrap_or(PollTimeout::MAX);
        match poll(&mut [PollFd::new(terminal, PollFlags::POLLIN)], timeout) {
            Ok(_) | Err(Errno::EINTR) => Ok(()),
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
        let _ = restore_terminal(&mut self.stdout, self.options, true);
    }
}

fn restore_terminal(
    writer: &mut impl Write,
    options: TerminalOptions,
    keyboard_pushed: bool,
) -> io::Result<()> {
    let _ = disable_raw_mode();
    queue_terminal_restore(writer, options, keyboard_pushed)?;
    writer.flush()
}

fn queue_terminal_restore(
    writer: &mut impl Write,
    options: TerminalOptions,
    keyboard_pushed: bool,
) -> io::Result<()> {
    queue!(
        writer,
        EndSynchronizedUpdate,
        ResetColor,
        SetAttribute(Attribute::Reset),
        SetCursorStyle::DefaultUserShape,
        Show
    )?;
    if keyboard_pushed {
        queue!(writer, PopKeyboardEnhancementFlags)?;
    }
    if options.mouse {
        queue!(writer, DisableMouseCapture)?;
    }
    if options.bracketed_paste {
        queue!(writer, DisableBracketedPaste)?;
    }
    if options.focus_events {
        queue!(writer, DisableFocusChange)?;
    }
    if options.alternate_screen {
        queue!(writer, LeaveAlternateScreen)?;
    }
    Ok(())
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
        let foreground = pty.foreground.map_or_else(
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
        let background = pty.background.map_or_else(
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
        queue!(writer, SetAttribute(Attribute::Reset))?;
        queue_colors(writer, foreground, background, tier)?;
        if pty.reverse {
            queue!(writer, SetAttribute(Attribute::Reverse))?;
        }
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
    let derived;
    let configured = match style {
        Style::Default => &theme.styles.default,
        Style::Muted => &theme.styles.muted,
        Style::Accent => &theme.styles.accent,
        Style::Status => &theme.styles.status,
        Style::StatusAccent => &theme.styles.status_accent,
        Style::StatusInsert
        | Style::StatusVisual
        | Style::StatusReplace
        | Style::StatusCommand
        | Style::StatusSection
        | Style::StatusEdgeNormal
        | Style::StatusEdgeInsert
        | Style::StatusEdgeVisual
        | Style::StatusEdgeReplace
        | Style::StatusEdgeCommand
        | Style::StatusEdgeSection
        | Style::StatusError
        | Style::StatusWarning => {
            derived = status_bar_style(style, theme);
            &derived
        }
        Style::Selection => &theme.styles.selection,
        Style::Search => &theme.styles.search,
        Style::Error => &theme.styles.error,
        Style::Warning => &theme.styles.warning,
        Style::DiagnosticError | Style::DiagnosticWarning | Style::DiagnosticHint => {
            derived = diagnostic_text_style(style, theme);
            &derived
        }
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
        Style::Syntax(group) => theme.highlight(group).unwrap_or(&theme.styles.default),
        Style::MarkdownHeading(group) => {
            derived = theme.markdown_heading(group);
            &derived
        }
        Style::MarkdownCode(group) => {
            derived = theme.markdown_code(group);
            &derived
        }
        Style::DocumentationSyntax(group) => {
            let popup = &theme.styles.documentation_text;
            derived = theme.highlight(group).map_or_else(
                || popup.clone(),
                |style| crate::config::ThemeStyle {
                    background: popup.background,
                    background_256: popup.background_256,
                    background_16: popup.background_16,
                    ..style.clone()
                },
            );
            &derived
        }
    };
    queue!(writer, SetAttribute(Attribute::Reset))?;
    queue_colors(
        writer,
        terminal_color(tier_color(
            configured.foreground,
            configured.foreground_256,
            configured.foreground_16,
            tier,
        )),
        terminal_color(tier_color(
            configured.background,
            configured.background_256,
            configured.background_16,
            tier,
        )),
        tier,
    )?;
    for attribute in &configured.attributes {
        queue!(writer, SetAttribute(terminal_attribute(*attribute)))?;
    }
    Ok(())
}

fn queue_colors(
    writer: &mut impl Write,
    foreground: Color,
    background: Color,
    tier: ColorTier,
) -> io::Result<()> {
    if tier != ColorTier::Ansi16 {
        return queue!(
            writer,
            SetForegroundColor(foreground),
            SetBackgroundColor(background)
        );
    }
    // Crossterm emits extended indexed SGR even for named colours.
    for (color, base) in [(foreground, 30), (background, 40)] {
        let code = if color == Color::Reset {
            base + 9
        } else {
            let index = (0..16)
                .find(|index| terminal_ansi_color(*index) == color)
                .expect("ANSI16 colours are reduced to named colours");
            base + index % 8 + if index >= 8 { 60 } else { 0 }
        };
        write!(writer, "\x1b[{code}m")?;
    }
    Ok(())
}

/// The status bar's blocks are mixed from colours every theme already has, so a mode shows in
/// its own colour without a theme naming each block.
fn diagnostic_text_style(style: Style, theme: &ThemeConfig) -> ThemeStyle {
    let mut derived = match style {
        Style::DiagnosticError => theme.styles.error.clone(),
        Style::DiagnosticWarning => theme.styles.warning.clone(),
        _ => theme.styles.muted.clone(),
    };
    if !derived.attributes.contains(&ThemeAttribute::Underlined) {
        derived.attributes.push(ThemeAttribute::Underlined);
    }
    derived
}

fn status_bar_style(style: Style, theme: &ThemeConfig) -> ThemeStyle {
    type Colour = (ThemeColor, Option<ThemeColor>, Option<ThemeColor>);
    let text = |style: &ThemeStyle| (style.foreground, style.foreground_256, style.foreground_16);
    let fill = |style: &ThemeStyle| (style.background, style.background_256, style.background_16);
    let styles = &theme.styles;
    let accent = &styles.status_accent;
    let group = |group| theme.highlight(group).unwrap_or(&styles.accent);
    let paint = |foreground: Colour, background: Colour, attributes: Vec<_>| ThemeStyle {
        foreground: foreground.0,
        foreground_256: foreground.1,
        foreground_16: foreground.2,
        background: background.0,
        background_256: background.1,
        background_16: background.2,
        attributes,
    };
    let block = |colour: &ThemeStyle| paint(text(accent), text(colour), accent.attributes.clone());
    let edge = |colour: Colour| paint(colour, fill(&styles.selection), Vec::new());
    match style {
        Style::StatusInsert => block(group(Highlight::STRING)),
        Style::StatusVisual => block(group(Highlight::KEYWORD)),
        Style::StatusReplace => block(&styles.error),
        Style::StatusCommand => block(&styles.warning),
        Style::StatusEdgeInsert => edge(text(group(Highlight::STRING))),
        Style::StatusEdgeVisual => edge(text(group(Highlight::KEYWORD))),
        Style::StatusEdgeReplace => edge(text(&styles.error)),
        Style::StatusEdgeCommand => edge(text(&styles.warning)),
        Style::StatusSection => paint(text(&styles.status), fill(&styles.selection), Vec::new()),
        Style::StatusEdgeSection => {
            paint(fill(&styles.selection), fill(&styles.status), Vec::new())
        }
        Style::StatusError => paint(text(&styles.error), fill(&styles.status), Vec::new()),
        Style::StatusWarning => paint(text(&styles.warning), fill(&styles.status), Vec::new()),
        _ => edge(fill(accent)),
    }
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
        ColorTier::Ansi16 => ansi_16.unwrap_or(primary).ansi16(),
    }
}

fn terminal_pty_color(color: crate::ui::PtyColor, tier: ColorTier) -> Color {
    match (color, tier) {
        (crate::ui::PtyColor::Indexed(index), ColorTier::Ansi16) => {
            terminal_color(ThemeColor::Indexed(index).ansi16())
        }
        (crate::ui::PtyColor::Indexed(index), _) if index > 15 => Color::AnsiValue(index),
        (crate::ui::PtyColor::Indexed(index), _) => terminal_ansi_color(index),
        (crate::ui::PtyColor::Rgb(r, g, b), ColorTier::TrueColor) => Color::Rgb { r, g, b },
        (crate::ui::PtyColor::Rgb(r, g, b), ColorTier::Ansi256) => {
            Color::AnsiValue(16 + 36 * (r / 51) + 6 * (g / 51) + (b / 51))
        }
        (crate::ui::PtyColor::Rgb(r, g, b), ColorTier::Ansi16) => {
            terminal_color(ThemeColor::Rgb(r, g, b).ansi16())
        }
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
        ThemeAttribute::Strikethrough => Attribute::CrossedOut,
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
    normalize_keyboard_event(event, false)
}

fn normalize_keyboard_event(event: Event, legacy: bool) -> Option<UiEvent> {
    match event {
        Event::Key(key) => {
            let code = match key.code {
                // Legacy terminal protocols encode Ctrl+\\ as 0x1c and
                // Ctrl+/ (Ctrl+_) as 0x1f. Crossterm exposes those bytes as
                // Ctrl+4 and Ctrl+7; restore the conventional logical keys so
                // configurable bindings behave the same with and without an
                // enhanced keyboard protocol.
                CtKeyCode::Char('4') if legacy && key.modifiers.contains(KeyModifiers::CONTROL) => {
                    KeyCode::Char('\\')
                }
                CtKeyCode::Char('7') if legacy && key.modifiers.contains(KeyModifiers::CONTROL) => {
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
                CtKeyCode::PageUp => KeyCode::PageUp,
                CtKeyCode::PageDown => KeyCode::PageDown,
                CtKeyCode::Delete => KeyCode::Delete,
                CtKeyCode::F(number) => KeyCode::F(number),
                _ => return None,
            };
            Some(UiEvent::Key {
                key: KeyEvent {
                    code,
                    modifiers: Modifiers {
                        control: key.modifiers.contains(KeyModifiers::CONTROL),
                        alt: key.modifiers.contains(KeyModifiers::ALT),
                        shift: key.modifiers.contains(KeyModifiers::SHIFT),
                    },
                }
                .normalized_shift(),
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
    use crate::editor::input::KeyCode as DustKeyCode;
    use crate::editor::input::Modifiers;
    use crossterm::event::Event;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;
    use crossterm::event::MouseEvent;
    use crossterm::event::MouseEventKind;

    use crate::ui::terminal::ColorTier;

    use crate::ui::terminal::KeyInputKind;

    use crate::ui::terminal::UiEvent;

    use crate::ui::terminal::configured_color_tier;

    use crate::ui::terminal::normalize_event;

    use crate::ui::terminal::queue_style;

    use crate::ui::terminal::terminal_color;

    use crate::ui::terminal::tier_color;

    #[test]
    fn resize_events_are_serviced_without_tty_input() {
        use portable_pty::CommandBuilder;
        use portable_pty::PtySize;
        use portable_pty::native_pty_system;
        use std::io::Read;
        use std::io::Write;
        use std::time::Duration;
        use std::time::Instant;

        if std::env::var_os("DUST_TEST_RESIZE_CHILD").is_some() {
            let terminal = crate::ui::terminal::TerminalSession::enter(
                crate::ui::terminal::TerminalOptions {
                    color_mode: crate::config::ColorMode::Ansi16,
                    alternate_screen: false,
                    bracketed_paste: false,
                    focus_events: false,
                    mouse: false,
                },
                crate::config::load_bundled_config().theme,
            )
            .unwrap();
            print!("resize-ready");
            std::io::stdout().flush().unwrap();
            // Let SIGWINCH queue the event before the tty readiness wait starts.
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(
                terminal.poll_event(Duration::from_millis(50)).unwrap(),
                Some(UiEvent::Resize(120, 40))
            );
            return;
        }

        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "ui::terminal::tests::resize_events_are_serviced_without_tty_input",
            "--nocapture",
        ]);
        command.env("DUST_TEST_RESIZE_CHILD", "1");
        command.env("TERM", "xterm-256color");
        let mut child = pair.slave.spawn_command(command).unwrap();
        drop(pair.slave);
        let mut output = pair.master.try_clone_reader().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            let mut byte = [0];
            while output.read(&mut byte).is_ok_and(|count| count != 0) {
                text.push(char::from(byte[0]));
                if text.ends_with("resize-ready") {
                    let _ = sender.send(());
                    break;
                }
            }
        });
        receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        pair.master
            .resize(PtySize {
                rows: 40,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "{status:?}");
                break;
            }
            assert!(Instant::now() < deadline, "resize child timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        assert_eq!(crate::ui::terminal::base64(b""), "");
        assert_eq!(crate::ui::terminal::base64(b"f"), "Zg==");
        assert_eq!(crate::ui::terminal::base64(b"fo"), "Zm8=");
        assert_eq!(crate::ui::terminal::base64(b"foo"), "Zm9v");
        assert_eq!(crate::ui::terminal::base64(b"hello\n"), "aGVsbG8K");
    }

    #[test]
    fn frame_painting_hides_intermediate_cursor_moves_and_publishes_one_final_cursor() {
        use crate::ui::CellGrid;
        use crate::ui::Frame;
        use crate::ui::Style;
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
            crate::ui::terminal::queue_frame(
                &mut output,
                &frame,
                None,
                &theme,
                ColorTier::TrueColor
            )
            .unwrap()
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
            !crate::ui::terminal::queue_frame(
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
        crate::ui::terminal::queue_frame(
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
        crate::ui::terminal::queue_frame(
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
        crate::ui::terminal::queue_frame(
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
        crate::ui::terminal::queue_frame(
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
        let ansi16 = String::from_utf8(ansi16).expect("ANSI is UTF-8");
        assert!(ansi16.contains("\x1b[96m"));
        assert!(!ansi16.contains("38;") && !ansi16.contains("48;"));
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
        // Reverse is an attribute, including when both theme colours are terminal defaults.
        let mut reversed = Vec::new();
        queue_style(
            &mut reversed,
            crate::ui::Style::Terminal(crate::ui::PtyStyle {
                reverse: true,
                ..crate::ui::PtyStyle::default()
            }),
            &theme,
            ColorTier::TrueColor,
        )
        .expect("queue reversed style");
        let (text, background) = (
            &theme.styles.terminal_text,
            &theme.styles.terminal_background,
        );
        let mut expected = Vec::new();
        crossterm::queue!(
            expected,
            crossterm::style::SetForegroundColor(terminal_color(tier_color(
                text.foreground,
                text.foreground_256,
                text.foreground_16,
                ColorTier::TrueColor,
            ))),
            crossterm::style::SetBackgroundColor(terminal_color(tier_color(
                background.background,
                background.background_256,
                background.background_16,
                ColorTier::TrueColor,
            ))),
            crossterm::style::SetAttribute(crossterm::style::Attribute::Reverse)
        )
        .expect("expected colours and reverse attribute");
        assert!(
            reversed
                .windows(expected.len())
                .any(|window| window == expected)
        );
        assert_eq!(
            configured_color_tier(crate::config::ColorMode::Ansi256),
            (ColorTier::Ansi256, "configured")
        );
    }

    #[test]
    fn reversed_default_terminal_colors_retain_the_reverse_attribute() {
        let mut theme = crate::config::load_bundled_config().theme;
        theme.styles.terminal_text.foreground = crate::config::ThemeColor::Reset;
        theme.styles.terminal_background.background = crate::config::ThemeColor::Reset;
        let mut output = Vec::new();
        queue_style(
            &mut output,
            crate::ui::Style::Terminal(crate::ui::PtyStyle {
                reverse: true,
                ..crate::ui::PtyStyle::default()
            }),
            &theme,
            ColorTier::TrueColor,
        )
        .unwrap();
        let emitted = String::from_utf8(output).unwrap();
        assert!(emitted.contains("\x1b[39m\x1b[49m\x1b[7m"));
    }

    #[test]
    fn ansi16_uses_basic_sgr_for_every_palette_color_and_quantizes_extended_colors() {
        use crate::ui::PtyColor;
        let mut theme = crate::config::load_bundled_config().theme;
        for index in 0..16 {
            let mut output = Vec::new();
            queue_style(
                &mut output,
                crate::ui::Style::Terminal(crate::ui::PtyStyle {
                    foreground: Some(PtyColor::Indexed(index)),
                    background: Some(PtyColor::Indexed(index)),
                    ..crate::ui::PtyStyle::default()
                }),
                &theme,
                ColorTier::Ansi16,
            )
            .unwrap();
            let emitted = String::from_utf8(output).unwrap();
            let foreground = 30 + index % 8 + if index >= 8 { 60 } else { 0 };
            let background = foreground + 10;
            assert!(emitted.contains(&format!("\x1b[{foreground}m\x1b[{background}m")));
            assert!(!emitted.contains("38;") && !emitted.contains("48;"));
        }
        for (input, expected) in [
            (PtyColor::Indexed(196), crossterm::style::Color::Red),
            (PtyColor::Indexed(46), crossterm::style::Color::Green),
            (PtyColor::Indexed(21), crossterm::style::Color::DarkBlue),
            (PtyColor::Indexed(244), crossterm::style::Color::DarkGrey),
            (PtyColor::Rgb(255, 0, 0), crossterm::style::Color::Red),
        ] {
            assert_eq!(
                crate::ui::terminal::terminal_pty_color(input, ColorTier::Ansi16),
                expected
            );
        }
        // Missing or extended theme fallbacks must not leak extended SGR either.
        theme.styles.accent.foreground = crate::config::ThemeColor::Indexed(196);
        theme.styles.accent.foreground_16 = None;
        theme.styles.accent.background_16 = Some(crate::config::ThemeColor::Rgb(0, 255, 0));
        let mut output = Vec::new();
        queue_style(
            &mut output,
            crate::ui::Style::Accent,
            &theme,
            ColorTier::Ansi16,
        )
        .unwrap();
        let emitted = String::from_utf8(output).unwrap();
        assert!(emitted.contains("\x1b[91m\x1b[102m"));
        assert!(!emitted.contains("38;") && !emitted.contains("48;"));
    }

    #[test]
    fn status_sections_use_text_on_selection_fill_in_every_color_tier() {
        use crate::config::ThemeColor;
        let mut theme = crate::config::load_bundled_config().theme;
        theme.styles.status_accent.background = theme.styles.selection.background;
        theme.styles.status.foreground = ThemeColor::White;
        theme.styles.status.foreground_256 = Some(ThemeColor::Indexed(255));
        theme.styles.status.foreground_16 = Some(ThemeColor::White);
        theme.styles.selection.background_256 = Some(ThemeColor::Indexed(236));
        theme.styles.selection.background_16 = Some(ThemeColor::DarkGrey);
        let section =
            crate::ui::terminal::status_bar_style(crate::ui::Style::StatusSection, &theme);
        assert_eq!(section.foreground, theme.styles.status.foreground);
        assert_eq!(section.foreground_256, theme.styles.status.foreground_256);
        assert_eq!(section.foreground_16, theme.styles.status.foreground_16);
        assert_eq!(section.background, theme.styles.selection.background);
        assert_eq!(
            section.background_256,
            theme.styles.selection.background_256
        );
        assert_eq!(section.background_16, theme.styles.selection.background_16);
        for tier in [ColorTier::Ansi16, ColorTier::Ansi256, ColorTier::TrueColor] {
            let mut output = Vec::new();
            queue_style(&mut output, crate::ui::Style::StatusSection, &theme, tier).unwrap();
            assert!(!output.is_empty());
        }
    }

    #[test]
    fn terminal_restoration_balances_enabled_modes_once() {
        for enabled in [false, true] {
            let options = crate::ui::terminal::TerminalOptions {
                color_mode: crate::config::ColorMode::Ansi16,
                alternate_screen: enabled,
                bracketed_paste: enabled,
                focus_events: enabled,
                mouse: enabled,
            };
            let mut output = Vec::new();
            crate::ui::terminal::queue_terminal_restore(&mut output, options, true).unwrap();
            let emitted = String::from_utf8(output).unwrap();
            assert_eq!(emitted.matches("\x1b[<1u").count(), 1);
            for sequence in ["\x1b[?1049l", "\x1b[?2004l", "\x1b[?1004l", "\x1b[?1000l"] {
                assert_eq!(emitted.contains(sequence), enabled, "{emitted:?}");
            }
            assert!(emitted.contains("\x1b[?25h"));
            let mut before_keyboard_push = Vec::new();
            crate::ui::terminal::queue_terminal_restore(&mut before_keyboard_push, options, false)
                .unwrap();
            assert!(
                !String::from_utf8(before_keyboard_push)
                    .unwrap()
                    .contains("\x1b[<1u")
            );
        }
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
    fn enhanced_control_digits_keep_their_identity() {
        for digit in ['4', '7'] {
            let event = normalize_event(Event::Key(KeyEvent::new(
                KeyCode::Char(digit),
                KeyModifiers::CONTROL,
            )));
            assert!(
                matches!(event, Some(UiEvent::Key { key: crate::editor::input::KeyEvent { code: DustKeyCode::Char(c), .. }, .. }) if c == digit)
            );
        }
    }

    #[test]
    fn legacy_terminal_control_bytes_normalize_to_slash_and_backslash() {
        use crate::ui::terminal::normalize_keyboard_event;
        let slash = normalize_keyboard_event(
            Event::Key(KeyEvent::new(KeyCode::Char('7'), KeyModifiers::CONTROL)),
            true,
        );
        let backslash = normalize_keyboard_event(
            Event::Key(KeyEvent::new(KeyCode::Char('4'), KeyModifiers::CONTROL)),
            true,
        );
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
    fn shifted_control_letters_match_configured_bindings() {
        for character in ['h', 'H'] {
            let event = normalize_event(Event::Key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )));
            assert_eq!(
                event,
                Some(UiEvent::Key {
                    key: "ctrl-shift-h".parse().unwrap(),
                    kind: KeyInputKind::Press,
                })
            );
        }
    }

    #[test]
    fn shifted_lowercase_text_keeps_the_terminals_translated_character() {
        // Shift with Caps Lock can produce lowercase text, not an uppercase shortcut.
        let event = normalize_event(Event::Key(KeyEvent::new(
            KeyCode::Char('h'),
            KeyModifiers::SHIFT,
        )));
        let Some(UiEvent::Key { key, .. }) = event else {
            panic!("expected key event");
        };
        assert_eq!(key.code, DustKeyCode::Char('h'));
        assert_eq!(key.text_character(), Some('h'));
        assert!(key.modifiers.shift);
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
