//! Prepared source and patch rows. Parsing and highlighting happen when content arrives,
//! not while the terminal paints or the user scrolls.
use crate::config::Highlight;
use crate::syntax::language_for_path;
use crate::ui::{DiffLineKind, PickerTextPreview, Style, parse_unified_patch};
use std::ops::Range;
use std::path::{Path, PathBuf};

pub(super) struct PreviewRow {
    pub text: String,
    pub style: Style,
    pub spans: Vec<(Range<usize>, Highlight)>,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub marker: Option<char>,
    pub path: Option<PathBuf>,
}
impl PreviewRow {
    fn message(text: &str) -> Self {
        Self {
            text: text.into(),
            style: if text.starts_with("commit ") || text.starts_with("diff --git ") {
                Style::Accent
            } else if text.starts_with("@@") {
                // Only the native cursor's containing hunk gets an active band.
                Style::Muted
            } else if text.starts_with("index ")
                || text.starts_with("--- ")
                || text.starts_with("+++ ")
            {
                Style::Muted
            } else {
                Style::Default
            },
            spans: Vec::new(),
            old: None,
            new: None,
            marker: None,
            path: None,
        }
    }
}

pub(super) struct SplitRow {
    pub heading: Option<usize>,
    pub old: Option<usize>,
    pub new: Option<usize>,
}
#[derive(Default)]
pub(super) struct Preview {
    pub rows: Vec<PreviewRow>,
    pub split: Vec<SplitRow>,
    pub patch: bool,
    pub search: super::search::SourceSearch,
    /// A byte within the real code cursor's row; independent of search feedback.
    pub cursor_byte: Option<(usize, usize)>,
}
impl From<&str> for Preview {
    fn from(text: &str) -> Self {
        Self {
            rows: text.lines().map(PreviewRow::message).collect(),
            split: Vec::new(),
            patch: false,
            search: super::search::SourceSearch::default(),
            cursor_byte: None,
        }
    }
}
impl From<String> for Preview {
    fn from(text: String) -> Self {
        Self::from(text.as_str())
    }
}
impl Preview {
    pub fn same_content(&self, other: &Self) -> bool {
        self.patch == other.patch
            && self.rows.len() == other.rows.len()
            && self.rows.iter().zip(&other.rows).all(|(left, right)| {
                left.path == right.path
                    && left.old == right.old
                    && left.new == right.new
                    && left.marker == right.marker
                    && left.text == right.text
            })
    }

    pub fn source(path: &Path, source: &str) -> Self {
        let highlighted = PickerTextPreview::new(
            source.lines().map(Into::into).collect(),
            0,
            None,
            language_for_path(path),
        );
        let mut rows = vec![PreviewRow::message(&path.display().to_string())];
        rows[0].style = Style::GitHeading;
        rows.extend(
            highlighted
                .lines()
                .iter()
                .enumerate()
                .map(|(i, line)| PreviewRow {
                    text: line.clone(),
                    style: Style::Default,
                    spans: highlighted.line_spans(i).to_vec(),
                    old: None,
                    new: Some(i as u32 + 1),
                    marker: None,
                    path: Some(path.to_path_buf()),
                }),
        );
        Self {
            rows,
            split: Vec::new(),
            patch: false,
            search: super::search::SourceSearch::default(),
            cursor_byte: None,
        }
    }

    pub fn git(text: &str, fallback_path: Option<&Path>) -> Self {
        let mut result = Self::default();
        let mut section = String::new();
        for line in text.lines() {
            if line.starts_with("diff --git ") && !section.is_empty() {
                result.append_patch(&section, fallback_path);
                section.clear();
            }
            section.push_str(line);
            section.push('\n');
        }
        result.append_patch(&section, fallback_path);
        result
    }

