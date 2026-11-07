//! Hunk actions in the changed-files picker, using its displayed Git snapshot.

use crate::app::App;
use crate::git::{GitDiffLayer, GitHunk, GitHunkSnapshot, GitMutationKind};
use crate::ui::PromptKind;
use crate::workspace::WorkspaceTrust;

impl App {
    fn git_review_hunk(&self) -> Result<(&GitHunkSnapshot, &GitHunk), String> {
        let file = self
            .changes_focused_relative()
            .ok_or("select a changed file first")?;
        let snapshot = self
            .search
            .diff_snapshot
            .as_ref()
            .ok_or("wait for the diff to load")?;
        let status = self.git.status().ok_or("Git status is not ready")?;
        if snapshot.file != file
            || snapshot.layer != self.search.changes_layer()
            || snapshot.status_revision != status.revision
            || snapshot.repository_id != status.repository_id
            || snapshot.root_revision != self.workspace.root_revision()
        {
            return Err("Git diff changed; wait for it to refresh".into());
        }
        let hunk = snapshot
            .hunks
            .get(self.search.diff_hunk)
            .ok_or("this file has no text hunks")?;
        Ok((snapshot, hunk))
    }

    pub(in crate::app) fn git_review_line(&self) -> Option<usize> {
        let (_, hunk) = self.git_review_hunk().ok()?;
        Some(
            hunk.change_lines()
                .first()
                .copied()
                .unwrap_or(hunk.new_start)
                .saturating_sub(1) as usize,
        )
    }

    pub(super) fn navigate_git_review_hunk(&mut self, direction: i8) {
        self.search.move_diff_hunk(direction);
        self.sync_picker_ui();
    }

    fn select_git_review_hunk(
        &mut self,
        layer: GitDiffLayer,
    ) -> Result<std::path::PathBuf, String> {
        let (snapshot, hunk) = self.git_review_hunk()?;
        if snapshot.layer != layer {
            return Err(match layer {
                GitDiffLayer::Staged => "select a staged hunk (Tab switches layers)",
                GitDiffLayer::Unstaged => "select an unstaged hunk (Tab switches layers)",
            }
            .into());
        }
        let file = snapshot.file.clone();
        let path = self
            .git
            .status()
            .ok_or("Git status is not ready")?
            .scope_root
            .join(&file);
        // These are the exact bytes the user reviewed, not a new diff at the buffer cursor.
        self.git.selected = Some((snapshot.clone(), hunk.clone(), path));
        self.git.selected_change.clear();
        Ok(file)
    }

    pub(super) fn stage_git_review_hunk(&mut self, unstage: bool) {
        let layer = if unstage {
            GitDiffLayer::Staged
        } else {
            GitDiffLayer::Unstaged
        };
        let result = self.select_git_review_hunk(layer).and_then(|file| {
            let path = self
                .git
                .status()
                .ok_or("Git status is not ready")?
                .scope_root
                .join(&file);
            if self
                .state
                .buffers
                .values()
                .any(|buffer| buffer.dirty && buffer.path.as_ref() == Some(&path))
            {
                return Err(
                    "save or close the unsaved buffer before staging its review hunk".into(),
                );
            }
            self.git.start_plan_for(
                &self.state,
                if unstage {
                    GitMutationKind::UnstageHunk
                } else {
                    GitMutationKind::StageHunk
                },
                self.workspace.root_revision(),
                self.workspace.trust() == WorkspaceTrust::Trusted,
                Vec::new(),
                Some(file),
            )
        });
        if let Err(error) = result {
            self.show_error(error);
        }
    }

    pub(super) fn request_reset_git_review_hunk(&mut self) {
        if self.workspace.trust() != WorkspaceTrust::Trusted {
            self.show_error("Git reset requires a trusted workspace");
            return;
        }
        match self.select_git_review_hunk(GitDiffLayer::Unstaged) {
            Ok(file) => {
                self.git.remember_reset_target();
                self.open_prompt(PromptKind::GitReset, String::new());
                self.prompt_help(format!(
                    "Discard hunk {} in {}. Type `reset this hunk`.",
                    self.search.diff_hunk + 1,
                    file.display()
                ));
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(super) fn confirm_reset_git_review_hunk(&mut self, phrase: String) {
        let same_target = self.git.take_reset_target();
        let result = self
            .git_review_hunk()
            .and_then(|(snapshot, hunk)| {
                let matches_selection =
                    self.git
                        .selected
                        .as_ref()
                        .is_some_and(|(selected, target, _)| {
                            selected.revision == snapshot.revision && target.id == hunk.id
                        });
                if !same_target || !matches_selection {
                    return Err("Git hunk target changed".into());
                }
                if phrase != "reset this hunk" {
                    return Err("Git hunk reset confirmation did not match".into());
                }
                Ok(snapshot.file.clone())
            })
            .and_then(|file| {
                self.git.mutate_for(
                    &self.state,
                    true,
                    self.workspace.trust() == WorkspaceTrust::Trusted,
                    self.workspace.root_revision(),
                    Some(file),
                )
            });
        if let Err(error) = result {
            self.show_error(error);
        }
    }
}
