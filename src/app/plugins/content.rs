//! Provider interaction and source-bound virtual document publication.
use crate::app::plugins::Plugins;
use crate::app::plugins::documents::VirtualDocumentEditorBridge;
use crate::app::windows::EditorWindowCandidate;
use crate::app::windows::NativeDescriptions;
use crate::app::windows::Windows;
use crate::editor::BufferId;
use crate::editor::EditorState;
use crate::editor::EditorVersion;
use crate::plugins::PluginCatalogVersion;
use crate::plugins::PluginLoadId;
use crate::plugins::PluginPickerChunk;
use crate::plugins::PluginPickerProjection;
use crate::plugins::PluginProviderError;
use crate::plugins::PluginProviderStart;
use crate::plugins::PluginVirtualDocumentBroker;
use crate::plugins::PluginVirtualDocumentProposal;
use crate::plugins::PluginVirtualDocumentSnapshot;

pub(in crate::app) struct VirtualContext<'a> {
    pub editor: &'a EditorState,
    pub windows: &'a Windows,
    pub scratch: &'a str,
}
pub(in crate::app) struct VirtualPublication<'a> {
    broker: &'a mut PluginVirtualDocumentBroker,
    bridge: &'a mut VirtualDocumentEditorBridge,
    next_broker: Option<PluginVirtualDocumentBroker>,
    next_bridge: VirtualDocumentEditorBridge,
    before: EditorVersion,
    after: EditorVersion,
    catalog: PluginCatalogVersion,
}
impl VirtualPublication<'_> {
    pub(in crate::app) fn before(&self) -> &EditorVersion {
        &self.before
    }
    pub(in crate::app) fn catalog(&self) -> &PluginCatalogVersion {
        &self.catalog
    }
    pub(in crate::app) fn finish(self, accepted: EditorState) -> Result<EditorState, String> {
        if accepted.version() != self.after {
            return Err("virtual publication belongs to a different editor proposal".into());
        }
        if let Some(broker) = self.next_broker {
            *self.broker = broker;
        }
        *self.bridge = self.next_bridge;
        Ok(accepted)
    }
}
pub(in crate::app) enum VirtualOpen {
    Existing(BufferId),
    Execute {
        plugin: String,
        load: PluginLoadId,
        command: String,
        document: u64,
    },
}
pub(in crate::app) struct ProviderInvocation {
    pub request: u64,
    pub plugin: String,
    pub command: String,
}
#[derive(Debug)]
pub(in crate::app) struct ProviderUpdate {
    pub refresh: Option<u64>,
    pub command: Option<String>,
}
impl Plugins {
    pub(in crate::app) fn start_provider(
        &mut self,
        provider: &str,
        query: String,
        workspace_revision: u64,
        editor: &EditorState,
    ) -> Result<ProviderInvocation, String> {
        let Some((plugin, load, command)) = self.plugin_contributions.ordered().find_map(|index| {
            index
                .providers
                .iter()
                .find(|item| item.id == provider)
                .map(|declaration| {
                    (
                        index.owner.clone(),
                        PluginLoadId(index.plugin_load),
                        declaration.command.clone(),
                    )
                })
        }) else {
            return Err("plugin picker provider is not statically declared".into());
        };
        if !self.manager.is_active_load(&plugin, load) {
            return Err(crate::plugins::PluginError::NotActive.to_string());
        }
        let command = command
            .strip_prefix(&format!("plugin.{plugin}."))
            .ok_or("plugin picker command is malformed")?
            .to_owned();
        let request = self
            .providers
            .reserve_replacing(
                PluginProviderStart {
                    plugin: plugin.clone(),
                    load,
                    provider: provider.into(),
                    query,
                    context: "editor".into(),
                    workspace_revision: Some(workspace_revision),
                    buffer_revision: editor
                        .active_buffer_id()
                        .map(|buffer| editor.buffers[buffer].text.revision().0),
                },
                self.active_picker,
            )
            .map_err(|error| error.detail)?;
        self.provider_sources.insert(
            request,
            editor
                .active_buffer_id()
                .map(|buffer| (buffer, editor.buffers[buffer].text.source_version())),
        );
        Ok(ProviderInvocation {
            request,
            plugin,
            command,
        })
    }
    pub(in crate::app) fn provider_presentable(&self, request: u64) -> bool {
        self.providers.presentable(request)
    }
    pub(in crate::app) fn discard_unpresented_provider(&mut self, request: u64) {
        if !self.provider_visible(request) {
            self.providers.cancel(request);
            self.provider_sources.remove(&request);
        }
    }
    pub(in crate::app) fn picker_installed(&mut self, provider: Option<u64>) {
        if provider.is_some_and(|request| !self.provider_presentable(request)) {
            return;
        }
        if self.active_picker != provider
            && let Some(previous) = self.active_picker.take()
        {
            self.providers.cancel(previous);
            self.provider_sources.remove(&previous);
        }
        self.active_picker = provider;
    }
    pub(in crate::app) fn provider_visible(&self, request: u64) -> bool {
        self.active_picker == Some(request)
    }
    pub(in crate::app) fn expire_providers(&mut self) -> Option<u64> {
        self.providers
            .expire()
            .into_iter()
            .find(|request| self.active_picker == Some(*request))
    }
    pub(in crate::app) fn provider_projection(
        &self,
        request: u64,
    ) -> Option<PluginPickerProjection> {
        self.providers.projection(request)
    }
    pub(in crate::app) fn provider_chunk(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        revisions: (Option<u64>, Option<u64>),
        chunk: PluginPickerChunk,
    ) -> Result<ProviderUpdate, PluginProviderError> {
        let request = chunk.request;
        self.providers
            .apply_chunk(plugin, load, revisions.0, revisions.1, chunk)?;
        let command = self.providers.continuation(request).and_then(|command| {
            command
                .strip_prefix(&format!("plugin.{plugin}."))
                .map(str::to_owned)
        });
        Ok(ProviderUpdate {
            refresh: self.provider_visible(request).then_some(request),
            command,
        })
    }
    pub(in crate::app) fn provider_selection(
        &self,
        plugin: &str,
        load: PluginLoadId,
        request: u64,
        item: &str,
        command: &str,
        editor: &EditorState,
    ) -> Option<(String, Vec<String>)> {
        let source = self.provider_sources.get(&request)?;
        if match source {
            Some((buffer, text)) => {
                editor.active_buffer_id() != Some(*buffer)
                    || editor
                        .buffers
                        .get(*buffer)
                        .is_none_or(|buffer| buffer.text.source_version() != *text)
            }
            None => editor.active_buffer_id().is_some(),
        } {
            return None;
        }
        if !self.provider_visible(request)
            || !self.manager.is_active_load(plugin, load)
            || !self
                .providers
                .owns_selection(plugin, load, request, item, command)
        {
            return None;
        }
        let projection = self.providers.projection(request)?;
        let selected = projection
            .items
            .iter()
            .find(|candidate| candidate.id == item)?;
        let command = command
            .strip_prefix(&format!("plugin.{plugin}."))?
            .to_owned();
        Some((
            command,
            vec![
                request.to_string(),
                selected.id.clone(),
                serde_json::to_string(&selected.action_data).ok()?,
            ],
        ))
    }
    pub(in crate::app) fn open_virtual(
        &mut self,
        source: &str,
        key: String,
        editor: &EditorState,
    ) -> Result<VirtualOpen, String> {
        if key.len() > crate::plugins::VIRTUAL_DOCUMENT_KEY_BYTE_LIMIT
            || key.chars().any(char::is_control)
        {
            return Err("invalid virtual document key".into());
        }
        let (plugin, load, command) = self
            .plugin_contributions
            .ordered()
            .find_map(|index| {
                index
                    .virtual_documents
                    .iter()
                    .find(|declaration| declaration.id == source)
                    .map(|declaration| {
                        (
                            index.owner.clone(),
                            PluginLoadId(index.plugin_load),
                            declaration.command.clone(),
                        )
                    })
            })
            .ok_or("unknown virtual document source")?;
        if !self.manager.is_active_load(&plugin, load) {
            return Err(crate::plugins::PluginError::NotActive.to_string());
        }
        if let Some(entry) = self.virtual_editor.find(&plugin, load, source, &key) {
            if !entry.matches(editor.into()) {
                return Err("virtual document source changed".into());
            }
            return Ok(VirtualOpen::Existing(entry.buffer));
        }
        if self
            .virtual_documents
            .is_opening(&plugin, load, source, &key)
        {
            return Err("virtual document is already opening".into());
        }
        let command = command
            .strip_prefix(&format!("plugin.{plugin}."))
            .ok_or("invalid virtual document command")?
            .to_owned();
        let document = self
            .virtual_documents
            .start(&plugin, load, source, key)
            .map_err(|error| error.detail)?;
        Ok(VirtualOpen::Execute {
            plugin,
            load,
            command,
            document,
        })
    }
    pub(in crate::app) fn refresh_virtual(
        &mut self,
        buffer: BufferId,
        editor: &EditorState,
    ) -> Result<VirtualOpen, String> {
        let entry = self
            .virtual_editor
            .entry_for_buffer(buffer)
            .ok_or("active buffer is not a virtual document")?;
        if !entry.matches(editor.into()) {
            return Err("virtual document source changed".into());
        }
        let command = self
            .plugin_contributions
            .get(&entry.plugin)
            .filter(|index| index.plugin_load == entry.load.0)
            .and_then(|index| {
                index
                    .virtual_documents
                    .iter()
                    .find(|source| source.id == entry.source)
            })
            .ok_or("virtual document source unavailable")?
            .command
            .strip_prefix(&format!("plugin.{}.", entry.plugin))
            .ok_or("invalid virtual document command")?
            .to_owned();
        if !self.manager.is_active_load(&entry.plugin, entry.load) {
            return Err(crate::plugins::PluginError::NotActive.to_string());
        }
        self.virtual_documents
            .refresh(&entry.plugin, entry.load, entry.document)
            .map_err(|error| error.detail)?;
        Ok(VirtualOpen::Execute {
            plugin: entry.plugin.clone(),
            load: entry.load,
            command,
            document: entry.document,
        })
    }
    pub(in crate::app) fn cancel_virtual_request(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        document: u64,
    ) {
        self.virtual_documents
            .cancel_pending(plugin, load, document);
    }
    pub(in crate::app) fn close_virtual_buffer(
        &mut self,
        buffer: BufferId,
    ) -> Result<Option<VirtualOpen>, String> {
        let Some(entry) = self.virtual_editor.entry_for_buffer(buffer).cloned() else {
            return Ok(None);
        };
        self.virtual_editor
            .forget_document(&entry.plugin, entry.load, entry.document);
        let command = self
            .virtual_documents
            .command_for_document(&entry.plugin, entry.load, entry.document)
            .and_then(|command| command.strip_prefix(&format!("plugin.{}.", entry.plugin)))
            .map(str::to_owned);
        let Some(command) = command else {
            self.virtual_documents
                .cancel(&entry.plugin, entry.load, entry.document);
            return Ok(None);
        };
        self.virtual_documents
            .cancel_pending(&entry.plugin, entry.load, entry.document);
        if let Err(error) = self
            .virtual_documents
            .close(&entry.plugin, entry.load, entry.document)
        {
            self.virtual_documents
                .cancel(&entry.plugin, entry.load, entry.document);
            return Err(error.detail);
        }
        Ok(Some(VirtualOpen::Execute {
            plugin: entry.plugin,
            load: entry.load,
            command,
            document: entry.document,
        }))
    }

