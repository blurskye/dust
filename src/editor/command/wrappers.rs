//! Ordered command wrappers and their suspended calls.

#[cfg(test)]
#[path = "wrapper_tests.rs"]
mod tests;

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{
    CommandAuthority, CommandError, CommandId, CommandInvocation, CommandOutcome, CommandRegistry,
    CommandSource, EditorState, validate_context,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PendingCommandId(u64);

impl PendingCommandId {
    pub(crate) fn allocate() -> Result<Self, CommandError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        NEXT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map(Self)
            .map_err(|_| CommandError::Failed("command IDs exhausted".into()))
    }
}

/// A wrapper receives `None` before the command and `Some(outcome)` after call-next.
/// Pending suspends only this invocation; its effects can schedule work off the UI thread.
#[derive(Clone, Debug)]
pub enum CommandWrapperAction<E> {
    CallNext(CommandInvocation),
    Complete(CommandOutcome<E>),
    Pending(Vec<E>),
}

type WrapperHandler<E> = dyn Fn(
        &EditorState,
        &CommandInvocation,
        PendingCommandId,
        Option<CommandOutcome<E>>,
    ) -> Result<CommandWrapperAction<E>, CommandError>
    + Send
    + Sync;

pub(super) struct CommandWrapper<E> {
    pub(super) owner: String,
    order: i32,
    authority: CommandAuthority,
    handler: Arc<WrapperHandler<E>>,
}

struct WrapperFrame {
    index: usize,
    invocation: CommandInvocation,
}

struct PendingCommand<E> {
    id: PendingCommandId,
    invocation: CommandInvocation,
    wrappers: Vec<Arc<CommandWrapper<E>>>,
    next: usize,
    frames: Vec<WrapperFrame>,
    waiting: Option<(usize, bool)>,
    authority: CommandAuthority,
}

pub(super) struct PendingCommands<E> {
    calls: BTreeMap<PendingCommandId, PendingCommand<E>>,
}

impl<E> Default for PendingCommands<E> {
    fn default() -> Self {
        Self {
            calls: BTreeMap::new(),
        }
    }
}

impl<E> CommandRegistry<E> {
    /// Lower order runs first; equal order preserves registration order.
    pub fn register_wrapper<F>(
        &mut self,
        command: &CommandId,
        owner: String,
        order: i32,
        authority: CommandAuthority,
        handler: F,
    ) -> Result<(), CommandError>
    where
        F: Fn(
                &EditorState,
                &CommandInvocation,
                PendingCommandId,
                Option<CommandOutcome<E>>,
            ) -> Result<CommandWrapperAction<E>, CommandError>
            + Send
            + Sync
            + 'static,
    {
        let command = self
            .commands
            .get_mut(command)
            .ok_or_else(|| CommandError::Unknown(command.clone()))?;
        authority.check(&command.spec)?;
        if command
            .spec
            .access
            .contains(&super::CommandAccess::HostOnly)
        {
            return Err(CommandError::AccessDenied(command.spec.id.clone()));
        }
        if command.wrappers.len() == 16 {
            return Err(CommandError::TooManyWrappers);
        }
        command.wrappers.push(Arc::new(CommandWrapper {
            owner,
            order,
            authority,
            handler: Arc::new(handler),
        }));
        command.wrappers.sort_by_key(|wrapper| wrapper.order);
        Ok(())
    }

    pub fn execute_with_authority(
        &self,
        state: &EditorState,
        invocation: &CommandInvocation,
        authority: CommandAuthority,
    ) -> Result<CommandOutcome<E>, CommandError> {
        let command = self
            .commands
            .get(&invocation.id)
            .ok_or_else(|| CommandError::Unknown(invocation.id.clone()))?;
        authority.check(&command.spec)?;
        command.spec.arguments.validate(&invocation.arguments)?;
        validate_context(state, invocation, command.spec.context)?;
        let id = PendingCommandId::allocate()?;
        self.advance(
            state,
            PendingCommand {
                id,
                invocation: invocation.clone(),
                wrappers: command.wrappers.clone(),
                next: 0,
                frames: Vec::new(),
                waiting: None,
                authority,
            },
            None,
        )
    }

    /// The ABI bridge supplies the authenticated wrapper owner, never guest-provided identity.
    pub fn resume(
        &self,
        state: &EditorState,
        id: PendingCommandId,
        owner: &str,
        action: CommandWrapperAction<E>,
    ) -> Result<CommandOutcome<E>, CommandError> {
        let mut call = {
            let mut pending = self.pending.borrow_mut();
            let call = pending.calls.get(&id).ok_or(CommandError::UnknownPending)?;
            let (index, _) = call.waiting.ok_or(CommandError::UnknownPending)?;
            if call.wrappers[index].owner != owner {
                return Err(CommandError::WrongWrapper);
            }
            pending.calls.remove(&id).expect("checked pending command")
        };
        let (index, after) = call.waiting.take().expect("waiting wrapper");
        let outcome = self.apply_wrapper_action(&mut call, index, after, action)?;
        self.advance(state, call, outcome)
    }

