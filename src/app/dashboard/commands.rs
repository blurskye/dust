//! Moving through the dashboard's boxes and opening what is selected in them.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::ui::DashboardPanel;
use crate::ui::render::dashboard_focus;
use crate::ui::render::dashboard_rows;
use crate::ui::render::dashboard_selected_row;

impl App {
    pub(in crate::app) fn process_dashboard_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        let panel = dashboard_focus(&self.ui, self.dashboard_panels());
        match effect {
            AppEffect::DashboardSelect(step) => {
                let last = dashboard_rows(&self.ui, panel).saturating_sub(1);
                let row = dashboard_selected_row(&self.ui, panel);
                let distance = usize::from(step.unsigned_abs());
                let row = if step < 0 {
                    row.saturating_sub(distance)
                } else {
                    row.saturating_add(distance).min(last)
                };
                if panel == DashboardPanel::Start {
                    self.ui.dashboard_selected = row;
                } else {
                    self.ui.dashboard_row = row;
                }
            }
            AppEffect::DashboardFocus(step) => {
                // Tab skips boxes with nothing to select and wraps around.
                let usable = self
                    .dashboard_panels()
                    .iter()
                    .copied()
                    .filter(|panel| dashboard_rows(&self.ui, *panel) > 0)
                    .collect::<Vec<_>>();
                let Some(index) = usable.iter().position(|usable| *usable == panel) else {
                    return Ok(());
                };
                let count = usable.len();
                let next = if step < 0 {
                    (index + count - 1) % count
                } else {
                    (index + 1) % count
                };
                self.ui.dashboard_focus = Some(usable[next]);
                self.ui.dashboard_row = 0;
            }
            AppEffect::DashboardOpenRecent(index) => self.open_dashboard_recent_file(index),
            AppEffect::DashboardOpen => {
                let row = dashboard_selected_row(&self.ui, panel);
                let workspace = &self.ui.dashboard_workspace;
                match panel {
                    DashboardPanel::RecentFiles => self.open_dashboard_recent_file(row),
                    DashboardPanel::Changes => {
                        if let Some(change) = workspace
                            .changes
                            .as_ref()
                            .and_then(|changes| changes.files.get(row))
                        {
                            let path = change.path.clone();
                            self.open_dashboard_file(path);
                        }
                    }
                    DashboardPanel::Projects => {
                        if let Some(project) = workspace.projects.get(row) {
                            let path = project.path.clone();
                            self.switch_workspace_root(path);
                        }
                    }
                    DashboardPanel::Start => {
                        if let Some(invocation) = self.configuration.dashboard_invocation(row) {
                            self.invoke_mapped(invocation);
                        }
                    }
                }
            }
            effect => return Err(effect),
        }
        Ok(())
    }
}
