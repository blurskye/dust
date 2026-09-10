use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::pty_screen::PtyStyle;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Style {
    #[default]
    Default,
    Muted,
    Accent,
    Status,
    StatusAccent,
    Selection,
    Search,
    Error,
    Warning,
    Border,
    BufferLineBackground,
    BufferLineActive,
    BufferLineInactive,
    BufferLineDirty,
    BufferLineIcon,
    BufferLinePin,
    BufferLineDiagnostic,
    BufferLineClose,
    BufferLineSeparator,
    KeyHintBackground,
    KeyHintBorder,
    KeyHintKey,
    KeyHintGroup,
    KeyHintCommand,
    KeyHintPrefix,
    DocumentationBackground,
    DocumentationBorder,
    DocumentationTitle,
    DocumentationText,
    GitSignAdded,
    GitSignChanged,
    GitSignDeleted,
    ExplorerBackground,
    ExplorerHeader,
    ExplorerCount,
    ExplorerTree,
    ExplorerIcon,
    ExplorerDirectory,
    ExplorerFile,
    ExplorerSymlink,
    ExplorerGit,
    ExplorerDiagnostic,
    ExplorerDecoration,
    ExplorerSelection,
    ExplorerBorder,
    TerminalBackground,
    TerminalBorder,
    TerminalTitle,
    TerminalStatus,
    TerminalStatusAccent,
    Terminal(PtyStyle),
    SyntaxComment,
    SyntaxString,
    SyntaxNumber,
    SyntaxKeyword,
    SyntaxType,
    SyntaxVariable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cell {
    pub symbol: String,
    pub style: Style,
    pub continuation: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            symbol: " ".into(),
            style: Style::Default,
            continuation: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CellGrid {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
    control_replacement: String,
    zero_width_replacement: String,
    ambiguous_wide: bool,
}

impl CellGrid {
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self::new_with_policy(width, height, "�", "◌", false)
    }

    #[must_use]
    pub fn new_with_replacements(
        width: u16,
        height: u16,
        control_replacement: &str,
        zero_width_replacement: &str,
    ) -> Self {
        Self::new_with_policy(
            width,
            height,
            control_replacement,
            zero_width_replacement,
            false,
        )
    }

    #[must_use]
    pub fn new_with_policy(
        width: u16,
        height: u16,
        control_replacement: &str,
        zero_width_replacement: &str,
        ambiguous_wide: bool,
    ) -> Self {
        Self {
            width,
            height,
            cells: vec![Cell::default(); usize::from(width) * usize::from(height)],
            control_replacement: control_replacement.into(),
            zero_width_replacement: zero_width_replacement.into(),
            ambiguous_wide,
        }
    }

    #[must_use]
    pub const fn width(&self) -> u16 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }

    #[must_use]
    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        (x < self.width && y < self.height).then(|| &self.cells[self.index(x, y)])
    }

    pub fn set_cell(&mut self, x: u16, y: u16, symbol: &str, style: Style) {
        if x >= self.width || y >= self.height {
            return;
        }
        let safe = self.sanitize_grapheme(symbol);
        let display_width = self.display_width(&safe).clamp(1, 2);
        if x.saturating_add(u16::try_from(display_width).unwrap_or(1)) > self.width {
            return;
        }
        let index = self.index(x, y);
        if self.cells[index].continuation && x > 0 {
            let previous = self.index(x - 1, y);
            self.cells[previous] = Cell::default();
        }
        if x + 1 < self.width {
            let next = self.index(x + 1, y);
            if self.cells[next].continuation {
                self.cells[next] = Cell::default();
            }
        }
        self.cells[index] = Cell {
            symbol: safe,
            style,
            continuation: false,
        };
        if display_width == 2 && x + 1 < self.width {
            let continuation = self.index(x + 1, y);
            self.cells[continuation] = Cell {
                symbol: " ".into(),
                style,
                continuation: true,
            };
        }
    }

    pub fn put_text(&mut self, x: u16, y: u16, text: &str, style: Style) -> u16 {
        self.put_text_clipped(x, y, text, style, self.width)
    }

    /// Write text without touching cells at or beyond `right_exclusive`.
    pub fn put_text_clipped(
        &mut self,
        mut x: u16,
        y: u16,
        text: &str,
        style: Style,
        right_exclusive: u16,
    ) -> u16 {
        let right_exclusive = right_exclusive.min(self.width);
        for grapheme in text.graphemes(true) {
            let safe = if grapheme == "\t" {
                " ".into()
            } else {
                self.sanitize_grapheme(grapheme)
            };
            let width = u16::try_from(self.display_width(&safe).clamp(1, 2)).unwrap_or(1);
            if x.saturating_add(width) > right_exclusive {
                break;
            }
            self.set_cell(x, y, &safe, style);
            x = x.saturating_add(width);
        }
        x
    }

    pub fn fill_row(&mut self, y: u16, style: Style) {
        if y >= self.height {
            return;
        }
        for x in 0..self.width {
            self.set_cell(x, y, " ", style);
        }
    }

    #[must_use]
    pub fn row_text(&self, y: u16) -> String {
        if y >= self.height {
            return String::new();
        }
        (0..self.width)
            .filter_map(|x| self.get(x, y))
            .filter(|cell| !cell.continuation)
            .map(|cell| cell.symbol.as_str())
            .collect()
    }

    fn index(&self, x: u16, y: u16) -> usize {
        usize::from(y) * usize::from(self.width) + usize::from(x)
    }

    pub(crate) fn display_width(&self, text: &str) -> usize {
        if self.ambiguous_wide {
            UnicodeWidthStr::width_cjk(text)
        } else {
            UnicodeWidthStr::width(text)
        }
    }

    pub(crate) fn confirmation_grapheme(&self, grapheme: &str) -> String {
        if grapheme == "\t" {
            "\\t".into()
        } else {
            self.sanitize_grapheme(grapheme)
        }
    }

    fn sanitize_grapheme(&self, grapheme: &str) -> String {
        if grapheme.chars().any(char::is_control) {
            self.control_replacement.clone()
        } else if self.display_width(grapheme) == 0 {
            self.zero_width_replacement.clone()
        } else {
            grapheme.into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CellGrid, Style};

    #[test]
    fn wide_graphemes_mark_continuation_and_controls_are_sanitized() {
        let mut grid = CellGrid::new(8, 1);
        grid.put_text(0, 0, "界\u{1b}x", Style::Default);
        assert_eq!(grid.get(0, 0).expect("wide").symbol, "界");
        assert!(grid.get(1, 0).expect("continuation").continuation);
        assert_eq!(grid.get(2, 0).expect("safe").symbol, "�");

        let mut configured = CellGrid::new_with_replacements(3, 1, "!", "?");
        configured.put_text(0, 0, "\u{1b}", Style::Default);
        assert_eq!(configured.get(0, 0).expect("configured").symbol, "!");

        let mut wide = CellGrid::new_with_policy(3, 1, "!", "?", true);
        wide.put_text(0, 0, "·x", Style::Default);
        assert!(wide.get(1, 0).expect("ambiguous continuation").continuation);
        assert_eq!(wide.get(2, 0).expect("following cell").symbol, "x");
    }

    #[test]
    fn clipped_text_never_crosses_its_region_or_splits_a_wide_grapheme() {
        let mut grid = CellGrid::new(8, 1);
        grid.set_cell(4, 0, "|", Style::Border);
        grid.put_text_clipped(1, 0, "abcdef", Style::Accent, 4);
        assert_eq!(grid.row_text(0), " abc|   ");

        let mut wide = CellGrid::new(4, 1);
        wide.set_cell(2, 0, "|", Style::Border);
        wide.put_text_clipped(1, 0, "界", Style::Accent, 2);
        assert_eq!(wide.row_text(0), "  | ");
    }
}
