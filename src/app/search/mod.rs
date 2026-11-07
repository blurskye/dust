//! Search requests, accepted results and picker lifetime.
//! Selected opaque actions share the lifetime and bounds of their actual rows.
use crate::app::picker;
use crate::app::search::workflow::ABS_MAX_BYTES;
use crate::app::search::workflow::SearchHostAction;
use crate::app::search::workflow::SearchMatcherKind;
use crate::app::search::workflow::SearchPresenterKind;
use crate::app::search::workflow::SearchRequestId;
use crate::app::search::workflow::SearchSourceKind;
use crate::app::search::workflow::SearchStreamId;
use crate::app::search::workflow::SearchWorkflowBroker;
use crate::app::search::workflow::SearchWorkflowChunk;
use crate::app::search::workflow::SearchWorkflowError;
use crate::app::search::workflow::SearchWorkflowId;
use crate::app::search::workflow::SearchWorkflowItem;
use crate::app::search::workflow::SearchWorkflowSpec;
use crate::app::workspace::WorkspaceVersion;
use crate::files::WorkspaceFileEntry;
use crate::files::WorkspaceFileEvent;
use crate::files::WorkspaceFilePolicy;
use crate::files::WorkspaceFileWorker;
use crate::files::WorkspaceSearchEvent;
use crate::files::WorkspaceSearchMatch;
use crate::files::WorkspaceSearchPolicy;
use crate::files::WorkspaceSearchWorker;

pub(in crate::app) const MAXIMUM_PICKER_ITEMS: usize = 10_000;
/// Bounds for walking the workspace in the file picker and project search.
pub(in crate::app) const SEARCH_MAXIMUM_FILES: usize = 100_000;
pub(in crate::app) const SEARCH_MAXIMUM_FILE_BYTES: usize = 8 * 1024 * 1024;
pub(in crate::app) const SEARCH_BATCH_SIZE: usize = 128;
pub(in crate::app) const SEARCH_QUEUE_CAPACITY: usize = 8;

type ByteNavigation = (crate::editor::text::ByteRange, Option<std::sync::Arc<[u8]>>);

