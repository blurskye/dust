//! Command registration and execution for git actions and accepted repository results.

use crate::app::commands::AppEffect;
use crate::app::commands::optional_string_arg;
use crate::app::commands::register;
use crate::app::commands::string_arg;
use crate::editor::ArgumentSchema;
use crate::editor::ArgumentType;
use crate::editor::CommandError;
use crate::editor::CommandRegistry;
use crate::editor::ContextRequirement;

pub(in crate::app) fn register_commands(
    registry: &mut CommandRegistry<AppEffect>,
) -> Result<(), CommandError> {
    register(
        registry,
        "workspace.git.pr.list",
        "Pick a pull request and check it out in its own worktree",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::PullRequestList,
    )?;
    register(
        registry,
        "workspace.git.pr.comments",
        "List the review comments of this branch's pull request",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::PullRequestComments,
    )?;
    register(
        registry,
        "workspace.git.pr.comment",
        "Comment on the current line in this branch's pull request",
        ContextRequirement::View,
        ArgumentSchema::default(),
        |_| AppEffect::PullRequestComment,
    )?;
    register(
        registry,
        "workspace.git.status.refresh",
        "Refresh Git status",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitStatusRefresh,
    )?;
    register(
        registry,
        "workspace.git.hunk.next",
        "Go to next Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkNavigate(1),
    )?;
    register(
        registry,
        "workspace.git.hunk.previous",
        "Go to previous Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkNavigate(-1),
    )?;
    register(
        registry,
        "workspace.git.conflict.next",
        "Go to next conflict",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |_| AppEffect::GitConflictNavigate(1),
    )?;
    register(
        registry,
        "workspace.git.conflict.previous",
        "Go to previous conflict",
        ContextRequirement::Buffer,
        ArgumentSchema::default(),
        |_| AppEffect::GitConflictNavigate(-1),
    )?;
    register(
        registry,
        "workspace.git.hunk.preview",
        "Preview selected Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkPreview,
    )?;
    register(
        registry,
        "workspace.git.hunk.reset",
        "Reset selected Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkReset,
    )?;
    register(
        registry,
        "workspace.git.hunk.stage",
        "Stage selected Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkStage,
    )?;
    register(
        registry,
        "workspace.git.worktrees.open",
        "Open the Git worktree list",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitWorktreesOpen,
    )?;
    register(
        registry,
        "workspace.git.worktree.remove",
        "Remove the selected worktree and its directory",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitWorktreeRemove,
    )?;
    register(
        registry,
        "workspace.git.worktree.create",
        "New Worktree: check out a branch in its own directory",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("branch", ArgumentType::String, true)
            .field("path", ArgumentType::String, false),
        |invocation| {
            Ok(AppEffect::GitWorktreeCreate {
                branch: string_arg(invocation, "branch")?.into(),
                path: optional_string_arg(invocation, "path").map(std::path::PathBuf::from),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.worktree.open",
        "Switch the editor to the selected worktree",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitWorktreeOpen),
    )?;
    register(
        registry,
        "workspace.git.conflicts.open",
        "Show the paused operation and each conflicted path's base, ours and theirs",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitConflictsOpen),
    )?;
    register(
        registry,
        "workspace.git.conflict.choose",
        "Accept one side of the active conflict into the result buffer",
        ContextRequirement::Workspace,
        ArgumentSchema::default().field("side", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::GitConflictChoose {
                side: string_arg(invocation, "side")?.to_owned(),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.conflict.accept",
        "Accept one side of the conflict block at the cursor",
        ContextRequirement::Buffer,
        ArgumentSchema::default().field("side", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::GitConflictAccept {
                side: string_arg(invocation, "side")?.to_owned(),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.conflict.stage",
        "Stage the resolved conflict in the active buffer",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitConflictStage),
    )?;
    register(
        registry,
        "workspace.git.conflict.continue",
        "Finish the paused operation by recording its merge commit",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitConflictContinue),
    )?;
    register(
        registry,
        "workspace.git.conflict.abort",
        "Abandon the paused operation and return to where it started",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_invocation| Ok(AppEffect::GitConflictAbort),
    )?;
    register(
        registry,
        "workspace.git.conflict.replace",
        "Preview replacing whole files with one side, then confirm it",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("side", ArgumentType::String, true)
            .field("files", ArgumentType::String, true),
        |invocation| {
            Ok(AppEffect::GitConflictReplace {
                side: string_arg(invocation, "side")?.to_owned(),
                files: string_arg(invocation, "files")?.to_owned(),
            })
        },
    )?;
    register(
        registry,
        "workspace.git.hunks.open_staged",
        "Open staged Git hunks",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunksOpenStaged,
    )?;
    register(
        registry,
        "workspace.git.hunk.unstage",
        "Unstage selected staged Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunkUnstage,
    )?;
    register(
        registry,
        "workspace.git.file.stage",
        "Stage the whole active file",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitFileStage,
    )?;
    register(
        registry,
        "workspace.git.file.unstage",
        "Unstage the whole active file",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitFileUnstage,
    )?;
    register(
        registry,
        "workspace.git.lines.stage",
        "Stage a line range inside the current Git hunk",
        ContextRequirement::Workspace,
        ArgumentSchema::default()
            .field("start", ArgumentType::Integer, false)
            .field("end", ArgumentType::Integer, false),
        |invocation| {
            let integer = |name: &str| match invocation.arguments.get(name) {
                Some(crate::editor::CommandValue::Integer(value)) => *value,
                _ => -1,
            };
            AppEffect::GitLinesStage {
                start: integer("start"),
                end: integer("end"),
            }
        },
    )?;
    register(
        registry,
        "workspace.git.hunks.open",
        "Open Git hunks",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitHunksOpen,
    )?;
    register(
        registry,
        "workspace.git.status.open",
        "Open Git status",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitStatusOpen,
    )?;
    register(
        registry,
        "workspace.git.browse",
        "Open the current line on the forge",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitBrowse { copy: false },
    )?;
    register(
        registry,
        "workspace.git.browse.copy",
        "Copy the forge URL of the current line",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitBrowse { copy: true },
    )?;
    register(
        registry,
        "workspace.git.diff.layer.switch",
        "Switch the diff between the unstaged and staged comparison",
        ContextRequirement::Workspace,
        ArgumentSchema::default(),
        |_| AppEffect::GitChangesLayerSwitch,
    )?;
    Ok(())
}

use crate::app::App;
use crate::workspace::WorkspaceTrust;

impl App {
    pub(in crate::app) fn process_git_effect(
        &mut self,
        effect: AppEffect,
    ) -> Result<(), AppEffect> {
        match effect {
            AppEffect::GitFileStage => {
                self.input.set_pending_stage(AppEffect::GitFileStage);
                // The plan itself refuses a path whose buffer has unsaved edits and reports
                // `UnsavedDocument`, which the mutation handler turns into the explicit
                // save-and-stage choice. The surface only has to name the right file.
                let file = self.changes_focused_relative();
                if let Err(error) = self.git.start_plan_for(
                    &self.state,
                    crate::git::GitMutationKind::StageFile,
                    self.workspace.root_revision(),
                    self.workspace.trust() == WorkspaceTrust::Trusted,
                    Vec::new(),
                    file,
                ) {
                    self.input.clear_pending_stage();
                    self.show_error(error);
                }
            }
            AppEffect::GitFileUnstage => {
                self.input.set_pending_stage(AppEffect::GitFileUnstage);
                let file = self.changes_focused_relative();
                if let Err(error) = self.git.start_plan_for(
                    &self.state,
                    crate::git::GitMutationKind::UnstageFile,
                    self.workspace.root_revision(),
                    self.workspace.trust() == WorkspaceTrust::Trusted,
                    Vec::new(),
                    file,
                ) {
                    self.input.clear_pending_stage();
                    self.show_error(error);
                }
            }
            AppEffect::GitLinesStage { start, end } => self.stage_git_lines(start, end),
            AppEffect::GitWorktreeCreate { branch, path } => {
                self.create_git_worktree(branch, path);
            }
            AppEffect::GitConflictsOpen => self.open_git_conflicts(),
            AppEffect::GitConflictAccept { side } => self.accept_conflict_block(&side),
            AppEffect::GitConflictChoose { side } => self.choose_git_conflict_side(&side),
            AppEffect::GitConflictStage => self.stage_git_conflict(),
            AppEffect::GitConflictContinue => self.continue_git_conflict(),
            AppEffect::GitConflictAbort => self.abort_git_conflict(),
            AppEffect::GitConflictReplace { side, files } => {
                self.preview_git_replacement(&side, &files);
            }
            AppEffect::GitWorktreeOpen => self.open_git_worktree(),
            AppEffect::GitWorktreeRemove => self.request_git_worktree_remove(),
            AppEffect::GitWorktreesOpen => self.start_git_worktrees(),
            AppEffect::GitHunksOpenStaged => {
                self.start_git_hunks_with_layer(None, crate::git::GitDiffLayer::Staged);
            }
            AppEffect::GitHunkUnstage => self.unstage_selected_git_hunk(),
            AppEffect::GitStatusRefresh => self.start_git_status_refresh(),
            AppEffect::GitStatusOpen => self.open_git_status_picker(),
            AppEffect::GitBrowse { copy } => self.browse_git_line(copy),
            AppEffect::PullRequestList => self.list_pull_requests(),
            AppEffect::PullRequestComments => self.list_review_comments(),
            AppEffect::PullRequestComment => self.begin_review_comment(),
            AppEffect::GitChangesLayerSwitch => {
                // Only the change surface has a layer to switch; elsewhere this does nothing
                // rather than reporting an error for a key that has no meaning there.
                if self.search.switch_changes_layer().is_some() {
                    self.sync_picker_ui();
                }
            }
            AppEffect::GitHunksOpen => self.start_git_hunks(),
            AppEffect::GitConflictNavigate(direction) => self.navigate_git_conflict(direction),
            AppEffect::GitHunkNavigate(direction) => self.navigate_git_hunk(direction),
            AppEffect::GitHunkPreview => self.preview_selected_git_hunk(),
            AppEffect::GitHunkStage => {
                self.input.set_pending_stage(AppEffect::GitHunkStage);
                self.stage_selected_git_hunk();
            }
            AppEffect::GitHunkReset => self.request_reset_selected_git_hunk(),
            effect => return Err(effect),
        }
        Ok(())
    }
}
