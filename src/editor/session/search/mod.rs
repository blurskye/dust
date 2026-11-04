//! Search worker admission, accepted results and interaction retirement.
//! Selected opaque actions share the lifetime and bounds of their actual rows.
mod picker;
mod preview;
use super::workspace::WorkspaceVersion;
use crate::files::{
    WorkspaceFileEntry, WorkspaceFileEvent, WorkspaceFilePolicy, WorkspaceFileService,
    WorkspaceSearchEvent, WorkspaceSearchMatch, WorkspaceSearchPolicy, WorkspaceSearchService,
};
use crate::workspace::search_workflow::{
    ABS_MAX_BYTES, SearchHostAction, SearchMatcherKind, SearchPresenterKind, SearchRequestId,
    SearchSourceKind, SearchStreamGeneration, SearchWorkflowBroker, SearchWorkflowChunk,
    SearchWorkflowError, SearchWorkflowId, SearchWorkflowItem, SearchWorkflowSpec,
};

pub(crate) const MAXIMUM_PICKER_ITEMS: usize = 10_000;
/// Bounds for walking the workspace in the file picker and project search.
pub(crate) const SEARCH_MAXIMUM_FILES: usize = 100_000;
pub(crate) const SEARCH_MAXIMUM_FILE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const SEARCH_BATCH_SIZE: usize = 128;
pub(crate) const SEARCH_QUEUE_CAPACITY: usize = 8;

pub(in crate::editor::session) use picker::{
    MatcherSource, PickerContent, PickerOrigin, PickerPayload,
};