#[derive(Default)]
pub(in crate::app) struct Search {
    pub(in crate::app) picker: picker::Picker,
    pub(in crate::app) preview: Option<crate::ui::PickerPreview>,
    pub(in crate::app) preview_snapshot_key:
        Option<(std::sync::Arc<[u8]>, std::ops::Range<usize>, usize)>,
    pub(in crate::app) preview_path: Option<std::path::PathBuf>,
    pub(in crate::app) preview_scope: Option<(WorkspaceVersion, u64)>,
    pub(in crate::app) preview_epoch: std::sync::Arc<()>,
    pub(in crate::app) preview_read: Option<(crate::files::RequestId, usize)>,
    files: WorkspaceFileWorker,
    file_stream_id: Option<u64>,
    pub(in crate::app) file_scope: Option<(WorkspaceVersion, u64)>,
    file_entries: Vec<WorkspaceFileEntry>,
    grep: WorkspaceSearchWorker,
    stream_id: Option<u64>,
    pub(in crate::app) scope: Option<(WorkspaceVersion, u64)>,
    workflow: Option<SearchWorkflowBroker>,
    sequence: u64,
    pub(in crate::app) matches: Vec<WorkspaceSearchMatch>,
    intent: Option<Intent>,
    pub(in crate::app) live: bool,
    pub(in crate::app) query: String,
    resume_selection: Option<crate::workspace::PickerItemId>,
    matcher_navigation: std::collections::HashMap<crate::files::RequestId, MatcherSource>,
    byte_navigation: std::collections::HashMap<crate::files::RequestId, ByteNavigation>,
    pub(in crate::app) preview_location: Option<(usize, usize)>,
    pub(in crate::app) diff_preview: crate::git::GitHunkWorker,
    pub(in crate::app) diff_snapshot: Option<crate::git::GitHunkSnapshot>,
    pub(in crate::app) diff_hunk: usize,
    pub(in crate::app) diff_preview_key: Option<(
        WorkspaceVersion,
        u64,
        std::path::PathBuf,
        crate::git::GitDiffLayer,
    )>,
    /// The icon a picker row of a file without its own icon gets, or `None` when the
    /// explorer draws no Nerd Font icons and the rows draw none either.
    pub(in crate::app) file_icon: Option<String>,
}
pub(in crate::app) enum Intent {
    Find,
    Replace(String),
}
pub(in crate::app) enum SearchUpdate {
    Batch,
    Complete {
        intent: Intent,
        files: usize,
        truncated: bool,
    },
    Failed(String),
}
impl Search {
    pub(in crate::app) fn register_matcher_navigation(
        &mut self,
        id: crate::files::RequestId,
        source: MatcherSource,
    ) {
        self.matcher_navigation.insert(id, source);
    }
    pub(in crate::app) fn take_matcher_navigation(
        &mut self,
        id: crate::files::RequestId,
    ) -> Option<MatcherSource> {
        self.matcher_navigation.remove(&id)
    }
    pub(in crate::app) fn register_byte_navigation(
        &mut self,
        id: crate::files::RequestId,
        range: crate::editor::text::ByteRange,
        snapshot: Option<std::sync::Arc<[u8]>>,
    ) {
        self.byte_navigation.insert(id, (range, snapshot));
    }
    pub(in crate::app) fn take_byte_navigation(
        &mut self,
        id: crate::files::RequestId,
    ) -> Option<ByteNavigation> {
        self.byte_navigation.remove(&id)
    }
    pub(in crate::app) fn forget_document_request(&mut self, id: crate::files::RequestId) {
        self.matcher_navigation.remove(&id);
        self.byte_navigation.remove(&id);
    }
    pub(in crate::app) fn live(&self) -> bool {
        self.live && !self.help_is_open()
    }
    pub(in crate::app) fn query(&self) -> &str {
        &self.query
    }
    pub(in crate::app) fn pending(&self) -> bool {
        self.stream_id.is_some()
    }
    pub(in crate::app) fn files_pending(&self) -> bool {
        self.file_stream_id.is_some()
    }
    pub(in crate::app) fn matches(&self) -> &[WorkspaceSearchMatch] {
        &self.matches
    }
    pub(in crate::app) fn workflow(&self) -> Option<&SearchWorkflowBroker> {
        self.workflow.as_ref()
    }
    pub(in crate::app) fn open_live(&mut self) {
        self.cancel();
        self.live = true;
        self.query.clear();
    }
    pub(in crate::app) fn set_file_icon(&mut self, icon: Option<String>) {
        self.file_icon = icon;
    }
    pub(in crate::app) fn set_query(&mut self, query: String) {
        if self.query != query {
            self.cancel_grep();
        }
        self.query = query;
    }
    pub(in crate::app) fn cancel_grep(&mut self) {
        self.grep.cancel();
        self.stream_id = None;
        self.scope = None;
        self.workflow = None;
        self.sequence = 0;
        self.matches.clear();
        self.resume_selection = None;
        self.intent = None;
    }
    pub(in crate::app) fn cancel_files(&mut self) {
        self.files.cancel();
        self.file_stream_id = None;
        self.file_scope = None;
        self.file_entries.clear();
    }
    pub(in crate::app) fn cancel(&mut self) {
        self.cancel_files();
        self.cancel_grep();
    }
    /// Move the current surface aside for a help page. Depth one by design: help is a page the
    /// user reads and leaves, not a place to nest, so a second help page replaces the snapshot
    /// rather than growing a stack.
    pub(in crate::app) fn suspend_for_help(&mut self) {
        self.picker.suspend_for_help();
    }
    /// Whether the surface on screen is a help page.
    pub(in crate::app) fn help_is_open(&self) -> bool {
        self.picker.help_is_open()
    }
    /// Mark the surface now on screen as a help page, so closing it resumes instead of closing.
    pub(in crate::app) fn mark_help_open(&mut self) {
        self.picker.mark_help_open();
    }
    pub(in crate::app) fn close_picker(&mut self) -> Option<crate::files::RequestId> {
        // Closing a help page returns the user to the surface help was opened over, with its
        // query, rows and selected row intact. Closing anything else behaves exactly as before.
        if self.picker.resume_after_help() {
            return None;
        }
        if self.picker_surface() == crate::ui::PickerSurfaceKind::Grep || self.live {
            self.resume_selection = self
                .picker()
                .and_then(|picker| picker.selected_item())
                .map(|item| item.id.clone());
            // Keep accepted rows and snapshots, but never let a closed picker publish late batches.
            self.grep.cancel();
            self.stream_id = None;
            self.intent = None;
            self.live = false;
        } else if self.resume_selection.is_none() && !self.pending() {
            self.cancel_grep();
            self.query.clear();
        }
        self.clear_picker();
        self.cancel_files();
        self.clear_preview()
    }
    pub(in crate::app) fn start_files(
        &mut self,
        policy: WorkspaceFilePolicy,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> Result<(), String> {
        let stream_id = self.files.start(policy).map_err(|e| e.to_string())?;
        self.file_stream_id = Some(stream_id);
        self.file_scope = Some((scope, configuration));
        self.file_entries.clear();
        Ok(())
    }
    pub(in crate::app) fn poll_files(
        &mut self,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> Option<Result<Vec<WorkspaceFileEntry>, String>> {
        if self
            .file_scope
            .as_ref()
            .is_some_and(|expected| expected != &(scope, configuration))
        {
            self.cancel_files();
            return None;
        }
        let mut changed = false;
        for _ in 0..256 {
            self.file_stream_id?;
            let event = match self.files.poll_event() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    self.cancel_files();
                    return Some(Err(error.into()));
                }
            };
            match event {
                WorkspaceFileEvent::Batch {
                    stream_id, entries, ..
                } if Some(stream_id) == self.file_stream_id => {
                    self.file_entries.extend(entries);
                    changed = true;
                }
                WorkspaceFileEvent::Complete { stream_id, .. }
                    if Some(stream_id) == self.file_stream_id =>
                {
                    self.file_stream_id = None;
                    self.files.cancel();
                    return Some(Ok(std::mem::take(&mut self.file_entries)));
                }
                WorkspaceFileEvent::Failed {
                    stream_id, error, ..
                } if Some(stream_id) == self.file_stream_id => {
                    self.cancel_files();
                    return Some(Err(error));
                }
                WorkspaceFileEvent::Cancelled { stream_id, .. }
                    if Some(stream_id) == self.file_stream_id =>
                {
                    self.cancel_files();
                    return None;
                }
                _ => {}
            }
        }
        changed.then(|| Ok(self.file_entries.clone()))
    }
    pub(in crate::app) fn start(
        &mut self,
        policy: WorkspaceSearchPolicy,
        scope: WorkspaceVersion,
        configuration: u64,
        replacement: Option<String>,
    ) -> Result<(), String> {
        let mut workflow = SearchWorkflowBroker::try_new(SearchWorkflowSpec {
            id: SearchWorkflowId(0),
            source: SearchSourceKind::Grep,
            matcher: SearchMatcherKind::Literal,
            presenter: SearchPresenterKind::Picker,
            maximum_items: policy.maximum_results.max(1),
            maximum_bytes: usize::try_from(policy.maximum_total_bytes)
                .unwrap_or(ABS_MAX_BYTES)
                .clamp(1, ABS_MAX_BYTES),
        })
        .map_err(|e| e.to_string())?;
        let query = policy.query.clone();
        let stream_id = self.grep.start(policy).map_err(|e| e.to_string())?;
        workflow
            .begin(SearchRequestId(stream_id), SearchStreamId(stream_id), "")
            .map_err(|e| e.to_string())?;
        self.query = query;
        self.stream_id = Some(stream_id);
        self.scope = Some((scope, configuration));
        self.workflow = Some(workflow);
        self.sequence = 0;
        self.matches.clear();
        self.intent = Some(replacement.map_or(Intent::Find, Intent::Replace));
        Ok(())
    }
    pub(in crate::app) fn poll(
        &mut self,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> Vec<SearchUpdate> {
        if self
            .scope
            .as_ref()
            .is_some_and(|expected| expected != &(scope, configuration))
        {
            self.cancel_grep();
            return Vec::new();
        }
        let mut updates = Vec::new();
        for _ in 0..256 {
            if self.stream_id.is_none() {
                break;
            }
            let event = match self.grep.poll_event() {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(error) => {
                    self.cancel_grep();
                    updates.push(SearchUpdate::Failed(error.into()));
                    break;
                }
            };
            let update = self.accept(event);
            if self.workflow.is_none() || update.is_some() {
                // The caller projects the accepted store, not intermediate copies.
                // Keep only its latest disposition within this bounded drain.
                updates.clear();
            }
            if let Some(update) = update {
                updates.push(update);
            }
        }
        updates
    }
    pub(in crate::app) fn accept(&mut self, event: WorkspaceSearchEvent) -> Option<SearchUpdate> {
        let stream_id = match &event {
            WorkspaceSearchEvent::Batch { stream_id, .. }
            | WorkspaceSearchEvent::Complete { stream_id, .. }
            | WorkspaceSearchEvent::Failed { stream_id, .. }
            | WorkspaceSearchEvent::Cancelled { stream_id } => *stream_id,
        };
        if self.stream_id != Some(stream_id) {
            return None;
        }
        match event {
            WorkspaceSearchEvent::Batch { matches, .. } => {
                let items: Vec<_> = matches
                    .iter()
                    .map(|found| SearchWorkflowItem {
                        stable_key: found.id.clone(),
                        title: found.relative_path.display().to_string(),
                        detail: found.line_text.clone(),
                        search_text: format!(
                            "{} {}",
                            found.relative_path.display(),
                            found.line_text
                        ),
                        action: SearchHostAction::OpenLocation {
                            uri: crate::language::lsp::file_uri(&found.canonical_path),
                        },
                    })
                    .collect();
                match self.accept_chunk(stream_id, items.clone(), false) {
                    Ok(()) => self.matches.extend(matches),
                    Err(SearchWorkflowError::Limit) => {
                        // A batch is atomic. Keep the prefix that still fits before ending
                        // the search as truncated, just like the worker's own limits.
                        for (found, item) in matches.into_iter().zip(items) {
                            match self.accept_chunk(stream_id, vec![item], false) {
                                Ok(()) => self.matches.push(found),
                                Err(SearchWorkflowError::Limit) => {
                                    return self.accept(WorkspaceSearchEvent::Complete {
                                        stream_id,
                                        summary: crate::files::WorkspaceSearchSummary {
                                            truncated: true,
                                            ..Default::default()
                                        },
                                    });
                                }
                                Err(error) => {
                                    self.cancel_grep();
                                    return Some(SearchUpdate::Failed(error.to_string()));
                                }
                            }
                        }
                    }
                    Err(error) => {
                        self.cancel_grep();
                        return Some(SearchUpdate::Failed(error.to_string()));
                    }
                }
                Some(SearchUpdate::Batch)
            }
            WorkspaceSearchEvent::Complete { summary, .. } => {
                if self.accept_chunk(stream_id, Vec::new(), true).is_err() {
                    self.cancel_grep();
                    return Some(SearchUpdate::Failed(
                        "workspace search workflow rejected completion".into(),
                    ));
                }
                self.stream_id = None;
                self.grep.cancel();
                self.intent.take().map(|intent| SearchUpdate::Complete {
                    intent,
                    files: summary.files_searched,
                    truncated: summary.truncated,
                })
            }
            WorkspaceSearchEvent::Failed { error, .. } => {
                self.cancel_grep();
                Some(SearchUpdate::Failed(error))
            }
            WorkspaceSearchEvent::Cancelled { .. } => {
                self.cancel_grep();
                None
            }
        }
    }
    fn accept_chunk(
        &mut self,
        stream_id: u64,
        items: Vec<SearchWorkflowItem>,
        complete: bool,
    ) -> Result<(), SearchWorkflowError> {
        let next = self
            .sequence
            .checked_add(1)
            .ok_or(SearchWorkflowError::Limit)?;
        self.workflow
            .as_mut()
            .ok_or(SearchWorkflowError::Stale)?
            .accept(SearchWorkflowChunk {
                request: SearchRequestId(stream_id),
                stream_id: SearchStreamId(stream_id),
                sequence: self.sequence,
                items,
                complete,
            })?;
        if !complete {
            self.sequence = next;
        }
        Ok(())
    }
    #[cfg(test)]
    pub(in crate::app) fn sequence(&self) -> u64 {
        self.sequence
    }
    #[cfg(test)]
    pub(in crate::app) fn file_requests(&self) -> u64 {
        self.files.stream_id()
    }
}

pub(in crate::app) mod commands;

use crate::app::picker::MatcherSource;
pub(in crate::app) mod workflow;

use crate::app::App;
use crate::app::picker::PickerOrigin;
use crate::app::picker::PickerPayload;
use crate::editor::DocumentSnapshot as FileSnapshot;
use crate::editor::EditorOperation;
use crate::editor::EditorTransaction;
use crate::editor::Precondition;
use crate::editor::UndoPolicy;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::PositionEncoding;
use crate::editor::text::TextEdit;
use crate::language::lsp::file_uri;
use crate::ui::PickerSurfaceKind;
use crate::ui::PromptKind;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;
use crate::workspace::WorkspaceEditDraft;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::PathBuf;

impl App {
    pub(in crate::app) fn open_live_workspace_search(&mut self) {
        self.close_picker();
        self.search.open_live();
        self.install_surface_picker_state(
            PickerSurfaceKind::Grep,
            self.message("workspace_search_title", &[]),
            Vec::new(),
            String::new(),
            true,
            PickerOrigin::Grep,
        );
    }
}

impl App {
    pub(in crate::app) fn resume_workspace_search(&mut self) {
        if self.search.scope.as_ref()
            != Some(&(
                self.workspace.version(),
                self.configuration.binding_revision(),
            ))
            || self.search.workflow().is_none()
        {
            self.show_error("No workspace search to resume in this workspace");
            return;
        }
        self.close_picker();
        let selected = self.search.resume_selection.clone();
        self.search.live = true;
        self.open_workspace_search_picker();
        if let Some(selected) = selected {
            self.search.select_picker_item(&selected);
        }
        self.sync_picker_ui();
    }
}

impl App {
    pub(in crate::app) fn restart_live_workspace_search(&mut self, query: String) {
        self.search.set_query(query.clone());
        if query.is_empty() {
            self.install_surface_picker_state(
                PickerSurfaceKind::Grep,
                self.message("workspace_search_title", &[]),
                Vec::new(),
                query,
                true,
                PickerOrigin::Grep,
            );
        } else {
            self.start_workspace_search(query, None);
        }
    }
}

impl App {
    pub(in crate::app) fn start_workspace_file_picker(&mut self) {
        self.close_picker();
        let configured = &self.configuration.settings().workspace.search;
        let root_revision = self.workspace.root_revision();
        let policy = WorkspaceFilePolicy {
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|r| r.identity.canonical_path.clone())
                .collect(),
            root_revision,
            include_hidden: configured.include_hidden,
            include_ignored: configured.include_ignored,
            maximum_files: SEARCH_MAXIMUM_FILES,
            maximum_path_bytes: 16 * 1024 * 1024,
            batch_size: SEARCH_BATCH_SIZE,
            event_queue_capacity: SEARCH_QUEUE_CAPACITY,
        };
        match self.search.start_files(
            policy,
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            Ok(()) => self.install_surface_picker_state(
                PickerSurfaceKind::Files,
                "Files".into(),
                Vec::new(),
                String::new(),
                false,
                PickerOrigin::Files,
            ),
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn process_workspace_file_events(&mut self) {
        if self.search.help_is_open() {
            return;
        }
        let Some(result) = self.search.poll_files(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) else {
            return;
        };
        let entries = match result {
            Ok(entries) => entries,
            Err(error) => {
                self.close_picker();
                self.show_error(error);
                return;
            }
        };
        let multi_root = self.workspace.roots().len() > 1;
        let items = entries
            .into_iter()
            .map(|entry| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(entry.id),
                        label: entry.relative_path.display().to_string(),
                        detail: String::new(),
                        source: if multi_root {
                            entry.root.file_name().map_or_else(String::new, |name| {
                                name.to_string_lossy().into_owned()
                            })
                        } else {
                            String::new()
                        },
                        action: PickerAction::None,
                    },
                    PickerPayload::MatcherSource(MatcherSource {
                        path: entry.canonical_path,
                        line: 0,
                        column: 0,
                        encoding: PositionEncoding::Utf8,
                    }),
                )
            })
            .collect();
        self.install_surface_picker_state(
            PickerSurfaceKind::Files,
            "Files".into(),
            items,
            String::new(),
            !self.search.files_pending(),
            PickerOrigin::Files,
        );
    }
}

