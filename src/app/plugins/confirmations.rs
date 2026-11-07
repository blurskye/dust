//! Plugin confirmation requests and their answers.

use crate::app::input::InputState;
use crate::app::plugins::edits::ConfirmationSource;
use crate::app::windows::Windows;
use crate::app::workspace::WorkspaceVersion;
use crate::editor::EditorState;
use crate::plugins::PluginConfirmationAnswer;
use crate::plugins::PluginConfirmationError;
use crate::plugins::PluginConfirmationErrorCode;
use crate::plugins::PluginConfirmationOutcome;
use crate::plugins::PluginConfirmationRequest;
use crate::plugins::PluginConfirmationResult;
use crate::plugins::PluginExecution;
use crate::plugins::PluginExecutionReply;
use crate::plugins::PluginExecutionRequest;
use crate::plugins::PluginHost;
use crate::plugins::PluginInvocationOrigin;
use crate::plugins::PluginLoadId;
use crate::plugins::PluginUiEvent;
use std::collections::BTreeMap;
use std::collections::VecDeque;

const CONFIRMATION_LIMIT: usize = 64;

#[derive(Clone)]
struct PendingConfirmation {
    plugin: String,
    load: PluginLoadId,
    command: String,
    source: ConfirmationSource,
    request: PluginConfirmationRequest,
}

struct AnsweredConfirmation {
    pending: PendingConfirmation,
    outcome: PluginConfirmationOutcome,
}

struct Delivery {
    origin: PluginInvocationOrigin,
    pending: PendingConfirmation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::app) struct ConfirmationPresentation {
    pub plugin: String,
    pub request: u64,
    pub title: String,
    pub message: String,
    pub default_answer: PluginConfirmationAnswer,
}

pub(super) struct ConfirmationContext<'a> {
    pub manager: &'a PluginHost,
    pub editor: &'a EditorState,
    pub workspace: &'a WorkspaceVersion,
    pub configuration: u64,
    pub input: &'a InputState,
    pub windows: &'a Windows,
}

pub(super) struct ConfirmationReservationLoan<'a> {
    owner: &'a mut PluginConfirmations,
    origin: PluginInvocationOrigin,
    reserved: bool,
    committed: bool,
}
impl ConfirmationReservationLoan<'_> {
    pub(in crate::app) fn available(&self) -> bool {
        self.reserved
    }
    pub(super) fn enqueue<T>(
        mut self,
        enqueue: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let result = enqueue();
        self.committed = result.is_ok();
        result
    }
}
impl Drop for ConfirmationReservationLoan<'_> {
    fn drop(&mut self) {
        if self.reserved
            && !self.committed
            && let Some(index) = self
                .owner
                .reservations
                .iter()
                .position(|reservation| reservation.same(&self.origin))
        {
            self.owner.reservations.remove(index);
        }
    }
}

#[derive(Default)]
pub(super) struct PluginConfirmations {
    pending: VecDeque<PendingConfirmation>,
    active: Option<PendingConfirmation>,
    answered: VecDeque<AnsweredConfirmation>,
    deliveries: Vec<Delivery>,
    reservations: Vec<PluginInvocationOrigin>,
}

impl PluginConfirmations {
    pub(in crate::app) fn len(&self) -> usize {
        self.pending.len()
            + usize::from(self.active.is_some())
            + self.answered.len()
            + self.deliveries.len()
            + self.reservations.len()
    }

