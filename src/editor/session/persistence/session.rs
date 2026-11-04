//! Session history and source-bound restore obligations.
use super::PersistenceSession;
use crate::{
    editor::{
        EditorDraft, EditorState, EditorVersion,
        session::{
            input::InputSession,
            windows::{NavigationStamp, WindowSession},
            workspace::{WorkspaceSession, WorkspaceVersion},
        },
        text::TextBuffer,
    },
    files::{RequestId, VfsResult},
    persistence::{
        PersistedContentDescriptor, PersistedLayout, PersistedLocation, PersistedMark,
        PersistedPane, PersistedSession, PersistedState, PersistedTab, PersistedWindowRule,
        validate_content_descriptors, validate_window_rules,
    },
};
use std::ops::ControlFlow::{self, Break, Continue};
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
};

pub(in crate::editor::session) struct RestoreContext<'a> {
    pub editor: &'a EditorState,
    pub workspace: &'a WorkspaceSession,
    pub input: &'a InputSession,
    pub windows: &'a WindowSession,
    pub configuration: u64,
}
pub(super) struct RestoreSource {
    editor: EditorVersion,
    workspace: WorkspaceVersion,
    capture: Arc<()>,
    navigation: NavigationStamp,
    configuration: u64,
}
impl RestoreSource {
    fn capture(context: &RestoreContext<'_>) -> Self {
        Self {
            editor: context.editor.version(),
            workspace: context.workspace.version(),
            capture: context.input.capture_epoch(),
            navigation: context
                .windows
                .navigation_stamp(context.editor.window_version(), context.editor.active_view),
            configuration: context.configuration,
        }
    }
    fn current(&self, context: &RestoreContext<'_>) -> bool {
        self.editor == context.editor.version()
            && context
                .windows
                .check_editor_admission(&context.editor.into())
                .is_ok()
            && self.workspace == context.workspace.version()
            && self.configuration == context.configuration
            && !context.workspace.safe_mode()
            && context.input.capture_current(&self.capture)
            && context.windows.navigation_current(
                &self.navigation,
                &context.editor.window_version(),
                context.editor.active_view,
            )
    }
}
pub(super) struct PendingRestore {
    source: RestoreSource,
    session: PersistedSession,
    remaining: BTreeSet<PathBuf>,
    requests: HashMap<RequestId, PathBuf>,
    loaded: HashMap<PathBuf, (PathBuf, TextBuffer)>,
}
pub(in crate::editor::session) struct RestoreRead<'a> {
    restore: &'a mut PendingRestore,
    path: PathBuf,
}
impl RestoreRead<'_> {
    pub(in crate::editor::session) fn register(self, id: RequestId) -> Result<(), String> {
        if self.restore.requests.contains_key(&id) {
            return Err("session restore read identity reused".into());
        }
        self.restore.remaining.remove(&self.path);
        self.restore.requests.insert(id, self.path);
        Ok(())
    }
}
pub(in crate::editor::session) struct RestorePrepared {
    pub draft: EditorDraft,
    pub paths: Vec<PathBuf>,
    pub window_rules: Vec<PersistedWindowRule>,
    pub content_descriptors: Vec<PersistedContentDescriptor>,
    pub pinned: Vec<PathBuf>,
}

pub(in crate::editor::session) struct RestoreReady {
    source: RestoreSource,
    session: PersistedSession,
    loaded: HashMap<PathBuf, (PathBuf, TextBuffer)>,
}
impl RestoreReady {
    pub(in crate::editor::session) fn prepare(
        self,
        context: RestoreContext<'_>,
    ) -> Result<RestorePrepared, String> {
        if !self.source.current(&context) {
            return Err("session restore source changed".into());
        }
        let mut draft = context.editor.draft();
        draft.restore_layout(&self.session, &self.loaded)?;
        // Files come back in the order their windows had, which the buffer line follows.
        let mut panes = Vec::new();
        for tab in &self.session.tabs {
            collect_persisted_panes(&tab.layout, &mut panes);
        }
        let mut paths = Vec::new();
        for pane in panes {
            if let Some((path, _)) = self.loaded.get(&pane.path)
                && !paths.contains(path)
            {
                paths.push(path.clone());
            }
        }
        Ok(RestorePrepared {
            draft,
            paths,
            window_rules: self.session.window_rules,
            content_descriptors: self.session.content_descriptors,
            pinned: self.session.pinned,
        })
    }
}
pub(in crate::editor::session) enum RestoreUpdate {
    Waiting,
    Ready(Box<RestoreReady>),
    /// The user started working while the files were read; their work stays.
    Abandoned,
    Failed(String),
}
impl PersistenceSession {
    /// Startup contributions must wait until the saved layout has been published.
    pub(in crate::editor::session) fn restoring(&self) -> bool {
        self.restore.is_some()
            || self.load_source.is_some() && self.pending.is_some_and(|(_, saving)| !saving)
    }

