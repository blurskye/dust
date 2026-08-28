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

/// Writes Dust's pinned generic compatibility flake in the private tool store
/// without ever evaluating a workspace-owned flake. Nix owns its cache/store closure.
pub struct NixWorkspaceService {
    next_generation: u64,
    sender: SyncSender<NixWorkspaceRequest>,
    receiver: Receiver<NixWorkspaceResult>,
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
        let (sender, requests) = mpsc::sync_channel(1);
        let (results, receiver) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("dust-nix-lsp-workspace".into())
            .spawn(move || {
                while let Ok(request) = requests.recv() {
                    let NixWorkspaceRequest::Prepare { generation } = request else {
                        break;
                    };
                    let result = prepare_nix_lsp_workspace();
                    if results
                        .send(NixWorkspaceResult { generation, result })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("Nix workspace worker must start");
        Self {
            next_generation: 1,
            sender,
            receiver,
            thread: Some(thread),
        }
    }

    pub fn prepare(&mut self) -> Result<u64, String> {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        self.sender
            .try_send(NixWorkspaceRequest::Prepare { generation })
            .map_err(|error| -> String {
                match error {
                    TrySendError::Full(_) => {
                        "Nix LSP workspace preparation is already pending".into()
                    }
                    TrySendError::Disconnected(_) => "Nix LSP workspace worker stopped".into(),
                }
            })?;
        Ok(generation)
    }

    #[must_use]
    pub fn try_result(&self) -> Option<NixWorkspaceResult> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for NixWorkspaceService {
    fn drop(&mut self) {
        let _ = self.sender.send(NixWorkspaceRequest::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
