//! Unified patch parsing and the diff data used by previews.

use unicode_segmentation::UnicodeSegmentation;

/// Parse unified diff text into a `DiffPreview`.
///
/// Deliberately not represented in the model, and therefore not produced here: file headers
/// (`diff --git`, `index`, `---`, `+++`, mode lines) and the `\ No newline at end of file`
/// marker, which annotates the preceding line rather than being a line of its own. A blank
/// line or an unrecognised header ends the current hunk and starts a new file's run, so a
/// multi-file patch produces each file's hunks with its own collapsed counts.
#[must_use]
pub fn parse_unified_patch(text: &str, provenance: impl Into<String>) -> DiffPreview {
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut current: Option<HunkBuilder> = None;
    // Exclusive end of the previous hunk's old range within the file being read, so the
    // collapsed count is computed from the real hunk headers rather than guessed.
    let mut previous_end: Option<u32> = None;
    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix("@@") {
            if let Some(builder) = current.take() {
                let end = builder.old_end();
                hunks.push(builder.finish(previous_end));
                previous_end = Some(end);
            }
            match parse_hunk_header(rest) {
                Some((old_start, new_start)) => {
                    current = Some(HunkBuilder::new(raw.to_owned(), old_start, new_start));
                }
                // A malformed header cannot be numbered, so it ends the run rather than
                // producing lines attributed to a hunk that does not exist.
                None => previous_end = None,
            }
            continue;
        }
        let Some(builder) = current.as_mut() else {
            continue;
        };
        match raw.as_bytes().first() {
            Some(b'\\') => {}
            Some(b'+') => builder.push(DiffLineKind::Added, raw.get(1..).unwrap_or_default()),
            Some(b'-') => builder.push(DiffLineKind::Removed, raw.get(1..).unwrap_or_default()),
            Some(b' ') => builder.push(DiffLineKind::Context, raw.get(1..).unwrap_or_default()),
            // A blank line or a file header ends this hunk and this file's run.
            _ => {
                if let Some(builder) = current.take() {
                    hunks.push(builder.finish(previous_end));
                }
                previous_end = None;
            }
        }
    }
    if let Some(builder) = current.take() {
        hunks.push(builder.finish(previous_end));
    }
    DiffPreview {
        hunks,
        provenance: provenance.into(),
        truncated: false,
        total_lines: 0,
        layout: DiffLayout::Unified,
    }
}

/// `@@ -31,6 +31,9 @@ section` → the old and new starting line numbers. A range with no
/// comma covers exactly one line.
fn parse_hunk_header(after_at: &str) -> Option<(u32, u32)> {
    let (ranges, _) = after_at.split_once("@@")?;
    let mut parts = ranges.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    Some((parse_range(old)?.0, parse_range(new)?.0))
}

