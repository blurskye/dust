//! Bounded editor requests and deliveries crossing the component worker.

use super::component::component_api::dust::plugin::host as wit;
use super::manifest::PluginCapability;

pub(crate) const REQUEST_LIMIT: usize = 64;
pub(crate) const VALUE_LIMIT: usize = 4096;
pub(crate) const BYTE_LIMIT: usize = 1024 * 1024;

impl Default for wit::CommandCall {
    fn default() -> Self {
        Self {
            command: String::new(),
            arguments: wit::CommandValue {
                nodes: vec![wit::ValueNode::Object(Vec::new())],
            },
            buffer: None,
            view: None,
            count: None,
            register: None,
            pending: None,
            after: false,
            previous: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum EditorRequest {
    Surface(crate::plugins::surface::SurfaceRequest),
    Register(wit::CommandDefinition),
    Run(u64, wit::CommandCall),
    Wrap(wit::CommandWrapper),
    EnterMode(wit::NamedMode),
    LeaveMode,
    PushLayer(wit::KeyLayer),
    PopLayer(String),
    Marks(wit::MarkSet),
    Diagnostics(wit::DiagnosticSet),
    Subscribe(String),
    CompletionSource(wit::CompletionSource),
    Completions(wit::CompletionBatch),
    LspFilter(wit::LspFilter),
    Timer(u64, wit::TimerRequest),
    CancelTimer(u64),
    Http(u64, wit::HttpRequest),
    System(u64, SystemRequest),
}

#[derive(Clone, Debug)]
pub(crate) enum SystemRequest {
    Start(wit::ProcessStart),
    Input(wit::ProcessInput),
    Kill(u64),
    File(wit::FileOperation, String),
    Query(wit::SyntaxQuery, String),
    Node(u64, u64, String),
}

#[derive(Clone, Debug, Default)]
pub struct PluginDelivery {
    pub(crate) view: Option<wit::ViewState>,
    pub(crate) events: Option<wit::EventBatch>,
    pub(crate) replies: Vec<wit::CommandReply>,
    pub(crate) http: Vec<wit::HttpResponse>,
    pub(crate) system: Vec<wit::SystemEvent>,
    pub(crate) marks: Vec<wit::MarkSet>,
}

#[derive(Default)]
pub(crate) struct ComponentRequests {
    pub(crate) delivery: PluginDelivery,
    pub(crate) queued: Vec<EditorRequest>,
    pub(crate) value: Option<wit::CommandValue>,
    pub(crate) call_next: Option<u64>,
    pub(crate) pending: Option<u64>,
    pub(crate) after: bool,
    next: u64,
    bytes: usize,
}

impl ComponentRequests {
    pub(crate) fn begin(&mut self, delivery: PluginDelivery, pending: Option<u64>, after: bool) {
        self.delivery = delivery;
        self.pending = pending;
        self.after = after;
        self.queued.clear();
        self.value = None;
        self.call_next = None;
        self.bytes = 0;
    }

    pub(crate) fn push(&mut self, request: EditorRequest, bytes: usize) -> Result<(), String> {
        if self.queued.len() >= REQUEST_LIMIT || bytes > BYTE_LIMIT.saturating_sub(self.bytes) {
            return Err("plugin request limit exceeded".into());
        }
        self.bytes += bytes;
        self.queued.push(request);
        Ok(())
    }

    pub(crate) fn token(&mut self) -> Result<u64, String> {
        self.next = self
            .next
            .checked_add(1)
            .ok_or("plugin request IDs exhausted")?;
        Ok(self.next)
    }
}

pub(crate) fn positional_value(arguments: Vec<String>) -> wit::CommandValue {
    let mut nodes = Vec::new();
    let fields = arguments
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            nodes.push(wit::ValueNode::Text(text));
            wit::ValueField {
                name: index.to_string(),
                value: index as u32,
            }
        })
        .collect();
    nodes.push(wit::ValueNode::Object(fields));
    wit::CommandValue { nodes }
}

pub(crate) fn value_bytes(value: &wit::CommandValue) -> Result<usize, String> {
    if value.nodes.is_empty() || value.nodes.len() > VALUE_LIMIT {
        return Err("command value node limit exceeded".into());
    }
    let mut bytes = 0usize;
    let mut used = vec![false; value.nodes.len()];
    let mut depths = Vec::with_capacity(value.nodes.len());
    for (index, node) in value.nodes.iter().enumerate() {
        let mut depth = 1;
        let mut child = |id: u32| -> Result<(), String> {
            let id = id as usize;
            if id >= index || used[id] {
                return Err("command values must be trees in postorder".into());
            }
            used[id] = true;
            depth = depth.max(depths[id] + 1);
            if depth > 64 {
                return Err("command value nesting limit exceeded".into());
            }
            Ok(())
        };
        bytes += match node {
            wit::ValueNode::Text(text) => text.len(),
            wit::ValueNode::Number(number) if !number.is_finite() => {
                return Err("non-finite number".into());
            }
            wit::ValueNode::Sequence(ids) => {
                for id in ids {
                    child(*id)?;
                }
                ids.len() * 4
            }
            wit::ValueNode::Object(fields) => {
                let mut names = std::collections::BTreeSet::new();
                for field in fields {
                    if !names.insert(&field.name) {
                        return Err("duplicate command field".into());
                    }
                    child(field.value)?;
                }
                fields.iter().map(|field| field.name.len() + 4).sum()
            }
            _ => 8,
        };
        depths.push(depth);
        if bytes > BYTE_LIMIT {
            return Err("command value byte limit exceeded".into());
        }
    }
    if used[..used.len() - 1].iter().any(|used| !used) {
        return Err("unreferenced command value node".into());
    }
    Ok(bytes)
}

impl super::component::ComponentHostState {
    pub(crate) fn queue_system(
        &mut self,
        request: SystemRequest,
        bytes: usize,
    ) -> Result<u64, String> {
        let token = self.editor_requests.token()?;
        self.editor_requests
            .push(EditorRequest::System(token, request), bytes)?;
        Ok(token)
    }

    pub(crate) fn require_editor_capability(
        &self,
        capability: PluginCapability,
    ) -> Result<(), String> {
        if !self.execution_available || self.confirmation_result_only {
            return Err("editor requests are unavailable in this callback".into());
        }
        if !self.capabilities.contains(&capability) {
            return Err("capability denied".into());
        }
        Ok(())
    }
}
