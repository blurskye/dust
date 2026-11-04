#[path = "launch.rs"]
mod launch;

pub(crate) const LSP_REQUEST_TIMEOUT_MS: u64 = 10_000;
pub(crate) const LSP_RESTART_LIMIT: u8 = 3;
pub(crate) const LSP_RESTART_BACKOFF_MS: u64 = 250;
pub(crate) const LSP_MAXIMUM_LOG_LINES: usize = 200;

pub(in crate::editor::session) use launch::{
    LaunchContext, LspLaunchRequest, LspLaunchSource, PendingLspProjectRoot,
};
#[path = "projection.rs"]
mod projection;
pub(in crate::editor::session) use projection::ProjectionChange;
// LSP runtime bookkeeping and accepted document lineage. No mutable editor authority.
use super::{
    lsp_edits::{
        CodeActionChoice, CodeActionKey, EditDocument, LspEditSources, OfferedAction, SourcePlan,
    },
    workspace_edits::{OriginKey, PreparedFiles},
};
use crate::{
    editor::{
        BufferId, EditorState, ViewId,
        text::{BufferRevision, TextVersion},
    },
    language::lsp::{
        ApplyEditReply, EditContext, LspCapabilities, LspCodeAction, LspDiagnostic, LspDocument,
        LspError, LspServerConfiguration, LspService, LspSessionLaunch, LspWorkspaceEdit,
        OwnedLspEvent,
    },
    tools::{ToolDefinition, nix_lsp_launch_environment_for_workspace_at, tool_launch_environment},
    workspace::{RootId, WorkspaceTrust},
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::editor::session) struct LspProjectionOwner {
    pub server: String,
    pub buffer: BufferId,
    pub view: ViewId,
    pub revision: BufferRevision,
    pub text: TextVersion,
    pub uri: String,
    pub language: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::editor::session) struct LspAutostartObservation {
    pub buffer: BufferId,
    pub path: PathBuf,
    pub language: String,
    pub root_revision: u64,
    pub trust: WorkspaceTrust,
    pub enabled: bool,
    pub auto_start: bool,
    pub nix_workspace_fallback: bool,
    pub launchable_server: Option<String>,
}
#[derive(Clone, Debug)]
pub(in crate::editor::session) enum LspLaunch {
    Direct {
        executable: PathBuf,
        managed: bool,
    },
    NixWorkspace {
        nix: PathBuf,
        workspace_root: PathBuf,
        shell: String,
    },
}
impl LspLaunch {
    pub(in crate::editor::session) fn executable(&self) -> &PathBuf {
        match self {
            Self::Direct { executable, .. } => executable,
            Self::NixWorkspace { nix, .. } => nix,
        }
    }
    pub(in crate::editor::session) fn arguments(&self, definition: &ToolDefinition) -> Vec<String> {
        definition.launch_args.clone()
    }
    pub(in crate::editor::session) fn environment(
        &self,
        definition: &ToolDefinition,
    ) -> Result<BTreeMap<String, String>, String> {
        match self {
            Self::Direct { managed: true, .. } => Ok(tool_launch_environment(definition)),
            Self::Direct { .. } => Ok(BTreeMap::new()),
            Self::NixWorkspace { workspace_root, .. } => {
                nix_lsp_launch_environment_for_workspace_at(workspace_root, definition)
            }
        }
    }
    pub(in crate::editor::session) fn with_preflighted_executable(
        self,
        executable: PathBuf,
    ) -> Self {
        match self {
            Self::Direct { managed, .. } => Self::Direct {
                executable,
                managed,
            },
            Self::NixWorkspace {
                workspace_root,
                shell,
                ..
            } => Self::NixWorkspace {
                nix: executable,
                workspace_root,
                shell,
            },
        }
    }
}
#[derive(Clone, Debug)]
pub(in crate::editor::session) struct LspRuntimeState {
    pub root: RootId,
    pub root_path: PathBuf,
    pub server_id: String,
    pub definition: ToolDefinition,
    pub launch: LspLaunch,
    pub capabilities: Option<LspCapabilities>,
    pub documents: HashMap<BufferId, LspDocument>,
    pub restart_count: u8,
    pub restart_due: Option<Instant>,
    pub circuit_open: bool,
    pub desired_running: bool,
    pub activate_on_restart: bool,
}
#[derive(Default)]
pub(super) struct LspSession {
    projection: projection::Projection,
    preparation: launch::Preparation,
    service: LspService,
    edits: LspEditSources,
    sessions: HashMap<String, LspRuntimeState>,
    active: Option<String>,
    synced: HashMap<(String, BufferId), TextVersion>,
    autostart: Option<LspAutostartObservation>,
    /// Languages already told which server to install, so the hint shows once per run.
    install_hinted: HashSet<String>,
    /// Servers a person stopped; opening a file doesn't start them until asked again.
    stopped: HashSet<String>,
    detached: HashSet<(String, BufferId)>,
    errors: BTreeMap<String, String>,
    pending_configurations: HashMap<String, LspServerConfiguration>,
    navigation_reads: HashMap<
        crate::files::RequestId,
        (
            crate::language::lsp::LspRange,
            crate::editor::text::PositionEncoding,
            crate::editor::session::windows::NavigationStamp,
            crate::editor::session::workspace::WorkspaceVersion,
        ),
    >,
    #[cfg(test)]
    fixture_capabilities: Option<LspCapabilities>,
}
impl LspSession {
    pub(in crate::editor::session) fn edit_context_current(&self, context: &EditContext) -> bool {
        self.service.edit_context_current(context)
    }
    pub(super) fn register_navigation_read(
        &mut self,
        id: crate::files::RequestId,
        range: crate::language::lsp::LspRange,
        encoding: crate::editor::text::PositionEncoding,
        navigation: crate::editor::session::windows::NavigationStamp,
        workspace: crate::editor::session::workspace::WorkspaceVersion,
    ) {
        self.navigation_reads
            .insert(id, (range, encoding, navigation, workspace));
    }
    pub(super) fn take_navigation_read(
        &mut self,
        id: crate::files::RequestId,
    ) -> Option<(
        crate::language::lsp::LspRange,
        crate::editor::text::PositionEncoding,
        crate::editor::session::windows::NavigationStamp,
        crate::editor::session::workspace::WorkspaceVersion,
    )> {
        self.navigation_reads.remove(&id)
    }
    pub(super) fn retire_document_request(&mut self, id: crate::files::RequestId) {
        self.navigation_reads.remove(&id);
    }
    pub(super) fn running_servers(&self) -> Vec<String> {
        self.service.running_servers()
    }
    pub(super) fn stop(&mut self, server: &str) -> Result<(), LspError> {
        self.service.stop(server)
    }
    pub(super) fn diagnostic_producer(
        &self,
        server: &str,
        prefix: &str,
    ) -> Option<crate::workspace::DiagnosticProducer> {
        self.service.diagnostic_producer(server, prefix)
    }
    pub(super) fn owns_event_context(&self, context: &EditContext) -> bool {
        self.service.owns_event_context(context)
    }
    pub(super) fn sync_owned(
        &mut self,
        server: &str,
        document: LspDocument,
        source: (BufferId, TextVersion),
    ) -> Result<(), LspError> {
        if !self.sessions.contains_key(server) {
            return Err(LspError::NotRunning(server.into()));
        }
        self.service
            .sync_owned(server, document.clone(), source.clone())?;
        self.document_synced(server, source.0, source.1, document);
        Ok(())
    }
    pub(super) fn close(&self, server: &str, uri: String) -> Result<(), LspError> {
        self.service.close(server, uri)
    }
    pub(super) fn explicitly_detached(&self, server: &str, buffer: BufferId) -> bool {
        self.detached.contains(&(server.to_owned(), buffer))
    }
    pub(super) fn clear_detached(&mut self, buffer: BufferId) {
        self.detached.retain(|(_, id)| *id != buffer);
    }
    pub(super) fn set_detached(&mut self, server: String, buffer: BufferId, detached: bool) {
        if detached {
            self.detached.insert((server, buffer));
        } else {
            self.detached.remove(&(server, buffer));
        }
    }
    pub(super) fn save(&self, server: &str, document: LspDocument) -> Result<(), LspError> {
        self.service.save(server, document)
    }
    pub(super) fn cancel(&self, server: &str, generation: u64) -> Result<(), LspError> {
        self.service.cancel(server, generation)
    }
    pub(super) fn configure(
        &mut self,
        server: &str,
        configuration: LspServerConfiguration,
    ) -> Result<(), LspError> {
        self.pending_configurations.remove(server);
        match self.service.configure(server, configuration.clone()) {
            Err(error @ LspError::Busy(_)) => {
                self.pending_configurations.insert(server.into(), configuration);
                Err(error)
            }
            result => result,
        }
    }
    pub(super) fn retry_configurations(&mut self) -> Vec<LspError> {
        let mut errors = Vec::new();
        for (server, configuration) in std::mem::take(&mut self.pending_configurations) {
            if let Err(error) = self.configure(&server, configuration)
                && !matches!(error, LspError::Busy(_))
            {
                errors.push(error);
            }
        }
        errors
    }
    pub(super) fn health(&self, server: &str) -> Result<(), LspError> {
        self.service.health(server)
    }
    pub(super) fn offer_actions(
        &mut self,
        server: &str,
        actions: Vec<LspCodeAction>,
        context: Option<EditContext>,
    ) {
        self.edits.offer_actions(server, actions, context);
    }
    pub(super) fn clear_offered_actions(&mut self) {
        self.edits.clear_offered_actions();
    }
    pub(super) fn has_offered_actions(&self) -> bool {
        self.edits.has_offered_actions()
    }
    pub(super) fn install_offered_actions(
        &mut self,
        maximum: usize,
    ) -> Result<Vec<CodeActionChoice>, String> {
        let first = self.active.as_deref();
        self.edits
            .install_offered_actions(first, &self.service, maximum)
    }
    pub(super) fn retain_actions<'a>(
        &mut self,
        displayed: impl Iterator<Item = &'a CodeActionKey>,
    ) {
        self.edits.retain_actions(displayed);
    }
    pub(super) fn clear_actions(&mut self) {
        self.edits.clear_actions();
    }
    pub(super) fn take_action(&mut self, key: &CodeActionKey) -> Result<OfferedAction, String> {
        self.edits.take_action(key, &self.service)
    }
    pub(super) fn poll_event(&self) -> Option<OwnedLspEvent> {
        self.edits.poll_event(&self.service)
    }
    pub(super) fn flush_edit_replies(&mut self) -> Vec<String> {
        self.edits.flush()
    }
    pub(super) fn pending_replies(&self) -> bool {
        self.edits.pending_replies()
    }
    pub(super) fn reject_reply(&mut self, reply: Option<ApplyEditReply>, reason: String) {
        self.edits.reject_reply(reply, reason);
    }
    pub(super) fn start_edit(
        &mut self,
        edit: LspWorkspaceEdit,
        encoding: crate::editor::text::PositionEncoding,
        context: Option<EditContext>,
        reply: Option<ApplyEditReply>,
        current: &[EditDocument<'_>],
    ) -> Result<SourcePlan, String> {
        self.edits
            .start(edit, encoding, context, reply, current, &self.service)
    }
    pub(super) fn check_edit(
        &self,
        key: &OriginKey,
        current: &[EditDocument<'_>],
    ) -> Result<(), String> {
        self.edits.check(key, current, &self.service)
    }
    pub(super) fn decode_edit(
        &mut self,
        key: &OriginKey,
        files: PreparedFiles<'_>,
    ) -> Result<Vec<crate::workspace::WorkspaceEditDraft>, String> {
        self.edits.decode(key, files)
    }
    pub(super) fn finish_edit(
        &mut self,
        key: &OriginKey,
        applied: bool,
        reason: Option<String>,
    ) -> Result<(), String> {
        let command = self.edits.take_command(key);
        self.edits.finish(key, applied, reason)?;
        if applied && let Some((server, command)) = command {
            let document = self
                .sessions
                .get(&server)
                .and_then(|session| session.documents.values().next())
                .cloned()
                .ok_or("code-action server is no longer attached")?;
            let generation = self
                .next_generation()
                .ok_or("language-server request identity exhausted")?;
            self.service
                .request(
                    &server,
                    crate::language::lsp::LspFeature::ExecuteCommand(command),
                    document,
                    crate::editor::text::ByteOffset(0),
                    generation,
                )
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    pub(super) fn set_edit_command(
        &mut self,
        server: String,
        command: crate::language::lsp::LspCommand,
    ) {
        self.edits.set_command(server, command);
    }
    pub(super) fn diagnostic_batch(
        &self,
        context: &EditContext,
        editor: &EditorState,
        publication: (&str, Option<u64>, Vec<LspDiagnostic>),
        policy: (bool, usize, &str),
    ) -> Result<super::diagnostics::DiagnosticBatch, String> {
        LspEditSources::diagnostic_batch(
            context,
            &self.service,
            editor,
            publication,
            policy.0,
            policy.1,
            policy.2,
        )
    }
    pub(super) fn sessions(&self) -> &HashMap<String, LspRuntimeState> {
        &self.sessions
    }
    pub(super) fn active(&self) -> Option<&String> {
        self.active.as_ref()
    }
    pub(super) fn capabilities(&self) -> Option<&LspCapabilities> {
        #[cfg(test)]
        if self.fixture_capabilities.is_some() {
            return self.fixture_capabilities.as_ref();
        }
        self.sessions
            .get(self.active.as_ref()?)?
            .capabilities
            .as_ref()
    }
    pub(super) fn errors(&self) -> &BTreeMap<String, String> {
        &self.errors
    }
    pub(super) fn failed(&mut self, server: String, error: String) {
        self.errors.insert(server, error);
    }
    pub(super) fn clear_error(&mut self, server: &str) {
        self.errors.remove(server);
    }
    pub(super) fn clear_active(&mut self) {
        self.active = None;
        #[cfg(test)]
        {
            self.fixture_capabilities = None;
        }
    }
    pub(super) fn start(
        &mut self,
        request: LspSessionLaunch<'_>,
        source: (BufferId, TextVersion),
        origin: (RootId, LspLaunch, bool),
        editor: &EditorState,
    ) -> Result<(), String> {
        let (buffer, version) = source;
        let document = request
            .document
            .as_ref()
            .ok_or("language-server launch has no document")?;
        if editor.buffers.get(buffer).is_none_or(|current| {
            current.text.source_version() != version
                || current.path.as_ref() != Some(&document.path)
                || current.text.revision().0 != document.version
        }) {
            return Err("language-server launch document source retired".into());
        }
        let key = request.session_key.to_owned();
        let definition = request.definition.clone();
        let root_path = request.root.clone();
        let document = document.clone();
        self.service
            .start_session_owned(request, Some((buffer, version.clone())))
            .map_err(|e| e.to_string())?;
        // No registry entry is installed unless the real transport admitted startup.
        let previous = self.sessions.remove(&key);
        let restart_count = previous.as_ref().map_or(0, |s| s.restart_count);
        // A server joining a document that already has one stays secondary.
        let primary_attached = self
            .active
            .as_ref()
            .and_then(|active| self.sessions.get(active))
            .is_some_and(|active| active.documents.contains_key(&buffer));
        let activate = previous.as_ref().map_or_else(
            || editor.active_buffer_id() == Some(buffer) && !primary_attached,
            |s| s.activate_on_restart,
        );
        let mut documents = previous.map_or_else(HashMap::new, |s| s.documents);
        documents.insert(buffer, document.clone());
        self.retire_projection_server(&key);
        self.forget_synced(&key);
        if origin.2 {
            for (id, replay) in &mut documents {
                if *id != buffer
                    && let Some(current) = editor
                        .buffers
                        .get(*id)
                        .filter(|b| b.path.as_ref() == Some(&replay.path))
                {
                    replay.version = current.text.revision().0;
                    replay.text = current.text.to_string();
                    let version = current.text.source_version();
                    if self
                        .service
                        .sync_owned(&key, replay.clone(), (*id, version.clone()))
                        .is_ok()
                    {
                        self.synced.insert((key.clone(), *id), version);
                    }
                }
            }
        }
        self.sessions.insert(
            key.clone(),
            LspRuntimeState {
                root: origin.0,
                root_path,
                server_id: definition.id.clone(),
                definition,
                launch: origin.1,
                capabilities: None,
                documents,
                restart_count,
                restart_due: None,
                circuit_open: false,
                desired_running: true,
                activate_on_restart: activate,
            },
        );
        self.synced.insert((key.clone(), buffer), version);
        if activate {
            self.select(&key);
        }
        Ok(())
    }
    pub(super) fn retry_documents(&mut self, editor: &EditorState) {
        for (key, session) in &mut self.sessions {
            if !session.desired_running || session.capabilities.is_none() {
                continue;
            }
            for (id, document) in &mut session.documents {
                let Some(buffer) = editor
                    .buffers
                    .get(*id)
                    .filter(|b| b.path.as_ref() == Some(&document.path))
                else {
                    continue;
                };
                let version = buffer.text.source_version();
                if self.synced.contains_key(&(key.clone(), *id)) {
                    continue;
                }
                document.text = buffer.text.to_string();
                document.version = buffer.text.revision().0;
                if self
                    .service
                    .sync_owned(key, document.clone(), (*id, version.clone()))
                    .is_ok()
                {
                    self.synced.insert((key.clone(), *id), version);
                }
            }
        }
    }
    pub(super) fn remove(&mut self, key: &str) {
        self.retire_projection_server(key);
        self.sessions.remove(key);
        self.forget_synced(key);
    }
    pub(super) fn select(&mut self, key: &str) -> bool {
        if !self.sessions.contains_key(key) {
            return false;
        }
        for (id, state) in &mut self.sessions {
            state.activate_on_restart = id == key;
        }
        self.active = Some(key.into());
        true
    }
    pub(super) fn running(&mut self, key: &str, capabilities: LspCapabilities) -> bool {
        let Some(session) = self.sessions.get_mut(key) else {
            return false;
        };
        self.errors.remove(&session.server_id);
        session.capabilities = Some(capabilities);
        self.active.as_deref() == Some(key)
    }
    pub(super) fn stopped(&mut self, key: &str) -> bool {
        self.retire_projection_server(key);
        self.forget_synced(key);
        self.sessions.get_mut(key).is_some_and(|state| {
            state.capabilities = None;
            state.desired_running
        })
    }
    pub(super) fn request_stop(&mut self, key: &str) {
        if let Some(state) = self.sessions.get_mut(key) {
            state.desired_running = false;
            state.restart_due = None;
        }
    }
    pub(super) fn request_start(&mut self, key: &str, restart: Option<Instant>) {
        if let Some(state) = self.sessions.get_mut(key) {
            state.restart_count = 0;
            state.restart_due = restart;
            state.circuit_open = false;
            state.desired_running = true;
        }
    }
    pub(super) fn schedule_restart(&mut self, key: &str, now: Instant) -> Option<String> {
        let state = self.sessions.get_mut(key)?;
        if !state.desired_running || state.restart_due.is_some() {
            return None;
        }
        if state.restart_count >= LSP_RESTART_LIMIT {
            state.circuit_open = true;
            return Some(state.server_id.clone());
        }
        state.restart_count = state.restart_count.saturating_add(1);
        let exponent = u32::from(state.restart_count.saturating_sub(1)).min(8);
        let delay = LSP_RESTART_BACKOFF_MS
            .saturating_mul(2_u64.saturating_pow(exponent))
            .min(60_000);
        state.restart_due = Some(now + Duration::from_millis(delay));
        None
    }
    pub(super) fn restart_due(&mut self, now: Instant) -> Option<String> {
        let (key, state) = self.sessions.iter_mut().find(|(_, state)| {
            state.desired_running
                && !state.circuit_open
                && state.restart_due.is_some_and(|due| due <= now)
        })?;
        state.restart_due = None;
        Some(key.clone())
    }
    pub(super) fn synced(&self, key: &str, buffer: BufferId, version: &TextVersion) -> bool {
        self.synced.get(&(key.into(), buffer)) == Some(version)
    }
    fn document_synced(
        &mut self,
        key: &str,
        buffer: BufferId,
        version: TextVersion,
        document: LspDocument,
    ) {
        if let Some(state) = self.sessions.get_mut(key) {
            state.documents.insert(buffer, document);
            self.synced.insert((key.into(), buffer), version);
        }
    }
    pub(super) fn invalidate_document(&mut self, key: &str, buffer: BufferId) {
        self.synced.remove(&(key.into(), buffer));
    }
    pub(super) fn forget_synced(&mut self, key: &str) {
        self.synced.retain(|(server, _), _| server != key);
    }
    pub(super) fn detach(&mut self, key: &str, buffer: BufferId) {
        self.invalidate_document(key, buffer);
        if let Some(state) = self.sessions.get_mut(key) {
            state.documents.remove(&buffer);
        }
    }
    pub(super) fn hint_install_once(&mut self, language: &str) -> bool {
        self.install_hinted.insert(language.to_owned())
    }
    pub(super) fn mark_stopped(&mut self, server_id: &str) {
        self.stopped.insert(server_id.to_owned());
    }
    pub(super) fn clear_stopped(&mut self, server_id: &str) {
        self.stopped.remove(server_id);
    }
    pub(super) fn clear_all_stopped(&mut self) {
        self.stopped.clear();
    }
    pub(super) fn is_stopped(&self, server_id: &str) -> bool {
        self.stopped.contains(server_id)
    }
    pub(super) fn reset_autostart(&mut self) {
        self.autostart = None;
    }
    /// Starts the next unattached server once per change in what the editor sees.
    pub(super) fn observe(
        &mut self,
        observation: LspAutostartObservation,
        safe_mode: bool,
    ) -> Option<String> {
        if self.autostart.as_ref() == Some(&observation) {
            return None;
        }
        let start = (observation.enabled
            && observation.auto_start
            && observation.trust == WorkspaceTrust::Trusted
            && !safe_mode)
            .then(|| observation.launchable_server.clone())
            .flatten();
        self.autostart = Some(observation);
        start
    }
}
#[cfg(test)]
impl LspSession {
    pub(super) fn fixture_transport(
        &mut self,
        key: &str,
        document: &LspDocument,
        source: (BufferId, TextVersion),
    ) -> crate::language::lsp::EditSourceFixture {
        self.service.replace_edit_source_fixture_for(
            key,
            document.uri(),
            source,
            crate::editor::text::PositionEncoding::Utf16,
            16,
        )
    }
    pub(super) fn fixture_session(&mut self, key: String, state: LspRuntimeState) {
        self.sessions.insert(key, state);
    }
    pub(super) fn fixture_capabilities(&mut self, capabilities: LspCapabilities) {
        self.fixture_capabilities = Some(capabilities);
    }
}
