//! Test-only execution lane for the normative target ABI. Production remains 0.2.
use std::collections::BTreeSet;

pub(crate) mod api {
    wasmtime::component::bindgen!({
        path: "docs/specs/api-next.wit",
        world: "editor-plugin",
    });
}

use api::dust::plugin::host as wire;

#[derive(Clone, Debug)]
pub(crate) struct StagedUiRequest {
    pub token: u64,
    pub invocation: wire::Invocation,
    pub request: wire::UiRequest,
}

struct HostState {
    limits: wasmtime::StoreLimits,
    invocation: Option<wire::Invocation>,
    next_token: u64,
    pending_tokens: BTreeSet<u64>,
    requests: Vec<StagedUiRequest>,
    notices: Vec<String>,
}

fn failure(code: wire::ErrorCode, message: impl Into<String>, retryable: bool) -> wire::Failure {
    wire::Failure {
        code,
        message: message.into(),
        retryable,
    }
}

fn unsupported(operation: &str) -> wire::Failure {
    failure(
        wire::ErrorCode::Unsupported,
        format!("target 0.3 {operation} is not implemented"),
        false,
    )
}

impl wire::Host for HostState {
    fn features(&mut self) -> Vec<wire::Feature> {
        vec![wire::Feature {
            id: "ui".into(),
            version: 1,
            available: true,
        }]
    }

    fn limits(&mut self) -> Vec<wire::Limit> {
        vec![
            wire::Limit {
                name: "per-load-requests".into(),
                value: 64,
            },
            wire::Limit {
                name: "event-batch".into(),
                value: 64,
            },
        ]
    }

    fn notify(&mut self, message: wire::Notice) -> Result<(), wire::Failure> {
        if message.message.is_empty() || message.message.len() > 4096 {
            return Err(failure(
                wire::ErrorCode::InvalidArgument,
                "notice is empty or oversized",
                false,
            ));
        }
        self.notices.push(message.message);
        Ok(())
    }

    fn queue_ui(&mut self, request: wire::UiRequest) -> Result<u64, wire::Failure> {
        let invocation = self.invocation.clone().ok_or_else(|| {
            failure(
                wire::ErrorCode::StaleSource,
                "target UI call has no active invocation",
                false,
            )
        })?;
        if invocation.kind == wire::ExecutionKind::ResultOnly {
            return Err(failure(
                wire::ErrorCode::PermissionDenied,
                "result-only callback cannot open or clone a view",
                false,
            ));
        }
        let ui = invocation.ui.as_ref().ok_or_else(|| {
            failure(
                wire::ErrorCode::StaleSource,
                "target UI call has no admitted UI source",
                false,
            )
        })?;
        let wire::UiRequest::Open(open) = &request else {
            return Err(unsupported("queue-ui request"));
        };
        if open.content_type != "plugin.demo.panel"
            || open.title.is_empty()
            || open.title.len() > 128
            || open.graph_revision != ui.graph_revision
            || open.tab != ui.tab
            || open.content.is_some_and(|content| content != ui.content)
            || open.tree.as_ref().is_none_or(|tree| {
                tree.revision != 1 || tree.nodes.is_empty() || tree.nodes.len() > 4096
            })
        {
            return Err(failure(
                wire::ErrorCode::InvalidArgument,
                "target open-view does not match its admitted source or declaration",
                false,
            ));
        }
        if self.requests.len() >= 64 {
            return Err(failure(
                wire::ErrorCode::QueueFull,
                "target UI request queue is full",
                true,
            ));
        }
        let token = self.next_token;
        self.next_token = token.checked_add(1).ok_or_else(|| {
            failure(
                wire::ErrorCode::Exhausted,
                "target UI token space is exhausted",
                false,
            )
        })?;
        self.pending_tokens.insert(token);
        self.requests.push(StagedUiRequest {
            token,
            invocation,
            request,
        });
        Ok(token)
    }

    fn queue_interaction(
        &mut self,
        _request: wire::InteractionRequest,
    ) -> Result<u64, wire::Failure> {
        Err(unsupported("queue-interaction"))
    }
    fn update_interaction(
        &mut self,
        _update: wire::InteractionUpdate,
    ) -> Result<(), wire::Failure> {
        Err(unsupported("update-interaction"))
    }
    fn validate_submission(&mut self, _validation: wire::Validation) -> Result<(), wire::Failure> {
        Err(unsupported("validate-submission"))
    }
    fn queue_snapshot(&mut self, _request: wire::SnapshotRequest) -> Result<u64, wire::Failure> {
        Err(unsupported("queue-snapshot"))
    }
    fn propose_edits(&mut self, _proposal: wire::Proposal) -> Result<u64, wire::Failure> {
        Err(unsupported("propose-edits"))
    }
    fn publish_virtual(&mut self, _update: wire::VirtualUpdate) -> Result<(), wire::Failure> {
        Err(unsupported("publish-virtual"))
    }
    fn publish_provider(&mut self, _chunk: wire::ProviderChunk) -> Result<(), wire::Failure> {
        Err(unsupported("publish-provider"))
    }
    fn queue_storage(&mut self, _request: wire::StorageRequest) -> Result<u64, wire::Failure> {
        Err(unsupported("queue-storage"))
    }
    fn request_grant(&mut self, _request: wire::GrantRequest) -> Result<u64, wire::Failure> {
        Err(unsupported("request-grant"))
    }
    fn queue_service(&mut self, _request: wire::ServiceRequest) -> Result<u64, wire::Failure> {
        Err(unsupported("queue-service"))
    }
    fn cancel(&mut self, _request: u64) -> Result<(), wire::Failure> {
        Err(unsupported("cancel"))
    }
    fn release_handle(&mut self, _value: u64) -> Result<(), wire::Failure> {
        Err(unsupported("release-handle"))
    }
}

