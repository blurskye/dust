//! Component instantiation and WIT host calls.

use crate::plugins::execution::PluginConfirmationAnswer;
use crate::plugins::execution::PluginConfirmationError;
use crate::plugins::execution::PluginConfirmationErrorCode;
use crate::plugins::execution::PluginConfirmationOutcome;
use crate::plugins::execution::PluginConfirmationRequest;
use crate::plugins::execution::PluginConfirmationResult;
use crate::plugins::execution::PluginPrivateCell;
use crate::plugins::execution::PluginPrivateCursor;
use crate::plugins::execution::PluginPrivateHitRegion;
use crate::plugins::execution::PluginPrivatePatch;
use crate::plugins::execution::PluginPrivateRect;
use crate::plugins::execution::PluginPrivateSnapshot;
use crate::plugins::execution::PluginPrivateStyle;
use crate::plugins::execution::PluginRequestedPlacement;
use crate::plugins::execution::PluginUiError;
use crate::plugins::execution::PluginUiErrorCode;
use crate::plugins::execution::PluginUiNode;
use crate::plugins::execution::PluginUiNodeKind;
use crate::plugins::execution::PluginUiRequest;
use crate::plugins::execution::PluginUiRequestKind;
use crate::plugins::execution::PluginUiStyle;
use crate::plugins::execution::PluginUiTree;
use crate::plugins::manifest::MAXIMUM_PRIVATE_BYTES;
use crate::plugins::manifest::MAXIMUM_PRIVATE_CELLS_PER_CONTENT;
use crate::plugins::manifest::MAXIMUM_PRIVATE_DIRTY_RECTANGLES;
use crate::plugins::manifest::MAXIMUM_PRIVATE_HIT_REGIONS;
use crate::plugins::manifest::MAXIMUM_PRIVATE_PATCH_CELLS;
use crate::plugins::manifest::MAXIMUM_PRIVATE_STYLES;
use crate::plugins::manifest::MAXIMUM_PROVIDER_BYTES;
use crate::plugins::manifest::MAXIMUM_VIRTUAL_DOCUMENT_AGGREGATE_BYTES;
use crate::plugins::manifest::MAXIMUM_VIRTUAL_DOCUMENT_BYTES;
use crate::plugins::manifest::MAXIMUM_VIRTUAL_PATCH_BYTES;
use crate::plugins::manifest::PluginCapability;
use crate::plugins::manifest::PluginStorageContribution;
use crate::plugins::providers;
use crate::plugins::providers::PROVIDER_REQUEST_QUEUE_LIMIT;
use crate::plugins::providers::PluginPickerChunk;
use crate::plugins::providers::PluginPickerItem;
use crate::plugins::providers::PluginPickerRequestEnvelope;
use crate::plugins::providers::PluginProviderError;
use crate::plugins::providers::PluginProviderErrorCode;
use crate::plugins::providers::PluginProviderTerminal;
use crate::plugins::storage;
use crate::plugins::storage::PluginStorageError;
use crate::plugins::storage::PluginStorageErrorCode;
use crate::plugins::storage::PluginStorageRequest;
use crate::plugins::storage::PluginStorageRequestKind;
use crate::plugins::storage::PluginStorageResult;
use crate::plugins::storage::STORAGE_REQUEST_LIMIT;
use crate::plugins::virtual_documents::PluginVirtualDocumentError;
use crate::plugins::virtual_documents::PluginVirtualDocumentErrorCode;
use crate::plugins::virtual_documents::PluginVirtualDocumentFull;
use crate::plugins::virtual_documents::PluginVirtualDocumentPatch;
use crate::plugins::virtual_documents::PluginVirtualDocumentPatchSet;
use crate::plugins::virtual_documents::PluginVirtualDocumentProposal;
use crate::plugins::virtual_documents::PluginVirtualDocumentProposalKind;
use crate::plugins::virtual_documents::PluginVirtualDocumentRequest;
use crate::plugins::virtual_documents::PluginVirtualDocumentRequestKind;
use crate::plugins::virtual_documents::VIRTUAL_DOCUMENT_LANGUAGE_BYTE_LIMIT;
use crate::plugins::virtual_documents::VIRTUAL_DOCUMENT_REQUEST_QUEUE_LIMIT;
use crate::plugins::virtual_documents::VIRTUAL_DOCUMENT_TITLE_BYTE_LIMIT;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

fn key_layer_bytes(layer: &component_api::dust::plugin::host::KeyLayer) -> Result<usize, String> {
    if layer.bindings.len() > 256 {
        return Err("key binding limit exceeded".into());
    }
    layer
        .bindings
        .iter()
        .try_fold(layer.name.len(), |bytes, binding| {
            Ok(bytes
                + binding.keys.len()
                + binding.command.len()
                + crate::plugins::requests::value_bytes(&binding.arguments)?)
        })
}

pub mod component_api {
    wasmtime::component::bindgen!({ path: "wit", world: "editor-plugin" });
}

pub struct ComponentLimits {
    limits: wasmtime::StoreLimits,
    maximum_memory_bytes: usize,
    memory_bytes: usize,
    last_growth: usize,
}

impl ComponentLimits {
    pub fn new(limits: wasmtime::StoreLimits, maximum_memory_bytes: usize) -> Self {
        Self {
            limits,
            maximum_memory_bytes,
            memory_bytes: 0,
            last_growth: 0,
        }
    }
}

impl wasmtime::ResourceLimiter for ComponentLimits {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let growth = desired.saturating_sub(current);
        if growth > self.maximum_memory_bytes.saturating_sub(self.memory_bytes)
            || !self.limits.memory_growing(current, desired, maximum)?
        {
            return Ok(false);
        }
        self.memory_bytes += growth;
        self.last_growth = growth;
        Ok(true)
    }

    fn memory_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.memory_bytes = self.memory_bytes.saturating_sub(self.last_growth);
        self.last_growth = 0;
        self.limits.memory_grow_failed(error)
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.limits.table_growing(current, desired, maximum)
    }

    fn instances(&self) -> usize {
        self.limits.instances()
    }
    fn tables(&self) -> usize {
        self.limits.tables()
    }
    fn memories(&self) -> usize {
        self.limits.memories()
    }
}

pub struct ComponentHostState {
    pub(crate) editor_requests: crate::plugins::requests::ComponentRequests,
    pub runtime_epoch: u64,
    pub limits: ComponentLimits,
    pub capabilities: BTreeSet<PluginCapability>,
    pub declared_content_types: BTreeSet<String>,
    pub snapshot: Option<component_api::dust::plugin::host::Snapshot>,
    pub proposals: Vec<(u64, u64, Vec<component_api::dust::plugin::host::Edit>)>,
    pub messages: Vec<String>,
    pub config: BTreeMap<String, String>,
    pub ui_revision: Option<u64>,
    pub confirmation_available: bool,
    pub confirmation_result_only: bool,
    pub ui_requests: Vec<PluginUiRequest>,
    pub ui_events: Vec<component_api::dust::plugin::host::UiEvent>,
    pub next_ui_token: u64,
    pub pending_ui_tokens: BTreeSet<u64>,
    pub confirmation_requests: Vec<PluginConfirmationRequest>,
    pub confirmation_results: Vec<component_api::dust::plugin::host::ConfirmationResult>,
    pub next_confirmation_token: u64,
    pub host_events: Vec<component_api::dust::plugin::host::HostEvent>,
    pub delivered_host_event_ids: BTreeSet<u64>,
    pub host_event_acknowledgements: Vec<u64>,
    pub execution_available: bool,
    pub pending_storage: BTreeMap<u64, usize>,
    pub storage_policy: Option<PluginStorageContribution>,
    pub storage_requests: Vec<PluginStorageRequest>,
    pub storage_results: Vec<component_api::dust::plugin::host::StorageResult>,
    pub next_storage_token: u64,
    pub provider_requests: Vec<component_api::dust::plugin::host::PickerProviderRequest>,
    pub delivered_provider_requests: BTreeSet<u64>,
    pub provider_chunks: Vec<PluginPickerChunk>,
    pub virtual_document_requests: Vec<component_api::dust::plugin::host::VirtualDocumentRequest>,
    pub delivered_virtual_document_requests: BTreeSet<u64>,
    pub virtual_document_proposals: Vec<PluginVirtualDocumentProposal>,
}
impl component_api::dust::plugin::host::Host for ComponentHostState {
    fn start_process(
        &mut self,
        request: component_api::dust::plugin::host::ProcessStart,
    ) -> Result<u64, String> {
        self.require_editor_capability(PluginCapability::Process)?;
        let bytes = request.program.len()
            + request.cwd.len()
            + request.handler.len()
            + request.arguments.iter().map(String::len).sum::<usize>()
            + request
                .environment
                .iter()
                .map(|(key, value)| key.len() + value.len())
                .sum::<usize>()
            + request.stdin.as_ref().map_or(0, Vec::len);
        self.queue_system(
            crate::plugins::requests::SystemRequest::Start(request),
            bytes,
        )
    }