    pub(in crate::app) fn virtual_descriptor_sources(
        &self,
    ) -> Vec<(BufferId, String, String, String)> {
        self.virtual_editor.descriptor_sources()
    }
    fn virtual_publication<'a>(
        &'a mut self,
        context: VirtualContext<'_>,
        draft: crate::editor::EditorDraft,
        bridge: VirtualDocumentEditorBridge,
        broker: Option<PluginVirtualDocumentBroker>,
        descriptions: Option<NativeDescriptions<'_>>,
    ) -> Result<(VirtualPublication<'a>, EditorWindowCandidate), String> {
        let before = context.editor.version();
        let after = draft.version();
        let catalog = self.plugin_contributions.version();
        // Only the window owner creates the sealed graph candidate. The caller
        // never receives a mutable draft detached from its source publication.
        let transition = context.windows.prepare_editor_transition(
            draft,
            &before,
            catalog.clone(),
            context.scratch,
            descriptions,
        )?;
        Ok((
            VirtualPublication {
                broker: &mut self.virtual_documents,
                bridge: &mut self.virtual_editor,
                next_broker: broker,
                next_bridge: bridge,
                before,
                after,
                catalog,
            },
            transition,
        ))
    }
    pub(in crate::app) fn prepare_virtual<'a>(
        &'a mut self,
        plugin: &str,
        load: PluginLoadId,
        proposal: PluginVirtualDocumentProposal,
        navigation_current: bool,
        context: VirtualContext<'_>,
    ) -> Result<(VirtualPublication<'a>, EditorWindowCandidate), String> {
        self.virtual_documents
            .check_proposal(plugin, load, &proposal)
            .map_err(|error| error.detail)?;
        if !navigation_current
            && matches!(&proposal.kind,
            crate::plugins::PluginVirtualDocumentProposalKind::Full(full) if full.expected_revision.is_none())
        {
            self.virtual_documents
                .cancel(plugin, load, proposal.document);
            return Err("virtual document opening cancelled after navigation".into());
        }
        if matches!(
            proposal.kind,
            crate::plugins::PluginVirtualDocumentProposalKind::Error(_)
        ) {
            return Err(self
                .virtual_documents
                .apply(plugin, load, proposal)
                .expect_err("an error proposal cannot publish a snapshot")
                .detail);
        }
        context
            .windows
            .check_editor_source(&context.editor.into())?;
        let mut broker = self.virtual_documents.clone();
        let snapshot = broker
            .apply(plugin, load, proposal)
            .map_err(|error| error.detail)?;
        let mut draft = context.editor.draft();
        let mut bridge = self.virtual_editor.clone();
        if snapshot.closed {
            broker.cancel(plugin, load, snapshot.document);
            return self.virtual_publication(context, draft, bridge, Some(broker), None);
        }
        bridge
            .publish(&mut draft, &snapshot)
            .map_err(|error| format!("virtual document publication rejected: {error:?}"))?;
        self.virtual_publication(context, draft, bridge, Some(broker), None)
    }
    pub(in crate::app) fn prepare_health<'a>(
        &'a mut self,
        refresh: bool,
        commands: usize,
        context: VirtualContext<'_>,
    ) -> Result<(VirtualPublication<'a>, EditorWindowCandidate), String> {
        context
            .windows
            .check_editor_source(&context.editor.into())?;
        let existing = self.virtual_editor.find(
            "builtin.health",
            PluginLoadId(1),
            "builtin.health.virtual",
            "health",
        );
        let mut draft = context.editor.draft();
        let mut bridge = self.virtual_editor.clone();
        if let Some(entry) = existing.filter(|_| !refresh) {
            if !entry.matches(draft.topology()) {
                return Err("virtual document source changed".into());
            }
            draft.show_document(entry.buffer).map_err(str::to_owned)?;
        } else {
            let revision = match existing {
                Some(entry) => entry
                    .revision
                    .checked_add(1)
                    .ok_or("health document revision exhausted")?,
                None => 1,
            };
            let snapshot = PluginVirtualDocumentSnapshot {
                plugin: "builtin.health".into(),
                load: PluginLoadId(1),
                source: "builtin.health.virtual".into(),
                document: 1,
                key: "health".into(),
                title: "Dust health".into(),
                language: "markdown".into(),
                text: format!(
                    "# Dust health\n\n- editor: ready\n- plugins indexed: {}\n- API commands: {commands}\n- persistence schema: {}\n",
                    self.manager.statuses.len(),
                    crate::persistence::STATE_VERSION
                ),
                revision,
                closed: false,
            };
            bridge
                .publish(&mut draft, &snapshot)
                .map_err(|error| format!("health document rejected: {error:?}"))?;
        }
        self.virtual_publication(context, draft, bridge, None, None)
    }
    pub(in crate::app) fn remove_virtual_documents<'a>(
        &'a mut self,
        plugin: &str,
        load: PluginLoadId,
        context: VirtualContext<'_>,
        mut descriptions: NativeDescriptions<'_>,
    ) -> Result<Option<(VirtualPublication<'a>, EditorWindowCandidate)>, String> {
        // Remove requests even if the window graph cannot remove the
        // retained read-only presentation. It is not publication rollback.
        self.virtual_documents.remove_load(plugin, load);
        if !self.virtual_editor.has_load(plugin, load) {
            return Ok(None);
        }
        context
            .windows
            .check_editor_source(&context.editor.into())?;
        let mut draft = context.editor.draft();
        let mut bridge = self.virtual_editor.clone();
        bridge.remove_load(&mut draft, plugin, load)?;
        descriptions.signature.dashboard_visible |= draft.topology().active_tab().is_none();
        self.virtual_publication(context, draft, bridge, None, Some(descriptions))
            .map(Some)
    }
    pub(in crate::app) fn remove_provider_load(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
    ) -> bool {
        self.providers.remove_load(plugin, load);
        self.provider_sources
            .retain(|request, _| self.providers.presentable(*request));
        if self
            .active_picker
            .is_some_and(|request| !self.providers.presentable(request))
        {
            self.active_picker = None;
            true
        } else {
            false
        }
    }
}
#[cfg(test)]
impl Plugins {
    pub(in crate::app) fn fixture_provider_install(
        &mut self,
        index: &crate::plugins::PluginContributionIndex,
    ) -> Result<(), PluginProviderError> {
        self.providers.install(index)
    }
    pub(in crate::app) fn fixture_provider_start(
        &mut self,
        start: PluginProviderStart,
        editor: &EditorState,
    ) -> Result<u64, PluginProviderError> {
        let request = self.providers.start(start)?;
        self.provider_sources.insert(
            request,
            editor
                .active_buffer_id()
                .map(|buffer| (buffer, editor.buffers[buffer].text.source_version())),
        );
        Ok(request)
    }
    pub(in crate::app) fn fixture_virtual_request(
        &mut self,
        index: &crate::plugins::PluginContributionIndex,
    ) -> crate::plugins::PluginVirtualDocumentRequest {
        let declaration = &index.virtual_documents[0];
        let load = PluginLoadId(index.plugin_load);
        self.virtual_documents.install(index).unwrap();
        self.virtual_documents
            .start(&index.owner, load, &declaration.id, "help".into())
            .unwrap();
        self.virtual_documents
            .request_lane()
            .deliver(&index.owner, load, &declaration.command)
            .pop()
            .unwrap()
    }
    pub(in crate::app) fn fixture_virtual_publish(
        &mut self,
        draft: &mut crate::editor::EditorDraft,
        snapshot: &PluginVirtualDocumentSnapshot,
    ) -> Result<BufferId, crate::app::plugins::documents::VirtualDocumentEditorError> {
        self.virtual_editor.publish(draft, snapshot)
    }
}
