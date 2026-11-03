//! A terminal tab's screen. alacritty_terminal reads the program's output as a real terminal
//! would; this module hands its grid and scrollback to the panel in Dust's own cell types.
use std::sync::{Arc, Mutex};

use alacritty_terminal::{
    event::{Event, EventListener},
    grid::Dimensions,
    index::{Column, Line},
    term::{
        Config, Term, TermMode,
        cell::{Cell, Flags},
    },
    vte::ansi::{Color, NamedColor, Processor},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyColor {
    Indexed(u8),
    Rgb(u8, u8, u8),
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PtyStyle {
    pub foreground: Option<PtyColor>,
    pub background: Option<PtyColor>,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub strikethrough: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PtyCell {
    pub text: String,
    pub style: PtyStyle,
    pub continuation: bool,
}
impl Default for PtyCell {
    fn default() -> Self {
        Self {
            text: " ".into(),
            style: PtyStyle::default(),
            continuation: false,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PtyMouseMode {
    #[default]
    Off,
    Click,
    Drag,
    Any,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyInputKey {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyMouseEvent {
    Press(u8),
    Release,
    Move(u8),
}

/// What the terminal answers the program, such as its cursor position when asked.
#[derive(Clone, Default)]
struct Replies(Arc<Mutex<Vec<u8>>>);
impl EventListener for Replies {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event
            && let Ok(mut replies) = self.0.lock()
        {
            replies.extend_from_slice(text.as_bytes());
        }
    }
}

struct Size {
    columns: usize,
    rows: usize,
}
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

pub struct PtyScreen {
    term: Term<Replies>,
    parser: Processor,
    replies: Replies,
}
impl PtyScreen {
    pub fn new(columns: u16, rows: u16, maximum_scrollback_lines: usize) -> Self {
        let replies = Replies::default();
        let config = Config {
            scrolling_history: maximum_scrollback_lines.max(1),
            ..Config::default()
        };
        Self {
            term: Term::new(config, &size(columns, rows), replies.clone()),
            parser: Processor::new(),
            replies,
        }
    }
    pub fn feed_bytes(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
        // A program that began a synchronized update and never ended it still gets shown.
        if self
            .parser
            .sync_timeout()
            .sync_timeout()
            .is_some_and(|deadline| deadline <= std::time::Instant::now())
        {
            self.parser.stop_sync(&mut self.term);
        }
    }
    /// Bytes the terminal owes the program, such as answers to cursor-position queries.
    pub fn take_replies(&mut self) -> Vec<u8> {
        self.replies
            .0
            .lock()
            .map(|mut replies| std::mem::take(&mut *replies))
            .unwrap_or_default()
    }
    pub fn resize(&mut self, columns: u16, rows: u16) {
        self.term.resize(size(columns, rows));
    }
    #[must_use]
    pub fn dimensions(&self) -> (usize, usize) {
        (self.term.columns(), self.term.screen_lines())
    }
    /// The cursor as (column, row) on the screen.
    #[must_use]
    pub fn cursor(&self) -> (usize, usize) {
        let point = self.term.grid().cursor.point;
        (point.column.0, usize::try_from(point.line.0).unwrap_or(0))
    }
    #[must_use]
    pub fn scrollback_len(&self) -> usize {
        self.term.grid().history_size()
    }
    #[must_use]
    pub fn history_len(&self) -> usize {
        self.scrollback_len() + self.term.screen_lines()
    }
    /// One row of scrollback and screen together, oldest first.
    #[must_use]
    pub fn history_row(&self, row: usize) -> Option<Vec<PtyCell>> {
        if row >= self.history_len() {
            return None;
        }
        let line = Line(i32::try_from(row).ok()? - i32::try_from(self.scrollback_len()).ok()?);
        let grid = self.term.grid();
        Some(
            (0..grid.columns())
                .map(|column| pty_cell(&grid[line][Column(column)]))
                .collect(),
        )
    }
    #[must_use]
    pub fn history_window(&self, start: usize, maximum_rows: usize) -> Vec<Vec<PtyCell>> {
        (start..self.history_len().min(start.saturating_add(maximum_rows)))
            .filter_map(|row| self.history_row(row))
            .collect()
    }
    pub fn lines(&self) -> Vec<String> {
        (0..self.history_len())
            .filter_map(|row| self.history_row(row))
            .map(|row| render_row(&row))
            .collect()
    }
    #[must_use]
    pub fn alternate_active(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }
    #[must_use]
    pub fn cursor_visible(&self) -> bool {
        self.term.mode().contains(TermMode::SHOW_CURSOR)
    }
    #[must_use]
    pub fn application_cursor(&self) -> bool {
        self.term.mode().contains(TermMode::APP_CURSOR)
    }
    #[must_use]
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }
    #[must_use]
    pub fn focus_reporting(&self) -> bool {
        self.term.mode().contains(TermMode::FOCUS_IN_OUT)
    }
    #[must_use]
    pub fn mouse_mode(&self) -> PtyMouseMode {
        let mode = self.term.mode();
        if mode.contains(TermMode::MOUSE_MOTION) {
            PtyMouseMode::Any
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            PtyMouseMode::Drag
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            PtyMouseMode::Click
        } else {
            PtyMouseMode::Off
        }
    }
    #[must_use]
    pub fn sgr_mouse(&self) -> bool {
        self.term.mode().contains(TermMode::SGR_MOUSE)
    }
}

fn size(columns: u16, rows: u16) -> Size {
    Size {
        columns: usize::from(columns.max(2)),
        rows: usize::from(rows.max(1)),
    }
}

fn pty_cell(cell: &Cell) -> PtyCell {
    let flags = cell.flags;
    let style = PtyStyle {
        foreground: pty_color(cell.fg),
        background: pty_color(cell.bg),
        bold: flags.contains(Flags::BOLD),
        dim: flags.contains(Flags::DIM) || matches!(cell.fg, Color::Named(named) if is_dim(named)),
        italic: flags.contains(Flags::ITALIC),
        underline: flags.intersects(Flags::ALL_UNDERLINES),
        reverse: flags.contains(Flags::INVERSE),
        strikethrough: flags.contains(Flags::STRIKEOUT),
    };
    if flags.contains(Flags::WIDE_CHAR_SPACER) {
        return PtyCell {
            text: String::new(),
            style,
            continuation: true,
        };
    }
    let mut text = if flags.intersects(Flags::HIDDEN | Flags::LEADING_WIDE_CHAR_SPACER)
        || cell.c == '\0'
    {
        " ".to_owned()
    } else {
        cell.c.to_string()
    };
    text.extend(cell.zerowidth().into_iter().flatten());
    PtyCell {
        text,
        style,
        continuation: false,
    }
}

fn is_dim(color: NamedColor) -> bool {
    (NamedColor::DimBlack as usize..=NamedColor::DimForeground as usize).contains(&(color as usize))
        && color != NamedColor::BrightForeground
}

/// The program's colours in the panel's terms; the default foreground and background are left
/// to the theme.
fn pty_color(color: Color) -> Option<PtyColor> {
    match color {
        Color::Spec(rgb) => Some(PtyColor::Rgb(rgb.r, rgb.g, rgb.b)),
        Color::Indexed(index) => Some(PtyColor::Indexed(index)),
        Color::Named(named) => {
            let index = named as usize;
            if index < 16 {
                u8::try_from(index).ok().map(PtyColor::Indexed)
            } else if is_dim(named) && named != NamedColor::DimForeground {
                u8::try_from(index - NamedColor::DimBlack as usize)
                    .ok()
                    .map(PtyColor::Indexed)
            } else {
                None
            }
        }
    }
}
pub fn encode_key(key: PtyInputKey, application: bool) -> &'static [u8] {
    match (key, application) {
        (PtyInputKey::Up, true) => b"\x1bOA",
        (PtyInputKey::Down, true) => b"\x1bOB",
        (PtyInputKey::Right, true) => b"\x1bOC",
        (PtyInputKey::Left, true) => b"\x1bOD",
        (PtyInputKey::Home, true) => b"\x1bOH",
        (PtyInputKey::End, true) => b"\x1bOF",
        (PtyInputKey::Up, false) => b"\x1b[A",
        (PtyInputKey::Down, false) => b"\x1b[B",
        (PtyInputKey::Right, false) => b"\x1b[C",
        (PtyInputKey::Left, false) => b"\x1b[D",
        (PtyInputKey::Home, false) => b"\x1b[H",
        (PtyInputKey::End, false) => b"\x1b[F",
        (PtyInputKey::PageUp, _) => b"\x1b[5~",
        (PtyInputKey::PageDown, _) => b"\x1b[6~",
    }
}
pub fn encode_paste(text: &str, bracketed: bool, ceiling: usize) -> Option<Vec<u8>> {
    let sanitized = text.replace("\x1b[200~", "").replace("\x1b[201~", "");
    let needed = sanitized.len() + if bracketed { 12 } else { 0 };
    if needed > ceiling {
        return None;
    }
    let mut out = Vec::with_capacity(needed);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~")
    }
    out.extend_from_slice(sanitized.as_bytes());
    if bracketed {
        out.extend_from_slice(b"\x1b[201~")
    }
    Some(out)
}
pub fn encode_focus(focused: bool, enabled: bool) -> Option<&'static [u8]> {
    enabled.then_some(if focused {
        &b"\x1b[I"[..]
    } else {
        &b"\x1b[O"[..]
    })
}
pub struct TerminalMouseEncoding {
    pub event: crate::editor::input::TerminalMouseInput,
    pub column: u16,
    pub row: u16,
    pub left: u16,
    pub top: u16,
    pub width: u16,
    pub height: u16,
    pub mode: PtyMouseMode,
    pub sgr: bool,
    pub ceiling: usize,
}

pub fn encode_terminal_mouse_event(input: TerminalMouseEncoding) -> Option<Vec<u8>> {
    let TerminalMouseEncoding {
        event,
        column,
        row,
        left,
        top,
        width,
        height,
        mode,
        sgr,
        ceiling,
    } = input;
    if column < left
        || row < top
        || column >= left.saturating_add(width)
        || row >= top.saturating_add(height)
    {
        return None;
    }
    let event = match event {
        crate::editor::input::TerminalMouseInput::LeftDown => PtyMouseEvent::Press(0),
        crate::editor::input::TerminalMouseInput::MiddleDown => PtyMouseEvent::Press(1),
        crate::editor::input::TerminalMouseInput::RightDown => PtyMouseEvent::Press(2),
        crate::editor::input::TerminalMouseInput::LeftUp
        | crate::editor::input::TerminalMouseInput::MiddleUp
        | crate::editor::input::TerminalMouseInput::RightUp => PtyMouseEvent::Release,
        crate::editor::input::TerminalMouseInput::LeftDrag => PtyMouseEvent::Move(0),
        crate::editor::input::TerminalMouseInput::MiddleDrag => PtyMouseEvent::Move(1),
        crate::editor::input::TerminalMouseInput::RightDrag => PtyMouseEvent::Move(2),
        crate::editor::input::TerminalMouseInput::Move => PtyMouseEvent::Move(3),
        crate::editor::input::TerminalMouseInput::ScrollUp => PtyMouseEvent::Press(64),
        crate::editor::input::TerminalMouseInput::ScrollDown => PtyMouseEvent::Press(65),
        crate::editor::input::TerminalMouseInput::ScrollLeft => PtyMouseEvent::Press(66),
        crate::editor::input::TerminalMouseInput::ScrollRight => PtyMouseEvent::Press(67),
    };
    let bytes = encode_sgr_mouse(event, column - left, row - top, mode, sgr)?;
    (bytes.len() <= ceiling).then_some(bytes)
}

pub fn encode_sgr_mouse(
    event: PtyMouseEvent,
    column: u16,
    row: u16,
    mode: PtyMouseMode,
    sgr: bool,
) -> Option<Vec<u8>> {
    if mode == PtyMouseMode::Off || !sgr {
        return None;
    }
    let (button, final_byte) = match event {
        PtyMouseEvent::Press(b) => (b, 'M'),
        PtyMouseEvent::Release => (0, 'm'),
        PtyMouseEvent::Move(b) if matches!(mode, PtyMouseMode::Drag | PtyMouseMode::Any) => {
            (b.saturating_add(32), 'M')
        }
        PtyMouseEvent::Move(_) => return None,
    };
    Some(
        format!(
            "\x1b[<{button};{};{}{final_byte}",
            u32::from(column) + 1,
            u32::from(row) + 1
        )
        .into_bytes(),
    )
}
fn render_row(row: &[PtyCell]) -> String {
    let mut output = row
        .iter()
        .map(|cell| cell.text.as_str())
        .collect::<String>();
    while output.ends_with(' ') {
        output.pop();
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{
        PtyColor, PtyInputKey, PtyMouseEvent, PtyMouseMode, PtyScreen, PtyStyle, encode_focus,
        encode_key, encode_paste, encode_sgr_mouse,
    };

    fn row(screen: &PtyScreen, row: usize) -> Vec<super::PtyCell> {
        screen.history_row(screen.scrollback_len() + row).unwrap()
    }

    #[test]
    fn shrinking_keeps_blank_rows_out_of_history() {
        let mut screen = PtyScreen::new(20, 10, 64);
        screen.feed_bytes(b"prompt$ ");
        screen.resize(20, 4);
        assert_eq!(screen.scrollback_len(), 0, "nothing was on those rows");
        assert_eq!(screen.lines()[0], "prompt$");
        assert_eq!(screen.cursor().1, 0);

        screen.feed_bytes(b"\r\n1\r\n2\r\n3");
        screen.resize(20, 2);
        assert_eq!(screen.scrollback_len(), 2, "output above the cursor is kept");
        assert_eq!(screen.lines()[0], "prompt$");
        assert_eq!(screen.cursor().1, 1);
    }

    #[test]
    fn raw_pty_bytes_preserve_split_utf8_and_consume_shell_controls() {
        let mut screen = PtyScreen::new(40, 4, 16);
        screen.feed_bytes(b"\x1b[?2004h\x1b[1;32mgreen \xe7\x95");
        screen.feed_bytes(b"\x8c\x1b[0m\x1b]0;shell title\x07 ready");

        assert_eq!(screen.lines()[0], "green 界 ready");
        assert_eq!(
            row(&screen, 0)[0].style,
            PtyStyle {
                foreground: Some(PtyColor::Indexed(2)),
                bold: true,
                ..PtyStyle::default()
            }
        );
        assert!(row(&screen, 0)[7].continuation);
    }

    #[test]
    fn alternate_modes_and_extended_sgr_are_streaming_and_restorative() {
        let mut screen = PtyScreen::new(24, 3, 8);
        screen.feed_bytes(b"primary");
        let sequence =
            b"\x1b[?1049h\x1b[?1;25;1004;1002;1006;2004h\x1b[1;2;3;4;7;9;38;5;201;48;2;1;2;3malt";
        for byte in sequence {
            screen.feed_bytes(&[*byte]);
        }
        assert!(
            screen.alternate_active()
                && screen.application_cursor()
                && screen.cursor_visible()
                && screen.bracketed_paste()
                && screen.focus_reporting()
                && screen.sgr_mouse()
        );
        assert_eq!(screen.mouse_mode(), PtyMouseMode::Drag);
        // As in xterm, the alternate screen keeps the cursor where "primary" left it.
        assert_eq!(screen.lines()[0], "       alt");
        let style = row(&screen, 0)[7].style;
        assert_eq!(style.foreground, Some(PtyColor::Indexed(201)));
        assert_eq!(style.background, Some(PtyColor::Rgb(1, 2, 3)));
        assert!(
            style.bold
                && style.dim
                && style.italic
                && style.underline
                && style.reverse
                && style.strikethrough
        );
        screen.feed_bytes(b"\x1b[?1049l");
        assert!(!screen.alternate_active());
        assert_eq!(screen.lines()[0], "primary");
    }

    #[test]
    fn moving_the_cursor_after_a_full_width_line_cancels_its_wrap() {
        // pi and other differential renderers end a frame on a full-width border, then move up
        // and rewrite from there; the rewrite must land where they moved, not a row lower.
        let mut screen = PtyScreen::new(4, 3, 8);
        screen.feed_bytes("top\r\n────\x1b[1A\rX".as_bytes());
        assert_eq!(screen.lines()[0], "Xop");
        assert_eq!(screen.cursor(), (1, 0));
        let mut screen = PtyScreen::new(4, 2, 8);
        screen.feed_bytes(b"a\r\nb\r\nc\x1b[3J");
        assert_eq!(screen.scrollback_len(), 0);
    }

    #[test]
    fn the_terminal_answers_cursor_and_device_queries() {
        let mut screen = PtyScreen::new(10, 4, 8);
        screen.feed_bytes(b"ab\r\ncd\x1b[6n\x1b[c");
        let replies = String::from_utf8(screen.take_replies()).unwrap();
        assert!(replies.starts_with("\x1b[2;3R"), "{replies:?}");
        assert!(replies.contains("\x1b[?"), "device attributes: {replies:?}");
        assert!(screen.take_replies().is_empty());
    }

    #[test]
    fn narrowing_rewraps_long_lines_and_widening_joins_them() {
        let mut screen = PtyScreen::new(10, 4, 8);
        screen.feed_bytes(b"0123456789abc\r\n$ ");
        screen.resize(5, 6);
        assert_eq!(screen.lines()[..3], ["01234", "56789", "abc"]);
        screen.resize(20, 4);
        assert_eq!(screen.lines()[0], "0123456789abc");
    }

    #[test]
    fn pure_terminal_input_encoding_obeys_modes_and_bounds() {
        assert_eq!(encode_key(PtyInputKey::Up, false), b"\x1b[A");
        assert_eq!(encode_key(PtyInputKey::Up, true), b"\x1bOA");
        assert_eq!(
            encode_paste("a\x1b[200~b", true, 32).unwrap(),
            b"\x1b[200~ab\x1b[201~"
        );
        assert!(encode_paste("long", true, 4).is_none());
        assert_eq!(encode_focus(true, true), Some(&b"\x1b[I"[..]));
        assert!(encode_focus(false, false).is_none());
        assert_eq!(
            encode_sgr_mouse(PtyMouseEvent::Press(0), 2, 3, PtyMouseMode::Click, true).unwrap(),
            b"\x1b[<0;3;4M"
        );
        assert!(
            encode_sgr_mouse(PtyMouseEvent::Move(0), 1, 1, PtyMouseMode::Click, true).is_none()
        );
        assert_eq!(
            super::encode_terminal_mouse_event(super::TerminalMouseEncoding {
                event: crate::editor::input::TerminalMouseInput::LeftDown,
                column: 12,
                row: 23,
                left: 10,
                top: 20,
                width: 8,
                height: 6,
                mode: PtyMouseMode::Click,
                sgr: true,
                ceiling: 64,
            })
            .unwrap(),
            b"\x1b[<0;3;4M"
        );
        assert!(
            super::encode_terminal_mouse_event(super::TerminalMouseEncoding {
                event: crate::editor::input::TerminalMouseInput::Move,
                column: 9,
                row: 20,
                left: 10,
                top: 20,
                width: 8,
                height: 6,
                mode: PtyMouseMode::Any,
                sgr: true,
                ceiling: 64,
            })
            .is_none()
        );
    }

    #[test]
    fn raw_pty_bytes_replace_invalid_utf8_without_exposing_controls() {
        let mut screen = PtyScreen::new(20, 2, 4);
        screen.feed_bytes(b"ok\xff!\xe2\x82\x1b[m");
        assert_eq!(screen.lines()[0], "ok\u{fffd}!\u{fffd}");
        assert!(!screen.lines()[0].contains('\u{1b}'));
    }
}