    pub fn cancel(&self, id: PendingCommandId) -> bool {
        self.pending.borrow_mut().calls.remove(&id).is_some()
    }

    pub fn is_pending(&self, id: PendingCommandId) -> bool {
        self.pending.borrow().calls.contains_key(&id)
    }

    pub(super) fn cancel_plugin_commands(&self, owner: &str) {
        self.pending.borrow_mut().calls.retain(|_, call| {
            !call.wrappers.iter().any(|wrapper| wrapper.owner == owner)
                && !matches!(self.commands.get(&call.invocation.id).map(|command| &command.spec.source),
                    Some(CommandSource::Plugin(candidate)) if candidate == owner)
                && !matches!(&call.authority, CommandAuthority::Plugin { initiator, .. } if initiator == owner)
        });
    }

    fn apply_wrapper_action(
        &self,
        call: &mut PendingCommand<E>,
        index: usize,
        after: bool,
        action: CommandWrapperAction<E>,
    ) -> Result<Option<CommandOutcome<E>>, CommandError> {
        match action {
            CommandWrapperAction::CallNext(invocation) => {
                if after {
                    return Err(CommandError::AlreadyContinued);
                }
                if invocation.id != call.invocation.id {
                    return Err(CommandError::InvalidArguments(
                        "call-next cannot change the command ID".into(),
                    ));
                }
                call.frames.push(WrapperFrame {
                    index,
                    invocation: call.invocation.clone(),
                });
                call.invocation = invocation;
                Ok(None)
            }
            CommandWrapperAction::Complete(outcome) => {
                self.validate_result(&call.invocation.id, &outcome)?;
                Ok(Some(outcome))
            }
            CommandWrapperAction::Pending(effects) => {
                call.waiting = Some((index, after));
                let mut outcome = CommandOutcome::empty();
                outcome.effects = effects;
                outcome.pending = Some(call.id);
                Ok(Some(outcome))
            }
        }
    }

    fn advance(
        &self,
        state: &EditorState,
        mut call: PendingCommand<E>,
        mut outcome: Option<CommandOutcome<E>>,
    ) -> Result<CommandOutcome<E>, CommandError> {
        loop {
            let command = self
                .commands
                .get(&call.invocation.id)
                .ok_or_else(|| CommandError::Unknown(call.invocation.id.clone()))?;
            call.authority.check(&command.spec)?;
            if call.waiting.is_some() {
                let mut outcome = outcome.expect("pending action has an outcome");
                outcome.authority = call.authority.clone();
                self.pending.borrow_mut().calls.insert(call.id, call);
                return Ok(outcome);
            }
            let (index, after, input) = if let Some(result) = outcome.take() {
                self.validate_result(&call.invocation.id, &result)?;
                let Some(frame) = call.frames.pop() else {
                    return Ok(CommandOutcome {
                        authority: call.authority,
                        ..result
                    });
                };
                call.invocation = frame.invocation;
                (frame.index, true, Some(result))
            } else {
                command
                    .spec
                    .arguments
                    .validate(&call.invocation.arguments)?;
                validate_context(state, &call.invocation, command.spec.context)?;
                if call.next == call.wrappers.len() {
                    outcome = Some((command.handler)(state, &call.invocation)?);
                    continue;
                }
                let index = call.next;
                call.next += 1;
                (index, false, None)
            };
            let wrapper = &call.wrappers[index];
            call.authority = call.authority.restrict(&wrapper.authority);
            call.authority.check(&command.spec)?;
            let action = (wrapper.handler)(state, &call.invocation, call.id, input)?;
            outcome = self.apply_wrapper_action(&mut call, index, after, action)?;
        }
    }

    fn validate_result(
        &self,
        id: &CommandId,
        outcome: &CommandOutcome<E>,
    ) -> Result<(), CommandError> {
        if outcome.pending.is_some() {
            return Err(CommandError::Failed(
                "handlers must suspend through a wrapper action".into(),
            ));
        }
        let command = self
            .commands
            .get(id)
            .ok_or_else(|| CommandError::Unknown(id.clone()))?;
        if let Some(kind) = command.spec.result
            && !outcome
                .value
                .as_ref()
                .is_some_and(|value| kind.accepts(value))
        {
            return Err(CommandError::Failed(format!(
                "command `{id}` returned the wrong type"
            )));
        }
        Ok(())
    }
}