fn parse_range(text: &str) -> Option<(u32, u32)> {
    match text.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

struct HunkBuilder {
    header: String,
    old_start: u32,
    old_next: u32,
    new_next: u32,
    lines: Vec<DiffLine>,
}

impl HunkBuilder {
    fn new(header: String, old_start: u32, new_start: u32) -> Self {
        Self {
            header,
            old_start,
            old_next: old_start,
            new_next: new_start,
            lines: Vec::new(),
        }
    }

    /// Exclusive end of the old range this hunk covers, which is where the next hunk's
    /// collapsed count starts counting from.
    fn old_end(&self) -> u32 {
        self.old_next
    }

    fn push(&mut self, kind: DiffLineKind, text: &str) {
        let (old_number, new_number) = match kind {
            DiffLineKind::Added => {
                let number = self.new_next;
                self.new_next = self.new_next.saturating_add(1);
                (None, Some(number))
            }
            DiffLineKind::Removed => {
                let number = self.old_next;
                self.old_next = self.old_next.saturating_add(1);
                (Some(number), None)
            }
            DiffLineKind::Context => {
                let old = self.old_next;
                let new = self.new_next;
                self.old_next = self.old_next.saturating_add(1);
                self.new_next = self.new_next.saturating_add(1);
                (Some(old), Some(new))
            }
        };
        self.lines.push(DiffLine {
            kind,
            old_number,
            new_number,
            text: text.to_owned(),
            spans: Vec::new(),
        });
    }

    fn finish(mut self, previous_end: Option<u32>) -> DiffHunk {
        emphasise_changed_spans(&mut self.lines);
        let collapsed_before = match previous_end {
            Some(end) => self.old_start.saturating_sub(end),
            None => self.old_start.saturating_sub(1),
        };
        DiffHunk {
            header: self.header,
            lines: self.lines,
            collapsed_before: usize::try_from(collapsed_before).unwrap_or(usize::MAX),
        }
    }
}

/// Pair each run of removed lines with the run of added lines that follows it and mark the
/// byte ranges that actually changed, so a one-character edit in a long line is visible.
///
/// Public so the hunk-based path shares it: a diff built from the structured hunk read and a
/// diff built by parsing patch text must agree about what changed inside a line.
pub fn emphasise_changed_spans(lines: &mut [DiffLine]) {
    let mut index = 0;
    while index < lines.len() {
        if lines[index].kind != DiffLineKind::Removed {
            index += 1;
            continue;
        }
        let removed_start = index;
        while index < lines.len() && lines[index].kind == DiffLineKind::Removed {
            index += 1;
        }
        let added_start = index;
        while index < lines.len() && lines[index].kind == DiffLineKind::Added {
            index += 1;
        }
        let pairs = (added_start - removed_start).min(index - added_start);
        for pair in 0..pairs {
            let (old, new) = changed_spans(
                &lines[removed_start + pair].text,
                &lines[added_start + pair].text,
            );
            lines[removed_start + pair].spans = old;
            lines[added_start + pair].spans = new;
        }
    }
}

/// The changed byte ranges of two lines: what lies between their common prefix and their
/// common suffix. A line wholly contained in the other yields no range, so nothing is
/// emphasised rather than the whole line being marked.
fn changed_spans(old: &str, new: &str) -> (Vec<DiffSpan>, Vec<DiffSpan>) {
    let old_chars: Vec<&str> = old.graphemes(true).collect();
    let new_chars: Vec<&str> = new.graphemes(true).collect();
    let mut prefix = 0;
    while prefix < old_chars.len()
        && prefix < new_chars.len()
        && old_chars[prefix] == new_chars[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_chars.len().saturating_sub(prefix)
        && suffix < new_chars.len().saturating_sub(prefix)
        && old_chars[old_chars.len() - 1 - suffix] == new_chars[new_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let span = |chars: &[&str], text: &str| -> Vec<DiffSpan> {
        let end = chars.len().saturating_sub(suffix);
        if end <= prefix {
            return Vec::new();
        }
        vec![DiffSpan {
            start: byte_of(text, prefix),
            end: byte_of(text, end),
        }]
    };
    (span(&old_chars, old), span(&new_chars, new))
}

fn byte_of(text: &str, char_index: usize) -> usize {
    text.grapheme_indices(true)
        .nth(char_index)
        .map_or(text.len(), |(byte, _)| byte)
}

#[cfg(test)]
mod tests {
    use crate::ui::diff::*;

    #[test]
    fn changed_accents_emphasise_the_whole_grapheme() {
        let (old, new) = changed_spans("áx", "àx");
        assert_eq!((old[0].start, old[0].end), (0, "á".len()));
        assert_eq!((new[0].start, new[0].end), (0, "à".len()));
    }

    /// Verbatim `git diff` output, captured by hand from a scratch repository so the parser is
    /// tested against real patch text rather than a structure someone hand-built. Two hunks
    /// with a one-word change in each, and 25 unchanged lines between them.
    const REAL_PATCH: &str = "\
diff --git a/f.txt b/f.txt
index 9792656..89d6f9e 100644
--- a/f.txt
+++ b/f.txt
@@ -1,6 +1,6 @@
 line one
 line two
-line three
+line three changed
 line four
 line five
 line six
@@ -32,7 +32,7 @@ line thirty-one
 line thirty-two
 line thirty-three
 line thirty-four
-line thirty-five
+line thirty-five modified
 line thirty-six
 line thirty-seven
 line thirty-eight
";

    #[test]
    fn real_patch_text_parses_into_hunks_lines_and_numbers() {
        let diff = parse_unified_patch(REAL_PATCH, "unstaged = index -> disk");
        assert_eq!(diff.hunks.len(), 2, "two hunks");
        let first = &diff.hunks[0];
        assert_eq!(first.header, "@@ -1,6 +1,6 @@");
        assert_eq!(
            first.lines.len(),
            7,
            "two context, a removal, an addition, three context"
        );
        // The file headers are not diff lines.
        assert!(
            !first
                .lines
                .iter()
                .any(|line| line.text.contains("diff --git")),
            "a file header must not become a diff line"
        );
        assert_eq!(
            (
                first.lines[2].kind,
                first.lines[2].old_number,
                first.lines[2].new_number
            ),
            (DiffLineKind::Removed, Some(3), None),
            "a removed line has an old number and no new number"
        );
        assert_eq!(
            (
                first.lines[3].kind,
                first.lines[3].old_number,
                first.lines[3].new_number
            ),
            (DiffLineKind::Added, None, Some(3)),
            "an added line has a new number and no old number"
        );
        assert_eq!(
            (
                first.lines[0].kind,
                first.lines[0].old_number,
                first.lines[0].new_number
            ),
            (DiffLineKind::Context, Some(1), Some(1)),
            "a context line has both numbers"
        );
        assert_eq!(
            (first.lines[6].old_number, first.lines[6].new_number),
            (Some(6), Some(6)),
            "numbering advances through the hunk"
        );
        let second = &diff.hunks[1];
        assert_eq!(second.header, "@@ -32,7 +32,7 @@ line thirty-one");
        assert_eq!(
            second.lines[0].old_number,
            Some(32),
            "the second hunk starts at the line its header names"
        );
    }

    #[test]
    fn collapsed_counts_come_from_the_real_hunk_headers() {
        let diff = parse_unified_patch(REAL_PATCH, "p");
        // The first hunk starts at line 1, so nothing precedes it.
        assert_eq!(diff.hunks[0].collapsed_before, 0);
        // The first hunk covers old lines 1..=6, so lines 7..=31 are the 25 that were skipped.
        assert_eq!(diff.hunks[1].collapsed_before, 25);
        assert!(
            diff.collapsed_text(diff.hunks[1].collapsed_before)
                .contains("25 unchanged lines"),
            "the collapsed row states the exact count"
        );
    }

    #[test]
    fn intra_line_spans_mark_only_what_changed() {
        let diff = parse_unified_patch(REAL_PATCH, "p");
        let first = &diff.hunks[0];
        let removed = &first.lines[2];
        let added = &first.lines[3];
        assert!(
            removed.spans.is_empty(),
            "a removed line whose whole text is a prefix of the added one marks nothing"
        );
        assert_eq!(
            added.spans.len(),
            1,
            "the added line marks one changed span"
        );
        let span = added.spans[0];
        assert_eq!(&added.text[span.start..span.end], " changed");
        let second = &diff.hunks[1];
        let span = second.lines[4].spans[0];
        assert_eq!(&second.lines[4].text[span.start..span.end], " modified");
    }

    #[test]
    fn spans_land_on_character_boundaries_for_multibyte_text() {
        let patch = "@@ -1 +1 @@\n-\u{4e00}\u{4e8c}\u{4e09}\n+\u{4e00}\u{4e8c}\u{56db}\n";
        let diff = parse_unified_patch(patch, "p");
        let added = &diff.hunks[0].lines[1];
        assert_eq!(added.spans.len(), 1);
        let span = added.spans[0];
        assert!(
            added.text.is_char_boundary(span.start) && added.text.is_char_boundary(span.end),
            "a span must never split a character"
        );
        assert_eq!(&added.text[span.start..span.end], "\u{56db}");
    }

    #[test]
    fn a_single_line_range_needs_no_count_and_a_newline_marker_is_not_a_line() {
        let patch = "@@ -5 +5 @@\n-old\n+new\n\\ No newline at end of file\n";
        let diff = parse_unified_patch(patch, "p");
        let hunk = &diff.hunks[0];
        assert_eq!(hunk.lines.len(), 2, "the newline marker is not a diff line");
        assert_eq!(hunk.lines[0].old_number, Some(5));
        assert_eq!(hunk.lines[1].new_number, Some(5));
    }

    #[test]
    fn a_second_file_starts_its_own_collapsed_run() {
        let patch = "@@ -3,1 +3,1 @@\n-a\n+b\n\ndiff --git a/g.txt b/g.txt\n--- a/g.txt\n+++ b/g.txt\n@@ -3,1 +3,1 @@\n-c\n+d\n";
        let diff = parse_unified_patch(patch, "p");
        assert_eq!(diff.hunks.len(), 2);
        assert_eq!(
            diff.hunks[0].collapsed_before, 2,
            "the first hunk of the first file counts from its own start"
        );
        assert_eq!(
            diff.hunks[1].collapsed_before, 2,
            "a second file counts from its own start, not from the previous file's end"
        );
    }
}

/// How a diff line relates to the comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

/// A byte range inside a diff line's text that changed, for intra-line emphasis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiffSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// The line's number on the old side, absent for a line the old side does not have.
    pub old_number: Option<u32>,
    /// The line's number on the new side, absent for a line the new side does not have.
    pub new_number: Option<u32>,
    pub text: String,
    pub spans: Vec<DiffSpan>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffHunk {
    /// The `@@` header, including the enclosing section where the language provides one.
    pub header: String,
    pub lines: Vec<DiffLine>,
    /// Unchanged lines omitted immediately before this hunk, with the exact count.
    pub collapsed_before: usize,
}

/// How a diff is laid out.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DiffLayout {
    /// One column, as a patch is written. This is the default.
    #[default]
    Unified,
    /// Old and new side by side, paired line for line.
    SideBySide,
}

/// One unified diff row, ready to paint: its text and the spans inside that text that
/// changed, already in the text's own coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffRow {
    pub kind: DiffRowKind,
    pub text: String,
    pub spans: Vec<DiffSpan>,
}

/// One half of a side-by-side row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffSideCell {
    pub number: Option<u32>,
    pub text: String,
    pub spans: Vec<DiffSpan>,
}

