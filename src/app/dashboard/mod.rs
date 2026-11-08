//! The dashboard: its visibility, what it shows about the workspace and opening its items.

use crate::app::App;
use crate::git::GitFileStatus;
use crate::persistence::PersistedLocation;
use crate::ui::DashboardChange;
use crate::ui::DashboardChanges;
use crate::ui::DashboardFile;
use crate::ui::DashboardPanel;
use crate::ui::DashboardProject;
use crate::ui::DashboardWorkspace;
use crate::ui::render::DASHBOARD_LIST_LIMIT;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc;

/// Changed files whose lines are counted at most.
const COUNTED_FILES_LIMIT: usize = 500;
/// Projects checked for existence and branch in one read.
const PROJECT_CANDIDATES_LIMIT: usize = 32;

/// What the dashboard reads on a worker thread, since each read opens a repository: line counts
/// for the changed files, the last commit, and each project's branch.
#[derive(Default)]
pub(in crate::app) struct Dashboard {
    /// What the last read was for: the workspace, its Git status revision and the projects.
    read_for: Option<(PathBuf, Option<u64>, Vec<PathBuf>)>,
    reading: Option<mpsc::Receiver<GitFacts>>,
    facts: GitFacts,
    /// A recent file in another project, opened once the switch to that project finishes.
    open_after_switch: Option<PathBuf>,
}

#[derive(Default)]
struct GitFacts {
    lines: BTreeMap<PathBuf, (usize, usize)>,
    last_commit: Option<(String, i64)>,
    /// What was found for each project the dashboard asked about.
    projects: Vec<ProjectFacts>,
}

struct ProjectFacts {
    path: PathBuf,
    /// Where the project really is, after symlinks; `None` when it no longer exists or is a
    /// tutor practice project, so it is not listed.
    canonical: Option<PathBuf>,
    branch: Option<String>,
}

fn read_git_facts(root: &Path, projects: &[PathBuf]) -> GitFacts {
    GitFacts {
        lines: crate::git::git_line_counts(root, COUNTED_FILES_LIMIT).unwrap_or_default(),
        last_commit: crate::git::git_last_commit(root),
        projects: projects
            .iter()
            .map(|project| {
                let canonical = project
                    .canonicalize()
                    .ok()
                    .filter(|canonical| canonical.is_dir())
                    .filter(|canonical| !crate::app::tutor::is_practice_project(canonical));
                let branch = canonical
                    .as_ref()
                    .and_then(|_| crate::git::git_branch_name(project).ok().flatten());
                ProjectFacts {
                    path: project.clone(),
                    canonical,
                    branch,
                }
            })
            .collect(),
    }
}

impl App {
    pub(in crate::app) fn set_dashboard_visibility(&mut self, visible: bool) {
        if visible {
            self.window_session.open_dashboard();
        } else {
            self.window_session.close_dashboard();
        }
        self.project_window_activation();
    }

