//! Documentation popup content, scrolling and lifetime.

use crate::editor::text::ByteRange;
use crate::ui::DocumentationMarkupKind;
use crate::ui::DocumentationPopupKind;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// Hover and completion documentation is cut to these bounds before it is laid out.
pub(in crate::app) const DOCUMENTATION_MAXIMUM_SECTIONS: usize = 16;
pub(in crate::app) const DOCUMENTATION_MAXIMUM_LINES: usize = 512;
pub(in crate::app) const DOCUMENTATION_MAXIMUM_BYTES: usize = 64 * 1024;

/// A display row and its styled runs, as byte ranges in the row.
type StyledRow = (
    String,
    Vec<(std::ops::Range<usize>, crate::config::Highlight)>,
);

/// Markdown rendered as the agent transcript is, one row per line. Control characters and the
/// base64 badge images language servers put in hovers are dropped first.
pub(in crate::app) fn markdown_rows(
    source: &str,
    width: usize,
    ambiguous_width: crate::config::AmbiguousWidth,
) -> Vec<StyledRow> {
    let clean = source
        .split('\n')
        .map(without_data_images)
        .collect::<Vec<_>>()
        .join("\n")
        .replace(
            |character: char| character.is_control() && !matches!(character, '\n' | '\t'),
            "",
        );
    let mut styled = crate::ui::markdown::Styled {
        ambiguous_width,
        ..Default::default()
    };
    crate::ui::markdown::render_compact(&mut styled, &clean, width);
    let mut rows = Vec::new();
    let mut start = 0;
    let mut span_index = 0;
    for line in styled.text.split('\n').take(DOCUMENTATION_MAXIMUM_LINES) {
        let range = start..start + line.len();
        while styled
            .spans
            .get(span_index)
            .is_some_and(|(span, _)| span.end <= start)
        {
            span_index += 1;
        }
        let spans = styled.spans[span_index..]
            .iter()
            .take_while(|(span, _)| span.start < range.end)
            .map(|(span, role)| {
                (
                    span.start.max(range.start) - start..span.end.min(range.end) - start,
                    *role,
                )
            })
            .collect();
        rows.push((line.to_owned(), spans));
        start = range.end + 1;
    }
    // The renderer ends every block with a newline.
    if rows
        .last()
        .is_some_and(|(line, _): &StyledRow| line.is_empty())
    {
        rows.pop();
    }
    rows
}

/// Breaks prose at spaces to fit `width`, keeping its indent. A longer word stays whole.
pub(in crate::app) fn wrap_prose(line: &str, width: usize) -> Vec<String> {
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
    while let Some(open) = rest.find("![") {
        kept.push_str(&rest[..open]);
        rest = &rest[open..];
        if let Some(label_end) = rest.find(']')
            && rest[label_end..].starts_with("](data:")
            && let Some(close) = rest[label_end..].find(')')
        {
            rest = &rest[label_end + close + 1..];
        } else {
            kept.push_str("![");
            rest = &rest[2..];
        }
    }
    kept.push_str(rest);
    kept
}

use crate::app::App;
use crate::language::lsp::LspMarkupContent;
use crate::language::lsp::LspMarkupKind;

impl App {
    pub(in crate::app) fn present_documentation_popup(
        &mut self,
        kind: DocumentationPopupKind,
        title: String,
        provider: String,
        source_range: Option<ByteRange>,
        source_sections: Vec<LspMarkupContent>,
    ) {
        if kind != DocumentationPopupKind::Completion {
            self.reconcile_editor_windows();
            self.input.hold_focus(self.window_session.capture_focus());
        }
        let config = self.configuration.settings();
        let available = usize::from(self.window_session.width());
        let available = if kind == DocumentationPopupKind::Completion {
            available / 2
        } else {
            available
        };
        let text_width = available
            .min(usize::from(
                crate::ui::render::documentation::DOCUMENTATION_MAX_WIDTH,
            ))
            .saturating_sub(
                2 + 2 * usize::from(crate::ui::render::documentation::DOCUMENTATION_PADDING),
            );
        self.input.present_documentation(
            kind,
            title,
            provider,
            source_range,
            source_sections.into_iter().map(|source| {
                (
                    match source.kind {
                        LspMarkupKind::PlainText => DocumentationMarkupKind::PlainText,
                        LspMarkupKind::Markdown => DocumentationMarkupKind::Markdown,
                    },
                    source.value,
                )
            }),
            config,
            text_width,
        );
        self.restore_input_focus();
        self.project_input_documentation();
    }
}