    fn write_process_stdin(
        &mut self,
        input: component_api::dust::plugin::host::ProcessInput,
    ) -> Result<u64, String> {
        self.require_editor_capability(PluginCapability::Process)?;
        let bytes = input.bytes.len();
        self.queue_system(crate::plugins::requests::SystemRequest::Input(input), bytes)
    }

    fn kill_process(&mut self, process: u64) -> Result<u64, String> {
        self.require_editor_capability(PluginCapability::Process)?;
        self.queue_system(crate::plugins::requests::SystemRequest::Kill(process), 8)
    }

    fn request_file(
        &mut self,
        operation: component_api::dust::plugin::host::FileOperation,
        handler: String,
    ) -> Result<u64, String> {
        self.require_editor_capability(PluginCapability::Files)?;
        use component_api::dust::plugin::host::FileOperation as Op;
        let bytes = handler.len()
            + match &operation {
                Op::Read(path)
                | Op::CreateDirectory(path)
                | Op::ListDirectory(path)
                | Op::Stat(path)
                | Op::Delete(path) => path.len(),
                Op::Write(write) => write.path.len() + write.bytes.len(),
                Op::Rename(rename) => rename.source.len() + rename.target.len(),
            };
        self.queue_system(
            crate::plugins::requests::SystemRequest::File(operation, handler),
            bytes,
        )
    }

    fn query_syntax(
        &mut self,
        query: component_api::dust::plugin::host::SyntaxQuery,
        handler: String,
    ) -> Result<u64, String> {
        self.require_editor_capability(PluginCapability::ReadCurrentBuffer)?;
        let bytes = query.query.len() + handler.len();
        self.queue_system(
            crate::plugins::requests::SystemRequest::Query(query, handler),
            bytes,
        )
    }

    fn node_at_position(
        &mut self,
        buffer: u64,
        position: u64,
        handler: String,
    ) -> Result<u64, String> {
        self.require_editor_capability(PluginCapability::ReadCurrentBuffer)?;
        let bytes = handler.len() + 16;
        self.queue_system(
            crate::plugins::requests::SystemRequest::Node(buffer, position, handler),
            bytes,
        )
    }

    fn take_system_events(&mut self) -> Vec<component_api::dust::plugin::host::SystemEvent> {
        std::mem::take(&mut self.editor_requests.delivery.system)
    }

    fn ui_input(
        &mut self,
        request: component_api::dust::plugin::host::InputRequest,
        handler: String,
    ) -> Result<u64, String> {
        let token = self.editor_requests.token()?;
        self.queue_surface(crate::plugins::surface::SurfaceRequest::Input(
            token, request, handler,
        ))?;
        Ok(token)
    }

    fn ui_select(
        &mut self,
        request: component_api::dust::plugin::host::SelectRequest,
        handler: String,
    ) -> Result<u64, String> {
        let token = self.editor_requests.token()?;
        self.queue_surface(crate::plugins::surface::SurfaceRequest::Select(
            token, request, handler,
        ))?;
        Ok(token)
    }

    fn notify(
        &mut self,
        request: component_api::dust::plugin::host::NotifyRequest,
    ) -> Result<(), String> {
        self.queue_surface(crate::plugins::surface::SurfaceRequest::Notify(request))
    }

    fn set_presenter(
        &mut self,
        kind: component_api::dust::plugin::host::PresenterKind,
        handler: Option<String>,
    ) -> Result<(), String> {
        self.queue_surface(crate::plugins::surface::SurfaceRequest::Presenter(
            kind, handler,
        ))
    }

    fn answer_input(&mut self, request: u64, text: Option<String>) -> Result<(), String> {
        self.queue_surface(crate::plugins::surface::SurfaceRequest::AnswerInput(
            request, text,
        ))
    }

    fn answer_select(&mut self, request: u64, item: Option<String>) -> Result<(), String> {
        self.queue_surface(crate::plugins::surface::SurfaceRequest::AnswerSelect(
            request, item,
        ))
    }

    fn open_editable_document(
        &mut self,
        document: component_api::dust::plugin::host::EditableDocument,
        handler: String,
    ) -> Result<u64, String> {
        let token = self.editor_requests.token()?;
        self.queue_surface(crate::plugins::surface::SurfaceRequest::Document(
            token, document, handler,
        ))?;
        Ok(token)
    }

    fn register_formatter(
        &mut self,
        filetype: String,
        step: component_api::dust::plugin::host::Formatter,
    ) -> Result<(), String> {
        self.queue_surface(crate::plugins::surface::SurfaceRequest::Formatter(
            filetype, step, false,
        ))
    }

    fn register_before_save(
        &mut self,
        filetype: String,
        step: component_api::dust::plugin::host::Formatter,
    ) -> Result<(), String> {
        self.queue_surface(crate::plugins::surface::SurfaceRequest::Formatter(
            filetype, step, true,
        ))
    }

    fn set_chrome_segment(
        &mut self,
        segment: component_api::dust::plugin::host::ChromeSegment,
    ) -> Result<(), String> {
        self.queue_surface(crate::plugins::surface::SurfaceRequest::Segment(segment))
    }

    fn remove_chrome_segment(
        &mut self,
        location: component_api::dust::plugin::host::SegmentLocation,
        name: String,
    ) -> Result<(), String> {
        self.queue_surface(crate::plugins::surface::SurfaceRequest::RemoveSegment(
            location, name,
        ))
    }

