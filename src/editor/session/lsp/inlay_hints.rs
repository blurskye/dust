use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn sync_ui_diagnostics(&mut self) {
        if let Some(candidate) = self.diagnostics.projection(&self.state) {
            self.ui.diagnostics = candidate;
            self.window_session.diagnostics_changed();
        }
        self.ui.expanded_diagnostic = self.diagnostics.expanded(&self.state);
    }
    fn clear_lsp_ui_rows(&mut self) {
        self.ui.language_completion = None;
        self.close_documentation();
        self.ui.lsp_semantic_spans.clear();
        self.ui.lsp_highlight_spans.clear();
        self.ui.lsp_inlay_hints.clear();
        self.ui.lsp_folds.clear();
    }
    pub(in crate::editor::session) fn clear_lsp_ui_projection(&mut self) {
        self.language.clear_projection();
        self.clear_lsp_ui_rows();
    }
    pub(in crate::editor::session) fn refresh_lsp_projection_owner(&mut self) {
        let document = self.active_lsp_document();
        if self.language.refresh_projection(
            &self.state,
            document
                .as_ref()
                .map(|(buffer, document)| (*buffer, document)),
        ) {
            self.clear_lsp_ui_rows();
            if self.language.projection().is_some() {
                self.schedule_lsp_inlay_refresh(true);
            }
        }
    }
    pub(in crate::editor::session) fn clear_lsp_session_state(&mut self) {
        if let Some(server) = self.language.active().cloned() {
            self.clear_lsp_server_ownership(&server);
        }
        self.language.clear_active();
        self.sync_ui_diagnostics();
        self.clear_lsp_ui_projection();
    }
    pub(in crate::editor::session) fn schedule_lsp_inlay_startup_refresh(&mut self) {
        self.language
            .startup_inlays(&self.configuration.settings().lsp, Instant::now());
    }
    pub(in crate::editor::session) fn schedule_lsp_inlay_warmup_retry(&mut self) -> bool {
        self.language
            .retry_inlays(&self.configuration.settings().lsp, Instant::now())
    }
    pub(in crate::editor::session) fn schedule_lsp_inlay_refresh(&mut self, immediate: bool) {
        self.language.schedule_inlays(
            &self.configuration.settings().lsp,
            immediate,
            Instant::now(),
        );
        if !self.configuration.settings().lsp.inlay_hints {
            self.ui.lsp_inlay_hints.clear();
        }
    }
    pub(in crate::editor::session) fn process_lsp_inlay_refresh(&mut self) {
        if self
            .language
            .inlay_ready(&self.configuration.settings().lsp, Instant::now())
        {
            self.request_lsp_inlay_hints();
        }
        if !self.configuration.settings().lsp.inlay_hints {
            self.ui.lsp_inlay_hints.clear();
        }
    }
}