    fn append_patch(&mut self, text: &str, fallback_path: Option<&Path>) {
        let diff = parse_unified_patch(text, "");
        if diff.hunks.is_empty() {
            let first = self.rows.len();
            self.rows.extend(text.lines().map(PreviewRow::message));
            self.split
                .extend((first..self.rows.len()).map(|index| SplitRow {
                    heading: Some(index),
                    old: None,
                    new: None,
                }));
            return;
        }
        let side_rows = diff.side_by_side_rows();
        let first_row = self.rows.len();
        let mut path = fallback_path.map(Path::to_path_buf);
        for line in text.lines().take_while(|line| !line.starts_with("@@")) {
            if let Some(header) = line
                .strip_prefix("--- ")
                .or_else(|| line.strip_prefix("+++ "))
                && let Some(file) = header_path(header)
            {
                path = Some(file);
            }
            if !["diff --git ", "index ", "--- ", "+++ "]
                .iter()
                .any(|prefix| line.starts_with(prefix))
            {
                self.rows.push(PreviewRow::message(line));
            }
        }
        let added = diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Added)
            .count();
        let removed = diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| line.kind == DiffLineKind::Removed)
            .count();
        let mut heading = PreviewRow::message(&format!(
            "{}   +{added} −{removed}",
            path.as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "Changes".into())
        ));
        heading.style = Style::GitHeading;
        self.rows.push(heading);
        for row in &mut self.rows[first_row..] {
            row.path = path.clone();
        }
        self.split
            .extend((first_row..self.rows.len()).map(|index| SplitRow {
                heading: Some(index),
                old: None,
                new: None,
            }));
        let language = path.as_deref().and_then(language_for_path);
        for hunk in diff.hunks {
            self.patch = true;
            let mut header = PreviewRow::message(&hunk.header);
            header.path = path.clone();
            self.rows.push(header);
            let old = PickerTextPreview::new(
                hunk.lines
                    .iter()
                    .filter(|line| line.kind != DiffLineKind::Added)
                    .map(|line| line.text.clone())
                    .collect(),
                0,
                None,
                language,
            );
            let new = PickerTextPreview::new(
                hunk.lines
                    .iter()
                    .filter(|line| line.kind != DiffLineKind::Removed)
                    .map(|line| line.text.clone())
                    .collect(),
                0,
                None,
                language,
            );
            let (mut old_at, mut new_at) = (0, 0);
            for line in hunk.lines {
                let (style, marker, spans) = match line.kind {
                    DiffLineKind::Added => (Style::GitSignAdded, '+', new.line_spans(new_at)),
                    DiffLineKind::Removed => (Style::GitSignDeleted, '-', old.line_spans(old_at)),
                    DiffLineKind::Context => (Style::Default, ' ', new.line_spans(new_at)),
                };
                self.rows.push(PreviewRow {
                    text: line.text,
                    style,
                    marker: Some(marker),
                    old: line.old_number,
                    new: line.new_number,
                    spans: spans.to_vec(),
                    path: path.clone(),
                });
                old_at += usize::from(line.kind != DiffLineKind::Added);
                new_at += usize::from(line.kind != DiffLineKind::Removed);
            }
        }
        let old = self.rows[first_row..]
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.old.map(|number| (number, first_row + index)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let new = self.rows[first_row..]
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.new.map(|number| (number, first_row + index)))
            .collect::<std::collections::BTreeMap<_, _>>();
        for row in side_rows {
            if let Some(text) = row.spanning {
                if let Some(index) = self.rows[first_row..]
                    .iter()
                    .position(|row| row.marker.is_none() && row.text == text)
                {
                    self.split.push(SplitRow {
                        heading: Some(first_row + index),
                        old: None,
                        new: None,
                    });
                }
            } else {
                self.split.push(SplitRow {
                    heading: None,
                    old: row
                        .old
                        .and_then(|cell| cell.number)
                        .and_then(|number| old.get(&number).copied()),
                    new: row
                        .new
                        .and_then(|cell| cell.number)
                        .and_then(|number| new.get(&number).copied()),
                });
            }
        }
    }

    /// Contiguous parsed hunk containing a real code row, including its header.
    /// Never cross a metadata/file boundary or borrow the previous file's hunk.
    pub fn hunk_rows(&self, cursor: usize) -> Option<Range<usize>> {
        let row = self.rows.get(cursor)?;
        row.marker?;
        let header = (0..cursor).rev().find(|&index| {
            self.rows[index].marker.is_none() && self.rows[index].text.starts_with("@@")
        })?;
        if self.rows[header].path != row.path {
            return None;
        }
        let end = self.rows[header + 1..]
            .iter()
            .position(|row| row.marker.is_none())
            .map_or(self.rows.len(), |offset| header + 1 + offset);
        (cursor < end).then_some(header..end)
    }

    /// Resolve a rendered code row against the complete captured hunk. Matching a
    /// line number alone is unsafe when either Git read raced with an external edit.
    pub fn captured_line(
        &self,
        cursor: usize,
        snapshot: &super::GitHunkSnapshot,
    ) -> Option<(usize, usize)> {
        let row = self.rows.get(cursor)?;
        if row.path.as_deref() != Some(snapshot.file.as_path()) || row.marker.is_none() {
            return None;
        }
        let range = self.hunk_rows(cursor)?;
        let header = range.start;
        let displayed = &self.rows[header + 1..range.end];
        let hunk = snapshot.hunks.iter().position(|hunk| {
            hunk.header.split("@@").nth(1) == self.rows[header].text.split("@@").nth(1)
                && hunk.lines.len() == displayed.len()
                && hunk.lines.iter().zip(displayed).all(|(line, row)| {
                    let marker = match line.kind {
                        super::GitDiffLineKind::Context => ' ',
                        super::GitDiffLineKind::Add => '+',
                        super::GitDiffLineKind::Delete => '-',
                    };
                    row.old == line.old_line
                        && row.new == line.new_line
                        && row.marker == Some(marker)
                        && row.text == line.text.trim_end_matches(['\r', '\n'])
                })
        })?;
        Some((hunk, cursor.checked_sub(header + 1)?))
    }

    /// Where `e` opens the editor: the file of the row at `cursor` and its
    /// line in the working copy, 1-based. A deleted line opens at the line that took its place,
    /// a heading at the first line below it.
    pub fn location(&self, cursor: usize) -> Option<(PathBuf, u32)> {
        let row = self.rows.get(cursor)?;
        let path = row
            .path
            .clone()
            .or_else(|| self.rows[cursor..].iter().find_map(|row| row.path.clone()))?;
        let same = |row: &&PreviewRow| row.path.as_ref() == Some(&path);
        let line = row
            .new
            .or_else(|| {
                self.rows[cursor..]
                    .iter()
                    .filter(same)
                    .find_map(|row| row.new)
            })
            .or_else(|| {
                self.rows[..cursor]
                    .iter()
                    .rev()
                    .filter(same)
                    .find_map(|row| row.new)
            })
            .unwrap_or(1);
        Some((path, line))
    }

    pub fn cursor_rows(&self, split: bool) -> Vec<usize> {
        let mut rows = if split {
            self.split
                .iter()
                .flat_map(|row| [row.old, row.new.filter(|new| Some(*new) != row.old)])
                .flatten()
                .collect::<Vec<_>>()
        } else {
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.old.is_some() || row.new.is_some())
                .map(|(index, _)| index)
                .collect()
        };
        if rows.is_empty() {
            rows = self
                .rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.style != Style::GitHeading)
                .map(|(index, _)| index)
                .collect();
        }
        rows
    }

    pub fn display_position(
        &self,
        index: usize,
        split: bool,
        pinned: Option<usize>,
    ) -> Option<usize> {
        if split {
            self.split
                .iter()
                .filter(|row| pinned.is_none() || row.heading != pinned)
                .position(|row| {
                    row.heading == Some(index) || row.old == Some(index) || row.new == Some(index)
                })
        } else {
            (0..self.rows.len())
                .filter(|row| Some(*row) != pinned)
                .position(|row| row == index)
        }
    }

    pub fn number_width(&self) -> usize {
        self.rows
            .iter()
            .flat_map(|line| [line.old, line.new])
            .flatten()
            .max()
            .unwrap_or(1)
            .to_string()
            .len()
    }
}

