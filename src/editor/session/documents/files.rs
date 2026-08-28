use super::*;

impl InteractiveApplication {
    pub(in crate::editor::session) fn start_open(&mut self, mut path: PathBuf) {
        if path.is_relative() {
            path = self.state.workspace_root.join(path);
        }
        match self.vfs.read(path) {
            Ok(id) => {
                self.pending_vfs.insert(id, PendingVfs::Open);
                self.ui.notification = Some(self.message("opening_file", &[]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn start_reload(&mut self, force: bool) {
        let Some(buffer_id) = self.state.active_buffer_id() else {
            self.show_error(self.message("no_active_reload", &[]));
            return;
        };
        let buffer = &self.state.buffers[buffer_id];
        if buffer.is_virtual_read_only() {
            self.show_error("virtual document is read-only");
            return;
        }
        if buffer.dirty && !force {
            self.show_error(self.message("dirty_reload", &[]));
            return;
        }
        let Some(path) = buffer.path.clone() else {
            self.show_error(self.message("scratch_reload", &[]));
            return;
        };
        match self.vfs.read(path) {
            Ok(id) => {
                self.pending_vfs
                    .insert(id, PendingVfs::Reload { buffer: buffer_id });
                self.ui.notification = Some(self.message("reloading", &[]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::editor::session) fn close_active_buffer(&mut self, force: bool) {
        let Some(view_id) = self.state.active_view else {
            self.ui.dashboard = true;
            return;
        };
        let buffer_id = self.state.views[view_id].buffer;
        let virtual_entry = self
            .plugin_virtual_editor
            .entry_for_buffer(buffer_id)
            .cloned();
        if self.state.buffers[buffer_id].dirty && !force {
            self.show_error(self.message("dirty_close", &[]));
            return;
        }
        if let Err(error) = self.state.close_active_view() {
            self.show_error(error);
            return;
        }
        let still_viewed = self
            .state
            .views
            .iter()
            .any(|(_, view)| view.buffer == buffer_id);
        if !still_viewed {
            if let Some(entry) = virtual_entry {
                self.plugin_virtual_documents
                    .cancel(&entry.plugin, entry.load, entry.document);
                self.plugin_virtual_editor.forget_document(
                    &entry.plugin,
                    entry.load,
                    entry.document,
                );
            }
            let documents = self
                .lsp_sessions
                .iter()
                .filter_map(|(server, state)| {
                    state
                        .documents
                        .get(&buffer_id)
                        .map(|document| (server.clone(), document.uri()))
                })
                .collect::<Vec<_>>();
            self.detach_lsp_documents(buffer_id, documents);
            self.state.remove_buffer(buffer_id);
            self.lsp_synced_revisions.remove(&buffer_id);
        }
        if self.state.active_view.is_none()
            && let Some(buffer) = self.state.buffers.keys().next()
        {
            let _ = self.state.show_buffer(buffer);
        }
        self.ui.dashboard = self.state.active_view.is_none();
        if !self.ui.dashboard {
            self.sync_lsp_document();
        } else {
            self.active_lsp_server = None;
            self.lsp_capabilities = None;
            self.clear_lsp_ui_projection();
        }
        self.ui.notification = Some(if self.ui.dashboard {
            self.message("returned_dashboard", &[])
        } else {
            self.message("buffer_closed", &[])
        });
        self.ui.notification_error = false;
    }

    pub(in crate::editor::session) fn start_save(
        &mut self,
        quit_after: bool,
        requested_path: Option<PathBuf>,
        force: bool,
    ) {
        let Some(buffer_id) = self.state.active_buffer_id() else {
            self.show_error(self.message("no_active_save", &[]));
            return;
        };
        let buffer = &self.state.buffers[buffer_id];
        if buffer.is_virtual_read_only() {
            self.show_error("virtual document is read-only");
            return;
        }
        let mut path = match requested_path {
            Some(path) => path,
            None => {
                let Some(path) = buffer.path.clone() else {
                    self.show_error(self.message("scratch_no_name", &[]));
                    return;
                };
                path
            }
        };
        if path.is_relative() {
            path = self.state.workspace_root.join(path);
        }
        let revision = buffer.text.revision();
        let bytes = configured_file_bytes(&buffer.text, &self.configuration.settings.editor);
        let expectation = if force {
            WriteExpectation::Any
        } else if buffer.path.as_ref() == Some(&path) {
            WriteExpectation::Exact(buffer.persisted_bytes().to_vec())
        } else {
            WriteExpectation::Missing
        };
        match self
            .vfs
            .write_checked(path.clone(), bytes.clone(), expectation)
        {
            Ok(id) => {
                self.pending_vfs.insert(
                    id,
                    PendingVfs::Save {
                        buffer: buffer_id,
                        revision,
                        bytes,
                        target: path,
                        quit_after,
                    },
                );
                self.ui.notification = Some(self.message("saving", &[]));
                self.ui.notification_error = false;
            }
            Err(error) => self.show_error(error),
        }
    }
}