impl App {
    /// `<leader>st`: every TODO-style comment keyword in the workspace, as todo-comments.nvim.
    pub(in crate::app) fn start_todo_search(&mut self) {
        self.start_search(
            r"\b(TODO|FIXME|FIX|HACK|BUG|PERF|WARN|WARNING|NOTE|XXX)\b".to_owned(),
            None,
            true,
        );
    }
}

impl App {
    pub(in crate::app) fn start_workspace_search(
        &mut self,
        query: String,
        replacement: Option<String>,
    ) {
        self.start_search(query, replacement, false);
    }
}

impl App {
    pub(in crate::app) fn start_search(
        &mut self,
        query: String,
        replacement: Option<String>,
        regex: bool,
    ) {
        let configured = &self.configuration.settings().workspace.search;
        let policy = WorkspaceSearchPolicy {
            roots: self
                .workspace
                .roots()
                .iter()
                .map(|root| root.identity.canonical_path.clone())
                .collect(),
            query,
            regex,
            smart_case: true,
            whole_word: false,
            include_globs: Vec::new(),
            exclude_globs: configured.exclude_globs.clone(),
            include_hidden: configured.include_hidden,
            include_ignored: configured.include_ignored,
            maximum_files: SEARCH_MAXIMUM_FILES,
            maximum_results: 10_000,
            maximum_file_bytes: SEARCH_MAXIMUM_FILE_BYTES as u64,
            maximum_total_bytes: 512 * 1024 * 1024,
            maximum_preview_bytes: 4096,
            batch_size: SEARCH_BATCH_SIZE,
            event_queue_capacity: SEARCH_QUEUE_CAPACITY,
        };
        match self.search.start(
            policy,
            self.workspace.version(),
            self.configuration.binding_revision(),
            replacement,
        ) {
            Ok(()) => {
                if self.search.live() {
                    self.install_surface_picker_state(
                        PickerSurfaceKind::Grep,
                        self.message("workspace_search_title", &[]),
                        Vec::new(),
                        self.search.query().into(),
                        false,
                        PickerOrigin::Grep,
                    );
                    self.clear_notice();
                } else {
                    self.close_picker();
                    self.notify_message(self.message("workspace_search_started", &[]), false);
                }
            }
            Err(error) => {
                self.show_error(self.message("workspace_search_failed", &[("diagnostic", error)]))
            }
        }
    }
}

