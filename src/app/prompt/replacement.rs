//! Replaceable ordinary input requests; permission questions always use host UI.

use crate::app::App;
use crate::ui::{PromptKind, PromptState};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

#[allow(dead_code)] // Presentation adapters consume this on their own worker.
pub(in crate::app) struct InputRequest {
    pub prompt: PromptState,
    pub reply: SyncSender<Option<String>>,
}

#[derive(Default)]
pub(in crate::app) struct InputReplacement {
    requests: Option<SyncSender<InputRequest>>,
    pending: Option<(Receiver<Option<String>>, Instant)>,
}

impl InputReplacement {
    #[allow(dead_code)]
    pub(in crate::app) fn install(&mut self, requests: Option<SyncSender<InputRequest>>) {
        self.requests = requests;
        self.pending = None;
    }

    fn request(&mut self, prompt: &PromptState) {
        self.pending = None;
        // Confirmation prompts have their own host-owned actions and visibility checks.
        // In particular workspace trust and plugin/agent permissions must never be replaced.
        if !matches!(
            prompt.kind,
            PromptKind::Command
                | PromptKind::SearchForward
                | PromptKind::SearchBackward
                | PromptKind::ExplorerRename
                | PromptKind::ReviewComment
                | PromptKind::BreakpointCondition
                | PromptKind::ReplaceFind
                | PromptKind::ReplaceWith
        ) {
            return;
        }
        let Some(requests) = &self.requests else {
            return;
        };
        let (reply, receiver) = std::sync::mpsc::sync_channel(1);
        if requests
            .try_send(InputRequest {
                prompt: prompt.clone(),
                reply,
            })
            .is_ok()
        {
            self.pending = Some((receiver, Instant::now() + Duration::from_secs(60)));
        }
    }
}

impl App {
    pub(super) fn request_input_replacement(&mut self) {
        if let Some(prompt) = self.input.prompt() {
            self.input_replacement.request(prompt);
        }
        self.project_input_prompt();
    }

    pub(in crate::app) fn process_input_replacement(&mut self) {
        let Some((receiver, deadline)) = self.input_replacement.pending.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Some(text)) => {
                // Preserve the native continuation: rename, search, command parsing, etc.
                if let Some(prompt) = self.input.prompt().cloned() {
                    self.input.open_prompt(prompt.kind, text, None);
                    self.project_input_prompt();
                    self.submit_prompt();
                }
            }
            Ok(None) => self.close_prompt(),
            Err(TryRecvError::Empty) if Instant::now() < deadline => {
                self.input_replacement.pending = Some((receiver, deadline));
            }
            Err(_) => self.project_input_prompt(),
        }
    }

    pub(super) fn cancel_input_replacement(&mut self) {
        self.input_replacement.pending = None;
    }

    pub(in crate::app) fn input_replacement_pending(&self) -> bool {
        self.input_replacement.pending.is_some()
    }
}
