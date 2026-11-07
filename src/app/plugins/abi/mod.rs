//! Translate component commands and queued requests into native editor owners.

mod drawing;
mod events;
#[cfg(test)]
mod example_tests;
mod language;
mod surface;
mod system;
#[cfg(test)]
mod tests;

use crate::app::{App, commands::AppEffect};
use crate::editor::{
    CommandAccess, CommandAuthority, CommandInvocation, CommandOutcome, CommandValue,
    CommandWrapperAction, InvocationSource, PendingCommandId,
};
use crate::plugins::component::component_api::dust::plugin::host as wit;
use crate::plugins::requests::{EditorRequest, value_bytes};
use crate::plugins::{
    PluginCapability, PluginDelivery, PluginExecutionRequest, PluginInvocationOrigin, PluginLoadId,
};
use slotmap::Key;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

struct WaitingCall {
    origin: PluginInvocationOrigin,
    pending: Option<PendingCommandId>,
    call: wit::CommandCall,
    previous: Option<Box<CommandOutcome<AppEffect>>>,
    authority: CommandAuthority,
    deadline: Instant,
}

#[derive(Default)]
pub(in crate::app) struct PluginCalls {
    surfaces: surface::Surfaces,
    next: u64,
    waiting: BTreeMap<(String, u64), WaitingCall>,
    replies: BTreeMap<String, Vec<wit::CommandReply>>,
    reply_handlers: BTreeMap<String, String>,
    subscriptions: BTreeMap<String, (String, Option<u64>)>,
    marks: BTreeMap<(String, u64, String), Vec<(wit::Mark, crate::editor::decorations::MarkId)>>,
    diagnostics: BTreeMap<(String, String), crate::workspace::DiagnosticProducer>,
    layers: Vec<(String, String)>,
    mode: Option<String>,
    timers: BTreeMap<(String, u64), (Instant, String, PluginLoadId)>,
    completions: BTreeMap<(String, String), (String, bool)>,
    completion_observation: Option<(u64, u64, u64)>,
    http: Option<crate::plugins::http::PluginHttp>,
    http_results: BTreeMap<String, Vec<wit::HttpResponse>>,
    system: system::SystemAccess,
    registrations: BTreeSet<(String, String)>,
    running: Option<(String, u64)>,
    awaiting: BTreeMap<PendingCommandId, (String, u64)>,
}

pub(in crate::app) fn encode_value(value: &CommandValue) -> wit::CommandValue {
    fn append(value: &CommandValue, nodes: &mut Vec<wit::ValueNode>) -> u32 {
        let node = match value {
            CommandValue::String(value) => wit::ValueNode::Text(value.clone()),
            CommandValue::Bool(value) => wit::ValueNode::Boolean(*value),
            CommandValue::Integer(value) => wit::ValueNode::Integer(*value),
            CommandValue::Number(value) => {
                wit::ValueNode::Number(value.as_f64().expect("finite JSON number"))
            }
            CommandValue::List(values) => {
                wit::ValueNode::Sequence(values.iter().map(|value| append(value, nodes)).collect())
            }
            CommandValue::Record(values) => wit::ValueNode::Object(
                values
                    .iter()
                    .map(|(name, value)| wit::ValueField {
                        name: name.clone(),
                        value: append(value, nodes),
                    })
                    .collect(),
            ),
        };
        let index = nodes.len() as u32;
        nodes.push(node);
        index
    }
    let mut nodes = Vec::new();
    append(value, &mut nodes);
    wit::CommandValue { nodes }
}

pub(in crate::app) fn decode_value(value: wit::CommandValue) -> Result<CommandValue, String> {
    value_bytes(&value)?;
    let mut values: Vec<Option<CommandValue>> = Vec::with_capacity(value.nodes.len());
    for node in value.nodes {
        let value = match node {
            wit::ValueNode::Text(value) => CommandValue::String(value),
            wit::ValueNode::Boolean(value) => CommandValue::Bool(value),
            wit::ValueNode::Integer(value) => CommandValue::Integer(value),
            wit::ValueNode::Number(value) => CommandValue::Number(
                serde_json::Number::from_f64(value).ok_or("non-finite number")?,
            ),
            wit::ValueNode::Sequence(ids) => CommandValue::List(
                ids.into_iter()
                    .map(|id| values[id as usize].take().expect("validated tree"))
                    .collect(),
            ),
            wit::ValueNode::Object(fields) => CommandValue::Record(
                fields
                    .into_iter()
                    .map(|field| {
                        (
                            field.name,
                            values[field.value as usize].take().expect("validated tree"),
                        )
                    })
                    .collect(),
            ),
        };
        values.push(Some(value));
    }
    Ok(values.pop().flatten().expect("nonempty validated tree"))
}