/// One side-by-side row: what the old side shows and what the new side shows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffSideRow {
    pub kind: DiffRowKind,
    /// Text for a row that spans both sides, such as the provenance or a hunk header.
    pub spanning: Option<String>,
    pub old: Option<DiffSideCell>,
    pub new: Option<DiffSideCell>,
}

/// A diff carried as structure: hunks of classified lines with both line numbers and the
/// spans that changed inside a line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiffPreview {
    pub hunks: Vec<DiffHunk>,
    /// The comparison this diff is relative to, e.g. `staged = HEAD -> index`.
    pub provenance: String,
    /// True when the patch was bounded and what is shown is a window onto a larger total.
    pub truncated: bool,
    /// The true number of changed lines, so a bounded view can state what it is not showing.
    pub total_lines: usize,
    pub layout: DiffLayout,
}

/// What a rendered diff row is, so painting and measurement read the same row set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffRowKind {
    Provenance,
    Collapsed,
    Header,
    Context,
    Added,
    Removed,
    Truncated,
}

impl DiffPreview {
    /// The widest line number, so both numbers align in their own columns.
    #[must_use]
    pub fn number_width(&self) -> usize {
        self.hunks
            .iter()
            .flat_map(|hunk| hunk.lines.iter())
            .flat_map(|line| [line.old_number, line.new_number])
            .flatten()
            .map(|number| number.to_string().len())
            .max()
            .unwrap_or(1)
    }

