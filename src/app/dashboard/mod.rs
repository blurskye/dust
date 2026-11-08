//! Dashboard actions, visibility and the workspace summary it shows.

use crate::app::App;
use crate::git::GitFileStatus;
use crate::ui::DashboardWorkspace;
use crate::ui::render::DASHBOARD_LIST_LIMIT;

impl App {
    pub(in crate::app) fn set_dashboard_visibility(&mut self, visible: bool) {
        if visible {
            self.window_session.open_dashboard();
        } else {
            self.window_session.close_dashboard();
        }
        self.project_window_activation();
    }

    /// Gathers what the dashboard shows from state the editor already keeps: the saved recent
    /// files, the workspaces Dust has opened and the last Git status. Nothing is read from disk.
    pub(in crate::app) fn sync_dashboard_ui(&mut self) {
        if !self.ui.dashboard && self.state.active_view().is_some() {
            return;
        }
        let root = &self.state.workspace_root;
        let seen = self.workspace.seen_roots();
        let mut summary = DashboardWorkspace::default();
        for path in self.persistence.recent_files() {
            if let Ok(relative) = path.strip_prefix(root) {
                if summary.recent_files.len() < DASHBOARD_LIST_LIMIT {
                    summary.recent_files.push(relative.display().to_string());
                }
            } else if let Some(project) = seen
                .iter()
                .filter(|project| path.starts_with(project))
                .max_by_key(|project| project.as_os_str().len())
                && summary.recent_projects.len() < DASHBOARD_LIST_LIMIT
                && !summary.recent_projects.contains(project)
            {
                summary.recent_projects.push(project.clone());
            }
        }
        summary.git_changes = self.git.status().map(|status| {
            // Files that differ only in an unsaved buffer are not counted; the disk is.
            let on_disk = status
                .entries
                .iter()
                .filter(|entry| entry.sources.staged || entry.sources.unstaged);
            let untracked = on_disk
                .clone()
                .filter(|entry| entry.status == GitFileStatus::Untracked)
                .count();
            (on_disk.count() - untracked, untracked)
        });
        self.ui.set(summary, |ui| &mut ui.dashboard_workspace);
    }
}

mod commands;

#[cfg(test)]
pub(in crate::app) mod tests;