impl App {
    pub(in crate::app) fn process_workspace_search_events(&mut self) {
        if self.search.help_is_open() {
            return;
        }
        for update in self.search.poll(
            self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            match update {
                SearchUpdate::Batch => {
                    if self.search.live() {
                        self.open_workspace_search_picker();
                    }
                }
                SearchUpdate::Failed(error) => {
                    self.close_picker();
                    self.show_error(
                        self.message("workspace_search_failed", &[("diagnostic", error)]),
                    );
                }
                SearchUpdate::Complete {
                    intent: Intent::Find,
                    files,
                    truncated,
                } => {
                    if self.search.live() || !self.search.matches().is_empty() {
                        self.open_workspace_search_picker();
                        if truncated {
                            self.notify_message(
                                format!(
                                    "Workspace search was truncated; showing {} matches",
                                    self.search.matches().len()
                                ),
                                false,
                            );
                        } else if self.search.live() {
                            self.clear_notice();
                        } else {
                            self.notify_message(
                                self.message(
                                    "workspace_search_complete",
                                    &[
                                        ("count", self.search.matches().len().to_string()),
                                        ("files", files.to_string()),
                                    ],
                                ),
                                false,
                            );
                        }
                    } else {
                        self.show_error(self.message("workspace_search_empty", &[]));
                    }
                }
                SearchUpdate::Complete {
                    intent: Intent::Replace(replacement),
                    truncated,
                    ..
                } => {
                    if truncated {
                        self.show_error(
                            "Workspace search was truncated; replacement was not offered",
                        );
                    } else if self.search.matches().is_empty() {
                        self.show_error(self.message("workspace_search_empty", &[]));
                    } else {
                        self.begin_workspace_search_replacement(replacement);
                    }
                }
            }
        }
    }
}

