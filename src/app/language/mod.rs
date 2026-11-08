//! Language-server requests and editor results.

pub(in crate::app) mod incoming;
mod inlay;
mod launch;

pub(in crate::app) const LSP_REQUEST_TIMEOUT_MS: u64 = 10_000;
pub(in crate::app) const LSP_RESTART_LIMIT: u8 = 3;
pub(in crate::app) const LSP_RESTART_BACKOFF_MS: u64 = 250;
pub(in crate::app) const LSP_MAXIMUM_LOG_LINES: usize = 200;

pub(in crate::app) use launch::LaunchContext;

pub(in crate::app) use launch::LspLaunchRequest;

pub(in crate::app) use launch::LspLaunchSource;

pub(in crate::app) use launch::PendingLspProjectRoot;
mod projection;
pub(in crate::app) use projection::ProjectionChange;
// LSP runtime bookkeeping and accepted document lineage. No mutable editor authority.
use crate::app::language::edits::CodeActionChoice;
use crate::app::language::edits::CodeActionKey;
use crate::app::language::edits::EditDocument;
use crate::app::language::edits::LspEditSources;
use crate::app::language::edits::OfferedAction;
use crate::app::language::edits::SourcePlan;
use crate::app::workspace::edits::OriginKey;
use crate::app::workspace::edits::PreparedFiles;
use crate::editor::BufferId;
use crate::editor::EditorState;
use crate::editor::ViewId;
use crate::editor::text::BufferRevision;
use crate::editor::text::TextVersion;
use crate::language::lsp::ApplyEditReply;
use crate::language::lsp::EditContext;
use crate::language::lsp::LspCapabilities;
use crate::language::lsp::LspClient;
use crate::language::lsp::LspCodeAction;
use crate::language::lsp::LspDiagnostic;
use crate::language::lsp::LspDocument;
use crate::language::lsp::LspError;
use crate::language::lsp::LspServerConfiguration;
use crate::language::lsp::LspSessionLaunch;
use crate::language::lsp::LspWorkspaceEdit;
use crate::language::lsp::OwnedLspEvent;
use crate::tools::ToolDefinition;
use crate::tools::nix_lsp_launch_environment_for_workspace_at;
use crate::tools::tool_launch_environment;
use crate::workspace::ProposalId;
use crate::workspace::RootId;
use crate::workspace::WorkspaceTrust;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct LspProjectionOwner {
    pub server: String,
    pub buffer: BufferId,
    pub view: ViewId,
    pub revision: BufferRevision,
    pub text: TextVersion,
    pub uri: String,
    pub language: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct LspAutostartObservation {
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
pub(in crate::app) enum LspLaunch {
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
    pub(in crate::app) fn executable(&self) -> &PathBuf {
        match self {
            Self::Direct { executable, .. } => executable,
            Self::NixWorkspace { nix, .. } => nix,
        }
    }
    pub(in crate::app) fn arguments(&self, definition: &ToolDefinition) -> Vec<String> {
        definition.launch_args.clone()
    }
    pub(in crate::app) fn environment(
        &self,
        definition: &ToolDefinition,
    ) -> Result<BTreeMap<String, std::ffi::OsString>, String> {
        match self {
            Self::Direct { managed: true, .. } => Ok(tool_launch_environment(definition)),
            Self::Direct { .. } => Ok(BTreeMap::new()),
            Self::NixWorkspace { workspace_root, .. } => {
                nix_lsp_launch_environment_for_workspace_at(workspace_root, definition)
            }
        }
    }
    pub(in crate::app) fn with_preflighted_executable(self, executable: PathBuf) -> Self {
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
pub(in crate::app) struct LspRuntimeState {
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
    marks: inlay::LanguageMarks,
    preparation: launch::Preparation,
    service: LspClient,
    incoming: std::collections::BTreeMap<i64, OwnedLspEvent>,
    incoming_id: i64,
    edits: LspEditSources,
    deferred_commands: Vec<(ProposalId, String, crate::language::lsp::LspCommand)>,
    sessions: HashMap<String, LspRuntimeState>,
    active: Option<String>,
    synced: HashMap<(String, BufferId), TextVersion>,
    autostart: Option<LspAutostartObservation>,
    /// Languages already told which server to install, so the hint shows once per run.
    install_hinted: HashSet<String>,
    /// The last unhealthy status each server reported, so a repeated report isn't shown again.
    statuses: HashMap<String, String>,
    /// Servers whose toolchain Dust already tried to repair this run.
    repaired: HashSet<String>,
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
            crate::app::windows::NavigationStamp,
            crate::app::workspace::WorkspaceVersion,
        ),
    >,
    #[cfg(test)]
    fixture_capabilities: Option<LspCapabilities>,
}
impl LspSession {
    pub(in crate::app) fn edit_context_current(&self, context: &EditContext) -> bool {
        self.service.edit_context_current(context)
    }
    pub(in crate::app) fn register_navigation_read(
        &mut self,
        id: crate::files::RequestId,
        range: crate::language::lsp::LspRange,
        encoding: crate::editor::text::PositionEncoding,
        navigation: crate::app::windows::NavigationStamp,
        workspace: crate::app::workspace::WorkspaceVersion,
    ) {
        self.navigation_reads
            .insert(id, (range, encoding, navigation, workspace));
    }
    pub(in crate::app) fn take_navigation_read(
        &mut self,
        id: crate::files::RequestId,
    ) -> Option<(
        crate::language::lsp::LspRange,
        crate::editor::text::PositionEncoding,
        crate::app::windows::NavigationStamp,
        crate::app::workspace::WorkspaceVersion,
    )> {
        self.navigation_reads.remove(&id)
    }
    pub(in crate::app) fn forget_document_request(&mut self, id: crate::files::RequestId) {
        self.navigation_reads.remove(&id);
    }
    pub(in crate::app) fn running_servers(&self) -> Vec<String> {
        self.service.running_servers()
    }
    pub(in crate::app) fn stop(&mut self, server: &str) -> Result<(), LspError> {
        self.service.stop(server)
    }
    pub(in crate::app) fn diagnostic_producer(
        &self,
        server: &str,
        prefix: &str,
    ) -> Option<crate::workspace::DiagnosticProducer> {
        self.service.diagnostic_producer(server, prefix)
    }
    pub(in crate::app) fn owns_event_context(&self, context: &EditContext) -> bool {
        self.service.owns_event_context(context)
    }
    pub(in crate::app) fn sync_owned(
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
    pub(in crate::app) fn close(&self, server: &str, uri: String) -> Result<(), LspError> {
        self.service.close(server, uri)
    }
    pub(in crate::app) fn explicitly_detached(&self, server: &str, buffer: BufferId) -> bool {
        self.detached.contains(&(server.to_owned(), buffer))
    }
    pub(in crate::app) fn clear_detached(&mut self, buffer: BufferId) {
        self.detached.retain(|(_, id)| *id != buffer);
    }
    pub(in crate::app) fn set_detached(
        &mut self,
        server: String,
        buffer: BufferId,
        detached: bool,
    ) {
        if detached {
            self.detached.insert((server, buffer));
        } else {
            self.detached.remove(&(server, buffer));
        }
    }
    pub(in crate::app) fn save(&self, server: &str, document: LspDocument) -> Result<(), LspError> {
        self.service.save(server, document)
    }
    pub(in crate::app) fn cancel(&self, server: &str, request_id: u64) -> Result<(), LspError> {
        self.service.cancel(server, request_id)
    }
    pub(in crate::app) fn configure(
        &mut self,
        server: &str,
        configuration: LspServerConfiguration,
    ) -> Result<(), LspError> {
        self.pending_configurations.remove(server);
        match self.service.configure(server, configuration.clone()) {
            Err(error @ LspError::Busy(_)) => {
                self.pending_configurations
                    .insert(server.into(), configuration);
                Err(error)
            }
            result => result,
        }
    }
    pub(in crate::app) fn retry_configurations(&mut self) -> Vec<LspError> {
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
    pub(in crate::app) fn health(&self, server: &str) -> Result<(), LspError> {
        self.service.health(server)
    }
    pub(in crate::app) fn offer_actions(
        &mut self,
        server: &str,
        uri: &str,
        actions: Vec<LspCodeAction>,
        context: Option<EditContext>,
    ) {
        self.edits.offer_actions(server, uri, actions, context);
    }
    pub(in crate::app) fn clear_offered_actions(&mut self) {
        self.edits.clear_offered_actions();
    }
    pub(in crate::app) fn has_offered_actions(&self) -> bool {
        self.edits.has_offered_actions()
    }
    pub(in crate::app) fn install_offered_actions(
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
    pub(in crate::app) fn clear_actions(&mut self) {
        self.edits.clear_actions();
    }
    pub(in crate::app) fn take_action(
        &mut self,
        key: &CodeActionKey,
        current: &[EditDocument<'_>],
    ) -> Result<OfferedAction, String> {
        self.edits.take_action(key, &self.service, current)
    }
    pub(in crate::app) fn poll_event(&self) -> Option<OwnedLspEvent> {
        self.edits.poll_event(&self.service)
    }
    pub(in crate::app) fn flush_edit_replies(&mut self) -> Vec<String> {
        self.edits.flush()
    }
    pub(in crate::app) fn pending_replies(&self) -> bool {
        self.edits.pending_replies()
    }
    pub(in crate::app) fn reject_reply(&mut self, reply: Option<ApplyEditReply>, reason: String) {
        self.edits.reject_reply(reply, reason);
    }
    pub(in crate::app) fn start_edit(
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
    pub(in crate::app) fn check_edit(
        &self,
        key: &OriginKey,
        current: &[EditDocument<'_>],
    ) -> Result<(), String> {
        self.edits.check(key, current, &self.service)
    }
    pub(in crate::app) fn decode_edit(
        &mut self,
        key: &OriginKey,
        files: PreparedFiles<'_>,
    ) -> Result<Vec<crate::workspace::WorkspaceEditDraft>, String> {
        self.edits.decode(key, files)
    }
    pub(in crate::app) fn finish_edit(
        &mut self,
        key: &OriginKey,
        applied: bool,
        reason: Option<String>,
        editor: &EditorState,
    ) -> Result<(), String> {
        let command = self.edits.take_command(key);
        self.edits.finish(key, applied, reason)?;
        if applied && let Some((server, command)) = command {
            self.execute_edit_command(&server, command, editor)?;
        }
        Ok(())
    }
    fn execute_edit_command(
        &mut self,
        server: &str,
        command: crate::language::lsp::LspCommand,
        editor: &EditorState,
    ) -> Result<(), String> {
        let fallback = self
            .sessions
            .get(server)
            .and_then(|session| session.documents.values().next())
            .cloned()
            .ok_or("code-action server is no longer attached")?;
        self.sync_edit_documents(server, editor)?;
        // executeCommand itself has no document parameter, even when the edit deleted it.
        let document = self
            .sessions
            .get(server)
            .and_then(|session| session.documents.values().next())
            .cloned()
            .unwrap_or(fallback);
        let request_id = self
            .next_request_id()
            .ok_or("language-server request identity exhausted")?;
        self.service
            .request(
                server,
                crate::language::lsp::LspFeature::ExecuteCommand(command),
                document,
                crate::editor::text::ByteOffset(0),
                request_id,
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }
    fn sync_edit_documents(&mut self, server: &str, editor: &EditorState) -> Result<(), String> {
        let documents = self
            .sessions
            .get(server)
            .into_iter()
            .flat_map(|session| {
                session
                    .documents
                    .iter()
                    .map(|(id, document)| (*id, document.clone()))
            })
            .collect::<Vec<_>>();
        for (id, mut document) in documents {
            let current = editor
                .buffers
                .get(id)
                .filter(|buffer| buffer.path.is_some());
            let path_changed =
                current.is_none_or(|buffer| buffer.path.as_ref() != Some(&document.path));
            if path_changed {
                self.close(server, document.uri())
                    .map_err(|error| error.to_string())?;
                self.detach(server, id);
            }
            let Some(buffer) = current else {
                continue;
            };
            let version = buffer.text.source_version();
            if !path_changed && self.synced(server, id, &version) {
                continue;
            }
            document.path = buffer.path.clone().expect("checked document path");
            if crate::language::lsp::language_id_for_path(&document.path)
                != Some(document.language_id.as_str())
            {
                continue;
            }
            document.version = buffer.text.revision().0;
            document.text = buffer.text.to_string();
            self.sync_owned(server, document, (id, version))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    pub(in crate::app) fn defer_edit(
        &mut self,
        key: &OriginKey,
        proposal: ProposalId,
    ) -> Result<(), String> {
        let command = self.edits.take_command(key);
        self.edits.finish(key, true, None)?;
        if let Some((server, command)) = command {
            self.deferred_commands.push((proposal, server, command));
        }
        Ok(())
    }
    pub(in crate::app) fn finish_deferred_edit(
        &mut self,
        proposal: ProposalId,
        editor: &EditorState,
    ) -> Result<(), String> {
        if let Some(index) = self
            .deferred_commands
            .iter()
            .position(|(id, _, _)| *id == proposal)
        {
            let (_, server, command) = self.deferred_commands.remove(index);
            self.execute_edit_command(&server, command, editor)?;
        }
        Ok(())
    }
    pub(in crate::app) fn cancel_deferred_edits(&mut self, proposal: Option<ProposalId>) {
        self.deferred_commands
            .retain(|(id, _, _)| proposal.is_some_and(|proposal| *id != proposal));
    }
    pub(in crate::app) fn set_edit_command(
        &mut self,
        server: String,
        command: crate::language::lsp::LspCommand,
    ) {
        self.edits.set_command(server, command);
    }
    pub(in crate::app) fn diagnostic_batch(
        &self,
        context: &EditContext,
        editor: &EditorState,
        publication: (&str, Option<u64>, Vec<LspDiagnostic>),
        policy: (bool, usize, &str),
    ) -> Result<crate::app::diagnostics::DiagnosticBatch, String> {
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
    pub(in crate::app) fn sessions(&self) -> &HashMap<String, LspRuntimeState> {
        &self.sessions
    }
    pub(in crate::app) fn active(&self) -> Option<&String> {
        self.active.as_ref()
    }
    pub(in crate::app) fn capabilities(&self) -> Option<&LspCapabilities> {
        #[cfg(test)]
        if self.fixture_capabilities.is_some() {
            return self.fixture_capabilities.as_ref();
        }
        self.sessions
            .get(self.active.as_ref()?)?
            .capabilities
            .as_ref()
    }
    pub(in crate::app) fn errors(&self) -> &BTreeMap<String, String> {
        &self.errors
    }
    pub(in crate::app) fn failed(&mut self, server: String, error: String) {
        self.errors.insert(server, error);
    }
    pub(in crate::app) fn clear_error(&mut self, server: &str) {
        self.errors.remove(server);
    }
    pub(in crate::app) fn clear_active(&mut self) {
        self.active = None;
        #[cfg(test)]
        {
            self.fixture_capabilities = None;
        }
    }
    pub(in crate::app) fn start(
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
            return Err("language-server launch document source is stale".into());
        }
        let key = request.session_key.to_owned();
        let definition = request.definition.clone();
        let root_path = request.root.clone();
        let document = document.clone();
        self.service
            .start_session_owned(request, Some((buffer, version.clone())))
            .map_err(|e| e.to_string())?;
        // No registry entry is installed unless the real transport accepted startup.
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
        self.clear_server_projection(&key);
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
    pub(in crate::app) fn close_stale_documents(&mut self, editor: &EditorState) {
        for (key, session) in &mut self.sessions {
            session.documents.retain(|id, document| {
                let current = editor
                    .buffers
                    .get(*id)
                    .is_some_and(|buffer| buffer.path.as_ref() == Some(&document.path));
                if !current {
                    // Keep the registration until a busy worker can accept its close.
                    if matches!(
                        self.service.close(key, document.uri()),
                        Err(LspError::Busy(_))
                    ) {
                        return true;
                    }
                    self.synced.remove(&(key.clone(), *id));
                }
                current
            });
        }
    }
    /// Returns whether the active document changed, so its UI refreshes still run.
    pub(in crate::app) fn retry_documents(&mut self, editor: &EditorState) -> bool {
        self.close_stale_documents(editor);
        let mut active_changed = false;
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
                if self.synced.get(&(key.clone(), *id)) == Some(&version) {
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
                    active_changed |= editor.active_buffer_id() == Some(*id);
                }
            }
        }
        active_changed
    }
    pub(in crate::app) fn remove(&mut self, key: &str) {
        self.clear_server_projection(key);
        self.sessions.remove(key);
        self.forget_synced(key);
    }
    pub(in crate::app) fn select(&mut self, key: &str) -> bool {
        if !self.sessions.contains_key(key) {
            return false;
        }
        for (id, state) in &mut self.sessions {
            state.activate_on_restart = id == key;
        }
        self.active = Some(key.into());
        true
    }
    pub(in crate::app) fn running(&mut self, key: &str, capabilities: LspCapabilities) -> bool {
        let Some(session) = self.sessions.get_mut(key) else {
            return false;
        };
        self.errors.remove(&session.server_id);
        session.capabilities = Some(capabilities);
        self.active.as_deref() == Some(key)
    }
    pub(in crate::app) fn stopped(&mut self, key: &str) -> bool {
        self.clear_server_projection(key);
        self.forget_synced(key);
        self.sessions.get_mut(key).is_some_and(|state| {
            state.capabilities = None;
            state.desired_running
        })
    }
    pub(in crate::app) fn request_stop(&mut self, key: &str) {
        if let Some(state) = self.sessions.get_mut(key) {
            state.desired_running = false;
            state.restart_due = None;
        }
    }
    pub(in crate::app) fn request_start(&mut self, key: &str, restart: Option<Instant>) {
        if let Some(state) = self.sessions.get_mut(key) {
            state.restart_count = 0;
            state.restart_due = restart;
            state.circuit_open = false;
            state.desired_running = true;
        }
    }
    pub(in crate::app) fn schedule_restart(&mut self, key: &str, now: Instant) -> Option<String> {
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
    pub(in crate::app) fn restart_due(&mut self, now: Instant) -> Option<String> {
        let (key, state) = self.sessions.iter_mut().find(|(_, state)| {
            state.desired_running
                && !state.circuit_open
                && state.restart_due.is_some_and(|due| due <= now)
        })?;
        state.restart_due = None;
        Some(key.clone())
    }
    pub(in crate::app) fn synced(
        &self,
        key: &str,
        buffer: BufferId,
        version: &TextVersion,
    ) -> bool {
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
    pub(in crate::app) fn invalidate_document(&mut self, key: &str, buffer: BufferId) {
        self.synced.remove(&(key.into(), buffer));
    }
    pub(in crate::app) fn forget_synced(&mut self, key: &str) {
        self.synced.retain(|(server, _), _| server != key);
    }
    pub(in crate::app) fn detach(&mut self, key: &str, buffer: BufferId) {
        self.invalidate_document(key, buffer);
        if let Some(document) = self
            .sessions
            .get_mut(key)
            .and_then(|state| state.documents.remove(&buffer))
        {
            self.clear_document_projection(key, &document.uri());
        }
    }
    pub(in crate::app) fn hint_install_once(&mut self, language: &str) -> bool {
        self.install_hinted.insert(language.to_owned())
    }
    /// Records a server's status and says whether it is a new problem worth reporting.
    pub(in crate::app) fn status_changed(
        &mut self,
        key: &str,
        healthy: bool,
        message: &str,
    ) -> bool {
        if healthy {
            self.statuses.remove(key);
            return false;
        }
        self.statuses
            .insert(key.to_owned(), message.to_owned())
            .as_deref()
            != Some(message)
    }
    /// A restarted server reports afresh, so a problem it still has is shown again.
    pub(in crate::app) fn forget_status(&mut self, key: &str) {
        self.statuses.remove(key);
    }
    pub(in crate::app) fn repair_once(&mut self, server_id: &str) -> bool {
        self.repaired.insert(server_id.to_owned())
    }
    pub(in crate::app) fn mark_stopped(&mut self, server_id: &str) {
        self.stopped.insert(server_id.to_owned());
    }
    pub(in crate::app) fn clear_stopped(&mut self, server_id: &str) {
        self.stopped.remove(server_id);
    }
    pub(in crate::app) fn clear_all_stopped(&mut self) {
        self.stopped.clear();
    }
    pub(in crate::app) fn is_stopped(&self, server_id: &str) -> bool {
        self.stopped.contains(server_id)
    }
    pub(in crate::app) fn reset_autostart(&mut self) {
        self.autostart = None;
    }
    /// Starts the next unattached server once per change in what the editor sees.
    pub(in crate::app) fn observe(
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
    pub(in crate::app) fn fixture_transport(
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
    pub(in crate::app) fn fixture_session(&mut self, key: String, state: LspRuntimeState) {
        self.sessions.insert(key, state);
    }
    pub(in crate::app) fn fixture_capabilities(&mut self, capabilities: LspCapabilities) {
        self.fixture_capabilities = Some(capabilities);
    }
}

pub(in crate::app) mod edits;
pub(in crate::app) mod snippets;

pub(in crate::app) mod commands;

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::diagnostics::MAXIMUM_DIAGNOSTICS_PER_URI;
use crate::app::picker::MatcherSource;
use crate::app::picker::PickerPayload;
use crate::app::tools::LspRuntimeSource;
use crate::editor::EditorOperation;
use crate::editor::EditorTransaction;
use crate::editor::UndoPolicy;
use crate::editor::text::ByteOffset;
use crate::editor::text::ByteRange;
use crate::editor::text::PositionCodec;
use crate::editor::text::PositionEncoding;
use crate::editor::text::TextBuffer;
use crate::editor::text::TextEdit;
use crate::language::lsp::LspEvent;
use crate::language::lsp::LspFeature;
use crate::language::lsp::LspFeatureResult;
use crate::language::lsp::LspProgressKind;
use crate::language::lsp::path_from_file_uri;
use crate::persistence::PersistedLocation;
use crate::tools::discover_executable;
use crate::ui::DocumentationPopupKind;
use crate::ui::PickerSurfaceKind;
use crate::ui::UiFold;
use crate::ui::UiInlayHint;
use crate::ui::UiSyntaxSpan;
use crate::ui::render::COMPLETION_MAX_ROWS;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;

impl App {
    pub(in crate::app) fn show_lsp_logs(&mut self) {
        let previous = if self.search.picker_surface() == PickerSurfaceKind::Logs {
            self.search.picker_bookmark()
        } else {
            None
        };
        let normal = previous.is_some() && self.input.picker_normal();
        let follow_tail = previous.is_none() || self.search.picker_follows_tail();
        let items = self
            .language
            .logs()
            .iter()
            .enumerate()
            .map(|(index, line)| {
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!(
                            "lsp-log:{}",
                            self.language.log_start().saturating_add(index as u64)
                        )),
                        label: line.clone(),
                        detail: String::new(),
                        source: "lsp".into(),
                        action: PickerAction::None,
                    },
                    PickerPayload::Notice {
                        title: self.message("lsp_logs", &[("logs", String::new())]),
                        text: line.clone(),
                    },
                )
            })
            .collect();
        self.install_surface_picker(
            PickerSurfaceKind::Logs,
            self.message("lsp_logs", &[("logs", String::new())]),
            items,
        );
        self.search.restore_picker_view(previous, follow_tail);
        self.input.set_picker_normal(normal);
        self.sync_picker_ui();
    }
}

impl App {
    pub(in crate::app) fn lsp_completion_kind(
        item: &crate::language::lsp::LspCompletionItem,
    ) -> &'static str {
        match item.kind {
            Some(1) => "Text",
            Some(2) => "Method",
            Some(3) => "Function",
            Some(4) => "Constructor",
            Some(5) => "Field",
            Some(6) => "Variable",
            Some(7) => "Class",
            Some(8) => "Interface",
            Some(9) => "Module",
            Some(10) => "Property",
            Some(11) => "Unit",
            Some(12) => "Value",
            Some(13) => "Enum",
            Some(14) => "Keyword",
            Some(15) => "Snippet",
            Some(16) => "Color",
            Some(17) => "File",
            Some(18) => "Reference",
            Some(19) => "Folder",
            Some(20) => "EnumMember",
            Some(21) => "Constant",
            Some(22) => "Struct",
            Some(23) => "Event",
            Some(24) => "Operator",
            Some(25) => "TypeParameter",
            _ => "Item",
        }
    }
}

impl App {
    pub(in crate::app) fn refresh_lsp_completion_documentation(&mut self) {
        let Some(selected) = self
            .ui
            .language_completion
            .as_ref()
            .map(|completion| completion.selected)
        else {
            return;
        };
        if self
            .language
            .completion_items()
            .get(selected)
            .is_some_and(|item| item.documentation.is_some() || item.signature().is_some())
        {
            self.open_lsp_completion_documentation(false);
        } else {
            if self
                .input
                .documentation()
                .is_some_and(|popup| popup.kind == DocumentationPopupKind::Completion)
            {
                self.close_documentation();
            }
            let can_resolve = self
                .language
                .completion_items()
                .get(selected)
                .is_some_and(|item| !item.resolved && item.resolve_item.is_some())
                && self
                    .language
                    .completion_server(selected)
                    .and_then(|server| self.language.sessions().get(server))
                    .and_then(|session| session.capabilities.as_ref())
                    .is_some_and(|capabilities| capabilities.completion_resolve);
            if can_resolve && let Some((_, document)) = self.active_lsp_document() {
                let offset = self
                    .state
                    .active_view()
                    .map_or(ByteOffset(0), |view| self.state.views[view].cursor);
                if let Err(error) = self
                    .language
                    .resolve_completion(selected, document, offset, false)
                {
                    self.show_error(error);
                }
            }
        }
    }
}

impl App {
    pub(in crate::app) fn move_lsp_completion(&mut self, direction: i8) {
        let rows = usize::from(COMPLETION_MAX_ROWS.max(1));
        if let Some((selected, scroll)) = self.language.move_completion(direction, rows)
            && let Some(completion) = self.ui.language_completion.as_mut()
        {
            completion.selected = selected;
            completion.scroll = scroll;
        }
        self.project_lsp_completion();
    }
}

impl App {
    pub(in crate::app) fn accept_lsp_completion(&mut self) {
        if !self.language.completion_current(&self.state) {
            self.language.clear_completion();
            self.ui.language_completion = None;
            return;
        }
        let selected = self.language.completion_position().0;
        let Some(item) = self.language.completion_items().get(selected).cloned() else {
            return;
        };
        let Some(buffer_id) = self.state.active_buffer_id() else {
            return;
        };
        let buffer = &self.state.buffers[buffer_id];
        let document = LspDocument {
            path: buffer.path.clone().unwrap_or_default(),
            language_id: String::new(),
            version: buffer.text.revision().0,
            text: buffer.text.to_string(),
        };
        // The item belongs to the server that offered it: resolve there and read its positions in that server's encoding.
        let server = self
            .language
            .completion_server(selected)
            .map(str::to_owned)
            .or_else(|| self.language.active().cloned())
            .unwrap_or_default();
        if !item.resolved
            && self
                .language
                .sessions()
                .get(&server)
                .and_then(|session| session.capabilities.as_ref())
                .is_some_and(|capabilities| capabilities.completion_resolve)
            && item.resolve_item.is_some()
        {
            let offset = self
                .state
                .active_view()
                .map_or(ByteOffset(0), |view| self.state.views[view].cursor);
            if let Err(error) = self
                .language
                .resolve_completion(selected, document, offset, true)
            {
                self.show_error(error);
            }
            return;
        }
        let encoding = self.language.completion_encoding(&server);
        let snippet = item.snippet;
        let offset = self
            .state
            .active_view()
            .map_or(ByteOffset(0), |view| self.state.views[view].cursor);
        let buffer = TextBuffer::from_text(&document.text);
        let codec = PositionCodec::new(&buffer);
        let (line, character) = codec.byte_to_lsp(offset, encoding).unwrap_or((0, 0));
        let here = crate::language::lsp::LspPosition { line, character };
        let mut edit = match item.text_edit {
            // The range was the server's at request time; what was typed since goes too.
            Some(mut edit) => {
                if (edit.range.start.line, edit.range.start.character)
                    > (edit.range.end.line, edit.range.end.character)
                {
                    self.show_error("completion edit range is reversed");
                    return;
                }
                if edit.range.end.line == line && edit.range.end.character < character {
                    edit.range.end = here;
                }
                edit
            }
            // No range from the server: replace the word typed so far.
            None => {
                let start = self
                    .language
                    .completion_start()
                    .filter(|start| {
                        *start <= offset
                            && buffer.byte_to_line(*start).is_ok_and(|start| start == line)
                    })
                    .and_then(|start| codec.byte_to_lsp(start, encoding).ok())
                    .map_or(here.clone(), |(line, character)| {
                        crate::language::lsp::LspPosition { line, character }
                    });
                crate::language::lsp::LspTextEdit {
                    range: crate::language::lsp::LspRange { start, end: here },
                    new_text: item.insert_text,
                }
            }
        };
        let mut tabstops = Vec::new();
        let mut transforms = Vec::new();
        if snippet {
            let path = &document.path;
            let current_line = buffer.line(line).unwrap_or_default();
            let line_start = buffer.line_to_byte(line).unwrap_or(ByteOffset(0)).get();
            let column = offset
                .get()
                .saturating_sub(line_start)
                .min(current_line.len());
            let word_start = current_line[..column]
                .char_indices()
                .rev()
                .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
                .map_or(0, |(at, c)| at + c.len_utf8());
            let word_end = current_line[column..]
                .char_indices()
                .find(|(_, c)| !c.is_alphanumeric() && *c != '_')
                .map_or(current_line.len(), |(at, _)| column + at);
            let selected = self
                .state
                .active_view()
                .and_then(|view| self.state.views[view].selection)
                .and_then(|selection| {
                    let start = selection.anchor.min(selection.head);
                    let end = selection.anchor.max(selection.head);
                    buffer.slice(ByteRange::new(start, end).ok()?).ok()
                })
                .unwrap_or_default();
            let mut variables = std::collections::HashMap::from([
                (
                    "TM_FILENAME".into(),
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "TM_FILENAME_BASE".into(),
                    path.file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "TM_DIRECTORY".into(),
                    path.parent().unwrap_or(path).to_string_lossy().into_owned(),
                ),
                ("TM_FILEPATH".into(), path.to_string_lossy().into_owned()),
                (
                    "RELATIVE_FILEPATH".into(),
                    path.strip_prefix(&self.state.workspace_root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "WORKSPACE_NAME".into(),
                    self.state
                        .workspace_root
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "WORKSPACE_FOLDER".into(),
                    self.state.workspace_root.to_string_lossy().into_owned(),
                ),
                ("TM_LINE_INDEX".into(), line.to_string()),
                ("TM_LINE_NUMBER".into(), (line + 1).to_string()),
                (
                    "TM_CURRENT_WORD".into(),
                    current_line[word_start..word_end].into(),
                ),
                (
                    "TM_CURRENT_LINE".into(),
                    current_line.trim_end_matches('\n').into(),
                ),
                ("TM_SELECTED_TEXT".into(), selected),
            ]);
            if variables
                .get("TM_SELECTED_TEXT")
                .is_some_and(String::is_empty)
            {
                variables.remove("TM_SELECTED_TEXT");
            }
            match crate::language::lsp::parse_snippet_with_variables(
                &edit.new_text,
                1024 * 1024,
                &variables,
            ) {
                Ok(parsed) => {
                    edit.new_text = parsed.text;
                    tabstops = parsed.tabstops;
                    transforms = parsed.transforms;
                }
                Err(error) => {
                    self.show_error(error);
                    return;
                }
            }
        }
        let insertion_start = TextBuffer::from_text(&document.text);
        let mut insertion_start = PositionCodec::new(&insertion_start)
            .lsp_to_byte(edit.range.start.line, edit.range.start.character, encoding)
            .ok();
        if let Some(start) = &mut insertion_start {
            let original = start.get();
            let mut shift = 0isize;
            for additional in &item.additional_edits {
                if let (Ok(begin), Ok(end)) = (
                    codec.lsp_to_byte(
                        additional.range.start.line,
                        additional.range.start.character,
                        encoding,
                    ),
                    codec.lsp_to_byte(
                        additional.range.end.line,
                        additional.range.end.character,
                        encoding,
                    ),
                ) && let Ok(range) = ByteRange::new(begin, end)
                    && end.get() <= original
                {
                    let inserted = TextEdit::new(range, additional.new_text.clone())
                        .replacement()
                        .len();
                    shift += inserted as isize - range.len() as isize;
                }
            }
            *start = ByteOffset(original.saturating_add_signed(shift));
        }
        let mut edits = vec![edit];
        edits.extend(item.additional_edits);
        let applied =
            crate::app::language::edits::decode_edits(edits, &buffer, encoding).and_then(|edits| {
                let transaction = EditorTransaction::new(self.transaction_id()).with_operation(
                    EditorOperation::ApplyEdits {
                        buffer: buffer_id,
                        edits: crate::editor::text::TextEditSet::new(
                            self.state.buffers[buffer_id].text.revision(),
                            edits,
                        ),
                    },
                );
                self.state
                    .apply_transaction(&transaction)
                    .map_err(|error| error.to_string())
            });
        let succeeded = applied.is_ok();
        if let Err(error) = applied {
            self.show_error(error);
        } else {
            if let Some(start) = insertion_start
                && !tabstops.is_empty()
            {
                let source = self.state.window_version();
                self.input.install_snippet(
                    crate::app::language::snippets::SnippetDocument {
                        editor: &source,
                        buffer: buffer_id,
                        text: &self.state.buffers[buffer_id].text,
                        view: self.state.active_view(),
                    },
                    start,
                    tabstops,
                );
                self.input.install_snippet_transforms(start, transforms);
                self.select_snippet_current();
            }
        }
        self.language.clear_completion();
        self.ui.language_completion = None;
        if succeeded && let Some(command) = item.command {
            self.execute_lsp_command(&server, command);
        }
    }
}

impl App {
    pub(in crate::app) fn execute_lsp_command(
        &mut self,
        server: &str,
        command: crate::language::lsp::LspCommand,
    ) {
        self.sync_lsp_document();
        self.refresh_lsp_projection_owner();
        if let Some((_, document)) = self.active_lsp_document()
            && let Err(error) = self.language.request_projected(
                server,
                LspFeature::ExecuteCommand(command),
                document,
                ByteOffset(0),
            )
        {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn select_snippet_current(&mut self) {
        let source = self.state.window_version();
        let document = self.input.snippet_buffer().and_then(|id| {
            self.state.buffers.get(id).map(|buffer| {
                crate::app::language::snippets::SnippetDocument {
                    editor: &source,
                    buffer: id,
                    text: &buffer.text,
                    view: self.state.active_view().filter(|view| {
                        self.state
                            .views
                            .get(*view)
                            .is_some_and(|view| view.buffer == id)
                    }),
                }
            })
        });
        let Some(operations) = self.input.snippet_selection(document) else {
            return;
        };
        let mut transaction =
            EditorTransaction::new(self.transaction_id()).with_undo_policy(UndoPolicy::Ignore);
        transaction.operations.extend(operations);
        if let Err(error) = self.state.apply_transaction(&transaction) {
            self.show_error(error);
        } else {
            self.ensure_cursor_visible();
        }
    }
}

impl App {
    pub(in crate::app) fn navigate_snippet(&mut self, direction: i8) {
        let buffer = self.input.snippet_buffer();
        if self.input.navigate_snippet(direction)
            && let Some(view) = self.state.active_view()
            && Some(self.state.views[view].buffer) == buffer
        {
            let transaction = EditorTransaction::new(self.transaction_id())
                .with_undo_policy(UndoPolicy::Ignore)
                .with_operation(EditorOperation::SetSelection {
                    view,
                    selection: None,
                });
            if let Err(error) = self.state.apply_transaction(&transaction) {
                self.show_error(error);
            }
        }
        self.select_snippet_current();
    }
}

impl App {
    pub(in crate::app) fn lsp_position_encoding(&self) -> PositionEncoding {
        self.language
            .capabilities()
            .map_or(PositionEncoding::Utf16, |c| c.position_encoding)
    }
}

/// How often, and how far apart, a request answered "content modified" before its server
/// reported loading is sent again: rust-analyzer does this for a file opened a moment ago.
const LSP_CONTENT_MODIFIED_RETRIES: u8 = 10;

const LSP_CONTENT_MODIFIED_DELAY: Duration = Duration::from_millis(500);

pub(in crate::app) enum NavigationRead {
    Matcher(MatcherSource),
    Persisted(PersistedLocation),
    Lsp(
        crate::language::lsp::LspRange,
        PositionEncoding,
        crate::app::windows::NavigationStamp,
        crate::app::workspace::WorkspaceVersion,
    ),
    ByteRange(ByteRange, Option<std::sync::Arc<[u8]>>),
    /// A recovery journal's path and text, to put into the file once it is open.
    Recovered(std::path::PathBuf, String),
}
impl App {
    fn clear_lsp_ui_rows(&mut self) {
        if self.language.completion_current(&self.state) {
            self.project_lsp_completion();
        } else {
            self.ui.language_completion = None;
            self.ui.inline_completion = None;
        }
        // A Git or Markdown preview doesn't come from a server, and this runs each frame in a file
        // without one.
        if self.input.documentation().is_some_and(|popup| {
            !matches!(
                popup.kind,
                DocumentationPopupKind::Git | DocumentationPopupKind::Notice
            ) && !(popup.kind == DocumentationPopupKind::Completion
                && self.language.completion_current(&self.state))
        }) {
            self.close_documentation();
        }
        self.ui.set(Vec::new(), |ui| &mut ui.lsp_semantic_spans);
        self.ui.set(Vec::new(), |ui| &mut ui.lsp_highlight_spans);
        self.ui.set(Vec::new(), |ui| &mut ui.lsp_inlay_hints);
        self.ui.set(Vec::new(), |ui| &mut ui.lsp_folds);
    }
}

impl App {
    pub(in crate::app) fn clear_lsp_ui_projection(&mut self) {
        self.language.clear_projection(&self.state);
        self.clear_lsp_ui_rows();
    }
}

impl App {
    pub(in crate::app) fn refresh_lsp_projection_owner(&mut self) {
        let document = self.active_lsp_document_without_text();
        let change = self.language.refresh_projection(
            &self.state,
            document
                .as_ref()
                .map(|(buffer, document)| (*buffer, document)),
        );
        if change != ProjectionChange::Same {
            // Typing keeps the server's colours, moved along with the edit, until it sends new
            // ones; dropping them flashes the text back to tree-sitter's.
            let semantic = std::mem::take(&mut self.ui.lsp_semantic_spans);
            let hints = std::mem::take(&mut self.ui.lsp_inlay_hints);
            self.clear_lsp_ui_rows();
            if change == ProjectionChange::Typed {
                self.ui.lsp_semantic_spans = semantic;
            }
            // Each buffer keeps its hints, moved along with edits, as in Neovim: switching back
            // to a file or typing shows them at once while fresh ones load.
            self.ui.lsp_inlay_hints = hints;
            self.project_language_marks();
            if !self.language.completion_items().is_empty() {
                self.project_lsp_completion();
            }
            if self.language.projection().is_some() {
                self.schedule_lsp_inlay_refresh(true);
            }
        }
    }
}

impl App {
    pub(in crate::app) fn clear_lsp_session_state(&mut self) {
        if let Some(server) = self.language.active().cloned() {
            self.clear_lsp_server_ownership(&server);
        }
        self.language.clear_active();
        self.sync_ui_diagnostics();
        self.clear_lsp_ui_projection();
    }
}

impl App {
    pub(in crate::app) fn schedule_lsp_inlay_startup_refresh(&mut self) {
        self.language
            .startup_inlays(&self.configuration.settings().lsp, Instant::now());
    }
}

impl App {
    pub(in crate::app) fn schedule_lsp_inlay_warmup_retry(&mut self) -> bool {
        self.language
            .retry_inlays(&self.configuration.settings().lsp, Instant::now())
    }
}

impl App {
    pub(in crate::app) fn schedule_lsp_inlay_refresh(&mut self, immediate: bool) {
        self.language.schedule_inlays(
            &self.configuration.settings().lsp,
            immediate,
            Instant::now(),
        );
        if !self.configuration.settings().lsp.inlay_hints {
            self.ui.lsp_inlay_hints.clear();
        }
    }
}

impl App {
    pub(in crate::app) fn process_lsp_inlay_refresh(&mut self) {
        self.project_language_marks();
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

impl App {
    fn reprobe_failed_path_runtime(&mut self, server: &str) {
        let Some(definition) = self.language.sessions().get(server).and_then(|state| {
            matches!(&state.launch, LspLaunch::Direct { managed: false, .. })
                .then(|| state.definition.clone())
        }) else {
            return;
        };
        let Some(executable) = discover_executable(&definition.executable) else {
            return;
        };
        let key = self.tool_version_key(&definition, LspRuntimeSource::Path, Some(&executable));
        self.tooling.forget_probe(&key);
        self.queue_tool_version_probe(&definition, LspRuntimeSource::Path, executable);
    }
}

impl App {
    pub(in crate::app) fn append_lsp_log(&mut self, line: String) {
        self.language.append_log(line, LSP_MAXIMUM_LOG_LINES);
        if self.search.picker_surface() == PickerSurfaceKind::Logs && self.ui.picker.is_some() {
            self.show_lsp_logs();
        }
    }
}

impl App {
    pub(in crate::app) fn lsp_server_label(&self, server_key: &str) -> String {
        self.language.server_label(server_key)
    }
}

impl App {
    pub(in crate::app) fn update_lsp_progress(
        &mut self,
        server: String,
        token: String,
        kind: LspProgressKind,
        title: String,
        message: String,
        percentage: Option<u8>,
    ) {
        let was_loading = self.language.server_loading(&server);
        self.language.update_progress(
            (server.clone(), token, kind, title, message, percentage),
            &self.configuration.settings().ui.lsp_progress,
            Instant::now(),
        );
        if was_loading && !self.language.server_loading(&server) {
            self.schedule_lsp_inlay_refresh(true);
        }
        self.project_lsp_progress();
    }
}

impl App {
    pub(in crate::app) fn clear_lsp_progress(&mut self, server: &str) {
        self.language.clear_progress(server);
        self.project_lsp_progress();
    }
}

impl App {
    fn project_lsp_progress(&mut self) {
        let (rows, frame) = self.language.progress();
        if self.ui.lsp_progress != rows {
            self.ui.lsp_progress = rows.to_vec();
        }
        if self.lsp_waiting_server.as_ref().is_some_and(|server| {
            self.language
                .sessions()
                .get(server)
                .is_some_and(|session| session.capabilities.is_some())
                && !self.language.server_loading(server)
        }) {
            self.lsp_waiting_server = None;
            self.lsp_retry_at = None;
            if let Some(feature) = self.lsp_waiting.take() {
                self.request_lsp(feature);
            }
        }
        self.ui.set(frame, |ui| &mut ui.lsp_progress_frame);
    }
}

impl App {
    pub(in crate::app) fn update_lsp_progress_animation(&mut self, now: Instant) {
        self.language
            .animate_progress(&self.configuration.settings().ui.lsp_progress, now);
        self.project_lsp_progress();
    }
}

impl App {
    /// Whether the server is still working through its project.
    /// Sends the waiting request again once a "content modified" retry is due.
    pub(in crate::app) fn process_lsp_retry(&mut self) {
        if self.lsp_retry_at.is_none_or(|due| Instant::now() < due) {
            return;
        }
        self.lsp_retry_at = None;
        self.lsp_waiting_server = None;
        if let Some(feature) = self.lsp_waiting.take() {
            self.request_lsp(feature);
        }
    }
}

impl App {
    fn lsp_server_loading(&self, server: &str) -> bool {
        self.language.server_loading(server)
    }
}

impl App {
    fn show_lsp_loading(&mut self) {
        self.ui.lsp_loading = Some(self.message("lsp_loading", &[]));
    }
}

impl App {
    /// An empty answer from a server still loading its project says nothing about the code.
    pub(in crate::app) fn show_lsp_no_result(&mut self, server: &str) {
        if self.lsp_server_loading(server) {
            self.show_lsp_loading();
            self.lsp_waiting = self.lsp_asked.clone();
            self.lsp_waiting_server = Some(server.into());
        } else {
            self.show_error(self.message("lsp_no_result", &[]));
        }
    }
}

impl App {
    pub(in crate::app) fn handle_lsp_request_failure(
        &mut self,
        server: String,
        request_id: u64,
        method: String,
        message: String,
    ) {
        // The status line's own request: an empty trail is the whole result.
        if self.language.is_breadcrumb_request(request_id) {
            self.language.retry_breadcrumbs();
            return;
        }
        // A formatter that fails must not cost the save.
        if method == "textDocument/formatting"
            && let Some(buffer) = self.formatting_save_for_request(&server, request_id)
        {
            self.append_lsp_log(format!("format on save failed: {message}"));
            self.finish_format_on_save(buffer);
            return;
        }
        if method == "textDocument/inlayHint" {
            if self.language.fail_inlay(&server, request_id) {
                self.schedule_lsp_inlay_warmup_retry();
                let server = self.lsp_server_label(&server);
                self.append_lsp_log(format!("{server}: inlay hint refresh failed: {message}"));
            }
            // Inlay hints are background, revision-owned projections. A failure
            // for any superseded request is stale by definition, while a
            // current failure is retried above. Neither is user-actionable.
            return;
        }
        let current_asked = self.lsp_asked_request_id == Some(request_id)
            && self
                .lsp_asked
                .as_ref()
                .is_some_and(|feature| feature.method() == method);
        if current_asked {
            self.ui.lsp_loading = None;
        }
        // Colours are background work, and a second server failing a completion
        // must not hide the items the others returned.
        let secondary = method == "textDocument/completion"
            && (self.language.completion_automatic()
                || self
                    .language
                    .active()
                    .is_some_and(|active| *active != server));
        // A server that fails code actions must not hide what the others offered.
        if method == "textDocument/codeAction" {
            self.language.forget_request(&server, request_id);
            let main = self
                .language
                .active()
                .is_some_and(|active| *active == server);
            let label = self.lsp_server_label(&server);
            if main {
                self.show_error(self.message(
                    "lsp_request_failed",
                    &[
                        ("server", label),
                        ("method", method),
                        ("diagnostic", message),
                    ],
                ));
            } else {
                self.append_lsp_log(format!("{label}: {method} failed: {message}"));
            }
            if !self.language.request_pending(request_id)
                && (!main || self.language.has_offered_actions())
            {
                self.open_lsp_code_actions_picker(&server);
            }
            return;
        }
        if method == "textDocument/documentColor" || secondary {
            self.language.forget_request(&server, request_id);
            let server = self.lsp_server_label(&server);
            self.append_lsp_log(format!("{server}: {method} failed: {message}"));
            return;
        }
        // A server still indexing answers "content modified"; the protocol says to ask again,
        // so the request waits for the load to finish, as an empty answer does.
        // Right after opening a file the server can answer that way before it reports loading;
        // then the request is sent again shortly, a bounded number of times.
        if message.contains("-32801") && current_asked {
            let loading = self.lsp_server_loading(&server);
            if loading || self.lsp_retries < LSP_CONTENT_MODIFIED_RETRIES {
                if !loading {
                    self.lsp_retries += 1;
                    self.lsp_retry_at = Some(Instant::now() + LSP_CONTENT_MODIFIED_DELAY);
                }
                self.show_lsp_loading();
                self.lsp_waiting = self.lsp_asked.clone();
                self.lsp_waiting_server = loading.then_some(server);
                return;
            }
        }
        if !current_asked && message.contains("-32801") {
            self.language.forget_request(&server, request_id);
            return;
        }
        let server = self.lsp_server_label(&server);
        self.show_error(self.message(
            "lsp_request_failed",
            &[
                ("server", server),
                ("method", method),
                ("diagnostic", message),
            ],
        ));
    }
}

impl App {
    pub(in crate::app) fn process_lsp_events(&mut self) {
        for error in self.language.retry_configurations() {
            self.show_error(error);
        }
        for error in self.language.flush_edit_replies() {
            self.show_error(error);
        }
        while let Some(owned) = self.language.poll_event() {
            self.route_lsp_event(owned);
        }
    }

    pub(super) fn handle_lsp_event(&mut self, owned: OwnedLspEvent) {
        let crate::language::lsp::OwnedLspEvent {
            event,
            context,
            reply,
        } = owned;
        if !self.language.owns_event_context(&context) {
            self.language.reject_reply(
                reply,
                "requesting language-server session is no longer current".into(),
            );
            return;
        }
        match event {
            LspEvent::Starting { .. } => {}
            LspEvent::Running {
                server,
                capabilities,
            } => {
                self.ui.lsp_loading = None;
                if self.language.running(&server, capabilities) {
                    self.schedule_lsp_inlay_startup_refresh();
                }
                if self
                    .lsp_waiting_server
                    .as_ref()
                    .is_none_or(|waiting| waiting == &server)
                    && !self.language.server_loading(&server)
                {
                    self.lsp_waiting_server = None;
                    if let Some(feature) = self.lsp_waiting.take() {
                        self.request_lsp(feature);
                    }
                }
                self.request_lsp_colors();
                // Successful lifecycle transitions are reflected by the
                // status line and bounded progress surface. They must not
                // leave persistent bottom-left notifications.
            }
            LspEvent::CapabilitiesChanged {
                server,
                capabilities,
                ..
            } => {
                if self.language.running(&server, capabilities) {
                    self.schedule_lsp_inlay_refresh(true);
                }
                self.request_lsp_colors();
            }

            LspEvent::InlayHintsRefresh { server } => {
                if self.language.active().map(String::as_str) == Some(&server) {
                    self.schedule_lsp_inlay_refresh(true);
                }
            }
            LspEvent::Diagnostics {
                server,
                uri,
                version,
                diagnostics,
            } => {
                let result = self
                    .language
                    .diagnostic_batch(
                        &context,
                        &self.state,
                        (&uri, version, diagnostics),
                        (
                            false,
                            MAXIMUM_DIAGNOSTICS_PER_URI,
                            &self.lsp_server_label(&server),
                        ),
                    )
                    .and_then(|batch| self.diagnostics.publish(batch, &self.state));
                match result {
                    Ok(()) => self.sync_ui_diagnostics(),
                    Err(error) => self.append_lsp_log(error),
                }
            }

            LspEvent::Result {
                server,
                request_id,
                method,
                uri,
                version,
                result,
            } => {
                if context
                    .document_source(&uri)
                    .is_some_and(|(buffer, source)| {
                        self.state
                            .buffers
                            .get(*buffer)
                            .is_none_or(|buffer| buffer.text.source_version() != *source)
                    })
                {
                    return;
                }
                let save_format = method == "textDocument/formatting"
                    && self
                        .formatting_save_for_request(&server, request_id)
                        .is_some();
                if save_format {
                    self.present_lsp_result_owned(
                        &server,
                        request_id,
                        &method,
                        &uri,
                        result,
                        Some(context),
                    );
                    return;
                }
                let current = self.active_lsp_document_without_text();
                if !current.as_ref().is_some_and(|(_, document)| {
                    document.uri() == uri && document.version == version
                }) {
                    return;
                }
                if let LspFeatureResult::Colors(colors) = result {
                    self.install_lsp_colors(&server, &uri, colors, context.encoding());
                    return;
                }
                if self.language.is_breadcrumb_request(request_id) {
                    self.refresh_lsp_projection_owner();
                    if !self.language.accept_breadcrumbs(&server, &uri, version) {
                        return;
                    }
                    if let LspFeatureResult::Symbols(symbols) = result {
                        self.install_lsp_breadcrumbs(symbols, context.encoding());
                    }
                    return;
                }
                self.refresh_lsp_projection_owner();
                if !self.language.accept_result(
                    &server,
                    request_id,
                    &method,
                    &uri,
                    version,
                    self.state
                        .active_view()
                        .and_then(|id| self.state.views.get(id))
                        .map_or(ByteOffset(0), |view| view.cursor),
                ) {
                    return;
                }
                self.present_lsp_result_owned(
                    &server,
                    request_id,
                    &method,
                    &uri,
                    result,
                    Some(context),
                );
            }
            LspEvent::ApplyEditRequest {
                server: _,
                request_id: _,
                label,
                edit,
            } => {
                self.begin_workspace_proposal(
                    label.unwrap_or_else(|| self.message("workspace_edit_server_label", &[])),
                    edit,
                    reply,
                    context.encoding(),
                );
            }
            LspEvent::Progress {
                server,
                token,
                kind,
                title,
                message,
                percentage,
            } => self.update_lsp_progress(server, token, kind, title, message, percentage),
            LspEvent::Log {
                server, message, ..
            } => {
                let server = self.lsp_server_label(&server);
                self.append_lsp_log(format!("{server}: {message}"));
            }
            LspEvent::ServerStatus {
                server,
                healthy,
                message,
            } => {
                if self.language.status_changed(&server, healthy, &message) {
                    self.report_lsp_status(&server, &message);
                }
            }
            LspEvent::Health { server, health } => {
                let server = self.lsp_server_label(&server);
                self.notify_message(
                    self.message(
                        "lsp_health",
                        &[
                            ("server", server),
                            ("documents", health.attached_documents.to_string()),
                            ("pending", health.pending_requests.to_string()),
                            (
                                "encoding",
                                health.capabilities.position_encoding.lsp_name().into(),
                            ),
                        ],
                    ),
                    false,
                );
            }
            LspEvent::Unsupported { server, method } => {
                let server = self.lsp_server_label(&server);
                self.show_error(
                    self.message("lsp_unsupported", &[("server", server), ("method", method)]),
                );
            }
            LspEvent::RequestFailed {
                server,
                request_id,
                method,
                message,
            } => self.handle_lsp_request_failure(server, request_id, method, message),
            LspEvent::Stopped { server, success } => {
                if context.diagnostic_ambiguous() {
                    self.language.request_start(&server, None);
                }
                if !success {
                    self.reprobe_failed_path_runtime(&server);
                }
                let desired = self.language.stopped(&server);
                for prefix in ["lsp.", "lsp-pull:"] {
                    if let Ok(producer) = context.diagnostic_producer(prefix) {
                        self.diagnostics.clear_source(&producer);
                    }
                }
                self.sync_ui_diagnostics();
                let _ = self.language.stop(&server);
                self.language.forget_synced(&server);
                self.clear_lsp_progress(&server);
                self.ui.lsp_colors.retain(|span| span.server != server);
                let server_label = self.lsp_server_label(&server);
                self.notify_message(
                    self.message("lsp_stopped", &[("server", server_label)]),
                    false,
                );
                if self.language.active().map(String::as_str) == Some(&server) {
                    self.clear_lsp_session_state();
                }
                if desired {
                    self.schedule_lsp_restart(server);
                }
            }
            LspEvent::Failed { server, message } => {
                self.reprobe_failed_path_runtime(&server);
                let desired = self.language.stopped(&server);
                for prefix in ["lsp.", "lsp-pull:"] {
                    if let Ok(producer) = context.diagnostic_producer(prefix) {
                        self.diagnostics.clear_source(&producer);
                    }
                }
                self.sync_ui_diagnostics();
                let _ = self.language.stop(&server);
                self.language.forget_synced(&server);
                self.clear_lsp_progress(&server);
                self.ui.lsp_colors.retain(|span| span.server != server);
                let server_label = self.lsp_server_label(&server);
                self.language.failed(server_label.clone(), message.clone());
                self.show_error(self.message(
                    "lsp_failed",
                    &[("server", server_label), ("diagnostic", message)],
                ));
                if self.language.active().map(String::as_str) == Some(&server) {
                    self.clear_lsp_session_state();
                }
                if desired {
                    self.schedule_lsp_restart(server);
                }
            }
        }
    }
}

impl App {
    pub(in crate::app) fn schedule_lsp_restart(&mut self, key: String) {
        if let Some(server) = self.language.schedule_restart(&key, Instant::now()) {
            self.show_error(self.message("lsp_restart_disabled", &[("server", server)]));
        }
    }
}

impl App {
    pub(in crate::app) fn restart_lsp_if_due(&mut self) {
        if let Some(key) = self.language.restart_due(Instant::now()) {
            self.restart_lsp_session(key);
        }
    }
}

impl App {
    pub(in crate::app) fn request_lsp_inlay_hints(&mut self) {
        self.refresh_lsp_projection_owner();
        let Some(owner) = self.language.projection() else {
            return;
        };
        if self.ui.large_file_buffers.contains(&owner.buffer)
            || !self
                .language
                .sessions()
                .get(&owner.server)
                .and_then(|session| session.capabilities.as_ref())
                .is_some_and(|capabilities| capabilities.inlay_hints)
        {
            self.ui
                .lsp_inlay_hints
                .retain(|hint| hint.buffer != owner.buffer);
            return;
        }
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        if buffer == owner.buffer {
            self.language.request_inlays(document);
        }
    }
}

impl App {
    /// Keeps the symbols of the document on screen fresh for the status line.
    pub(in crate::app) fn process_lsp_breadcrumbs(&mut self) {
        let settings = self.configuration.settings();
        let shown = settings.ui.status_line.enabled
            && settings
                .ui
                .status_line
                .components
                .iter()
                .any(|component| component == "breadcrumbs");
        let Some((buffer, document)) = self.active_lsp_document_without_text().filter(|_| shown)
        else {
            self.ui.set(None, |ui| &mut ui.breadcrumbs);
            return;
        };
        if self.ui.large_file_buffers.contains(&buffer) {
            return;
        }
        if let Some(server) = self
            .language
            .request_breadcrumbs(buffer, &document, Instant::now())
            && let Some((_, document)) = self.active_lsp_document()
        {
            self.language.send_breadcrumbs(&server, buffer, document);
        }
    }
}

impl App {
    /// The symbols a cursor can be inside, in the server's order: outer ones before inner ones.
    fn install_lsp_breadcrumbs(
        &mut self,
        symbols: Vec<crate::language::lsp::LspSymbol>,
        encoding: PositionEncoding,
    ) {
        // As LazyVim's status line: Variables and such aren't places in the file.
        const PLACES: [u8; 12] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 23];
        let Some(buffer) = self.state.active_buffer_id() else {
            return;
        };
        let codec = PositionCodec::new(&self.state.buffers[buffer].text);
        let symbols = symbols
            .into_iter()
            .filter(|symbol| symbol.kind.is_some_and(|kind| PLACES.contains(&kind)))
            .filter_map(|symbol| {
                let extent = symbol.extent?;
                let start = codec.lsp_to_byte(extent.start.line, extent.start.character, encoding);
                let end = codec.lsp_to_byte(extent.end.line, extent.end.character, encoding);
                Some(crate::ui::UiBreadcrumb {
                    name: symbol.name.trim_start().to_owned(),
                    range: ByteRange::new(start.ok()?, end.ok()?).ok()?,
                })
            })
            .collect();
        self.ui
            .set(Some((buffer, symbols)), |ui| &mut ui.breadcrumbs);
    }
}

impl App {
    /// Asks every attached server with a colour provider for the active document's colours.
    pub(in crate::app) fn request_lsp_colors(&mut self) {
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        if !self.ui.large_file_buffers.contains(&buffer) {
            self.language.request_colors(buffer, &document);
        }
    }
}

impl App {
    fn install_lsp_colors(
        &mut self,
        server: &str,
        uri: &str,
        colors: Vec<crate::language::lsp::LspColor>,
        encoding: PositionEncoding,
    ) {
        let Some(path) = path_from_file_uri(uri) else {
            return;
        };
        let Some((buffer_id, buffer)) = self
            .state
            .buffers
            .iter()
            .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
        else {
            return;
        };
        let codec = PositionCodec::new(&buffer.text);
        let revision = buffer.text.revision();
        let spans = colors.into_iter().filter_map(|color| {
            let start = codec
                .lsp_to_byte(
                    color.range.start.line,
                    color.range.start.character,
                    encoding,
                )
                .ok()?;
            let end = codec
                .lsp_to_byte(color.range.end.line, color.range.end.character, encoding)
                .ok()?;
            Some(crate::ui::UiColorSpan {
                server: server.to_owned(),
                buffer: buffer_id,
                revision,
                range: ByteRange::new(start, end).ok()?,
                rgb: color.rgb,
            })
        });
        let mut next = self
            .ui
            .lsp_colors
            .iter()
            .filter(|span| {
                (span.server != server || span.buffer != buffer_id)
                    && self.state.buffers.get(span.buffer).is_some()
            })
            .cloned()
            .chain(spans)
            .collect::<Vec<_>>();
        next.sort_unstable_by_key(|span| (span.buffer, span.range.start, span.range.end));
        self.ui.set(next, |ui| &mut ui.lsp_colors);
        self.project_language_marks();
    }
}

impl App {
    pub(in crate::app) fn request_lsp(&mut self, mut feature: LspFeature) {
        if matches!(feature, LspFeature::Formatting { .. })
            && !self
                .state
                .active_buffer_id()
                .is_some_and(|buffer| self.format_on_save.contains_key(&buffer))
            && self.begin_document_format()
        {
            return;
        }
        if matches!(feature, LspFeature::InlayHints { .. }) {
            self.schedule_lsp_inlay_refresh(true);
            self.process_lsp_inlay_refresh();
            return;
        }
        if matches!(&feature, LspFeature::WorkspaceSymbols { query } if query.is_empty()) {
            self.open_workspace_symbols_prompt();
            return;
        }
        self.close_documentation();
        let Some(view_id) = self.state.active_view() else {
            return;
        };
        let view = &self.state.views[view_id];
        let offset = view.cursor;
        let selection = view.selection;
        let Some((buffer, document)) = self.active_lsp_document() else {
            return;
        };
        let server = if matches!(feature, LspFeature::Formatting { .. }) {
            // Not every server on a file formats it: Tailwind sits beside the TypeScript one.
            self.language
                .formatting_server(buffer, &document)
                .or_else(|| self.language.supporting_server(buffer, &document, &feature))
        } else {
            self.language.supporting_server(buffer, &document, &feature)
        };
        // Someone is waiting on these, unlike a save's format or what the editor asks by itself.
        let asked = !self.format_on_save.contains_key(&buffer)
            && !matches!(
                feature,
                LspFeature::SemanticTokensFull | LspFeature::SemanticTokensDelta { .. }
            )
            && !(matches!(feature, LspFeature::Completion) && self.language.completion_automatic());
        let Some(server) = server else {
            self.start_lsp(None);
            if asked
                && self
                    .language
                    .sessions()
                    .values()
                    .any(|session| session.capabilities.is_none())
            {
                self.show_lsp_loading();
                self.lsp_waiting = Some(feature);
                self.lsp_waiting_server = None;
            }
            return;
        };
        // Switching the server or the buffer's owner clears completion state, including where
        // this one started.
        let completion = matches!(feature, LspFeature::Completion).then(|| {
            (
                self.language.completion_start(),
                self.language.completion_automatic(),
            )
        });
        // A formatter beside the file's main server, such as Ruff by basedpyright, doesn't replace it.
        let formats = matches!(
            feature,
            LspFeature::Formatting { .. } | LspFeature::RangeFormatting { .. }
        );
        if !formats && self.language.active() != Some(&server) {
            self.select_lsp_session(&server);
        }
        // The application owns a session record while its process is still
        // initializing. Do not expose that record as a runnable server or
        // send a request that can race the worker's startup/disconnect path.
        // `Running` publishes capabilities before this becomes requestable.
        if self
            .language
            .sessions()
            .get(&server)
            .is_none_or(|session| session.capabilities.is_none())
        {
            if asked {
                self.show_lsp_loading();
                self.lsp_waiting = Some(feature);
                self.lsp_waiting_server = Some(server.clone());
            }
            return;
        }
        if asked && (matches!(feature, LspFeature::Completion) || self.lsp_server_loading(&server))
        {
            self.show_lsp_loading();
        }
        if asked {
            self.lsp_asked = Some(feature.clone());
            self.lsp_asked_request_id = None;
        }
        if matches!(feature, LspFeature::CodeAction { .. }) {
            self.language.clear_offered_actions();
            let encoding = self
                .language
                .capabilities()
                .map_or(PositionEncoding::Utf16, |capabilities| {
                    capabilities.position_encoding
                });
            let buffer = TextBuffer::from_text(&document.text);
            let codec = PositionCodec::new(&buffer);
            let (start, end) = selection.map_or((offset, offset), |selection| {
                if selection.anchor <= selection.head {
                    (selection.anchor, selection.head)
                } else {
                    (selection.head, selection.anchor)
                }
            });
            let Ok((start_line, start_character)) = codec.byte_to_lsp(start, encoding) else {
                return;
            };
            let Ok((end_line, end_character)) = codec.byte_to_lsp(end, encoding) else {
                return;
            };
            // As LazyVim asks: everything on the cursor's lines, so a fix is offered from anywhere
            // on the line that has the problem.
            let line_end = buffer
                .line_to_byte(end_line.saturating_add(1))
                .unwrap_or_else(|_| ByteOffset(buffer.len_bytes()));
            let line_start = buffer.line_to_byte(start_line).unwrap_or(start);
            let position = |offset| {
                codec
                    .byte_to_lsp(offset, encoding)
                    .ok()
                    .map(|(line, character)| crate::language::lsp::LspPosition { line, character })
            };
            let diagnostics = self
                .diagnostics
                .within(&document.uri(), line_start, line_end)
                .into_iter()
                .filter_map(|problem| {
                    Some(crate::language::lsp::LspDiagnostic {
                        range: crate::language::lsp::LspRange {
                            start: position(problem.range.start)?,
                            end: position(problem.range.end)?,
                        },
                        severity: Some(match problem.severity {
                            crate::workspace::DiagnosticSeverity::Error => 1,
                            crate::workspace::DiagnosticSeverity::Warning => 2,
                            crate::workspace::DiagnosticSeverity::Information => 3,
                            crate::workspace::DiagnosticSeverity::Hint => 4,
                        }),
                        code: problem.code.clone(),
                        related: Vec::new(),
                        source: Some(problem.source.clone()),
                        message: problem.message.clone(),
                        data: problem.data.clone(),
                    })
                })
                .collect();
            feature = LspFeature::CodeAction {
                range: crate::language::lsp::LspRange {
                    start: crate::language::lsp::LspPosition {
                        line: start_line,
                        character: start_character,
                    },
                    end: crate::language::lsp::LspPosition {
                        line: end_line,
                        character: end_character,
                    },
                },
                diagnostics,
            };
        }
        match &mut feature {
            LspFeature::Formatting {
                tab_size,
                insert_spaces,
            }
            | LspFeature::RangeFormatting {
                tab_size,
                insert_spaces,
                ..
            } => {
                *tab_size = self.state.policy.tab_width;
                *insert_spaces = self.state.policy.expand_tabs;
            }
            _ => {}
        }
        self.refresh_lsp_projection_owner();
        if let Some((start, automatic)) = completion {
            self.language.begin_completion(start, automatic);
        }
        match self
            .language
            .request_projected(&server, feature, document, offset)
        {
            Ok(()) => {
                if asked {
                    self.lsp_asked_request_id = Some(self.language.request_id());
                }
                if formats
                    && let Some(pending) = self
                        .format_on_save
                        .get_mut(&buffer)
                        .filter(|pending| pending.request.is_none())
                {
                    pending.request = Some((server.clone(), self.language.request_id()));
                }
            }
            // The worker has already reported (or will imminently report) its
            // real startup/exit failure. Avoid replacing that diagnostic with
            // a misleading raw transport-key "not running" error.
            Err(crate::language::lsp::LspError::NotRunning(_)) => {
                self.language.stopped(&server);
            }
            Err(error) => self.show_error(error),
        }
    }
}

impl App {
    pub(in crate::app) fn open_lsp_completion_documentation(&mut self, asked: bool) {
        let Some(selected) = self
            .ui
            .language_completion
            .as_ref()
            .map(|completion| completion.selected)
        else {
            return;
        };
        let (server, label, documentation) = match self
            .language
            .completion_documentation(&self.state, selected)
        {
            Ok(value) => value,
            Err(error) => {
                let stale = error == "completion documentation is stale";
                if stale {
                    self.close_documentation();
                }
                // Docs that open by themselves simply lag the menu; that is nothing to report.
                if !stale || asked {
                    self.show_error(error);
                }
                return;
            }
        };
        let server_label = self.lsp_server_label(&server);
        self.present_documentation_popup(
            DocumentationPopupKind::Completion,
            self.message(
                "lsp_completion_documentation_title",
                &[("server", server_label), ("label", label)],
            ),
            server,
            None,
            vec![documentation],
        );
    }
}

impl App {
    pub(in crate::app) fn present_lsp_result(
        &mut self,
        server: &str,
        request_id: u64,
        method: &str,
        uri: &str,
        result: LspFeatureResult,
    ) {
        self.present_lsp_result_owned(server, request_id, method, uri, result, None);
    }
}

impl App {
    pub(in crate::app) fn present_lsp_result_owned(
        &mut self,
        server: &str,
        request_id: u64,
        method: &str,
        uri: &str,
        result: LspFeatureResult,
        context: Option<crate::language::lsp::EditContext>,
    ) {
        self.ui.lsp_loading = None;
        let server_label = self.lsp_server_label(server);
        // A null result has the same meaning as an empty list for these features.
        let result = match (method, result) {
            ("textDocument/codeAction", LspFeatureResult::Empty) => {
                LspFeatureResult::CodeActions(Vec::new())
            }
            (
                "textDocument/documentHighlight"
                | "textDocument/foldingRange"
                | "textDocument/selectionRange",
                LspFeatureResult::Empty,
            ) => LspFeatureResult::Ranges(Vec::new()),
            (
                "textDocument/semanticTokens/full" | "textDocument/semanticTokens/full/delta",
                LspFeatureResult::Empty,
            ) => LspFeatureResult::SemanticTokens(crate::language::lsp::LspSemanticTokens {
                result_id: None,
                data: Vec::new(),
            }),
            ("textDocument/inlayHint", LspFeatureResult::Empty) => {
                LspFeatureResult::InlayHints(Vec::new())
            }
            (_, result) => result,
        };
        match result {
            // Another server on this file already answered.
            LspFeatureResult::Hover(_) if self.hover_is_showing() => {}
            LspFeatureResult::Hover(hover) => {
                let source_range = hover.range.and_then(|range| {
                    let buffer_id = self.state.active_buffer_id()?;
                    let buffer = self.state.buffers.get(buffer_id)?;
                    let codec = PositionCodec::new(&buffer.text);
                    let encoding = self.lsp_position_encoding();
                    let start = codec
                        .lsp_to_byte(range.start.line, range.start.character, encoding)
                        .ok()?;
                    let end = codec
                        .lsp_to_byte(range.end.line, range.end.character, encoding)
                        .ok()?;
                    ByteRange::new(start, end).ok()
                });
                self.present_documentation_popup(
                    DocumentationPopupKind::Hover,
                    self.message("lsp_hover_title", &[("server", server_label.clone())]),
                    server.into(),
                    source_range,
                    hover.sections,
                );
            }
            LspFeatureResult::Signature(content) => {
                self.present_documentation_popup(
                    DocumentationPopupKind::Signature,
                    self.message("lsp_signature_title", &[("server", server_label.clone())]),
                    server.into(),
                    None,
                    vec![content],
                );
            }
            LspFeatureResult::Completion { items, incomplete } => {
                self.language
                    .install_completions(server, items, incomplete, request_id);
                self.narrow_lsp_completion();
                if self.ui.language_completion.is_none()
                    && !self.language.request_pending(request_id)
                    && !self.language.completion_automatic()
                {
                    self.show_lsp_no_result(server);
                }
            }
            LspFeatureResult::CompletionResolved(item) => {
                let accept = self.language.completion_resolve_accepts(request_id);
                if self
                    .language
                    .accept_resolved(request_id, *item, &self.state)
                {
                    self.project_lsp_completion();
                    if accept {
                        self.accept_lsp_completion();
                    }
                }
            }
            LspFeatureResult::Locations(locations) => {
                if locations.is_empty() {
                    self.show_lsp_no_result(server);
                } else if method == "textDocument/references" || locations.len() > 1 {
                    self.open_lsp_locations_picker(
                        locations,
                        self.language.server_encoding(server),
                    );
                } else if let Some(location) = locations.first() {
                    self.navigate_lsp_location_encoded(
                        location,
                        self.language.server_encoding(server),
                    );
                }
            }
            LspFeatureResult::Symbols(mut symbols) => {
                for location in symbols
                    .iter_mut()
                    .filter_map(|symbol| symbol.location.as_mut())
                {
                    if location.uri.is_empty() {
                        location.uri = uri.into();
                    }
                }
                self.open_lsp_symbols_picker(server, symbols);
            }
            LspFeatureResult::WorkspaceEdit(mut edit) => {
                for document in &mut edit.documents {
                    if document.uri.is_empty() {
                        document.uri = uri.into();
                    }
                }
                // Formatting was asked for and touches one file, so it applies without a review.
                if matches!(
                    method,
                    "textDocument/formatting" | "textDocument/rangeFormatting"
                ) {
                    // Typing while the server worked moved the text its edits point at.
                    let edit_is_current = context.as_ref().is_some_and(|context| {
                        self.language.edit_context_current(context)
                            && context
                                .document_source(uri)
                                .is_some_and(|(buffer, version)| {
                                    self.state.buffers.get(*buffer).is_some_and(|buffer| {
                                        buffer.text.source_version() == *version
                                    })
                                })
                    });
                    let has_edits = edit
                        .documents
                        .iter()
                        .any(|document| !document.edits.is_empty());
                    if edit_is_current && has_edits {
                        let encoding = self.language.server_encoding(server);
                        let replaced = edit
                            .documents
                            .iter()
                            .flat_map(|document| &document.edits)
                            .filter(|edit| edit.range.end.line > edit.range.start.line)
                            .map(|edit| edit.range.start.line..=edit.range.end.line)
                            .collect::<Vec<_>>();
                        let positions = self.view_positions(|line| {
                            replaced.iter().any(|lines| lines.contains(&line))
                        });
                        if let Err(error) = self.apply_lsp_workspace_edit(edit, encoding) {
                            self.show_error(error);
                        }
                        self.restore_view_positions(positions);
                    }
                    if let Some(buffer) = self.formatting_save_for_request(server, request_id) {
                        self.finish_format_on_save(buffer);
                    }
                    return;
                }
                self.begin_workspace_proposal_owned(
                    self.message("workspace_edit_lsp_label", &[]),
                    edit,
                    None,
                    self.lsp_position_encoding(),
                    context,
                );
            }
            LspFeatureResult::CodeActions(actions) => {
                self.language.offer_actions(server, uri, actions, context);
                if !self.language.request_pending(request_id) {
                    self.open_lsp_code_actions_picker(server);
                }
            }
            LspFeatureResult::PullDiagnostics(result) => {
                if let Some(id) = result.result_id.clone() {
                    self.language.record_pull(server, uri, id);
                }
                if result.unchanged {
                    return;
                }
                let normalized = context
                    .as_ref()
                    .ok_or_else(|| "pull diagnostics lack a requesting context".to_string())
                    .and_then(|context| {
                        self.language.diagnostic_batch(
                            context,
                            &self.state,
                            (uri, None, result.items),
                            (true, MAXIMUM_DIAGNOSTICS_PER_URI, &server_label),
                        )
                    });
                match normalized.and_then(|batch| self.diagnostics.publish(batch, &self.state)) {
                    Ok(()) => self.sync_ui_diagnostics(),
                    Err(error) => self.append_lsp_log(error),
                }
            }
            LspFeatureResult::SemanticTokens(result) => {
                if !self.language.record_semantic(server, uri, &result) {
                    return;
                }
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let legend = self
                        .language
                        .capabilities()
                        .map(|value| value.semantic_token_types.as_slice())
                        .unwrap_or(&[]);
                    // As in Neovim, a token colours only when the theme gives its
                    // `@lsp.type.*` group a colour; otherwise tree-sitter's stays.
                    let theme = self.configuration.theme();
                    let groups: Vec<_> = legend
                        .iter()
                        .map(|name| {
                            crate::config::Highlight::for_semantic_token(name)
                                .filter(|group| theme.highlight(*group).is_some())
                        })
                        .collect();
                    let mut line = 0usize;
                    let mut character = 0usize;
                    let mut spans = Vec::new();
                    for token in result.data.chunks_exact(5).take(100_000) {
                        line = line.saturating_add(token[0] as usize);
                        character = if token[0] == 0 {
                            character.saturating_add(token[1] as usize)
                        } else {
                            token[1] as usize
                        };
                        let Some(role) = groups.get(token[3] as usize).copied().flatten() else {
                            continue;
                        };
                        let Ok(start) = codec.lsp_to_byte(line, character, encoding) else {
                            continue;
                        };
                        let Ok(end) = codec.lsp_to_byte(
                            line,
                            character.saturating_add(token[2] as usize),
                            encoding,
                        ) else {
                            continue;
                        };
                        let Ok(range) = ByteRange::new(start, end) else {
                            continue;
                        };
                        spans.push(UiSyntaxSpan {
                            buffer: buffer_id,
                            revision: buffer.text.revision(),
                            range,
                            role,
                        });
                    }
                    self.ui.lsp_semantic_spans = spans;
                }
            }
            LspFeatureResult::SemanticTokenDelta(delta) => {
                if let Some(full) = self.language.semantic_delta(server, uri, delta) {
                    self.present_lsp_result(
                        server,
                        request_id,
                        method,
                        uri,
                        LspFeatureResult::SemanticTokens(full),
                    );
                } else {
                    self.request_lsp(LspFeature::SemanticTokensFull);
                }
            }
            LspFeatureResult::InlayHints(items) => {
                let provisional = items
                    .iter()
                    .any(|item| item.label.to_ascii_lowercase().contains("{unknown}"));
                // A server still loading answers with nothing; keep the hints on screen
                // until the retry brings real ones.
                let retrying = if items.is_empty() || provisional {
                    self.schedule_lsp_inlay_warmup_retry()
                } else {
                    self.language.inlays_received();
                    false
                };
                let keep_shown = retrying
                    && self.state.active_buffer_id().is_some_and(|active| {
                        self.ui
                            .lsp_inlay_hints
                            .iter()
                            .any(|hint| hint.buffer == active)
                    });
                if let Some(path) = path_from_file_uri(uri).filter(|_| !keep_shown)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let hints = items
                        .into_iter()
                        .filter_map(|item| {
                            Some(UiInlayHint {
                                buffer: buffer_id,
                                revision: buffer.text.revision(),
                                position: codec
                                    .lsp_to_byte(
                                        item.position.line,
                                        item.position.character,
                                        encoding,
                                    )
                                    .ok()?,
                                label: item.label,
                            })
                        })
                        .collect::<Vec<_>>();
                    self.ui
                        .lsp_inlay_hints
                        .retain(|hint| hint.buffer != buffer_id);
                    self.ui.lsp_inlay_hints.extend(hints);
                    self.project_language_marks();
                }
            }
            LspFeatureResult::CodeLens(items) => {
                let Some(context) =
                    context.filter(|context| self.language.edit_context_current(context))
                else {
                    self.show_error("code-lens source is stale");
                    return;
                };
                let rows = items
                    .into_iter()
                    .enumerate()
                    .filter_map(|(index, item)| {
                        let label = if item.command.is_some() {
                            item.title.clone()
                        } else if item.resolve_item.is_some()
                            && self
                                .language
                                .sessions()
                                .get(server)
                                .and_then(|session| session.capabilities.as_ref())
                                .is_some_and(|caps| caps.code_lens_resolve)
                        {
                            format!("Line {}", item.range.start.line + 1)
                        } else {
                            return None;
                        };
                        Some((
                            PickerItem {
                                badge: String::new(),
                                kind: crate::workspace::PickerItemKind::Entry,
                                id: PickerItemId(format!("code-lens:{index}")),
                                label,
                                detail: item
                                    .command
                                    .as_ref()
                                    .map(|command| command.command.clone())
                                    .unwrap_or_default(),
                                source: String::new(),
                                action: PickerAction::None,
                            },
                            PickerPayload::LspCodeLens {
                                server: server.into(),
                                context: Box::new(context.clone()),
                                lens: item,
                            },
                        ))
                    })
                    .collect::<Vec<_>>();
                if rows.is_empty() {
                    self.show_lsp_no_result(server);
                } else {
                    self.install_picker(self.message("picker_code_lens_title", &[]), rows);
                }
            }
            LspFeatureResult::Ranges(items) => {
                if let Some(path) = path_from_file_uri(uri)
                    && let Some((buffer_id, buffer)) = self
                        .state
                        .buffers
                        .iter()
                        .find(|(_, buffer)| buffer.path.as_ref() == Some(&path))
                {
                    let encoding = self.lsp_position_encoding();
                    let codec = PositionCodec::new(&buffer.text);
                    let ranges = items
                        .into_iter()
                        .filter_map(|item| {
                            ByteRange::new(
                                codec
                                    .lsp_to_byte(item.start.line, item.start.character, encoding)
                                    .ok()?,
                                codec
                                    .lsp_to_byte(item.end.line, item.end.character, encoding)
                                    .ok()?,
                            )
                            .ok()
                        })
                        .collect::<Vec<_>>();
                    match method {
                        "textDocument/documentHighlight" => {
                            self.ui.lsp_highlight_spans = ranges
                                .into_iter()
                                .map(|range| UiSyntaxSpan {
                                    buffer: buffer_id,
                                    revision: buffer.text.revision(),
                                    range,
                                    role: crate::config::Highlight::VARIABLE,
                                })
                                .collect()
                        }
                        "textDocument/foldingRange" => {
                            if let Some(view) = self.state.active_view() {
                                self.ui.lsp_folds = ranges
                                    .into_iter()
                                    .filter_map(|range| {
                                        Some(UiFold {
                                            view,
                                            start_line: buffer
                                                .text
                                                .byte_to_line(range.start)
                                                .ok()?,
                                            end_line: buffer.text.byte_to_line(range.end).ok()?,
                                            collapsed: false,
                                        })
                                    })
                                    .collect()
                            }
                        }
                        "textDocument/selectionRange" => {
                            let expand =
                                self.language.complete_selection_ranges(request_id, ranges);
                            if let Some(cursor) = expand
                                && self
                                    .state
                                    .active_view()
                                    .is_some_and(|view| self.state.views[view].cursor == cursor)
                            {
                                self.process_effect(AppEffect::LspSelectionExpand);
                            }
                        }
                        _ => {}
                    }
                }
            }
            LspFeatureResult::Colors(_) => {}
            // Nothing to change is a normal answer from a formatter.
            LspFeatureResult::Empty
                if matches!(
                    method,
                    "textDocument/formatting" | "textDocument/rangeFormatting"
                ) =>
            {
                if let Some(buffer) = self.formatting_save_for_request(server, request_id) {
                    self.finish_format_on_save(buffer);
                }
            }
            // Only the last of several servers can say that nobody knows.
            LspFeatureResult::Empty
                if method == "textDocument/hover"
                    && (self.language.request_pending(request_id) || self.hover_is_showing()) => {}
            // Completion asks by itself as you type, and nothing to offer is no news.
            LspFeatureResult::Empty
                if method == "textDocument/completion"
                    && (self.language.request_pending(request_id)
                        || self.language.completion_automatic()
                        || self.ui.language_completion.is_some()) => {}
            LspFeatureResult::Empty if method == "workspace/executeCommand" => {}
            LspFeatureResult::Empty => self.show_lsp_no_result(server),
        }
    }
}

use crate::app::configuration::configured_text_width;
use crate::app::tools::nix_workspace_shell_for_host;
use crate::editor::EditorMode;
use crate::language::lsp::language_id_for_path;
use crate::tools::ToolKind;
use crate::tools::ToolStatus;
use crate::tools::is_nixos_host;
use crate::tools::nix_lsp_launch_environment;
use crate::tools::nix_lsp_profile_installed;
use crate::ui::CompletionItem;
use crate::ui::LanguageCompletionState;
use crate::ui::PromptKind;
use crate::workspace::DiagnosticOwner;
use crate::workspace::ExecutionSource;

impl App {
    pub(in crate::app) fn sync_lsp_status_ui(&mut self) {
        let running = self.language.running_servers();
        let server = self
            .active_lsp_document_without_text()
            .map(|(buffer, document)| {
                let session = self
                    .language
                    .sessions()
                    .iter()
                    .filter(|(_, session)| session.documents.contains_key(&buffer))
                    .min_by_key(|(key, session)| {
                        (
                            self.language.active() != Some(*key),
                            session.capabilities.is_none(),
                            session.definition.languages.len(),
                        )
                    });
                if let Some((key, session)) = session {
                    let state = if session.capabilities.is_some() {
                        "✓"
                    } else if session.circuit_open {
                        "failed"
                    } else if session.restart_due.is_some() {
                        "restarting"
                    } else if running.contains(key) {
                        "starting"
                    } else {
                        "stopped"
                    };
                    return format!("{} {state}", session.server_id);
                }
                if !self.configuration.settings().lsp.enabled {
                    return "LSP disabled".into();
                }
                let entry = self
                    .tooling
                    .entries()
                    .iter()
                    .filter(|entry| {
                        entry.definition.kind == ToolKind::Lsp
                            && entry
                                .definition
                                .automatically_supports_language(&document.language_id)
                    })
                    .min_by_key(|entry| {
                        (
                            !matches!(entry.status, ToolStatus::Installed(_)),
                            entry.definition.languages.len(),
                        )
                    });
                let Some(entry) = entry else {
                    return "no server: <leader>cm".into();
                };
                let id = &entry.definition.id;
                if self.language.launch_pending(id) {
                    format!("{id} starting")
                } else if self.language.errors().contains_key(id)
                    || matches!(entry.status, ToolStatus::Failed(_))
                {
                    format!("{id} failed")
                } else if matches!(entry.status, ToolStatus::Installing { .. }) {
                    format!("{id} installing")
                } else if !matches!(entry.status, ToolStatus::Installed(_)) {
                    "no server: <leader>cm".into()
                } else if self.workspace.safe_mode() {
                    "LSP blocked: safe mode".into()
                } else if self.workspace.execution_allowed().is_err() {
                    "LSP blocked: :trust".into()
                } else {
                    format!("{id} stopped")
                }
            });
        self.ui.set(server, |ui| &mut ui.lsp_server);
        let refresh = self.ui.tool_menu.as_ref().is_some_and(|menu| {
            menu.rows.iter().any(|row| {
                let blocked = if self.workspace.safe_mode() {
                    "blocked: safe mode"
                } else if self.workspace.execution_allowed().is_err() {
                    "blocked: untrusted"
                } else {
                    "installed"
                };
                if matches!(
                    row.lifecycle.as_str(),
                    "installed" | "blocked: untrusted" | "blocked: safe mode"
                ) && self.tooling.entries().iter().any(|entry| {
                    entry.definition.id == row.id && entry.definition.kind != ToolKind::Agent
                }) && row.lifecycle != blocked
                {
                    return true;
                }
                self.tooling
                    .entries()
                    .iter()
                    .find(|entry| {
                        entry.definition.id == row.id && entry.definition.kind == ToolKind::Lsp
                    })
                    .is_some_and(|entry| {
                        !matches!(entry.status, ToolStatus::Installing { .. })
                            && self.lsp_menu_lifecycle(&row.id).0 != row.session_state
                    })
            })
        });
        if refresh {
            let category = self
                .ui
                .tool_menu
                .as_ref()
                .map_or_else(|| "lsp".into(), |menu| menu.category.clone());
            let selected = self.selected_tool_id();
            self.rebuild_tool_menu(&category, selected.as_deref());
        }
    }
}

impl App {
    pub(in crate::app) fn reconcile_lsp_autostart(
        &mut self,
        buffer: crate::editor::BufferId,
        document: &LspDocument,
        matching_session: bool,
    ) {
        // A server runs for a workspace root; a file outside every root, such as the
        // tutorial's copy, gets none rather than an error.
        if self.workspace.require_path(&document.path).is_err() {
            return;
        }
        // Wait before recording an observation or copying a launch document: another
        // language's discovery can still own the shared lane.
        if !self
            .workspace
            .execution_lane(crate::app::workspace::DiscoveryKind::Language)
            .discovery_available()
        {
            return;
        }
        // Every matching server attaches, as in VS Code and Neovim: Tailwind runs
        // beside the HTML or TypeScript server. They start one at a time because
        // project-root discovery takes one launch at a time.
        let attached = self
            .language
            .sessions()
            .values()
            .filter(|state| state.documents.contains_key(&buffer))
            .map(|state| state.server_id.clone())
            .collect::<Vec<_>>();
        let mut launchable = self
            .tooling
            .entries()
            .iter()
            .filter(|entry| {
                entry.definition.kind == ToolKind::Lsp
                    && entry
                        .definition
                        .automatically_supports_language(&document.language_id)
                    && !matches!(entry.status, ToolStatus::Installing { .. })
                    && self.selected_lsp_runtime_available(&entry.definition)
            })
            .collect::<Vec<_>>();
        // The most specific server starts first and stays primary for hover and
        // definitions: typescript-language-server before Tailwind.
        launchable.sort_by_key(|entry| entry.definition.languages.len());
        let launchable = launchable
            .into_iter()
            .map(|entry| entry.definition.id.clone())
            .collect::<Vec<_>>();
        if !self
            .workspace
            .execution_lane(crate::app::workspace::DiscoveryKind::Language)
            .discovery_available()
            || launchable.iter().any(|id| self.language.launch_pending(id))
        {
            return;
        }
        let launchable_server = launchable
            .iter()
            .find(|id| {
                !attached.contains(id)
                    && !self.language.is_stopped(id)
                    && !self.language.explicitly_detached(id, buffer)
                    && !self.language.project_root_missing(
                        id,
                        &document.path,
                        &self.workspace.version(),
                        self.configuration.binding_revision(),
                    )
            })
            .cloned();
        // Like mason-lspconfig, install the server when none can start, or point at it when
        // automatic installs are off or this host has no way to install it.
        if launchable.is_empty() && !matching_session && self.configuration.settings().lsp.enabled {
            let installable = self
                .tooling
                .entries()
                .iter()
                .filter(|entry| {
                    entry.definition.kind == ToolKind::Lsp
                        && entry
                            .definition
                            .automatically_supports_language(&document.language_id)
                        && (entry.status == ToolStatus::Available
                            && entry.definition.install.is_some()
                            || self
                                .rust_analyzer_rustup_install(&entry.definition)
                                .is_some())
                })
                // The most specific server: zls for Zig, not a spell checker that covers it.
                .min_by_key(|entry| entry.definition.languages.len())
                .map(|entry| {
                    let source = if self
                        .rust_analyzer_rustup_install(&entry.definition)
                        .is_some()
                    {
                        Some(LspRuntimeSource::Path)
                    } else {
                        Some(self.selected_lsp_runtime_source(&entry.definition)).filter(|source| {
                            matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix)
                        })
                    };
                    (
                        entry.definition.id.clone(),
                        entry.definition.name.clone(),
                        source,
                    )
                });
            if let Some((id, server, source)) = installable
                && self.language.hint_install_once(&document.language_id)
            {
                if source.is_some()
                    && self.configuration.settings().lsp.auto_install
                    && self.workspace.execution_allowed().is_ok()
                    && self
                        .workspace
                        .authorize_execution(ExecutionSource::ToolInstall)
                        .is_ok()
                {
                    self.install_tool(&id, source);
                    return;
                }
                self.notify_message(
                    self.message(
                        "lsp_install_hint",
                        &[
                            ("server", server),
                            ("language", document.language_id.clone()),
                        ],
                    ),
                    false,
                );
            }
        }
        let observation = LspAutostartObservation {
            buffer,
            path: document.path.clone(),
            language: document.language_id.clone(),
            root_revision: self.workspace.root_revision(),
            trust: self.workspace.trust(),
            enabled: self.configuration.settings().lsp.enabled,
            auto_start: self.configuration.settings().lsp.auto_start,
            nix_workspace_fallback: self.configuration.settings().lsp.nix_workspace_fallback,
            launchable_server,
        };
        if let Some(server) = self
            .language
            .observe(observation, self.workspace.safe_mode())
        {
            self.start_lsp(Some(server));
        }
    }
}

impl App {
    pub(in crate::app) fn sync_lsp_document(&mut self) {
        let mut changed = self.language.retry_documents(&self.state);
        let Some(buffer) = self.state.active_buffer_id() else {
            self.language.reset_autostart();
            return;
        };
        // The text is copied only for a server that needs it, not on every pass.
        let Some((_, mut document)) = self.active_lsp_document_without_text() else {
            let stale = self
                .language
                .sessions()
                .iter()
                .filter_map(|(key, state)| {
                    state
                        .documents
                        .get(&buffer)
                        .map(|document| (key.clone(), document.uri()))
                })
                .collect::<Vec<_>>();
            self.detach_lsp_documents(buffer, stale);
            // Focusing a panel or terminal leaves the files beside it showing their hints.
            let mut hints = std::mem::take(&mut self.ui.lsp_inlay_hints);
            hints.retain(|hint| hint.buffer != buffer);
            self.clear_lsp_ui_projection();
            self.ui.lsp_inlay_hints = hints;
            self.language.clear_active();

            self.language.reset_autostart();
            return;
        };
        let revision = self.state.buffers[buffer].text.source_version();
        let mut sessions = Vec::new();
        let mut detach = Vec::new();
        for (key, state) in self.language.sessions() {
            // A lexical `starts_with` check would let a session for an outer
            // project absorb a nested, independently resolved project. Only
            // the asynchronous resolver may attach a new document to a server.
            let owns = state.documents.contains_key(&buffer);
            let supports = state.definition.supports_language(&document.language_id);
            if owns && supports {
                if let Some(previous) = state.documents.get(&buffer) {
                    if previous.path != document.path
                        || previous.language_id != document.language_id
                    {
                        detach.push((key.clone(), previous.uri()));
                    } else {
                        sessions.push(key.clone());
                    }
                }
            } else if let Some(previous) = state.documents.get(&buffer) {
                detach.push((key.clone(), previous.uri()));
            }
        }
        sessions.sort();
        if sessions.is_empty() {
            self.language.clear_active();

            self.clear_lsp_ui_projection();
        }
        if !sessions.is_empty()
            && !sessions
                .iter()
                .any(|key| self.language.active() == Some(key))
        {
            // A full language server rather than a linter such as ESLint names the file's server.
            let primary = sessions
                .iter()
                .find(|key| {
                    self.language
                        .sessions()
                        .get(*key)
                        .and_then(|session| session.capabilities.as_ref())
                        .is_some_and(|capabilities| capabilities.definition)
                })
                .unwrap_or(&sessions[0])
                .clone();
            self.select_lsp_session(&primary);
        }
        self.detach_lsp_documents(buffer, detach);
        let completion_open = self.ui.language_completion.is_some()
            && self.language.projection().is_some_and(|owner| {
                owner.buffer == buffer
                    && Some(owner.view) == self.state.active_view()
                    && self.language.active() == Some(&owner.server)
                    && owner.uri == document.uri()
            });
        self.refresh_lsp_projection_owner();
        for server in &sessions {
            if self.language.synced(server, buffer, &revision) {
                continue;
            }
            if document.text.is_empty() {
                document.text = self.state.buffers[buffer].text.to_string();
            }
            if self
                .language
                .sync_owned(server, document.clone(), (buffer, revision.clone()))
                .is_ok()
            {
                changed = true;
            }
        }
        if changed {
            self.request_lsp_colors();
        }
        if changed
            && !(self.configuration.settings().lsp.inlay_hint_level
                == crate::config::InlayHintLevel::Max
                && self.ui.lsp_inlay_hints.is_empty()
                && self.language.inlay_pending())
        {
            self.schedule_lsp_inlay_refresh(false);
        }
        self.reconcile_lsp_autostart(buffer, &document, !sessions.is_empty());
        let typed = self.language.take_typed();
        let active_attached = self
            .language
            .active()
            .is_some_and(|active| sessions.contains(active));
        let trigger =
            typed.is_some_and(|typed| self.language.is_completion_trigger(&sessions, typed));
        if completion_open {
            if changed && trigger && active_attached {
                self.mark_lsp_completion_start(true);
                self.request_lsp(LspFeature::Completion);
            } else if changed && self.language.completion_incomplete() && active_attached {
                self.request_lsp(LspFeature::Completion);
            }
            self.narrow_lsp_completion();
        } else if changed
            && self.configuration.settings().lsp.auto_completion
            && !sessions.is_empty()
            && !self.ui.large_file_buffers.contains(&buffer)
            && self.lsp_completion_should_open(typed, trigger)
        {
            self.mark_lsp_completion_start(true);
            self.request_lsp(LspFeature::Completion);
        }
    }
}

impl App {
    /// Whether a typed character opens the menu: a server's trigger character, or a letter that
    /// starts or continues a word. Numbers don't, and a request already out will show its answer.
    fn lsp_completion_should_open(&self, typed: Option<char>, trigger: bool) -> bool {
        if !self.lsp_completion_in_insert() {
            return false;
        }
        if trigger {
            return true;
        }
        typed.is_some_and(completion_word_char)
            && !self.language.request_pending(self.language.request_id())
            && self
                .lsp_word_before_cursor()
                .and_then(|word| word.chars().next())
                .is_some_and(|first| !first.is_numeric())
    }
}

impl App {
    fn lsp_completion_in_insert(&self) -> bool {
        self.state
            .active_view()
            .and_then(|view| self.state.views.get(view))
            .is_some_and(|view| view.mode == EditorMode::Insert)
    }
}

impl App {
    /// The word the cursor is at the end of, with where it starts.
    fn lsp_word_before_cursor(&self) -> Option<String> {
        let start = self.lsp_word_start()?;
        let view = &self.state.views[self.state.active_view()?];
        let range = ByteRange::new(start, view.cursor).ok()?;
        self.state.buffers.get(view.buffer)?.text.slice(range).ok()
    }
}

impl App {
    fn lsp_word_start(&self) -> Option<ByteOffset> {
        let view = &self.state.views[self.state.active_view()?];
        let text = &self.state.buffers.get(view.buffer)?.text;
        let line_start = text
            .line_to_byte(text.byte_to_line(view.cursor).ok()?)
            .ok()?;
        let before = text
            .slice(ByteRange::new(line_start, view.cursor).ok()?)
            .ok()?;
        let word = before
            .char_indices()
            .rev()
            .take_while(|(_, character)| completion_word_char(*character))
            .last()
            .map_or(before.len(), |(index, _)| index);
        Some(ByteOffset(line_start.get() + word))
    }
}

impl App {
    /// Notes where this completion starts, which later typing is measured from.
    pub(in crate::app) fn mark_lsp_completion_start(&mut self, automatic: bool) {
        let start = self.lsp_word_start();
        self.language.begin_completion(start, automatic);
    }
}

impl App {
    pub(in crate::app) fn cancel_lsp_completion(&mut self) {
        self.language.cancel_completion();
        self.ui.language_completion = None;
    }
}

impl App {
    /// Shows the items matching what was typed since the request, best first. The menu closes
    /// when the cursor leaves the word or nothing matches.
    pub(in crate::app) fn narrow_lsp_completion(&mut self) {
        let Some(view) = self
            .state
            .active_view()
            .and_then(|view| self.state.views.get(view))
        else {
            return;
        };
        let Some(buffer) = self.state.buffers.get(view.buffer) else {
            return;
        };
        let cursor = view.cursor;
        let revision = buffer.text.revision();
        let automatic_left_insert =
            self.language.completion_automatic() && view.mode != EditorMode::Insert;
        let start = self.language.completion_start();
        let same_line = |offset: ByteOffset| {
            buffer.text.byte_to_line(offset).ok() == buffer.text.byte_to_line(cursor).ok()
        };
        if automatic_left_insert || start.is_some_and(|start| start > cursor || !same_line(start)) {
            self.cancel_lsp_completion();
            return;
        }
        if !self.language.completion_needs_narrowing(cursor, revision) {
            return;
        }
        let codec = PositionCodec::new(&buffer.text);
        let typed = |from: ByteOffset| {
            ByteRange::new(from, cursor)
                .ok()
                .and_then(|range| buffer.text.slice(range).ok())
                .unwrap_or_default()
        };
        let from_start = start.map(typed).unwrap_or_default();
        let encodings = self.language.completion_encodings();
        let matches = self.language.score_completions(|server, item| {
            // A server's range says where its item begins; other items start at the word.
            let edit_start = item.text_edit.as_ref().and_then(|edit| {
                let encoding = encodings.get(server).copied()?;
                codec
                    .lsp_to_byte(edit.range.start.line, edit.range.start.character, encoding)
                    .ok()
                    .filter(|offset| *offset <= cursor && same_line(*offset))
            });
            edit_start.map_or_else(|| from_start.clone(), typed)
        });
        self.language.narrow_completions(matches, cursor, revision);
        self.project_lsp_completion();
    }
}

impl App {
    /// Mirrors the current items into the menu the renderer draws.
    pub(in crate::app) fn project_lsp_completion(&mut self) {
        self.ui.inline_completion =
            self.language
                .inline_completion(&self.state)
                .and_then(|label| {
                    let id = self.state.active_view()?;
                    let view = &self.state.views[id];
                    Some((
                        id,
                        crate::ui::UiInlayHint {
                            buffer: view.buffer,
                            revision: self.state.buffers[view.buffer].text.revision(),
                            position: view.cursor,
                            label,
                        },
                    ))
                });
        let icons = &self
            .configuration
            .settings()
            .ui
            .command_line
            .completion_kind_icons;
        let items = self
            .language
            .completion_items()
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let kind = Self::lsp_completion_kind(item);
                CompletionItem {
                    icon: icons.get(kind).cloned(),
                    label: item.label.clone(),
                    detail: if kind == "Snippet" {
                        kind.to_owned()
                    } else {
                        self.lsp_server_label(
                            self.language.completion_server(index).unwrap_or_default(),
                        )
                    },
                    color: item.color(),
                }
            })
            .collect::<Vec<_>>();
        let typed = self.lsp_completion_typed_columns();
        let (selected, scroll) = self.language.completion_position();
        self.ui.language_completion = (!items.is_empty()).then_some(LanguageCompletionState {
            typed,
            items,
            selected,
            scroll,
        });
        if self.ui.language_completion.is_some() {
            self.ui.lsp_loading = None;
        }
        self.refresh_lsp_completion_documentation();
    }
}

impl App {
    /// How much of the word being completed is already on the line.
    fn lsp_completion_typed_columns(&self) -> u16 {
        let Some(view) = self
            .state
            .active_view()
            .and_then(|view| self.state.views.get(view))
        else {
            return 0;
        };
        let Some(start) = self.language.completion_start() else {
            return 0;
        };
        let text = &self.state.buffers[view.buffer].text;
        if start > view.cursor
            || text.byte_to_line(start).ok() != text.byte_to_line(view.cursor).ok()
        {
            return 0;
        }
        let Ok(line) = text.byte_to_line(start) else {
            return 0;
        };
        let (Ok(line_start), Ok(content)) = (text.line_to_byte(line), text.line(line)) else {
            return 0;
        };
        let from = start.get().saturating_sub(line_start.get());
        let to = view.cursor.get().saturating_sub(line_start.get());
        content.get(from..to).map_or(0, |typed| {
            u16::try_from(configured_text_width(
                typed,
                self.configuration.settings().editor.tab_width,
                self.configuration.settings().editor.ambiguous_width,
            ))
            .unwrap_or(u16::MAX)
        })
    }
}

impl App {
    /// `<leader>cr` and F2: the command line, ready with the name under the cursor.
    fn word_at_cursor(&self) -> Option<String> {
        self.state
            .active_view()
            .and_then(|view| self.state.views.get(view))
            .and_then(|view| {
                let source = self.state.buffers.get(view.buffer)?.text.to_string();
                let cursor = view.cursor.get();
                let start = source[..cursor]
                    .char_indices()
                    .rev()
                    .take_while(|(_, character)| completion_word_char(*character))
                    .last()
                    .map_or(cursor, |(index, _)| index);
                let end = source[cursor..]
                    .char_indices()
                    .find(|(_, character)| !completion_word_char(*character))
                    .map_or(source.len(), |(relative, _)| cursor + relative);
                Some(source[start..end].to_owned())
            })
    }
}

impl App {
    pub(in crate::app) fn open_rename_prompt(&mut self) {
        let word = self.word_at_cursor();
        self.open_prompt(
            PromptKind::Command,
            format!("rename {}", word.unwrap_or_default()),
        );
    }
}

impl App {
    /// Servers list nothing for an empty query, so `<leader>sS` asks for one.
    pub(in crate::app) fn open_workspace_symbols_prompt(&mut self) {
        let word = self.word_at_cursor().unwrap_or_default();
        self.open_prompt(PromptKind::Command, format!("lsp.workspace_symbols {word}"));
    }
}

/// Where a conflicted file's first marker is, so opening it lands there.
fn completion_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}
impl App {
    pub(in crate::app) fn active_lsp_document(
        &self,
    ) -> Option<(crate::editor::BufferId, LspDocument)> {
        let (buffer_id, mut document) = self.active_lsp_document_without_text()?;
        document.text = self.state.buffers[buffer_id].text.to_string();
        Some((buffer_id, document))
    }
}

impl App {
    /// The active document's path, language and version, with empty text.
    pub(in crate::app) fn active_lsp_document_without_text(
        &self,
    ) -> Option<(crate::editor::BufferId, LspDocument)> {
        let buffer_id = self.state.active_buffer_id()?;
        let buffer = self.state.buffers.get(buffer_id)?;
        let path = buffer.path.clone()?;
        let language_id = language_id_for_path(&path)?.to_owned();
        Some((
            buffer_id,
            LspDocument {
                path,
                language_id,
                version: buffer.text.revision().0,
                text: String::new(),
            },
        ))
    }
}

impl App {
    pub(in crate::app) fn start_lsp(&mut self, requested: Option<String>) {
        self.launch_lsp(requested, false);
    }
}

impl App {
    /// `asked` is a person starting this server, who is told when the project can't have it.
    pub(in crate::app) fn launch_lsp(&mut self, requested: Option<String>, asked: bool) {
        if let Some(id) = requested.as_ref().filter(|_| asked) {
            self.language.clear_stopped(id);
        }
        if !self.configuration.settings().lsp.enabled {
            self.show_error(self.message("lsp_disabled", &[]));
            return;
        }
        let Some((buffer, document)) = self.active_lsp_document() else {
            self.show_error(self.message("lsp_no_buffer", &[]));
            return;
        };
        let entry = if let Some(id) = requested.as_ref() {
            let entry =
                self.tooling.entries().iter().find(|entry| {
                    entry.definition.id == *id && entry.definition.kind == ToolKind::Lsp
                });
            if !entry.is_some_and(|entry| entry.definition.supports_language(&document.language_id))
            {
                self.show_error(format!(
                    "language server `{id}` does not support {}",
                    document.language_id
                ));
                return;
            }
            entry
        } else {
            self.tooling
                .entries()
                .iter()
                .find(|entry| {
                    entry.definition.kind == ToolKind::Lsp
                        && entry
                            .definition
                            .automatically_supports_language(&document.language_id)
                        && matches!(entry.status, ToolStatus::Installed(_))
                })
                .or_else(|| {
                    self.tooling.entries().iter().find(|entry| {
                        entry.definition.kind == ToolKind::Lsp
                            && entry
                                .definition
                                .automatically_supports_language(&document.language_id)
                    })
                })
        };
        let Some(entry) = entry else {
            self.show_error(self.message("lsp_no_server", &[]));
            return;
        };
        if matches!(entry.status, ToolStatus::Installing { .. }) {
            self.show_error("language-server runtime is being changed");
            return;
        }
        let base_definition = entry.definition.clone();
        let source = self.selected_lsp_runtime_source(&base_definition);
        let definition = if matches!(source, LspRuntimeSource::Managed | LspRuntimeSource::Nix) {
            self.selected_managed_definition(&base_definition)
        } else {
            base_definition.clone()
        };
        let launch_source = match source {
            LspRuntimeSource::Path => {
                let Some(executable) = discover_executable(&definition.executable) else {
                    self.show_error(format!(
                        "{} is not available on PATH; press p to choose another runtime",
                        definition.name
                    ));
                    return;
                };
                if definition.id == "rust-analyzer"
                    && Self::rust_analyzer_is_rustup_proxy(&executable)
                {
                    let key = self.tool_version_key(
                        &definition,
                        LspRuntimeSource::Path,
                        Some(&executable),
                    );
                    const CHECKING: &str = "checking rust-analyzer in the active rustup toolchain…";
                    match self.tooling.version(&key) {
                        Some(Ok(_)) => {
                            if self
                                .notices
                                .current()
                                .is_some_and(|(_, notice)| notice == CHECKING)
                            {
                                self.clear_notice();
                            }
                        }
                        Some(Err(_)) => {
                            self.show_error(Self::unavailable_rust_analyzer_message());
                            return;
                        }
                        None => {
                            self.queue_tool_version_probe(
                                &definition,
                                LspRuntimeSource::Path,
                                executable,
                            );
                            self.notify_message(CHECKING.into(), false);
                            return;
                        }
                    }
                }
                LspLaunchSource::Direct {
                    executable,
                    managed: false,
                }
            }
            LspRuntimeSource::Managed => {
                let Some(executable) = self.selected_managed_executable(&base_definition) else {
                    self.show_error(format!(
                        "{} Dust-download version {} is not installed; press i to install it",
                        definition.name,
                        definition.version.as_deref().unwrap_or("unknown")
                    ));
                    return;
                };
                LspLaunchSource::Direct {
                    executable,
                    managed: true,
                }
            }
            LspRuntimeSource::Nix => {
                let Some(shell) = nix_workspace_shell_for_host(
                    &definition,
                    self.configuration.settings().lsp.nix_workspace_fallback,
                    is_nixos_host(),
                ) else {
                    self.show_error("Dust via Nix is unavailable on this host");
                    return;
                };
                if !nix_lsp_profile_installed(&definition) {
                    self.show_error("Dust via Nix is not installed; press i to install it");
                    return;
                }
                let Some(executable) = self.selected_managed_executable(&base_definition) else {
                    self.show_error(format!(
                        "{} Dust via Nix version {} is not installed; press i",
                        definition.name,
                        definition.version.as_deref().unwrap_or("unknown")
                    ));
                    return;
                };
                if let Err(error) = nix_lsp_launch_environment(&definition) {
                    self.show_error(error);
                    return;
                }
                LspLaunchSource::NixWorkspace {
                    nix: executable,
                    shell: shell.into(),
                }
            }
        };
        self.language.clear_error(&definition.id);
        let Ok(root_id) = self.workspace.require_path(&document.path) else {
            self.show_error(self.message("lsp_no_buffer", &[]));
            return;
        };
        let Some(root_path) = self
            .workspace
            .roots()
            .iter()
            .find(|root| root.id == root_id)
            .map(|root| root.identity.canonical_path.clone())
        else {
            return;
        };
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        if let Err(error) = self.workspace.authorize_execution(
            if matches!(launch_source, LspLaunchSource::NixWorkspace { .. }) {
                ExecutionSource::ProjectLocal
            } else {
                ExecutionSource::SystemTool
            },
        ) {
            self.show_error(error);
            return;
        }
        let context = crate::app::language::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.discover_launch(
            PendingLspProjectRoot {
                asked,
                definition,
                launch_source,
                root_id,
                workspace_root: root_path,
                buffer,
                document,
            },
            context,
            self.workspace
                .execution_lane(crate::app::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn process_lsp_project_root(&mut self) {
        let Some(result) = self
            .workspace
            .project_result(crate::app::workspace::DiscoveryKind::Language)
        else {
            return;
        };
        let context = crate::app::language::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.accept_project_root(
            result,
            context,
            self.workspace
                .execution_lane(crate::app::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn process_nix_lsp_workspace(&mut self) {
        let context = crate::app::language::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.poll_nix_launch(
            context,
            self.workspace
                .execution_lane(crate::app::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn queue_lsp_preflight(&mut self, pending: LspLaunchRequest) {
        let context = crate::app::language::LaunchContext {
            editor: &self.state,
            workspace: self.workspace.version(),
            configuration: self.configuration.binding_revision(),
        };
        if let Err(error) = self.language.queue_launch(
            pending,
            context,
            self.workspace
                .execution_lane(crate::app::workspace::DiscoveryKind::Language),
        ) {
            self.show_error(error);
        }
    }
}

impl App {
    pub(in crate::app) fn restart_lsp_session(&mut self, key: String) {
        self.language.close_stale_documents(&self.state);
        let Some(state) = self.language.sessions().get(&key) else {
            return;
        };
        if state.circuit_open || !state.desired_running {
            return;
        }
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        let Some((&buffer, document)) = state.documents.iter().find(|(id, document)| {
            self.state
                .buffers
                .get(**id)
                .is_some_and(|buffer| buffer.path.as_ref() == Some(&document.path))
        }) else {
            return;
        };
        let (definition, launch, root, root_path, document) = (
            state.definition.clone(),
            state.launch.clone(),
            state.root,
            state.root_path.clone(),
            document.clone(),
        );
        self.queue_lsp_preflight(LspLaunchRequest {
            session_key: key,
            definition,
            launch,
            root_id: root,
            project_root: root_path,
            buffer,
            document,
            restart: true,
        });
    }
}

impl App {
    pub(in crate::app) fn select_lsp_session(&mut self, key: &str) {
        if self.language.select(key) {
            self.ui.lsp_inlay_hints.clear();
            self.schedule_lsp_inlay_refresh(true);
        }
    }
}

impl App {
    pub(in crate::app) fn show_lsp_sessions(&mut self) {
        let active = self.language.active().map(String::as_str);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let items = self
            .language
            .sessions()
            .iter()
            .map(|(key, session)| {
                let root = &session.root_path;
                let folder = root.file_name().map_or_else(
                    || root.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                // The folder and state come first so a narrow picker keeps them; the path last.
                let state = if session.circuit_open {
                    "crashed"
                } else if session.restart_due.is_some() {
                    "restarting"
                } else if session.desired_running {
                    "running"
                } else {
                    "stopped"
                };
                let mut detail = format!("{folder} · {state}");
                if active == Some(key) {
                    detail.push_str(" · active");
                }
                match session.restart_count {
                    0 => {}
                    1 => detail.push_str(" · 1 restart"),
                    count => detail.push_str(&format!(" · {count} restarts")),
                }
                let source = match home
                    .as_deref()
                    .and_then(|home| root.strip_prefix(home).ok())
                {
                    Some(rest) => format!("~/{}", rest.display()),
                    None => root.display().to_string(),
                };
                (
                    PickerItem {
                        badge: String::new(),
                        kind: crate::workspace::PickerItemKind::Entry,
                        id: PickerItemId(format!("lsp-session:{key}")),
                        label: session.server_id.clone(),
                        detail,
                        source,
                        action: PickerAction::None,
                    },
                    PickerPayload::LspSession(key.clone()),
                )
            })
            .collect();
        self.install_picker(self.message("picker_lsp_sessions_title", &[]), items);
    }
}

impl App {
    pub(in crate::app) fn clear_lsp_server_ownership(&mut self, server: &str) {
        for prefix in ["lsp.", "lsp-pull:"] {
            if let Ok(owner) = DiagnosticOwner::new(format!("{prefix}{server}")) {
                self.diagnostics.clear_owner(&owner);
            }
        }
        self.sync_ui_diagnostics();
        self.language.forget_synced(server);
        self.language.clear_server_projection(server);
    }
}

impl App {
    pub(in crate::app) fn detach_lsp_documents(
        &mut self,
        buffer: crate::editor::BufferId,
        documents: Vec<(String, String)>,
    ) {
        let detached = !documents.is_empty();
        for (server, uri) in documents {
            let _ = self.language.close(&server, uri.clone());
            self.language.invalidate_document(&server, buffer);
            self.language.clear_document_projection(&server, &uri);
            for prefix in ["lsp.", "lsp-pull:"] {
                if let Ok(owner) = DiagnosticOwner::new(format!("{prefix}{server}")) {
                    self.diagnostics.clear_owner_uri(&owner, &uri);
                }
            }
            self.language.detach(&server, buffer);
        }
        if detached {
            self.sync_ui_diagnostics();
            if self.state.active_buffer_id() == Some(buffer) {
                self.clear_lsp_ui_projection();
            }
        }
    }
}

#[cfg(test)]
pub(in crate::app) mod tests;