    fn register_command(
        &mut self,
        definition: component_api::dust::plugin::host::CommandDefinition,
    ) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::Commands)?;
        if definition.arguments.len() > 32 || definition.access.len() > 5 {
            return Err("command declaration limit exceeded".into());
        }
        let bytes = definition.name.len()
            + definition.title.len()
            + definition.handler.len()
            + definition
                .arguments
                .iter()
                .map(|field| field.name.len())
                .sum::<usize>();
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::Register(definition),
            bytes,
        )
    }

    fn run_command(
        &mut self,
        call: component_api::dust::plugin::host::CommandCall,
    ) -> Result<u64, String> {
        self.require_editor_capability(PluginCapability::Commands)?;
        let bytes = crate::plugins::requests::value_bytes(&call.arguments)?;
        if call.pending.is_some()
            || call.previous.is_some()
            || call.after
            || call.command.len() > 256
        {
            return Err("run-command cannot supply wrapper state".into());
        }
        let token = self.editor_requests.token()?;
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::Run(token, call),
            bytes,
        )?;
        Ok(token)
    }

    fn call_next(&mut self, pending: u64) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::Commands)?;
        if self.editor_requests.pending != Some(pending)
            || self.editor_requests.after
            || self.editor_requests.call_next.is_some()
        {
            return Err("call-next requires the current waiting wrapper".into());
        }
        self.editor_requests.call_next = Some(pending);
        Ok(())
    }

    fn set_command_result(
        &mut self,
        value: component_api::dust::plugin::host::CommandValue,
    ) -> Result<(), String> {
        if self.confirmation_result_only
            || !self.execution_available
            || self.editor_requests.call_next.is_some()
        {
            return Err("command result is unavailable".into());
        }
        crate::plugins::requests::value_bytes(&value)?;
        self.editor_requests.value = Some(value);
        Ok(())
    }

    fn take_command_replies(&mut self) -> Vec<component_api::dust::plugin::host::CommandReply> {
        std::mem::take(&mut self.editor_requests.delivery.replies)
    }

    fn wrap_command(
        &mut self,
        wrapper: component_api::dust::plugin::host::CommandWrapper,
    ) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::Commands)?;
        let bytes = wrapper.command.len() + wrapper.handler.len();
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::Wrap(wrapper),
            bytes,
        )
    }

    fn enter_mode(
        &mut self,
        mode: component_api::dust::plugin::host::NamedMode,
    ) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::Input)?;
        let bytes = mode.label.len() + mode.name.len() + key_layer_bytes(&mode.layer)?;
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::EnterMode(mode),
            bytes,
        )
    }

    fn leave_mode(&mut self) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::Input)?;
        self.editor_requests
            .push(crate::plugins::requests::EditorRequest::LeaveMode, 0)
    }

    fn push_key_layer(
        &mut self,
        layer: component_api::dust::plugin::host::KeyLayer,
    ) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::Input)?;
        let bytes = key_layer_bytes(&layer)?;
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::PushLayer(layer),
            bytes,
        )
    }

    fn pop_key_layer(&mut self, name: String) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::Input)?;
        let bytes = name.len();
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::PopLayer(name),
            bytes,
        )
    }

    fn replace_marks(
        &mut self,
        marks: component_api::dust::plugin::host::MarkSet,
    ) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::Decorations)?;
        if marks.marks.len() > 4096 {
            return Err("mark limit exceeded".into());
        }
        let bytes = marks.namespace.len()
            + marks
                .marks
                .iter()
                .map(|mark| {
                    40 + mark.decoration.as_ref().map_or(0, |decoration| {
                        decoration.highlight.as_ref().map_or(0, String::len)
                            + decoration
                                .virtual_text
                                .as_ref()
                                .map_or(0, |text| text.text.len() + text.style.len())
                            + decoration
                                .sign
                                .as_ref()
                                .map_or(0, |sign| sign.text.len() + sign.style.len())
                    })
                })
                .sum::<usize>();
        self.editor_requests
            .push(crate::plugins::requests::EditorRequest::Marks(marks), bytes)
    }

    fn publish_diagnostics(
        &mut self,
        set: component_api::dust::plugin::host::DiagnosticSet,
    ) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::ShowMessage)?;
        if set.diagnostics.len() > 2000 {
            return Err("diagnostic limit exceeded".into());
        }
        let bytes = set.namespace.len()
            + set
                .diagnostics
                .iter()
                .map(|item| 24 + item.message.len() + item.source.len())
                .sum::<usize>();
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::Diagnostics(set),
            bytes,
        )
    }

    fn read_marks(
        &mut self,
        buffer: u64,
        namespace: String,
    ) -> Result<Vec<component_api::dust::plugin::host::Mark>, String> {
        self.require_editor_capability(PluginCapability::Decorations)?;
        Ok(self
            .editor_requests
            .delivery
            .marks
            .iter()
            .find(|set| set.buffer == buffer && set.namespace == namespace)
            .map(|set| set.marks.clone())
            .unwrap_or_default())
    }

    fn subscribe_events(&mut self, handler: String) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::EventSubscriptions)?;
        let bytes = handler.len();
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::Subscribe(handler),
            bytes,
        )
    }

    fn take_editor_events(&mut self) -> Option<component_api::dust::plugin::host::EventBatch> {
        self.editor_requests.delivery.events.take()
    }

    fn register_completion_source(
        &mut self,
        source: component_api::dust::plugin::host::CompletionSource,
    ) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::Completion)?;
        let bytes = source.name.len() + source.handler.len();
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::CompletionSource(source),
            bytes,
        )
    }

    fn publish_completions(
        &mut self,
        batch: component_api::dust::plugin::host::CompletionBatch,
    ) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::Completion)?;
        if batch.items.len() > 4096 {
            return Err("completion item limit exceeded".into());
        }
        let bytes = batch.source.len()
            + batch
                .items
                .iter()
                .map(|item| {
                    item.label.len() + item.text.len() + item.detail.as_ref().map_or(0, String::len)
                })
                .sum::<usize>();
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::Completions(batch),
            bytes,
        )
    }

    fn filter_lsp(
        &mut self,
        filter: component_api::dust::plugin::host::LspFilter,
    ) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::Commands)?;
        let bytes = filter.method.len() + filter.handler.len();
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::LspFilter(filter),
            bytes,
        )
    }

    fn start_timer(
        &mut self,
        request: component_api::dust::plugin::host::TimerRequest,
    ) -> Result<u64, String> {
        self.require_editor_capability(PluginCapability::Timers)?;
        if request.milliseconds == 0 || request.milliseconds > 86_400_000 {
            return Err("timer must be between 1 ms and one day".into());
        }
        let token = self.editor_requests.token()?;
        let bytes = request.handler.len();
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::Timer(token, request),
            bytes,
        )?;
        Ok(token)
    }

    fn cancel_timer(&mut self, request: u64) -> Result<(), String> {
        self.require_editor_capability(PluginCapability::Timers)?;
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::CancelTimer(request),
            8,
        )
    }

    fn request_http(
        &mut self,
        request: component_api::dust::plugin::host::HttpRequest,
    ) -> Result<u64, String> {
        self.require_editor_capability(PluginCapability::Http)?;
        if request.headers.len() > 64 {
            return Err("HTTP header limit exceeded".into());
        }
        let bytes = request.url.len()
            + request.method.len()
            + request.body.len()
            + request.handler.len()
            + request
                .headers
                .iter()
                .map(|(key, value)| key.len() + value.len())
                .sum::<usize>();
        let token = self.editor_requests.token()?;
        self.editor_requests.push(
            crate::plugins::requests::EditorRequest::Http(token, request),
            bytes,
        )?;
        Ok(token)
    }

    fn take_http_responses(&mut self) -> Vec<component_api::dust::plugin::host::HttpResponse> {
        std::mem::take(&mut self.editor_requests.delivery.http)
    }

    fn current_view(&mut self) -> Option<component_api::dust::plugin::host::ViewState> {
        if self
            .capabilities
            .contains(&PluginCapability::ReadCurrentBuffer)
        {
            self.editor_requests.delivery.view.clone()
        } else {
            None
        }
    }

    fn current_buffer(&mut self) -> Result<component_api::dust::plugin::host::Snapshot, String> {
        if self
            .capabilities
            .contains(&PluginCapability::ReadCurrentBuffer)
        {
            self.snapshot
                .clone()
                .ok_or_else(|| "no current buffer snapshot".into())
        } else {
            Err("capability denied".into())
        }
    }
    fn propose_edits(
        &mut self,
        buffer: u64,
        revision: u64,
        edits: Vec<component_api::dust::plugin::host::Edit>,
    ) -> Result<u64, String> {
        if self.confirmation_result_only {
            return Err("confirmation result cannot propose edits".into());
        }
        if !self.capabilities.contains(&PluginCapability::ProposeEdits) {
            return Err("capability denied".into());
        }
        if edits.len() > 4096 {
            return Err("edit limit exceeded".into());
        }
        if edits
            .iter()
            .any(|edit| edit.start > edit.end || edit.replacement.len() > 1024 * 1024)
        {
            return Err("invalid edit proposal".into());
        }
        // Bound the complete guest-produced batch, not just each host call.
        if self.proposals.len() >= 1024
            || self
                .proposals
                .iter()
                .map(|(_, _, edits)| edits.len())
                .sum::<usize>()
                + edits.len()
                > 4096
            || self
                .proposals
                .iter()
                .flat_map(|(_, _, edits)| edits)
                .chain(&edits)
                .try_fold(0usize, |bytes, edit| {
                    bytes.checked_add(edit.replacement.len())
                })
                .is_none_or(|bytes| bytes > 1024 * 1024)
        {
            return Err("edit proposal batch limit exceeded".into());
        }
        self.proposals.push((buffer, revision, edits));
        Ok(self.proposals.len() as u64)
    }
    fn show_message(&mut self, message: String) -> Result<(), String> {
        if !self.capabilities.contains(&PluginCapability::ShowMessage) {
            return Err("capability denied".into());
        }
        self.messages.push(
            message
                .chars()
                .filter(|character| !character.is_control())
                .take(4096)
                .collect(),
        );
        if self.messages.len() > 64 {
            self.messages.remove(0);
        }
        Ok(())
    }
    fn config_get(&mut self, key: String) -> Option<String> {
        self.capabilities
            .contains(&PluginCapability::ReadConfig)
            .then(|| self.config.get(&key).cloned())
            .flatten()
    }

    fn current_ui_revision(&mut self) -> Option<u64> {
        self.ui_revision
    }

    fn queue_ui_request(
        &mut self,
        request: component_api::dust::plugin::host::UiRequest,
    ) -> Result<u64, component_api::dust::plugin::host::UiError> {
        if !self.execution_available {
            return Err(component_ui_error(PluginUiError::new(
                PluginUiErrorCode::InternalRejected,
                "UI requests are unavailable during activation",
            )));
        }
        if self.confirmation_result_only
            && !matches!(
                request,
                component_api::dust::plugin::host::UiRequest::ReplaceUi(_)
                    | component_api::dust::plugin::host::UiRequest::ReplacePrivate(_)
                    | component_api::dust::plugin::host::UiRequest::PatchPrivate(_)
            )
        {
            return Err(component_ui_error(PluginUiError::new(
                PluginUiErrorCode::InternalRejected,
                "confirmation result may update existing plugin UI only",
            )));
        }
        if self.pending_ui_tokens.len() >= 64 {
            return Err(component_ui_error(PluginUiError::new(
                PluginUiErrorCode::QueueFull,
                "UI request limit exceeded",
            )));
        }
        let token = self.next_ui_token;
        self.next_ui_token = self.next_ui_token.checked_add(1).ok_or_else(|| {
            component_ui_error(PluginUiError::new(
                PluginUiErrorCode::InternalRejected,
                "UI request token exhausted",
            ))
        })?;
        let kind = convert_component_ui_request(request, &self.declared_content_types)
            .map_err(component_ui_error)?;
        if self
            .ui_requests
            .iter()
            .map(|request| ui_request_bytes(&request.kind))
            .chain(std::iter::once(ui_request_bytes(&kind)))
            .try_fold(0_usize, usize::checked_add)
            .is_none_or(|bytes| bytes > 1024 * 1024)
        {
            return Err(component_ui_error(PluginUiError::new(
                PluginUiErrorCode::QueueFull,
                "UI request byte limit exceeded",
            )));
        }
        self.pending_ui_tokens.insert(token);
        self.ui_requests.push(PluginUiRequest { token, kind });
        Ok(token)
    }

    fn take_ui_events(&mut self) -> Vec<component_api::dust::plugin::host::UiEvent> {
        let events = std::mem::take(&mut self.ui_events);
        for event in &events {
            self.pending_ui_tokens.remove(&event.token);
        }
        events
    }

    fn queue_confirmation(
        &mut self,
        request: component_api::dust::plugin::host::ConfirmationRequest,
    ) -> Result<u64, component_api::dust::plugin::host::ConfirmationError> {
        if !self.confirmation_available || !self.confirmation_requests.is_empty() {
            return Err(component_confirmation_error(PluginConfirmationError::new(
                PluginConfirmationErrorCode::QueueFull,
                "one confirmation may be requested per execution",
            )));
        }
        if request.title.is_empty()
            || request.title.len() > 128
            || request.message.is_empty()
            || request.message.len() > 4_096
            || request
                .title
                .chars()
                .chain(request.message.chars())
                .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
            || matches!(
                request.default_answer,
                component_api::dust::plugin::host::ConfirmationAnswer::Cancelled
            )
        {
            return Err(component_confirmation_error(PluginConfirmationError::new(
                PluginConfirmationErrorCode::InvalidRequest,
                "confirmation title, message, or default is invalid",
            )));
        }
        let token = self.next_confirmation_token;
        self.next_confirmation_token =
            self.next_confirmation_token.checked_add(1).ok_or_else(|| {
                component_confirmation_error(PluginConfirmationError::new(
                    PluginConfirmationErrorCode::Unavailable,
                    "confirmation token exhausted",
                ))
            })?;
        self.confirmation_requests.push(PluginConfirmationRequest {
            token,
            title: request.title,
            message: request.message,
            default_answer: internal_confirmation_answer(request.default_answer),
        });
        Ok(token)
    }

    fn take_confirmation_results(
        &mut self,
    ) -> Vec<component_api::dust::plugin::host::ConfirmationResult> {
        std::mem::take(&mut self.confirmation_results)
    }

    fn take_host_events(&mut self) -> Vec<component_api::dust::plugin::host::HostEvent> {
        if self
            .capabilities
            .contains(&PluginCapability::EventSubscriptions)
        {
            std::mem::take(&mut self.host_events)
        } else {
            Vec::new()
        }
    }

    fn acknowledge_host_events(&mut self, ids: Vec<u64>) -> Result<(), String> {
        if !self
            .capabilities
            .contains(&PluginCapability::EventSubscriptions)
        {
            return Err("capability denied".into());
        }
        if ids.len() > 64 {
            return Err("host event acknowledgement limit exceeded".into());
        }
        let mut unique = BTreeSet::new();
        for id in ids {
            if id == 0 || !self.delivered_host_event_ids.contains(&id) || !unique.insert(id) {
                return Err("invalid host event acknowledgement".into());
            }
        }
        unique.extend(self.host_event_acknowledgements.iter().copied());
        if unique.len() > 64 {
            return Err("host event acknowledgement limit exceeded".into());
        }
        self.host_event_acknowledgements = unique.into_iter().collect();
        Ok(())
    }

    fn queue_storage_request(
        &mut self,
        request: component_api::dust::plugin::host::StorageRequest,
    ) -> Result<u64, component_api::dust::plugin::host::StorageError> {
        if !self.execution_available || self.confirmation_result_only {
            return Err(component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "storage is unavailable during activation or confirmation results",
            )));
        }
        if !self.capabilities.contains(&PluginCapability::PluginStorage) {
            return Err(component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "capability denied",
            )));
        }
        if self.storage_requests.len() >= STORAGE_REQUEST_LIMIT {
            return Err(component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::QueueFull,
                "storage request limit exceeded",
            )));
        }
        let token = self.next_storage_token;
        self.next_storage_token = self.next_storage_token.checked_add(1).ok_or_else(|| {
            component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "storage token exhausted",
            ))
        })?;
        let kind = convert_component_storage_request(request);
        let candidate = PluginStorageRequest { token, kind };
        let policy = self.storage_policy.as_ref().ok_or_else(|| {
            component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::Unavailable,
                "storage policy is unavailable",
            ))
        })?;
        storage::validate_request(&candidate, policy).map_err(component_storage_error)?;
        let bytes = match &candidate.kind {
            PluginStorageRequestKind::Get { .. } => storage::STORAGE_VALUE_BYTE_LIMIT + 4096,
            PluginStorageRequestKind::List { .. } => storage::STORAGE_LIST_BYTE_LIMIT + 4096,
            _ => 4096,
        };
        if self.pending_storage.len() >= STORAGE_REQUEST_LIMIT
            || self
                .pending_storage
                .values()
                .sum::<usize>()
                .saturating_add(bytes)
                > storage::STORAGE_RESULT_QUEUE_BYTE_LIMIT
        {
            return Err(component_storage_error(PluginStorageError::new(
                PluginStorageErrorCode::QueueFull,
                "storage result inbox is full",
            )));
        }
        self.pending_storage.insert(token, bytes);
        self.storage_requests.push(candidate);
        Ok(token)
    }

    fn take_storage_results(&mut self) -> Vec<component_api::dust::plugin::host::StorageResult> {
        if self.capabilities.contains(&PluginCapability::PluginStorage) {
            let results = std::mem::take(&mut self.storage_results);
            for result in &results {
                self.pending_storage.remove(&result.token);
            }
            results
        } else {
            Vec::new()
        }
    }

    fn take_picker_provider_requests(
        &mut self,
    ) -> Vec<component_api::dust::plugin::host::PickerProviderRequest> {
        if self
            .capabilities
            .contains(&PluginCapability::PickerProviders)
        {
            std::mem::take(&mut self.provider_requests)
        } else {
            Vec::new()
        }
    }

    fn queue_picker_provider_chunk(
        &mut self,
        chunk: component_api::dust::plugin::host::PickerProviderChunk,
    ) -> Result<(), component_api::dust::plugin::host::ProviderError> {
        if self.confirmation_result_only {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::Unavailable,
                "confirmation result cannot publish provider chunks",
            )));
        }
        if !self
            .capabilities
            .contains(&PluginCapability::PickerProviders)
        {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::Unavailable,
                "capability denied",
            )));
        }
        if self.provider_chunks.len() >= PROVIDER_REQUEST_QUEUE_LIMIT {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::QueueFull,
                "provider chunk queue is full",
            )));
        }
        if !self.delivered_provider_requests.contains(&chunk.request) {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::UnknownRequest,
                "provider request was not delivered in this execution",
            )));
        }
        let terminal_is_error = matches!(
            chunk.terminal.kind,
            component_api::dust::plugin::host::ProviderTerminalKind::Error
        );
        if terminal_is_error != chunk.terminal.error.is_some() {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::Malformed,
                "provider terminal/error payload mismatch",
            )));
        }
        let chunk = convert_component_provider_chunk(chunk);
        providers::validate_chunk(&chunk).map_err(component_provider_error)?;
        if self
            .provider_chunks
            .iter()
            .chain(std::iter::once(&chunk))
            .map(provider_chunk_bytes)
            .try_fold(0usize, usize::checked_add)
            .is_none_or(|bytes| bytes > MAXIMUM_PROVIDER_BYTES as usize)
        {
            return Err(component_provider_error(PluginProviderError::new(
                PluginProviderErrorCode::ByteLimit,
                "provider chunk queue byte limit exceeded",
            )));
        }
        self.provider_chunks.push(chunk);
        Ok(())
    }

    fn take_virtual_document_requests(
        &mut self,
    ) -> Vec<component_api::dust::plugin::host::VirtualDocumentRequest> {
        if self
            .capabilities
            .contains(&PluginCapability::VirtualDocuments)
        {
            std::mem::take(&mut self.virtual_document_requests)
        } else {
            Vec::new()
        }
    }

    fn queue_virtual_document_proposal(
        &mut self,
        proposal: component_api::dust::plugin::host::VirtualDocumentProposal,
    ) -> Result<(), component_api::dust::plugin::host::VirtualDocumentError> {
        if self.confirmation_result_only {
            return Err(component_virtual_document_error(
                PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::ReadOnly,
                    "confirmation result cannot publish virtual documents",
                ),
            ));
        }
        if !self
            .capabilities
            .contains(&PluginCapability::VirtualDocuments)
        {
            return Err(component_virtual_document_error(
                PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::ReadOnly,
                    "capability denied",
                ),
            ));
        }
        if self.virtual_document_proposals.len() >= VIRTUAL_DOCUMENT_REQUEST_QUEUE_LIMIT {
            return Err(component_virtual_document_error(
                PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::QueueFull,
                    "proposal queue full",
                ),
            ));
        }
        if !self
            .delivered_virtual_document_requests
            .contains(&proposal.request)
        {
            return Err(component_virtual_document_error(
                PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::UnknownRequest,
                    "request was not delivered in this execution",
                ),
            ));
        }
        let proposal = convert_component_virtual_document_proposal(proposal)
            .map_err(component_virtual_document_error)?;
        if self
            .virtual_document_proposals
            .iter()
            .chain(std::iter::once(&proposal))
            .map(virtual_document_proposal_bytes)
            .try_fold(0usize, usize::checked_add)
            .is_none_or(|bytes| bytes > MAXIMUM_VIRTUAL_DOCUMENT_AGGREGATE_BYTES as usize)
        {
            return Err(component_virtual_document_error(
                PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::ByteLimit,
                    "virtual document proposal queue byte limit exceeded",
                ),
            ));
        }
        self.virtual_document_proposals.push(proposal);
        Ok(())
    }
}

