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