    pub(in crate::editor::session) fn load_session(
        &mut self,
        policy: crate::persistence::PersistencePolicy,
        context: RestoreContext<'_>,
        restore_allowed: bool,
    ) -> Result<(), String> {
        self.load(policy)?;
        self.load_source = restore_allowed.then(|| RestoreSource::capture(&context));
        Ok(())
    }
    /// Let the next `begin_restore` replace what is open, for a session kept in memory, as
    /// `load_session` does for the one read from disk.
    pub(in crate::editor::session) fn allow_restore(&mut self, context: RestoreContext<'_>) {
        self.load_source = Some(RestoreSource::capture(&context));
    }
    pub(in crate::editor::session) fn begin_restore(
        &mut self,
        session: PersistedSession,
        context: RestoreContext<'_>,
        maximum: usize,
    ) -> Result<Option<Vec<PathBuf>>, String> {
        if self.restore.is_some() || self.closing {
            return Err("session restoration is already pending or closing".into());
        }
        validate_window_rules(&session.window_rules).map_err(|error| error.to_string())?;
        validate_content_descriptors(&session, maximum).map_err(|error| error.to_string())?;
        let Some(source) = self.load_source.take() else {
            return Ok(None);
        };
        // The user started working before the saved session was read: keep what they have.
        if !source.current(&context) || context.editor.buffers.values().any(|buffer| buffer.dirty) {
            return Ok(None);
        }
        let mut panes = Vec::new();
        for tab in &session.tabs {
            collect_persisted_panes(&tab.layout, &mut panes);
        }
        let paths = panes
            .into_iter()
            .map(|pane| pane.path)
            .collect::<BTreeSet<_>>();
        if paths.len() > maximum
            || paths.iter().any(|path| {
                !path.is_absolute()
                    || path
                        .components()
                        .any(|part| matches!(part, std::path::Component::ParentDir))
                    || !context
                        .workspace
                        .roots()
                        .iter()
                        .any(|root| path.starts_with(&root.identity.canonical_path))
            })
        {
            return Err("session restore paths exceed policy or leave the workspace".into());
        }
        if paths.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let result = paths.iter().cloned().collect();
        self.restore = Some(PendingRestore {
            source,
            session,
            remaining: paths,
            requests: HashMap::new(),
            loaded: HashMap::new(),
        });
        Ok(Some(result))
    }
    pub(in crate::editor::session) fn restore_read(
        &mut self,
        path: &std::path::Path,
        context: RestoreContext<'_>,
    ) -> Option<RestoreRead<'_>> {
        let restore = self.restore.as_mut()?;
        if !restore.source.current(&context) || !restore.remaining.contains(path) {
            return None;
        }
        Some(RestoreRead {
            restore,
            path: path.to_path_buf(),
        })
    }
    pub(in crate::editor::session) fn cancel_restore(&mut self) -> Vec<RequestId> {
        let ids = self
            .restore
            .take()
            .map(|restore| restore.requests.into_keys().collect::<Vec<_>>())
            .unwrap_or_default();
        self.retired_reads.extend(ids.iter().copied());
        ids
    }
    pub(in crate::editor::session) fn restore_transport_stopped(&mut self) {
        self.restore = None;
        self.retired_reads.clear();
    }
    pub(in crate::editor::session) fn accept_restore(
        &mut self,
        event: VfsResult,
        context: RestoreContext<'_>,
    ) -> ControlFlow<RestoreUpdate, VfsResult> {
        let id = event.request_id();
        if self.retired_reads.remove(&id) {
            return Break(RestoreUpdate::Waiting);
        }
        let Some(restore) = self
            .restore
            .as_ref()
            .filter(|restore| restore.requests.contains_key(&id))
        else {
            return Continue(event);
        };
        let VfsResult::Read { path, result, .. } = event else {
            return Continue(event);
        };
        let current = !self.closing && restore.source.current(&context);
        let requested = self
            .restore
            .as_mut()
            .expect("owned read")
            .requests
            .remove(&id)
            .expect("owned read identity");
        if !current {
            return Break(RestoreUpdate::Abandoned);
        }
        if !context
            .workspace
            .roots()
            .iter()
            .any(|root| path.starts_with(&root.identity.canonical_path))
        {
            return Break(RestoreUpdate::Failed(
                "session restore read left its workspace".into(),
            ));
        }
        let text = match result
            .map_err(|error| error.to_string())
            .and_then(|bytes| TextBuffer::from_bytes(&bytes).map_err(|error| error.to_string()))
        {
            Ok(text) => text,
            Err(error) => return Break(RestoreUpdate::Failed(error)),
        };
        let restore = self.restore.as_mut().expect("checked restore source");
        restore.loaded.insert(requested, (path, text));
        if !restore.remaining.is_empty() || !restore.requests.is_empty() {
            return Break(RestoreUpdate::Waiting);
        }
        let restore = self.restore.take().expect("completed restore");
        Break(RestoreUpdate::Ready(Box::new(RestoreReady {
            source: restore.source,
            session: restore.session,
            loaded: restore.loaded,
        })))
    }
    pub(in crate::editor::session) fn recent_files(&self) -> &[PathBuf] {
        &self.recent_files
    }
    pub(in crate::editor::session) fn marks(&self) -> &[PersistedMark] {
        &self.marks
    }
    pub(in crate::editor::session) fn jumps(&self) -> &[PersistedLocation] {
        &self.jumps
    }
    pub(in crate::editor::session) fn changes(&self) -> &[PersistedLocation] {
        &self.changes
    }
    pub(in crate::editor::session) fn record_recent(&mut self, path: PathBuf, maximum: usize) {
        self.recent_files.retain(|previous| previous != &path);
        self.recent_files.insert(0, path);
        self.recent_files.truncate(maximum);
    }
    pub(in crate::editor::session) fn reconcile_marks(&mut self, editor: &EditorState) {
        self.mark_sources
            .retain(|name, _| self.marks.iter().any(|mark| &mark.name == name));
        for mark in &mut self.marks {
            let Some((id, buffer)) = editor
                .buffers
                .iter()
                .find(|(_, buffer)| buffer.path.as_ref() == Some(&mark.location.path))
            else {
                self.mark_sources.remove(&mark.name);
                continue;
            };
            let version = buffer.text.source_version();
            if let Some((previous_id, previous, revision)) = self.mark_sources.get(&mark.name)
                && *previous_id == id
                && previous.same_incarnation(&version)
                && let Some(mapped) = buffer.text.map_offset_since(
                    *revision,
                    crate::editor::text::ByteOffset(mark.location.byte),
                )
            {
                mark.location.byte = mapped.get();
                if let Some(anchor) = buffer.text.map_offset_since(
                    *revision,
                    crate::editor::text::ByteOffset(mark.location.anchor),
                ) {
                    mark.location.anchor = anchor.get();
                }
            }
            self.mark_sources
                .insert(mark.name.clone(), (id, version, buffer.text.revision()));
        }
    }

    pub(in crate::editor::session) fn set_mark(
        &mut self,
        name: String,
        location: PersistedLocation,
        maximum: usize,
    ) {
        if name.len() > 64 || name.chars().any(char::is_control) {
            return;
        }
        self.mark_sources.remove(&name);
        self.marks.retain(|mark| mark.name != name);
        self.marks.push(PersistedMark { name, location });
        let extra = self.marks.len().saturating_sub(maximum);
        self.marks.drain(..extra);
    }
    pub(in crate::editor::session) fn record_location(
        &mut self,
        location: PersistedLocation,
        change: bool,
        maximum: usize,
    ) {
        let list = if change {
            &mut self.changes
        } else {
            &mut self.jumps
        };
        if change
            && list.last().is_some_and(|previous| {
                previous.path == location.path && previous.byte == location.byte
            })
        {
            return;
        }
        if !change {
            // A new jump ends a walk with `ctrl-o`, and a place is only kept once, as in Vim.
            self.jump_index = None;
            list.retain(|previous| {
                previous.path != location.path || previous.byte != location.byte
            });
        }
        list.push(location);
        let extra = list.len().saturating_sub(maximum);
        list.drain(..extra);
    }
    /// One step of Vim's `ctrl-o` (`back`) or `ctrl-i` through the jump list. The first step
    /// back records where the cursor is, so `ctrl-i` can return to it.
    pub(in crate::editor::session) fn walk_jumps(
        &mut self,
        current: Option<PersistedLocation>,
        back: bool,
        maximum: usize,
    ) -> Option<PersistedLocation> {
        let index = match self.jump_index {
            Some(index) => index,
            None if back => {
                if let Some(current) = current {
                    self.record_location(current, false, maximum);
                    self.jumps.len().checked_sub(1)?
                } else {
                    self.jumps.len()
                }
            }
            None => return None,
        };
        let next = if back {
            index.checked_sub(1)?
        } else {
            index + 1
        };
        let location = self.jumps.get(next)?.clone();
        self.jump_index = Some(next);
        Some(location)
    }
    pub(in crate::editor::session) fn restore_history(&mut self, state: &mut PersistedState) {
        // Loaded history is older than edits made while the read was in flight.
        self.jump_index = None;
        let mut seen = std::collections::HashSet::new();
        self.recent_files = std::mem::take(&mut self.recent_files)
            .into_iter()
            .chain(std::mem::take(&mut state.recent_files))
            .filter(|path| seen.insert(path.clone()))
            .take(super::RECENT_FILE_LIMIT)
            .collect();
        let mut seen = std::collections::HashSet::new();
        self.marks = std::mem::take(&mut self.marks)
            .into_iter()
            .rev()
            .chain(std::mem::take(&mut state.marks).into_iter().rev())
            .filter(|mark| seen.insert(mark.name.clone()))
            .take(super::MARK_LIMIT)
            .collect();
        self.marks.reverse();
        for (current, mut past, maximum) in [
            (
                &mut self.jumps,
                std::mem::take(&mut state.jumps),
                super::JUMP_LIMIT,
            ),
            (
                &mut self.changes,
                std::mem::take(&mut state.changes),
                super::CHANGE_LIMIT,
            ),
        ] {
            past.append(current);
            *current = past;
            let extra = current.len().saturating_sub(maximum);
            current.drain(..extra);
        }
    }
    pub(in crate::editor::session) fn capture_session(
        &self,
        state: &EditorState,
        workspace: &WorkspaceSession,
        windows: &WindowSession,
        rules: Vec<PersistedWindowRule>,
        virtual_sources: &[(crate::editor::BufferId, String, String, String)],
    ) -> Result<Option<PersistedSession>, String> {
        let mut tabs = Vec::new();
        let mut descriptors = Vec::new();
        let mut active_tab = 0;
        for (id, tab) in &state.tabs {
            let Some(layout) = capture_persisted_layout(state, &tab.root) else {
                continue;
            };
            if Some(id) == state.active_tab {
                active_tab = tabs.len();
            }
            let tab_index = tabs.len();
            let mut panes = Vec::new();
            collect_persistable_editor_panes(state, &tab.root, &mut panes);
            let focused_path = panes
                .iter()
                .find(|(pane, _)| *pane == tab.focused)
                .map(|(_, path)| path.clone());
            for (pane_index, (pane, path)) in panes.into_iter().enumerate() {
                descriptors.push(PersistedContentDescriptor {
                    semantic_id: format!("text:{tab_index}:{pane_index}"),
                    content_type: "builtin.text-buffer".into(),
                    adapter: "text-buffer".into(),
                    path: Some(path),
                    provider: None,
                    source: None,
                    key: None,
                    desired_placement: windows.persisted_editor_placement(
                        pane,
                        crate::ui::window::ContentAdapterKind::TextBuffer,
                    )?,
                });
            }
            tabs.push(PersistedTab {
                layout,
                focused_path,
            });
        }

        let sources = virtual_sources
            .iter()
            .map(|(buffer, provider, source, key)| (*buffer, (provider, source, key)))
            .collect::<HashMap<_, _>>();
        let mut virtual_index = 0usize;
        for (_, tab) in &state.tabs {
            let mut panes = Vec::new();
            collect_editor_panes(&tab.root, &mut panes);
            for pane in panes {
                let Some(view) = state
                    .panes
                    .get(pane)
                    .and_then(|pane| state.views.get(pane.view))
                else {
                    continue;
                };
                let Some((provider, source, key)) = sources.get(&view.buffer) else {
                    continue;
                };
                descriptors.push(PersistedContentDescriptor {
                    semantic_id: format!("virtual:{virtual_index}"),
                    content_type: "builtin.virtual-document".into(),
                    adapter: "virtual-document".into(),
                    path: None,
                    provider: Some((*provider).clone()),
                    source: Some((*source).clone()),
                    key: Some((*key).clone()),
                    desired_placement: windows.persisted_editor_placement(
                        pane,
                        crate::ui::window::ContentAdapterKind::VirtualDocument,
                    )?,
                });
                virtual_index = virtual_index
                    .checked_add(1)
                    .ok_or("virtual descriptor identity is exhausted")?;
            }
        }

        Ok(
            (!tabs.is_empty() || !descriptors.is_empty()).then(|| PersistedSession {
                roots: workspace
                    .roots()
                    .iter()
                    .map(|root| root.identity.canonical_path.clone())
                    .collect(),
                tabs,
                active_tab,
                window_rules: rules,
                content_descriptors: descriptors,
                pinned: windows
                    .pins()
                    .iter()
                    .filter_map(|buffer| state.buffers.get(*buffer)?.path.clone())
                    .collect(),
            }),
        )
    }
}
#[cfg(test)]
impl PersistenceSession {
    pub(in crate::editor::session) fn fixture_restore_source(
        &mut self,
        context: RestoreContext<'_>,
    ) {
        self.load_source = Some(RestoreSource::capture(&context));
    }
}
fn collect_editor_panes(
    layout: &crate::editor::LayoutNode,
    panes: &mut Vec<crate::editor::PaneId>,
) {
    match layout {
        crate::editor::LayoutNode::Pane(pane) => panes.push(*pane),
        crate::editor::LayoutNode::Split { first, second, .. } => {
            collect_editor_panes(first, panes);
            collect_editor_panes(second, panes);
        }
    }
}