impl App {
    pub(in crate::app) fn open_workspace_search_picker(&mut self) {
        let Some(order) = self
            .search
            .workflow()
            .and_then(|broker| broker.ordered_items())
        else {
            self.show_error("workspace search workflow projection is unavailable");
            return;
        };
        let matches = self
            .search
            .matches()
            .iter()
            .map(|found| (found.id.as_str(), found))
            .collect::<BTreeMap<_, _>>();
        let multi_root = self.workspace.roots().len() > 1;
        let items = order
            .into_iter()
            .filter_map(|item| matches.get(item.stable_key.as_str()).copied())
            .map(|found| {
                let item = PickerItem {
                    badge: String::new(),
                    kind: crate::workspace::PickerItemKind::Entry,
                    id: PickerItemId(found.id.clone()),
                    label: self.message(
                        "workspace_search_row",
                        &[
                            ("path", found.relative_path.display().to_string()),
                            ("line", found.line.saturating_add(1).to_string()),
                            ("column", found.column.saturating_add(1).to_string()),
                        ],
                    ),
                    detail: found.line_text.trim().to_owned(),
                    source: if multi_root {
                        found
                            .root
                            .file_name()
                            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
                    } else {
                        String::new()
                    },
                    action: PickerAction::None,
                };
                (item, PickerPayload::WorkspaceSearch(found.id.clone()))
            })
            .collect();
        if self.search.live() {
            self.install_surface_picker_state(
                PickerSurfaceKind::Grep,
                self.message("workspace_search_title", &[]),
                items,
                self.search.query().into(),
                !self.search.pending(),
                PickerOrigin::Grep,
            );
        } else {
            self.install_surface_picker_state(
                PickerSurfaceKind::Grep,
                self.message("workspace_search_title", &[]),
                items,
                String::new(),
                true,
                PickerOrigin::Grep,
            );
        }
    }
}

