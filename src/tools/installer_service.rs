use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolOperation {
    InstallManaged,
    InstallRustup,
    UninstallManaged,
    InstallNix,
    UninstallNix,
}

#[derive(Clone, Debug)]
pub struct ToolInstallResult {
    pub id: String,
    pub operation: ToolOperation,
    pub result: Result<Option<PathBuf>, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolInstallProgress {
    pub id: String,
    pub operation: ToolOperation,
    pub phase: String,
    pub completed: u64,
    pub total: Option<u64>,
}

pub(super) enum InstallerRequest {
    Install {
        definition: ToolDefinition,
    },
    Uninstall {
        definition: ToolDefinition,
    },
    InstallNix {
        definition: ToolDefinition,
        nix: PathBuf,
    },
    InstallRustup {
        definition: ToolDefinition,
        rustup: PathBuf,
        proxy: PathBuf,
        cwd: PathBuf,
        component: String,
    },
    UninstallNix {
        definition: ToolDefinition,
    },
    Shutdown,
}

pub struct ToolInstaller {
    sender: SyncSender<InstallerRequest>,
    receiver: Receiver<ToolInstallResult>,
    progress_receiver: Receiver<ToolInstallProgress>,
    cancel: Sender<()>,
    busy: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ToolInstaller {
    #[must_use]
    pub fn start() -> Self {
        Self::start_at(tool_store_root())
    }

    #[must_use]
    pub fn start_at(root: PathBuf) -> Self {
        let (sender, requests) = mpsc::sync_channel(1);
        let (results, receiver) = mpsc::channel();
        let (progress, progress_receiver) = mpsc::sync_channel(32);
        let (cancel, cancellations) = mpsc::channel();
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = Arc::clone(&busy);
        let thread = thread::Builder::new()
            .name("dust-tool-installer".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let (definition, operation, executable, proxy, cwd, component) = match request {
                        InstallerRequest::Install { definition } => (
                            definition,
                            ToolOperation::InstallManaged,
                            None,
                            None,
                            None,
                            None,
                        ),
                        InstallerRequest::InstallRustup {
                            definition,
                            rustup,
                            proxy,
                            cwd,
                            component,
                        } => (
                            definition,
                            ToolOperation::InstallRustup,
                            Some(rustup),
                            Some(proxy),
                            Some(cwd),
                            Some(component),
                        ),
                        InstallerRequest::Uninstall { definition } => (
                            definition,
                            ToolOperation::UninstallManaged,
                            None,
                            None,
                            None,
                            None,
                        ),
                        InstallerRequest::InstallNix { definition, nix } => (
                            definition,
                            ToolOperation::InstallNix,
                            Some(nix),
                            None,
                            None,
                            None,
                        ),
                        InstallerRequest::UninstallNix { definition } => (
                            definition,
                            ToolOperation::UninstallNix,
                            None,
                            None,
                            None,
                            None,
                        ),
                        InstallerRequest::Shutdown => break,
                    };
                    let id = definition.id.clone();
                    let result_id = id.clone();
                    let progress = progress.clone();
                    let mut report = move |phase: &str, completed: u64, total: Option<u64>| {
                        let _ = progress.try_send(ToolInstallProgress {
                            id: id.clone(),
                            operation,
                            phase: phase.into(),
                            completed,
                            total,
                        });
                    };
                    report(
                        match operation {
                            ToolOperation::InstallManaged
                            | ToolOperation::InstallRustup
                            | ToolOperation::InstallNix => "preparing",
                            ToolOperation::UninstallManaged | ToolOperation::UninstallNix => {
                                "uninstalling"
                            }
                        },
                        0,
                        None,
                    );
                    let result = match operation {
                        ToolOperation::InstallManaged => {
                            install_tool(&definition, &root, &cancellations, &mut report, None)
                        }
                        ToolOperation::InstallRustup => install_path_rustup_component(
                            &definition,
                            executable
                                .as_deref()
                                .expect("rustup install request has executable"),
                            proxy.as_deref().expect("rustup install request has proxy"),
                            cwd.as_deref().expect("rustup install request has cwd"),
                            component
                                .as_deref()
                                .expect("rustup install request has component"),
                            &cancellations,
                            &mut report,
                        ),
                        ToolOperation::UninstallManaged => {
                            uninstall_managed_tool(&definition, &root).map(|()| None)
                        }
                        ToolOperation::InstallNix => install_nix_lsp_profile(
                            &definition,
                            &root,
                            executable
                                .as_deref()
                                .expect("Nix install request has executable"),
                            &cancellations,
                            &mut report,
                        ),
                        ToolOperation::UninstallNix => {
                            uninstall_nix_lsp_tool(&definition, &root).map(|()| None)
                        }
                    };
                    worker_busy.store(false, Ordering::Release);
                    if results
                        .send(ToolInstallResult {
                            id: result_id,
                            operation,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("tool installer thread must start");
        Self {
            sender,
            receiver,
            progress_receiver,
            cancel,
            busy,
            thread: Some(thread),
        }
    }

    pub fn install(&self, definition: ToolDefinition) -> Result<(), ToolInstallerError> {
        if definition.install.is_none() {
            return Err(ToolInstallerError::Unavailable);
        }
        self.submit(InstallerRequest::Install { definition })
    }

    pub fn uninstall(&self, definition: ToolDefinition) -> Result<(), ToolInstallerError> {
        self.submit(InstallerRequest::Uninstall { definition })
    }

    pub fn install_rustup_component(
        &self,
        definition: ToolDefinition,
        rustup: PathBuf,
        proxy: PathBuf,
        cwd: PathBuf,
        component: String,
    ) -> Result<(), ToolInstallerError> {
        self.submit(InstallerRequest::InstallRustup {
            definition,
            rustup,
            proxy,
            cwd,
            component,
        })
    }

    pub fn install_nix(
        &self,
        definition: ToolDefinition,
        nix: PathBuf,
    ) -> Result<(), ToolInstallerError> {
        if nix_lsp_shell(&definition).is_none() {
            return Err(ToolInstallerError::Unavailable);
        }
        self.submit(InstallerRequest::InstallNix { definition, nix })
    }

    pub fn uninstall_nix(&self, definition: ToolDefinition) -> Result<(), ToolInstallerError> {
        if nix_lsp_shell(&definition).is_none() {
            return Err(ToolInstallerError::Unavailable);
        }
        self.submit(InstallerRequest::UninstallNix { definition })
    }

    fn submit(&self, request: InstallerRequest) -> Result<(), ToolInstallerError> {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ToolInstallerError::Busy);
        }
        self.sender.try_send(request).map_err(|error| {
            self.busy.store(false, Ordering::Release);
            match error {
                TrySendError::Full(_) => ToolInstallerError::Busy,
                TrySendError::Disconnected(_) => ToolInstallerError::Stopped,
            }
        })
    }

    pub fn try_progress(&self) -> Result<Option<ToolInstallProgress>, ToolInstallerError> {
        match self.progress_receiver.try_recv() {
            Ok(progress) => Ok(Some(progress)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ToolInstallerError::Stopped),
        }
    }

    pub fn try_result(&self) -> Result<Option<ToolInstallResult>, ToolInstallerError> {
        match self.receiver.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ToolInstallerError::Stopped),
        }
    }
}

impl Drop for ToolInstaller {
    fn drop(&mut self) {
        let _ = self.cancel.send(());
        let _ = self.sender.send(InstallerRequest::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