    /// The row that states how many unchanged lines a hunk skipped.
    #[must_use]
    pub fn collapsed_text(&self, count: usize) -> String {
        format!(
            "{:>number_width$}   {count} unchanged lines",
            "",
            number_width = self.number_width()
        )
    }

    /// The row that states a bounded patch is not the whole patch.
    #[must_use]
    pub fn truncated_text(&self) -> String {
        format!(
            "patch truncated; {} changed lines in total",
            self.total_lines
        )
    }

    /// The widest content this diff draws, so a surface sizes its pane from the same rows the
    /// painter uses rather than from a re-derivation that could drift.
    #[must_use]
    pub fn content_width(&self, ambiguous_width: crate::config::AmbiguousWidth) -> usize {
        let number_width = self.number_width();
        let measure = |text: &str| crate::ui::geometry::text_width(text, ambiguous_width);
        match self.layout {
            DiffLayout::Unified => self
                .iter_rows()
                .take(64)
                .map(|row| measure(&row.text))
                .max()
                .unwrap_or(28),
            DiffLayout::SideBySide => {
                let rows = self.side_by_side_rows();
                let spanning = rows
                    .iter()
                    .filter_map(|row| row.spanning.as_deref())
                    .take(64)
                    .map(measure)
                    .max()
                    .unwrap_or(0);
                let (left, right) = rows.iter().fold((0usize, 0usize), |(left, right), row| {
                    let half = |cell: &DiffSideCell| {
                        measure(&Self::side_cell_text_with_width(cell, number_width).0)
                    };
                    (
                        left.max(row.old.as_ref().map_or(0, half)),
                        right.max(row.new.as_ref().map_or(0, half)),
                    )
                });
                // The two halves and the divider between them share one row.
                spanning.max(left.saturating_add(right).saturating_add(1))
            }
        }
    }

