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
            style: if text.starts_with("commit ")
                || text.starts_with("diff --git ")
                || text.starts_with("@@")
            {
                Style::Accent
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

#[derive(Default)]
pub(super) struct Preview {
    pub rows: Vec<PreviewRow>,
    pub patch: bool,
}
impl From<&str> for Preview {
    fn from(text: &str) -> Self {
        Self {
            rows: text.lines().map(PreviewRow::message).collect(),
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
        let mut rows = vec![PreviewRow::message(&format!(
            "{} (untracked)",
            path.display()
        ))];
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
        Self { rows, patch: false }
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
            self.rows.extend(text.lines().map(PreviewRow::message));
            return;
        }
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
            self.rows.push(PreviewRow::message(line));
        }
        for row in &mut self.rows[first_row..] {
            row.path = path.clone();
        }
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
