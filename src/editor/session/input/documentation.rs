use super::InputSession;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use crate::{
    config::DocumentationPopupConfig,
    editor::text::ByteRange,
    ui::{
        DocumentationLineRole, DocumentationMarkupKind, DocumentationPopupKind,
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
            let mut roles = Vec::new();
            let markdown = markup == DocumentationMarkupKind::Markdown;
            let source_rows = if markdown {
                markdown_rows(&normalized, text_width)
            } else {
                normalized
                    .split('\n')
                    .map(|line| (line.to_owned(), DocumentationLineRole::Text))
                    .collect()
            };
            for (line, role) in source_rows {
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
                // Code, tables and diffs keep their lines; prose fits the popup.
                let prose = matches!(
                    role,
                    DocumentationLineRole::Text | DocumentationLineRole::Heading
                ) && (markdown || kind != DocumentationPopupKind::Git);
                let rows = if prose {
                    wrap_prose(&sanitized[..end], text_width)
                } else {
                    vec![sanitized[..end].to_owned()]
                };
                for row in rows.into_iter().take(remaining_lines) {
                    lines.push(row);
                    roles.push(role);
                    remaining_lines = remaining_lines.saturating_sub(1);
                }
            }
            if !lines.iter().all(|line| line.trim().is_empty()) {
                sections.push(DocumentationSection {
                    heading: None,
                    markup,
                    lines,
                    roles,
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

/// Markdown's blocks as display rows: no fences or heading marks, lists with bullets and
/// tables in columns.
fn markdown_rows(source: &str, width: usize) -> Vec<(String, DocumentationLineRole)> {
    use DocumentationLineRole::{Code, Heading, Table, Text};
    let mut rows = Vec::new();
    let mut in_code = false;
    let mut lines = source.split('\n').peekable();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        let indent = &line[..line.len() - trimmed.len()];
        if trimmed.starts_with("```") {
            in_code = !in_code;
        } else if in_code {
            rows.push((line.to_owned(), Code));
        } else if trimmed.starts_with('|') {
            let mut table = vec![trimmed];
            while let Some(row) = lines.next_if(|row| row.trim_start().starts_with('|')) {
                table.push(row.trim_start());
            }
            rows.extend(table_rows(&table).into_iter().map(|row| (row, Table)));
        } else if let Some(heading) = heading_text(trimmed) {
            rows.push((plain_markdown(heading), Heading));
        } else if trimmed.len() >= 3
            && ["-", "*", "_"].iter().any(|mark| trimmed.replace(mark, "").is_empty())
        {
            rows.push(("─".repeat(width.clamp(1, 40)), Text));
        } else if let Some(item) = ["- ", "* ", "+ "].iter().find_map(|mark| trimmed.strip_prefix(mark)) {
            let item = match item.get(..4) {
                Some("[ ] ") => format!("☐ {}", &item[4..]),
                Some("[x] " | "[X] ") => format!("☑ {}", &item[4..]),
                _ => format!("• {item}"),
            };
            rows.push((format!("{indent}{}", plain_markdown(&item)), Text));
        } else if let Some(quote) = trimmed.strip_prefix('>') {
            rows.push((format!("{indent}│ {}", plain_markdown(quote.trim_start())), Text));
        } else if !trimmed.is_empty()
            && lines.next_if(|next| setext_underline(next.trim())).is_some()
        {
            rows.push((plain_markdown(trimmed), Heading));
        } else {
            rows.push((plain_markdown(line), Text));
        }
    }
    rows
}

/// The `===` or `---` line under a heading written the other way.
fn setext_underline(line: &str) -> bool {
    !line.is_empty() && (line.chars().all(|c| c == '=') || line.chars().all(|c| c == '-'))
}

/// The text of an ATX heading, `## Like this ##`.
fn heading_text(line: &str) -> Option<&str> {
    let text = line.trim_start_matches('#');
    let level = line.len() - text.len();
    ((1..=6).contains(&level) && text.starts_with(' '))
        .then(|| text.trim().trim_end_matches('#').trim_end())
}

/// A Markdown table with its columns lined up.
fn table_rows(table: &[&str]) -> Vec<String> {
    let parsed = table.iter().map(|row| table_cells(row)).collect::<Vec<_>>();
    let is_rule = |cells: &[String]| {
        cells.iter().all(|cell| !cell.is_empty() && cell.chars().all(|c| matches!(c, '-' | ':')))
    };
    let mut widths = Vec::new();
    for cells in parsed.iter().filter(|cells| !is_rule(cells)) {
        widths.resize(widths.len().max(cells.len()), 0);
        for (width, cell) in widths.iter_mut().zip(cells) {
            *width = cell.width().max(*width);
        }
    }
    parsed
        .iter()
        .map(|cells| {
            let rule = is_rule(cells);
            let columns = widths.iter().enumerate().map(|(index, width)| {
                if rule {
                    "─".repeat(*width)
                } else {
                    let cell = cells.get(index).map_or("", String::as_str);
                    format!("{cell}{}", " ".repeat(width.saturating_sub(cell.width())))
                }
            });
            let row = columns.collect::<Vec<_>>().join(if rule { "─┼─" } else { " │ " });
            row.trim_end().to_owned()
        })
        .collect()
}

/// One table row's cells. A pipe with a backslash before it is text.
fn table_cells(row: &str) -> Vec<String> {
    let row = row.trim().trim_start_matches('|');
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut characters = row.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\\' && characters.peek() == Some(&'|') {
            cell.push('\\');
            cell.push('|');
            characters.next();
        } else if character == '|' {
            cells.push(plain_markdown(cell.trim()));
            cell.clear();
        } else {
            cell.push(character);
        }
    }
    if !cell.trim().is_empty() {
        cells.push(plain_markdown(cell.trim()));
    }
    cells
}

/// Reduce one Markdown line to what reads well without a renderer.
fn plain_markdown(line: &str) -> String {
    let text = link_labels(&without_data_images(line))
        .replace("**", "")
        .replace("~~", "")
        .replace('`', "");
    let text = without_emphasis(&text);
    [
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
        ("&amp;", "&"),
    ]
    .iter()
    .fold(without_escapes(&text), |text, (from, to)| text.replace(from, to))
}

/// A backslash before punctuation is Markdown's escape: `project\.` reads `project.`.
fn without_escapes(line: &str) -> String {
    let mut kept = String::new();
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\\' || !characters.peek().is_some_and(char::is_ascii_punctuation) {
            kept.push(character);
        }
    }
    kept
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

/// `[label](target)` becomes `label`. The targets are long and can't be followed from here.
fn link_labels(line: &str) -> String {
    let mut kept = String::new();
    let mut rest = line;
    while let Some(middle) = rest.find("](") {
        let (Some(open), Some(close)) = (rest[..middle].rfind('['), rest[middle..].find(')')) else {
            break;
        };
        kept.push_str(rest[..open].strip_suffix('!').unwrap_or(&rest[..open]));
        kept.push_str(&rest[open + 1..middle]);
        rest = &rest[middle + close + 1..];
    }
    kept.push_str(rest);
    kept
}

/// Drop `_` and `*` pairs that mark emphasis, and leave those inside names such as
/// `snake_case` or in sums such as `2 * 3`.
fn without_emphasis(line: &str) -> String {
    ['_', '*'].iter().fold(line.to_owned(), |text, mark| without_marks(&text, *mark))
}

fn without_marks(line: &str, mark: char) -> String {
    let characters = line.chars().collect::<Vec<_>>();
    let opens = |index: usize| {
        characters[index] == mark
            && index.checked_sub(1).is_none_or(|before| {
                !characters[before].is_alphanumeric() && characters[before] != '\\'
            })
            && characters.get(index + 1).is_some_and(|after| after.is_alphanumeric())
    };
    let closes = |index: usize| {
        characters[index] == mark
            && index
                .checked_sub(1)
                .is_some_and(|before| !matches!(characters[before], ' ' | '\\'))
            && characters.get(index + 1).is_none_or(|after| !after.is_alphanumeric())
    };
    let mut kept = String::new();
    let mut index = 0;
    while index < characters.len() {
        if opens(index)
            && let Some(end) = (index + 1..characters.len()).find(|end| closes(*end))
        {
            kept.extend(&characters[index + 1..end]);
            index = end + 1;
        } else {
            kept.push(characters[index]);
            index += 1;
        }
    }
    kept
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
    use super::{DocumentationLineRole as Role, markdown_rows, plain_markdown, wrap_prose};

    #[test]
    fn markdown_blocks_read_as_a_rendered_page() {
        let rows = markdown_rows(
            "# Title\n*em* and `code`\n- item\n> quote\n---\n```rust\nlet a = *b*;\n```\n\
             | Name | Age |\n|---|:-:|\n| Ada | 36 |\nOther\n=====\n- [x] ~~done~~\n",
            4,
        );
        let text = |text: &str, role| (text.to_owned(), role);
        assert_eq!(
            rows,
            [
                text("Title", Role::Heading),
                text("em and code", Role::Text),
                text("• item", Role::Text),
                text("│ quote", Role::Text),
                text("────", Role::Text),
                text("let a = *b*;", Role::Code),
                text("Name │ Age", Role::Table),
                text("─────┼────", Role::Table),
                text("Ada  │ 36", Role::Table),
                text("Other", Role::Heading),
                text("☑ done", Role::Text),
                text("", Role::Text),
            ]
        );
    }

    #[test]
    fn emphasis_marks_leave_sums_and_pointers() {
        assert_eq!(plain_markdown("2 * 3 * 4"), "2 * 3 * 4");
        assert_eq!(plain_markdown("*const T and *mut U"), "*const T and *mut U");
        assert_eq!(plain_markdown("![alt](pic.png) *note*"), "alt note");
    }

    #[test]
    fn prose_wraps_at_spaces_and_keeps_its_indent() {
        assert_eq!(wrap_prose("short", 10), ["short"]);
        assert_eq!(wrap_prose("a abcdefgh", 3), ["a", "abc", "def", "gh"]);
        assert_eq!(wrap_prose("one two three", 7), ["one two", "three"]);
        assert_eq!(wrap_prose("  one two", 5), ["  one", "  two"]);
        assert_eq!(wrap_prose("unbreakable x", 6), ["unbrea", "kable", "x"]);
    }

    #[test]
    fn markdown_lines_read_as_plain_text() {
        assert_eq!(
            plain_markdown("[MDN Reference](https://developer.mozilla.org/x) and **more**"),
            "MDN Reference and more"
        );
        assert_eq!(plain_markdown("Syntax: &lt;color&gt; &amp; more"), "Syntax: <color> & more");
        assert_eq!(plain_markdown("_Widely available_ across browsers"), "Widely available across browsers");
        assert_eq!(plain_markdown("keeps snake_case_name and a_b"), "keeps snake_case_name and a_b");
        assert_eq!(plain_markdown("![badge](data:image/svg+xml;base64,AA==) text"), " text");
        assert_eq!(plain_markdown(r"escaped \_name\_"), "escaped _name_");
        assert_eq!(plain_markdown(r"the project\."), "the project.");
        assert_eq!(plain_markdown(r"C:\path stays"), r"C:\path stays");
    }
}