    /// The text of one unified diff line, with the changed spans moved into its coordinates.
    #[must_use]
    pub fn line_row(&self, line: &DiffLine) -> (String, Vec<DiffSpan>) {
        Self::line_row_with_width(line, self.number_width())
    }

    fn line_row_with_width(line: &DiffLine, number_width: usize) -> (String, Vec<DiffSpan>) {
        let old = line.old_number.map_or_else(String::new, |n| n.to_string());
        let new = line.new_number.map_or_else(String::new, |n| n.to_string());
        let glyph = match line.kind {
            DiffLineKind::Added => '+',
            DiffLineKind::Removed => '-',
            DiffLineKind::Context => ' ',
        };
        let prefix = format!("{old:>number_width$} {new:>number_width$} {glyph} ");
        let offset = prefix.len();
        let spans = line
            .spans
            .iter()
            .map(|span| DiffSpan {
                start: span.start.saturating_add(offset),
                end: span.end.saturating_add(offset),
            })
            .collect();
        (format!("{prefix}{}", line.text), spans)
    }

    /// The text of one side-by-side half, with its number column and its changed spans in the
    /// returned text's coordinates.
    #[must_use]
    pub fn side_cell_text(&self, cell: &DiffSideCell) -> (String, Vec<DiffSpan>) {
        Self::side_cell_text_with_width(cell, self.number_width())
    }

    pub(crate) fn side_cell_text_with_width(
        cell: &DiffSideCell,
        width: usize,
    ) -> (String, Vec<DiffSpan>) {
        let number = cell.number.map_or_else(String::new, |n| n.to_string());
        let prefix = format!("{number:>width$} ", width = width);
        let offset = prefix.len();
        let spans = cell
            .spans
            .iter()
            .map(|span| DiffSpan {
                start: span.start.saturating_add(offset),
                end: span.end.saturating_add(offset),
            })
            .collect();
        (format!("{prefix}{}", cell.text), spans)
    }

    /// The rows that stay on top of the box while the rest scroll: the comparison's name.
    #[must_use]
    pub fn pinned_rows(&self) -> usize {
        usize::from(!self.provenance.is_empty())
    }

    /// The furthest a box of `height` rows can scroll.
    #[must_use]
    pub fn last_scroll(&self, height: usize) -> usize {
        let pinned = self.pinned_rows();
        let total = self
            .hunks
            .iter()
            .map(|hunk| hunk.lines.len() + 1 + usize::from(hunk.collapsed_before > 0))
            .sum::<usize>()
            + pinned
            + usize::from(self.truncated);
        total
            .saturating_sub(pinned)
            .saturating_sub(height.saturating_sub(pinned))
    }

    /// Where a box of `height` rows opens: at the top when the first change fits, otherwise
    /// on that change with one row above it.
    #[must_use]
    pub fn opening_scroll(&self, height: usize) -> usize {
        let pinned = self.pinned_rows();
        let first_change = self
            .iter_rows()
            .skip(pinned)
            .position(|row| matches!(row.kind, DiffRowKind::Added | DiffRowKind::Removed))
            .unwrap_or(0);
        if first_change < height.saturating_sub(pinned) {
            0
        } else {
            first_change.saturating_sub(1)
        }
    }

    /// Every row this diff draws in the unified layout, in order.
    ///
    /// Measurement and painting both read this, so a preview's width cannot be computed from
    /// one row set while a different one is drawn, and the spans a row carries are already in
    /// that row's own coordinates so the painter cannot shift them wrongly.
    #[must_use]
    pub fn rows(&self) -> Vec<DiffRow> {
        self.iter_rows().collect()
    }

