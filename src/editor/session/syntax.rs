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
        let Some(result) = self.syntax_accepted.get(&buffer_id) else {
            self.show_error(self.message("syntax_indent_pending", &[]));
            return;
        };
        if result.language != language || result.revision != revision.0 {
            self.show_error(self.message("syntax_indent_pending", &[]));
            return;
        }
        if result.capabilities.indents == SyntaxSupport::Unsupported {
            self.show_error(self.message(
                "syntax_indent_unsupported",
                &[("language", self.syntax_language_label(language))],
            ));
            return;
        }
        if result.indents_truncated {
            self.show_error(self.message("syntax_indent_truncated", &[]));
            return;
        }
        let levels = result
            .indents
            .iter()
            .filter(|range| line > range.start_row && line < range.end_row)
            .count();
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
            self.ui.notification = Some(self.message("syntax_indent_unchanged", &[]));
            self.ui.notification_error = false;
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
            self.ui.notification = Some(self.message("syntax_indent_applied", &[]));
            self.ui.notification_error = false;
        }
    }

    pub(super) fn accepts_syntax_event(
        &self,
        buffer: crate::editor::BufferId,
        revision: u64,
        generation: u64,
        language: SyntaxLanguage,
    ) -> bool {
        self.syntax_submitted.get(&buffer)
            == Some(&(BufferRevision(revision), generation, language))
            && self.state.buffers.get(buffer).is_some_and(|state| {
                state.text.revision().0 == revision
                    && state.path.as_deref().and_then(language_for_path) == Some(language)
            })
    }

    pub(super) fn process_syntax(&mut self) {
        if !self.configuration.settings.syntax.enabled {
            for document in self.syntax_documents.values().copied() {
                self.syntax.cancel(document);
            }
            self.syntax_documents.clear();
            self.syntax_submitted.clear();
            self.syntax_accepted.clear();
            self.syntax_failures.clear();
            self.syntax_due.clear();
            self.ui.syntax_spans.clear();
            self.ui.syntax_folds.clear();
            return;
        }
        let live = self.state.buffers.keys().collect::<Vec<_>>();
        self.syntax_documents.retain(|buffer, document| {
            let keep = live.contains(buffer);
            if !keep {
                self.syntax.cancel(*document);
            }
            keep
        });
        self.syntax_submitted
            .retain(|buffer, _| live.contains(buffer));
        self.syntax_accepted.retain(|buffer, result| {
            live.contains(buffer)
                && self.state.buffers.get(*buffer).is_some_and(|state| {
                    state.path.as_deref().and_then(language_for_path) == Some(result.language)
                        && state.text.revision().0 == result.revision
                })
        });
        self.syntax_failures.retain(|buffer, failure| {
            live.contains(buffer)
                && self.state.buffers.get(*buffer).is_some_and(|state| {
                    state.text.revision() == failure.revision
                        && state.path.as_deref().and_then(language_for_path)
                            == Some(failure.language)
                })
                && self.syntax_submitted.get(buffer)
                    == Some(&(failure.revision, failure.generation, failure.language))
        });
        self.syntax_due.retain(|buffer, _| live.contains(buffer));
        for buffer in &live {
            if self
                .state
                .buffers
                .get(*buffer)
                .and_then(|state| state.path.as_deref())
                .and_then(language_for_path)
                .is_none()
            {
                if let Some(document) = self.syntax_documents.remove(buffer) {
                    self.syntax.cancel(document);
                }
                self.syntax_submitted.remove(buffer);
                self.syntax_accepted.remove(buffer);
                self.syntax_failures.remove(buffer);
                self.syntax_due.remove(buffer);
            }
        }
        let now = Instant::now();
        let candidates = self
            .state
            .buffers
            .iter()
            .filter_map(|(id, buffer)| {
                let path = buffer.path.as_ref()?;
                let language = language_for_path(path)?;
                let large = self.effective_large_file(id);
                Some((id, buffer.text.revision(), language, large))
            })
            .collect::<Vec<_>>();
        for (buffer, revision, language, large) in candidates {
            if self
                .syntax_submitted
                .get(&buffer)
                .is_some_and(|(submitted, _, previous)| {
                    *submitted != revision || *previous != language
                })
            {
                self.syntax_failures.remove(&buffer);
                self.syntax_accepted.remove(&buffer);
            }
            if large && self.configuration.settings.editor.large_file.disable_syntax {
                if let Some(document) = self.syntax_documents.remove(&buffer) {
                    self.syntax.cancel(document);
                }
                self.syntax_submitted.remove(&buffer);
                self.syntax_accepted.remove(&buffer);
                self.syntax_failures.remove(&buffer);
                self.syntax_due.remove(&buffer);
                continue;
            }
            if self.syntax_submitted.get(&buffer).is_some_and(
                |(submitted, _, submitted_language)| {
                    *submitted == revision && *submitted_language == language
                },
            ) {
                continue;
            }
            let due = self.syntax_due.entry(buffer).or_insert((
                revision,
                now + Duration::from_millis(self.configuration.settings.syntax.debounce_ms),
            ));
            if due.0 != revision {
                *due = (
                    revision,
                    now + Duration::from_millis(self.configuration.settings.syntax.debounce_ms),
                );
                continue;
            }
            if now < due.1 {
                continue;
            }
            let document = *self.syntax_documents.entry(buffer).or_insert_with(|| {
                let id = SyntaxDocumentId(self.next_syntax_document);
                self.next_syntax_document = self.next_syntax_document.saturating_add(1);
                id
            });
            let generation = self.next_syntax_generation;
            self.next_syntax_generation = self.next_syntax_generation.saturating_add(1);
            let configured = &self.configuration.settings.syntax;
            let policy = SyntaxPolicy {
                maximum_source_bytes: configured.maximum_source_bytes,
                maximum_spans: configured.maximum_spans,
                maximum_folds: configured.maximum_folds,
                maximum_indent_rows: configured.maximum_indent_rows,
            };
            let Some(source) = self
                .state
                .buffers
                .get(buffer)
                .map(|state| state.text.to_string())
            else {
                continue;
            };
            if self
                .syntax
                .submit(document, generation, revision.0, language, source, policy)
                .is_ok()
            {
                self.syntax_submitted
                    .insert(buffer, (revision, generation, language));
                self.syntax_due.remove(&buffer);
            }
        }
        while let Ok(Some(event)) = self.syntax.try_event() {
            match event {
                SyntaxEvent::Parsed(result) => {
                    if let Some((&buffer, _)) = self
                        .syntax_documents
                        .iter()
                        .find(|(_, document)| **document == result.document)
                    {
                        if self.accepts_syntax_event(
                            buffer,
                            result.revision,
                            result.generation,
                            result.language,
                        ) {
                            self.syntax_failures.remove(&buffer);
                            self.syntax_accepted.insert(buffer, result);
                        }
                    }
                }
                SyntaxEvent::Failed {
                    document,
                    generation,
                    revision,
                    language,
                    diagnostic,
                } => {
                    if let Some((&buffer, _)) = self
                        .syntax_documents
                        .iter()
                        .find(|(_, candidate)| **candidate == document)
                        && self.accepts_syntax_event(buffer, revision, generation, language)
                    {
                        self.syntax_accepted.remove(&buffer);
                        self.syntax_failures.insert(
                            buffer,
                            SyntaxFailure {
                                revision: BufferRevision(revision),
                                generation,
                                language,
                                diagnostic,
                            },
                        );
                    }
                }
                SyntaxEvent::Skipped {
                    document,
                    generation,
                    revision,
                    language,
                } => {
                    if let Some((&buffer, _)) = self
                        .syntax_documents
                        .iter()
                        .find(|(_, candidate)| **candidate == document)
                        && self.accepts_syntax_event(buffer, revision, generation, language)
                    {
                        self.syntax_accepted.remove(&buffer);
                        self.syntax_failures.insert(
                            buffer,
                            SyntaxFailure {
                                revision: BufferRevision(revision),
                                generation,
                                language,
                                diagnostic: self.message("syntax_source_too_large", &[]),
                            },
                        );
                    }
                }
            }
        }

        let collapsed = self
            .ui
            .syntax_folds
            .iter()
            .filter(|fold| fold.collapsed)
            .map(|fold| (fold.view, fold.start_line, fold.end_line))
            .collect::<HashSet<_>>();
        let mut syntax_folds = Vec::new();
        for (view_id, view) in self.state.views.iter() {
            if let Some(result) = self.syntax_accepted.get(&view.buffer) {
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
            .syntax_accepted
            .iter()
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

pub(in crate::editor::session) fn semantic_role(name: &str) -> crate::language::syntax::SyntaxRole {
    match name {
        "comment" => crate::language::syntax::SyntaxRole::Comment,
        "string" => crate::language::syntax::SyntaxRole::String,
        "number" => crate::language::syntax::SyntaxRole::Number,
        "keyword" | "modifier" | "operator" => crate::language::syntax::SyntaxRole::Keyword,
        "type" | "class" | "enum" | "interface" | "struct" => {
            crate::language::syntax::SyntaxRole::Type
        }
        _ => crate::language::syntax::SyntaxRole::Variable,
    }
}