#[derive(Default)]
pub(in crate::editor::session) struct SearchSession {
    picker: picker::PickerSession,
    preview: Option<crate::ui::PickerPreview>,
    preview_snapshot_key: Option<(std::sync::Arc<[u8]>, std::ops::Range<usize>, usize)>,
    preview_path: Option<std::path::PathBuf>,
    preview_scope: Option<(WorkspaceVersion, u64)>,
    preview_epoch: std::sync::Arc<()>,
    preview_read: Option<(crate::files::RequestId, usize)>,
    files: WorkspaceFileService,
    file_generation: Option<u64>,
    file_scope: Option<(WorkspaceVersion, u64)>,
    file_entries: Vec<WorkspaceFileEntry>,
    grep: WorkspaceSearchService,
    generation: Option<u64>,
    scope: Option<(WorkspaceVersion, u64)>,
    workflow: Option<SearchWorkflowBroker>,
    sequence: u64,
    matches: Vec<WorkspaceSearchMatch>,
    intent: Option<Intent>,
    live: bool,
    query: String,
    matcher_navigation: std::collections::HashMap<crate::files::RequestId, MatcherSource>,
    workspace_navigation:
        std::collections::HashMap<crate::files::RequestId, WorkspaceSearchMatch>,
}
pub(in crate::editor::session) enum Intent {
    Find,
    Replace(String),
}
pub(in crate::editor::session) enum SearchUpdate {
    Batch,
    Complete {
        intent: Intent,
        files: usize,
        truncated: bool,
    },
    Failed(String),
}
impl SearchSession {
    pub(in crate::editor::session) fn register_matcher_navigation(
        &mut self,
        id: crate::files::RequestId,
        source: MatcherSource,
    ) {
        self.matcher_navigation.insert(id, source);
    }
    pub(in crate::editor::session) fn take_matcher_navigation(
        &mut self,
        id: crate::files::RequestId,
    ) -> Option<MatcherSource> {
        self.matcher_navigation.remove(&id)
    }
    pub(in crate::editor::session) fn register_workspace_navigation(
        &mut self,
        id: crate::files::RequestId,
        found: WorkspaceSearchMatch,
    ) {
        self.workspace_navigation.insert(id, found);
    }
    pub(in crate::editor::session) fn take_workspace_navigation(
        &mut self,
        id: crate::files::RequestId,
    ) -> Option<WorkspaceSearchMatch> {
        self.workspace_navigation.remove(&id)
    }
    pub(in crate::editor::session) fn retire_document_request(
        &mut self,
        id: crate::files::RequestId,
    ) {
        self.matcher_navigation.remove(&id);
        self.workspace_navigation.remove(&id);
    }
    pub(in crate::editor::session) fn live(&self) -> bool {
        self.live && !self.help_is_open()
    }
    pub(in crate::editor::session) fn query(&self) -> &str {
        &self.query
    }
    pub(in crate::editor::session) fn pending(&self) -> bool {
        self.generation.is_some()
    }
    pub(in crate::editor::session) fn files_pending(&self) -> bool {
        self.file_generation.is_some()
    }
    pub(in crate::editor::session) fn matches(&self) -> &[WorkspaceSearchMatch] {
        &self.matches
    }
    pub(in crate::editor::session) fn workflow(&self) -> Option<&SearchWorkflowBroker> {
        self.workflow.as_ref()
    }
    pub(in crate::editor::session) fn open_live(&mut self) {
        self.cancel();
        self.live = true;
        self.query.clear();
    }
    pub(in crate::editor::session) fn set_query(&mut self, query: String) {
        if self.query != query {
            self.cancel_grep();
        }
        self.query = query;
    }
    pub(in crate::editor::session) fn cancel_grep(&mut self) {
        self.grep.cancel();
        self.generation = None;
        self.scope = None;
        self.workflow = None;
        self.sequence = 0;
        self.matches.clear();
        self.intent = None;
    }
    fn cancel_files(&mut self) {
        self.files.cancel();
        self.file_generation = None;
        self.file_scope = None;
        self.file_entries.clear();
    }
    pub(in crate::editor::session) fn cancel(&mut self) {
        self.cancel_files();
        self.cancel_grep();
    }
    /// Move the current surface aside for a help page. Depth one by design: help is a page the
    /// user reads and leaves, not a place to nest, so a second help page replaces the snapshot
    /// rather than growing a stack.
    pub(in crate::editor::session) fn suspend_for_help(&mut self) {
        self.picker.suspend_for_help();
    }
    /// Whether the surface on screen is a help page.
    pub(in crate::editor::session) fn help_is_open(&self) -> bool {
        self.picker.help_is_open()
    }
    /// Mark the surface now on screen as a help page, so closing it resumes instead of closing.
    pub(in crate::editor::session) fn mark_help_open(&mut self) {
        self.picker.mark_help_open();
    }
    pub(in crate::editor::session) fn close_picker(&mut self) -> Option<crate::files::RequestId> {
        // Closing a help page returns the user to the surface help was opened over, with its
        // query, rows and selected row intact. Closing anything else behaves exactly as before.
        if self.picker.resume_after_help() {
            return None;
        }
        self.retire_picker();
        self.cancel_files();
        if self.live || !self.pending() {
            self.cancel_grep();
            self.live = false;
            self.query.clear();
        }
        self.clear_preview()
    }
    pub(in crate::editor::session) fn start_files(
        &mut self,
        policy: WorkspaceFilePolicy,
        scope: WorkspaceVersion,
        configuration: u64,
    ) -> Result<(), String> {
        let generation = self.files.start(policy).map_err(|e| e.to_string())?;
        self.file_generation = Some(generation);
        self.file_scope = Some((scope, configuration));
        self.file_entries.clear();
        Ok(())
    }
    pub(in crate::editor::session) fn poll_files(
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
            self.file_generation?;
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
                    generation,
                    entries,
                    ..
                } if Some(generation) == self.file_generation => {
                    self.file_entries.extend(entries);
                    changed = true;
                }
                WorkspaceFileEvent::Complete { generation, .. }
                    if Some(generation) == self.file_generation =>
                {
                    self.file_generation = None;
                    self.files.cancel();
                    return Some(Ok(std::mem::take(&mut self.file_entries)));
                }
                WorkspaceFileEvent::Failed {
                    generation, error, ..
                } if Some(generation) == self.file_generation => {
                    self.cancel_files();
                    return Some(Err(error));
                }
                WorkspaceFileEvent::Cancelled { generation, .. }
                    if Some(generation) == self.file_generation =>
                {
                    self.cancel_files();
                    return None;
                }
                _ => {}
            }
        }
        changed.then(|| Ok(self.file_entries.clone()))
    }
    pub(in crate::editor::session) fn start(
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
        let generation = self.grep.start(policy).map_err(|e| e.to_string())?;
        workflow
            .begin(
                SearchRequestId(generation),
                SearchStreamGeneration(generation),
                "",
            )
            .map_err(|e| e.to_string())?;
        self.query = query;
        self.generation = Some(generation);
        self.scope = Some((scope, configuration));
        self.workflow = Some(workflow);
        self.sequence = 0;
        self.matches.clear();
        self.intent = Some(replacement.map_or(Intent::Find, Intent::Replace));
        Ok(())
    }
    pub(in crate::editor::session) fn poll(
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
            if self.generation.is_none() {
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
    fn accept(&mut self, event: WorkspaceSearchEvent) -> Option<SearchUpdate> {
        let generation = match &event {
            WorkspaceSearchEvent::Batch { generation, .. }
            | WorkspaceSearchEvent::Complete { generation, .. }
            | WorkspaceSearchEvent::Failed { generation, .. }
            | WorkspaceSearchEvent::Cancelled { generation } => *generation,
        };
        if self.generation != Some(generation) {
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
                match self.accept_chunk(generation, items.clone(), false) {
                    Ok(()) => self.matches.extend(matches),
                    Err(SearchWorkflowError::Limit) => {
                        // A batch is atomic. Keep the prefix that still fits before ending
                        // the search as truncated, just like the worker's own limits.
                        for (found, item) in matches.into_iter().zip(items) {
                            match self.accept_chunk(generation, vec![item], false) {
                                Ok(()) => self.matches.push(found),
                                Err(SearchWorkflowError::Limit) => {
                                    return self.accept(WorkspaceSearchEvent::Complete {
                                        generation,
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
                if self.accept_chunk(generation, Vec::new(), true).is_err() {
                    self.cancel_grep();
                    return Some(SearchUpdate::Failed(
                        "workspace search workflow rejected completion".into(),
                    ));
                }
                self.generation = None;
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
        generation: u64,
        items: Vec<SearchWorkflowItem>,
        complete: bool,
    ) -> Result<(), SearchWorkflowError> {
        let next = self.sequence.checked_add(1).ok_or(SearchWorkflowError::Limit)?;
        self.workflow
            .as_mut()
            .ok_or(SearchWorkflowError::Stale)?
            .accept(SearchWorkflowChunk {
                request: SearchRequestId(generation),
                generation: SearchStreamGeneration(generation),
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
    pub(in crate::editor::session) fn sequence(&self) -> u64 {
        self.sequence
    }
    #[cfg(test)]
    pub(in crate::editor::session) fn file_requests(&self) -> u64 {
        self.files.generation()
    }
}

#[cfg(test)]
mod tests;
