//! Plugin loading, execution and runtime ownership.

use crate::plugins::execution::ActiveRuntime;
use crate::plugins::execution::LoadedComponent;
use crate::plugins::execution::PluginExecution;
use crate::plugins::execution::PluginExecutionRequest;
use crate::plugins::execution::PluginLoadId;
use crate::plugins::execution::PluginStatus;
use crate::plugins::index::PluginError;
use crate::plugins::index::execute_component;
use crate::plugins::index::index_manifest_contributions;
use crate::plugins::index::index_manifest_from_store;
use crate::plugins::index::load_component;
use crate::plugins::index::load_manifest_component;
use crate::plugins::index::validate_manifest;
use crate::plugins::manifest::PluginContributionIndex;
use crate::plugins::manifest::PluginLimits;
use crate::plugins::manifest::PluginManifest;
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TryRecvError;
use std::sync::mpsc::TrySendError;
use std::thread;

#[derive(Debug)]
struct QueueSlot(Arc<AtomicUsize>);
impl Drop for QueueSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Debug)]
pub(super) struct QueuedRequest {
    request: Request,
    slot: QueueSlot,
}
impl QueuedRequest {
    pub(super) fn into_request(self) -> Request {
        let Self { request, slot } = self;
        drop(slot);
        request
    }
}
#[derive(Debug)]
pub(super) enum Request {
    IndexFromStore {
        load: PluginLoadId,
        expected_plugin: String,
        store: PathBuf,
        manifest_path: PathBuf,
        component_path: PathBuf,
        limits: PluginLimits,
    },
    ActivateIndexed {
        load: PluginLoadId,
        plugin: String,
        checksum: String,
    },
    LoadFromStore {
        load: PluginLoadId,
        expected_plugin: String,
        store: PathBuf,
        manifest_path: PathBuf,
        component_path: PathBuf,
        limits: PluginLimits,
    },
    Load {
        load: PluginLoadId,
        store: PathBuf,
        path: PathBuf,
        manifest: Box<PluginManifest>,
        limits: PluginLimits,
        object_lock: Option<Arc<fs::File>>,
    },
    Execute {
        load: PluginLoadId,
        origin: Option<PluginInvocationOrigin>,
        invocation: Box<PluginExecutionRequest>,
    },
    Disable {
        load: PluginLoadId,
        plugin: String,
    },
}
#[derive(Debug)]
pub(super) enum WorkerEvent {
    CommandFailed {
        load: PluginLoadId,
        plugin: String,
        error: PluginError,
    },
    Indexed {
        load: PluginLoadId,
        plugin: String,
        index: PluginContributionIndex,
    },
    Loaded {
        load: PluginLoadId,
        component: LoadedComponent,
    },
    Executed {
        load: PluginLoadId,
        origin: Option<PluginInvocationOrigin>,
        execution: PluginExecution,
    },
    Failed {
        load: PluginLoadId,
        plugin: String,
        error: PluginError,
    },
}
#[derive(Debug)]
pub enum PluginEvent {
    CommandFailed {
        load: PluginLoadId,
        plugin: String,
        error: PluginError,
    },
    Indexed {
        load: PluginLoadId,
        plugin: String,
        index: PluginContributionIndex,
    },
    Loaded {
        load: PluginLoadId,
        component: LoadedComponent,
    },
    Executed(PluginExecution),
    Failed {
        load: PluginLoadId,
        plugin: String,
        error: PluginError,
    },
}
// Private editor collaboration. Public events intentionally remain compatible.
#[derive(Clone, Debug)]
pub(crate) struct PluginInvocationOrigin {
    manager: std::sync::Arc<()>,
    invocation: std::sync::Arc<()>,
    plugin: String,
    command: String,
    load: PluginLoadId,
}
impl PluginInvocationOrigin {
    pub(crate) fn same(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.invocation, &other.invocation)
    }
    pub(crate) fn current(&self, manager: &PluginHost) -> bool {
        std::sync::Arc::ptr_eq(&self.manager, &manager.owner)
            && manager.is_active_load(&self.plugin, self.load)
    }
    pub(crate) fn load(&self) -> PluginLoadId {
        self.load
    }
    pub(crate) fn plugin(&self) -> &str {
        &self.plugin
    }
    pub(crate) fn command(&self) -> &str {
        &self.command
    }
}
pub(crate) struct PluginExecutionReply {
    execution: PluginExecution,
    origin: Option<PluginInvocationOrigin>,
}
impl PluginExecutionReply {
    pub(crate) fn origin(&self) -> Option<&PluginInvocationOrigin> {
        self.origin.as_ref()
    }
    pub(crate) fn into_parts(self) -> (PluginExecution, Option<PluginInvocationOrigin>) {
        (self.execution, self.origin)
    }
}
pub(crate) enum OwnedPluginEvent {
    Other(PluginEvent),
    Executed(PluginExecutionReply),
}
pub(crate) struct PluginInvocation<'a> {
    manager: &'a mut PluginHost,
    origin: PluginInvocationOrigin,
    slot: QueueSlot,
}
impl PluginInvocation<'_> {
    pub(crate) fn origin(&self) -> &PluginInvocationOrigin {
        &self.origin
    }
    pub(crate) fn check(&self, invocation: &PluginExecutionRequest) -> Result<(), PluginError> {
        if invocation.plugin != self.origin.plugin || invocation.command != self.origin.command {
            return Err(PluginError::InvalidInvocation);
        }
        PluginHost::validate_invocation(invocation)
    }
    pub(crate) fn submit(self, invocation: PluginExecutionRequest) -> Result<(), PluginError> {
        self.check(&invocation)?;
        self.manager.send_reserved(QueuedRequest {
            request: Request::Execute {
                load: self.origin.load,
                origin: Some(self.origin),
                invocation: Box::new(invocation),
            },
            slot: self.slot,
        })
    }
}
pub(super) struct RuntimeSlot {
    load: PluginLoadId,
    runtime: ActiveRuntime,
}
pub(super) struct IndexedCandidate {
    store: PathBuf,
    component_path: PathBuf,
    manifest: PluginManifest,
    limits: PluginLimits,
    _object_lock: Option<Arc<fs::File>>,
}
pub struct PluginHost {
    pub(super) owner: std::sync::Arc<()>,
    pub(super) queued: Arc<AtomicUsize>,
    pub(super) sender: SyncSender<QueuedRequest>,
    pub(super) receiver: Receiver<WorkerEvent>,
    pub statuses: BTreeMap<String, PluginStatus>,
    pub(super) loads: BTreeMap<String, PluginLoadId>,
    pub(super) next_load: u64,
    pub(super) pending_disables: VecDeque<(String, PluginLoadId)>,
    pub(super) maximum_loads: usize,
    pub(super) stopped: bool,
}
impl PluginHost {
    #[cfg(test)]
    pub(crate) fn fixture_queued(&self) -> usize {
        self.queued.load(Ordering::Acquire)
    }