fn provider_chunk_bytes(chunk: &PluginPickerChunk) -> usize {
    chunk
        .items
        .iter()
        .map(|item| {
            item.id.len()
                + item.primary.len()
                + item.secondary.as_ref().map_or(0, String::len)
                + item.action_data.len()
        })
        .sum::<usize>()
        + match &chunk.terminal {
            PluginProviderTerminal::Error(error) => error.detail.len(),
            _ => 0,
        }
}

fn virtual_document_proposal_bytes(proposal: &PluginVirtualDocumentProposal) -> usize {
    match &proposal.kind {
        PluginVirtualDocumentProposalKind::Full(full) => {
            full.title.len() + full.language.len() + full.text.len()
        }
        PluginVirtualDocumentProposalKind::Patches(patches) => patches
            .patches
            .iter()
            .map(|patch| patch.replacement.len())
            .sum(),
        PluginVirtualDocumentProposalKind::Error(error) => error.detail.len(),
        PluginVirtualDocumentProposalKind::CloseAcknowledged => 0,
    }
}

pub(super) fn internal_confirmation_answer(
    answer: component_api::dust::plugin::host::ConfirmationAnswer,
) -> PluginConfirmationAnswer {
    use component_api::dust::plugin::host::ConfirmationAnswer as A;
    match answer {
        A::Yes => PluginConfirmationAnswer::Yes,
        A::No => PluginConfirmationAnswer::No,
        A::Cancelled => PluginConfirmationAnswer::Cancelled,
    }
}

