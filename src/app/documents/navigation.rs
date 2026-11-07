//! File navigation requests and cursor destinations.

use crate::app::App;
use crate::app::commands::DiagnosticFilter;
use crate::app::persistence::CHANGE_LIMIT;
use crate::app::persistence::JUMP_LIMIT;
use crate::app::picker::MatcherSource;
use crate::editor::EditorOperation;
use crate::editor::EditorTransaction;
use crate::editor::Precondition;
use crate::editor::UndoPolicy;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::PositionCodec;
use crate::editor::text::PositionEncoding;
use crate::language::lsp::path_from_file_uri;
use crate::persistence::PersistedLocation;
use std::path::Path;

impl App {
    pub(in crate::app) fn navigate_byte_location(&mut self, uri: &str, range: ByteRange) {
        let Some((buffer, _)) = self.state.buffers.iter().find(|(id, buffer)| {
            crate::app::diagnostics::document_uri(*id, buffer.path.as_deref()) == uri
        }) else {
            let Some(path) = path_from_file_uri(uri) else {
                return;
            };
            let roots = self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect();
            match self.documents.read_scoped(path, roots) {
                Ok(id) => {
                    self.remember_navigation_read(id);
                    self.search.register_byte_navigation(id, range, None);
                }
                Err(error) => self.show_error(error),
            }
            return;
        };
        let transaction = self.transaction_id();
        match self
            .state
            .prepare_document_location(buffer, range.start, transaction)
            .and_then(|draft| self.commit_editor_draft(draft, None, false))
        {
            Ok(()) => self.ensure_cursor_visible(),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn navigate_lsp_location_encoded(
        &mut self,
        location: &crate::language::lsp::LspLocation,
        encoding: PositionEncoding,
    ) {
        self.navigate_location_encoded(location, encoding, false);
    }
}

impl App {
    pub(in crate::app) fn navigate_location_encoded(
        &mut self,
        location: &crate::language::lsp::LspLocation,
        encoding: PositionEncoding,
        cancel_on_capture_change: bool,
    ) {
        let Some(path) = path_from_file_uri(&location.uri) else {
            self.show_error(self.message("lsp_invalid_uri", &[("uri", location.uri.clone())]));
            return;
        };
        self.record_jump();
        let Some((buffer_id, buffer)) = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        else {
            let mut roots: Vec<_> = self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect();
            // A definition in a library or the toolchain opens too, read-only: the reader may
            // look at it, and nothing here can write it.
            let mut path = path;
            if self.workspace.require_path(&path).is_err() {
                if let Ok(real) = path.canonicalize() {
                    path = real;
                }
                roots.extend(path.parent().map(std::path::Path::to_owned));
            }
            match self.documents.read_scoped(path, roots) {
                Ok(id) => {
                    if cancel_on_capture_change {
                        self.remember_navigation_read(id);
                    }
                    self.language.register_navigation_read(
                        id,
                        location.range.clone(),
                        encoding,
                        self.window_session.navigation_stamp(
                            self.state.window_version(),
                            self.state.active_view(),
                        ),
                        self.workspace.version(),
                    );
                    self.notify_message(self.message("opening_file", &[]), false);
                }
                Err(error) => self.show_error(error),
            }
            return;
        };
        let Ok(position) = PositionCodec::new(&buffer.text).lsp_to_byte(
            location.range.start.line,
            location.range.start.character,
            encoding,
        ) else {
            self.show_error(self.message("lsp_invalid_position", &[]));
            return;
        };
        if let Ok(view) = self.state.show_buffer(buffer_id) {
            let transaction = EditorTransaction::new(self.transaction_id())
                .with_precondition(Precondition::ViewDisplays {
                    view,
                    buffer: buffer_id,
                })
                .with_operation(EditorOperation::SetCursor { view, position })
                .with_undo_policy(UndoPolicy::Ignore);
            if let Err(error) = self.state.apply_transaction(&transaction) {
                self.show_error(error);
            } else {
                self.ensure_cursor_visible();
            }
        }
    }
}

impl App {
    pub(in crate::app) fn current_persisted_location(&self) -> Option<PersistedLocation> {
        let view = self.state.views.get(self.state.active_view()?)?;
        let buffer = self.state.buffers.get(view.buffer)?;
        Some(PersistedLocation {
            path: buffer.path.clone()?,
            byte: view.cursor.get(),
            anchor: view
                .selection
                .map_or(view.cursor.get(), |selection| selection.anchor.get()),
            scroll_line: view.scroll_line,
            scroll_column: view.scroll_column,
        })
    }
}

impl App {
    /// Remember where the cursor is before it jumps, for `ctrl-o`.
    pub(in crate::app) fn record_jump(&mut self) {
        if let Some(location) = self.current_persisted_location() {
            self.persistence
                .record_location(location, false, JUMP_LIMIT);
        }
    }
}

impl App {
    /// `ctrl-o` (back) and `ctrl-i` (forward) through the jump list.
    pub(in crate::app) fn walk_jumps(&mut self, back: bool) {
        let current = self.current_persisted_location();
        let maximum = JUMP_LIMIT;
        if let Some(location) = self.persistence.walk_jumps(current, back, maximum) {
            self.go_to_persisted_location(location);
        }
    }
}

impl App {
    pub(in crate::app) fn push_change_location(&mut self) {
        if let Some(location) = self.current_persisted_location() {
            self.persistence
                .record_location(location, true, CHANGE_LIMIT);
        }
    }
}

impl App {
    pub(in crate::app) fn remember_navigation_read(&mut self, id: crate::files::RequestId) {
        self.navigation_reads.insert(
            id,
            (
                self.window_session
                    .navigation_stamp(self.state.window_version(), self.state.active_view()),
                self.input.capture_epoch(),
            ),
        );
    }
}

impl App {
    pub(in crate::app) fn navigate_matcher_source(&mut self, mut source: MatcherSource) {
        if !source.path.is_absolute() {
            self.show_error(self.message("navigation_outside_workspace", &[]));
            return;
        }
        if let Some((buffer, document)) = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&source.path))
        {
            let position = document
                .text
                .line_to_byte(source.line)
                .ok()
                .map(|offset| {
                    ByteOffset(
                        offset
                            .get()
                            .saturating_add(source.column)
                            .min(document.text.len_bytes()),
                    )
                })
                .filter(|offset| document.text.validate_offset(*offset).is_ok())
                .unwrap_or(ByteOffset(0));
            let transaction = self.transaction_id();
            match self
                .state
                .prepare_document_location(buffer, position, transaction)
                .and_then(|draft| self.commit_editor_draft(draft, None, false))
            {
                Ok(()) => {
                    self.set_dashboard_visibility(false);
                    self.ensure_cursor_visible();
                }
                Err(error) => self.show_error(error),
            }
            return;
        }
        let mut roots: Vec<_> = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        // A debugger frame in a library or the toolchain opens too, read-only, as definitions do.
        if !roots.iter().any(|root| source.path.starts_with(root)) {
            if let Ok(real) = source.path.canonicalize() {
                source.path = real;
            }
            roots.extend(source.path.parent().map(std::path::Path::to_owned));
        }
        match self.documents.read_scoped(source.path.clone(), roots) {
            Ok(id) => {
                self.remember_navigation_read(id);
                self.search.register_matcher_navigation(id, source);
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn navigate_persisted_location(&mut self, location: PersistedLocation) {
        if !location.path.is_absolute()
            || !self
                .workspace
                .roots()
                .iter()
                .any(|root| location.path.starts_with(&root.identity.canonical_path))
        {
            self.show_error(self.message("navigation_outside_workspace", &[]));
            return;
        }
        self.record_jump();
        self.go_to_persisted_location(location);
    }
}

impl App {
    fn go_to_persisted_location(&mut self, location: PersistedLocation) {
        let mut roots: Vec<_> = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        // An open file was let in already, even when it is outside the project.
        if self
            .state
            .buffers
            .values()
            .any(|buffer| buffer.path.as_ref() == Some(&location.path))
        {
            roots.extend(location.path.parent().map(Path::to_path_buf));
        }
        match self.documents.read_scoped(location.path.clone(), roots) {
            Ok(id) => {
                self.remember_navigation_read(id);
                self.persistence.register_navigation_read(id, location);
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn navigate_diagnostic(
        &mut self,
        direction: i8,
        filter: DiagnosticFilter,
        count: u64,
    ) {
        let Some(view_id) = self.state.active_view() else {
            return;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor = view.cursor;
        let uri = crate::app::diagnostics::document_uri(
            buffer_id,
            self.state.buffers[buffer_id].path.as_deref(),
        );
        self.diagnostics.clear_stale_documents(&self.state);
        let Some(key) =
            self.diagnostics
                .adjacent(&uri, cursor, direction, filter.severities(), count)
        else {
            self.show_error(self.message("diagnostic_none", &[]));
            return;
        };
        let Some(target) = self.diagnostics.resolve(&key, &self.state) else {
            self.sync_ui_diagnostics();
            return;
        };
        let position = target.range.start;
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::ViewDisplays {
                view: view_id,
                buffer: buffer_id,
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position,
            })
            .with_undo_policy(UndoPolicy::Ignore);
        let result = self
            .state
            .prepare_transaction(&transaction)
            .map_err(|e| e.to_string())
            .and_then(|draft| self.commit_editor_draft(draft, None, false));
        if let Err(error) = result {
            self.show_error(error);
        } else {
            self.diagnostics
                .expand(key, filter.severities(), &self.state);
            self.sync_ui_diagnostics();
            self.clear_notice();
            self.ensure_cursor_visible();
        }
    }
}