pub(crate) struct Runtime {
    store: wasmtime::Store<HostState>,
    bindings: api::EditorPlugin,
    commands: BTreeSet<String>,
}

impl Runtime {
    pub(crate) fn load(core: &[u8]) -> Result<Self, String> {
        let component = wit_component::ComponentEncoder::default()
            .module(core)
            .map_err(|error| error.to_string())?
            .validate(true)
            .encode()
            .map_err(|error| error.to_string())?;
        let mut config = wasmtime::Config::new();
        config
            .wasm_component_model(true)
            .consume_fuel(true)
            .epoch_interruption(true);
        let engine = wasmtime::Engine::new(&config).map_err(|error| error.to_string())?;
        let component = wasmtime::component::Component::from_binary(&engine, &component)
            .map_err(|error| error.to_string())?;
        let limits = wasmtime::StoreLimitsBuilder::new()
            .memory_size(64 * 1024 * 1024)
            .instances(64)
            .build();
        let mut store = wasmtime::Store::new(
            &engine,
            HostState {
                limits,
                invocation: None,
                next_token: 1,
                pending_tokens: BTreeSet::new(),
                requests: Vec::new(),
                notices: Vec::new(),
            },
        );
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(10_000_000)
            .map_err(|error| error.to_string())?;
        store.set_epoch_deadline(1);
        let mut linker = wasmtime::component::Linker::new(&engine);
        api::EditorPlugin::add_to_linker(&mut linker, |state| state)
            .map_err(|error| error.to_string())?;
        let bindings = api::EditorPlugin::instantiate(&mut store, &component, &linker)
            .map_err(|error| error.to_string())?;
        let commands = bindings
            .call_activate(&mut store)
            .map_err(|error| error.to_string())?
            .map_err(|failure| format!("{:?}: {}", failure.code, failure.message))?
            .into_iter()
            .collect();
        Ok(Self {
            store,
            bindings,
            commands,
        })
    }

    pub(crate) fn execute(&mut self, invocation: wire::Invocation) -> Result<(), String> {
        if !self.commands.contains(&invocation.command) {
            return Err("target command is not activated".into());
        }
        self.store.data_mut().invocation = Some(invocation.clone());
        let result = self
            .bindings
            .call_execute_command(&mut self.store, &invocation)
            .map_err(|error| error.to_string())?
            .map_err(|failure| format!("{:?}: {}", failure.code, failure.message));
        self.store.data_mut().invocation = None;
        result
    }

    pub(crate) fn handle_events(&mut self, events: Vec<wire::Event>) -> Result<(), String> {
        if events.is_empty() || events.len() > 64 {
            return Err("target event batch is empty or exceeds its ceiling".into());
        }
        let mut delivered = BTreeSet::new();
        for event in &events {
            if event.id == 0
                || event.context.kind != wire::ExecutionKind::ResultOnly
                || event.context.ui.is_some()
            {
                return Err("target result callback context is invalid".into());
            }
            if let wire::EventValue::UiResult((token, _)) = &event.value
                && (!self.store.data().pending_tokens.contains(token) || !delivered.insert(*token))
            {
                return Err("target UI result token is stale or duplicated".into());
            }
        }
        for token in &delivered {
            self.store.data_mut().pending_tokens.remove(token);
        }
        self.store.data_mut().invocation = events.first().map(|event| event.context.clone());
        let result = self
            .bindings
            .call_handle_events(&mut self.store, &events)
            .map_err(|error| error.to_string())?
            .map_err(|failure| format!("{:?}: {}", failure.code, failure.message));
        self.store.data_mut().invocation = None;
        result
    }

    pub(crate) fn deactivate(&mut self) -> Result<(), String> {
        self.bindings
            .call_deactivate(&mut self.store)
            .map_err(|error| error.to_string())?
            .map_err(|failure| format!("{:?}: {}", failure.code, failure.message))
    }

    pub(crate) fn activated_commands(&self) -> BTreeSet<String> {
        self.commands.clone()
    }

    pub(crate) fn take_requests(&mut self) -> Vec<StagedUiRequest> {
        std::mem::take(&mut self.store.data_mut().requests)
    }

    pub(crate) fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.store.data_mut().notices)
    }
}
