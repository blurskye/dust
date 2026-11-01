use super::InputSession;
use crate::{
    config::DocumentationPopupConfig,
    editor::text::ByteRange,
    ui::{
        DocumentationMarkupKind, DocumentationPopupKind, DocumentationPopupState,
        DocumentationSection,
    },
};

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
    ) {
        self.capture_changed();
        if !policy.enabled {
            self.documentation = None;
            return;
        }
        let mut remaining_lines = policy.maximum_lines;
        let mut remaining_bytes = policy.maximum_bytes;
        let mut sections = Vec::new();
        for (markup, value) in source_sections.into_iter().take(policy.maximum_sections) {
            if remaining_lines == 0 || remaining_bytes == 0 {
                break;
            }
            let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
            let mut lines = Vec::new();
            for line in normalized.split('\n') {
                if remaining_lines == 0 || remaining_bytes == 0 {
                    break;
                }
                // The popup has no Markdown renderer: code fences and bold markers are noise.
                let bare;
                let line = if markup == DocumentationMarkupKind::Markdown {
                    if line.trim_start().starts_with("```") {
                        continue;
                    }
                    bare = plain_markdown(line);
                    bare.as_str()
                } else {
                    line
                };
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
                lines.push(sanitized[..end].to_owned());
                remaining_bytes = remaining_bytes.saturating_sub(end);
                remaining_lines = remaining_lines.saturating_sub(1);
            }
            if !lines.iter().all(|line| line.trim().is_empty()) {
                sections.push(DocumentationSection {
                    heading: None,
                    markup,
                    lines,
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

/// Reduce one Markdown line to what reads well without a renderer.
fn plain_markdown(line: &str) -> String {
    let text = link_labels(&without_data_images(line)).replace("**", "");
    let text = without_emphasis(&text);
    [
        ("\\_", "_"),
        ("\\*", "*"),
        ("\\<", "<"),
        ("\\>", ">"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&nbsp;", " "),
        ("&amp;", "&"),
    ]
    .iter()
    .fold(text, |text, (from, to)| text.replace(from, to))
}

/// `[label](target)` becomes `label`. The targets are long and can't be followed from here.
fn link_labels(line: &str) -> String {
    let mut kept = String::new();
    let mut rest = line;
    while let Some(middle) = rest.find("](") {
        let (Some(open), Some(close)) = (rest[..middle].rfind('['), rest[middle..].find(')')) else {
            break;
        };
        kept.push_str(&rest[..open]);
        kept.push_str(&rest[open + 1..middle]);
        rest = &rest[middle + close + 1..];
    }
    kept.push_str(rest);
    kept
}

/// Drop `_` pairs that mark emphasis, and leave those inside names such as `snake_case`.
fn without_emphasis(line: &str) -> String {
    let characters = line.chars().collect::<Vec<_>>();
    let opens = |index: usize| {
        characters[index] == '_'
            && index.checked_sub(1).is_none_or(|before| {
                !characters[before].is_alphanumeric() && characters[before] != '\\'
            })
            && characters.get(index + 1).is_some_and(|after| after.is_alphanumeric())
    };
    let closes = |index: usize| {
        characters[index] == '_'
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
    use super::plain_markdown;

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
    }
}
