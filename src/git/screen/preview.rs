//! Prepared source and patch rows. Parsing and highlighting happen when content arrives,
//! not while the terminal paints or the user scrolls.
use crate::config::Highlight;
use crate::language::syntax::language_for_path;
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
            } else if text.starts_with("@@")
                || text.starts_with("index ")
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
}
impl From<&str> for Preview {
    fn from(text: &str) -> Self {
        Self {
            rows: text.lines().map(PreviewRow::message).collect(),
            split: Vec::new(),
            patch: false,
        }
    }
}
impl From<String> for Preview {
    fn from(text: String) -> Self {
        Self::from(text.as_str())
    }
}
impl Preview {
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
