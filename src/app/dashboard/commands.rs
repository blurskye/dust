//! Moving through the dashboard's boxes and opening what is selected in them.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::ui::DashboardPanel;
use crate::ui::render::dashboard_cell;
use crate::ui::render::dashboard_focus;
use crate::ui::render::dashboard_rows;
use crate::ui::render::dashboard_selected_row;
use crate::windows::SpatialDirection;

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
            AppEffect::DashboardFocus(direction) => {
                // Like moving between windows: boxes with nothing to select are passed over, and
                // past the last box the move goes on to a terminal or panel beside the dashboard.
                let shown = self.dashboard_panels();
                let (columns, rows) = match direction {
                    SpatialDirection::Left => (-1, 0),
                    SpatialDirection::Right => (1, 0),
                    SpatialDirection::Up => (0, -1),
                    SpatialDirection::Down => (0, 1),
                };
                let mut cell = dashboard_cell(shown, panel);
                let target = loop {
                    let Some((column, row)) = cell else {
                        break None;
                    };
                    let next = column
                        .checked_add_signed(columns)
                        .zip(row.checked_add_signed(rows));
                    let box_there = next.and_then(|next| {
                        shown
                            .iter()
                            .copied()
                            .find(|panel| dashboard_cell(shown, *panel) == Some(next))
                    });
                    match box_there {
                        Some(found) if dashboard_rows(&self.ui, found) > 0 => break Some(found),
                        Some(_) => cell = next,
                        None => break None,
                    }
                };
                match target {
                    Some(target) => {
                        self.ui.dashboard_focus = Some(target);
                        self.ui.dashboard_row = 0;
                    }
                    None => self.focus_surface(direction),
                }
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
