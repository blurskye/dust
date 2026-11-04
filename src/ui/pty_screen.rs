//! A terminal tab's screen. alacritty_terminal reads the program's output as a real terminal
//! would; this module hands its grid and scrollback to the panel in Dust's own cell types.
use std::sync::{Arc, Mutex};

use alacritty_terminal::{
    event::{Event, EventListener},
    grid::Dimensions,
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
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
    Release(u8),
    Move(u8),
}

/// What the terminal answers the program, such as its cursor position when asked.
#[derive(Clone, Default)]
struct Replies(Arc<Mutex<Vec<Event>>>);
impl EventListener for Replies {
    fn send_event(&self, event: Event) {
        if matches!(event, Event::PtyWrite(_) | Event::ColorRequest(..))
            && let Ok(mut replies) = self.0.lock()
        {
            replies.push(event);
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
    scrollback_bytes: Option<usize>,
    default_colors: (crate::config::ThemeColor, crate::config::ThemeColor),
}
impl PtyScreen {
    pub fn new(columns: u16, rows: u16, maximum_scrollback_lines: usize) -> Self {
        let replies = Replies::default();
        let theme = crate::config::load_bundled_config().theme;
        let config = Config {
            scrolling_history: maximum_scrollback_lines.max(1),
            ..Config::default()
        };
        Self {
            term: Term::new(config, &size(columns, rows), replies.clone()),
            parser: Processor::new(),
            replies,
            scrollback_bytes: None,
            default_colors: (
                theme.styles.terminal_background.foreground,
                theme.styles.terminal_background.background,
            ),
        }
    }
    pub fn with_scrollback_bytes(columns: u16, rows: u16, bytes: usize) -> Self {
        let mut screen = Self::new(columns, rows, Self::scrollback_lines(columns, bytes));
        screen.scrollback_bytes = Some(bytes);
        screen
    }
    fn scrollback_lines(columns: u16, bytes: usize) -> usize {
        bytes
            / (usize::from(columns.max(2)) * std::mem::size_of::<Cell>()
                + std::mem::size_of::<alacritty_terminal::grid::Row<Cell>>())
    }
    pub fn feed_bytes(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
        self.expire_sync(std::time::Instant::now());
    }
    pub fn feed_bytes_tracked(
        &mut self,
        bytes: &[u8],
        cursor: &mut Option<(usize, usize)>,
        anchor: &mut Option<(usize, usize)>,
        offset: &mut usize,
    ) {
        let Some(old_cursor) = *cursor else {
            self.feed_bytes(bytes);
            return;
        };
        let old_anchor = anchor.unwrap_or(old_cursor);
        let point = |(row, column): (usize, usize)| {
            Point::new(
                Line(row as i32 - self.scrollback_len() as i32),
                Column(column),
            )
        };
        let mut selection = Selection::new(SelectionType::Simple, point(old_anchor), Side::Left);
        selection.update(point(old_cursor), Side::Right);
        selection.include_all();
        self.term.selection = Some(selection);
        self.feed_bytes(bytes);
        let selection = self
            .term
            .selection
            .take()
            .and_then(|selection| selection.to_range(&self.term));
        if let Some(selection) = selection {
            let position = |point: Point| {
                (
                    (point.line.0 + self.scrollback_len() as i32).max(0) as usize,
                    point.column.0,
                )
            };
            let (new_anchor, new_cursor) = if old_anchor <= old_cursor {
                (position(selection.start), position(selection.end))
            } else {
                (position(selection.end), position(selection.start))
            };
            *cursor = Some(new_cursor);
            if anchor.is_some() {
                *anchor = Some(new_anchor);
            }
            *offset = offset.saturating_sub(old_cursor.0.saturating_sub(new_cursor.0));
        } else {
            *cursor = Some((0, 0));
            *anchor = None;
            *offset = 0;
        }
    }
    pub fn sync_deadline(&self) -> Option<std::time::Instant> {
        self.parser.sync_timeout().sync_timeout()
    }
    pub fn expire_sync(&mut self, now: std::time::Instant) -> bool {
        if self.sync_deadline().is_some_and(|deadline| deadline <= now) {
            self.parser.stop_sync(&mut self.term);
            true
        } else {
            false
        }
    }
    /// Bytes the terminal owes the program, such as answers to cursor-position queries.
    pub fn take_replies(&mut self) -> Vec<u8> {
        let events = self
            .replies
            .0
            .lock()
            .map(|mut events| std::mem::take(&mut *events))
            .unwrap_or_default();
        let mut replies = Vec::new();
        for event in events {
            let text = match event {
                Event::PtyWrite(text) => text,
                Event::ColorRequest(index, format) => {
                    let default = if index == NamedColor::Background as usize {
                        self.default_colors.1
                    } else {
                        self.default_colors.0
                    };
                    let rgb = self.term.colors()[index].or_else(|| {
                        let color = if index < 256 {
                            crate::config::ThemeColor::Indexed(index as u8)
                        } else {
                            default
                        };
                        color
                            .rgb()
                            .map(|(r, g, b)| alacritty_terminal::vte::ansi::Rgb { r, g, b })
                    });
                    let Some(rgb) = rgb else { continue };
                    format(rgb)
                }
                _ => continue,
            };
            replies.extend_from_slice(text.as_bytes());
        }
        replies
    }
    pub fn set_theme(&mut self, theme: &crate::config::ThemeConfig) {
        self.default_colors = (
            theme.styles.terminal_background.foreground,
            theme.styles.terminal_background.background,
        );
    }

    pub fn resize(&mut self, columns: u16, rows: u16) {
        if let Some(bytes) = self.scrollback_bytes {
            self.term.set_options(Config {
                scrolling_history: Self::scrollback_lines(columns, bytes),
                ..Config::default()
            });
        }
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
                .map(|column| pty_cell(&grid[line][Column(column)], self.term.colors()))
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

fn pty_cell(cell: &Cell, colors: &alacritty_terminal::term::color::Colors) -> PtyCell {
    let flags = cell.flags;
    let style = PtyStyle {
        foreground: pty_color(cell.fg, colors),
        background: pty_color(cell.bg, colors),
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
    let mut text =
        if flags.intersects(Flags::HIDDEN | Flags::LEADING_WIDE_CHAR_SPACER) || cell.c == '\0' {
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
fn pty_color(color: Color, colors: &alacritty_terminal::term::color::Colors) -> Option<PtyColor> {
    let index = match color {
        Color::Indexed(index) => Some(usize::from(index)),
        Color::Named(named) => Some(named as usize),
        Color::Spec(_) => None,
    };
    if let Some(rgb) = index.and_then(|index| colors[index]) {
        return Some(PtyColor::Rgb(rgb.r, rgb.g, rgb.b));
    }
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
    let sanitized = text
        .replace("\x1b[200~", "")
        .replace("\x1b[201~", "")
        .replace('\x1b', "");
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
        crate::editor::input::TerminalMouseInput::LeftUp => PtyMouseEvent::Release(0),
        crate::editor::input::TerminalMouseInput::MiddleUp => PtyMouseEvent::Release(1),
        crate::editor::input::TerminalMouseInput::RightUp => PtyMouseEvent::Release(2),
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
    if mode == PtyMouseMode::Off {
        return None;
    }
    let (button, final_byte) = match event {
        PtyMouseEvent::Press(b) => (b, 'M'),
        PtyMouseEvent::Release(button) => (button, 'm'),
        PtyMouseEvent::Move(b) if matches!(mode, PtyMouseMode::Drag | PtyMouseMode::Any) => {
            (b.saturating_add(32), 'M')
        }
        PtyMouseEvent::Move(_) => return None,
    };
    if !sgr {
        if column > 222 || row > 222 {
            return None;
        }
        let button = if matches!(event, PtyMouseEvent::Release(_)) {
            3
        } else {
            button
        };
        return Some(vec![
            0x1b,
            b'[',
            b'M',
            button + 32,
            column as u8 + 33,
            row as u8 + 33,
        ]);
    }
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
    fn history_byte_budget_is_recomputed_after_resize() {
        let mut screen = PtyScreen::with_scrollback_bytes(10, 2, 4096);
        for _ in 0..100 {
            screen.feed_bytes(b"x\r\n");
        }
        assert!(screen.scrollback_len() <= PtyScreen::scrollback_lines(10, 4096));
        screen.resize(100, 2);
        for _ in 0..100 {
            screen.feed_bytes(b"x\r\n");
        }
        assert!(screen.scrollback_len() <= PtyScreen::scrollback_lines(100, 4096));
        assert!(PtyScreen::scrollback_lines(100, 4096) < PtyScreen::scrollback_lines(10, 4096));
    }

    #[test]
    fn quiet_synchronized_output_expires_without_more_bytes() {
        let mut screen = PtyScreen::new(10, 2, 4);
        screen.feed_bytes(b"\x1b[?2026hframe");
        let deadline = screen
            .sync_deadline()
            .expect("synchronized update deadline");
        assert!(!screen.lines().iter().any(|line| line.contains("frame")));
        assert!(screen.expire_sync(deadline));
        assert!(screen.lines().iter().any(|line| line.contains("frame")));
        assert!(screen.sync_deadline().is_none());
    }

    #[test]
    fn terminal_palette_overrides_and_color_queries_use_program_colors() {
        let mut screen = PtyScreen::new(10, 2, 4);
        screen
            .feed_bytes(b"\x1b]4;1;rgb:00/ff/00\x07\x1b[31mX\x1b]11;rgb:12/34/56\x07\x1b]11;?\x07");
        assert_eq!(
            row(&screen, 0)[0].style.foreground,
            Some(PtyColor::Rgb(0, 255, 0))
        );
        assert_eq!(
            String::from_utf8(screen.take_replies()).unwrap(),
            "\x1b]11;rgb:1212/3434/5656\x07"
        );
        screen.feed_bytes(b"\x1b]11\x07\x1b]11;?\x07");
        assert!(
            !screen.take_replies().is_empty(),
            "the theme answers a default background query"
        );
    }

    #[test]
    fn mouse_releases_preserve_buttons_and_legacy_reports_are_bounded() {
        for button in 0..=2 {
            assert_eq!(
                encode_sgr_mouse(
                    PtyMouseEvent::Release(button),
                    2,
                    3,
                    PtyMouseMode::Click,
                    true
                )
                .unwrap(),
                format!("\x1b[<{button};3;4m").as_bytes(),
            );
        }
        assert_eq!(
            encode_sgr_mouse(PtyMouseEvent::Press(2), 2, 3, PtyMouseMode::Click, false).unwrap(),
            b"\x1b[M\x22\x23\x24"
        );
        assert_eq!(
            encode_sgr_mouse(PtyMouseEvent::Release(2), 2, 3, PtyMouseMode::Click, false).unwrap(),
            b"\x1b[M\x23\x23\x24"
        );
        assert!(
            encode_sgr_mouse(PtyMouseEvent::Press(0), 223, 0, PtyMouseMode::Click, false).is_none()
        );
        assert!(
            encode_sgr_mouse(PtyMouseEvent::Move(3), 0, 0, PtyMouseMode::Click, false).is_none()
        );
    }

    #[test]
    fn scrollback_eviction_preserves_retained_selection_text() {
        let mut screen = PtyScreen::new(10, 2, 3);
        screen.feed_bytes(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        assert_eq!(screen.scrollback_len(), 3);
        let mut cursor = Some((2, 2));
        let mut anchor = Some((1, 0));
        let mut offset = 1;
        screen.feed_bytes_tracked(b"\r\nsix", &mut cursor, &mut anchor, &mut offset);
        assert_eq!(cursor, Some((1, 2)));
        assert_eq!(anchor, Some((0, 0)));
        assert_eq!(offset, 0);
        assert_eq!(screen.lines()[0], "two");
        assert_eq!(screen.lines()[1], "three");
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
        assert_eq!(
            screen.scrollback_len(),
            2,
            "output above the cursor is kept"
        );
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

    #[test]
    fn paste_sanitization_cannot_reconstruct_escape_sequences() {
        let encoded = encode_paste("\x1b[201\x1b[201\x1b[200~~~\ncommand\n", true, 128).unwrap();
        let payload = &encoded[6..encoded.len() - 6];
        assert!(!payload.contains(&0x1b));
        assert!(payload.ends_with(b"\ncommand\n"));
    }
}
