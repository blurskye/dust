use super::*;
use crate::forge::model::{MergeMethod, Side};
use crate::forge::request::{RerunScope, Verdict};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::git::screen) enum Action {
    OpenItem(usize, u64),
    OpenRemote(inbox::Target),
    Browser(usize, u64),
    Scoped {
        host: String,
        repo: String,
        page: usize,
        number: u64,
        action: Box<Action>,
    },
    Filter,
    Views,
    SaveView(views::Scope, Option<String>),
    DeleteView(views::Scope),
    CreateIssue,
    CreatePullRequest,
    ConfirmPullRequest(Box<crate::forge::pull_request::CreatePullRequest>),
    Develop(u64),
    Commit(String),
    Edit(usize, u64),
    Comment(usize, u64),
    State(usize, u64, bool),
    Metadata(usize, u64, &'static str),
    Subscription(usize, u64),
    PrTab(usize),
    Details(usize, u64),
    Review(u64),
    Merge(u64, String),
    Checkout(u64),
    Worktree(u64),
    Inline {
        number: u64,
        path: String,
        line: u32,
        side: &'static str,
        head: String,
    },
    Reply {
        number: u64,
        thread: String,
        repo: String,
        host: String,
    },
    Resolve(String, bool),
    Files(u64),
    Viewed(u64, String),
    Collapse(u64, String),
    SelectFile(u64, String),
    Run(u64),
    /// Dispatch a workflow; 0 means the rail's workflow, else the selected run's.
    Dispatch(u64),
    Cancel(u64),
    Rerun(u64),
    Attempt(u32),
    Approve(u64),
    /// Ctrl+s in a job log: write it to a local file.
    SaveLog(u64),
    Download(u64, String),
}
impl Action {
    pub(super) fn item_scoped(&self) -> bool {
        matches!(
            self,
            Self::Browser(..)
                | Self::Develop(_)
                | Self::Edit(..)
                | Self::Comment(..)
                | Self::State(..)
                | Self::Metadata(..)
                | Self::Subscription(..)
                | Self::Review(_)
                | Self::Merge(..)
                | Self::Checkout(_)
                | Self::Worktree(_)
                | Self::Inline { .. }
                | Self::Reply { .. }
                | Self::Resolve(..)
                | Self::Files(_)
                | Self::Viewed(..)
        )
    }
    pub fn writes(&self) -> bool {
        if let Self::Scoped { action, .. } = self {
            return action.writes();
        }
        !matches!(
            self,
            Self::OpenItem(..)
                | Self::OpenRemote(_)
                | Self::Browser(..)
                | Self::Filter
                | Self::Views
                | Self::Commit(_)
                | Self::PrTab(_)
                | Self::Details(..)
                | Self::Collapse(..)
                | Self::SelectFile(..)
                | Self::Files(_)
                | Self::Run(_)
                | Self::Attempt(_)
                | Self::SaveLog(_)
        )
    }
}
impl Workspace {
    /// An item's title from its detail or the list, for sheet titles.
    pub(super) fn item_title(&self, page: usize, id: u64) -> String {
        self.value(&Key::Item(page, id))
            .and_then(|value| value["title"].as_str())
            .or_else(|| {
                self.items()
                    .into_iter()
                    .find(|row| row["number"].as_u64() == Some(id))
                    .and_then(|row| row["title"].as_str())
            })
            .unwrap_or_default()
            .to_owned()
    }
}

pub(super) fn field(label: &str, value: &str, required: bool, multiline: bool) -> Field {
    let mut field = Field::new(label, value.into());
    field.required = required;
    field.multiline = multiline;
    field
}
pub(super) fn choice(label: &str, value: &str, choices: &[&str]) -> Field {
    let mut field = field(label, value, true, false);
    field.choices = choices.iter().map(|s| (*s).into()).collect();
    field
}
fn kind(page: usize) -> ItemKind {
    if page == 1 {
        ItemKind::Issue
    } else {
        ItemKind::PullRequest
    }
}
fn list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}
impl Workspace {
    fn reply_target(
        &self,
        number: u64,
        thread: &str,
        repo: &str,
        host: &str,
    ) -> Result<&Value, String> {
        if self.repo != repo || self.host != host || self.page != 2 || self.number() != number {
            return Err(
                "The reply source changed; reopen the original conversation (draft preserved)"
                    .into(),
            );
        }
        self.review_threads(number)
            .into_iter()
            .find(|value| string(value, "id") == thread)
            .ok_or_else(|| {
                "Refresh the original review conversation before replying (draft preserved)".into()
            })
    }
    fn select_file(&mut self, id: u64, path: &str) -> Result<(), String> {
        let item = self
            .value(&Key::Item(2, id))
            .ok_or("Pull request is loading")?;
        if !array(item, "files")
            .iter()
            .any(|file| string(file, "path") == path)
        {
            return Err("This file is no longer in the pull request; choose a changed file".into());
        }
        self.selected_files.insert(id, path.into());
        // From the overview's file list, Enter opens the Files tab on that file.
        self.pr_tab = 3;
        self.horizontal = 0;
        self.detail_scroll = 0;
        self.detail_cursor = 0;
        self.detail_rows.clear();
        self.reveal_file = true;
        self.focus = 2;
        self.ensure(false);
        Ok(())
    }
    pub(in crate::git::screen) fn activate(
        &mut self,
        action: Action,
    ) -> Result<Option<Form>, String> {
        self.check_inbox_action(&action)?;
        if let Some(reason) = self.refusal(&action) {
            // The registry's own command (`tea login add --url https://…`) when it has one.
            let hint = match self.host_state() {
                Some(crate::forge::host_state::HostState::Anonymous { hint, .. }) if !hint.is_empty() => {
                    crate::forge::cli::redact(&hint)
                }
                _ => super::issue::sign_in(self.forge_kind()).0.to_owned(),
            };
            return Err(format!("{reason} · {hint}"));
        }
        if self.repo.is_empty() {
            return Err(
                format!("The repository is not loaded; sign in ({}) and refresh", super::issue::sign_in(self.forge_kind()).0),
            );
        }
        if action != Action::CreatePullRequest {
            self.pending_pr = false;
            self.ready_form = None;
        }
        let title_owned: String;
        let mut fields;
        let title: &str;
        match &action {
            Action::Scoped { .. } => return Err("Reopen the original item action".into()),
            Action::Browser(_, id) => {
                if *id == 0 {
                    return Err("Select an item first".into());
                }
                title = "Open this item in your browser?";
                fields = vec![];
            }
            Action::OpenRemote(target) => {
                if target.host != self.host {
                    return Err("Linked item belongs to another host".into());
                }
                self.remember_list_selection();
                self.inbox.origin_page.get_or_insert(self.page);
                self.switch_detail_repository(target.repo.clone());
                self.reset_remote_detail();
                self.page = target.page;
                self.opened = Some((target.page, target.number));
                self.focus = 2;
                self.pr_tab = 0;
                self.issue_tab = 0;
                self.ensure(false);
                return Ok(None);
            }
            Action::OpenItem(page, id) => {
                if self.inbox.enabled {
                    self.inbox.origin_page.get_or_insert(self.page);
                }
                self.remember_list_selection();
                self.page = *page;
                if matches!(*page, 1 | 2) {
                    self.restore_list_selection();
                }
                self.opened = Some((*page, *id));
                self.focus = 2;
                self.reading = true;
                self.detail_cursor = 0;
                self.detail_scroll = 0;
                self.horizontal = 0;
                self.pr_tab = 0;
                self.issue_tab = 0;
                self.selected_commit = None;
                self.detail_rows.clear();
                self.reveal_file = false;
                self.ensure(false);
                return Ok(None);
            }
            Action::Commit(sha) => {
                git2::Oid::from_str(sha).map_err(|_| "Invalid commit identifier")?;
                self.selected_commit = Some(sha.clone());
                self.ensure(false);
                return Ok(None);
            }
            Action::PrTab(tab) => {
                self.focus = 2;
                // A new tab reads from its top; Files (a cursor over code) and Merge (one
                // control) start on their rows.
                self.reading = self.page == 1 || !matches!(*tab, pr::FILES | pr::MERGE);
                if self.page == 1 {
                    self.issue_tab = *tab;
                } else {
                    self.pr_tab = *tab;
                }
                self.detail_cursor = 0;
                self.detail_scroll = 0;
                self.horizontal = 0;
                self.detail_rows.clear();
                self.reveal_file = *tab == 3;
                self.ensure(false);
                return Ok(None);
            }
            Action::Details(page, id) => {
                if !self.expanded_details.remove(&(*page, *id)) {
                    self.expanded_details.insert((*page, *id));
                }
                self.ensure(false);
                return Ok(None);
            }
            Action::Viewed(..) => {
                self.submit(action, Vec::new())?;
                return Ok(None);
            }
            Action::SelectFile(id, path) => {
                self.select_file(*id, path)?;
                return Ok(None);
            }
            Action::Files(id) => {
                let item = self
                    .value(&Key::Item(2, *id))
                    .ok_or("Pull request is loading")?;
                let paths = array(item, "files")
                    .iter()
                    .map(|file| string(file, "path"))
                    .collect::<Vec<_>>();
                if paths.is_empty() {
                    return Err("This pull request has no changed files".into());
                }
                return Ok(Some(self.form(
                    "Choose changed file",
                    action.clone(),
                    vec![choice("File", self.selected_file(*id, item), &paths)],
                )));
            }
            Action::Collapse(id, path) => {
                let value = (*id, path.clone());
                if !self.collapsed.remove(&value) {
                    self.collapsed.insert(value);
                }
                return Ok(None);
            }
            Action::Run(id) => {
                self.remember_list_selection();
                self.page = 3;
                self.run_id = Some(*id);
                self.attempt = 0;
                self.last_list = 1;
                self.focus = 1;
                self.detail_cursor = 0;
                self.open_run(*id);
                return Ok(None);
            }
            Action::Attempt(attempt) => {
                self.attempt = *attempt;
                self.detail_cursor = 0;
                self.ensure(false);
                return Ok(None);
            }
            Action::SaveLog(job) => {
                if self.raw(&Key::Log(*job)).is_none() {
                    return Err("The log is still loading".into());
                }
                let name = self.log_file_name(*job);
                title_owned = format!("Save job log · {name}");
                return Ok(Some(self.form(
                    &title_owned,
                    action.clone(),
                    vec![field("File", &name, true, false)],
                )));
            }
            Action::Download(_, name) => {
                let caps = self.capabilities();
                if let Some(reason) = caps.artifacts_download.reason() {
                    return Err(reason.into());
                }
                title_owned = format!("Download artifact · {name}");
                return Ok(Some(self.form(
                    &title_owned,
                    action.clone(),
                    vec![field("Destination folder", "artifacts", true, false)],
                )));
            }
            Action::Views => return self.views_form().map(Some),
            Action::SaveView(..) => return self.save_view_form().map(Some),
            Action::DeleteView(_) => return self.delete_view_form().map(Some),
            Action::Filter => {
                title = if self.page == 1 {
                    "Filter issues · /"
                } else {
                    "Filter pull requests · /"
                };
                let index = self.page.saturating_sub(1).min(1);
                // The cursor lands after a space, so typing adds a term to the section's query.
                let query = &self.queries[index];
                let query = if query.is_empty() { String::new() } else { format!("{query} ") };
                fields = vec![
                    field(&format!("Search ({} query)", self.forge_name()), &query, false, false),
                    choice(
                        "State",
                        &self.states[index],
                        if self.page == 2 {
                            &["open", "closed", "merged", "all"]
                        } else {
                            &["open", "closed", "all"]
                        },
                    ),
                ];
            }
            Action::CreateIssue => {
                title = "Create issue";
                fields = vec![
                    field("Title", "", true, false),
                    field("Description", "", false, true),
                    field("Labels (comma separated)", "", false, false),
                    field("Assignees (comma separated)", "", false, false),
                ];
            }
            Action::CreatePullRequest => return self.create_pr_form(),
            Action::ConfirmPullRequest(_) => {
                return Err("Open the PR form to review its source and destination".into());
            }
            Action::Develop(id) => {
                title_owned = format!("Create linked branch on {} (current checkout stays unchanged)", self.forge_name());
                title = &title_owned;
                let base = self
                    .value(&Key::Repository)
                    .and_then(|value| value["defaultBranchRef"]["name"].as_str())
                    .unwrap_or("");
                fields = vec![
                    field("Branch", &format!("issue-{id}"), true, false),
                    field("Base branch", base, true, false),
                ];
            }
            Action::Edit(page, id) => {
                title = "Edit title and description";
                let value = self
                    .value(&Key::Item(*page, *id))
                    .ok_or("Item is loading")?;
                fields = vec![
                    field("Title", string(value, "title"), true, false),
                    field("Description", string(value, "body"), false, true),
                ];
            }
            Action::Comment(_, id) => {
                if *id == 0 {
                    return Err("Select an issue or pull request".into());
                }
                title = "Leave a comment";
                fields = vec![field("Comment", "", true, true)];
            }
            Action::State(page, number, closed) => {
                let noun = if *page == 1 {
                    "issue".to_owned()
                } else {
                    self.terms().pr_singular.to_owned()
                };
                title_owned = if *closed {
                    format!("Close {noun} {}{number}?", self.sigil(*page == 2))
                } else {
                    format!("Reopen {noun} {}{number}?", self.sigil(*page == 2))
                };
                title = &title_owned;
                // §3.34: closing an issue says why; a forge without the reason says so.
                fields = if *closed && *page == 1 {
                    if self.capabilities().issue_not_planned.allowed() {
                        vec![choice("Reason", "Completed", &["Completed", "Not planned"])]
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                };
            }
            Action::Metadata(page, id, name) => {
                title = "Edit metadata";
                let value = self
                    .value(&Key::Item(*page, *id))
                    .ok_or("Item is loading")?;
                let current = match *name {
                    "labels" => array(value, "labels")
                        .iter()
                        .map(|v| string(v, "name"))
                        .collect::<Vec<_>>()
                        .join(","),
                    "assignees" => array(value, "assignees")
                        .iter()
                        .map(|v| string(v, "login"))
                        .collect::<Vec<_>>()
                        .join(","),
                    "milestone" => string(&value["milestone"], "title").into(),
                    "projects" => array(value, "projectItems")
                        .iter()
                        .filter_map(|v| {
                            v["title"]
                                .as_str()
                                .or_else(|| v["project"]["title"].as_str())
                        })
                        .collect::<Vec<_>>()
                        .join(","),
                    _ => String::new(),
                };
                fields = vec![field(name, &current, false, false)];
            }
            Action::Subscription(_, _) => {
                title = "Notifications";
                fields = vec![choice(
                    "Subscription",
                    "SUBSCRIBED",
                    &["SUBSCRIBED", "UNSUBSCRIBED", "IGNORED"],
                )];
            }
            Action::Review(id) => {
                // §3.30: the verdicts this forge offers; the summary follows.
                let caps = self.capabilities();
                let mut verdicts = vec!["Comment"];
                if caps.review_approve.allowed() {
                    verdicts.push("Approve");
                }
                if caps.review_request_changes.allowed() {
                    verdicts.push("Request changes");
                }
                title_owned = format!("Review {}{id} {}", self.sigil(true), self.item_title(2, *id));
                title = &title_owned;
                fields = vec![
                    choice("Decision", "Comment", &verdicts),
                    field("Summary", "", false, true),
                ];
            }
            Action::Merge(id, _) => {
                // §3.32: the methods this forge offers, then what happens to the branch.
                let caps = self.capabilities();
                let methods = caps
                    .merge_methods
                    .iter()
                    .filter_map(|method| match method {
                        MergeMethod::Squash => Some("Squash"),
                        MergeMethod::Merge => Some("Merge"),
                        MergeMethod::Rebase => Some("Rebase"),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let first = if methods.contains(&"Squash") { "Squash" } else { methods.first().copied().unwrap_or("Merge") };
                let name = self.item_title(2, *id);
                title_owned = format!("Merge {}{id} {name}", self.sigil(true));
                title = &title_owned;
                fields = vec![choice("Method", first, &methods)];
                if caps.delete_branch_on_merge.allowed() {
                    fields.push(choice(
                        "Source branch",
                        "Keep it",
                        &["Keep it", "Delete it after the merge"],
                    ));
                }
                fields.push(field("Commit title", &format!("{name} ({}{id})", self.sigil(true)), false, false));
            }
            Action::Checkout(_) => {
                title = "Check out pull request in this repository?";
                fields = vec![];
            }
            Action::Worktree(_) => {
                title = "Check out pull request in a separate worktree?";
                fields = vec![];
            }
            Action::Inline { number, path, line, .. } => {
                // §3.28: where the forge batches reviews, a comment can wait for the review
                // (`Add to review`) instead of notifying at once; once one waits, the next
                // waits too unless changed.
                let mut fields = vec![field("Comment", "", true, true)];
                if self.capabilities().batched_review.allowed() {
                    let waiting = self.inline_drafts.get(number).is_some_and(|drafts| !drafts.is_empty());
                    fields.push(choice(
                        "Send",
                        if waiting { "Add to review" } else { "Post now" },
                        &["Post now", "Add to review"],
                    ));
                }
                return Ok(Some(self.form(&format!("Comment on {path}:{line}"), action, fields)));
            }
            Action::Reply {
                number,
                thread,
                repo,
                host,
            } => {
                let target = self.reply_target(*number, thread, repo, host)?;
                let first = array(&target["comments"], "nodes")
                    .first()
                    .ok_or("Conversation has no comments")?;
                let mut source = field(
                    "Source · original conversation",
                    &format!("{host}/{repo} · PR #{number}"),
                    false,
                    false,
                );
                source.read_only = true;
                source.input.cursor = 0;
                return Ok(Some(self.form(
                    &format!(
                        "Reply to @{} · {} · PR #{number}",
                        login(first),
                        string(first, "path")
                    ),
                    action.clone(),
                    vec![field("Reply", "", true, true), source],
                )));
            }
            Action::Resolve(_, resolved) => {
                title = if *resolved {
                    "Resolve review thread?"
                } else {
                    "Reopen review thread?"
                };
                fields = vec![];
            }
            Action::Dispatch(id) => {
                let caps = self.capabilities();
                if let Some(reason) = caps.dispatch.reason() {
                    return Err(reason.into());
                }
                let workflow_noun = self.terms().workflow_noun;
                let id = if *id == 0 {
                    self.dispatch_workflow()
                        .ok_or_else(|| format!("Choose a {workflow_noun} in the rail first"))?
                } else {
                    *id
                };
                let key = Key::Workflow(id, String::new());
                let definition = match self.cache.get(&key) {
                    Some(Ok(reply)) => Some(workflow::Definition::parse(&reply.text)?),
                    // A forge without an inputs schema dispatches with a ref alone.
                    // Without its definition the sheet still dispatches with a ref alone.
                    Some(Err(_)) => None,
                    None if !caps.dispatch_inputs_schema.allowed() => None,
                    None => {
                        self.begin_dispatch(id);
                        self.ensure(false);
                        self.notice = format!("Reading the {workflow_noun}…");
                        self.notice_error = false;
                        return Ok(None);
                    }
                };
                if definition.as_ref().is_some_and(|definition| !definition.dispatch) {
                    return Err(format!("This {workflow_noun} has no workflow_dispatch trigger"));
                }
                let refs = self.dispatch_refs();
                let mut reference = field("Ref", refs.first().map_or("", String::as_str), true, false);
                reference.custom_choices = true;
                reference.choices = refs;
                let mut inputs = vec![reference];
                for input in definition.map(|definition| definition.inputs).unwrap_or_default() {
                    let mut f = field(&input.name, &input.default, input.required, false);
                    f.choices = input.choices;
                    inputs.push(f);
                }
                title_owned = format!("Run {}", self.workflow_name(id));
                return Ok(Some(self.form(&title_owned, Action::Dispatch(id), inputs)));
            }
            Action::Cancel(id) => {
                let caps = self.capabilities();
                if let Some(reason) = caps.cancel_run.reason() {
                    return Err(reason.into());
                }
                let (name, number, run) = self.sheet_run(*id).ok_or("The run is loading")?;
                if self.run_finished(run) {
                    return Err(format!("{name} #{number} already finished"));
                }
                title_owned = format!("Cancel {name} #{number}?");
                title = &title_owned;
                fields = vec![];
            }
            Action::Rerun(id) => {
                let caps = self.capabilities();
                if let Some(reason) = caps.rerun_all.reason().filter(|_| !caps.rerun_failed.allowed()) {
                    return Err(reason.into());
                }
                let (name, number, run) = self.sheet_run(*id).ok_or("The run is loading")?;
                if !self.run_finished(run) {
                    return Err(format!("{name} #{number} is still running · x cancels it"));
                }
                let (failed, all) = self.run_jobs(*id);
                let mut scopes = Vec::new();
                if caps.rerun_failed.allowed() && failed > 0 {
                    scopes.push(format!("Failed jobs only ({failed})"));
                }
                if caps.rerun_all.allowed() {
                    scopes.push(if all > 0 { format!("All jobs ({all})") } else { "All jobs".into() });
                }
                if scopes.is_empty() {
                    return Err(caps.rerun_failed.reason().unwrap_or("Nothing failed to rerun").into());
                }
                let scope_refs = scopes.iter().map(String::as_str).collect::<Vec<_>>();
                let mut fields_now = vec![choice("Jobs", scope_refs[0], &scope_refs)];
                if self.forge_kind().is_none_or(|kind| kind == crate::forge::provider::ProviderKind::GitHub) {
                    fields_now.push(choice("Debug logging", "Off", &["Off", "On"]));
                }
                title_owned = format!("Rerun {name} #{number}");
                return Ok(Some(self.form(&title_owned, action.clone(), fields_now)));
            }
            Action::Approve(id) => {
                let caps = self.capabilities();
                if let Some(reason) = caps.approve_fork_run.reason() {
                    return Err(reason.into());
                }
                let (name, number, run) = self.sheet_run(*id).ok_or("The run is loading")?;
                if run["needsApproval"].as_bool() != Some(true) && string(run, "conclusion") != "action_required" {
                    return Err(format!("{name} #{number} is not waiting for approval"));
                }
                title_owned = format!("{} {name} #{number}?", self.terms().approve_verb);
                title = &title_owned;
                fields = vec![];
            }
        }
        Ok(Some(self.form(title, action, fields)))
    }
    pub fn submit(&mut self, action: Action, values: Vec<String>) -> Result<(), String> {
        if let Action::SaveLog(job) = &action {
            return self.save_log(*job, &values);
        }
        if self.writing() {
            return Err(format!("Wait for the current {} write", self.forge_name()));
        }
        self.save_draft(&action, values.clone());
        if let Action::Scoped {
            host,
            repo,
            page,
            number,
            action,
        } = action
        {
            if host != self.host
                || repo != self.repo
                || page != self.page
                || number != self.number()
            {
                return Err(
                    "The action source changed; reopen the original item (draft preserved)".into(),
                );
            }
            return self.submit(*action, values);
        }
        self.check_inbox_action(&action)?;
        let write = match &action {
            Action::Views => {
                let [label] = values.as_slice() else {
                    return Err("Choose a repository view".into());
                };
                return self.choose_view(label);
            }
            Action::SaveView(scope, _) => {
                let draft = self.draft_key(&action);
                self.save_view(scope, &values)?;
                self.drafts.remove(&draft);
                return Ok(());
            }
            Action::DeleteView(scope) => return self.delete_view(scope, &values),
            Action::Filter => {
                let [query, state] = values.as_slice() else {
                    return Err("Invalid repository filter".into());
                };
                let active = self.active_views[self.page.saturating_sub(1).min(1)].clone();
                self.apply_view(query.trim().to_owned(), state.clone(), active)?;
                self.drafts.remove(&self.draft_key(&action));
                return Ok(());
            }
            Action::Browser(page, id) => Write::OpenInBrowser {
                kind: kind(*page),
                number: *id,
                url: None,
            },
            Action::CreateIssue => Write::CreateIssue {
                title: values[0].clone(),
                body: values[1].clone(),
                labels: list(&values[2]),
                assignees: list(&values[3]),
            },
            Action::ConfirmPullRequest(request) => {
                self.writing_draft = Some(self.draft_key(&Action::CreatePullRequest));
                let op = Op::Write(
                    ForgeTarget::repository(&request.host, &request.base_repository),
                    Write::CreatePullRequest((**request).clone()),
                );
                self.queue.push_back(Request { key: Key::Write, op });
                self.notice = if request.push.is_some() {
                    "Pushing the reviewed branch, then creating the PR…"
                } else {
                    "Creating pull request…"
                }
                .into();
                return Ok(());
            }
            Action::Develop(id) => Write::Develop {
                number: *id,
                branch: values[0].clone(),
                base: values[1].clone(),
            },
            Action::Edit(page, id) => Write::Edit {
                kind: kind(*page),
                number: *id,
                title: values[0].clone(),
                body: values[1].clone(),
            },
            Action::Comment(page, id) => Write::Comment {
                kind: kind(*page),
                number: *id,
                body: values[0].clone(),
            },
            Action::State(page, id, closed) => Write::SetState {
                kind: kind(*page),
                number: *id,
                open: !*closed,
                reason: match values.first().map(String::as_str) {
                    Some("Not planned") if *closed => {
                        Some(crate::forge::model::ClosedReason::NotPlanned)
                    }
                    Some("Completed") if *closed => Some(crate::forge::model::ClosedReason::Completed),
                    _ => None,
                },
            },
            Action::Metadata(page, id, name) => {
                let value = self
                    .value(&Key::Item(*page, *id))
                    .ok_or("Refresh the item before editing")?;
                let new = values[0]
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .collect::<BTreeSet<_>>();
                let old = match *name {
                    "labels" => array(value, "labels")
                        .iter()
                        .map(|v| string(v, "name"))
                        .collect::<BTreeSet<_>>(),
                    "assignees" => array(value, "assignees")
                        .iter()
                        .map(|v| string(v, "login"))
                        .collect(),
                    "projects" => array(value, "projectItems")
                        .iter()
                        .filter_map(|v| {
                            v["title"]
                                .as_str()
                                .or_else(|| v["project"]["title"].as_str())
                        })
                        .collect(),
                    "reviewers" | "milestone" => BTreeSet::new(),
                    _ => return Err("Unknown metadata field".into()),
                };
                let remove = old
                    .difference(&new)
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>();
                let add = new
                    .difference(&old)
                    .map(|value| (*value).to_owned())
                    .collect::<Vec<_>>();
                let (kind, number) = (kind(*page), *id);
                match *name {
                    "milestone" => Write::SetMilestone {
                        kind,
                        number,
                        milestone: (!values[0].is_empty()).then(|| values[0].clone()),
                    },
                    _ if add.is_empty() && remove.is_empty() => return Ok(()),
                    "labels" => Write::SetLabels {
                        kind,
                        number,
                        add,
                        remove,
                    },
                    "assignees" => Write::SetAssignees {
                        kind,
                        number,
                        add,
                        remove,
                    },
                    "projects" => Write::SetProjects {
                        kind,
                        number,
                        add,
                        remove,
                    },
                    _ => Write::SetReviewers {
                        number,
                        add,
                        remove,
                    },
                }
            }
            Action::Subscription(page, id) => {
                let node = string(
                    self.value(&Key::Item(*page, *id))
                        .ok_or("Item is loading")?,
                    "id",
                );
                Write::Subscribe {
                    kind: kind(*page),
                    number: *id,
                    node: node.into(),
                    state: crate::forge::request::SubscriptionState::parse(&values[0])
                        .ok_or("Choose a subscription state")?,
                }
            }
            Action::Review(id) => {
                if values[0] != "Approve" && values[1].trim().is_empty() {
                    return Err("A comment or request for changes needs a summary".into());
                }
                let login = crate::forge::host_state::account(&crate::forge::transport::HostKey::new(&self.host))
                    .and_then(|account| account.login);
                let author = self
                    .value(&Key::Item(2, *id))
                    .and_then(|value| value["author"]["login"].as_str());
                if values[0] == "Approve" && login.is_some() && login.as_deref() == author {
                    return Err("Approve is unavailable on your own pull request; leave a comment instead".into());
                }
                Write::SubmitReview {
                    number: *id,
                    verdict: match values[0].as_str() {
                        "Approve" => Verdict::Approve,
                        "Request changes" => Verdict::RequestChanges,
                        _ => Verdict::Comment,
                    },
                    body: values[1].clone(),
                    drafts: self
                        .inline_drafts
                        .get(id)
                        .map(|drafts| drafts.iter().map(|(draft, _)| draft.clone()).collect())
                        .unwrap_or_default(),
                    head: self.inline_drafts.get(id).and_then(|drafts| drafts.first()).map(|(_, head)| head.clone()),
                }
            }
            Action::Merge(id, head) => {
                if head.is_empty() {
                    return Err("Refresh the pull request before merging".into());
                }
                let method = match values[0].as_str() {
                    "Merge" => MergeMethod::Merge,
                    "Rebase" => MergeMethod::Rebase,
                    _ => MergeMethod::Squash,
                };
                let delete_branch = values.iter().any(|value| value == "Delete it after the merge");
                let title = values
                    .last()
                    .filter(|_| values.len() > 1 && method != MergeMethod::Rebase)
                    .map(|title| title.trim().to_owned())
                    .filter(|title| !title.is_empty());
                Write::Merge {
                    number: *id,
                    method,
                    head: head.clone(),
                    delete_branch,
                    title,
                    message: None,
                }
            }
            Action::Checkout(id) => Write::Checkout { number: *id },
            Action::Worktree(id) => {
                self.queue.push_back(Request {
                    key: Key::WorktreeWrite(*id),
                    op: Op::Write(self.target(), Write::Worktree { number: *id }),
                });
                self.writing_draft = Some(self.draft_key(&action));
                return Ok(());
            }
            Action::Inline {
                number,
                path,
                line,
                side,
                head,
            } => {
                let side = if *side == "LEFT" { Side::Left } else { Side::Right };
                if values.get(1).map(String::as_str) == Some("Add to review") {
                    let body = values[0].trim();
                    if body.is_empty() {
                        return Err("Comment is required".into());
                    }
                    let drafts = self.inline_drafts.entry(*number).or_default();
                    drafts.push((
                        crate::forge::request::InlineDraft {
                            path: path.clone(),
                            line: u64::from(*line),
                            side,
                            body: body.to_owned(),
                        },
                        head.clone(),
                    ));
                    let count = drafts.len();
                    self.drafts.remove(&self.draft_key(&action));
                    self.notice_error = false;
                    self.notice = format!(
                        "Draft saved · {count} inline draft{} wait for your review · r review",
                        if count == 1 { "" } else { "s" }
                    );
                    return Ok(());
                }
                Write::InlineComment {
                    number: *number,
                    path: path.clone(),
                    line: u64::from(*line),
                    side,
                    head: head.clone(),
                    body: values[0].clone(),
                }
            }
            Action::Files(id) => {
                let path = values.first().ok_or("Choose a changed file")?;
                self.select_file(*id, path)?;
                self.drafts.remove(&self.draft_key(&action));
                return Ok(());
            }
            Action::Reply {
                number,
                thread,
                repo,
                host,
            } => {
                self.reply_target(*number, thread, repo, host)?;
                let body = values
                    .first()
                    .filter(|body| !body.trim().is_empty())
                    .ok_or("Reply is required")?;
                Write::Reply {
                    number: *number,
                    thread: thread.as_str().into(),
                    body: body.clone(),
                }
            }
            Action::Viewed(id, path) => {
                let viewed = self
                    .file_viewed(*id, path)
                    .ok_or("Viewed state is loading")?;
                let node = string(
                    self.value(&Key::Item(2, *id))
                        .ok_or("Pull request is loading")?,
                    "id",
                );
                Write::SetViewed {
                    number: *id,
                    node: node.into(),
                    path: path.clone(),
                    viewed: !viewed,
                }
            }
            Action::Resolve(id, resolved) => Write::Resolve {
                thread: id.as_str().into(),
                resolved: *resolved,
            },
            Action::Dispatch(id) => {
                let inputs = match self.raw(&Key::Workflow(*id, String::new())) {
                    Some(yaml) => workflow::Definition::parse(yaml)?.inputs,
                    None => Vec::new(),
                };
                let reference = values[0].trim().to_owned();
                self.note_dispatch(*id, &reference);
                Write::Dispatch {
                    workflow: (*id).into(),
                    reference,
                    inputs: inputs
                        .iter()
                        .zip(values.iter().skip(1))
                        .filter(|(input, value)| input.required || !value.is_empty())
                        .map(|(input, value)| (input.name.clone(), value.clone()))
                        .collect(),
                }
            }
            Action::Cancel(id) => Write::CancelRun((*id).into()),
            Action::Rerun(id) => Write::Rerun {
                run: (*id).into(),
                scope: if values[0].starts_with("Failed") {
                    RerunScope::Failed
                } else {
                    RerunScope::All
                },
                debug: values.get(1).is_some_and(|value| value == "On"),
            },
            Action::Approve(id) => {
                let run = self.sheet_run(*id).ok_or("The run is loading")?.2;
                Write::ApproveRun {
                    run: (*id).into(),
                    expected: crate::forge::request::RunIdentity {
                        sha: string(run, "headSha").to_owned(),
                        branch: string(run, "headBranch").to_owned(),
                        head_owner: String::new(),
                    },
                }
            }
            Action::Download(artifact, name) => {
                let folder = values[0].trim();
                let directory = if std::path::Path::new(folder).is_absolute() {
                    PathBuf::from(folder)
                } else {
                    self.root.join(folder)
                };
                Write::DownloadArtifact {
                    artifact: (*artifact).into(),
                    name: name.clone(),
                    directory,
                }
            }
            _ => return Err("This action has no form".into()),
        };
        self.writing_draft = Some(self.draft_key(&action));
        self.cache.remove(&Key::Write);
        self.optimistic(&write);
        self.enqueue(Key::Write, Op::Write(self.target(), write), false);
        self.notice_error = false;
        self.notice = format!("Sending to {}…", self.forge_name());
        Ok(())
    }
}