pub(super) fn component_confirmation_answer(
    answer: PluginConfirmationAnswer,
) -> component_api::dust::plugin::host::ConfirmationAnswer {
    use component_api::dust::plugin::host::ConfirmationAnswer as A;
    match answer {
        PluginConfirmationAnswer::Yes => A::Yes,
        PluginConfirmationAnswer::No => A::No,
        PluginConfirmationAnswer::Cancelled => A::Cancelled,
    }
}

pub(super) fn component_confirmation_error(
    error: PluginConfirmationError,
) -> component_api::dust::plugin::host::ConfirmationError {
    use component_api::dust::plugin::host::ConfirmationErrorCode as C;
    component_api::dust::plugin::host::ConfirmationError {
        code: match error.code {
            PluginConfirmationErrorCode::QueueFull => C::QueueFull,
            PluginConfirmationErrorCode::InvalidRequest => C::InvalidRequest,
            PluginConfirmationErrorCode::LoadUnavailable => C::LoadUnavailable,
            PluginConfirmationErrorCode::StaleSource => C::StaleSource,
            PluginConfirmationErrorCode::Unavailable => C::Unavailable,
        },
        detail: error.detail,
    }
}

pub(super) fn component_confirmation_result(
    result: PluginConfirmationResult,
) -> component_api::dust::plugin::host::ConfirmationResult {
    use component_api::dust::plugin::host::ConfirmationOutcome as O;
    component_api::dust::plugin::host::ConfirmationResult {
        request: result.request,
        outcome: match result.outcome {
            PluginConfirmationOutcome::Answered(answer) => {
                O::Answered(component_confirmation_answer(answer))
            }
            PluginConfirmationOutcome::Failed(error) => {
                O::Failed(component_confirmation_error(error))
            }
        },
    }
}

pub(super) fn component_virtual_document_request(
    request: PluginVirtualDocumentRequest,
) -> component_api::dust::plugin::host::VirtualDocumentRequest {
    use component_api::dust::plugin::host::VirtualDocumentRequestKind as K;
    component_api::dust::plugin::host::VirtualDocumentRequest {
        request: request.request,
        document: request.document,
        source: request.source,
        key: request.key,
        kind: match request.kind {
            PluginVirtualDocumentRequestKind::Open => K::Open,
            PluginVirtualDocumentRequestKind::Refresh => K::Refresh,
            PluginVirtualDocumentRequestKind::CloseNotice => K::CloseNotice,
        },
        current_revision: request.current_revision,
        deadline_milliseconds: request.deadline_milliseconds,
        cancellation_id: request.cancellation_id,
        maximum_document_bytes: request.maximum_document_bytes,
        maximum_document_lines: request.maximum_document_lines,
        maximum_patch_bytes: request.maximum_patch_bytes,
        maximum_patches: request.maximum_patches,
    }
}
pub(super) fn convert_component_virtual_document_proposal(
    p: component_api::dust::plugin::host::VirtualDocumentProposal,
) -> Result<PluginVirtualDocumentProposal, PluginVirtualDocumentError> {
    use component_api::dust::plugin::host::VirtualDocumentProposalKind as K;
    let kind = match p.kind {
        K::Full(f) => {
            if f.title.len() > VIRTUAL_DOCUMENT_TITLE_BYTE_LIMIT
                || f.language.len() > VIRTUAL_DOCUMENT_LANGUAGE_BYTE_LIMIT
                || f.text.len() > MAXIMUM_VIRTUAL_DOCUMENT_BYTES as usize
            {
                return Err(PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::ByteLimit,
                    "virtual document full proposal exceeds absolute limit",
                ));
            }
            PluginVirtualDocumentProposalKind::Full(PluginVirtualDocumentFull {
                expected_revision: f.expected_revision,
                revision: f.revision,
                title: f.title,
                language: f.language,
                text: f.text,
            })
        }
        K::Patches(s) => {
            if s.patches.len() > 256 {
                return Err(PluginVirtualDocumentError::new(
                    PluginVirtualDocumentErrorCode::PatchLimit,
                    "virtual document patch count exceeds absolute limit",
                ));
            }
            let mut bytes = 0usize;
            let mut patches = Vec::with_capacity(s.patches.len());
            for p in s.patches {
                bytes = bytes.checked_add(p.replacement.len()).ok_or_else(|| {
                    PluginVirtualDocumentError::new(
                        PluginVirtualDocumentErrorCode::PatchLimit,
                        "patch byte overflow",
                    )
                })?;
                if bytes > MAXIMUM_VIRTUAL_PATCH_BYTES as usize {
                    return Err(PluginVirtualDocumentError::new(
                        PluginVirtualDocumentErrorCode::PatchLimit,
                        "patch bytes exceed absolute limit",
                    ));
                }
                patches.push(PluginVirtualDocumentPatch {
                    start: p.start,
                    end: p.end,
                    replacement: p.replacement,
                });
            }
            PluginVirtualDocumentProposalKind::Patches(PluginVirtualDocumentPatchSet {
                expected_revision: s.expected_revision,
                revision: s.revision,
                patches,
            })
        }
        K::CloseAcknowledged => PluginVirtualDocumentProposalKind::CloseAcknowledged,
        K::Error(e) => PluginVirtualDocumentProposalKind::Error(internal_virtual_document_error(e)),
    };
    Ok(PluginVirtualDocumentProposal {
        request: p.request,
        document: p.document,
        cancellation_id: p.cancellation_id,
        kind,
    })
}
pub(super) fn internal_virtual_document_error(
    e: component_api::dust::plugin::host::VirtualDocumentError,
) -> PluginVirtualDocumentError {
    use component_api::dust::plugin::host::VirtualDocumentErrorCode as C;
    PluginVirtualDocumentError::new(
        match e.code {
            C::QueueFull => PluginVirtualDocumentErrorCode::QueueFull,
            C::UnknownSource => PluginVirtualDocumentErrorCode::UnknownSource,
            C::UnknownRequest => PluginVirtualDocumentErrorCode::UnknownRequest,
            C::UnknownDocument => PluginVirtualDocumentErrorCode::UnknownDocument,
            C::StaleLoad => PluginVirtualDocumentErrorCode::StaleLoad,
            C::StaleRevision => PluginVirtualDocumentErrorCode::StaleRevision,
            C::Cancelled => PluginVirtualDocumentErrorCode::Cancelled,
            C::Expired => PluginVirtualDocumentErrorCode::Expired,
            C::InvalidTitle => PluginVirtualDocumentErrorCode::InvalidTitle,
            C::InvalidLanguage => PluginVirtualDocumentErrorCode::InvalidLanguage,
            C::InvalidUtf8Range => PluginVirtualDocumentErrorCode::InvalidUtf8Range,
            C::InvalidControl => PluginVirtualDocumentErrorCode::InvalidControl,
            C::ByteLimit => PluginVirtualDocumentErrorCode::ByteLimit,
            C::LineLimit => PluginVirtualDocumentErrorCode::LineLimit,
            C::PatchLimit => PluginVirtualDocumentErrorCode::PatchLimit,
            C::ReadOnly => PluginVirtualDocumentErrorCode::ReadOnly,
            C::Malformed => PluginVirtualDocumentErrorCode::Malformed,
            C::Internal => PluginVirtualDocumentErrorCode::Internal,
        },
        e.detail,
    )
}
pub(super) fn component_virtual_document_error(
    e: PluginVirtualDocumentError,
) -> component_api::dust::plugin::host::VirtualDocumentError {
    use component_api::dust::plugin::host::VirtualDocumentErrorCode as C;
    component_api::dust::plugin::host::VirtualDocumentError {
        code: match e.code {
            PluginVirtualDocumentErrorCode::QueueFull => C::QueueFull,
            PluginVirtualDocumentErrorCode::UnknownSource => C::UnknownSource,
            PluginVirtualDocumentErrorCode::UnknownRequest => C::UnknownRequest,
            PluginVirtualDocumentErrorCode::UnknownDocument => C::UnknownDocument,
            PluginVirtualDocumentErrorCode::StaleLoad => C::StaleLoad,
            PluginVirtualDocumentErrorCode::StaleRevision => C::StaleRevision,
            PluginVirtualDocumentErrorCode::Cancelled => C::Cancelled,
            PluginVirtualDocumentErrorCode::Expired => C::Expired,
            PluginVirtualDocumentErrorCode::InvalidTitle => C::InvalidTitle,
            PluginVirtualDocumentErrorCode::InvalidLanguage => C::InvalidLanguage,
            PluginVirtualDocumentErrorCode::InvalidUtf8Range => C::InvalidUtf8Range,
            PluginVirtualDocumentErrorCode::InvalidControl => C::InvalidControl,
            PluginVirtualDocumentErrorCode::ByteLimit => C::ByteLimit,
            PluginVirtualDocumentErrorCode::LineLimit => C::LineLimit,
            PluginVirtualDocumentErrorCode::PatchLimit => C::PatchLimit,
            PluginVirtualDocumentErrorCode::ReadOnly => C::ReadOnly,
            PluginVirtualDocumentErrorCode::Malformed => C::Malformed,
            PluginVirtualDocumentErrorCode::Internal => C::Internal,
        },
        detail: e.detail,
    }
}

