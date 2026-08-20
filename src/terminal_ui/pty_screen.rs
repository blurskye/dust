use unicode_width::UnicodeWidthChar;

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
    scrollback: Vec<Vec<PtyCell>>,
    maximum_scrollback_lines: usize,
    unsupported_sequences: u64,
    alternate: bool,
    primary_saved: Option<(Vec<Vec<PtyCell>>, usize, usize)>,
    saved_cursor: Option<(usize, usize)>,
    cursor_visible: bool,
    application_cursor: bool,
    bracketed_paste: bool,
    focus_reporting: bool,
    mouse_mode: PtyMouseMode,
    sgr_mouse: bool,
    scroll_top: usize,
    scroll_bottom: usize,
    origin_mode: bool,
    autowrap: bool,
    pending_wrap: bool,
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
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyMouseEvent {
    Press(u8),
    Release,
    Move(u8),
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
            alternate: false,
            primary_saved: None,
            saved_cursor: None,
            cursor_visible: true,
            application_cursor: false,
            bracketed_paste: false,
            focus_reporting: false,
            mouse_mode: PtyMouseMode::Off,
            sgr_mouse: false,
            scroll_top: 0,
            scroll_bottom: rows,
            origin_mode: false,
            autowrap: true,
            pending_wrap: false,
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
                    '7' => self.saved_cursor = Some((self.cursor_row, self.cursor_column)),
                    'D' => self.index(),
                    'E' => {
                        self.cursor_column = 0;
                        self.index();
                    }
                    'M' => self.reverse_index(),
                    '8' => {
                        if let Some((row, column)) = self.saved_cursor {
                            self.cursor_row = row.min(self.rows - 1);
                            self.cursor_column = column.min(self.columns - 1);
                        }
                    }
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
            let drained = self.cells.drain(..self.rows - rows).collect::<Vec<_>>();
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
        if let Some((saved, saved_row, saved_column)) = &mut self.primary_saved {
            saved.resize(rows, vec![PtyCell::default(); columns]);
            for row in saved.iter_mut() {
                row.resize(columns, PtyCell::default());
            }
            *saved_row = (*saved_row).min(rows - 1);
            *saved_column = (*saved_column).min(columns - 1);
        }
        self.rows = rows;
        self.columns = columns;
        self.scroll_top = 0;
        self.scroll_bottom = rows;
        self.pending_wrap = false;
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
            .chain(&self.cells)
            .map(|row| render_row(row))
            .collect()
    }
    #[must_use]
    pub fn history_len(&self) -> usize {
        self.scrollback.len().saturating_add(self.cells.len())
    }
    #[must_use]
    pub fn history_row(&self, row: usize) -> Option<&[PtyCell]> {
        if row < self.scrollback.len() {
            self.scrollback.get(row).map(Vec::as_slice)
        } else {
            self.cells
                .get(row.saturating_sub(self.scrollback.len()))
                .map(Vec::as_slice)
        }
    }
    #[must_use]
    pub fn history_window(&self, start: usize, maximum_rows: usize) -> Vec<Vec<PtyCell>> {
        (start..self.history_len().min(start.saturating_add(maximum_rows)))
            .filter_map(|row| self.history_row(row).map(<[PtyCell]>::to_vec))
            .collect()
    }
    #[must_use]
    pub const fn unsupported_sequences(&self) -> u64 {
        self.unsupported_sequences
    }
    #[must_use]
    pub const fn alternate_active(&self) -> bool {
        self.alternate
    }
    #[must_use]
    pub const fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }
    #[must_use]
    pub const fn application_cursor(&self) -> bool {
        self.application_cursor
    }
    #[must_use]
    pub const fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }
    #[must_use]
    pub const fn focus_reporting(&self) -> bool {
        self.focus_reporting
    }
    #[must_use]
    pub const fn mouse_mode(&self) -> PtyMouseMode {
        self.mouse_mode
    }
    #[must_use]
    pub const fn sgr_mouse(&self) -> bool {
        self.sgr_mouse
    }
    fn put(&mut self, character: char) {
        let width = character.width().unwrap_or(0);
        if self.pending_wrap {
            if self.autowrap {
                self.cursor_column = 0;
                self.index();
            }
            self.pending_wrap = false;
        }
        if width == 0 {
            if self.cursor_column > 0 {
                self.cells[self.cursor_row][self.cursor_column - 1]
                    .text
                    .push(character);
            }
            return;
        }
        if self.cursor_column >= self.columns {
            self.cursor_column = self.columns - 1;
        }
        if width == 2 && self.cursor_column + 1 >= self.columns {
            if self.autowrap {
                self.cursor_column = 0;
                self.index();
            } else {
                return;
            }
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
            self.cursor_column = self.columns - 1;
            self.pending_wrap = self.autowrap;
        }
    }
    fn newline(&mut self) {
        self.cursor_column = 0;
        self.pending_wrap = false;
        self.index();
    }
    fn index(&mut self) {
        if self.cursor_row >= self.scroll_top && self.cursor_row + 1 >= self.scroll_bottom {
            self.scroll_up_region(1)
        } else {
            self.cursor_row = (self.cursor_row + 1).min(self.rows - 1)
        }
    }
    fn reverse_index(&mut self) {
        if self.cursor_row == self.scroll_top {
            self.cells
                .insert(self.scroll_top, vec![PtyCell::default(); self.columns]);
            self.cells.remove(self.scroll_bottom);
        } else {
            self.cursor_row = self.cursor_row.saturating_sub(1).max(self.scroll_top)
        }
    }
    fn scroll_up_region(&mut self, count: usize) {
        for _ in 0..count.min(self.scroll_bottom - self.scroll_top) {
            let row = self.cells.remove(self.scroll_top);
            if self.scroll_top == 0 && self.scroll_bottom == self.rows && !self.alternate {
                self.push_scrollback(row);
            }
            self.cells.insert(
                self.scroll_bottom - 1,
                vec![PtyCell::default(); self.columns],
            );
        }
    }
    fn scroll_down_region(&mut self, count: usize) {
        for _ in 0..count.min(self.scroll_bottom - self.scroll_top) {
            self.cells.remove(self.scroll_bottom - 1);
            self.cells
                .insert(self.scroll_top, vec![PtyCell::default(); self.columns]);
        }
    }
    fn push_scrollback(&mut self, line: Vec<PtyCell>) {
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
            'h' | 'l' if source.starts_with('?') => {
                let enabled = final_byte == 'h';
                for mode in source
                    .trim_start_matches('?')
                    .split(';')
                    .filter_map(|v| v.parse::<usize>().ok())
                {
                    match mode {
                        1 => self.application_cursor = enabled,
                        6 => {
                            self.origin_mode = enabled;
                            self.cursor_row = if enabled { self.scroll_top } else { 0 };
                            self.cursor_column = 0;
                            self.pending_wrap = false;
                        }
                        7 => {
                            self.autowrap = enabled;
                            self.pending_wrap = false;
                        }
                        25 => self.cursor_visible = enabled,
                        47 | 1049 => self.set_alternate(enabled),
                        1000 => {
                            self.mouse_mode = if enabled {
                                PtyMouseMode::Click
                            } else {
                                PtyMouseMode::Off
                            }
                        }
                        1002 => {
                            self.mouse_mode = if enabled {
                                PtyMouseMode::Drag
                            } else {
                                PtyMouseMode::Off
                            }
                        }
                        1003 => {
                            self.mouse_mode = if enabled {
                                PtyMouseMode::Any
                            } else {
                                PtyMouseMode::Off
                            }
                        }
                        1004 => self.focus_reporting = enabled,
                        1006 => self.sgr_mouse = enabled,
                        2004 => self.bracketed_paste = enabled,
                        _ => {
                            self.unsupported_sequences =
                                self.unsupported_sequences.saturating_add(1)
                        }
                    }
                }
            }
            's' => self.saved_cursor = Some((self.cursor_row, self.cursor_column)),
            'u' => {
                if let Some((row, column)) = self.saved_cursor {
                    self.cursor_row = row.min(self.rows - 1);
                    self.cursor_column = column.min(self.columns - 1)
                }
            }
            '@' => {
                let count = first
                    .max(1)
                    .min(self.columns - self.cursor_column.min(self.columns));
                let row = &mut self.cells[self.cursor_row];
                for _ in 0..count {
                    row.insert(self.cursor_column.min(self.columns), PtyCell::default());
                    row.pop();
                }
            }
            'P' => {
                let count = first
                    .max(1)
                    .min(self.columns - self.cursor_column.min(self.columns));
                let row = &mut self.cells[self.cursor_row];
                for _ in 0..count {
                    if self.cursor_column < row.len() {
                        row.remove(self.cursor_column);
                        row.push(PtyCell::default());
                    }
                }
            }
            'A' => self.cursor_row = self.cursor_row.saturating_sub(first.max(1)),
            'B' => self.cursor_row = (self.cursor_row + first.max(1)).min(self.rows - 1),
            'C' => self.cursor_column = (self.cursor_column + first.max(1)).min(self.columns - 1),
            'D' => self.cursor_column = self.cursor_column.saturating_sub(first.max(1)),
            'G' => self.cursor_column = first.max(1).saturating_sub(1).min(self.columns - 1),
            'H' | 'f' => {
                let base = if self.origin_mode { self.scroll_top } else { 0 };
                let maximum = if self.origin_mode {
                    self.scroll_bottom - 1
                } else {
                    self.rows - 1
                };
                self.cursor_row = (base + first.max(1).saturating_sub(1)).min(maximum);
                self.cursor_column = parameters
                    .get(1)
                    .copied()
                    .unwrap_or(1)
                    .max(1)
                    .saturating_sub(1)
                    .min(self.columns - 1)
            }
            'r' => {
                let top = first.max(1).saturating_sub(1);
                let bottom = parameters
                    .get(1)
                    .copied()
                    .unwrap_or(self.rows)
                    .min(self.rows);
                if top < bottom {
                    self.scroll_top = top;
                    self.scroll_bottom = bottom;
                    self.cursor_row = if self.origin_mode { top } else { 0 };
                    self.cursor_column = 0;
                    self.pending_wrap = false;
                } else {
                    self.unsupported_sequences = self.unsupported_sequences.saturating_add(1)
                }
            }
            'L' => {
                if self.cursor_row >= self.scroll_top && self.cursor_row < self.scroll_bottom {
                    for _ in 0..first.max(1).min(self.scroll_bottom - self.cursor_row) {
                        self.cells
                            .insert(self.cursor_row, vec![PtyCell::default(); self.columns]);
                        self.cells.remove(self.scroll_bottom);
                    }
                }
            }
            'M' => {
                if self.cursor_row >= self.scroll_top && self.cursor_row < self.scroll_bottom {
                    for _ in 0..first.max(1).min(self.scroll_bottom - self.cursor_row) {
                        self.cells.remove(self.cursor_row);
                        self.cells.insert(
                            self.scroll_bottom - 1,
                            vec![PtyCell::default(); self.columns],
                        );
                    }
                }
            }
            'S' => self.scroll_up_region(first.max(1)),
            'T' => self.scroll_down_region(first.max(1)),
            'X' => {
                let end = (self.cursor_column + first.max(1)).min(self.columns);
                self.cells[self.cursor_row][self.cursor_column..end].fill(PtyCell::default());
            }
            'J' => self.erase_display(first),
            'K' => self.erase_line(first),
            'm' => self.sgr(&parameters),
            _ => self.unsupported_sequences = self.unsupported_sequences.saturating_add(1),
        }
    }
    fn set_alternate(&mut self, enabled: bool) {
        if enabled && !self.alternate {
            self.primary_saved = Some((
                std::mem::replace(
                    &mut self.cells,
                    vec![vec![PtyCell::default(); self.columns]; self.rows],
                ),
                self.cursor_row,
                self.cursor_column,
            ));
            self.cursor_row = 0;
            self.cursor_column = 0;
            self.alternate = true;
        } else if !enabled && self.alternate {
            if let Some((cells, row, column)) = self.primary_saved.take() {
                self.cells = cells;
                self.cursor_row = row.min(self.rows - 1);
                self.cursor_column = column.min(self.columns - 1);
            }
            self.alternate = false;
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
        let values = if parameters.is_empty() {
            vec![0]
        } else {
            parameters.to_vec()
        };
        let mut index = 0;
        while index < values.len() {
            let value = values[index];
            match value {
                0 => self.style = PtyStyle::default(),
                1 => self.style.bold = true,
                2 => self.style.dim = true,
                3 => self.style.italic = true,
                4 => self.style.underline = true,
                7 => self.style.reverse = true,
                9 => self.style.strikethrough = true,
                22 => {
                    self.style.bold = false;
                    self.style.dim = false
                }
                23 => self.style.italic = false,
                24 => self.style.underline = false,
                27 => self.style.reverse = false,
                29 => self.style.strikethrough = false,
                30..=37 => self.style.foreground = Some(PtyColor::Indexed((value - 30) as u8)),
                39 => self.style.foreground = None,
                40..=47 => self.style.background = Some(PtyColor::Indexed((value - 40) as u8)),
                49 => self.style.background = None,
                90..=97 => self.style.foreground = Some(PtyColor::Indexed((value - 90 + 8) as u8)),
                100..=107 => {
                    self.style.background = Some(PtyColor::Indexed((value - 100 + 8) as u8))
                }
                38 | 48 => {
                    let target_foreground = value == 38;
                    let color = match values.get(index + 1).copied() {
                        Some(5) => {
                            index += 2;
                            values
                                .get(index)
                                .and_then(|v| u8::try_from(*v).ok())
                                .map(PtyColor::Indexed)
                        }
                        Some(2) => {
                            index += 4;
                            if index < values.len() {
                                match (
                                    u8::try_from(values[index - 2]),
                                    u8::try_from(values[index - 1]),
                                    u8::try_from(values[index]),
                                ) {
                                    (Ok(r), Ok(g), Ok(b)) => Some(PtyColor::Rgb(r, g, b)),
                                    _ => None,
                                }
                            } else {
                                index = values.len().saturating_sub(1);
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(color) = color {
                        if target_foreground {
                            self.style.foreground = Some(color)
                        } else {
                            self.style.background = Some(color)
                        }
                    } else {
                        self.unsupported_sequences = self.unsupported_sequences.saturating_add(1)
                    }
                }
                _ => self.unsupported_sequences = self.unsupported_sequences.saturating_add(1),
            }
            index += 1;
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
pub fn encode_terminal_mouse_event(
    event: crate::input::TerminalMouseInput,
    column: u16,
    row: u16,
    left: u16,
    top: u16,
    width: u16,
    height: u16,
    mode: PtyMouseMode,
    sgr: bool,
    ceiling: usize,
) -> Option<Vec<u8>> {
    if column < left
        || row < top
        || column >= left.saturating_add(width)
        || row >= top.saturating_add(height)
    {
        return None;
    }
    let event = match event {
        crate::input::TerminalMouseInput::LeftDown => PtyMouseEvent::Press(0),
        crate::input::TerminalMouseInput::MiddleDown => PtyMouseEvent::Press(1),
        crate::input::TerminalMouseInput::RightDown => PtyMouseEvent::Press(2),
        crate::input::TerminalMouseInput::LeftUp
        | crate::input::TerminalMouseInput::MiddleUp
        | crate::input::TerminalMouseInput::RightUp => PtyMouseEvent::Release,
        crate::input::TerminalMouseInput::LeftDrag => PtyMouseEvent::Move(0),
        crate::input::TerminalMouseInput::MiddleDrag => PtyMouseEvent::Move(1),
        crate::input::TerminalMouseInput::RightDrag => PtyMouseEvent::Move(2),
        crate::input::TerminalMouseInput::Move => PtyMouseEvent::Move(3),
        crate::input::TerminalMouseInput::ScrollUp => PtyMouseEvent::Press(64),
        crate::input::TerminalMouseInput::ScrollDown => PtyMouseEvent::Press(65),
        crate::input::TerminalMouseInput::ScrollLeft => PtyMouseEvent::Press(66),
        crate::input::TerminalMouseInput::ScrollRight => PtyMouseEvent::Press(67),
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

    #[test]
    fn raw_pty_bytes_preserve_split_utf8_and_consume_shell_controls() {
        let mut screen = PtyScreen::new(40, 4, 16);
        screen.feed_bytes(b"\x1b[?2004h\x1b[1;32mgreen \xe7\x95");
        screen.feed_bytes(b"\x8c\x1b[0m\x1b]0;shell title\x07 ready");

        assert_eq!(screen.lines()[0], "green 界 ready");
        assert_eq!(
            screen.cells()[0][0].style,
            PtyStyle {
                foreground: Some(super::PtyColor::Indexed(2)),
                bold: true,
                ..PtyStyle::default()
            }
        );
        assert!(screen.cells()[0][7].continuation);
        assert_eq!(screen.unsupported_sequences(), 0);
    }

    #[test]
    fn alternate_modes_and_extended_sgr_are_streaming_and_restorative() {
        let mut screen = PtyScreen::new(24, 3, 8);
        screen.feed("primary");
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
        let style = screen.cells()[0][0].style;
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
        screen.feed("\x1b[?1049l");
        assert!(!screen.alternate_active());
        assert_eq!(screen.lines()[0], "primary");
        for _ in 0..256 {
            assert_eq!(screen.lines()[0], "primary");
        }
    }

    #[test]
    fn malformed_extended_sgr_preserves_prior_color() {
        for sequence in [
            "\x1b[38;5;256m",
            "\x1b[38;2;256;2;3m",
            "\x1b[38;2;1;2m",
            "\x1b[38;5m",
        ] {
            let mut screen = PtyScreen::new(4, 1, 1);
            screen.feed("\x1b[31m");
            let before = screen.style.foreground;
            let unsupported = screen.unsupported_sequences();
            screen.feed(sequence);
            assert_eq!(screen.style.foreground, before);
            assert_eq!(screen.unsupported_sequences(), unsupported + 1);
        }
        let mut screen = PtyScreen::new(4, 1, 1);
        screen.feed("\x1b[38;5;255mX\x1b[48;2;0;255;0mY");
        assert_eq!(
            screen.cells()[0][0].style.foreground,
            Some(PtyColor::Indexed(255))
        );
        assert_eq!(
            screen.cells()[0][1].style.background,
            Some(PtyColor::Rgb(0, 255, 0))
        );
    }

    #[test]
    fn scroll_regions_origin_wrap_and_line_operations_are_bounded() {
        let mut screen = PtyScreen::new(6, 4, 8);
        for byte in b"one\r\ntwo\r\nthree\r\nfour" {
            screen.feed_bytes(&[*byte]);
        }
        let scrollback = screen.scrollback_len();
        screen.feed("\x1b[2;4r\x1b[?6h\x1b[3;1Hbottom\x1bD");
        assert_eq!(
            screen.scrollback_len(),
            scrollback,
            "interior region never reaches scrollback"
        );
        screen.feed("\x1b[2;1H\x1bLx\x1b[1M\x1b[2S\x1b[1T\x1b[2X");
        assert_eq!(screen.dimensions(), (6, 4));
        screen.feed("\x1b[?6l\x1b[?7l\x1b[1;6HZQ");
        assert_eq!(screen.cursor().0, 5, "disabled autowrap stays at margin");
        let before = (screen.scroll_top, screen.scroll_bottom);
        screen.feed("\x1b[4;2r");
        assert_eq!((screen.scroll_top, screen.scroll_bottom), before);
        assert!(screen.unsupported_sequences() > 0);
        screen.feed("\x1b[?1049h");
        screen.resize(8, 5);
        screen.feed("alternate\x1b[?1049l");
        assert_eq!(screen.dimensions(), (8, 5));
        assert_eq!(screen.cells().len(), 5);
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
            super::encode_terminal_mouse_event(
                crate::input::TerminalMouseInput::LeftDown,
                12,
                23,
                10,
                20,
                8,
                6,
                PtyMouseMode::Click,
                true,
                64
            )
            .unwrap(),
            b"\x1b[<0;3;4M"
        );
        assert!(
            super::encode_terminal_mouse_event(
                crate::input::TerminalMouseInput::Move,
                9,
                20,
                10,
                20,
                8,
                6,
                PtyMouseMode::Any,
                true,
                64
            )
            .is_none()
        );
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