impl super::GitScreen {
    /// Keep both the real cursor and its byte-level search target on an unchanged
    /// refresh. Any changed source owns a fresh search, with no stale row indices.
    pub(super) fn update_preview(&mut self, mut preview: Preview) {
        if self.preview.same_content(&preview) {
            preview.search = std::mem::take(&mut self.preview.search);
            preview.cursor_byte = self.preview.cursor_byte;
        } else {
            self.preview_cursor = None;
            if self.preview.search.active() {
                self.preview_column = 0;
            }
        }
        self.preview = preview;
    }

    pub(super) fn pinned_preview_heading(&self) -> Option<usize> {
        (self.commit.is_some() || self.last_list == 0)
            .then(|| {
                self.preview
                    .rows
                    .iter()
                    .position(|row| row.style == Style::GitHeading)
            })
            .flatten()
    }

    pub(super) fn normalize_preview_cursor(&mut self) {
        let rows = self.preview.cursor_rows(self.preview_split);
        if self
            .preview_cursor
            .is_none_or(|cursor| !rows.contains(&cursor))
        {
            self.preview_cursor = rows.first().copied();
        }
    }

    pub(super) fn move_preview(&mut self, delta: isize) {
        self.preview.search.leave_match();
        self.preview.cursor_byte = None;
        let rows = self.preview.cursor_rows(self.preview_split);
        let Some(first) = rows.first().copied() else {
            return;
        };
        let position = self
            .preview_cursor
            .and_then(|cursor| rows.iter().position(|&row| row == cursor))
            .unwrap_or(0);
        let index = rows
            .get(position.saturating_add_signed(delta).min(rows.len() - 1))
            .copied()
            .unwrap_or(first);
        self.preview_cursor = Some(index);
        // Scroll on the very first motion, as before. Once the viewport reaches the
        // end, the real cursor can still reach every remaining code line.
        self.preview_offset = self
            .preview
            .display_position(index, self.preview_split, self.pinned_preview_heading())
            .unwrap_or(0)
            .min(self.preview_rows.saturating_sub(self.preview_visible));
    }