impl App {
    pub(in crate::app) fn hover_is_showing(&self) -> bool {
        self.input
            .documentation()
            .is_some_and(|popup| popup.kind == DocumentationPopupKind::Hover)
    }
}

impl App {
    pub(in crate::app) fn project_input_documentation(&mut self) {
        let popup = self.input.documentation().cloned();
        self.ui.set(popup, |ui| &mut ui.documentation_popup);
        if self.ui.documentation_popup.is_some() {
            let (_, geometry) = crate::ui::render::render_with_documentation_geometry(
                &self.state,
                &self.ui,
                self.configuration.settings(),
                self.configuration.dashboard(),
                self.window_session.width(),
                self.window_session.height(),
            );
            if let Some((visible, total)) = geometry {
                self.input.scroll_documentation(0, 0, visible, total);
                let popup = self.input.documentation().cloned();
                self.ui.set(popup, |ui| &mut ui.documentation_popup);
            }
        }
    }
}

impl App {
    pub(in crate::app) fn close_documentation(&mut self) {
        self.input.close_documentation();
        self.restore_input_focus();
        self.project_input_documentation();
    }
}

use crate::app::commands::AppEffect;

impl App {
    pub(in crate::app) fn process_documentation_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::LspDocumentationScroll(direction) => {
                self.reconcile_editor_windows();
                // Use the same final layout as painting, including a narrowed completion box.
                let (_, geometry) = crate::ui::render::render_with_documentation_geometry(
                    &self.state,
                    &self.ui,
                    self.configuration.settings(),
                    self.configuration.dashboard(),
                    self.window_session.width(),
                    self.window_session.height(),
                );
                if let Some((visible, total)) = geometry {
                    self.input.scroll_documentation(
                        direction,
                        crate::ui::render::documentation::DOCUMENTATION_SCROLL_STEP,
                        visible,
                        total,
                    );
                }
                self.project_input_documentation();
            }
            AppEffect::LspDocumentationClose => self.close_documentation(),
            effect => return Err(effect),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::documentation::markdown_rows;
    use crate::app::documentation::wrap_prose;
    use crate::config::Highlight;

    #[test]
    fn markdown_reads_as_a_rendered_page_with_styles() {
        let rows = markdown_rows(
            "# Title\n*em* and `code` &lt;T&gt; ~~gone~~\n- [x] done\n\
             ![badge](data:image/svg+xml;base64,AA==) 2 * 3\nOther\n=====",
            40,
            crate::config::AmbiguousWidth::Narrow,
        );
        let text = rows
            .iter()
            .map(|(line, _)| line.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            text,
            ["Title", "em and code <T> gone", "✓ done", " 2 * 3", "Other"]
        );
        assert_eq!(rows[4].1, [(0..5, Highlight::HEADING)]);
        assert_eq!(rows[0].1, [(0..5, Highlight::HEADING)]);
        assert_eq!(
            rows[1].1,
            [
                (0..2, Highlight::ITALIC),
                (7..11, Highlight::RAW),
                (16..20, Highlight::STRIKETHROUGH)
            ]
        );
    }