pub(super) fn component_provider_request(
    request: PluginPickerRequestEnvelope,
) -> component_api::dust::plugin::host::PickerProviderRequest {
    component_api::dust::plugin::host::PickerProviderRequest {
        id: request.id,
        provider: request.provider,
        query: request.query,
        context: request.context,
        workspace_revision: request.workspace_revision,
        buffer_revision: request.buffer_revision,
        deadline_milliseconds: request.deadline_milliseconds,
        cancellation_id: request.cancellation_id,
        maximum_items: request.maximum_items,
        maximum_bytes: request.maximum_bytes,
    }
}

pub(super) fn convert_component_provider_chunk(
    chunk: component_api::dust::plugin::host::PickerProviderChunk,
) -> PluginPickerChunk {
    use component_api::dust::plugin::host::ProviderTerminalKind;
    let terminal = match chunk.terminal.kind {
        ProviderTerminalKind::More => PluginProviderTerminal::More,
        ProviderTerminalKind::Complete => PluginProviderTerminal::Complete,
        ProviderTerminalKind::Truncated => PluginProviderTerminal::Truncated,
        ProviderTerminalKind::Error => PluginProviderTerminal::Error(
            chunk
                .terminal
                .error
                .map(internal_provider_error)
                .unwrap_or_else(|| {
                    PluginProviderError::new(
                        PluginProviderErrorCode::Malformed,
                        "provider error terminal omitted its error",
                    )
                }),
        ),
    };
    PluginPickerChunk {
        request: chunk.request,
        sequence: chunk.sequence,
        items: chunk
            .items
            .into_iter()
            .map(|item| PluginPickerItem {
                id: item.id,
                primary: item.primary,
                secondary: item.secondary,
                action_data: item.action_data,
            })
            .collect(),
        terminal,
    }
}

pub(super) fn internal_provider_error(
    error: component_api::dust::plugin::host::ProviderError,
) -> PluginProviderError {
    use component_api::dust::plugin::host::ProviderErrorCode;
    PluginProviderError::new(
        match error.code {
            ProviderErrorCode::QueueFull => PluginProviderErrorCode::QueueFull,
            ProviderErrorCode::UnknownRequest => PluginProviderErrorCode::UnknownRequest,
            ProviderErrorCode::UnknownProvider => PluginProviderErrorCode::UnknownProvider,
            ProviderErrorCode::StaleLoad => PluginProviderErrorCode::StaleLoad,
            ProviderErrorCode::StaleRevision => PluginProviderErrorCode::StaleRevision,
            ProviderErrorCode::Cancelled => PluginProviderErrorCode::Cancelled,
            ProviderErrorCode::Expired => PluginProviderErrorCode::Expired,
            ProviderErrorCode::InvalidSequence => PluginProviderErrorCode::InvalidSequence,
            ProviderErrorCode::DuplicateItem => PluginProviderErrorCode::DuplicateItem,
            ProviderErrorCode::ItemLimit => PluginProviderErrorCode::ItemLimit,
            ProviderErrorCode::ByteLimit => PluginProviderErrorCode::ByteLimit,
            ProviderErrorCode::Malformed => PluginProviderErrorCode::Malformed,
            ProviderErrorCode::Unavailable => PluginProviderErrorCode::Unavailable,
            ProviderErrorCode::Internal => PluginProviderErrorCode::Internal,
        },
        error.detail,
    )
}

pub(super) fn component_provider_error(
    error: PluginProviderError,
) -> component_api::dust::plugin::host::ProviderError {
    use component_api::dust::plugin::host::ProviderErrorCode;
    component_api::dust::plugin::host::ProviderError {
        code: match error.code {
            PluginProviderErrorCode::QueueFull => ProviderErrorCode::QueueFull,
            PluginProviderErrorCode::UnknownRequest => ProviderErrorCode::UnknownRequest,
            PluginProviderErrorCode::UnknownProvider => ProviderErrorCode::UnknownProvider,
            PluginProviderErrorCode::StaleLoad => ProviderErrorCode::StaleLoad,
            PluginProviderErrorCode::StaleRevision => ProviderErrorCode::StaleRevision,
            PluginProviderErrorCode::Cancelled => ProviderErrorCode::Cancelled,
            PluginProviderErrorCode::Expired => ProviderErrorCode::Expired,
            PluginProviderErrorCode::InvalidSequence => ProviderErrorCode::InvalidSequence,
            PluginProviderErrorCode::DuplicateItem => ProviderErrorCode::DuplicateItem,
            PluginProviderErrorCode::ItemLimit => ProviderErrorCode::ItemLimit,
            PluginProviderErrorCode::ByteLimit => ProviderErrorCode::ByteLimit,
            PluginProviderErrorCode::Malformed => ProviderErrorCode::Malformed,
            PluginProviderErrorCode::Unavailable => ProviderErrorCode::Unavailable,
            PluginProviderErrorCode::Internal => ProviderErrorCode::Internal,
        },
        detail: error.detail,
    }
}