    pub(super) fn preview_edge(&mut self, end: bool) {
        self.preview.search.leave_match();
        self.preview.cursor_byte = None;
        let rows = self.preview.cursor_rows(self.preview_split);
        self.preview_cursor = if end { rows.last() } else { rows.first() }.copied();
        self.preview_offset = if end {
            self.preview_rows.saturating_sub(self.preview_visible)
        } else {
            0
        };
    }
}

fn header_path(header: &str) -> Option<PathBuf> {
    let decoded = if header.starts_with('"') {
        serde_json::from_str::<String>(header).ok()?
    } else {
        header.to_owned()
    };
    decoded
        .strip_prefix("a/")
        .or_else(|| decoded.strip_prefix("b/"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containing_hunk_range_includes_header_but_never_another_hunk_or_file() {
        let preview = Preview::git(
            concat!(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n",
                "@@ -1,2 +1,2 @@\n-old_first\n+new_first\n context\n",
                "@@ -20,2 +20,2 @@\n-old_second\n+new_second\n context\n",
                "diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n",
                "@@ -1 +1 @@\n-old_other\n+new_other\n",
            ),
            None,
        );
        let headers = preview
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.text.starts_with("@@"))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        assert_eq!(headers.len(), 3);
        for (index, row) in preview.rows.iter().enumerate() {
            if row.marker.is_none() {
                assert_eq!(preview.hunk_rows(index), None);
            } else {
                let range = preview.hunk_rows(index).unwrap();
                assert!(range.contains(&index));
                assert!(preview.rows[range.start].text.starts_with("@@"));
                assert_eq!(preview.rows[range.start].path, row.path);
                assert!(
                    preview.rows[range.start + 1..range.end]
                        .iter()
                        .all(|part| part.marker.is_some() && part.path == row.path)
                );
                assert!(
                    range.end == preview.rows.len() || preview.rows[range.end].marker.is_none()
                );
            }
        }
        assert_eq!(
            preview.hunk_rows(headers[0] + 1),
            Some(headers[0]..headers[1])
        );
        assert_eq!(preview.hunk_rows(preview.rows.len()), None);
        let source = Preview::source(Path::new("a.rs"), "plain code\n");
        assert_eq!(source.hunk_rows(1), None);
    }

    #[test]
    fn e_opens_the_new_line_and_a_deleted_line_at_the_one_that_took_its_place() {
        let preview = Preview::git(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -4,3 +4,3 @@\n keep\n-old\n+new\n tail\n",
            None,
        );
        let at = |text: &str| {
            preview
                .rows
                .iter()
                .position(|row| row.text.ends_with(text))
                .unwrap()
        };
        let line = |index: usize| preview.location(index);
        let a = PathBuf::from("a.rs");
        assert_eq!(line(at("keep")), Some((a.clone(), 4)));
        assert_eq!(line(at("old")), Some((a.clone(), 5)));
        assert_eq!(line(at("new")), Some((a.clone(), 5)));
        assert_eq!(line(at("tail")), Some((a.clone(), 6)));
        // The file heading opens at the first line below it.
        assert_eq!(line(0), Some((a, 4)));
        let source = Preview::source(Path::new("b.rs"), "one\ntwo\n");
        assert_eq!(source.location(2), Some((PathBuf::from("b.rs"), 2)));
    }
}