    /// Gathers what the dashboard shows: the saved recent files, the last Git status, the
    /// workspaces Dust has opened, and what the worker last read about them.
    pub(in crate::app) fn sync_dashboard_ui(&mut self) {
        if !self.ui.dashboard && self.state.active_view().is_some() {
            return;
        }
        let root = self.state.workspace_root.clone();
        let seen = self.workspace.seen_roots();
        let now = self
            .clock
            .system_time()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let mut summary = DashboardWorkspace::default();
        // This workspace first, then projects in the order their files were last opened.
        let mut projects = vec![root.clone()];
        for file in self.persistence.recent_files() {
            let project = if file.path.starts_with(&root) {
                Some(&root)
            } else {
                project_of(&seen, &file.path)
            };
            if let Some(project) = project
                && !projects.contains(project)
            {
                projects.push(project.clone());
            }
            if summary.recent_files.len() < DASHBOARD_LIST_LIMIT {
                let label = project
                    .and_then(|project| file.path.strip_prefix(project).ok())
                    .unwrap_or(&file.path)
                    .display()
                    .to_string();
                summary.recent_files.push(DashboardFile {
                    path: file.path.clone(),
                    label,
                    project: project
                        .filter(|project| **project != root)
                        .map(|project| directory_name(project)),
                    age: file
                        .opened_at
                        .map(|opened| short_age(now.saturating_sub(opened)))
                        .unwrap_or_default(),
                });
            }
        }
        let mut others = seen
            .into_iter()
            .filter(|project| !projects.contains(project))
            .collect::<Vec<_>>();
        others.sort();
        projects.extend(others);
        projects.truncate(PROJECT_CANDIDATES_LIMIT);

        let status_revision = self.git.status().map(|status| status.revision);
        let read_for = (root.clone(), status_revision, projects.clone());
        if self.dashboard.read_for.as_ref() != Some(&read_for) {
            self.dashboard.read_for = Some(read_for);
            let (sender, receiver) = mpsc::channel();
            let (read_root, read_projects) = (root.clone(), projects.clone());
            let started = std::thread::Builder::new()
                .name("dust-dashboard".into())
                .spawn(move || {
                    let _ = sender.send(read_git_facts(&read_root, &read_projects));
                });
            self.dashboard.reading = started.is_ok().then_some(receiver);
        }
        if let Some(receiver) = &self.dashboard.reading {
            match receiver.try_recv() {
                Ok(facts) => {
                    self.dashboard.facts = facts;
                    self.dashboard.reading = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.dashboard.reading = None,
            }
        }
        let facts = &self.dashboard.facts;

        summary.changes = self.git.status().map(|status| {
            // Files that differ only in an unsaved buffer are left out; the disk is shown.
            let on_disk = status
                .entries
                .iter()
                .filter(|entry| entry.sources.staged || entry.sources.unstaged)
                .collect::<Vec<_>>();
            let untracked = on_disk
                .iter()
                .filter(|entry| entry.status == GitFileStatus::Untracked)
                .count();
            let mut files = on_disk
                .iter()
                .map(|entry| {
                    let path = status.scope_root.join(&entry.relative_path);
                    DashboardChange {
                        lines: facts.lines.get(&path).copied(),
                        label: entry.relative_path.display().to_string(),
                        code: change_code(&entry.status),
                        path,
                    }
                })
                .collect::<Vec<_>>();
            files.sort_by_key(|change| change.code == '?');
            files.truncate(DASHBOARD_LIST_LIMIT);
            DashboardChanges {
                files,
                changed: on_disk.len() - untracked,
                untracked,
            }
        });
        summary.last_commit = facts.last_commit.as_ref().map(|(subject, time)| {
            let made = u64::try_from(*time).unwrap_or(0);
            let age = match short_age(now.saturating_sub(made)) {
                age if age == "now" => "just now".to_owned(),
                age => format!("{age} ago"),
            };
            (subject.clone(), age)
        });
        // Each folder once, however it was reached; until the worker has looked, only this
        // workspace is listed.
        let mut listed = Vec::new();
        summary.projects = projects
            .iter()
            .filter_map(|project| {
                let read = facts.projects.iter().find(|read| read.path == *project);
                let canonical = match read {
                    Some(read) => read.canonical.clone()?,
                    None if *project == root => project.clone(),
                    None => return None,
                };
                if listed.contains(&canonical) {
                    return None;
                }
                listed.push(canonical);
                let branch = if *project == root {
                    self.ui.git_branch.clone()
                } else {
                    None
                };
                Some(DashboardProject {
                    path: project.clone(),
                    name: directory_name(project),
                    branch: branch.or_else(|| read.and_then(|read| read.branch.clone())),
                })
            })
            .take(DASHBOARD_LIST_LIMIT)
            .collect();
        self.ui.set(summary, |ui| &mut ui.dashboard_workspace);
    }

    /// The boxes the dashboard shows at its current size.
    pub(in crate::app) fn dashboard_panels(&self) -> &'static [DashboardPanel] {
        let (width, height) = crate::ui::render::dashboard_size(&self.ui)
            .unwrap_or((self.window_session.width(), self.window_session.height()));
        crate::ui::render::dashboard_panels(width, height)
    }

    /// Opens the `index`th recent file, first switching to its project when that is not open.
    pub(in crate::app) fn open_dashboard_recent_file(&mut self, index: usize) {
        let Some(file) = self.ui.dashboard_workspace.recent_files.get(index) else {
            return;
        };
        let path = file.path.clone();
        let open_here = self
            .workspace
            .roots()
            .iter()
            .any(|root| path.starts_with(&root.identity.canonical_path));
        if !open_here && let Some(project) = project_of(&self.workspace.seen_roots(), &path) {
            let project = project.clone();
            self.dashboard.open_after_switch = Some(path);
            self.switch_workspace_root(project);
            return;
        }
        self.open_dashboard_file(path);
    }

    pub(in crate::app) fn open_dashboard_file(&mut self, path: PathBuf) {
        self.navigate_persisted_location(PersistedLocation {
            path,
            byte: 0,
            anchor: 0,
            scroll_line: 0,
            scroll_column: 0,
        });
    }

    /// The recent file waiting for a switch to `root` to finish, if there is one.
    pub(in crate::app) fn take_dashboard_file_after_switch(
        &mut self,
        root: &Path,
    ) -> Option<PathBuf> {
        self.dashboard
            .open_after_switch
            .take()
            .filter(|file| file.starts_with(root))
    }
}

/// The innermost project `path` is in.
fn project_of<'a>(projects: &'a [PathBuf], path: &Path) -> Option<&'a PathBuf> {
    projects
        .iter()
        .filter(|project| path.starts_with(project))
        .max_by_key(|project| project.as_os_str().len())
}

fn directory_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn change_code(status: &GitFileStatus) -> char {
    match status {
        GitFileStatus::Untracked => '?',
        GitFileStatus::Added => 'A',
        GitFileStatus::Modified => 'M',
        GitFileStatus::TypeChanged => 'T',
        GitFileStatus::Deleted => 'D',
        GitFileStatus::Renamed => 'R',
        GitFileStatus::Conflicted => 'U',
    }
}

/// A duration in seconds, shortly: `now`, `14m`, `3h`, `2d`, `5w`, `8mo` or `2y`.
fn short_age(seconds: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    match seconds {
        seconds if seconds < MINUTE => "now".to_owned(),
        seconds if seconds < HOUR => format!("{}m", seconds / MINUTE),
        seconds if seconds < DAY => format!("{}h", seconds / HOUR),
        seconds if seconds < 14 * DAY => format!("{}d", seconds / DAY),
        seconds if seconds < 60 * DAY => format!("{}w", seconds / (7 * DAY)),
        seconds if seconds < 365 * DAY => format!("{}mo", seconds / (30 * DAY)),
        seconds => format!("{}y", seconds / (365 * DAY)),
    }
}

mod commands;

#[cfg(test)]
pub(in crate::app) mod tests;
