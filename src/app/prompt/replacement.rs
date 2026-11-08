//! Replaceable ordinary input requests; permission questions always use host UI.

use crate::app::App;
use crate::ui::{PromptKind, PromptState};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

pub(in crate::app) struct InputRequest {
    pub prompt: PromptState,
    pub authority: crate::editor::CommandAuthority,
    pub secret: bool,
    pub reply: SyncSender<Option<String>>,
}

#[derive(Default)]
pub(in crate::app) struct InputReplacement {
    requests: Option<SyncSender<InputRequest>>,
    pending: Option<(
        Receiver<Option<String>>,
        Instant,
        crate::editor::CommandAuthority,
    )>,
    authority: crate::editor::CommandAuthority,
    response: Option<SyncSender<Option<String>>>,
    secret: bool,
}

impl InputReplacement {
    pub(in crate::app) fn install(
        &mut self,
        requests: Option<SyncSender<InputRequest>>,
        authority: crate::editor::CommandAuthority,
    ) {
        self.requests = requests;
        self.authority = authority;
        self.pending = None;
    }

    fn request(&mut self, prompt: &PromptState, authority: &crate::editor::CommandAuthority) {
        self.pending = None;
        // Confirmation prompts have their own host-owned actions and visibility checks.
        // In particular workspace trust and plugin/agent permissions must never be replaced.
        if !matches!(
            prompt.kind,
            PromptKind::Text
                | PromptKind::Command
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
                authority: authority.restrict(&self.authority),
                secret: self.secret,
                reply,
            })
            .is_ok()
        {
            self.pending = Some((
                receiver,
                Instant::now() + Duration::from_secs(60),
                authority.restrict(&self.authority),
            ));
        }
    }
}

impl App {
    pub(in crate::app) fn request_text_input(
        &mut self,
        title: String,
        initial: String,
        secret: bool,
        reply: SyncSender<Option<String>>,
    ) -> Result<(), String> {
        self.effect_authority
            .require(crate::editor::CommandAccess::Ui)
            .map_err(|error| error.to_string())?;
        if self.input.prompt().is_some() {
            return Err("another prompt is open".into());
        }
        self.open_prompt(PromptKind::Text, initial);
        self.input.prompt_help(title);
        self.input_replacement.response = Some(reply);
        self.input_replacement.secret = secret;
        self.request_input_replacement();
        Ok(())
    }

    pub(super) fn finish_text_input(&mut self, value: Option<String>) {
        if let Some(reply) = self.input_replacement.response.take() {
            let _ = reply.try_send(value);
        }
        self.input_replacement.secret = false;
    }

    pub(super) fn mask_text_input(&self, prompt: &mut PromptState) {
        if self.input_replacement.secret {
            prompt.cursor = prompt.input[..prompt.cursor].chars().count();
            prompt.input = "*".repeat(prompt.input.chars().count());
        }
    }

    pub(super) fn request_input_replacement(&mut self) {
        if let Some(prompt) = self.input.prompt() {
            self.input_replacement
                .request(prompt, &self.effect_authority);
        }
        self.project_input_prompt();
    }

    pub(in crate::app) fn process_input_replacement(&mut self) {
        let Some((receiver, deadline, authority)) = self.input_replacement.pending.take() else {
            return;
        };
        match receiver.try_recv() {
            Ok(Some(text)) => {
                // Preserve the native continuation: rename, search, command parsing, etc.
                if let Some(prompt) = self.input.prompt().cloned() {
                    self.input.open_prompt(prompt.kind, text, None);
                    self.project_input_prompt();
                    let previous = std::mem::replace(&mut self.effect_authority, authority);
                    self.submit_prompt();
                    self.effect_authority = previous;
                }
            }
            Ok(None) => self.close_prompt(),
            Err(TryRecvError::Empty) if Instant::now() < deadline => {
                self.input_replacement.pending = Some((receiver, deadline, authority));
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