impl App {
    pub(in crate::app) fn begin_workspace_search_replacement(&mut self, replacement: String) {
        if self.workspace_edits.preparing() || self.workspace_edits.proposal().is_some() {
            self.show_error(self.message("workspace_edit_busy", &[]));
            return;
        }
        let mut grouped =
            BTreeMap::<PathBuf, (std::sync::Arc<[u8]>, Vec<TextEdit>, HashSet<*const [u8]>)>::new();
        for found in self.search.matches() {
            let entry = grouped
                .entry(found.canonical_path.clone())
                .or_insert_with(|| (found.snapshot.clone(), Vec::new(), HashSet::new()));
            // Search matches normally share one snapshot. Compare each allocation only once.
            if entry.2.insert(std::sync::Arc::as_ptr(&found.snapshot))
                && !std::sync::Arc::ptr_eq(&entry.0, &found.snapshot)
                && entry.0.as_ref() != found.snapshot.as_ref()
            {
                self.show_error(self.message("workspace_search_stale_result", &[]));
                return;
            }
            let Ok(range) =
                ByteRange::new(ByteOffset(found.range.start), ByteOffset(found.range.end))
            else {
                self.show_error(self.message("workspace_search_stale_result", &[]));
                return;
            };
            entry.1.push(TextEdit::new(range, replacement.clone()));
        }
        let mut snapshots = Vec::with_capacity(grouped.len());
        let mut drafts = Vec::with_capacity(grouped.len());
        for (path, (searched_bytes, edits, _)) in grouped {
            let revision = if let Some((_, buffer)) = self
                .state
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
            {
                if buffer.text.to_bytes().as_slice() != searched_bytes.as_ref() {
                    self.show_error(self.message("workspace_search_stale_result", &[]));
                    return;
                }
                Some(buffer.text.revision())
            } else {
                None
            };
            snapshots.push(FileSnapshot {
                canonical_path: path.clone(),
                revision,
                bytes: searched_bytes.to_vec(),
            });
            drafts.push(WorkspaceEditDraft {
                canonical_path: path,
                expected_revision: revision,
                edits,
            });
        }
        let id = match self.state.workspace_edit_sources().and_then(|source| {
            self.workspace_edits.begin(
                self.workspace.version(),
                self.configuration.binding_revision(),
                source,
            )
        }) {
            Ok(id) => id,
            Err(error) => {
                self.show_error(error);
                return;
            }
        };
        match self.workspace_edits.build(
            crate::app::workspace::edits::PreparedEdits {
                ticket: id,
                label: self.message("workspace_replace_label", &[]),
                drafts,
                snapshots,
                resources: Vec::new(),
            },
            self.workspace.edit_scope(),
            &self.workspace.version(),
            self.configuration.binding_revision(),
        ) {
            Ok(()) => {
                self.show_workspace_proposal();
            }
            Err(error) => {
                self.workspace_edits.reject();
                self.show_error(self.message("workspace_edit_failed", &[("diagnostic", error)]));
            }
        }
    }
}

