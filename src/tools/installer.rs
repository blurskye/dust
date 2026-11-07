//! Background tool installation and progress.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs::File;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::mpsc::TryRecvError;
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::tools::catalog::ToolDefinition;

use crate::tools::catalog::nix_lsp_shell;

use crate::tools::catalog::tool_store_root;

use crate::tools::install::ToolInstallerError;

use crate::tools::install::install_tool;

use crate::tools::install_prepare::ensure_private_directory;

use crate::tools::runtimes::install_nix_lsp_profile;

use crate::tools::runtimes::install_path_rustup_component;

use crate::tools::runtimes::uninstall_managed_tool;

use crate::tools::runtimes::uninstall_nix_lsp_tool;

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

fn lock_tool_store(
    root: &Path,
    cancellations: &Receiver<()>,
) -> Result<nix::fcntl::Flock<File>, String> {
    ensure_private_directory(root)?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(root.join(".installer.lock"))
        .map_err(|error| format!("cannot open tool store lock: {error}"))?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("tool store lock must be a regular file".into());
    }
    loop {
        match nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock) {
            Ok(lock) => return Ok(lock),
            Err((returned, nix::errno::Errno::EWOULDBLOCK)) => file = returned,
            Err((_, error)) => return Err(format!("cannot lock tool store: {error}")),
        }
        match cancellations.recv_timeout(Duration::from_millis(50)) {
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            _ => return Err("tool installation cancelled while waiting for the store lock".into()),
        }
    }
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
                    let result = (|| {
                        // All instances share runtimes and destinations under this store.
                        let _lock = lock_tool_store(&root, &cancellations)?;
                        match operation {
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
                        }
                    })();
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
    use crate::tools::installer::*;

    #[test]
    fn installer_store_lock_serializes_instances_and_waiting_can_be_cancelled() {
        let directory = tempfile::tempdir().unwrap();
        let (cancel, cancellations) = mpsc::channel();
        let held = lock_tool_store(directory.path(), &cancellations).unwrap();
        let definition = crate::tools::bundled_tool_catalog()
            .into_iter()
            .find(|tool| tool.id == "rust-analyzer")
            .unwrap();
        let destination = directory.path().join(&definition.id);
        std::fs::create_dir(&destination).unwrap();
        let installer = ToolInstaller::start_at(directory.path().to_path_buf());
        installer.uninstall(definition.clone()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if installer.try_progress().unwrap().is_some() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "worker did not start");
            thread::yield_now();
        }
        // Drop cancels a waiting worker rather than hanging on another process's lock.
        drop(installer);
        assert!(destination.is_dir(), "a locked store must not be mutated");
        drop(held);
        drop(cancel);

        let installer = ToolInstaller::start_at(directory.path().to_path_buf());
        installer.uninstall(definition).unwrap();
        loop {
            if let Some(result) = installer.try_result().unwrap() {
                assert!(result.result.is_ok(), "{result:?}");
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "uninstall did not finish"
            );
            thread::yield_now();
        }
        assert!(!destination.exists());
    }

    #[test]
    fn installer_store_lock_is_released_after_failure() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join(".installer.lock"), b"").unwrap();
        let installer = ToolInstaller::start_at(directory.path().to_path_buf());
        let mut definition = crate::tools::bundled_tool_catalog()
            .into_iter()
            .find(|tool| tool.id == "rust-analyzer")
            .unwrap();
        definition.id = "../invalid".into();
        installer.uninstall(definition).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = installer.try_result().unwrap() {
                assert!(result.result.is_err());
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not finish"
            );
            thread::yield_now();
        }
        let file = File::open(directory.path().join(".installer.lock")).unwrap();
        let _lock =
            nix::fcntl::Flock::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock).unwrap();
    }

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