    pub fn start(capacity: usize) -> Self {
        let capacity = capacity.clamp(1, 64);
        let (tx, rx) = mpsc::sync_channel::<QueuedRequest>(capacity);
        let (etx, erx) = mpsc::sync_channel(capacity);
        thread::Builder::new()
            .name("dust-components".into())
            .spawn(move || {
                let mut runtimes: BTreeMap<String, RuntimeSlot> = BTreeMap::new();
                let mut indexed: BTreeMap<(String, PluginLoadId), IndexedCandidate> =
                    BTreeMap::new();
                while let Ok(request) = rx.recv() {
                    let event = match request.into_request() {
                        Request::IndexFromStore {
                            load,
                            expected_plugin,
                            store,
                            manifest_path,
                            component_path,
                            limits,
                        } => match index_manifest_from_store(
                            &store,
                            &manifest_path,
                            &component_path,
                            load,
                            &limits,
                        ) {
                            Ok((manifest, index, canonical_store, component_path))
                                if manifest.id == expected_plugin =>
                            {
                                indexed.insert(
                                    (expected_plugin.clone(), load),
                                    IndexedCandidate {
                                        store: canonical_store,
                                        component_path,
                                        manifest,
                                        limits,
                                        _object_lock: None,
                                    },
                                );
                                WorkerEvent::Indexed {
                                    load,
                                    plugin: expected_plugin,
                                    index,
                                }
                            }
                            Ok(_) => WorkerEvent::Failed {
                                load,
                                plugin: expected_plugin,
                                error: PluginError::InvalidManifest,
                            },
                            Err(error) => WorkerEvent::Failed {
                                load,
                                plugin: expected_plugin,
                                error,
                            },
                        },
                        Request::ActivateIndexed {
                            load,
                            plugin,
                            checksum,
                        } => match indexed.remove(&(plugin.clone(), load)) {
                            Some(candidate)
                                if candidate
                                    .manifest
                                    .component_sha256
                                    .eq_ignore_ascii_case(&checksum) =>
                            {
                                match load_component(
                                    &candidate.store,
                                    &candidate.store.join(&candidate.component_path),
                                    candidate.manifest,
                                    &candidate.limits,
                                ) {
                                    Ok(outcome) => {
                                        runtimes.insert(
                                            plugin,
                                            RuntimeSlot {
                                                load,
                                                runtime: outcome.runtime,
                                            },
                                        );
                                        WorkerEvent::Loaded {
                                            load,
                                            component: outcome.loaded,
                                        }
                                    }
                                    Err(error) => WorkerEvent::Failed {
                                        load,
                                        plugin,
                                        error,
                                    },
                                }
                            }
                            Some(_) => WorkerEvent::Failed {
                                load,
                                plugin,
                                error: PluginError::ChecksumMismatch,
                            },
                            None => WorkerEvent::Failed {
                                load,
                                plugin,
                                error: PluginError::IndexNotAccepted,
                            },
                        },
                        Request::Load {
                            load,
                            store,
                            path,
                            manifest,
                            limits,
                            object_lock,
                        } => {
                            let plugin = manifest.id.clone();
                            let prepared = (|| {
                                let store =
                                    crate::files::canonicalize(store).map_err(PluginError::Io)?;
                                if path.is_absolute()
                                    && !crate::files::canonicalize(&path)
                                        .map_err(PluginError::Io)?
                                        .starts_with(&store)
                                {
                                    return Err(PluginError::OutsideStore);
                                }
                                let index = index_manifest_contributions(
                                    &manifest,
                                    load.0,
                                    format!("<prepared-plugin:{plugin}>"),
                                )?;
                                Ok::<_, PluginError>((store, index))
                            })();
                            match prepared {
                                Ok((store, index)) => {
                                    indexed.insert(
                                        (plugin.clone(), load),
                                        IndexedCandidate {
                                            store,
                                            component_path: path,
                                            manifest: *manifest,
                                            limits,
                                            _object_lock: object_lock,
                                        },
                                    );
                                    WorkerEvent::Indexed {
                                        load,
                                        plugin,
                                        index,
                                    }
                                }
                                Err(error) => WorkerEvent::Failed {
                                    load,
                                    plugin,
                                    error,
                                },
                            }
                        }
                        Request::LoadFromStore {
                            load,
                            expected_plugin,
                            store,
                            manifest_path,
                            component_path,
                            limits,
                        } => match load_manifest_component(
                            &store,
                            &manifest_path,
                            &component_path,
                            &limits,
                        ) {
                            Ok(outcome) if outcome.loaded.manifest.id == expected_plugin => {
                                runtimes.insert(
                                    expected_plugin,
                                    RuntimeSlot {
                                        load,
                                        runtime: outcome.runtime,
                                    },
                                );
                                WorkerEvent::Loaded {
                                    load,
                                    component: outcome.loaded,
                                }
                            }
                            Ok(_) => WorkerEvent::Failed {
                                load,
                                plugin: expected_plugin,
                                error: PluginError::InvalidManifest,
                            },
                            Err((_, error)) => WorkerEvent::Failed {
                                load,
                                plugin: expected_plugin,
                                error,
                            },
                        },
                        Request::Execute {
                            load,
                            origin,
                            invocation,
                        } => {
                            let plugin = invocation.plugin.clone();
                            match runtimes.get_mut(&plugin).filter(|slot| slot.load == load) {
                                Some(slot) => execute_component(*invocation, &mut slot.runtime)
                                    .map_or_else(
                                        |error| {
                                            if matches!(error, PluginError::Execution(_)) {
                                                runtimes.remove(&plugin);
                                                WorkerEvent::Failed {
                                                    load,
                                                    plugin,
                                                    error,
                                                }
                                            } else {
                                                WorkerEvent::CommandFailed {
                                                    load,
                                                    plugin,
                                                    error,
                                                }
                                            }
                                        },
                                        |execution| WorkerEvent::Executed {
                                            load,
                                            origin,
                                            execution,
                                        },
                                    ),
                                None => WorkerEvent::Failed {
                                    load,
                                    plugin,
                                    error: PluginError::NotActive,
                                },
                            }
                        }

                        Request::Disable { load, plugin } => {
                            if runtimes.get(&plugin).is_some_and(|slot| slot.load == load) {
                                runtimes.remove(&plugin);
                            }
                            indexed.remove(&(plugin, load));
                            continue;
                        }
                    };
                    if let WorkerEvent::Failed { plugin, load, .. } = &event {
                        // Failure releases the load before foreground ownership
                        // is removed, including execution attempted before activation.
                        indexed.remove(&(plugin.clone(), *load));
                    }
                    if etx.send(event).is_err() {
                        break;
                    }
                }
            })
            .expect("plugin loader");
        Self {
            owner: std::sync::Arc::new(()),
            queued: Arc::new(AtomicUsize::new(0)),
            sender: tx,
            receiver: erx,
            statuses: BTreeMap::new(),
            loads: BTreeMap::new(),
            next_load: 1,
            pending_disables: VecDeque::new(),
            maximum_loads: capacity,
            stopped: false,
        }
    }
    fn queue_disable(&mut self, plugin: String, load: PluginLoadId) {
        if !self
            .pending_disables
            .iter()
            .any(|(candidate, candidate_load)| candidate == &plugin && *candidate_load == load)
        {
            self.pending_disables.push_back((plugin, load));
        }
    }
    fn flush_pending_disables(&mut self) -> bool {
        while let Some((plugin, load)) = self.pending_disables.pop_front() {
            let slot = match self.reserve_queue() {
                Ok(slot) => slot,
                Err(_) => {
                    self.pending_disables.push_front((plugin, load));
                    return false;
                }
            };
            match self.sender.try_send(QueuedRequest {
                request: Request::Disable { load, plugin },
                slot,
            }) {
                Ok(()) => {}
                Err(TrySendError::Full(QueuedRequest {
                    request: Request::Disable { load, plugin },
                    ..
                })) => {
                    self.pending_disables.push_front((plugin, load));
                    return false;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.pending_disables.clear();
                    self.stopped = true;
                    return true;
                }
                Err(TrySendError::Full(_)) => unreachable!("disable request changed variant"),
            }
        }
        true
    }
    fn require_disable_order(&mut self) -> Result<(), PluginError> {
        if self.stopped {
            self.fail_stopped_loads();
            return Err(PluginError::Stopped);
        }
        self.flush_pending_disables();
        if self.stopped {
            self.fail_stopped_loads();
            return Err(PluginError::Stopped);
        }
        if self.pending_disables.is_empty() {
            Ok(())
        } else {
            Err(PluginError::QueueFull)
        }
    }
    fn reserve_queue(&self) -> Result<QueueSlot, PluginError> {
        self.queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count
                    .checked_add(1)
                    .filter(|count| *count <= self.maximum_loads)
            })
            .map_err(|_| PluginError::QueueFull)?;
        Ok(QueueSlot(self.queued.clone()))
    }
    fn send_request(&mut self, request: Request) -> Result<(), PluginError> {
        let slot = self.reserve_queue()?;
        self.send_reserved(QueuedRequest { request, slot })
    }
    fn send_reserved(&mut self, request: QueuedRequest) -> Result<(), PluginError> {
        match self.sender.try_send(request) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(PluginError::QueueFull),
            Err(TrySendError::Disconnected(_)) => {
                self.stopped = true;
                self.fail_stopped_loads();
                Err(PluginError::Stopped)
            }
        }
    }
    fn allocate_load(&mut self) -> Result<PluginLoadId, PluginError> {
        let load = PluginLoadId(self.next_load);
        self.next_load = self
            .next_load
            .checked_add(1)
            .ok_or(PluginError::InvalidInvocation)?;
        Ok(load)
    }
    pub fn index_from_store(
        &mut self,
        expected_plugin: String,
        store: PathBuf,
        manifest_path: PathBuf,
        component_path: PathBuf,
        limits: PluginLimits,
    ) -> Result<PluginLoadId, PluginError> {
        self.require_disable_order()?;
        if self.loads.contains_key(&expected_plugin) {
            return Err(PluginError::AlreadyActive);
        }
        if self.loads.len() >= self.maximum_loads {
            return Err(PluginError::LoadLimit);
        }
        let load = self.allocate_load()?;
        self.send_request(Request::IndexFromStore {
            load,
            expected_plugin: expected_plugin.clone(),
            store,
            manifest_path,
            component_path,
            limits,
        })?;
        self.loads.insert(expected_plugin.clone(), load);
        self.statuses.insert(expected_plugin, PluginStatus::Loading);
        Ok(load)
    }

    pub fn activate_indexed(
        &mut self,
        plugin: &str,
        load: PluginLoadId,
        checksum: String,
    ) -> Result<(), PluginError> {
        if self.loads.get(plugin) != Some(&load)
            || !matches!(self.statuses.get(plugin), Some(PluginStatus::Indexed))
        {
            return Err(PluginError::IndexNotAccepted);
        }
        self.send_request(Request::ActivateIndexed {
            load,
            plugin: plugin.into(),
            checksum,
        })?;
        self.statuses.insert(plugin.into(), PluginStatus::Loading);
        Ok(())
    }

    pub fn load_from_store(
        &mut self,
        expected_plugin: String,
        store: PathBuf,
        manifest_path: PathBuf,
        component_path: PathBuf,
        limits: PluginLimits,
    ) -> Result<PluginLoadId, PluginError> {
        self.require_disable_order()?;
        if self.loads.contains_key(&expected_plugin) {
            return Err(PluginError::AlreadyActive);
        }
        if self.loads.len() >= self.maximum_loads {
            return Err(PluginError::LoadLimit);
        }
        let load = self.allocate_load()?;
        self.send_request(Request::LoadFromStore {
            load,
            expected_plugin: expected_plugin.clone(),
            store,
            manifest_path,
            component_path,
            limits,
        })?;
        self.loads.insert(expected_plugin.clone(), load);
        self.statuses.insert(expected_plugin, PluginStatus::Loading);
        Ok(load)
    }
    pub fn load(
        &mut self,
        store: PathBuf,
        path: PathBuf,
        manifest: PluginManifest,
        limits: PluginLimits,
    ) -> Result<PluginLoadId, PluginError> {
        self.load_protected(store, path, manifest, limits, None)
    }
    pub(crate) fn load_protected(
        &mut self,
        store: PathBuf,
        path: PathBuf,
        manifest: PluginManifest,
        limits: PluginLimits,
        object_lock: Option<Arc<fs::File>>,
    ) -> Result<PluginLoadId, PluginError> {
        validate_manifest(&manifest, &limits)?;
        let plugin = manifest.id.clone();
        self.require_disable_order()?;
        if self.loads.contains_key(&plugin) {
            return Err(PluginError::AlreadyActive);
        }
        if self.loads.len() >= self.maximum_loads {
            return Err(PluginError::LoadLimit);
        }
        let load = self.allocate_load()?;
        self.send_request(Request::Load {
            load,
            store,
            path,
            manifest: Box::new(manifest),
            limits,
            object_lock,
        })?;
        self.loads.insert(plugin.clone(), load);
        self.statuses.insert(plugin, PluginStatus::Loading);
        Ok(load)
    }
    pub(crate) fn validate_invocation(
        invocation: &PluginExecutionRequest,
    ) -> Result<(), PluginError> {
        crate::plugins::requests::value_bytes(&invocation.call.arguments)
            .map_err(|_| PluginError::InvalidInvocation)?;
        if let Some(value) = &invocation.call.previous {
            crate::plugins::requests::value_bytes(value)
                .map_err(|_| PluginError::InvalidInvocation)?;
        }
        if invocation.call.command.len() > 256 || invocation.call.count == Some(0) {
            return Err(PluginError::InvalidInvocation);
        }
        if invocation.arguments.len() > 256
            || invocation
                .arguments
                .iter()
                .any(|argument| argument.len() > 65536)
            || invocation.config.len() > 1024
        {
            return Err(PluginError::InvalidInvocation);
        }
        Ok(())
    }
    pub(crate) fn prepare_execution(
        &mut self,
        plugin: &str,
        command: &str,
    ) -> Result<PluginInvocation<'_>, PluginError> {
        self.require_disable_order()?;
        let load = self.load_id(plugin).ok_or(PluginError::NotActive)?;
        if !self.is_active_load(plugin, load) {
            return Err(PluginError::NotActive);
        }
        if !matches!(self.statuses.get(plugin), Some(PluginStatus::Active(commands)) if commands.iter().any(|c| c == command))
        {
            return Err(PluginError::UndeclaredCommand);
        }
        let slot = self.reserve_queue()?;
        let origin = PluginInvocationOrigin {
            manager: self.owner.clone(),
            invocation: std::sync::Arc::new(()),
            plugin: plugin.into(),
            command: command.into(),
            load,
        };
        Ok(PluginInvocation {
            manager: self,
            origin,
            slot,
        })
    }
    pub fn execute(&mut self, invocation: PluginExecutionRequest) -> Result<(), PluginError> {
        Self::validate_invocation(&invocation)?;
        self.require_disable_order()?;
        let load = self
            .loads
            .get(&invocation.plugin)
            .copied()
            .ok_or(PluginError::NotActive)?;
        self.send_request(Request::Execute {
            load,
            origin: None,
            invocation: Box::new(invocation),
        })
    }
    fn fail_stopped_loads(&mut self) {
        let ids = self.loads.keys().cloned().collect::<Vec<_>>();
        self.loads.clear();
        self.pending_disables.clear();
        for id in ids {
            self.statuses
                .insert(id, PluginStatus::Failed(PluginError::Stopped.to_string()));
        }
    }
    pub fn poll(&mut self) -> Result<Vec<PluginEvent>, PluginError> {
        self.poll_owned().map(|events| {
            events
                .into_iter()
                .map(|event| match event {
                    OwnedPluginEvent::Other(event) => event,
                    OwnedPluginEvent::Executed(reply) => PluginEvent::Executed(reply.execution),
                })
                .collect()
        })
    }
    pub(crate) fn poll_owned(&mut self) -> Result<Vec<OwnedPluginEvent>, PluginError> {
        if self.stopped {
            self.fail_stopped_loads();
            return Err(PluginError::Stopped);
        }
        self.flush_pending_disables();
        if self.stopped {
            self.fail_stopped_loads();
            return Err(PluginError::Stopped);
        }
        let mut events = Vec::new();
        loop {
            match self.receiver.try_recv() {
                Ok(event) => {
                    match event {
                        WorkerEvent::CommandFailed {
                            load,
                            plugin,
                            error,
                        } => {
                            if self.loads.get(&plugin) == Some(&load) {
                                events.push(OwnedPluginEvent::Other(PluginEvent::CommandFailed {
                                    load,
                                    plugin,
                                    error,
                                }));
                            }
                        }
                        WorkerEvent::Indexed {
                            load,
                            plugin,
                            index,
                        } => {
                            if self.loads.get(&plugin) != Some(&load) {
                                self.queue_disable(plugin, load);
                                self.flush_pending_disables();
                                continue;
                            }
                            self.statuses.insert(plugin.clone(), PluginStatus::Indexed);
                            events.push(OwnedPluginEvent::Other(PluginEvent::Indexed {
                                load,
                                plugin,
                                index,
                            }));
                        }
                        WorkerEvent::Loaded { load, component } => {
                            let plugin = component.manifest.id.clone();
                            if self.loads.get(&plugin) != Some(&load) {
                                self.queue_disable(plugin, load);
                                self.flush_pending_disables();
                                continue;
                            }
                            self.statuses.insert(plugin,if !component.activated_commands.is_empty(){PluginStatus::Active(component.activated_commands.clone())}else if component.instantiated{PluginStatus::Instantiated}else{PluginStatus::ValidatedUntilHost(component.unavailable_host.clone().unwrap_or_else(||"component imports require unavailable host bindings".into()))});
                            events.push(OwnedPluginEvent::Other(PluginEvent::Loaded {
                                load,
                                component,
                            }))
                        }
                        WorkerEvent::Executed {
                            load,
                            origin,
                            execution,
                        } => {
                            if self.loads.get(&execution.plugin) == Some(&load) {
                                events.push(OwnedPluginEvent::Executed(PluginExecutionReply {
                                    origin,
                                    execution,
                                }));
                            }
                        }
                        WorkerEvent::Failed {
                            load,
                            plugin,
                            error,
                        } => {
                            if self.loads.get(&plugin) != Some(&load) {
                                continue;
                            }
                            self.loads.remove(&plugin);
                            self.statuses
                                .insert(plugin.clone(), PluginStatus::Failed(error.to_string()));
                            events.push(OwnedPluginEvent::Other(PluginEvent::Failed {
                                load,
                                plugin,
                                error,
                            }));
                        }
                    }
                }
                Err(TryRecvError::Empty) => {
                    self.flush_pending_disables();
                    return Ok(events);
                }
                Err(TryRecvError::Disconnected) => {
                    self.stopped = true;
                    self.fail_stopped_loads();
                    return Err(PluginError::Stopped);
                }
            }
        }
    }
    pub fn load_id(&self, id: &str) -> Option<PluginLoadId> {
        self.loads.get(id).copied()
    }
    #[must_use]
    pub fn is_active_load(&self, id: &str, load: PluginLoadId) -> bool {
        self.loads.get(id) == Some(&load)
            && matches!(self.statuses.get(id), Some(PluginStatus::Active(_)))
    }
    pub fn unload_load(&mut self, id: &str, load: PluginLoadId) -> Result<(), PluginError> {
        if self.loads.get(id) != Some(&load) {
            return Ok(());
        }
        self.loads.remove(id);
        self.statuses.remove(id);
        self.queue_disable(id.into(), load);
        self.flush_pending_disables();
        Ok(())
    }
    pub fn disable_load(&mut self, id: &str, load: PluginLoadId) -> Result<(), PluginError> {
        if self.loads.get(id) != Some(&load) {
            return Ok(());
        }
        self.loads.remove(id);
        self.statuses.insert(id.into(), PluginStatus::Disabled);
        self.queue_disable(id.into(), load);
        self.flush_pending_disables();
        Ok(())
    }
    pub fn disable(&mut self, id: &str) -> Result<(), PluginError> {
        if let Some(load) = self.loads.remove(id) {
            self.queue_disable(id.into(), load);
            self.flush_pending_disables();
        }
        self.statuses.insert(id.into(), PluginStatus::Disabled);
        Ok(())
    }
    pub fn unload(&mut self, id: &str) -> Result<(), PluginError> {
        if let Some(load) = self.loads.remove(id) {
            self.queue_disable(id.into(), load);
            self.flush_pending_disables();
        }
        self.statuses.remove(id);
        Ok(())
    }
}
