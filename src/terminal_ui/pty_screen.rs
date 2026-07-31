use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PtyStyle {
    pub foreground: Option<u8>,
    pub background: Option<u8>,
    pub bold: bool,
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
#[derive(Clone, Debug)]
enum Parser {
    Ground,
    Escape,
    EscapeIntermediate,
    Csi {
        parameters: String,
        overflowed: bool,
    },
    Osc,
    OscEscape,
    StringControl,
    StringControlEscape,
}
#[derive(Clone, Debug)]
pub struct PtyScreen {
    rows: usize,
    columns: usize,
    cursor_row: usize,
    cursor_column: usize,
    cells: Vec<Vec<PtyCell>>,
    style: PtyStyle,
    parser: Parser,
    utf8_pending: Vec<u8>,
    scrollback: Vec<String>,
    maximum_scrollback_lines: usize,
    unsupported_sequences: u64,
}
impl PtyScreen {
    pub fn new(columns: u16, rows: u16, maximum_scrollback_lines: usize) -> Self {
        let columns = usize::from(columns.max(1));
        let rows = usize::from(rows.max(1));
        Self {
            rows,
            columns,
            cursor_row: 0,
            cursor_column: 0,
            cells: vec![vec![PtyCell::default(); columns]; rows],
            style: PtyStyle::default(),
            parser: Parser::Ground,
            utf8_pending: Vec::new(),
            scrollback: Vec::new(),
            maximum_scrollback_lines: maximum_scrollback_lines.max(1),
            unsupported_sequences: 0,
        }
    }
    pub fn feed(&mut self, text: &str) {
        for character in text.chars() {
            let parser = std::mem::replace(&mut self.parser, Parser::Ground);
            match parser {
                Parser::Ground => match character {
                    '\u{1b}' => self.parser = Parser::Escape,
                    '\r' => self.cursor_column = 0,
                    '\n' => self.newline(),
                    '\u{8}' => self.cursor_column = self.cursor_column.saturating_sub(1),
                    '\t' => {
                        let next = (self.cursor_column / 8 + 1) * 8;
                        self.cursor_column = next.min(self.columns.saturating_sub(1));
                    }
                    character if !character.is_control() => self.put(character),
                    _ => {}
                },
                Parser::Escape => match character {
                    '[' => {
                        self.parser = Parser::Csi {
                            parameters: String::new(),
                            overflowed: false,
                        }
                    }
                    ']' => self.parser = Parser::Osc,
                    'P' | '_' | '^' => self.parser = Parser::StringControl,
                    '(' | ')' | '*' | '+' => self.parser = Parser::EscapeIntermediate,
                    _ => self.unsupported_sequences = self.unsupported_sequences.saturating_add(1),
                },
                Parser::EscapeIntermediate => {}
                Parser::Csi {
                    mut parameters,
                    mut overflowed,
                } => {
                    if ('@'..='~').contains(&character) {
                        if overflowed {
                            self.unsupported_sequences =
                                self.unsupported_sequences.saturating_add(1)
                        } else {
                            self.csi(&parameters, character)
                        }
                    } else if (' '..='?').contains(&character) {
                        if parameters.len() < 128 {
                            parameters.push(character)
                        } else {
                            overflowed = true
                        }
                        self.parser = Parser::Csi {
                            parameters,
                            overflowed,
                        }
                    } else {
                        self.unsupported_sequences = self.unsupported_sequences.saturating_add(1)
                    }
                }
                Parser::Osc => match character {
                    '\u{7}' => {}
                    '\u{1b}' => self.parser = Parser::OscEscape,
                    _ => self.parser = Parser::Osc,
                },
                Parser::OscEscape => {
                    if character != '\\' {
                        self.parser = if character == '\u{1b}' {
                            Parser::OscEscape
                        } else {
                            Parser::Osc
                        }
                    }
                }
                Parser::StringControl => {
                    self.parser = if character == '\u{1b}' {
                        Parser::StringControlEscape
                    } else {
                        Parser::StringControl
                    }
                }
                Parser::StringControlEscape => {
                    if character != '\\' {
                        self.parser = if character == '\u{1b}' {
                            Parser::StringControlEscape
                        } else {
                            Parser::StringControl
                        }
                    }
                }
            }
        }
    }
    pub fn feed_bytes(&mut self, bytes: &[u8]) {
        let mut input = std::mem::take(&mut self.utf8_pending);
        input.extend_from_slice(bytes);
        let mut remaining = input.as_slice();
        while !remaining.is_empty() {
            match std::str::from_utf8(remaining) {
                Ok(text) => {
                    self.feed(text);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        self.feed(std::str::from_utf8(&remaining[..valid]).expect("valid prefix"));
                    }
                    if let Some(length) = error.error_len() {
                        self.feed("\u{fffd}");
                        remaining = &remaining[valid.saturating_add(length)..];
                    } else {
                        self.utf8_pending.extend_from_slice(&remaining[valid..]);
                        break;
                    }
                }
            }
        }
    }
    pub fn finish_bytes(&mut self) {
        if !self.utf8_pending.is_empty() {
            self.utf8_pending.clear();
            self.feed("\u{fffd}");
        }
    }
    pub fn resize(&mut self, columns: u16, rows: u16) {
        let columns = usize::from(columns.max(1));
        let rows = usize::from(rows.max(1));
        if rows < self.rows {
            let drained = self
                .cells
                .drain(..self.rows - rows)
                .map(|row| render_row(&row))
                .collect::<Vec<_>>();
            for row in drained {
                self.push_scrollback(row);
            }
        } else {
            self.cells
                .extend((0..rows - self.rows).map(|_| vec![PtyCell::default(); self.columns]));
        }
        for row in &mut self.cells {
            row.resize(columns, PtyCell::default());
        }
        self.rows = rows;
        self.columns = columns;
        self.cursor_row = self.cursor_row.min(rows - 1);
        self.cursor_column = self.cursor_column.min(columns - 1);
    }
    #[must_use]
    pub const fn dimensions(&self) -> (usize, usize) {
        (self.columns, self.rows)
    }
    #[must_use]
    pub const fn cursor(&self) -> (usize, usize) {
        (self.cursor_column, self.cursor_row)
    }
    #[must_use]
    pub fn cells(&self) -> &[Vec<PtyCell>] {
        &self.cells
    }
    #[must_use]
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }
    pub fn lines(&self) -> Vec<String> {
        self.scrollback
            .iter()
            .cloned()
            .chain(self.cells.iter().map(|row| render_row(row)))
            .collect()
    }
    #[must_use]
    pub const fn unsupported_sequences(&self) -> u64 {
        self.unsupported_sequences
    }
    fn put(&mut self, character: char) {
        let width = character.width().unwrap_or(0);
        if width == 0 {
            if self.cursor_column > 0 {
                self.cells[self.cursor_row][self.cursor_column - 1]
                    .text
                    .push(character);
            }
            return;
        }
        if self.cursor_column >= self.columns {
            self.newline()
        }
        self.cells[self.cursor_row][self.cursor_column] = PtyCell {
            text: character.to_string(),
            style: self.style,
            continuation: false,
        };
        if width == 2 && self.cursor_column + 1 < self.columns {
            self.cells[self.cursor_row][self.cursor_column + 1] = PtyCell {
                text: String::new(),
                style: self.style,
                continuation: true,
            };
        }
        self.cursor_column += width;
        if self.cursor_column >= self.columns {
            self.cursor_column = self.columns;
        }
    }
    fn newline(&mut self) {
        self.cursor_column = 0;
        if self.cursor_row + 1 >= self.rows {
            let row = self.cells.remove(0);
            self.push_scrollback(render_row(&row));
            self.cells.push(vec![PtyCell::default(); self.columns]);
        } else {
            self.cursor_row += 1
        }
    }
    fn push_scrollback(&mut self, line: String) {
        self.scrollback.push(line);
        if self.scrollback.len() > self.maximum_scrollback_lines {
            let excess = self.scrollback.len() - self.maximum_scrollback_lines;
            self.scrollback.drain(..excess);
        }
    }
    fn csi(&mut self, source: &str, final_byte: char) {
        let parameters = source
            .trim_start_matches('?')
            .split(';')
            .map(|value| value.parse::<usize>().unwrap_or(0))
            .collect::<Vec<_>>();
        let first = parameters.first().copied().unwrap_or(0);
        match final_byte {
            'h' | 'l' if source == "?2004" => {}
            'A' => self.cursor_row = self.cursor_row.saturating_sub(first.max(1)),
            'B' => self.cursor_row = (self.cursor_row + first.max(1)).min(self.rows - 1),
            'C' => self.cursor_column = (self.cursor_column + first.max(1)).min(self.columns - 1),
            'D' => self.cursor_column = self.cursor_column.saturating_sub(first.max(1)),
            'G' => self.cursor_column = first.max(1).saturating_sub(1).min(self.columns - 1),
            'H' | 'f' => {
                self.cursor_row = first.max(1).saturating_sub(1).min(self.rows - 1);
                self.cursor_column = parameters
                    .get(1)
                    .copied()
                    .unwrap_or(1)
                    .max(1)
                    .saturating_sub(1)
                    .min(self.columns - 1)
            }
            'J' => self.erase_display(first),
            'K' => self.erase_line(first),
            'm' => self.sgr(&parameters),
            _ => self.unsupported_sequences = self.unsupported_sequences.saturating_add(1),
        }
    }
    fn erase_line(&mut self, mode: usize) {
        match mode {
            0 => {
                for cell in &mut self.cells[self.cursor_row][self.cursor_column.min(self.columns)..]
                {
                    *cell = PtyCell::default()
                }
            }
            1 => {
                for cell in
                    &mut self.cells[self.cursor_row][..=self.cursor_column.min(self.columns - 1)]
                {
                    *cell = PtyCell::default()
                }
            }
            2 => self.cells[self.cursor_row].fill(PtyCell::default()),
            _ => self.unsupported_sequences = self.unsupported_sequences.saturating_add(1),
        }
    }
    fn erase_display(&mut self, mode: usize) {
        match mode {
            0 => {
                self.erase_line(0);
                for row in self.cells.iter_mut().skip(self.cursor_row + 1) {
                    row.fill(PtyCell::default())
                }
            }
            1 => {
                self.erase_line(1);
                for row in self.cells.iter_mut().take(self.cursor_row) {
                    row.fill(PtyCell::default())
                }
            }
            2 => {
                for row in &mut self.cells {
                    row.fill(PtyCell::default())
                }
            }
            _ => self.unsupported_sequences = self.unsupported_sequences.saturating_add(1),
        }
    }
    fn sgr(&mut self, parameters: &[usize]) {
        for value in if parameters.is_empty() {
            &[0][..]
        } else {
            parameters
        } {
            match *value {
                0 => self.style = PtyStyle::default(),
                1 => self.style.bold = true,
                22 => self.style.bold = false,
                30..=37 => self.style.foreground = Some((*value - 30) as u8),
                39 => self.style.foreground = None,
                40..=47 => self.style.background = Some((*value - 40) as u8),
                49 => self.style.background = None,
                90..=97 => self.style.foreground = Some((*value - 90 + 8) as u8),
                100..=107 => self.style.background = Some((*value - 100 + 8) as u8),
                _ => self.unsupported_sequences = self.unsupported_sequences.saturating_add(1),
            }
        }
    }
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
    use super::{PtyScreen, PtyStyle};

    #[test]
    fn raw_pty_bytes_preserve_split_utf8_and_consume_shell_controls() {
        let mut screen = PtyScreen::new(40, 4, 16);
        screen.feed_bytes(b"\x1b[?2004h\x1b[1;32mgreen \xe7\x95");
        screen.feed_bytes(b"\x8c\x1b[0m\x1b]0;shell title\x07 ready");

        assert_eq!(screen.lines()[0], "green 界 ready");
        assert_eq!(
            screen.cells()[0][0].style,
            PtyStyle {
                foreground: Some(2),
                background: None,
                bold: true,
            }
        );
        assert!(screen.cells()[0][7].continuation);
        assert_eq!(screen.unsupported_sequences(), 0);
    }

    #[test]
    fn raw_pty_bytes_replace_invalid_or_incomplete_utf8_without_exposing_controls() {
        let mut screen = PtyScreen::new(20, 2, 4);
        screen.feed_bytes(b"ok\xff\xe2\x82");
        screen.finish_bytes();
        assert_eq!(screen.lines()[0], "ok��");
        assert!(!screen.lines()[0].contains('\u{1b}'));
    }
}
