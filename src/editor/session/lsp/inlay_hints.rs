use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn sync_ui_diagnostics(&mut self) {
        let candidate = self
            .diagnostics
            .problems(None, None)
            .into_iter()
            .filter_map(|problem| {
                Some(UiDiagnostic {
                    path: path_from_file_uri(&problem.uri)?,
                    range: problem.range,
                    severity: problem.severity,
                    source: problem.source,
                    code: problem.code,
                    message: problem.message,
                })
            })
            .collect::<Vec<_>>();
        if candidate != self.ui.diagnostics {
            self.ui.diagnostics = candidate;
            self.content_line_diagnostic_revision =
                self.content_line_diagnostic_revision.saturating_add(1);
            self.content_line_projection_signature = None;
        }
    }

    pub(in crate::editor::session) fn clear_lsp_ui_projection(&mut self) {
        self.lsp_completion_items.clear();
        self.ui.language_completion = None;
        self.ui.documentation_popup = None;
        self.ui.lsp_semantic_spans.clear();
        self.ui.lsp_highlight_spans.clear();
        self.ui.lsp_inlay_hints.clear();
        self.ui.lsp_folds.clear();
        self.lsp_selection_ranges.clear();
        self.lsp_projection_owner = None;
        self.lsp_request_owner = None;
        self.lsp_inlay_request_owner = None;
        self.lsp_inlay_empty_retries = 0;
        self.lsp_completion_owner = None;
    }

    pub(in crate::editor::session) fn current_lsp_projection_owner(
        &self,
    ) -> Option<LspProjectionOwner> {
        let server = self.active_lsp_server.clone()?;
        let view = self.state.active_view?;
        let (buffer, document) = self.active_lsp_document()?;
        let session = self.lsp_sessions.get(&server)?;
        if !session.definition.supports_language(&document.language_id)
            || !session.documents.contains_key(&buffer)
            || !document.path.starts_with(&session.root_path)
        {
            return None;
        }
        Some(LspProjectionOwner {
            server,
            buffer,
            view,
            revision: BufferRevision(document.version),
            uri: document.uri(),
            language: document.language_id,
        })
    }

    pub(in crate::editor::session) fn refresh_lsp_projection_owner(&mut self) {
        let current = self.current_lsp_projection_owner();
        if self.lsp_projection_owner != current {
            self.clear_lsp_ui_projection();
            self.lsp_projection_owner = current;
            if self.lsp_projection_owner.is_some() {
                self.schedule_lsp_inlay_refresh(true);
            }
        }
    }

    pub(in crate::editor::session) fn clear_lsp_session_state(&mut self) {
        if let Some(server) = self.active_lsp_server.clone() {
            self.clear_lsp_server_ownership(&server);
        }
        self.active_lsp_server = None;
        self.lsp_synced_revisions.clear();
        self.lsp_capabilities = None;
        self.sync_ui_diagnostics();
        self.clear_lsp_ui_projection();
    }

    pub(in crate::editor::session) fn schedule_lsp_inlay_startup_refresh(&mut self) {
        self.lsp_inlay_empty_retries = 0;
        self.lsp_inlay_refresh_due = Some(
            Instant::now()
                + if self.configuration.settings.lsp.inlay_hint_level
                    == crate::config::InlayHintLevel::Max
                {
                    LSP_INLAY_HINT_MAX_STARTUP_DELAY
                } else {
                    Duration::ZERO
                },
        );
    }

    pub(in crate::editor::session) fn schedule_lsp_inlay_warmup_retry(&mut self) -> bool {
        if !self.configuration.settings.lsp.inlay_hints
            || self.configuration.settings.lsp.inlay_hint_level
                != crate::config::InlayHintLevel::Max
            || self.lsp_inlay_empty_retries >= LSP_INLAY_HINT_WARMUP_RETRY_LIMIT
        {
            return false;
        }
        let multiplier = 1_u32 << u32::from(self.lsp_inlay_empty_retries.min(4));
        self.lsp_inlay_empty_retries = self.lsp_inlay_empty_retries.saturating_add(1);
        self.lsp_inlay_refresh_due =
            Some(Instant::now() + LSP_INLAY_HINT_EMPTY_RETRY_BASE.saturating_mul(multiplier));
        true
    }

    pub(in crate::editor::session) fn schedule_lsp_inlay_refresh(&mut self, immediate: bool) {
        if !self.configuration.settings.lsp.inlay_hints {
            self.lsp_inlay_refresh_due = None;
            self.lsp_inlay_request_owner = None;
            self.lsp_inlay_empty_retries = 0;
            self.ui.lsp_inlay_hints.clear();
            return;
        }
        self.lsp_inlay_empty_retries = 0;
        self.lsp_inlay_refresh_due = Some(if immediate {
            Instant::now()
        } else {
            Instant::now() + LSP_INLAY_HINT_DEBOUNCE
        });
    }

    pub(in crate::editor::session) fn process_lsp_inlay_refresh(&mut self) {
        if !self.configuration.settings.lsp.inlay_hints {
            self.lsp_inlay_refresh_due = None;
            self.lsp_inlay_request_owner = None;
            self.lsp_inlay_empty_retries = 0;
            self.ui.lsp_inlay_hints.clear();
            return;
        }
        let Some(due) = self.lsp_inlay_refresh_due else {
            return;
        };
        if Instant::now() < due {
            return;
        }
        self.lsp_inlay_refresh_due = None;
        self.request_lsp_inlay_hints();
    }
}