fn collect_persistable_editor_panes(
    state: &EditorState,
    layout: &crate::editor::LayoutNode,
    panes: &mut Vec<(crate::editor::PaneId, PathBuf)>,
) {
    match layout {
        crate::editor::LayoutNode::Pane(pane) => {
            if let Some(path) = state
                .panes
                .get(*pane)
                .and_then(|pane| state.views.get(pane.view))
                .and_then(|view| state.buffers.get(view.buffer))
                .filter(|buffer| {
                    !buffer.dirty
                        && !(buffer.read_only
                            && buffer
                                .path
                                .as_ref()
                                .is_some_and(|path| !path.starts_with(&state.workspace_root)))
                })
                .and_then(|buffer| buffer.path.clone())
            {
                panes.push((*pane, path));
            }
        }
        crate::editor::LayoutNode::Split { first, second, .. } => {
            collect_persistable_editor_panes(state, first, panes);
            collect_persistable_editor_panes(state, second, panes);
        }
    }
}

fn collect_persisted_panes(layout: &PersistedLayout, panes: &mut Vec<PersistedPane>) {
    match layout {
        PersistedLayout::Pane { pane } => panes.push(pane.clone()),
        PersistedLayout::Split { first, second, .. } => {
            collect_persisted_panes(first, panes);
            collect_persisted_panes(second, panes);
        }
    }
}