pub(in crate::app) fn command_call(
    invocation: &CommandInvocation,
    previous: Option<&CommandValue>,
) -> wit::CommandCall {
    wit::CommandCall {
        command: invocation.id.to_string(),
        arguments: encode_value(&CommandValue::Record(invocation.arguments.clone())),
        buffer: invocation.target.buffer.map(|id| id.data().as_ffi()),
        view: invocation.target.view.map(|id| id.data().as_ffi()),
        count: invocation.count.map(std::num::NonZeroU64::get),
        register: invocation.register,
        pending: None,
        after: false,
        previous: previous.map(encode_value),
    }
}

fn callback_call(kind: &str) -> wit::CommandCall {
    wit::CommandCall {
        arguments: encode_value(&CommandValue::Record(BTreeMap::from([(
            "event".into(),
            CommandValue::String(kind.into()),
        )]))),
        ..Default::default()
    }
}

fn invocation(call: &wit::CommandCall) -> Result<CommandInvocation, String> {
    let mut invocation = CommandInvocation::new(&call.command, InvocationSource::Plugin)
        .map_err(|error| error.to_string())?;
    let CommandValue::Record(arguments) = decode_value(call.arguments.clone())? else {
        return Err("command arguments must be a record".into());
    };
    invocation.arguments = arguments;
    invocation.target.buffer = call.buffer.map(|id| slotmap::KeyData::from_ffi(id).into());
    invocation.target.view = call.view.map(|id| slotmap::KeyData::from_ffi(id).into());
    invocation.count = call
        .count
        .map(|count| std::num::NonZeroU64::new(count).ok_or("count must be positive"))
        .transpose()?;
    invocation.register = call.register;
    Ok(invocation)
}

impl App {
    pub(in crate::app) fn plugin_authority(&self, plugin: &str) -> CommandAuthority {
        let mut grants = BTreeSet::new();
        if let Some(index) = self.extension.contributions().get(plugin) {
            for capability in &index.capabilities {
                match capability {
                    PluginCapability::ProposeEdits => {
                        grants.insert(CommandAccess::Edit);
                    }
                    PluginCapability::ShowMessage
                    | PluginCapability::Input
                    | PluginCapability::PrivateCells => {
                        grants.insert(CommandAccess::Ui);
                    }
                    PluginCapability::Files => {
                        grants.insert(CommandAccess::Files);
                    }
                    PluginCapability::Process => {
                        grants.insert(CommandAccess::Process);
                    }
                    PluginCapability::Http => {
                        grants.insert(CommandAccess::Network);
                    }
                    _ => {}
                }
            }
        }
        CommandAuthority::Plugin {
            initiator: plugin.into(),
            grants,
        }
    }

