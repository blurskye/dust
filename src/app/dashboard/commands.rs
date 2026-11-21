//! Moving through the dashboard's boxes and opening what is selected in them.

use crate::app::App;
use crate::app::commands::AppEffect;
use crate::ui::DashboardPanel;
use crate::ui::render::dashboard_cell;
use crate::ui::render::dashboard_focus;
use crate::ui::render::dashboard_rect;
use crate::ui::render::dashboard_rows;
use crate::ui::render::dashboard_selected_row;
use crate::windows::SpatialDirection;
use crate::windows::WindowRect;

impl App {
    pub(in crate::app) fn process_dashboard_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        let panel = dashboard_focus(&self.ui, self.dashboard_panels());
        match effect {
            AppEffect::DashboardSelect(step) => {
                if !self.move_dashboard_selection(panel, step) {
                    self.focus_surface(if step < 0 {
                        SpatialDirection::Up
                    } else {
                        SpatialDirection::Down
                    });
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

    fn move_dashboard_selection(&mut self, panel: DashboardPanel, step: i8) -> bool {
        let last = dashboard_rows(&self.ui, panel).saturating_sub(1);
        let row = dashboard_selected_row(&self.ui, panel);
        let next = row.saturating_add_signed(isize::from(step)).min(last);
        if row == next {
            return false;
        }
        self.set_dashboard_row(panel, next);
        true
    }

    pub(in crate::app) fn focus_dashboard_box(&mut self, direction: SpatialDirection) -> bool {
        let shown = self.dashboard_panels();
        let panel = dashboard_focus(&self.ui, shown);
        let (columns, rows) = match direction {
            SpatialDirection::Left => (-1, 0),
            SpatialDirection::Right => (1, 0),
            SpatialDirection::Up => (0, -1),
            SpatialDirection::Down => (0, 1),
        };
        let Some((mut column, mut row)) = dashboard_cell(shown, panel) else {
            return false;
        };

        // Pass over boxes with nothing to select; past the grid, let window focus take over.
        loop {
            let Some(next) = column
                .checked_add_signed(columns)
                .zip(row.checked_add_signed(rows))
            else {
                return false;
            };
            let Some(target) = shown
                .iter()
                .copied()
                .find(|panel| dashboard_cell(shown, *panel) == Some(next))
            else {
                return false;
            };
            if dashboard_rows(&self.ui, target) > 0 {
                self.land_in_dashboard_box(target, direction);
                return true;
            }
            (column, row) = next;
        }
    }

    pub(in crate::app) fn focus_dashboard_from(
        &mut self,
        direction: SpatialDirection,
        from: WindowRect,
    ) {
        let Some(layout) = &self.ui.compositor_layout else {
            return;
        };
        let bounds = dashboard_rect(layout);
        let vertical = matches!(direction, SpatialDirection::Up | SpatialDirection::Down);
        let far_edge = matches!(direction, SpatialDirection::Up | SpatialDirection::Left);
        let center = |x, width| 2 * u32::from(x) + u32::from(width);
        let second = if vertical {
            center(from.x, from.width) > center(bounds.x, bounds.width)
        } else {
            center(from.y, from.height) > center(bounds.y, bounds.height)
        };

        // Prefer a populated box on the entry edge, then the column or row nearest the old window.
        let shown = self.dashboard_panels();
        let target = shown
            .iter()
            .copied()
            .filter(|panel| dashboard_rows(&self.ui, *panel) > 0)
            .filter_map(|panel| {
                let (column, row) = dashboard_cell(shown, panel)?;
                let (depth, across) = if vertical {
                    (row, column)
                } else {
                    (column, row)
                };
                let depth = if far_edge { usize::MAX - depth } else { depth };
                Some(((depth, across != usize::from(second)), panel))
            })
            .min_by_key(|(rank, _)| *rank);
        if let Some((_, panel)) = target {
            self.land_in_dashboard_box(panel, direction);
        }
    }

    fn land_in_dashboard_box(&mut self, panel: DashboardPanel, direction: SpatialDirection) {
        let row = if direction == SpatialDirection::Up {
            dashboard_rows(&self.ui, panel).saturating_sub(1)
        } else {
            0
        };
        self.ui.dashboard_focus = Some(panel);
        self.set_dashboard_row(panel, row);
    }

    fn set_dashboard_row(&mut self, panel: DashboardPanel, row: usize) {
        if panel == DashboardPanel::Start {
            self.ui.dashboard_selected = row;
        } else {
            self.ui.dashboard_row = row;
        }
    }
}
