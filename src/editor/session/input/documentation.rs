use super::InputSession;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use crate::{
    config::DocumentationPopupConfig,
    editor::text::ByteRange,
    ui::{
        DocumentationMarkupKind, DocumentationPopupKind,
        DocumentationPopupState, DocumentationSection,
    },
};

/// Hover and completion documentation is cut to these bounds before it is laid out.
pub(crate) const DOCUMENTATION_MAXIMUM_SECTIONS: usize = 16;
pub(crate) const DOCUMENTATION_MAXIMUM_LINES: usize = 512;
pub(crate) const DOCUMENTATION_MAXIMUM_BYTES: usize = 64 * 1024;

impl InputSession {
    pub(in crate::editor::session) fn documentation(&self) -> Option<&DocumentationPopupState> {
        self.documentation.as_ref()
    }
    pub(in crate::editor::session) fn close_documentation(&mut self) {
        if self.documentation.is_some() {
            self.capture_changed();
        }
        self.documentation = None;
    }
    pub(in crate::editor::session) fn scroll_documentation(
        &mut self,
        direction: isize,
        step: usize,
        visible_rows: usize,
    ) {
        if let Some(popup) = &mut self.documentation {
            let maximum = popup.line_count().saturating_sub(visible_rows.max(1));
            popup.scroll = if direction < 0 {
                popup.scroll.saturating_sub(step)
            } else {
                popup.scroll.saturating_add(step).min(maximum)
            };
        }
    }
    pub(in crate::editor::session) fn present_documentation(
        &mut self,
        kind: DocumentationPopupKind,
        title: String,
        provider: String,
        source_range: Option<ByteRange>,
        source_sections: impl IntoIterator<Item = (DocumentationMarkupKind, String)>,
        policy: &DocumentationPopupConfig,
        text_width: usize,
    ) {
        self.capture_changed();
        if !policy.enabled {
            self.documentation = None;
            return;
        }
        let mut remaining_lines = DOCUMENTATION_MAXIMUM_LINES;
        let mut remaining_bytes = DOCUMENTATION_MAXIMUM_BYTES;
        let mut sections = Vec::new();
        for (markup, value) in source_sections.into_iter().take(DOCUMENTATION_MAXIMUM_SECTIONS) {
            if remaining_lines == 0 || remaining_bytes == 0 {
                break;
            }
            let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
            let mut lines = Vec::new();
            let mut spans = Vec::new();
            let markdown = markup == DocumentationMarkupKind::Markdown;
            let source_rows = if markdown {
                markdown_rows(&normalized, text_width)
            } else {
                normalized
                    .split('\n')
                    .map(|line| (line.to_owned(), Vec::new()))
                    .collect()
            };
            for (line, line_spans) in source_rows {
                if remaining_lines == 0 || remaining_bytes == 0 {
                    break;
                }
                let sanitized = line
                    .chars()
                    .filter(|character| !character.is_control() || *character == '\t')
                    .take(8192)
                    .collect::<String>();
                let take = sanitized.len().min(remaining_bytes);
                let mut end = take;
                while !sanitized.is_char_boundary(end) {
                    end = end.saturating_sub(1);
                }
                remaining_bytes = remaining_bytes.saturating_sub(end);
                // Markdown is laid out already and diffs keep their lines; plain prose fits the
                // popup.
                let rows = if !markdown && kind != DocumentationPopupKind::Git {
                    wrap_prose(&sanitized[..end], text_width)
                } else {
                    vec![sanitized[..end].to_owned()]
                };
                let line_spans = line_spans
                    .into_iter()
                    .filter(|(range, _)| range.end <= end)
                    .collect::<Vec<_>>();
                for row in rows.into_iter().take(remaining_lines) {
                    lines.push(row);
                    spans.push(line_spans.clone());
                    remaining_lines = remaining_lines.saturating_sub(1);
                }
            }
            if !lines.iter().all(|line| line.trim().is_empty()) {
                sections.push(DocumentationSection {
                    heading: None,
                    markup,
                    lines,
                    spans,
                });
            }
        }
        if sections.is_empty() {
            self.documentation = None;
            return;
        }
        self.documentation = Some(DocumentationPopupState {
            kind,
            title,
            provider: provider.chars().take(256).collect(),
            source_range,
            sections,
            scroll: 0,
        });
    }
}

