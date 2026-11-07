//! Editable documents and ordered work that must finish before a save.

use crate::app::App;
use crate::app::documents::{DocumentFailure, Documents};
use crate::app::windows::EditorCloseSave;
use crate::editor::BufferId;
use crate::editor::text::{TextBuffer, TextVersion};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

/// The receiver runs outside the editor loop and answers exactly once. Dropping it cancels
/// the save; a failed custom write never falls through to a filesystem write.
pub(in crate::app) struct SaveWorkRequest {
    pub buffer: BufferId,
    pub authority: crate::editor::CommandAuthority,
    pub text: TextBuffer,
    pub reply: SyncSender<Result<Option<String>, String>>,
}

#[derive(Clone)]
pub(in crate::app) struct SaveStep {
    pub name: String,
    pub requests: SyncSender<SaveWorkRequest>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(in crate::app) enum Formatter {
    Lsp,
    Tool(String),
    Text(SaveStep),
}

struct EditableDocument {
    source: TextVersion,
    save: SaveStep,
}

struct PendingSave {
    buffer: BufferId,
    authority: crate::editor::CommandAuthority,
    source: TextVersion,
    steps: VecDeque<SaveStep>,
    reply: Option<Receiver<Result<Option<String>, String>>>,
    deadline: Instant,
    path: Option<PathBuf>,
    force: bool,
    quit: bool,
    close: Option<EditorCloseSave>,
    provider: bool,
    stage: bool,
    formatting: bool,
    snapshot: TextBuffer,
}

#[derive(Default)]
pub(super) struct SaveWork {
    documents: HashMap<BufferId, EditableDocument>,
    before: HashMap<String, Vec<SaveStep>>,
    formatters: HashMap<String, Vec<Formatter>>,
    named: HashMap<String, (String, bool, i32, SaveStep)>,
    pub(super) remaining_formatters: HashMap<BufferId, VecDeque<Formatter>>,
    pub(super) format_only: std::collections::HashSet<BufferId>,
    pending: HashMap<BufferId, PendingSave>,
}

impl Documents {
    pub(in crate::app) fn register_save_step(
        &mut self,
        filetype: String,
        before: bool,
        order: i32,
        step: SaveStep,
    ) {
        self.save_work
            .named
            .insert(step.name.clone(), (filetype, before, order, step));
    }

    pub(in crate::app) fn remove_save_step(&mut self, name: &str) {
        self.save_work.named.remove(name);
    }

    fn named_save_steps(&self, filetype: &str, before: bool) -> Vec<SaveStep> {
        let mut steps: Vec<_> = self
            .save_work
            .named
            .values()
            .filter(|(language, kind, _, _)| language == filetype && *kind == before)
            .collect();
        steps.sort_by_key(|(_, _, order, step)| (*order, &step.name));
        steps
            .into_iter()
            .map(|(_, _, _, step)| step.clone())
            .collect()
    }

    #[allow(dead_code)]
    pub(in crate::app) fn set_before_save(&mut self, filetype: String, steps: Vec<SaveStep>) {
        self.save_work.before.insert(filetype, steps);
    }

    #[allow(dead_code)]
    pub(in crate::app) fn set_formatters(&mut self, filetype: String, steps: Vec<Formatter>) {
        self.save_work.formatters.insert(filetype, steps);
    }

    pub(super) fn save_work_pending(&self, buffer: BufferId) -> bool {
        self.save_work.pending.contains_key(&buffer)
    }

    pub(super) fn formatter_chain(&self, filetype: &str) -> Option<VecDeque<Formatter>> {
        let named = self.named_save_steps(filetype, false);
        let existing = self.save_work.formatters.get(filetype);
        if named.is_empty() && existing.is_none() {
            return None;
        }
        Some(
            existing
                .into_iter()
                .flatten()
                .cloned()
                .chain(named.into_iter().map(Formatter::Text))
                .collect(),
        )
    }

    pub(in crate::app) fn stage_after_save_work(&mut self, buffer: BufferId) {
        if let Some(pending) = self.save_work.pending.get_mut(&buffer) {
            pending.stage = true;
        }
    }

    pub(super) fn forget_save_work(&mut self, buffer: BufferId) {
        self.save_work.documents.remove(&buffer);
        self.save_work.pending.remove(&buffer);
        self.save_work.remaining_formatters.remove(&buffer);
        self.save_work.format_only.remove(&buffer);
    }

    pub(super) fn editable_document(&self, buffer: BufferId) -> bool {
        self.save_work.documents.contains_key(&buffer)
    }
}

impl App {
    pub(in crate::app) fn open_editable_document(
        &mut self,
        title: String,
        filetype: String,
        text: String,
        save: SaveStep,
    ) -> Result<BufferId, String> {
        self.effect_authority
            .require(crate::editor::CommandAccess::Edit)
            .map_err(|error| error.to_string())?;
        let (buffer, _) = self
            .state
            .open_text(TextBuffer::from_text(&text), None)
            .map_err(|error| error.to_string())?;
        let document = &mut self.state.buffers[buffer];
        document.virtual_title = Some(title);
        document.virtual_language = Some(filetype);
        self.documents.save_work.documents.insert(
            buffer,
            EditableDocument {
                source: document.text.source_version(),
                save,
            },
        );
        self.set_dashboard_visibility(false);
        Ok(buffer)
    }