    pub(in crate::app) fn execute_plugin_call(
        &mut self,
        plugin: String,
        handler: String,
        mut call: wit::CommandCall,
        pending: Option<PendingCommandId>,
        previous: Option<Box<CommandOutcome<AppEffect>>>,
        authority: CommandAuthority,
    ) -> bool {
        let result = (|| -> Result<(), String> {
            if self.extension.abi.waiting.len() >= 64 {
                return Err("plugin callback limit exceeded".into());
            }
            let id = self
                .extension
                .abi
                .next
                .checked_add(1)
                .ok_or("plugin callback IDs exhausted")?;
            self.extension.abi.next = id;
            if call.command.is_empty() {
                call.command = format!("plugin.{plugin}.{handler}");
            }
            call.pending = pending.map(|_| id);
            call.after = previous.is_some();
            let mut delivery = self.plugin_delivery(&plugin)?;
            let delivered_sequence = delivery.events.as_ref().map(|batch| batch.sequence);
            if let Some(view) = call.view {
                let id = slotmap::KeyData::from_ffi(view).into();
                let view_state = self
                    .state
                    .views
                    .get(id)
                    .ok_or("plugin target view closed")?;
                if call
                    .buffer
                    .is_some_and(|buffer| buffer != view_state.buffer.data().as_ffi())
                {
                    return Err("plugin target view and buffer disagree".into());
                }
                delivery.view = Some(wit::ViewState {
                    view,
                    buffer: view_state.buffer.data().as_ffi(),
                    cursor: view_state.cursor.0 as u64,
                    mode: format!("{:?}", view_state.mode),
                });
            }
            delivery.replies = self
                .extension
                .abi
                .replies
                .remove(&plugin)
                .unwrap_or_default();
            delivery.http = self
                .extension
                .abi
                .http_results
                .remove(&plugin)
                .unwrap_or_default();
            let system_authority = authority.clone();
            if pending.is_none()
                && decode_value(call.arguments.clone())?.as_record().is_ok_and(|arguments| {
                    matches!(arguments.get("event"), Some(CommandValue::String(event)) if event == "system")
                })
            {
                delivery.system = self.extension.abi.system.delivery(&plugin, &handler, &authority);
            }
            let system_count = delivery.system.len();
            let snapshot = None;
            let own = self.plugin_authority(&plugin);
            let call_authority = match (authority, own) {
                (CommandAuthority::Host, own) => own,
                (
                    CommandAuthority::Plugin { initiator, grants },
                    CommandAuthority::Plugin { grants: own, .. },
                ) => CommandAuthority::Plugin {
                    initiator,
                    grants: grants.intersection(&own).copied().collect(),
                },
                (authority, _) => authority,
            };
            let navigation = self
                .window_session
                .navigation_stamp(self.state.window_version(), self.state.active_view());
            let interaction_available = self.input.prompt().is_none()
                && self.ui.picker.is_none()
                && self.ui.settings.is_none()
                && self.input.documentation().is_none();
            let loan = self
                .extension
                .edits
                .prepare_interactive(
                    &mut self.extension.manager,
                    (&plugin, &handler),
                    &self.state,
                    &self.workspace,
                    self.configuration.binding_revision_ref(),
                    (
                        1024 * 1024,
                        Some(self.input.capture_epoch()),
                        Some(navigation),
                        interaction_available,
                    ),
                )?
                .direct(call.buffer.map(|id| slotmap::KeyData::from_ffi(id).into()))?;
            let origin = loan.origin().clone();
            let reservation = self.extension.confirmations.reserve(&origin);
            let confirmation_available = reservation.available();
            let request = PluginExecutionRequest {
                runtime_epoch: self.extension.runtime_epoch,
                plugin: plugin.clone(),
                command: handler.clone(),
                arguments: Vec::new(),
                call: call.clone(),
                delivery,
                snapshot,
                config: BTreeMap::new(),
                ui_revision: self.window_session.graph().revision(),
                confirmation_available,
                ui_events: self
                    .extension
                    .ui
                    .remove(&(plugin.clone(), origin.load()))
                    .unwrap_or_default(),
                confirmation_results: Vec::new(),
                host_events: self.extension.events.deliver(&plugin, origin.load()),
                storage_results: self
                    .extension
                    .results
                    .remove(&(plugin.clone(), origin.load()))
                    .unwrap_or_default(),
                provider_requests: self.extension.providers.requests(
                    &plugin,
                    origin.load(),
                    &format!("plugin.{plugin}.{handler}"),
                ),
                virtual_document_requests: self.extension.virtual_documents.request_lane().deliver(
                    &plugin,
                    origin.load(),
                    &format!("plugin.{plugin}.{handler}"),
                ),
                request_id: id,
            };
            reservation.enqueue(|| loan.submit(request))?;
            self.extension
                .abi
                .system
                .delivered(&plugin, &handler, &system_authority, system_count);
            if let Some(sequence) = delivered_sequence
                && let Some((_, after)) = self.extension.abi.subscriptions.get_mut(&plugin)
            {
                *after = Some(sequence);
            }
            self.extension.abi.waiting.insert(
                (plugin, id),
                WaitingCall {
                    origin,
                    pending,
                    call,
                    previous,
                    authority: call_authority,
                    deadline: Instant::now() + Duration::from_secs(2),
                },
            );
            Ok(())
        })();
        if let Err(error) = result {
            self.show_error(error);
            return false;
        }
        true
    }