    fn iter_rows(&self) -> impl Iterator<Item = DiffRow> + '_ {
        let number_width = self.number_width();
        let plain = |kind, text| DiffRow {
            kind,
            text,
            spans: Vec::new(),
        };
        let provenance = (!self.provenance.is_empty())
            .then(|| plain(DiffRowKind::Provenance, self.provenance.clone()));
        let hunks = self.hunks.iter().flat_map(move |hunk| {
            let collapsed = (hunk.collapsed_before > 0).then(|| {
                plain(
                    DiffRowKind::Collapsed,
                    format!(
                        "{:>number_width$}   {} unchanged lines",
                        "", hunk.collapsed_before
                    ),
                )
            });
            collapsed
                .into_iter()
                .chain(std::iter::once(plain(
                    DiffRowKind::Header,
                    hunk.header.clone(),
                )))
                .chain(hunk.lines.iter().map(move |line| {
                    let kind = match line.kind {
                        DiffLineKind::Added => DiffRowKind::Added,
                        DiffLineKind::Removed => DiffRowKind::Removed,
                        DiffLineKind::Context => DiffRowKind::Context,
                    };
                    let (text, spans) = Self::line_row_with_width(line, number_width);
                    DiffRow { kind, text, spans }
                }))
        });
        provenance.into_iter().chain(hunks).chain(
            self.truncated
                .then(|| plain(DiffRowKind::Truncated, self.truncated_text())),
        )
    }

    /// Every row a side-by-side diff draws, paired line for line.
    ///
    /// The side-by-side layout reads this for both measurement and painting the same way the
    /// unified layout reads `rows`, so neither layout can be measured from one row set while
    /// a different one is drawn.
    #[must_use]
    pub fn side_by_side_rows(&self) -> Vec<DiffSideRow> {
        let number_width = self.number_width();
        let spanning = |kind: DiffRowKind, text: String| DiffSideRow {
            kind,
            spanning: Some(text),
            old: None,
            new: None,
        };
        let cell = |number: Option<u32>, line: &DiffLine| DiffSideCell {
            number,
            text: line.text.clone(),
            spans: line.spans.clone(),
        };
        let mut rows = Vec::new();
        if !self.provenance.is_empty() {
            rows.push(spanning(DiffRowKind::Provenance, self.provenance.clone()));
        }
        for hunk in &self.hunks {
            if hunk.collapsed_before > 0 {
                rows.push(spanning(
                    DiffRowKind::Collapsed,
                    format!(
                        "{:>number_width$}   {} unchanged lines",
                        "", hunk.collapsed_before
                    ),
                ));
            }
            rows.push(spanning(DiffRowKind::Header, hunk.header.clone()));
            let mut index = 0usize;
            while index < hunk.lines.len() {
                let line = &hunk.lines[index];
                match line.kind {
                    DiffLineKind::Context => {
                        rows.push(DiffSideRow {
                            kind: DiffRowKind::Context,
                            spanning: None,
                            old: Some(cell(line.old_number, line)),
                            new: Some(cell(line.new_number, line)),
                        });
                        index += 1;
                    }
                    DiffLineKind::Removed => {
                        let removed_start = index;
                        while index < hunk.lines.len()
                            && hunk.lines[index].kind == DiffLineKind::Removed
                        {
                            index += 1;
                        }
                        let added_start = index;
                        while index < hunk.lines.len()
                            && hunk.lines[index].kind == DiffLineKind::Added
                        {
                            index += 1;
                        }
                        let removed = &hunk.lines[removed_start..added_start];
                        let added = &hunk.lines[added_start..index];
                        // A change block pairs its removed and added lines so the two sides
                        // stay in step; an unpaired line leaves the other side blank.
                        for pair in 0..removed.len().max(added.len()) {
                            rows.push(DiffSideRow {
                                kind: DiffRowKind::Removed,
                                spanning: None,
                                old: removed.get(pair).map(|l| cell(l.old_number, l)),
                                new: added.get(pair).map(|l| cell(l.new_number, l)),
                            });
                        }
                    }
                    DiffLineKind::Added => {
                        rows.push(DiffSideRow {
                            kind: DiffRowKind::Added,
                            spanning: None,
                            old: None,
                            new: Some(cell(line.new_number, line)),
                        });
                        index += 1;
                    }
                }
            }
        }
        if self.truncated {
            rows.push(spanning(DiffRowKind::Truncated, self.truncated_text()));
        }
        rows
    }
}