pub(super) fn convert_component_storage_request(
    request: component_api::dust::plugin::host::StorageRequest,
) -> PluginStorageRequestKind {
    use component_api::dust::plugin::host::StorageRequest;
    match request {
        StorageRequest::Get(key) => PluginStorageRequestKind::Get { key },
        StorageRequest::CompareAndSet(request) => PluginStorageRequestKind::CompareAndSet {
            key: request.key,
            expected_revision: request.expected_revision,
            value: request.value,
        },
        StorageRequest::Delete(request) => PluginStorageRequestKind::Delete {
            key: request.key,
            expected_revision: request.expected_revision,
        },
        StorageRequest::PrefixList(request) => PluginStorageRequestKind::List {
            prefix: request.prefix,
            maximum: request.maximum,
        },
    }
}

pub(super) fn component_storage_error(
    error: PluginStorageError,
) -> component_api::dust::plugin::host::StorageError {
    use component_api::dust::plugin::host::StorageErrorCode;
    component_api::dust::plugin::host::StorageError {
        code: match error.code {
            PluginStorageErrorCode::QueueFull => StorageErrorCode::QueueFull,
            PluginStorageErrorCode::InvalidKey => StorageErrorCode::InvalidKey,
            PluginStorageErrorCode::InvalidValue => StorageErrorCode::InvalidValue,
            PluginStorageErrorCode::StaleRevision => StorageErrorCode::StaleRevision,
            PluginStorageErrorCode::QuotaExceeded => StorageErrorCode::QuotaExceeded,
            PluginStorageErrorCode::Unavailable => StorageErrorCode::Unavailable,
            PluginStorageErrorCode::Corrupt => StorageErrorCode::Corrupt,
            PluginStorageErrorCode::LoadUnavailable => StorageErrorCode::LoadUnavailable,
        },
        detail: error.detail,
    }
}

pub(super) fn component_storage_result(
    result: PluginStorageResult,
) -> component_api::dust::plugin::host::StorageResult {
    component_api::dust::plugin::host::StorageResult {
        token: result.token,
        value: result.value,
        revision: result.revision,
        entries: result
            .entries
            .into_iter()
            .map(|entry| component_api::dust::plugin::host::StorageEntry {
                key: entry.key,
                revision: entry.revision,
                value: entry.value,
            })
            .collect(),
        truncated: result.truncated,
        error: result.error.map(component_storage_error),
    }
}

pub(super) fn component_ui_error(
    error: PluginUiError,
) -> component_api::dust::plugin::host::UiError {
    use component_api::dust::plugin::host::UiErrorCode;
    let code = match error.code {
        PluginUiErrorCode::QueueFull => UiErrorCode::QueueFull,
        PluginUiErrorCode::StaleGraph => UiErrorCode::StaleGraph,
        PluginUiErrorCode::StaleTree => UiErrorCode::StaleTree,
        PluginUiErrorCode::StaleHandle => UiErrorCode::StaleHandle,
        PluginUiErrorCode::ForeignLoad => UiErrorCode::ForeignLoad,
        PluginUiErrorCode::UndeclaredContent => UiErrorCode::UndeclaredContent,
        PluginUiErrorCode::UndeclaredAction => UiErrorCode::UndeclaredAction,
        PluginUiErrorCode::DisallowedPlacement => UiErrorCode::DisallowedPlacement,
        PluginUiErrorCode::HiddenFocus => UiErrorCode::HiddenFocus,
        PluginUiErrorCode::CrossTab => UiErrorCode::CrossTab,
        PluginUiErrorCode::InstanceLimit => UiErrorCode::InstanceLimit,
        PluginUiErrorCode::GraphLimit => UiErrorCode::GraphLimit,
        PluginUiErrorCode::InvalidTree => UiErrorCode::InvalidTree,
        PluginUiErrorCode::InvalidPlacement => UiErrorCode::InvalidPlacement,
        PluginUiErrorCode::CloseOrder => UiErrorCode::CloseOrder,
        PluginUiErrorCode::LoadUnavailable => UiErrorCode::LoadUnavailable,
        PluginUiErrorCode::InternalRejected => UiErrorCode::InternalRejected,
        PluginUiErrorCode::WrongPresenter => UiErrorCode::WrongPresenter,
        PluginUiErrorCode::StaleSurface => UiErrorCode::StaleSurface,
        PluginUiErrorCode::StaleDimensions => UiErrorCode::StaleDimensions,
        PluginUiErrorCode::InvalidLocalBounds => UiErrorCode::InvalidLocalBounds,
        PluginUiErrorCode::CellOverlap => UiErrorCode::CellOverlap,
        PluginUiErrorCode::SplitWideGrapheme => UiErrorCode::SplitWideGrapheme,
        PluginUiErrorCode::InvalidGrapheme => UiErrorCode::InvalidGrapheme,
        PluginUiErrorCode::InvalidStyle => UiErrorCode::InvalidStyle,
        PluginUiErrorCode::InvalidCursor => UiErrorCode::InvalidCursor,
        PluginUiErrorCode::InvalidHitRegion => UiErrorCode::InvalidHitRegion,
        PluginUiErrorCode::PatchLimit => UiErrorCode::PatchLimit,
    };
    component_api::dust::plugin::host::UiError {
        code,
        detail: error.detail,
    }
}

fn ui_request_bytes(kind: &PluginUiRequestKind) -> usize {
    fn tree_bytes(tree: &PluginUiTree) -> usize {
        tree.nodes
            .iter()
            .map(|node| {
                std::mem::size_of::<PluginUiNode>()
                    + node.text.len()
                    + node.action.as_ref().map_or(0, String::len)
                    + node.children.len() * 8
            })
            .sum()
    }
    match kind {
        PluginUiRequestKind::OpenWindow {
            title,
            content_type,
            tree,
            ..
        } => title.len() + content_type.len() + tree.as_ref().map_or(0, tree_bytes),
        PluginUiRequestKind::ReplaceUi { tree, .. } => tree_bytes(tree),
        PluginUiRequestKind::ReplacePrivate { snapshot, .. } => {
            snapshot
                .cells
                .iter()
                .map(|cell| cell.grapheme.len() + std::mem::size_of::<PluginPrivateCell>())
                .sum::<usize>()
                + snapshot
                    .hit_regions
                    .iter()
                    .map(|hit| hit.action.len() + std::mem::size_of::<PluginPrivateHitRegion>())
                    .sum::<usize>()
                + snapshot.styles.len() * std::mem::size_of::<PluginPrivateStyle>()
        }
        PluginUiRequestKind::PatchPrivate { patch, .. } => {
            patch
                .cells
                .iter()
                .map(|cell| cell.grapheme.len() + std::mem::size_of::<PluginPrivateCell>())
                .sum::<usize>()
                + patch
                    .hit_regions
                    .iter()
                    .map(|hit| hit.action.len() + std::mem::size_of::<PluginPrivateHitRegion>())
                    .sum::<usize>()
                + patch.styles.len() * std::mem::size_of::<PluginPrivateStyle>()
                + patch.dirty.len() * std::mem::size_of::<PluginPrivateRect>()
        }
        _ => 0,
    }
}

pub(super) fn convert_component_tree(
    tree: component_api::dust::plugin::host::UiTree,
) -> Result<PluginUiTree, PluginUiError> {
    if tree.nodes.len() > 256
        || tree.nodes.iter().any(|node| {
            node.text.len() > 4096
                || node
                    .action
                    .as_ref()
                    .is_some_and(|action| action.len() > 256)
        })
        || tree.nodes.iter().map(|node| node.text.len()).sum::<usize>() > 64 * 1024
        || tree
            .nodes
            .iter()
            .map(|node| node.children.len())
            .try_fold(0_usize, usize::checked_add)
            .is_none_or(|children| children > 1024)
    {
        return Err(PluginUiError::new(
            PluginUiErrorCode::InvalidTree,
            "retained UI node limit exceeded",
        ));
    }
    let nodes = tree
        .nodes
        .into_iter()
        .map(|node| PluginUiNode {
            id: node.id,
            kind: match node.kind {
                component_api::dust::plugin::host::UiNodeKind::Container => {
                    PluginUiNodeKind::Container
                }
                component_api::dust::plugin::host::UiNodeKind::Text => PluginUiNodeKind::Text,
                component_api::dust::plugin::host::UiNodeKind::ListNode => PluginUiNodeKind::List,
                component_api::dust::plugin::host::UiNodeKind::Button => PluginUiNodeKind::Button,
            },
            text: node.text,
            style: match node.style {
                component_api::dust::plugin::host::UiStyle::Default => PluginUiStyle::Default,
                component_api::dust::plugin::host::UiStyle::Muted => PluginUiStyle::Muted,
                component_api::dust::plugin::host::UiStyle::Accent => PluginUiStyle::Accent,
                component_api::dust::plugin::host::UiStyle::Warning => PluginUiStyle::Warning,
                component_api::dust::plugin::host::UiStyle::Error => PluginUiStyle::Error,
                component_api::dust::plugin::host::UiStyle::Selected => PluginUiStyle::Selected,
                component_api::dust::plugin::host::UiStyle::Border => PluginUiStyle::Border,
            },
            action: node.action,
            children: node.children,
        })
        .collect();
    Ok(PluginUiTree {
        revision: tree.revision,
        root: tree.root,
        nodes,
    })
}