pub(in crate::editor::session) fn capture_persisted_layout(
    state: &EditorState,
    layout: &crate::editor::LayoutNode,
) -> Option<PersistedLayout> {
    match layout {
        crate::editor::LayoutNode::Pane(pane) => {
            let pane = state.panes.get(*pane)?;
            let view = state.views.get(pane.view)?;
            let buffer = state.buffers.get(view.buffer)?;
            if buffer.dirty {
                return None;
            }
            let path = buffer.path.clone()?;
            if buffer.read_only && !path.starts_with(&state.workspace_root) {
                return None;
            }
            Some(PersistedLayout::Pane {
                pane: PersistedPane {
                    path,
                    cursor: view.cursor.get(),
                    anchor: view
                        .selection
                        .map_or(view.cursor, |selection| selection.anchor)
                        .get(),
                    scroll_line: view.scroll_line,
                    scroll_column: view.scroll_column,
                },
            })
        }
        crate::editor::LayoutNode::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let first = capture_persisted_layout(state, first);
            let second = capture_persisted_layout(state, second);
            match (first, second) {
                (Some(first), Some(second)) => Some(PersistedLayout::Split {
                    axis: match axis {
                        crate::editor::Axis::Horizontal => "horizontal",
                        crate::editor::Axis::Vertical => "vertical",
                    }
                    .into(),
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(layout), None) | (None, Some(layout)) => Some(layout),
                (None, None) => None,
            }
        }
    }
}