impl App {
    pub(in crate::app) fn search_word_under_cursor(&mut self, forward: bool, count: u64) -> bool {
        let Some(view_id) = self.state.active_view() else {
            return false;
        };
        let view = &self.state.views[view_id];
        let source = self.state.buffers[view.buffer].text.to_string();
        let cursor = view.cursor.get();
        let word = |grapheme: &str| grapheme.chars().any(|c| c.is_alphanumeric() || c == '_');
        if !source[cursor..].graphemes(true).next().is_some_and(word) {
            self.show_error(self.message("cursor_not_word", &[]));
            return false;
        }
        let start = source[..cursor]
            .grapheme_indices(true)
            .rev()
            .take_while(|(_, grapheme)| word(grapheme))
            .last()
            .map_or(cursor, |(index, _)| index);
        let end = source[cursor..]
            .grapheme_indices(true)
            .find(|(_, grapheme)| !word(grapheme))
            .map_or(source.len(), |(relative, _)| cursor + relative);
        self.ui.search_query = Some(source[start..end].to_owned());
        self.input.remember_search(forward);
        self.input.remember_word_search();
        self.ui.search_whole_word = true;
        self.perform_search(forward, count)
    }
}

impl App {
    /// As with Vim's incsearch: while a `/` pattern is typed the cursor sits on its next match,
    /// and it goes back when the prompt closes.
    pub(in crate::app) fn preview_search(&mut self) {
        let Some(prompt) = self.input.prompt().filter(|prompt| {
            matches!(
                prompt.kind,
                PromptKind::SearchForward | PromptKind::SearchBackward
            )
        }) else {
            return;
        };
        let forward = prompt.kind == PromptKind::SearchForward;
        let query = prompt.input.clone();
        let Some(view_id) = self.state.active_view() else {
            return;
        };
        let buffer_id = self.state.views[view_id].buffer;
        if self.ui.large_file_buffers.contains(&buffer_id) {
            return;
        }
        let origin = self
            .input
            .search_origin(view_id, &self.state.views[view_id]);
        let search = &self.configuration.settings().search;
        let buffer = &self.state.buffers[buffer_id];
        let matches = crate::editor::input::search_matches(
            &buffer.text.to_string(),
            &query,
            search.smart_case,
            false,
        )
        .unwrap_or_default();
        let target = search_target(&buffer.text, &matches, origin, forward, search.wrap);
        self.move_search_cursor(view_id, target.map_or(origin, ByteOffset));
    }
}

impl App {
    pub(in crate::app) fn move_search_cursor(
        &mut self,
        view: crate::editor::ViewId,
        position: ByteOffset,
    ) {
        if !self.state.views.contains_key(view) {
            return;
        }
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_operation(EditorOperation::SetCursor { view, position })
            .with_undo_policy(UndoPolicy::Ignore);
        if self.state.apply_transaction(&transaction).is_ok() {
            self.ensure_cursor_visible();
        }
    }
}

impl App {
    pub(in crate::app) fn perform_search(&mut self, forward: bool, count: u64) -> bool {
        let Some(query) = self.ui.search_query.clone() else {
            self.show_error(self.message("no_previous_search", &[]));
            return false;
        };
        self.record_jump();
        let Some(view_id) = self.state.active_view() else {
            return false;
        };
        let view = &self.state.views[view_id];
        let buffer_id = view.buffer;
        let cursor_offset = view.cursor;
        let buffer = &self.state.buffers[buffer_id];
        let revision = buffer.text.revision();
        let source = buffer.text.to_string();
        let mut start = if forward && cursor_offset.get() < buffer.text.len_bytes() {
            cursor_offset.get()
                + source[cursor_offset.get()..]
                    .graphemes(true)
                    .next()
                    .map_or(0, str::len)
        } else {
            cursor_offset.get()
        };
        let mut found = Ok(None);
        for _ in 0..count {
            found = crate::editor::input::search_match(
                &source,
                &query,
                self.configuration.settings().search.smart_case,
                self.input.whole_word_search(),
                start,
                forward,
                self.configuration.settings().search.wrap,
            );
            let Ok(Some(matched)) = &found else { break };
            start = matched.start
                + if forward {
                    source[matched.start..]
                        .graphemes(true)
                        .next()
                        .map_or(0, str::len)
                } else {
                    0
                };
        }
        let target = match found {
            Ok(Some(found)) => found.start,
            Ok(None) => {
                self.show_error(self.message("pattern_not_found", &[("query", query)]));
                return false;
            }
            Err(error) => {
                self.show_error(error);
                return false;
            }
        };
        let transaction = EditorTransaction::new(self.transaction_id())
            .with_precondition(Precondition::BufferRevision {
                buffer: buffer_id,
                revision,
            })
            .with_operation(EditorOperation::SetCursor {
                view: view_id,
                position: ByteOffset(target),
            })
            .with_undo_policy(UndoPolicy::Ignore);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
            false
        } else {
            self.ensure_cursor_visible();
            true
        }
    }
}

