use super::*;

#[derive(Clone, Debug)]
pub struct NixWorkspaceResult {
    pub generation: u64,
    pub result: Result<PathBuf, String>,
}

pub(super) enum NixWorkspaceRequest {
    Prepare { generation: u64 },
    Shutdown,
}

/// Writes the pinned compatibility flake, without evaluating workspace-owned flakes.
/// Closing revokes delivery; a running filesystem operation may still finish.
pub struct NixWorkspaceService {
    next_generation: Option<u64>,
    sender: Option<SyncSender<NixWorkspaceRequest>>,
    receiver: Option<Receiver<NixWorkspaceResult>>,
    thread: Option<JoinHandle<()>>,
}
impl Default for NixWorkspaceService {
    fn default() -> Self {
        Self::start()
    }
}
impl NixWorkspaceService {
    #[must_use]
    pub fn start() -> Self {
        Self::start_with(prepare_nix_lsp_workspace)
    }
    fn start_with(mut prepare: impl FnMut() -> Result<PathBuf, String> + Send + 'static) -> Self {
        let (sender, requests) = mpsc::sync_channel(1);
        let (results, receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("dust-nix-lsp-workspace".into())
            .spawn(move || {
                while let Ok(NixWorkspaceRequest::Prepare { generation }) = requests.recv() {
                    if results
                        .send(NixWorkspaceResult {
                            generation,
                            result: prepare(),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .ok();
        Self {
            next_generation: Some(1),
            sender: Some(sender),
            receiver: Some(receiver),
            thread,
        }
    }
    pub fn prepare(&mut self) -> Result<u64, String> {
        let generation = self
            .next_generation
            .ok_or("Nix workspace request identity exhausted")?;
        let sender = self
            .sender
            .as_ref()
            .ok_or("Nix workspace admission closed")?;
        sender
            .try_send(NixWorkspaceRequest::Prepare { generation })
            .map_err(|error| match error {
                TrySendError::Full(_) => {
                    "Nix LSP workspace preparation is already pending".to_owned()
                }
                TrySendError::Disconnected(_) => "Nix LSP workspace worker stopped".to_owned(),
            })?;
        self.next_generation = generation.checked_add(1);
        Ok(generation)
    }
    pub(crate) fn poll_result(&self) -> Result<Option<NixWorkspaceResult>, String> {
        let receiver = self
            .receiver
            .as_ref()
            .ok_or("Nix workspace delivery closed")?;
        match receiver.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err("Nix LSP workspace worker stopped".into()),
        }
    }
    #[must_use]
    pub fn try_result(&self) -> Option<NixWorkspaceResult> {
        self.poll_result().ok().flatten()
    }
    pub(crate) fn close(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.try_send(NixWorkspaceRequest::Shutdown);
        }
        self.receiver.take();
    }
    pub(crate) fn pending(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }
}
impl Drop for NixWorkspaceService {
    fn drop(&mut self) {
        self.close();
    } // Never join a possibly blocked filesystem worker.
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nix_workspace_close_does_not_join_blocked_work_and_ids_never_repeat() {
        let (entered, wait_entered) = mpsc::channel();
        let (release, wait_release) = mpsc::channel();
        // Owned callback gate, not a blocked syscall and never a real Nix/store operation.
        let mut worker = NixWorkspaceService::start_with(move || {
            entered.send(()).unwrap();
            wait_release.recv().unwrap();
            Ok(PathBuf::from("owned-fixture"))
        });
        worker.next_generation = Some(u64::MAX);
        assert_eq!(worker.prepare().unwrap(), u64::MAX);
        assert!(worker.prepare().unwrap_err().contains("exhausted"));
        wait_entered.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.close();
        assert!(worker.prepare().is_err());
        assert!(worker.poll_result().is_err());
        assert!(worker.pending());
        let (dropped, wait_dropped) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(worker);
            dropped.send(()).unwrap();
        });
        let result = wait_dropped.recv_timeout(Duration::from_secs(1));
        release.send(()).unwrap();
        dropper.join().unwrap();
        assert!(
            result.is_ok(),
            "drop waited for work that had not been released"
        );
    }
}