    pub(super) fn reserve<'a>(
        &'a mut self,
        origin: &PluginInvocationOrigin,
    ) -> ConfirmationReservationLoan<'a> {
        let reserved = self.len() < CONFIRMATION_LIMIT;
        if reserved {
            self.reservations.push(origin.clone());
        }
        ConfirmationReservationLoan {
            owner: self,
            origin: origin.clone(),
            reserved,
            committed: false,
        }
    }

    pub(in crate::app) fn release_terminal(&mut self, origin: &PluginInvocationOrigin) -> bool {
        let Some(index) = self
            .reservations
            .iter()
            .position(|reservation| reservation.same(origin))
        else {
            return false;
        };
        self.reservations.remove(index);
        true
    }

    pub(in crate::app) fn queue(
        &mut self,
        source: ConfirmationSource,
        requests: Vec<PluginConfirmationRequest>,
        manager: &PluginHost,
    ) -> Result<(), String> {
        if !source.manager_current(manager) {
            return Err("plugin confirmation source is stale".into());
        }
        if requests.len() > 1 {
            return Err("plugin execution exceeded its confirmation request limit".into());
        }
        let reservation = self
            .reservations
            .iter()
            .position(|reservation| reservation.same(source.origin()));
        if !requests.is_empty() && reservation.is_none() {
            return Err("plugin confirmation request had no reserved host capacity".into());
        }
        if let Some(index) = reservation {
            self.reservations.remove(index);
        }
        for request in requests {
            let origin = source.origin();
            let duplicate = self.pending.iter().any(|pending| {
                pending.plugin == origin.plugin()
                    && pending.load == origin.load()
                    && pending.request.token == request.token
            }) || self.active.as_ref().is_some_and(|pending| {
                pending.plugin == origin.plugin()
                    && pending.load == origin.load()
                    && pending.request.token == request.token
            }) || self.answered.iter().any(|answered| {
                answered.pending.plugin == origin.plugin()
                    && answered.pending.load == origin.load()
                    && answered.pending.request.token == request.token
            }) || self.deliveries.iter().any(|delivery| {
                delivery.pending.plugin == origin.plugin()
                    && delivery.pending.load == origin.load()
                    && delivery.pending.request.token == request.token
            });
            if duplicate {
                return Err("duplicate plugin confirmation request".into());
            }
            let pending = PendingConfirmation {
                plugin: origin.plugin().into(),
                load: origin.load(),
                command: origin.command().into(),
                source: source.clone(),
                request,
            };
            self.pending.push_back(pending);
        }
        Ok(())
    }

    fn failed(pending: PendingConfirmation, detail: &str) -> AnsweredConfirmation {
        AnsweredConfirmation {
            pending,
            outcome: PluginConfirmationOutcome::Failed(PluginConfirmationError::new(
                PluginConfirmationErrorCode::StaleSource,
                detail,
            )),
        }
    }

    pub(in crate::app) fn activate_next(
        &mut self,
        context: ConfirmationContext<'_>,
    ) -> Option<ConfirmationPresentation> {
        if self.active.is_some() {
            return None;
        }
        while let Some(pending) = self.pending.pop_front() {
            if !pending.source.manager_current(context.manager) {
                continue;
            }
            if !pending.source.presentation_current(
                context.manager,
                context.editor,
                context.workspace,
                context.configuration,
                context.input,
                context.windows,
            ) {
                self.answered.push_back(Self::failed(
                    pending,
                    "confirmation source changed before presentation",
                ));
                continue;
            }
            let presentation = ConfirmationPresentation {
                plugin: pending.plugin.clone(),
                request: pending.request.token,
                title: pending.request.title.clone(),
                message: pending.request.message.clone(),
                default_answer: pending.request.default_answer,
            };
            self.active = Some(pending);
            return Some(presentation);
        }
        None
    }

    pub(in crate::app) fn answer(&mut self, answer: PluginConfirmationAnswer) -> bool {
        let Some(pending) = self.active.take() else {
            return false;
        };
        self.answered.push_back(AnsweredConfirmation {
            pending,
            outcome: PluginConfirmationOutcome::Answered(answer),
        });
        true
    }

    pub(in crate::app) fn dispatch(
        &mut self,
        manager: &mut PluginHost,
        editor: &EditorState,
        workspace: &WorkspaceVersion,
        configuration: u64,
        ui: &mut BTreeMap<(String, PluginLoadId), Vec<PluginUiEvent>>,
        runtime_epoch: u64,
    ) -> Result<bool, String> {
        let Some(answered) = self.answered.front_mut() else {
            return Ok(false);
        };
        if !answered.pending.source.manager_current(manager) {
            self.answered.pop_front();
            return Ok(true);
        }
        if matches!(answered.outcome, PluginConfirmationOutcome::Answered(_))
            && !answered
                .pending
                .source
                .current(manager, editor, workspace, configuration)
        {
            answered.outcome = PluginConfirmationOutcome::Failed(PluginConfirmationError::new(
                PluginConfirmationErrorCode::StaleSource,
                "confirmation source changed before result delivery",
            ));
        }
        let reservation = manager
            .prepare_execution(&answered.pending.plugin, &answered.pending.command)
            .map_err(|error| error.to_string())?;
        if reservation.origin().load() != answered.pending.load {
            return Err("plugin confirmation load changed before result delivery".into());
        }
        let origin = reservation.origin().clone();
        let ui_events = ui
            .remove(&(answered.pending.plugin.clone(), answered.pending.load))
            .unwrap_or_default();
        let request = PluginExecutionRequest {
            call: Default::default(),
            delivery: Default::default(),
            runtime_epoch,
            plugin: answered.pending.plugin.clone(),
            command: answered.pending.command.clone(),
            arguments: Vec::new(),
            snapshot: None,
            config: BTreeMap::new(),
            ui_revision: None,
            confirmation_available: false,
            ui_events,
            confirmation_results: vec![PluginConfirmationResult {
                request: answered.pending.request.token,
                outcome: answered.outcome.clone(),
            }],
            host_events: Vec::new(),
            storage_results: Vec::new(),
            provider_requests: Vec::new(),
            virtual_document_requests: Vec::new(),
            request_id: answered.pending.request.token,
        };
        reservation
            .submit(request)
            .map_err(|error| error.to_string())?;
        let answered = self.answered.pop_front().expect("checked answer");
        self.deliveries.push(Delivery {
            origin,
            pending: answered.pending,
        });
        Ok(true)
    }

    pub(in crate::app) fn owns(&self, origin: &PluginInvocationOrigin) -> bool {
        self.deliveries
            .iter()
            .any(|delivery| delivery.origin.same(origin))
    }

    pub(in crate::app) fn receive(
        &mut self,
        reply: PluginExecutionReply,
        manager: &PluginHost,
    ) -> Result<(PluginExecution, ConfirmationSource), String> {
        let origin = reply
            .origin()
            .ok_or("plugin confirmation delivery has no source")?
            .clone();
        let index = self
            .deliveries
            .iter()
            .position(|delivery| delivery.origin.same(&origin))
            .ok_or("plugin confirmation result was stale or duplicated")?;
        let delivery = self.deliveries.remove(index);
        let (execution, reply_origin) = reply.into_parts();
        let reply_origin = reply_origin.ok_or("plugin confirmation result lost its source")?;
        if !delivery.pending.source.manager_current(manager)
            || !reply_origin.current(manager)
            || execution.plugin != delivery.pending.plugin
            || execution.command != delivery.pending.command
            || !execution.proposals.is_empty()
        {
            return Err(
                "plugin confirmation result source changed or proposed an unrelated edit".into(),
            );
        }
        Ok((execution, delivery.pending.source))
    }

    pub(in crate::app) fn invalidate_stale(&mut self, context: ConfirmationContext<'_>) -> bool {
        let active_stale = self.active.as_ref().is_some_and(|pending| {
            pending.source.manager_current(context.manager)
                && !pending.source.current(
                    context.manager,
                    context.editor,
                    context.workspace,
                    context.configuration,
                )
        });
        if active_stale {
            let pending = self.active.take().expect("checked active confirmation");
            self.answered.push_back(Self::failed(
                pending,
                "confirmation source changed while awaiting an answer",
            ));
        }
        let mut retained = VecDeque::new();
        while let Some(pending) = self.pending.pop_front() {
            if !pending.source.manager_current(context.manager) {
                continue;
            }
            if pending.source.presentation_current(
                context.manager,
                context.editor,
                context.workspace,
                context.configuration,
                context.input,
                context.windows,
            ) {
                retained.push_back(pending);
            } else {
                self.answered.push_back(Self::failed(
                    pending,
                    "confirmation source changed before presentation",
                ));
            }
        }
        self.pending = retained;
        self.answered
            .retain(|answered| answered.pending.source.manager_current(context.manager));
        self.deliveries
            .retain(|delivery| delivery.pending.source.manager_current(context.manager));
        self.reservations
            .retain(|origin| origin.current(context.manager));
        active_stale
    }

    pub(in crate::app) fn remove_load(&mut self, plugin: &str, load: PluginLoadId) -> bool {
        let active = self
            .active
            .as_ref()
            .is_some_and(|pending| pending.plugin == plugin && pending.load == load);
        if active {
            self.active = None;
        }
        self.pending
            .retain(|pending| pending.plugin != plugin || pending.load != load);
        self.answered
            .retain(|answered| answered.pending.plugin != plugin || answered.pending.load != load);
        self.deliveries
            .retain(|delivery| delivery.pending.plugin != plugin || delivery.pending.load != load);
        self.reservations
            .retain(|origin| origin.plugin() != plugin || origin.load() != load);
        active
    }

    pub(in crate::app) fn reset(&mut self) -> bool {
        let active = self.active.is_some();
        *self = Self::default();
        active
    }

    #[cfg(test)]
    pub(in crate::app) fn pending_count(&self) -> usize {
        self.len()
    }
}

#[cfg(test)]
mod tests {
    use crate::app::plugins::confirmations::*;

    #[test]
    fn reservation_loan_releases_only_failed_enqueue_and_allows_retry() {
        let (mut manager, _fixture) = crate::plugins::ExecutionFixture::new(2);
        let reservation = manager.prepare_execution("demo", "run").unwrap();
        let origin = reservation.origin().clone();
        drop(reservation);
        let mut confirmations = PluginConfirmations::default();

        let loan = confirmations.reserve(&origin);
        assert!(loan.available());
        assert!(loan.enqueue::<()>(|| Err("refused".into())).is_err());
        assert_eq!(confirmations.len(), 0);

        let loan = confirmations.reserve(&origin);
        assert!(loan.available());
        loan.enqueue(|| Ok(())).unwrap();
        assert_eq!(confirmations.len(), 1);
    }
}
