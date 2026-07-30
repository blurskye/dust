#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread,
    time::Duration,
};
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginCapability {
    ReadCurrentBuffer,
    ProposeEdits,
    ShowMessage,
    ReadConfig,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub component_sha256: String,
    pub capabilities: BTreeSet<PluginCapability>,
    pub commands: BTreeMap<String, String>,
}
#[derive(Clone, Debug)]
pub struct PluginLimits {
    pub maximum_component_bytes: usize,
    pub maximum_commands: usize,
    pub fuel: u64,
    pub epoch_deadline_ticks: u64,
    pub maximum_memory_bytes: usize,
    pub maximum_table_elements: u32,
    pub maximum_instances: u32,
}
impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            maximum_component_bytes: 64 * 1024 * 1024,
            maximum_commands: 256,
            fuel: 10_000_000,
            epoch_deadline_ticks: 10,
            maximum_memory_bytes: 64 * 1024 * 1024,
            maximum_table_elements: 10_000,
            maximum_instances: 16,
        }
    }
}
#[derive(Clone, Debug)]
pub enum PluginStatus {
    Loading,
    ValidatedUntilHost(String),
    Instantiated,
    Active(Vec<String>),
    Disabled,
    Failed(String),
}
#[derive(Clone, Debug)]
pub struct LoadedComponent {
    pub canonical_path: PathBuf,
    pub bytes: Vec<u8>,
    pub manifest: PluginManifest,
    pub instantiated: bool,
    pub unavailable_host: Option<String>,
    pub activated_commands: Vec<String>,
}
struct ActiveRuntime {
    store: wasmtime::Store<ComponentHostState>,
    bindings: component_api::EditorPlugin,
    commands: BTreeSet<String>,
    limits: PluginLimits,
}
struct LoadOutcome {
    loaded: LoadedComponent,
    runtime: ActiveRuntime,
}
#[derive(Clone, Debug)]
pub struct PluginSnapshot {
    pub buffer: u64,
    pub revision: u64,
    pub utf8: String,
}
#[derive(Clone, Debug)]
pub struct PluginEditProposal {
    pub buffer: u64,
    pub revision: u64,
    pub edits: Vec<PluginEdit>,
}
#[derive(Clone, Debug)]
pub struct PluginEdit {
    pub start: u64,
    pub end: u64,
    pub replacement: String,
}
#[derive(Clone, Debug)]
pub struct PluginExecution {
    pub plugin: String,
    pub command: String,
    pub generation: u64,
    pub result: String,
    pub messages: Vec<String>,
    pub proposals: Vec<PluginEditProposal>,
}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PluginLoadId(pub u64);
#[derive(Debug)]
enum Request {
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
        manifest: PluginManifest,
        limits: PluginLimits,
    },
    Execute {
        load: PluginLoadId,
        plugin: String,
        command: String,
        arguments: Vec<String>,
        snapshot: Option<PluginSnapshot>,
        config: BTreeMap<String, String>,
        generation: u64,
    },
    Disable {
        load: PluginLoadId,
        plugin: String,
    },
}
#[derive(Debug)]
enum WorkerEvent {
    Loaded {
        load: PluginLoadId,
        component: LoadedComponent,
    },
    Executed {
        load: PluginLoadId,
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
struct RuntimeSlot {
    load: PluginLoadId,
    runtime: ActiveRuntime,
}
pub struct PluginManager {
    sender: SyncSender<Request>,
    receiver: Receiver<WorkerEvent>,
    pub statuses: BTreeMap<String, PluginStatus>,
    loads: BTreeMap<String, PluginLoadId>,
    next_load: u64,
    pending_disables: VecDeque<(String, PluginLoadId)>,
    maximum_loads: usize,
    stopped: bool,
}
impl PluginManager {
    pub fn start(capacity: usize) -> Self {
        let capacity = capacity.clamp(1, 64);
        let (tx, rx) = mpsc::sync_channel::<Request>(capacity);
        let (etx, erx) = mpsc::sync_channel(capacity);
        thread::Builder::new()
            .name("dust-components".into())
            .spawn(move || {
                let mut runtimes: BTreeMap<String, RuntimeSlot> = BTreeMap::new();
                while let Ok(request) = rx.recv() {
                    let event = match request {
                        Request::Load {
                            load,
                            store,
                            path,
                            manifest,
                            limits,
                        } => {
                            let plugin = manifest.id.clone();
                            match load_component(&store, &path, manifest, &limits) {
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
                            plugin,
                            command,
                            arguments,
                            snapshot,
                            config,
                            generation,
                        } => match runtimes.get_mut(&plugin).filter(|slot| slot.load == load) {
                            Some(slot) => execute_component(
                                &plugin,
                                &command,
                                arguments,
                                snapshot,
                                config,
                                generation,
                                &mut slot.runtime,
                            )
                            .map_or_else(
                                |error| {
                                    runtimes.remove(&plugin);
                                    WorkerEvent::Failed {
                                        load,
                                        plugin,
                                        error,
                                    }
                                },
                                |execution| WorkerEvent::Executed { load, execution },
                            ),
                            None => WorkerEvent::Failed {
                                load,
                                plugin,
                                error: PluginError::NotActive,
                            },
                        },
                        Request::Disable { load, plugin } => {
                            if runtimes.get(&plugin).is_some_and(|slot| slot.load == load) {
                                runtimes.remove(&plugin);
                            }
                            continue;
                        }
                    };
                    if etx.send(event).is_err() {
                        break;
                    }
                }
            })
            .expect("plugin loader");
        Self {
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
            match self.sender.try_send(Request::Disable { load, plugin }) {
                Ok(()) => {}
                Err(TrySendError::Full(Request::Disable { load, plugin })) => {
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
    fn send_request(&mut self, request: Request) -> Result<(), PluginError> {
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
            manifest,
            limits,
        })?;
        self.loads.insert(plugin.clone(), load);
        self.statuses.insert(plugin, PluginStatus::Loading);
        Ok(load)
    }
    pub fn execute(
        &mut self,
        plugin: &str,
        command: &str,
        arguments: Vec<String>,
        snapshot: Option<PluginSnapshot>,
        config: BTreeMap<String, String>,
        generation: u64,
    ) -> Result<(), PluginError> {
        if arguments.len() > 256
            || arguments.iter().any(|argument| argument.len() > 65536)
            || config.len() > 1024
        {
            return Err(PluginError::InvalidInvocation);
        }
        self.require_disable_order()?;
        let load = self
            .loads
            .get(plugin)
            .copied()
            .ok_or(PluginError::NotActive)?;
        self.send_request(Request::Execute {
            load,
            plugin: plugin.into(),
            command: command.into(),
            arguments,
            snapshot,
            config,
            generation,
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
                        WorkerEvent::Loaded { load, component } => {
                            let plugin = component.manifest.id.clone();
                            if self.loads.get(&plugin) != Some(&load) {
                                self.queue_disable(plugin, load);
                                self.flush_pending_disables();
                                continue;
                            }
                            self.statuses.insert(plugin,if !component.activated_commands.is_empty(){PluginStatus::Active(component.activated_commands.clone())}else if component.instantiated{PluginStatus::Instantiated}else{PluginStatus::ValidatedUntilHost(component.unavailable_host.clone().unwrap_or_else(||"component imports require unavailable host bindings".into()))});
                            events.push(PluginEvent::Loaded { load, component })
                        }
                        WorkerEvent::Executed { load, execution } => {
                            if self.loads.get(&execution.plugin) == Some(&load) {
                                events.push(PluginEvent::Executed(execution));
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
                            events.push(PluginEvent::Failed {
                                load,
                                plugin,
                                error,
                            });
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
    pub fn unload_generation(&mut self, id: &str, load: PluginLoadId) -> Result<(), PluginError> {
        if self.loads.get(id) != Some(&load) {
            return Ok(());
        }
        self.loads.remove(id);
        self.statuses.remove(id);
        self.queue_disable(id.into(), load);
        self.flush_pending_disables();
        Ok(())
    }
    pub fn disable_generation(&mut self, id: &str, load: PluginLoadId) -> Result<(), PluginError> {
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
fn load_manifest_component(
    store: &Path,
    manifest_relative: &Path,
    component_relative: &Path,
    limits: &PluginLimits,
) -> Result<LoadOutcome, (String, PluginError)> {
    let fallback = manifest_relative
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown")
        .chars()
        .take(256)
        .collect::<String>();
    let result = (|| {
        if manifest_relative.is_absolute() || component_relative.is_absolute() {
            return Err(PluginError::OutsideStore);
        }
        let store = fs::canonicalize(store).map_err(PluginError::Io)?;
        let manifest_path =
            fs::canonicalize(store.join(manifest_relative)).map_err(PluginError::Io)?;
        if !manifest_path.starts_with(&store) {
            return Err(PluginError::OutsideStore);
        }
        let metadata = fs::metadata(&manifest_path).map_err(PluginError::Io)?;
        if metadata.len() > 1024 * 1024 {
            return Err(PluginError::InvalidManifest);
        }
        let source = fs::read_to_string(manifest_path).map_err(PluginError::Io)?;
        let manifest: PluginManifest =
            toml::from_str(&source).map_err(|_| PluginError::InvalidManifest)?;
        validate_manifest(&manifest, limits)?;
        load_component(&store, &store.join(component_relative), manifest, limits)
    })();
    result.map_err(|error| (fallback, error))
}

fn validate_manifest(manifest: &PluginManifest, limits: &PluginLimits) -> Result<(), PluginError> {
    if manifest.id.is_empty()
        || manifest.id.len() > 256
        || manifest.version.is_empty()
        || manifest.component_sha256.len() != 64
        || manifest.commands.len() > limits.maximum_commands
        || manifest
            .commands
            .keys()
            .any(|command| command.is_empty() || command.len() > 256)
    {
        return Err(PluginError::InvalidManifest);
    }
    Ok(())
}
pub fn activate_candidate(
    store: &Path,
    path: &Path,
    manifest: PluginManifest,
    limits: &PluginLimits,
) -> Result<Vec<String>, PluginError> {
    let outcome = load_component(store, path, manifest, limits)?;
    Ok(outcome.loaded.activated_commands)
}

fn load_component(
    store: &Path,
    path: &Path,
    manifest: PluginManifest,
    limits: &PluginLimits,
) -> Result<LoadOutcome, PluginError> {
    let store = fs::canonicalize(store).map_err(PluginError::Io)?;
    let path = fs::canonicalize(path).map_err(PluginError::Io)?;
    if !path.starts_with(&store) {
        return Err(PluginError::OutsideStore);
    }
    let metadata = fs::symlink_metadata(&path).map_err(PluginError::Io)?;
    if !metadata.file_type().is_file() || metadata.len() > limits.maximum_component_bytes as u64 {
        return Err(PluginError::InvalidComponent);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    fs::File::open(&path)
        .map_err(PluginError::Io)?
        .take(limits.maximum_component_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(PluginError::Io)?;
    if bytes.len() > limits.maximum_component_bytes || !bytes.starts_with(b"\0asm") {
        return Err(PluginError::InvalidComponent);
    }
    let digest = format!("{:x}", Sha256::digest(&bytes));
    if digest != manifest.component_sha256.to_ascii_lowercase() {
        return Err(PluginError::ChecksumMismatch);
    }
    let mut config = wasmtime::Config::new();
    config
        .wasm_component_model(true)
        .consume_fuel(true)
        .epoch_interruption(true);
    let engine = wasmtime::Engine::new(&config)
        .map_err(|error| PluginError::Validation(error.to_string()))?;
    let component = wasmtime::component::Component::from_binary(&engine, &bytes)
        .map_err(|error| PluginError::Validation(error.to_string()))?;
    let store_limits = wasmtime::StoreLimitsBuilder::new()
        .memory_size(limits.maximum_memory_bytes)
        .table_elements(limits.maximum_table_elements as usize)
        .instances(limits.maximum_instances as usize)
        .build();
    let host = ComponentHostState {
        limits: store_limits,
        capabilities: manifest.capabilities.clone(),
        snapshot: None,
        proposals: Vec::new(),
        messages: Vec::new(),
        config: BTreeMap::new(),
    };
    let mut store = wasmtime::Store::new(&engine, host);
    store.limiter(|state| &mut state.limits);
    store
        .set_fuel(limits.fuel)
        .map_err(|error| PluginError::Validation(error.to_string()))?;
    store.set_epoch_deadline(limits.epoch_deadline_ticks);
    let mut linker = wasmtime::component::Linker::new(&engine);
    let activation = (|| {
        component_api::EditorPlugin::add_to_linker(&mut linker, |state| state)
            .map_err(|error| error.to_string())?;
        let bindings = component_api::EditorPlugin::instantiate(&mut store, &component, &linker)
            .map_err(|error| error.to_string())?;
        let commands = bindings
            .call_activate(&mut store)
            .map_err(|error| error.to_string())?
            .map_err(|error| error)?;
        Ok::<_, String>((bindings, commands))
    })();
    let (bindings, activated) = activation.map_err(PluginError::Activation)?;
    let activated_commands = activated
        .into_iter()
        .take(limits.maximum_commands)
        .map(|command| command.chars().take(256).collect::<String>())
        .collect::<Vec<_>>();
    if activated_commands
        .iter()
        .any(|command| !manifest.commands.contains_key(command))
    {
        return Err(PluginError::UndeclaredCommand);
    }
    let commands = activated_commands.iter().cloned().collect();
    let loaded = LoadedComponent {
        canonical_path: path,
        bytes,
        manifest,
        instantiated: true,
        unavailable_host: None,
        activated_commands,
    };
    Ok(LoadOutcome {
        loaded,
        runtime: ActiveRuntime {
            store,
            bindings,
            commands,
            limits: limits.clone(),
        },
    })
}
fn execute_component(
    plugin: &str,
    command: &str,
    arguments: Vec<String>,
    snapshot: Option<PluginSnapshot>,
    config: BTreeMap<String, String>,
    generation: u64,
    runtime: &mut ActiveRuntime,
) -> Result<PluginExecution, PluginError> {
    if !runtime.commands.contains(command) {
        return Err(PluginError::UndeclaredCommand);
    }
    runtime
        .store
        .set_fuel(runtime.limits.fuel)
        .map_err(|error| PluginError::Execution(error.to_string()))?;
    runtime.store.set_epoch_deadline(1);
    let epoch_engine = runtime.store.engine().clone();
    let deadline = runtime
        .limits
        .epoch_deadline_ticks
        .max(1)
        .saturating_mul(10);
    let _ = thread::Builder::new()
        .name("dust-plugin-deadline".into())
        .spawn(move || {
            thread::sleep(Duration::from_millis(deadline));
            epoch_engine.increment_epoch();
        });
    {
        let state = runtime.store.data_mut();
        state.snapshot = snapshot.map(|snapshot| component_api::dust::plugin::host::Snapshot {
            buffer: snapshot.buffer,
            revision: snapshot.revision,
            utf8: snapshot.utf8,
        });
        state.config = config;
        state.proposals.clear();
        state.messages.clear();
    }
    let result = runtime
        .bindings
        .call_execute_command(&mut runtime.store, command, &arguments)
        .map_err(|error| PluginError::Execution(error.to_string()))?
        .map_err(PluginError::Execution)?;
    if result.len() > 1024 * 1024 {
        return Err(PluginError::InvalidInvocation);
    }
    let state = runtime.store.data_mut();
    let messages = std::mem::take(&mut state.messages);
    let proposals = std::mem::take(&mut state.proposals)
        .into_iter()
        .map(|(buffer, revision, edits)| PluginEditProposal {
            buffer,
            revision,
            edits: edits
                .into_iter()
                .map(|edit| PluginEdit {
                    start: edit.start,
                    end: edit.end,
                    replacement: edit.replacement,
                })
                .collect(),
        })
        .collect();
    Ok(PluginExecution {
        plugin: plugin.into(),
        command: command.into(),
        generation,
        result,
        messages,
        proposals,
    })
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("invalid plugin manifest")]
    InvalidManifest,
    #[error("invalid or oversized WebAssembly component")]
    InvalidComponent,
    #[error("component is outside immutable package store")]
    OutsideStore,
    #[error("component checksum does not match its manifest")]
    ChecksumMismatch,
    #[error("component validation failed: {0}")]
    Validation(String),
    #[error("plugin activation failed: {0}")]
    Activation(String),
    #[error("plugin execution failed: {0}")]
    Execution(String),
    #[error("plugin command was not declared")]
    UndeclaredCommand,
    #[error("plugin invocation exceeded a bound")]
    InvalidInvocation,
    #[error("plugin already has a current load")]
    AlreadyActive,
    #[error("plugin active-load ceiling reached")]
    LoadLimit,
    #[error("plugin is not active")]
    NotActive,
    #[error("plugin loader queue is full")]
    QueueFull,
    #[error("plugin loader stopped")]
    Stopped,
    #[error("plugin I/O failed: {0}")]
    Io(#[source] std::io::Error),
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    fn manager(sender: SyncSender<Request>, receiver: Receiver<WorkerEvent>) -> PluginManager {
        PluginManager {
            sender,
            receiver,
            statuses: BTreeMap::new(),
            loads: BTreeMap::new(),
            next_load: 1,
            pending_disables: VecDeque::new(),
            maximum_loads: 4,
            stopped: false,
        }
    }
    #[test]
    fn failed_enqueue_does_not_publish_loading() {
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        let (_etx, erx) = mpsc::sync_channel(1);
        let mut manager = manager(tx, erx);
        assert!(matches!(
            manager.load_from_store(
                "demo".into(),
                PathBuf::from("store"),
                PathBuf::from("manifest"),
                PathBuf::from("component"),
                PluginLimits::default()
            ),
            Err(PluginError::Stopped)
        ));
        assert!(!manager.statuses.contains_key("demo"));
        assert!(manager.load_id("demo").is_none());
        assert!(manager.stopped);
    }
    #[test]
    fn stale_failure_and_unload_cannot_replace_current_generation() {
        let (tx, _rx) = mpsc::sync_channel(4);
        let (etx, erx) = mpsc::sync_channel(4);
        let mut manager = manager(tx, erx);
        manager.loads.insert("demo".into(), PluginLoadId(2));
        manager
            .statuses
            .insert("demo".into(), PluginStatus::Loading);
        etx.send(WorkerEvent::Failed {
            load: PluginLoadId(1),
            plugin: "demo".into(),
            error: PluginError::NotActive,
        })
        .unwrap();
        assert!(manager.poll().unwrap().is_empty());
        assert_eq!(manager.load_id("demo"), Some(PluginLoadId(2)));
        manager.unload_generation("demo", PluginLoadId(1)).unwrap();
        assert_eq!(manager.load_id("demo"), Some(PluginLoadId(2)));
    }
    #[test]
    fn active_load_ceiling_bounds_disable_ownership() {
        let (tx, _rx) = mpsc::sync_channel(4);
        let (_etx, erx) = mpsc::sync_channel(1);
        let mut manager = manager(tx, erx);
        manager.maximum_loads = 1;
        manager.loads.insert("one".into(), PluginLoadId(1));
        assert!(matches!(
            manager.load_from_store(
                "two".into(),
                PathBuf::from("store"),
                PathBuf::from("manifest"),
                PathBuf::from("component"),
                PluginLimits::default()
            ),
            Err(PluginError::LoadLimit)
        ));
        manager.unload_generation("one", PluginLoadId(1)).unwrap();
        assert!(manager.pending_disables.len() <= manager.maximum_loads);
    }
    #[test]
    fn queue_full_cancellation_stays_pending_and_suppresses_events() {
        let (tx, rx) = mpsc::sync_channel(1);
        tx.try_send(Request::Disable {
            load: PluginLoadId(9),
            plugin: "occupied".into(),
        })
        .unwrap();
        let (etx, erx) = mpsc::sync_channel(2);
        let mut manager = manager(tx, erx);
        manager.loads.insert("demo".into(), PluginLoadId(1));
        manager
            .statuses
            .insert("demo".into(), PluginStatus::Loading);
        manager.unload_generation("demo", PluginLoadId(1)).unwrap();
        assert_eq!(manager.pending_disables.len(), 1);
        assert!(matches!(
            manager.load_from_store(
                "demo".into(),
                PathBuf::from("store"),
                PathBuf::from("manifest"),
                PathBuf::from("component"),
                PluginLimits::default()
            ),
            Err(PluginError::QueueFull)
        ));
        etx.send(WorkerEvent::Failed {
            load: PluginLoadId(1),
            plugin: "demo".into(),
            error: PluginError::NotActive,
        })
        .unwrap();
        assert!(manager.poll().unwrap().is_empty());
        assert_eq!(manager.pending_disables.len(), 1);
        let _ = rx.recv().unwrap();
        assert!(manager.poll().unwrap().is_empty());
        assert!(manager.pending_disables.is_empty());
        assert!(
            matches!(rx.recv().unwrap(),Request::Disable{load:PluginLoadId(1),plugin}if plugin=="demo")
        );
    }
    #[test]
    fn disable_is_ordered_before_same_id_replacement_and_duplicates_are_rejected() {
        let (tx, rx) = mpsc::sync_channel(4);
        let (_etx, erx) = mpsc::sync_channel(1);
        let mut manager = manager(tx, erx);
        manager.loads.insert("demo".into(), PluginLoadId(1));
        manager.next_load = 2;
        assert!(matches!(
            manager.load_from_store(
                "demo".into(),
                PathBuf::from("store"),
                PathBuf::from("manifest"),
                PathBuf::from("component"),
                PluginLimits::default()
            ),
            Err(PluginError::AlreadyActive)
        ));
        manager.unload_generation("demo", PluginLoadId(1)).unwrap();
        let replacement = manager
            .load_from_store(
                "demo".into(),
                PathBuf::from("store"),
                PathBuf::from("manifest"),
                PathBuf::from("component"),
                PluginLimits::default(),
            )
            .unwrap();
        assert_ne!(replacement, PluginLoadId(1));
        assert!(matches!(
            rx.recv().unwrap(),
            Request::Disable {
                load: PluginLoadId(1),
                ..
            }
        ));
        assert!(matches!(rx.recv().unwrap(),Request::LoadFromStore{load,..}if load==replacement));
    }
}

pub mod component_api {
    wasmtime::component::bindgen!({ path: "wit", world: "editor-plugin" });
}

pub struct ComponentHostState {
    pub limits: wasmtime::StoreLimits,
    pub capabilities: BTreeSet<PluginCapability>,
    pub snapshot: Option<component_api::dust::plugin::host::Snapshot>,
    pub proposals: Vec<(u64, u64, Vec<component_api::dust::plugin::host::Edit>)>,
    pub messages: Vec<String>,
    pub config: BTreeMap<String, String>,
}
impl component_api::dust::plugin::host::Host for ComponentHostState {
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
        if !self.capabilities.contains(&PluginCapability::ProposeEdits) {
            return Err("capability denied".into());
        }
        if edits.len() > 4096 {
            return Err("edit ceiling exceeded".into());
        }
        if edits
            .iter()
            .any(|edit| edit.start > edit.end || edit.replacement.len() > 1024 * 1024)
        {
            return Err("invalid edit proposal".into());
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
}
