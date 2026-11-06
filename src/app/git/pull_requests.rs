//! Pull request review through the forge CLI (`gh` or `tea`): pick a pull request to check out
//! into its own worktree, list its review comments, and comment on the current line. Viewing,
//! diffing and submitting a GitHub review run `gh` in a terminal float (the `gh_pr_*` profiles).

use std::path::PathBuf;

use crate::app::App;
use crate::app::picker::PickerPayload;
use crate::forge::ForgeJobs;
use crate::forge::ForgeReply;
use crate::forge::ReviewCommentTarget;
use crate::language::lsp::LspLocation;
use crate::language::lsp::LspPosition;
use crate::language::lsp::LspRange;
use crate::language::lsp::file_uri;
use crate::ui::PromptKind;
use crate::workspace::PickerAction;
use crate::workspace::PickerItem;
use crate::workspace::PickerItemId;
use crate::workspace::PickerItemKind;

#[derive(Default)]
pub(in crate::app) struct PullRequests {
    jobs: ForgeJobs,
    source: Option<crate::app::workspace::WorkspaceVersion>,
    request_id: u64,
    comment_body: Option<String>,
    comment_sending: bool,
    comment_loading: bool,
    comment_target: Option<ReviewCommentTarget>,
}

impl PullRequests {
    pub(in crate::app) fn prepare(&mut self, source: crate::app::workspace::WorkspaceVersion) {
        if self.source.as_ref() != Some(&source) {
            self.jobs.cancel();
            self.request_id += 1;
            self.source = Some(source);
            self.comment_target = None;
            self.comment_body = None;
            self.comment_sending = false;
            self.comment_loading = false;
        }
    }
}

impl App {
    fn pull_request_request_id(&mut self) -> u64 {
        let source = self.workspace.version();
        self.pull_requests.prepare(source);
        self.pull_requests.request_id
    }

    fn pull_request_is_current(&self, root: &std::path::Path, request_id: u64) -> bool {
        root == self.state.workspace_root
            && request_id == self.pull_requests.request_id
            && self.pull_requests.source.as_ref() == Some(&self.workspace.version())
    }

