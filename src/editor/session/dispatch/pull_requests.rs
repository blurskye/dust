//! Pull request review through `gh`: pick a pull request to check out into its own worktree,
//! list its review comments, and comment on the current line. Viewing, diffing and submitting
//! a review run `gh` in a terminal float (see the `gh_pr_*` terminal profiles).

use std::path::PathBuf;

use super::InteractiveApplication;
use crate::{
    editor::session::search::PickerPayload,
    forge::{ForgeJobs, ForgeReply},
    language::lsp::{LspLocation, LspPosition, LspRange, file_uri},
    ui::PromptKind,
    workspace::{PickerAction, PickerItem, PickerItemId, PickerItemKind},
};

#[derive(Default)]
pub(super) struct PullRequests {
    jobs: ForgeJobs,
    /// The file and 1-based line the open comment prompt is about.
    comment_target: Option<(PathBuf, usize)>,
}

impl InteractiveApplication {
    pub(super) fn list_pull_requests(&mut self) {
        self.pull_requests.jobs.list(self.state.workspace_root.clone());
        self.notify_message("loading pull requests…".into(), false);
    }

    pub(super) fn list_review_comments(&mut self) {
        self.pull_requests
            .jobs
            .comments(self.state.workspace_root.clone());
        self.notify_message("loading review comments…".into(), false);
    }

    pub(super) fn begin_review_comment(&mut self) {
        let (Some(view), Some(buffer)) = (self.state.active_view, self.state.active_buffer_id())
        else {
            self.show_error("no file to comment on");
            return;
        };
        let Some(path) = self.state.buffers[buffer].path.clone() else {
            self.show_error("the current buffer has no file");
            return;
        };
        let line = self.state.buffers[buffer]
            .text
            .byte_to_line(self.state.views[view].cursor)
            .unwrap_or(0)
            + 1;
        self.pull_requests.comment_target = Some((path, line));
        self.open_prompt(PromptKind::ReviewComment, String::new());
    }

    pub(super) fn submit_review_comment(&mut self, body: String) {
        let Some((path, line)) = self.pull_requests.comment_target.take() else {
            return;
        };
        if body.trim().is_empty() {
            return;
        }
        self.pull_requests
            .jobs
            .comment(self.state.workspace_root.clone(), path, line, body);
        self.notify_message("sending comment…".into(), false);
    }

    pub(super) fn checkout_pull_request(&mut self, number: u64) {
        self.pull_requests
            .jobs
            .checkout(self.state.workspace_root.clone(), number);
        self.notify_message(format!("checking out #{number}…"), false);
    }

    pub(super) fn process_pull_request_events(&mut self) {
        while let Some(reply) = self.pull_requests.jobs.try_reply() {
            // The "loading…" notice is answered by whatever this reply shows.
            self.clear_notice();
            match reply {
                ForgeReply::List(Err(error))
                | ForgeReply::CheckedOut(Err(error))
                | ForgeReply::Comments(Err(error))
                | ForgeReply::Commented(Err(error)) => self.show_error(format!("gh: {error}")),
                ForgeReply::List(Ok(list)) if list.is_empty() => {
                    self.notify_message("no open pull requests".into(), false);
                }
                ForgeReply::List(Ok(list)) => {
                    let items = list
                        .into_iter()
                        .map(|pr| {
                            (
                                PickerItem {
                                    badge: if pr.draft { "draft".into() } else { String::new() },
                                    kind: PickerItemKind::Entry,
                                    id: PickerItemId(format!("pr:{}", pr.number)),
                                    label: format!("#{} {}", pr.number, pr.title),
                                    detail: format!("@{} · {}", pr.author, pr.branch),
                                    source: String::new(),
                                    action: PickerAction::None,
                                },
                                PickerPayload::PullRequest(pr.number),
                            )
                        })
                        .collect();
                    self.install_picker("Pull Requests".to_owned(), items);
                    self.search
                        .set_picker_footer("Enter Check out in a worktree · Esc Close".to_owned());
                    self.sync_picker_ui();
                }
                ForgeReply::CheckedOut(Ok(path)) => self.switch_workspace_root(path),
                ForgeReply::Comments(Ok(comments)) if comments.is_empty() => {
                    self.notify_message("no review comments".into(), false);
                }
                ForgeReply::Comments(Ok(comments)) => {
                    let root = self.state.workspace_root.clone();
                    let items = comments
                        .into_iter()
                        .enumerate()
                        .map(|(index, comment)| {
                            let line = comment.line.unwrap_or(1).max(1) as usize;
                            let shown = comment.path.strip_prefix(&root).unwrap_or(&comment.path);
                            let position = LspPosition {
                                line: line - 1,
                                character: 0,
                            };
                            (
                                PickerItem {
                                    badge: if comment.line.is_none() {
                                        "outdated".into()
                                    } else {
                                        String::new()
                                    },
                                    kind: PickerItemKind::Entry,
                                    id: PickerItemId(format!("review:{index}")),
                                    label: comment.body.lines().next().unwrap_or_default().to_owned(),
                                    detail: format!(
                                        "{}:{line} · @{}",
                                        shown.display(),
                                        comment.author
                                    ),
                                    source: String::new(),
                                    action: PickerAction::None,
                                },
                                PickerPayload::LspLocation(LspLocation {
                                    uri: file_uri(&comment.path),
                                    range: LspRange {
                                        start: position.clone(),
                                        end: position,
                                    },
                                }),
                            )
                        })
                        .collect();
                    self.install_picker("Review Comments".to_owned(), items);
                    self.search
                        .set_picker_footer("Enter Go to line · Esc Close".to_owned());
                    self.sync_picker_ui();
                }
                ForgeReply::Commented(Ok(())) => {
                    self.notify_message("comment sent".into(), false);
                }
            }
        }
    }
}
