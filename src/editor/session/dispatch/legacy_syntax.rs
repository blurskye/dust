use super::*;

impl InteractiveApplication {
    pub(super) fn syntax_language_label(&self, language: SyntaxLanguage) -> String {
        self.message(
            match language {
                SyntaxLanguage::Rust => "syntax_language_rust",
                SyntaxLanguage::TypeScript => "syntax_language_typescript",
                SyntaxLanguage::Tsx => "syntax_language_tsx",
                SyntaxLanguage::JavaScript => "syntax_language_javascript",
                SyntaxLanguage::Go => "syntax_language_go",
                SyntaxLanguage::Json => "syntax_language_json",
                SyntaxLanguage::Toml => "syntax_language_toml",
                SyntaxLanguage::Markdown => "syntax_language_markdown",
            },
            &[],
        )
    }

    pub(super) fn syntax_support_label(&self, support: SyntaxSupport) -> String {
        self.message(
            match support {
                SyntaxSupport::Full => "syntax_support_full",
                SyntaxSupport::Partial => "syntax_support_partial",
                SyntaxSupport::Unsupported => "syntax_support_unsupported",
            },
            &[],
        )
    }

    pub(super) fn apply_syntax_indent_line(&mut self) {
        let Some(view_id) = self.state.active_view else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor = view.cursor;
        let Some(buffer) = self.state.buffers.get(buffer_id) else {
            return;
        };
        let Some(language) = buffer.path.as_deref().and_then(language_for_path) else {
            self.show_error(self.message("syntax_indent_unavailable", &[]));
            return;
        };
        let revision = buffer.text.revision();
        let line = match buffer.text.byte_to_line(cursor) {
            Ok(line) => line,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let levels = match self.syntax.indentation(buffer_id, revision, language, line) {
            Ok(levels) => levels,
            Err(error) => {
                let message = match error {
                    IndentError::Pending => self.message("syntax_indent_pending", &[]),
                    IndentError::Unsupported => self.message(
                        "syntax_indent_unsupported",
                        &[("language", self.syntax_language_label(language))],
                    ),
                    IndentError::Truncated => self.message("syntax_indent_truncated", &[]),
                };
                self.show_error(message);
                return;
            }
        };
        let desired = if self.state.policy.expand_tabs {
            " ".repeat(levels.saturating_mul(self.state.policy.indent_width))
        } else {
            "\t".repeat(levels)
        };
        let text = match buffer.text.line(line) {
            Ok(text) => text,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let whitespace = text
            .bytes()
            .take_while(|byte| matches!(*byte, b' ' | b'\t'))
            .count();
        if text.get(..whitespace) == Some(desired.as_str()) {
            self.notify_message(self.message("syntax_indent_unchanged", &[]), false);
            return;
        }
        let start = match buffer.text.line_to_byte(line) {
            Ok(start) => start,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        let end = ByteOffset(start.get().saturating_add(whitespace));
        let edits = TextEditSet::new(
            revision,
            vec![TextEdit::new(
                ByteRange::new(start, end).expect("ordered indentation"),
                desired,
            )],
        );
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_precondition(Precondition::BufferWritable(buffer_id))
            .with_precondition(Precondition::ViewDisplays {
                view: view_id,
                buffer: buffer_id,
            })
            .with_operation(EditorOperation::ApplyEdits {
                buffer: buffer_id,
                edits,
            });
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error)
        } else {
            self.notify_message(self.message("syntax_indent_applied", &[]), false);
        }
    }

    pub(super) fn process_syntax(&mut self) {
        let documents = self
            .state
            .buffers
            .iter()
            .map(|(buffer, state)| SyntaxDocument {
                buffer,
                text: &state.text,
                language: state.path.as_deref().and_then(language_for_path),
                enabled: !(self.effective_large_file(buffer)
                    && self
                        .configuration
                        .settings()
                        .editor
                        .large_file
                        .disable_syntax),
            })
            .collect::<Vec<_>>();
        let too_large = self.message("syntax_source_too_large", &[]);
        self.syntax.tick(
            &documents,
            &self.configuration.settings().syntax,
            self.clock.monotonic(),
            &too_large,
        );
        let collapsed = self
            .ui
            .syntax_folds
            .iter()
            .filter(|fold| fold.collapsed)
            .map(|fold| (fold.view, fold.start_line, fold.end_line))
            .collect::<HashSet<_>>();
        let mut syntax_folds = Vec::new();
        for (view_id, view) in self.state.views.iter() {
            if let Some(result) = self.syntax.result(view.buffer) {
                for fold in &result.folds {
                    syntax_folds.push(UiFold {
                        view: view_id,
                        start_line: fold.start_row,
                        end_line: fold.end_row,
                        collapsed: collapsed.contains(&(view_id, fold.start_row, fold.end_row)),
                    });
                }
            }
        }
        self.ui.syntax_folds = syntax_folds;
        self.ui.syntax_spans = self
            .syntax
            .results()
            .flat_map(|(buffer, result)| {
                result.spans.iter().filter_map(|span| {
                    Some(UiSyntaxSpan {
                        buffer: *buffer,
                        revision: BufferRevision(result.revision),
                        range: ByteRange::new(ByteOffset(span.start), ByteOffset(span.end)).ok()?,
                        role: span.role,
                    })
                })
            })
            .collect();
    }
}
