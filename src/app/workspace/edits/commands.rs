//! Command registration and execution for workspace edit preparation and approval.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::app::picker::PickerPayload;

impl App {
    pub(in crate::app) fn process_workspace_edits_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::WorkspaceEditToggle => {
                self.close_stale_picker();
                let selected = self
                    .selected_picker_payload()
                    .and_then(|payload| match payload {
                        PickerPayload::ProposalFile(index) => Some(*index),
                        _ => None,
                    });
                if let Some(index) = selected
                    && self.workspace_edits.toggle(index).is_ok()
                {
                    self.show_workspace_proposal();
                }
            }
            AppEffect::WorkspaceEditAccept => self.accept_workspace_proposal(),
            AppEffect::WorkspaceEditReject => self.reject_workspace_proposal(),
            AppEffect::WorkspaceResourceApply => loop {
                let proposal = self.workspace_edits.next_resource_proposal();
                let result = match self.workspace_edits.prepare_resource(
                    &self.workspace,
                    self.configuration.binding_revision_ref(),
                    &self.state,
                ) {
                    Ok(Some(prepared)) => self.documents.submit_workspace_resource(prepared),
                    Ok(None) => break,
                    Err(error) => {
                        self.language.cancel_deferred_edits(proposal);
                        self.show_error(error);
                        break;
                    }
                };
                match result {
                    Ok(true) => {}
                    Ok(false) => {
                        self.notify_message(
                            "Document queue is full; remaining resources are still staged. Apply them again after pending work finishes.".into(),
                            false,
                        );
                        break;
                    }
                    Err(error) => {
                        self.language.cancel_deferred_edits(proposal);
                        self.show_error(error);
                        break;
                    }
                }
            },
            effect => return Err(effect),
        }
        Ok(())
    }
}