pub(super) fn convert_component_placement(
    placement: component_api::dust::plugin::host::WindowPlacement,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    size: u16,
) -> PluginRequestedPlacement {
    use component_api::dust::plugin::host::WindowPlacement;
    match placement {
        WindowPlacement::DockedLeft => PluginRequestedPlacement::DockedLeft { size },
        WindowPlacement::DockedRight => PluginRequestedPlacement::DockedRight { size },
        WindowPlacement::DockedTop => PluginRequestedPlacement::DockedTop { size },
        WindowPlacement::DockedBottom => PluginRequestedPlacement::DockedBottom { size },
        WindowPlacement::Floating => PluginRequestedPlacement::Floating {
            x,
            y,
            width,
            height,
        },
        WindowPlacement::Fullscreen => PluginRequestedPlacement::Fullscreen,
        WindowPlacement::Hidden => PluginRequestedPlacement::Hidden,
    }
}

pub(super) fn convert_private_style(
    style: component_api::dust::plugin::host::PrivateStyle,
) -> PluginPrivateStyle {
    PluginPrivateStyle {
        id: style.id,
        style: match style.style {
            component_api::dust::plugin::host::UiStyle::Default => PluginUiStyle::Default,
            component_api::dust::plugin::host::UiStyle::Muted => PluginUiStyle::Muted,
            component_api::dust::plugin::host::UiStyle::Accent => PluginUiStyle::Accent,
            component_api::dust::plugin::host::UiStyle::Warning => PluginUiStyle::Warning,
            component_api::dust::plugin::host::UiStyle::Error => PluginUiStyle::Error,
            component_api::dust::plugin::host::UiStyle::Selected => PluginUiStyle::Selected,
            component_api::dust::plugin::host::UiStyle::Border => PluginUiStyle::Border,
        },
    }
}

pub(super) fn convert_private_cell(
    cell: component_api::dust::plugin::host::PrivateCell,
) -> PluginPrivateCell {
    PluginPrivateCell {
        x: cell.x,
        y: cell.y,
        grapheme: cell.grapheme,
        continuation: cell.continuation,
        style: cell.style,
    }
}

pub(super) fn convert_private_hit(
    hit: component_api::dust::plugin::host::PrivateHitRegion,
) -> PluginPrivateHitRegion {
    PluginPrivateHitRegion {
        x: hit.x,
        y: hit.y,
        width: hit.width,
        height: hit.height,
        action: hit.action,
    }
}

pub(super) fn convert_private_snapshot(
    snapshot: component_api::dust::plugin::host::PrivateSnapshot,
) -> PluginPrivateSnapshot {
    PluginPrivateSnapshot {
        revision: snapshot.revision,
        dimensions_revision: snapshot.dimensions_revision,
        styles: snapshot
            .styles
            .into_iter()
            .map(convert_private_style)
            .collect(),
        cells: snapshot
            .cells
            .into_iter()
            .map(convert_private_cell)
            .collect(),
        cursor: snapshot.cursor.map(|cursor| PluginPrivateCursor {
            x: cursor.x,
            y: cursor.y,
        }),
        hit_regions: snapshot
            .hit_regions
            .into_iter()
            .map(convert_private_hit)
            .collect(),
    }
}

pub(super) fn convert_private_patch(
    patch: component_api::dust::plugin::host::PrivatePatch,
) -> PluginPrivatePatch {
    PluginPrivatePatch {
        revision: patch.revision,
        expected_surface_revision: patch.expected_surface_revision,
        dimensions_revision: patch.dimensions_revision,
        dirty: patch
            .dirty
            .into_iter()
            .map(|rect| PluginPrivateRect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            })
            .collect(),
        styles: patch
            .styles
            .into_iter()
            .map(convert_private_style)
            .collect(),
        cells: patch.cells.into_iter().map(convert_private_cell).collect(),
        replace_cursor: patch.replace_cursor,
        cursor: patch.cursor.map(|cursor| PluginPrivateCursor {
            x: cursor.x,
            y: cursor.y,
        }),
        replace_hit_regions: patch.replace_hit_regions,
        hit_regions: patch
            .hit_regions
            .into_iter()
            .map(convert_private_hit)
            .collect(),
    }
}

pub(super) fn convert_component_ui_request(
    request: component_api::dust::plugin::host::UiRequest,
    declared_content_types: &BTreeSet<String>,
) -> Result<PluginUiRequestKind, PluginUiError> {
    use component_api::dust::plugin::host::UiRequest;
    Ok(match request {
        UiRequest::OpenWindow(request) => {
            if !declared_content_types.contains(&request.content_type) {
                return Err(PluginUiError::new(
                    PluginUiErrorCode::UndeclaredContent,
                    "content type was not statically declared",
                ));
            }
            if crate::windows::validate_title(&request.title).is_err() {
                return Err(PluginUiError::new(
                    PluginUiErrorCode::InvalidTree,
                    "window title limit exceeded",
                ));
            }
            PluginUiRequestKind::OpenWindow {
                expected_graph_revision: request.expected_graph_revision,
                content_type: request.content_type,
                title: request.title,
                placement: convert_component_placement(
                    request.placement,
                    request.x,
                    request.y,
                    request.width,
                    request.height,
                    request.size,
                ),
                tree: request.tree.map(convert_component_tree).transpose()?,
            }
        }
        UiRequest::ReplaceUi(request) => PluginUiRequestKind::ReplaceUi {
            handle: request.handle,
            expected_tree_revision: request.expected_tree_revision,
            tree: convert_component_tree(request.tree)?,
        },
        UiRequest::PlaceWindow(request) => PluginUiRequestKind::PlaceWindow {
            handle: request.handle,
            expected_graph_revision: request.expected_graph_revision,
            placement: convert_component_placement(
                request.placement,
                request.x,
                request.y,
                request.width,
                request.height,
                request.size,
            ),
        },
        UiRequest::FocusWindow(request) => PluginUiRequestKind::FocusWindow {
            handle: request.handle,
            expected_graph_revision: request.expected_graph_revision,
        },
        UiRequest::CloseWindow(request) => PluginUiRequestKind::CloseWindow {
            handle: request.handle,
            expected_graph_revision: request.expected_graph_revision,
        },
        UiRequest::ReplacePrivate(request) => {
            if request.snapshot.cells.len() > usize::from(MAXIMUM_PRIVATE_CELLS_PER_CONTENT)
                || request.snapshot.styles.len() > usize::from(MAXIMUM_PRIVATE_STYLES)
                || request.snapshot.hit_regions.len() > usize::from(MAXIMUM_PRIVATE_HIT_REGIONS)
                || request
                    .snapshot
                    .cells
                    .iter()
                    .map(|cell| cell.grapheme.len())
                    .chain(
                        request
                            .snapshot
                            .hit_regions
                            .iter()
                            .map(|hit| hit.action.len()),
                    )
                    .try_fold(0_usize, usize::checked_add)
                    .is_none_or(|bytes| bytes > MAXIMUM_PRIVATE_BYTES as usize)
            {
                return Err(PluginUiError::new(
                    PluginUiErrorCode::PatchLimit,
                    "private snapshot exceeds absolute host limits",
                ));
            }
            PluginUiRequestKind::ReplacePrivate {
                handle: request.handle,
                snapshot: convert_private_snapshot(request.snapshot),
            }
        }
        UiRequest::PatchPrivate(request) => {
            if request.patch.cells.len() > usize::from(MAXIMUM_PRIVATE_PATCH_CELLS)
                || request.patch.dirty.len() > usize::from(MAXIMUM_PRIVATE_DIRTY_RECTANGLES)
                || request.patch.styles.len() > usize::from(MAXIMUM_PRIVATE_STYLES)
                || request.patch.hit_regions.len() > usize::from(MAXIMUM_PRIVATE_HIT_REGIONS)
                || request
                    .patch
                    .cells
                    .iter()
                    .map(|cell| cell.grapheme.len())
                    .chain(request.patch.hit_regions.iter().map(|hit| hit.action.len()))
                    .try_fold(0_usize, usize::checked_add)
                    .is_none_or(|bytes| bytes > MAXIMUM_PRIVATE_BYTES as usize)
            {
                return Err(PluginUiError::new(
                    PluginUiErrorCode::PatchLimit,
                    "private patch exceeds absolute host limits",
                ));
            }
            PluginUiRequestKind::PatchPrivate {
                handle: request.handle,
                patch: convert_private_patch(request.patch),
            }
        }
    })
}