    pub(super) fn save_filetype(&self, buffer: BufferId) -> String {
        let document = &self.state.buffers[buffer];
        document
            .virtual_language
            .clone()
            .or_else(|| {
                document
                    .path
                    .as_deref()
                    .and_then(crate::language::lsp::language_id_for_path)
                    .map(str::to_owned)
            })
            .unwrap_or_default()
    }

    pub(super) fn begin_save_work(
        &mut self,
        buffer: BufferId,
        quit: bool,
        path: Option<PathBuf>,
        force: bool,
        close: Option<EditorCloseSave>,
    ) -> Result<bool, DocumentFailure> {
        if self.documents.save_work.pending.contains_key(&buffer) {
            return Err(DocumentFailure::Error(
                "a save for this buffer is already pending".into(),
            ));
        }
        let Some(document) = self.state.buffers.get(buffer) else {
            return Err(DocumentFailure::Policy("no_active_save"));
        };
        if document.read_only || document.is_virtual_read_only() {
            return Err(DocumentFailure::Error("document is read-only".into()));
        }
        let filetype = self.save_filetype(buffer);
        let mut steps: VecDeque<_> = self
            .documents
            .save_work
            .before
            .get(&filetype)
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        steps.extend(self.documents.named_save_steps(&filetype, true));
        let provider = self.documents.save_work.documents.get(&buffer);
        if let Some(provider) = provider {
            if path.is_some() {
                return Err(DocumentFailure::Error(
                    "an editable provider document cannot be saved as a file".into(),
                ));
            }
            if !provider
                .source
                .same_instance(&document.text.source_version())
            {
                return Err(DocumentFailure::Error(
                    "editable document was replaced".into(),
                ));
            }
            steps.push_back(provider.save.clone());
        }
        if steps.is_empty() {
            return Ok(false);
        }
        self.documents.save_work.pending.insert(
            buffer,
            PendingSave {
                buffer,
                authority: self.effect_authority.clone(),
                source: document.text.source_version(),
                snapshot: document.text.clone(),
                steps,
                reply: None,
                deadline: Instant::now() + Duration::from_secs(3),
                path,
                force,
                quit,
                close,
                provider: provider.is_some(),
                stage: false,
                formatting: false,
            },
        );
        self.process_save_work();
        Ok(true)
    }

    pub(super) fn start_next_save_formatter(&mut self, buffer: BufferId) {
        let Some(step) = self
            .documents
            .save_work
            .remaining_formatters
            .get_mut(&buffer)
            .and_then(VecDeque::pop_front)
        else {
            return;
        };
        let pending = self
            .format_on_save
            .get_mut(&buffer)
            .expect("formatting save");
        pending.request = None;
        pending.formatter = None;
        pending.started = true;
        pending.deadline = Instant::now() + Duration::from_secs(3);
        match step {
            Formatter::Tool(name) => {
                pending.formatter = Some(name);
                pending.started = false;
                self.start_pending_save_formatters();
            }
            Formatter::Lsp => {
                pending.deadline = Instant::now() + Duration::from_secs(1);
                self.request_save_lsp_format(buffer);
            }
            Formatter::Text(step) => {
                let document = &self.state.buffers[buffer];
                self.documents.save_work.pending.insert(
                    buffer,
                    PendingSave {
                        buffer,
                        authority: pending.authority.clone(),
                        source: document.text.source_version(),
                        snapshot: document.text.clone(),
                        steps: VecDeque::from([step]),
                        reply: None,
                        deadline: Instant::now() + Duration::from_secs(3),
                        path: None,
                        force: false,
                        quit: false,
                        close: None,
                        provider: false,
                        stage: false,
                        formatting: true,
                    },
                );
                self.process_save_work();
            }
        }
    }

