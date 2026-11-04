use std::{
    cell::RefCell,
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread::{self, JoinHandle},
};

use super::{
    catalog::{ToolDefinition, nix_lsp_shell, tool_store_root},
    install::{ToolInstallerError, install_tool},
    runtimes::{
        install_nix_lsp_profile, install_path_rustup_component, uninstall_managed_tool,
        uninstall_nix_lsp_tool,
    },
};

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
    sender: Sender<InstallerRequest>,
    receiver: Receiver<ToolInstallResult>,
    progress_receiver: Receiver<ToolInstallProgress>,
    progress: RefCell<VecDeque<ToolInstallProgress>>,
    cancel: Sender<()>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ToolInstaller {
    #[must_use]
    pub fn start() -> Self {
        Self::start_at(tool_store_root())
    }

    #[must_use]
    pub fn start_at(root: PathBuf) -> Self {
        // Requests queue up and run one at a time, so asking for several installs works.
        let (sender, requests) = mpsc::channel();
        let (results, receiver) = mpsc::channel();
        let (progress, progress_receiver) = mpsc::sync_channel(32);
        let (cancel, cancellations) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let thread = thread::Builder::new()
            .name("dust-tool-installer".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    if worker_stopping.load(Ordering::Acquire) {
                        break;
                    }
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
            progress: RefCell::new(VecDeque::new()),
            cancel,
            stopping,
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
        self.sender
            .send(request)
            .map_err(|_| ToolInstallerError::Stopped)
    }

    pub fn try_progress(&self) -> Result<Option<ToolInstallProgress>, ToolInstallerError> {
        if let Some(progress) = self.progress.borrow_mut().pop_front() {
            return Ok(Some(progress));
        }
        match self.progress_receiver.try_recv() {
            Ok(progress) => Ok(Some(progress)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ToolInstallerError::Stopped),
        }
    }

    pub fn try_result(&self) -> Result<Option<ToolInstallResult>, ToolInstallerError> {
        match self.receiver.try_recv() {
            Ok(result) => {
                let mut progress = self.progress.borrow_mut();
                progress.extend(self.progress_receiver.try_iter());
                progress.retain(|update| update.id != result.id);
                Ok(Some(result))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(ToolInstallerError::Stopped),
        }
    }
}

impl Drop for ToolInstaller {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.cancel.send(());
        let _ = self.sender.send(InstallerRequest::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_discards_late_progress_for_only_that_tool() {
        let (sender, _) = mpsc::channel();
        let (results, receiver) = mpsc::channel();
        let (progress_sender, progress_receiver) = mpsc::channel();
        let (cancel, _) = mpsc::channel();
        let installer = ToolInstaller {
            sender,
            receiver,
            progress_receiver,
            progress: RefCell::new(VecDeque::new()),
            cancel,
            stopping: Arc::new(AtomicBool::new(false)),
            thread: None,
        };
        assert!(installer.try_progress().unwrap().is_none());
        for id in ["one", "two"] {
            progress_sender
                .send(ToolInstallProgress {
                    id: id.into(),
                    operation: ToolOperation::InstallManaged,
                    phase: "verifying".into(),
                    completed: 1,
                    total: None,
                })
                .unwrap();
        }
        results
            .send(ToolInstallResult {
                id: "one".into(),
                operation: ToolOperation::InstallManaged,
                result: Ok(None),
            })
            .unwrap();
        assert_eq!(installer.try_result().unwrap().unwrap().id, "one");
        assert_eq!(installer.try_progress().unwrap().unwrap().id, "two");
        assert!(installer.try_progress().unwrap().is_none());
    }
}
