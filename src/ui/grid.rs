//! Terminal cells, styles and frame buffers.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::ui::pty_screen::PtyStyle;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExplorerSegmentRole {
    Tree,
    Icon,
    IconAzure,
    IconBlue,
    IconCyan,
    IconGreen,
    IconGrey,
    IconOrange,
    IconPurple,
    IconRed,
    IconYellow,
    Directory,
    File,
    Symlink,
    Loading,
    Git,
    Diagnostic,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Style {
    #[default]
    Default,
    Muted,
    Accent,
    Status,
    StatusMuted,
    StatusAccent,
    StatusInsert,
    StatusVisual,
    StatusReplace,
    StatusCommand,
    StatusSection,
    StatusEdgeNormal,
    StatusEdgeInsert,
    StatusEdgeVisual,
    StatusEdgeReplace,
    StatusEdgeCommand,
    StatusEdgeSection,
    StatusError,
    StatusWarning,
    Selection,
    Search,
    Error,
    Warning,
    /// Text a diagnostic covers: its severity's colour, underlined as Neovim marks it, so a
    /// link inside keeps its underline.
    DiagnosticError,
    DiagnosticWarning,
    DiagnosticHint,
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
    /// An explorer foreground on its selected row's background.
    ExplorerSelected(ExplorerSegmentRole),
    ExplorerBorder,
    TerminalBackground,
    TerminalBorder,
    TerminalTitle,
    TerminalStatus,
    TerminalStatusAccent,
    Terminal(PtyStyle),
    /// A Neovim highlight group, coloured by the theme's `[highlights]`.
    Syntax(crate::config::Highlight),
    /// A syntax foreground drawn on the selection background.
    SelectionSyntax(crate::config::Highlight),
    /// A syntax group drawn on the documentation popup's background.
    DocumentationSyntax(crate::config::Highlight),
    /// A markdown heading's group drawn on its bar.
    MarkdownHeading(crate::config::Highlight),
    /// A syntax group, or plain text, drawn on a markdown code block's background.
    MarkdownCode(Option<crate::config::Highlight>),
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
        self.set_cell_with_width(x, y, &safe, style, display_width);
    }

    /// Write a validated cell whose column width is owned by its source protocol.
    pub(crate) fn set_cell_with_width(
        &mut self,
        x: u16,
        y: u16,
        symbol: &str,
        style: Style,
        display_width: usize,
    ) {
        let display_width = display_width.clamp(1, 2);
        if x >= self.width || y >= self.height || display_width > usize::from(self.width - x) {
            return;
        }
        let index = self.index(x, y);
        for occupied in x..x + display_width as u16 {
            let at = self.index(occupied, y);
            if self.cells[at].continuation && occupied > 0 {
                let previous = self.index(occupied - 1, y);
                self.cells[previous] = Cell::default();
            }
            if occupied + 1 < self.width {
                let next = self.index(occupied + 1, y);
                if self.cells[next].continuation {
                    self.cells[next] = Cell::default();
                }
            }
        }
        self.cells[index] = Cell {
            symbol: symbol.into(),
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
            if x >= right_exclusive || width > right_exclusive - x {
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

    pub(crate) fn border_glyphs(
        &self,
        style: crate::config::BorderStyle,
    ) -> (char, char, char, char, char, char) {
        let glyphs = style.glyphs();
        if self.display_width(&glyphs.0.to_string()) == 1 {
            glyphs
        } else {
            ('-', '|', '+', '+', '+', '+')
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
    use crate::ui::grid::CellGrid;
    use crate::ui::grid::Style;

    #[test]
    fn overlapping_wide_writes_clear_the_old_trailing_cell() {
        let mut grid = crate::ui::grid::CellGrid::new(6, 1);
        grid.set_cell(2, 0, "界", crate::ui::grid::Style::Default);
        grid.set_cell(1, 0, "語", crate::ui::grid::Style::Default);
        assert!(grid.get(2, 0).unwrap().continuation);
        assert!(!grid.get(3, 0).unwrap().continuation);
        assert_eq!(grid.get(3, 0).unwrap().symbol, " ");
    }

    #[test]
    fn wide_writes_at_the_maximum_grid_edge_are_rejected() {
        let mut grid = CellGrid::new(u16::MAX, 1);
        let last = u16::MAX - 1;
        grid.set_cell(last, 0, "界", Style::Accent);
        assert_eq!(grid.get(last, 0), Some(&crate::ui::grid::Cell::default()));
        assert_eq!(grid.put_text(last, 0, "界", Style::Accent), last);
        assert_eq!(grid.get(last, 0), Some(&crate::ui::grid::Cell::default()));
        grid.set_cell(last - 1, 0, "界", Style::Accent);
        assert!(grid.get(last, 0).unwrap().continuation);
        grid.set_cell(last, 0, "x", Style::Default);
        assert_eq!(grid.get(last, 0).unwrap().symbol, "x");
    }

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
    fn border_glyphs_fit_one_column_under_both_width_policies() {
        for wide in [false, true] {
            let grid = CellGrid::new_with_policy(8, 1, "�", "◌", wide);
            for style in [
                crate::config::BorderStyle::Single,
                crate::config::BorderStyle::Double,
                crate::config::BorderStyle::Rounded,
            ] {
                let (h, v, tl, tr, bl, br) = grid.border_glyphs(style);
                for glyph in [h, v, tl, tr, bl, br] {
                    assert_eq!(grid.display_width(&glyph.to_string()), 1);
                }
                assert_eq!(h, if wide { '-' } else { style.glyphs().0 });
            }
        }
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

use crossterm::cursor::SetCursorStyle;

pub(crate) const CONTROL_CHARACTER_REPLACEMENT: &str = "�";

pub(crate) const ZERO_WIDTH_REPLACEMENT: &str = "◌";

#[derive(Clone, Debug)]
pub struct Frame {
    pub grid: CellGrid,
    pub cursor: Option<(u16, u16, SetCursorStyle)>,
}