    #[test]
    fn only_data_images_are_removed_without_consuming_other_links() {
        let line = "![logo](https://example.org/logo.png) Important text [download](data:text/plain;base64,AA==)";
        assert_eq!(crate::app::documentation::without_data_images(line), line);
        assert_eq!(
            crate::app::documentation::without_data_images(
                "before ![badge](data:image/png;base64,AA==) after"
            ),
            "before  after"
        );
        assert_eq!(
            crate::app::documentation::without_data_images(
                "[download](data:text/plain,AA) ![badge](data:image/png,AA)"
            ),
            "[download](data:text/plain,AA) "
        );
    }

    #[test]
    fn documentation_bounds_source_before_markdown_rendering() {
        let mut input = crate::app::input::InputState::new(100);
        let config = crate::config::load_bundled_config().config;
        // Rendering would remove these badges and otherwise reveal the text after the source limit.
        let source = format!(
            "{}outside byte limit",
            "![badge](data:image/png;base64,AA==)".repeat(3000)
        );
        input.present_documentation(
            crate::ui::DocumentationPopupKind::Hover,
            "hover".into(),
            String::new(),
            None,
            [(crate::ui::DocumentationMarkupKind::Markdown, source)],
            &config,
            40,
        );
        assert!(input.documentation().is_none_or(|popup| {
            !popup.sections[0]
                .lines
                .concat()
                .contains("outside byte limit")
        }));
        let source = format!(
            "{}outside line limit",
            "![badge](data:image/png,AA)\n"
                .repeat(crate::app::documentation::DOCUMENTATION_MAXIMUM_LINES)
        );
        input.present_documentation(
            crate::ui::DocumentationPopupKind::Hover,
            "hover".into(),
            String::new(),
            None,
            [(crate::ui::DocumentationMarkupKind::Markdown, source)],
            &config,
            40,
        );
        assert!(input.documentation().is_none());
    }

    #[test]
    fn git_wrapping_keeps_graphemes_whole_and_uses_configured_width() {
        let mut input = crate::app::input::InputState::new(100);
        let mut config = crate::config::load_bundled_config().config;
        let text = format!("+{}👩‍💻", "x".repeat(65));
        input.present_documentation(
            crate::ui::DocumentationPopupKind::Git,
            "hunk".into(),
            String::new(),
            None,
            [(crate::ui::DocumentationMarkupKind::PlainText, text.clone())],
            &config,
            68,
        );
        assert_eq!(input.documentation().unwrap().sections[0].lines, [text]);
        config.editor.ambiguous_width = crate::config::AmbiguousWidth::Wide;
        input.present_documentation(
            crate::ui::DocumentationPopupKind::Git,
            "hunk".into(),
            String::new(),
            None,
            [(crate::ui::DocumentationMarkupKind::PlainText, "a·b".into())],
            &config,
            3,
        );
        assert_eq!(
            input.documentation().unwrap().sections[0].lines,
            ["a·", "b"]
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
    #[test]
    fn long_git_lines_store_scrollable_wrapped_rows() {
        let mut input = crate::app::input::InputState::new(100);
        let text = format!("+{}", "x".repeat(1000));
        input.present_documentation(
            crate::ui::DocumentationPopupKind::Git,
            "hunk".into(),
            String::new(),
            None,
            [(crate::ui::DocumentationMarkupKind::PlainText, text.clone())],
            &crate::config::load_bundled_config().config,
            40,
        );
        let popup = input.documentation().unwrap();
        assert!(popup.line_count() > 20);
        assert_eq!(popup.sections[0].lines.concat(), text);
        input.scroll_documentation(1, 10, 10, input.documentation().unwrap().line_count());
        assert_eq!(input.documentation().unwrap().scroll, 10);
        input.scroll_documentation(-1, 1, 10, 12);
        assert_eq!(input.documentation().unwrap().scroll, 1);
        input.scroll_documentation(1, 10, 10, 30);
        input.scroll_documentation(0, 0, 10, 5);
        assert_eq!(input.documentation().unwrap().scroll, 0);
    }
}