    pub(in crate::app) fn list_pull_requests(&mut self) {
        let request_id = self.pull_request_request_id();
        match self
            .pull_requests
            .jobs
            .list(self.state.workspace_root.clone(), request_id)
        {
            Ok(()) => self.notify_message("loading pull requests…".into(), false),
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::app) fn list_review_comments(&mut self) {
        self.pull_requests.prepare(self.workspace.version());
        match self.pull_requests.jobs.comments(
            self.state.workspace_root.clone(),
            self.pull_requests.request_id,
        ) {
            Ok(()) => self.notify_message("loading review comments…".into(), false),
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::app) fn begin_review_comment(&mut self) {
        self.pull_requests.prepare(self.workspace.version());
        if self.pull_requests.comment_sending || self.pull_requests.comment_loading {
            self.notify_message("a review comment request is still running".into(), false);
            return;
        }
        if let Some(body) = self.pull_requests.comment_body.clone() {
            self.open_prompt(PromptKind::ReviewComment, body);
            return;
        }
        let (Some(view), Some(buffer)) = (self.state.active_view(), self.state.active_buffer_id())
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
        match self.pull_requests.jobs.comment_target(
            self.state.workspace_root.clone(),
            self.pull_requests.request_id,
            path,
            line,
            self.state.buffers[buffer].text.to_string(),
        ) {
            Ok(()) => {
                self.pull_requests.comment_loading = true;
                self.notify_message("loading pull request for review comment…".into(), false);
            }
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::app) fn submit_review_comment(&mut self, body: String) {
        if self.pull_requests.comment_sending {
            return;
        }
        let Some(target) = self.pull_requests.comment_target.clone() else {
            return;
        };
        if body.trim().is_empty() {
            return;
        }
        if self.pull_requests.source.as_ref() != Some(&self.workspace.version()) {
            self.show_error("the comment belongs to a previous workspace");
            return;
        }
        self.pull_requests.comment_body = Some(body.clone());
        match self.pull_requests.jobs.comment(
            self.state.workspace_root.clone(),
            self.pull_requests.request_id,
            target,
            body,
        ) {
            Ok(()) => {
                self.pull_requests.comment_sending = true;
                self.notify_message("sending comment…".into(), false);
            }
            Err(error) => {
                if let Some(body) = self.pull_requests.comment_body.clone() {
                    self.open_prompt(PromptKind::ReviewComment, body);
                }
                self.show_error(error);
            }
        }
    }

    pub(in crate::app) fn checkout_pull_request(
        &mut self,
        root: PathBuf,
        request_id: u64,
        number: u64,
        local: Option<PathBuf>,
    ) {
        if !self.pull_request_is_current(&root, request_id) {
            return;
        }
        if let Some(path) = local {
            self.switch_workspace_root(path);
            return;
        }
        if let Err(failure) = self.workspace.execution_allowed() {
            self.show_workspace_failure(failure);
            return;
        }
        match self.pull_requests.jobs.checkout(
            root,
            request_id,
            number,
            self.workspace.edit_scope().clone(),
        ) {
            Ok(()) => self.notify_message(format!("checking out #{number}…"), false),
            Err(error) => self.show_error(error),
        }
    }

    pub(in crate::app) fn process_pull_request_events(&mut self) {
        self.pull_requests.prepare(self.workspace.version());
        while let Some(event) = self.pull_requests.jobs.try_reply() {
            if !self.pull_request_is_current(&event.root, event.request_id) {
                continue;
            }
            let root = event.root;
            let request_id = event.request_id;
            // The "loading…" notice is answered by whatever this reply shows.
            self.clear_notice();
            match event.reply {
                ForgeReply::List(Err(error))
                | ForgeReply::CheckedOut(Err(error))
                | ForgeReply::Comments(Err(error)) => self.show_error(error),
                ForgeReply::CommentTarget(Err(error)) => {
                    self.pull_requests.comment_loading = false;
                    self.show_error(error);
                }
                ForgeReply::CommentTarget(Ok(target)) => {
                    self.pull_requests.comment_loading = false;
                    self.pull_requests.comment_target = Some(target);
                    self.pull_requests.comment_body = Some(String::new());
                    if self.input.prompt().is_none() {
                        self.open_prompt(PromptKind::ReviewComment, String::new());
                    } else {
                        self.notify_message(
                            "review comment ready; open it again when finished with this prompt"
                                .into(),
                            false,
                        );
                    }
                }
                ForgeReply::Commented(Err(error)) => {
                    self.pull_requests.comment_sending = false;
                    if self.input.prompt().is_none()
                        && let Some(body) = self.pull_requests.comment_body.clone()
                    {
                        self.open_prompt(PromptKind::ReviewComment, body);
                    }
                    self.show_error(error);
                }
                ForgeReply::Offline { error, local } if local.is_empty() => {
                    self.notify_warning(format!("pull requests unavailable: {error}"));
                }
                ForgeReply::Offline { error, local } => {
                    let items = local
                        .into_iter()
                        .map(|(number, path)| {
                            (
                                PickerItem {
                                    badge: "local".into(),
                                    kind: PickerItemKind::Entry,
                                    id: PickerItemId(format!("pr:{number}")),
                                    label: format!("#{number}"),
                                    detail: path.display().to_string(),
                                    source: String::new(),
                                    action: PickerAction::None,
                                },
                                PickerPayload::PullRequest {
                                    root: root.clone(),
                                    request_id,
                                    number,
                                    local: Some(path),
                                },
                            )
                        })
                        .collect();
                    self.install_picker("Pull Requests (local)".to_owned(), items);
                    self.search
                        .set_picker_footer("Enter Open the worktree · Esc Esc Close".to_owned());
                    self.sync_picker_ui();
                    self.notify_warning(format!("{error} · showing local checkouts"));
                }
                ForgeReply::List(Ok(list)) if list.is_empty() => {
                    self.notify_message("no open pull requests".into(), false);
                }
                ForgeReply::List(Ok(list)) => {
                    let items = list
                        .into_iter()
                        .map(|pr| {
                            (
                                PickerItem {
                                    badge: if pr.draft {
                                        "draft".into()
                                    } else {
                                        String::new()
                                    },
                                    kind: PickerItemKind::Entry,
                                    id: PickerItemId(format!("pr:{}", pr.number)),
                                    label: format!("#{} {}", pr.number, pr.title),
                                    detail: format!("@{} · {}", pr.author, pr.branch),
                                    source: String::new(),
                                    action: PickerAction::None,
                                },
                                PickerPayload::PullRequest {
                                    root: root.clone(),
                                    request_id,
                                    number: pr.number,
                                    local: None,
                                },
                            )
                        })
                        .collect();
                    self.install_picker("Pull Requests".to_owned(), items);
                    self.search.set_picker_footer(
                        "Enter Check out in a worktree · Esc Esc Close".to_owned(),
                    );
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
                                    label: comment
                                        .body
                                        .lines()
                                        .next()
                                        .unwrap_or_default()
                                        .to_owned(),
                                    detail: format!(
                                        "{}:{line} · @{}",
                                        shown.display(),
                                        comment.author
                                    ),
                                    source: String::new(),
                                    action: PickerAction::None,
                                },
                                PickerPayload::LspLocation(
                                    LspLocation {
                                        uri: file_uri(&comment.path),
                                        range: LspRange {
                                            start: position.clone(),
                                            end: position,
                                        },
                                    },
                                    crate::editor::text::PositionEncoding::Utf16,
                                ),
                            )
                        })
                        .collect();
                    self.install_picker("Review Comments".to_owned(), items);
                    self.search
                        .set_picker_footer("Enter Go to line · Esc Esc Close".to_owned());
                    self.sync_picker_ui();
                }
                ForgeReply::Commented(Ok(())) => {
                    self.pull_requests.comment_sending = false;
                    self.pull_requests.comment_target = None;
                    self.pull_requests.comment_body = None;
                    self.notify_message("comment sent".into(), false);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::git::pull_requests::*;

    pub(in crate::app) fn app() -> App {
        App::new(
            crate::app::startup::StartupTarget::Dashboard {
                root: tempfile::tempdir().unwrap().keep(),
            },
            crate::config::load_bundled_config(),
            false,
            tempfile::tempdir().unwrap().keep(),
            false,
            false,
            std::sync::Arc::new(crate::clock::SystemClock),
        )
        .unwrap()
    }

    #[test]
    fn checkout_requires_trust_and_respects_safe_mode() {
        let mut app = app();
        let request_id = app.pull_request_request_id();
        app.checkout_pull_request(app.state.workspace_root.clone(), request_id, 1, None);
        assert!(app.pull_requests.jobs.try_reply().is_none());
        app.workspace.fixture_safe_mode(true);
        let request_id = app.pull_request_request_id();
        app.checkout_pull_request(app.state.workspace_root.clone(), request_id, 1, None);
        assert!(app.pull_requests.jobs.try_reply().is_none());
    }

    #[test]
    fn outdated_workspace_replies_do_not_open_pickers_or_switch_roots() {
        let mut app = app();
        app.pull_requests.prepare(app.workspace.version());
        app.pull_requests.jobs.reply_for_test(
            app.state.workspace_root.clone(),
            app.pull_requests.request_id,
            ForgeReply::List(Ok(vec![crate::forge::PullRequest {
                number: 1,
                title: "old".into(),
                author: "author".into(),
                branch: "old".into(),
                draft: false,
            }])),
        );
        app.pull_requests.jobs.reply_for_test(
            app.state.workspace_root.clone(),
            app.pull_requests.request_id,
            ForgeReply::CheckedOut(Ok(app.state.workspace_root.join("old"))),
        );
        app.workspace.fixture_safe_mode(true);
        app.process_pull_request_events();
        assert!(app.ui.picker.is_none());
        assert!(!app.workspace.root_operation_pending());
    }

    #[test]
    fn sending_review_comments_cannot_reopen_the_prompt() {
        let mut app = app();
        app.pull_requests.prepare(app.workspace.version());
        app.pull_requests.comment_body = Some("sending draft".into());
        app.pull_requests.comment_sending = true;
        app.begin_review_comment();
        assert!(app.input.prompt().is_none());
        assert_eq!(
            app.pull_requests.comment_body.as_deref(),
            Some("sending draft")
        );
        app.pull_requests.jobs.reply_for_test(
            app.state.workspace_root.clone(),
            app.pull_requests.request_id,
            ForgeReply::Commented(Ok(())),
        );
        app.process_pull_request_events();
        assert!(!app.pull_requests.comment_sending);
        assert!(app.pull_requests.comment_body.is_none());
    }

    #[test]
    fn loading_review_comments_does_not_replace_another_prompt() {
        let mut app = app();
        app.pull_requests.prepare(app.workspace.version());
        app.pull_requests.comment_loading = true;
        app.open_prompt(PromptKind::Command, "workspace.replace query=draft".into());
        let target = ReviewCommentTarget {
            path: app.state.workspace_root.join("file.txt"),
            line: 3,
            text: "source".into(),
            head: crate::forge::PullRequestHead {
                number: 7,
                commit: "abc".into(),
            },
        };
        app.pull_requests.jobs.reply_for_test(
            app.state.workspace_root.clone(),
            app.pull_requests.request_id,
            ForgeReply::CommentTarget(Ok(target)),
        );
        app.process_pull_request_events();
        let prompt = app.input.prompt().unwrap();
        assert_eq!(prompt.kind, PromptKind::Command);
        assert_eq!(prompt.input, "workspace.replace query=draft");
        app.close_prompt();
        app.begin_review_comment();
        assert_eq!(app.input.prompt().unwrap().kind, PromptKind::ReviewComment);
        assert_eq!(
            app.pull_requests
                .comment_target
                .as_ref()
                .unwrap()
                .head
                .number,
            7
        );
        assert!(!app.pull_requests.comment_loading);
    }

    #[test]
    fn failed_review_comments_restore_the_body_and_target() {
        let mut app = app();
        app.pull_requests.prepare(app.workspace.version());
        let path = app.state.workspace_root.join("file.txt");
        app.pull_requests.comment_target = Some(ReviewCommentTarget {
            path: path.clone(),
            line: 3,
            text: "source".into(),
            head: crate::forge::PullRequestHead {
                number: 7,
                commit: "abc".into(),
            },
        });
        app.pull_requests.comment_body = Some("my draft".into());
        app.pull_requests.jobs.reply_for_test(
            app.state.workspace_root.clone(),
            app.pull_requests.request_id,
            ForgeReply::Commented(Err("offline".into())),
        );
        app.process_pull_request_events();
        assert_eq!(app.input.prompt().unwrap().input, "my draft");
        assert_eq!(
            app.pull_requests.comment_target.as_ref().unwrap().path,
            path
        );
        assert_eq!(app.pull_requests.comment_target.as_ref().unwrap().line, 3);
    }
    #[test]
    fn outdated_forge_replies_and_picker_payloads_cannot_change_the_workspace() {
        let mut app = app();
        let root = app.state.workspace_root.clone();
        let request_id = app.pull_request_request_id();
        let list = vec![crate::forge::PullRequest {
            number: 7,
            title: "from A".into(),
            author: "ana".into(),
            branch: "fix".into(),
            draft: false,
        }];
        app.pull_requests.jobs.reply_for_test(
            PathBuf::from("/old-root"),
            request_id,
            ForgeReply::List(Ok(list.clone())),
        );
        app.pull_requests.jobs.reply_for_test(
            root.clone(),
            request_id + 1,
            ForgeReply::List(Ok(list.clone())),
        );
        app.process_pull_request_events();
        assert!(app.search.picker().is_none());
        app.pull_requests
            .jobs
            .reply_for_test(root.clone(), request_id, ForgeReply::List(Ok(list)));
        app.process_pull_request_events();
        assert!(
            matches!(app.search.selected_picker_payload(), Some(PickerPayload::PullRequest { root: source, request_id: version, number: 7, local: None }) if source == &root && *version == request_id)
        );
        let other = tempfile::tempdir().unwrap();
        app.checkout_pull_request(
            root.clone(),
            request_id + 1,
            7,
            Some(other.path().to_path_buf()),
        );
        app.checkout_pull_request(
            PathBuf::from("/old-root"),
            request_id,
            7,
            Some(other.path().to_path_buf()),
        );
        assert_eq!(app.state.workspace_root, root);

        // Switching between existing roots does not change root_revision.
        let first = app.workspace.active_root().unwrap();
        let second = app
            .workspace
            .fixture_add_root(
                "other".into(),
                crate::files::RootIdentity::capture(other.path()).unwrap(),
            )
            .unwrap();
        app.workspace.set_active_root(first).unwrap();
        let request_id = app.pull_request_request_id();
        let revision = app.workspace.root_revision();
        app.workspace.set_active_root(second).unwrap();
        app.workspace.set_active_root(first).unwrap();
        assert_eq!(app.workspace.root_revision(), revision);
        assert!(!app.pull_request_is_current(&root, request_id));
        app.close_picker();
        app.pull_requests.jobs.reply_for_test(
            root.clone(),
            request_id,
            ForgeReply::CheckedOut(Ok(other.path().to_path_buf())),
        );
        app.process_pull_request_events();
        app.checkout_pull_request(
            root.clone(),
            request_id,
            7,
            Some(other.path().to_path_buf()),
        );
        assert_eq!(app.state.workspace_root, root);
        assert!(app.pull_request_request_id() > request_id);
    }
}