    pub(in crate::app::plugins) fn apply_plugin_call_source(
        &mut self,
        source: &mut crate::app::plugins::edits::SourceExecution,
    ) {
        let key = self
            .extension
            .abi
            .waiting
            .iter()
            .find_map(|(key, waiting)| {
                source
                    .confirmation_source
                    .origin()
                    .same(&waiting.origin)
                    .then(|| key.clone())
            });
        let Some(key) = key else {
            if source.source_actions_allowed && !source.confirmation_result {
                let plugin = source.execution.plugin.clone();
                let authority = self.plugin_authority(&plugin);
                for request in std::mem::take(&mut source.execution.editor_requests) {
                    if let Err(error) = self.apply_editor_request(&plugin, &authority, request) {
                        self.show_error(error);
                        break;
                    }
                }
                let previous = std::mem::replace(&mut self.effect_authority, authority);
                for message in std::mem::take(&mut source.execution.messages) {
                    self.notify_message(message, false);
                }
                self.effect_authority = previous;
            }
            return;
        };
        let waiting = self
            .extension
            .abi
            .waiting
            .remove(&key)
            .expect("matched callback");
        let execution = &mut source.execution;
        if !self.extension.is_active_load(&key.0, waiting.origin.load())
            || Instant::now() > waiting.deadline
        {
            self.finish_surface_callback(
                &key,
                Err("surface callback timed out or unloaded".into()),
            );
            if let Some(pending) = waiting.pending {
                self.cancel_plugin_pending_command(pending);
            }
            source.source_actions_allowed = false;
            return;
        }
        let surface_value = execution.value.clone();
        let result = (|| -> Result<(), String> {
            if let Some(error) = &execution.error {
                return Err(error.clone());
            }
            if !source.source_actions_allowed
                || !source.confirmation_source.current(
                    &self.extension.manager,
                    &self.state,
                    &self.workspace.version(),
                    self.configuration.binding_revision(),
                )
            {
                return Err("plugin callback source changed".into());
            }
            self.extension
                .abi
                .reply_handlers
                .insert(key.0.clone(), execution.command.clone());
            for request in std::mem::take(&mut execution.editor_requests) {
                self.apply_editor_request(&key.0, &waiting.authority, request)?;
            }
            let mut outcome = waiting
                .previous
                .map(|outcome| *outcome)
                .unwrap_or_else(CommandOutcome::empty);
            if let Some(value) = execution.value.take() {
                outcome.value = Some(decode_value(value)?);
            }
            if !execution.proposals.is_empty() {
                if let CommandAuthority::Plugin { grants, .. } = &waiting.authority
                    && !grants.contains(&CommandAccess::Edit)
                {
                    return Err("edit capability denied".into());
                }
                let mut transaction =
                    crate::editor::EditorTransaction::new(crate::editor::TransactionId(key.1));
                for proposal in std::mem::take(&mut execution.proposals) {
                    let buffer = slotmap::KeyData::from_ffi(proposal.buffer).into();
                    let text = &self.state.buffers.get(buffer).ok_or("buffer closed")?.text;
                    if text.revision().0 != proposal.revision {
                        return Err("stale plugin edit".into());
                    }
                    let edits = proposal
                        .edits
                        .into_iter()
                        .map(|edit| {
                            crate::editor::text::TextEdit::new(
                                crate::editor::text::ByteRange {
                                    start: crate::editor::text::ByteOffset(edit.start as usize),
                                    end: crate::editor::text::ByteOffset(edit.end as usize),
                                },
                                edit.replacement,
                            )
                        })
                        .collect();
                    transaction =
                        transaction.with_operation(crate::editor::EditorOperation::ApplyEdits {
                            buffer,
                            edits: crate::editor::text::TextEditSet::new(text.revision(), edits),
                        });
                }
                outcome.transaction = Some(transaction);
            }
            if let Some(pending) = waiting.pending {
                let action = if execution.call_next.is_some() {
                    if execution.call_next != waiting.call.pending {
                        return Err("wrong wrapper callback".into());
                    }
                    let mut next = invocation(&waiting.call)?;
                    if let Some(value) = outcome.value {
                        let CommandValue::Record(arguments) = value else {
                            return Err("wrapper arguments must be a record".into());
                        };
                        next.arguments = arguments;
                    }
                    CommandWrapperAction::CallNext(next)
                } else {
                    CommandWrapperAction::Complete(outcome)
                };
                self.process_effect(AppEffect::CommandResume {
                    pending,
                    owner: key.0.clone(),
                    action: Box::new(action),
                });
            } else if let Some(transaction) = outcome.transaction {
                self.state
                    .apply_transaction(&transaction)
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })();
        if result.is_ok() {
            let previous = std::mem::replace(&mut self.effect_authority, waiting.authority);
            for message in std::mem::take(&mut source.execution.messages) {
                self.notify_message(message, false);
            }
            self.effect_authority = previous;
        }
        self.finish_surface_callback(&key, result.clone().map(|()| surface_value));
        if let Err(error) = result {
            if let Some(pending) = waiting.pending {
                self.cancel_plugin_pending_command(pending);
            }
            source.source_actions_allowed = false;
            self.show_error(error);
        }
    }

    fn handler(&self, plugin: &str, handler: &str) -> Result<(), String> {
        if self
            .extension
            .contributions()
            .get(plugin)
            .is_some_and(|index| index.commands.contains_key(handler))
        {
            Ok(())
        } else {
            Err("callback handler is not declared".into())
        }
    }

    fn apply_editor_request(
        &mut self,
        plugin: &str,
        authority: &CommandAuthority,
        request: EditorRequest,
    ) -> Result<(), String> {
        match request {
            EditorRequest::System(token, request) => {
                let authority = self.effect_authority.restrict(authority);
                let previous = std::mem::replace(&mut self.effect_authority, authority);
                let result = self.apply_system_request(plugin, token, request);
                self.effect_authority = previous;
                result?;
            }
            EditorRequest::Surface(request) => {
                self.apply_surface_request(plugin, authority, request)?
            }
            EditorRequest::Register(definition) => {
                self.register_guest_command(plugin, definition)?
            }
            EditorRequest::Run(token, call) => {
                if self
                    .extension
                    .abi
                    .replies
                    .get(plugin)
                    .is_some_and(|replies| replies.len() >= 64)
                    || self.extension.abi.awaiting.len() >= 64
                {
                    return Err("command reply limit exceeded".into());
                }
                if self.extension.abi.running.is_some() {
                    return Err("nested plugin dispatch is unavailable".into());
                }
                let invocation = invocation(&call)?;
                self.extension.abi.running = Some((plugin.into(), token));
                let succeeded = self.invoke_with_authority(invocation, authority.clone());
                if self.extension.abi.running.is_some() {
                    self.plugin_command_result(
                        None,
                        (!succeeded).then_some("plugin command failed".into()),
                    );
                }
            }
            EditorRequest::Wrap(wrapper) => {
                self.install_plugin_wrapper(plugin, wrapper, authority.clone())?
            }
            EditorRequest::LspFilter(filter) => {
                let command = crate::app::language::incoming::command_id(&filter.method);
                self.install_plugin_wrapper(
                    plugin,
                    wit::CommandWrapper {
                        command,
                        handler: filter.handler,
                        order: filter.order,
                    },
                    authority.clone(),
                )?;
            }
            EditorRequest::EnterMode(mode) => {
                let mode = native_mode(plugin, mode)?;
                if !self.process_effect_checked(AppEffect::Authorized {
                    authority: authority.clone(),
                    effect: Box::new(AppEffect::Input(
                        crate::app::input::InputCommand::EnterMode(Box::new(mode)),
                    )),
                }) {
                    return Err("mode capability denied".into());
                }
                self.extension.abi.mode = Some(plugin.into());
            }
            EditorRequest::LeaveMode => {
                if self.extension.abi.mode.as_deref() != Some(plugin) {
                    return Err("mode belongs to another plugin".into());
                }
                if !self.process_effect_checked(AppEffect::Authorized {
                    authority: authority.clone(),
                    effect: Box::new(AppEffect::Input(crate::app::input::InputCommand::LeaveMode)),
                }) {
                    return Err("mode capability denied".into());
                }
                self.extension.abi.mode = None;
            }
            EditorRequest::PushLayer(layer) => {
                if self.extension.abi.layers.len() >= 16 {
                    return Err("key layer limit exceeded".into());
                }
                let name = layer.name.clone();
                let layer = native_layer(plugin, layer)?;
                if !self.process_effect_checked(AppEffect::Authorized {
                    authority: authority.clone(),
                    effect: Box::new(AppEffect::Input(
                        crate::app::input::InputCommand::PushNamedLayer {
                            owner: plugin.into(),
                            name: name.clone(),
                            layer: Box::new(layer),
                        },
                    )),
                }) {
                    return Err("key layer capability denied".into());
                }
                self.extension.abi.layers.push((plugin.into(), name));
            }
            EditorRequest::PopLayer(name) => {
                if !self.process_effect_checked(AppEffect::Authorized {
                    authority: authority.clone(),
                    effect: Box::new(AppEffect::Input(
                        crate::app::input::InputCommand::RemoveNamedLayer {
                            owner: plugin.into(),
                            name: name.clone(),
                        },
                    )),
                }) {
                    return Err("key layer capability denied".into());
                }
                self.extension
                    .abi
                    .layers
                    .retain(|(owner, layer)| owner != plugin || layer != &name);
            }
            EditorRequest::Marks(set) => self.replace_plugin_marks(plugin, authority, set)?,
            EditorRequest::Diagnostics(set) => {
                self.publish_plugin_diagnostics(plugin, authority, set)?
            }
            EditorRequest::Subscribe(handler) => {
                self.handler(plugin, &handler)?;
                self.extension
                    .abi
                    .subscriptions
                    .insert(plugin.into(), (handler, None));
            }
            EditorRequest::CompletionSource(source) => {
                self.require_plugin_drawing(plugin, authority)?;
                self.install_plugin_completion(plugin, source)?
            }
            EditorRequest::Completions(batch) => {
                self.require_plugin_drawing(plugin, authority)?;
                self.publish_plugin_completions(plugin, batch)?;
            }
            EditorRequest::Timer(token, request) => {
                self.handler(plugin, &request.handler)?;
                if self.extension.abi.timers.len() >= 64 {
                    return Err("timer limit exceeded".into());
                }
                let load = self.extension.load_id(plugin).ok_or("plugin unloaded")?;
                self.extension.abi.timers.insert(
                    (plugin.into(), token),
                    (
                        Instant::now() + Duration::from_millis(request.milliseconds.into()),
                        request.handler,
                        load,
                    ),
                );
            }
            EditorRequest::CancelTimer(token) => {
                self.extension.abi.timers.remove(&(plugin.into(), token));
            }
            EditorRequest::Http(token, request) => {
                self.workspace
                    .execution_allowed()
                    .map_err(|_| "HTTP requires a trusted workspace".to_owned())?;
                if let CommandAuthority::Plugin { grants, .. } = authority
                    && !grants.contains(&CommandAccess::Network)
                {
                    return Err("network capability denied".into());
                }
                self.handler(plugin, &request.handler)?;
                let index = self
                    .extension
                    .contributions()
                    .get(plugin)
                    .ok_or("plugin unloaded")?;
                let hosts = index
                    .capabilities
                    .iter()
                    .filter_map(|cap| match cap {
                        PluginCapability::HttpHost(host) => Some(host.clone()),
                        _ => None,
                    })
                    .collect();
                let load = self.extension.load_id(plugin).ok_or("plugin unloaded")?;
                self.extension
                    .abi
                    .http
                    .get_or_insert_with(crate::plugins::http::PluginHttp::start)
                    .request(plugin.into(), load, token, request, hosts)?;
            }
        }
        Ok(())
    }

    pub(in crate::app) fn plugin_command_pending(&mut self, pending: PendingCommandId) {
        if let Some(request) = self.extension.abi.running.take() {
            self.extension.abi.awaiting.insert(pending, request);
        }
    }

    pub(in crate::app) fn plugin_command_resuming(&mut self, pending: PendingCommandId) {
        self.extension.abi.running = self.extension.abi.awaiting.remove(&pending);
    }

    pub(in crate::app) fn plugin_command_result(
        &mut self,
        value: Option<&CommandValue>,
        error: Option<String>,
    ) {
        if let Some((plugin, request)) = self.extension.abi.running.take() {
            self.extension
                .abi
                .replies
                .entry(plugin)
                .or_default()
                .push(wit::CommandReply {
                    request,
                    value: value.map(encode_value),
                    error,
                });
        }
    }

    fn register_guest_command(
        &mut self,
        plugin: &str,
        definition: wit::CommandDefinition,
    ) -> Result<(), String> {
        self.handler(plugin, &definition.handler)?;
        if self.commands.specs().filter(|spec| matches!(&spec.source, crate::editor::CommandSource::Plugin(owner) if owner == plugin)).count() >= 256 { return Err("plugin command limit exceeded".into()); }
        if definition.title.is_empty()
            || definition.title.len() > 256
            || definition.title.chars().any(char::is_control)
        {
            return Err("invalid command title".into());
        }
        let id = crate::editor::CommandId::new(format!("plugin.{plugin}.{}", definition.name))
            .map_err(|error| error.to_string())?;
        let mut spec = crate::editor::CommandSpec::builtin("edit.insert_text", &definition.title)
            .map_err(|error| error.to_string())?;
        spec.id = id.clone();
        spec.source = crate::editor::CommandSource::Plugin(plugin.into());
        spec.access = definition
            .access
            .into_iter()
            .map(|access| match access {
                wit::CommandAccess::Edit => CommandAccess::Edit,
                wit::CommandAccess::Ui => CommandAccess::Ui,
                wit::CommandAccess::Files => CommandAccess::Files,
                wit::CommandAccess::Process => CommandAccess::Process,
                wit::CommandAccess::Network => CommandAccess::Network,
            })
            .collect();
        let kind = |kind| match kind {
            wit::ArgumentKind::Text => crate::editor::ArgumentType::String,
            wit::ArgumentKind::Boolean => crate::editor::ArgumentType::Bool,
            wit::ArgumentKind::Integer => crate::editor::ArgumentType::Integer,
            wit::ArgumentKind::Number => crate::editor::ArgumentType::Number,
            wit::ArgumentKind::Sequence => crate::editor::ArgumentType::List,
            wit::ArgumentKind::Object => crate::editor::ArgumentType::Record,
        };
        for field in definition.arguments {
            spec.arguments = spec
                .arguments
                .field(&field.name, kind(field.kind), field.required);
        }
        spec.result = definition.result_kind.map(kind);
        self.commands
            .register(spec, |_, _| Ok(CommandOutcome::empty()))
            .map_err(|error| error.to_string())?;
        self.install_plugin_wrapper(
            plugin,
            wit::CommandWrapper {
                command: id.to_string(),
                handler: definition.handler,
                order: 0,
            },
            self.plugin_authority(plugin),
        )
    }

    fn install_plugin_wrapper(
        &mut self,
        plugin: &str,
        wrapper: wit::CommandWrapper,
        authority: CommandAuthority,
    ) -> Result<(), String> {
        self.handler(plugin, &wrapper.handler)?;
        let key = (plugin.to_owned(), wrapper.command.clone());
        if self.extension.abi.registrations.len() >= 256 {
            return Err("wrapper limit exceeded".into());
        }
        if self.extension.abi.registrations.contains(&key) {
            return Err("wrapper already registered".into());
        }
        let id =
            crate::editor::CommandId::new(&wrapper.command).map_err(|error| error.to_string())?;
        let owner = plugin.to_owned();
        self.commands
            .register_wrapper(
                &id,
                owner.clone(),
                wrapper.order,
                authority,
                move |_, invocation, pending, previous| {
                    Ok(CommandWrapperAction::Pending(vec![AppEffect::PluginCall {
                        plugin: owner.clone(),
                        handler: wrapper.handler.clone(),
                        call: Box::new(command_call(
                            invocation,
                            previous.as_ref().and_then(|outcome| outcome.value.as_ref()),
                        )),
                        pending: Some(pending),
                        previous: previous.map(Box::new),
                    }]))
                },
            )
            .map_err(|error| error.to_string())?;
        self.extension.abi.registrations.insert(key);
        Ok(())
    }

    pub(in crate::app) fn remove_plugin_abi(&mut self, plugin: &str) {
        self.extension.abi.system.remove(plugin);
        self.remove_surface_plugin(plugin);
        let pending: Vec<_> = self
            .extension
            .abi
            .waiting
            .iter()
            .filter(|((owner, _), _)| owner == plugin)
            .filter_map(|(_, waiting)| waiting.pending)
            .collect();
        for pending in pending {
            self.cancel_plugin_pending_command(pending);
        }
        let layers: Vec<_> = self
            .extension
            .abi
            .layers
            .iter()
            .filter(|(owner, _)| owner == plugin)
            .map(|(_, name)| name.clone())
            .collect();
        for name in layers {
            self.process_effect(AppEffect::Input(
                crate::app::input::InputCommand::RemoveNamedLayer {
                    owner: plugin.into(),
                    name,
                },
            ));
        }
        self.extension
            .abi
            .layers
            .retain(|(owner, _)| owner != plugin);
        if self.extension.abi.mode.as_deref() == Some(plugin) {
            self.process_effect(AppEffect::Input(crate::app::input::InputCommand::LeaveMode));
            self.extension.abi.mode = None;
        }
        let marks: Vec<_> = self
            .extension
            .abi
            .marks
            .keys()
            .filter(|(owner, _, _)| owner == plugin)
            .cloned()
            .collect();
        for key in marks {
            if let Some(buffer) = self
                .state
                .buffers
                .get_mut(slotmap::KeyData::from_ffi(key.1).into())
            {
                buffer
                    .decorations
                    .clear(&format!("plugin.{plugin}.{}", key.2));
            }
            self.extension.abi.marks.remove(&key);
        }
        self.remove_plugin_diagnostics(plugin);
        let sources: Vec<_> = self
            .extension
            .abi
            .completions
            .keys()
            .filter(|(owner, _)| owner == plugin)
            .cloned()
            .collect();
        for key in sources {
            self.language
                .remove_completion_source(&format!("plugin.{plugin}.{}", key.1));
            self.extension.abi.completions.remove(&key);
        }
        self.project_lsp_completion();
        self.extension.abi.subscriptions.remove(plugin);
        self.extension.abi.replies.remove(plugin);
        self.extension.abi.reply_handlers.remove(plugin);
        self.extension.abi.http_results.remove(plugin);
        self.extension
            .abi
            .timers
            .retain(|(owner, _), _| owner != plugin);
        self.extension
            .abi
            .registrations
            .retain(|(owner, _)| owner != plugin);
        self.extension
            .abi
            .waiting
            .retain(|(owner, _), _| owner != plugin);
        self.extension
            .abi
            .awaiting
            .retain(|_, (owner, _)| owner != plugin);
        self.finish_incoming_lsp_commands();
    }

    pub(in crate::app) fn poll_plugin_callbacks(&mut self) {
        self.poll_system_callbacks();
        self.poll_surface_callbacks();
        let now = Instant::now();
        let expired: Vec<_> = self
            .extension
            .abi
            .waiting
            .iter()
            .filter(|(_, waiting)| {
                now > waiting.deadline || !waiting.origin.current(&self.extension.manager)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in expired {
            self.finish_surface_callback(
                &key,
                Err("surface callback timed out or unloaded".into()),
            );
            if let Some((_, after)) = self.extension.abi.subscriptions.get_mut(&key.0) {
                *after = None;
            }
            let waiting = self
                .extension
                .abi
                .waiting
                .get_mut(&key)
                .expect("expired callback");
            let current = waiting.origin.current(&self.extension.manager);
            let pending = waiting.pending.take();
            if !current {
                self.extension.abi.waiting.remove(&key);
            }
            if let Some(pending) = pending {
                self.cancel_plugin_pending_command(pending);
                self.show_error("plugin command timed out or unloaded");
            }
        }
        let mut callbacks = Vec::new();
        let timers: Vec<_> = self
            .extension
            .abi
            .timers
            .iter()
            .filter(|(_, (deadline, _, _))| now >= *deadline)
            .map(|(key, _)| key.clone())
            .collect();
        for key in timers {
            let (_, handler, load) = self.extension.abi.timers.remove(&key).expect("due timer");
            if self.extension.is_active_load(&key.0, load) {
                callbacks.push((key.0, handler, callback_call("timer")));
            }
        }
        if let Some(http) = &mut self.extension.abi.http {
            for reply in http.poll() {
                if self
                    .extension
                    .manager
                    .is_active_load(&reply.plugin, reply.load)
                {
                    self.extension
                        .abi
                        .http_results
                        .entry(reply.plugin.clone())
                        .or_default()
                        .push(reply.response);
                    callbacks.push((reply.plugin, reply.handler, callback_call("http")));
                }
            }
        }
        for (plugin, replies) in &self.extension.abi.replies {
            if !replies.is_empty()
                && !self
                    .extension
                    .abi
                    .waiting
                    .keys()
                    .any(|(owner, _)| owner == plugin)
                && let Some(handler) = self.extension.abi.reply_handlers.get(plugin)
            {
                callbacks.push((plugin.clone(), handler.clone(), callback_call("commands")));
            }
        }
        self.collect_plugin_event_callbacks(&mut callbacks);
        self.collect_plugin_completion_callbacks(&mut callbacks);
        for (plugin, handler, call) in callbacks {
            self.execute_plugin_call(plugin, handler, call, None, None, CommandAuthority::Host);
        }
    }
}

fn native_layer(
    plugin: &str,
    layer: wit::KeyLayer,
) -> Result<crate::editor::input::Keymap, String> {
    let mut map = crate::editor::input::Keymap::default();
    for binding in layer.bindings {
        if !binding.command.starts_with(&format!("plugin.{plugin}.")) {
            return Err("key layers may only call their owner's commands".into());
        }
        let keys = binding
            .keys
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<crate::editor::input::KeyEvent>, _>>()
            .map_err(|error| error.to_string())?;
        if keys.is_empty() || keys.len() > 8 {
            return Err("invalid key sequence".into());
        }
        let call = wit::CommandCall {
            command: binding.command,
            arguments: binding.arguments,
            ..Default::default()
        };
        let mut invocation = invocation(&call)?;
        invocation.source = InvocationSource::Keymap;
        if !map.insert(&keys, invocation) {
            return Err("duplicate key binding".into());
        }
    }
    Ok(map)
}

fn native_mode(
    plugin: &str,
    mode: wit::NamedMode,
) -> Result<crate::editor::input::NamedMode, String> {
    use crate::editor::input::{ModeCursor, ModeKeys, NamedMode};
    let cursor = match mode.cursor {
        wit::ModeCursor::Default => ModeCursor::Default,
        wit::ModeCursor::Block => ModeCursor::Block,
        wit::ModeCursor::Bar => ModeCursor::Bar,
        wit::ModeCursor::Underline => ModeCursor::Underline,
        wit::ModeCursor::BlinkingBlock => ModeCursor::BlinkingBlock,
        wit::ModeCursor::BlinkingBar => ModeCursor::BlinkingBar,
        wit::ModeCursor::BlinkingUnderline => ModeCursor::BlinkingUnderline,
    };
    let keys = match (mode.base, mode.capture) {
        (Some(base), None) => ModeKeys::Base(match base {
            wit::BaseMode::Normal => crate::editor::EditorMode::Normal,
            wit::BaseMode::Insert => crate::editor::EditorMode::Insert,
            wit::BaseMode::Replace => crate::editor::EditorMode::Replace,
            wit::BaseMode::VisualChar => crate::editor::EditorMode::VisualChar,
            wit::BaseMode::VisualLine => crate::editor::EditorMode::VisualLine,
            wit::BaseMode::VisualBlock => crate::editor::EditorMode::VisualBlock,
            wit::BaseMode::Multi => crate::editor::EditorMode::Multi,
        }),
        (None, Some(handler)) if handler.starts_with(&format!("plugin.{plugin}.")) => {
            ModeKeys::Capture(
                CommandInvocation::new(&handler, InvocationSource::Keymap)
                    .map_err(|error| error.to_string())?,
            )
        }
        _ => return Err("mode must choose a base policy or its own capture command".into()),
    };
    NamedMode::new(
        format!("plugin.{plugin}.{}", mode.name),
        mode.label,
        cursor,
        native_layer(plugin, mode.layer)?,
        keys,
    )
    .map_err(|error| error.to_string())
}
