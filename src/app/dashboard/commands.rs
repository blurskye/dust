//! Command registration and execution for dashboard actions and visibility.

use crate::app::App;
use crate::app::commands::AppEffect;

impl App {
    pub(in crate::app) fn process_dashboard_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::DashboardSelect(step) => {
                let last = self.ui.dashboard_actions.len().saturating_sub(1);
                self.ui.dashboard_selected = if step < 0 {
                    self.ui
                        .dashboard_selected
                        .saturating_sub(usize::from(step.unsigned_abs()))
                } else {
                    self.ui
                        .dashboard_selected
                        .saturating_add(usize::from(step.unsigned_abs()))
                }
                .min(last);
            }
            AppEffect::DashboardOpen => {
                if let Some(invocation) = self
                    .configuration
                    .dashboard_invocation(self.ui.dashboard_selected)
                {
                    self.invoke_mapped(invocation);
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
