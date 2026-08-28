use super::*;

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
        manifest: PluginManifest,
        limits: PluginLimits,
    },
    Execute {
        load: PluginLoadId,
        invocation: PluginExecutionRequest,
    },
    Disable {
        load: PluginLoadId,
        plugin: String,
    },
}
#[derive(Debug)]
pub(super) enum WorkerEvent {
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
pub(super) struct RuntimeSlot {
    load: PluginLoadId,
    runtime: ActiveRuntime,
}
pub(super) struct IndexedCandidate {
    store: PathBuf,
    component_path: PathBuf,
    manifest: PluginManifest,
    limits: PluginLimits,
}
pub struct PluginManager {
    pub(super) sender: SyncSender<Request>,
    pub(super) receiver: Receiver<WorkerEvent>,
    pub statuses: BTreeMap<String, PluginStatus>,
    pub(super) loads: BTreeMap<String, PluginLoadId>,
    pub(super) next_load: u64,
    pub(super) pending_disables: VecDeque<(String, PluginLoadId)>,
    pub(super) maximum_loads: usize,
    pub(super) stopped: bool,
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
                let mut indexed: BTreeMap<(String, PluginLoadId), IndexedCandidate> =
                    BTreeMap::new();
                while let Ok(request) = rx.recv() {
                    let event = match request {
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
                        } => {
                            let plugin = manifest.id.clone();
                            let prepared = (|| {
                                let store = fs::canonicalize(store).map_err(PluginError::Io)?;
                                if path.is_absolute() && !path.starts_with(&store) {
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
                                            manifest,
                                            limits,
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
                        Request::Execute { load, invocation } => {
                            let plugin = invocation.plugin.clone();
                            match runtimes.get_mut(&plugin).filter(|slot| slot.load == load) {
                                Some(slot) => execute_component(invocation, &mut slot.runtime)
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
    pub fn execute(&mut self, invocation: PluginExecutionRequest) -> Result<(), PluginError> {
        if invocation.arguments.len() > 256
            || invocation
                .arguments
                .iter()
                .any(|argument| argument.len() > 65536)
            || invocation.config.len() > 1024
        {
            return Err(PluginError::InvalidInvocation);
        }
        self.require_disable_order()?;
        let load = self
            .loads
            .get(&invocation.plugin)
            .copied()
            .ok_or(PluginError::NotActive)?;
        self.send_request(Request::Execute { load, invocation })
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
                            events.push(PluginEvent::Indexed {
                                load,
                                plugin,
                                index,
                            });
                        }
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
    #[must_use]
    pub fn is_active_generation(&self, id: &str, load: PluginLoadId) -> bool {
        self.loads.get(id) == Some(&load)
            && matches!(self.statuses.get(id), Some(PluginStatus::Active(_)))
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