impl App {
    /// `gn`: selects the search match under the cursor, else the next one, as Vim does.
    /// Returns false, having said why, when there is none.
    pub(in crate::app) fn select_next_search_match(&mut self, count: u64) -> bool {
        let Some(query) = self.ui.search_query.clone() else {
            self.show_error(self.message("no_previous_search", &[]));
            return false;
        };
        let Some(view_id) = self.state.active_view() else {
            return false;
        };
        let view = &self.state.views[view_id];
        let cursor = view.cursor.get();
        let buffer = &self.state.buffers[view.buffer];
        let source = buffer.text.to_string();
        let search = &self.configuration.settings().search;
        let whole_word = self.input.whole_word_search();
        let found = crate::editor::input::search_match(
            &source,
            &query,
            search.smart_case,
            whole_word,
            cursor.saturating_add(1),
            false,
            false,
        )
        .and_then(|previous| {
            if let Some(found) = previous.filter(|found| found.end > cursor) {
                Ok(Some(found))
            } else {
                crate::editor::input::search_match(
                    &source,
                    &query,
                    search.smart_case,
                    whole_word,
                    cursor,
                    true,
                    search.wrap,
                )
            }
        });
        let found = match found {
            Ok(Some(found)) => found,
            Ok(None) => {
                self.show_error(self.message("pattern_not_found", &[("query", query)]));
                return false;
            }
            Err(error) => {
                self.show_error(error);
                return false;
            }
        };
        let mut found = found;
        for _ in 1..count {
            match crate::editor::input::search_match(
                &source,
                &query,
                search.smart_case,
                whole_word,
                found.end,
                true,
                search.wrap,
            ) {
                Ok(Some(next)) => found = next,
                Ok(None) => {
                    self.show_error(self.message("pattern_not_found", &[("query", query)]));
                    return false;
                }
                Err(error) => {
                    self.show_error(error);
                    return false;
                }
            }
        }
        self.select_text(view_id, found.start, found.end)
    }
}

impl App {
    pub(in crate::app) fn navigate_workspace_search_match(&mut self, found: WorkspaceSearchMatch) {
        if let Some(buffer) = self
            .state
            .buffers
            .values()
            .find(|buffer| buffer.path.as_ref() == Some(&found.canonical_path))
            && !buffer.text.matches_encoded_text(&found.snapshot)
        {
            self.show_error(self.message("workspace_search_stale_result", &[]));
            return;
        }
        let Ok(range) = ByteRange::new(ByteOffset(found.range.start), ByteOffset(found.range.end))
        else {
            self.show_error(self.message("workspace_search_stale_result", &[]));
            return;
        };
        self.record_jump();
        if self
            .state
            .buffers
            .values()
            .any(|buffer| buffer.path.as_ref() == Some(&found.canonical_path))
        {
            self.navigate_byte_location(&file_uri(&found.canonical_path), range);
            return;
        }
        let roots = self
            .workspace
            .roots()
            .iter()
            .map(|root| root.identity.canonical_path.clone())
            .collect();
        match self.documents.read_scoped(found.canonical_path, roots) {
            Ok(id) => {
                self.remember_navigation_read(id);
                self.search
                    .register_byte_navigation(id, range, Some(found.snapshot));
            }
            Err(error) => self.show_error(error),
        }
    }
}

use crate::editor::text::TextBuffer;

/// A character of an identifier, for the word completion replaces and filters on.
/// Where a search from `cursor` lands, as a byte offset.
fn search_target(
    text: &TextBuffer,
    matches: &[std::ops::Range<usize>],
    cursor: ByteOffset,
    forward: bool,
    wrap: bool,
) -> Option<usize> {
    let offset = cursor.get().min(text.len_bytes());
    if forward {
        let start = if offset < text.len_bytes() {
            crate::editor::text::next_grapheme_boundary(text, cursor).get()
        } else {
            offset
        };
        matches
            .iter()
            .find(|matched| matched.start >= start)
            .or_else(|| wrap.then(|| matches.first()).flatten())
            .map(|matched| matched.start)
    } else {
        matches
            .iter()
            .rev()
            .find(|matched| matched.start < offset)
            .or_else(|| wrap.then(|| matches.last()).flatten())
            .map(|matched| matched.start)
    }
}
use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
pub(in crate::app) mod tests;