/// A display row and its styled runs, as byte ranges in the row.
type StyledRow = (String, Vec<(std::ops::Range<usize>, crate::language::syntax::Highlight)>);

/// Markdown rendered as the agent transcript is, one row per line. Control characters and the
/// base64 badge images language servers put in hovers are dropped first.
fn markdown_rows(source: &str, width: usize) -> Vec<StyledRow> {
    let clean = source
        .split('\n')
        .map(without_data_images)
        .collect::<Vec<_>>()
        .join("\n")
        .replace(|character: char| character.is_control() && !matches!(character, '\n' | '\t'), "");
    let mut styled = crate::ui::markdown::Styled::default();
    crate::ui::markdown::render_compact(&mut styled, &clean, width);
    let mut rows = Vec::new();
    let mut start = 0;
    for line in styled.text.split('\n') {
        let range = start..start + line.len();
        let spans = styled
            .spans
            .iter()
            .filter(|(span, _)| span.start < range.end && span.end > range.start)
            .map(|(span, role)| {
                (span.start.max(range.start) - start..span.end.min(range.end) - start, *role)
            })
            .collect();
        rows.push((line.to_owned(), spans));
        start = range.end + 1;
    }
    // The renderer ends every block with a newline.
    if rows.last().is_some_and(|(line, _): &StyledRow| line.is_empty()) {
        rows.pop();
    }
    rows
}

/// Breaks prose at spaces to fit `width`, keeping its indent. A longer word stays whole.
fn wrap_prose(line: &str, width: usize) -> Vec<String> {
    if line.width() <= width {
        return vec![line.to_owned()];
    }
    let indent = &line[..line.len() - line.trim_start().len()];
    let mut lines = Vec::new();
    let mut current = String::from(indent);
    for word in line.split_whitespace() {
        if current.len() > indent.len() && current.width() + 1 + word.width() > width {
            lines.push(std::mem::replace(&mut current, String::from(indent)));
        }
        if current.len() > indent.len() {
            current.push(' ');
        }
        // A word wider than the popup, such as a long path, breaks at the edge.
        for character in word.chars() {
            if current.len() > indent.len()
                && current.width() + character.width().unwrap_or(0) > width
            {
                lines.push(std::mem::replace(&mut current, String::from(indent)));
            }
            current.push(character);
        }
    }
    lines.push(current);
    lines
}

/// Language servers put badges in hover text as base64 images, which only fill a terminal with
/// noise.
fn without_data_images(line: &str) -> String {
    let mut kept = String::new();
    let mut rest = line;
    while let Some(link) = rest.find("](data:") {
        let (Some(open), Some(close)) = (rest[..link].rfind("!["), rest[link..].find(')')) else {
            break;
        };
        kept.push_str(&rest[..open]);
        rest = &rest[link + close + 1..];
    }
    kept.push_str(rest);
    kept
}

#[cfg(test)]
mod tests {
    use super::{markdown_rows, wrap_prose};
    use crate::language::syntax::Highlight;

    #[test]
    fn markdown_reads_as_a_rendered_page_with_styles() {
        let rows = markdown_rows(
            "# Title\n*em* and `code` &lt;T&gt; ~~gone~~\n- [x] done\n\
             ![badge](data:image/svg+xml;base64,AA==) 2 * 3\nOther\n=====",
            40,
        );
        let text = rows.iter().map(|(line, _)| line.as_str()).collect::<Vec<_>>();
        assert_eq!(text, ["Title", "em and code <T> gone", "✓ done", " 2 * 3", "Other"]);
        assert_eq!(rows[4].1, [(0..5, Highlight::HEADING)]);
        assert_eq!(rows[0].1, [(0..5, Highlight::HEADING)]);
        assert_eq!(
            rows[1].1,
            [(0..2, Highlight::ITALIC), (7..11, Highlight::RAW), (16..20, Highlight::STRIKETHROUGH)]
        );
    }

    #[test]
    fn prose_wraps_at_spaces_and_keeps_its_indent() {
        assert_eq!(wrap_prose("short", 10), ["short"]);
        assert_eq!(wrap_prose("a abcdefgh", 3), ["a", "abc", "def", "gh"]);
        assert_eq!(wrap_prose("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap_prose("  one two", 5), ["  one", "  two"]);
        assert_eq!(wrap_prose("unbreakable x", 6), ["unbrea", "kable", "x"]);
    }
}