    fn request_save_lsp_format(&mut self, buffer: BufferId) {
        let Some(document) = self.state.buffers.get(buffer).and_then(|buffer| {
            let path = buffer.path.clone()?;
            Some(crate::language::lsp::LspDocument {
                language_id: crate::language::lsp::language_id_for_path(&path)?.into(),
                path,
                version: buffer.text.revision().0,
                text: buffer.text.to_string(),
            })
        }) else {
            return;
        };
        let Some(server) = self.language.formatting_server(buffer, &document) else {
            return;
        };
        match self.language.request_buffer_format(
            &self.state,
            buffer,
            &server,
            crate::language::lsp::LspFeature::Formatting {
                tab_size: self.state.policy.tab_width,
                insert_spaces: self.state.policy.expand_tabs,
            },
            document,
        ) {
            Ok(()) => {
                self.format_on_save
                    .get_mut(&buffer)
                    .expect("formatting save")
                    .request = Some((server, self.language.request_id()))
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn apply_save_text(&mut self, buffer: BufferId, text: String) -> Result<(), String> {
        self.effect_authority
            .require(crate::editor::CommandAccess::Edit)
            .map_err(|error| error.to_string())?;
        if self.state.buffers[buffer].text.text_equals(&text) {
            return Ok(());
        }
        self.replace_with_recovered(buffer, text)
    }

    pub(in crate::app) fn process_save_work(&mut self) {
        let buffers: Vec<_> = self.documents.save_work.pending.keys().copied().collect();
        for buffer in buffers {
            let mut pending = self
                .documents
                .save_work
                .pending
                .remove(&buffer)
                .expect("pending save");
            let current = self
                .state
                .buffers
                .get(buffer)
                .map(|document| document.text.source_version());
            if current.as_ref() != Some(&pending.source) {
                self.fail_save_work(
                    pending,
                    "document changed while before-save work was pending".into(),
                );
                continue;
            }
            if let Some(reply) = &pending.reply {
                let result = match reply.try_recv() {
                    Ok(result) => result,
                    Err(TryRecvError::Empty) if Instant::now() < pending.deadline => {
                        self.documents.save_work.pending.insert(buffer, pending);
                        continue;
                    }
                    Err(TryRecvError::Empty) => Err("save work timed out".into()),
                    Err(TryRecvError::Disconnected) => Err("save work was cancelled".into()),
                };
                match result {
                    Ok(text) => {
                        let previous = std::mem::replace(
                            &mut self.effect_authority,
                            pending.authority.clone(),
                        );
                        let result = text.map_or(Ok(()), |text| self.apply_save_text(buffer, text));
                        self.effect_authority = previous;
                        if let Err(error) = result {
                            self.fail_save_work(pending, error);
                            continue;
                        }
                        pending.source = self.state.buffers[buffer].text.source_version();
                        pending.snapshot = self.state.buffers[buffer].text.clone();
                        pending.reply = None;
                    }
                    Err(error) => {
                        self.fail_save_work(pending, error);
                        continue;
                    }
                }
            }
            if let Some(step) = pending.steps.pop_front() {
                let (reply, receiver) = std::sync::mpsc::sync_channel(1);
                let request = SaveWorkRequest {
                    buffer,
                    authority: pending.authority.clone(),
                    text: pending.snapshot.clone(),
                    reply,
                };
                if step.requests.try_send(request).is_err() {
                    self.fail_save_work(pending, format!("{} is unavailable", step.name));
                    continue;
                }
                pending.reply = Some(receiver);
                self.documents.save_work.pending.insert(buffer, pending);
            } else if pending.formatting {
                self.finish_format_on_save(buffer);
            } else if pending.provider {
                self.state.mark_buffer_saved_as(
                    buffer,
                    pending.snapshot.revision(),
                    pending.snapshot.to_bytes(),
                    None,
                );
                self.finish_provider_save(buffer, pending);
            } else {
                let previous = std::mem::replace(&mut self.effect_authority, pending.authority);
                let result = self.request_disk_save(
                    buffer,
                    pending.quit,
                    pending.path,
                    pending.force,
                    pending.close.clone(),
                );
                self.effect_authority = previous;
                match result {
                    Ok(id) if pending.stage => self.input.bind_stage_save(id),
                    Ok(_) => {}
                    Err(error) => {
                        if let Some(close) = &pending.close {
                            self.window_session.fail_editor_close_save(close);
                        }
                        if pending.stage {
                            self.input.clear_pending_stage();
                        }
                        self.show_document_failure(error);
                    }
                }
            }
        }
    }

    fn fail_save_work(&mut self, pending: PendingSave, error: String) {
        if let Some(close) = &pending.close {
            self.window_session.fail_editor_close_save(close);
        }
        if pending.stage {
            self.input.clear_pending_stage();
        }
        if pending.formatting {
            self.format_on_save.remove(&pending.buffer);
            self.documents
                .save_work
                .remaining_formatters
                .remove(&pending.buffer);
            self.documents.save_work.format_only.remove(&pending.buffer);
        }
        self.show_error(error);
    }

    fn finish_provider_save(&mut self, buffer: BufferId, pending: PendingSave) {
        self.notify_message("Saved".into(), false);
        if let Some(close) = pending.close {
            match self
                .window_session
                .finish_editor_close_save(&close, &(&*self.state).into())
            {
                Ok(step) => self.handle_editor_close_step(step),
                Err(error) => self.show_error(error),
            }
        }
        if pending.quit
            && self
                .state
                .buffers
                .get(buffer)
                .is_none_or(|document| !document.dirty)
        {
            self.process_effect(crate::app::commands::AppEffect::Quit { force: false });
        }
    }
}

#[cfg(test)]
mod tests;
