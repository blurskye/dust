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
}
impl Default for PtyCell {
    fn default() -> Self {
        Self {
            text: " ".into(),
            style: PtyStyle::default(),
        }
    }
}
#[derive(Clone, Debug)]
enum Parser {
    Ground,
    Escape,
    Csi(String),
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
                Parser::Escape => {
                    if character == '[' {
                        self.parser = Parser::Csi(String::new())
                    } else {
                        self.unsupported_sequences = self.unsupported_sequences.saturating_add(1)
                    }
                }
                Parser::Csi(mut parameters) => {
                    if ('@'..='~').contains(&character) {
                        self.csi(&parameters, character)
                    } else if parameters.len() < 128
                        && (character.is_ascii_digit() || matches!(character, ';' | '?' | ':'))
                    {
                        parameters.push(character);
                        self.parser = Parser::Csi(parameters)
                    } else {
                        self.unsupported_sequences = self.unsupported_sequences.saturating_add(1)
                    }
                }
            }
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
        };
        if width == 2 && self.cursor_column + 1 < self.columns {
            self.cells[self.cursor_row][self.cursor_column + 1] = PtyCell {
                text: String::new(),
                style: self.style,
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
