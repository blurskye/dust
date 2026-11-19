use super::*;
use crate::forge::model::{MergeMethod, Side};
use crate::forge::request::{RerunScope, Verdict};

/// The `Send` choices of a line comment, in GitHub's words.
const SINGLE_COMMENT: &str = "Add single comment";
const START_REVIEW: &str = "Start a review";
const REVIEW_COMMENT: &str = "Add review comment";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::screen) enum Action {
    OpenItem(usize, u64),
    OpenRemote(inbox::Target),
    Browser(usize, u64),
    /// One card of the Conversation (a comment, a thread) on the forge: `o`.
    Web(usize, u64, String),
    Scoped {
        host: String,
        repo: String,
        page: usize,
        number: u64,
        action: Box<Action>,
    },
    Filter,
    /// The empty state's "Clear the filter": every open item, no query (GitHub's "Clear
    /// current search query, filters, and sorts").
    ClearFilter,
    /// `F`: GitHub's filter menus (Author, Label, Projects, Milestones, Assignee, Sort) as
    /// one sheet.
    Facets,
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
    PrTab(usize),
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
    /// A review thread's suggested change (pull request, thread id), committed to the head
    /// branch after its sheet.
    CommitSuggestion(u64, String),
    Files(u64),
    Viewed(u64, String),
    Collapse(u64, String),
    SelectFile(u64, String),
    Run(u64),
    /// A pull request's check in its Checks tab, by `checks.rs`'s identity: Enter opens its run
    /// (or, with no run, its page on the forge).
    Check(u64, String),
    /// Dispatch a workflow; 0 means the rail's workflow, else the selected run's.
    Dispatch(u64),
    Cancel(u64),
    Rerun(u64),
    Attempt(u32),
    Approve(u64),
    /// Ctrl+s in a job log: write it to a local file.
    SaveLog(u64),
    Download(u64, String),
    /// A comment on lines `start..=line` (`V`, then `c` in Files): held for the review.
    InlineRange {
        number: u64,
        path: String,
        start: u32,
        line: u32,
        side: &'static str,
        head: String,
    },
    /// The pull request's `n`th review draft: edit or delete it.
    Draft(u64, usize),
    /// Signed out: the forge's own new pull request form, prefilled, in the browser.
    CreatePullRequestWeb,
    /// The merge box's `Method` row: the next method this forge allows. Nothing is written
    /// until the merge itself is confirmed.
    MergeMethod(u64),
}
impl Action {
    pub(super) fn item_scoped(&self) -> bool {
        matches!(
            self,
            Self::Browser(..)
                | Self::Web(..)
                | Self::Develop(_)
                | Self::Edit(..)
                | Self::Comment(..)
                | Self::State(..)
                | Self::Metadata(..)
                | Self::Review(_)
                | Self::Merge(..)
                | Self::Checkout(_)
                | Self::Worktree(_)
                | Self::Inline { .. }
                | Self::InlineRange { .. }
                | Self::Draft(..)
                | Self::Reply { .. }
                | Self::Resolve(..)
                | Self::CommitSuggestion(..)
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
                | Self::Web(..)
                | Self::Filter
                | Self::Facets
                | Self::Views
                | Self::Commit(_)
                | Self::PrTab(_)
                | Self::Collapse(..)
                | Self::SelectFile(..)
                | Self::Files(_)
                | Self::Run(_)
                | Self::Check(..)
                | Self::Attempt(_)
                | Self::SaveLog(_)
                | Self::Draft(..)
                | Self::CreatePullRequestWeb
                | Self::MergeMethod(_)
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
                "This conversation changed · reopen it to reply · your draft is kept".into(),
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
    pub(in crate::screen) fn activate(&mut self, action: Action) -> Result<Option<Form>, String> {
        if let Some(result) = self.activate_feature(&action) {
            return result;
        }
        self.check_inbox_action(&action)?;
        if let Some(reason) = self.refusal(&action) {
            // The sign-in key and this forge's own way in: `gh`, `glab`, or the token
            // settings page on Gitea and Forgejo.
            let hint = super::states::sign_in_hint(self.forge_kind(), &self.host);
            return Err(format!("{reason} · {hint}"));
        }
        if self.repo.is_empty() {
            return Err(format!(
                "The repository is not loaded; sign in ({}) and refresh",
                super::issue::sign_in(self.forge_kind()).0
            ));
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
            Action::Browser(page, id) => {
                if *id == 0 {
                    return Err("Select an item first".into());
                }
                // Names what opens, as Close does (`Open pull request #13 in your browser?`).
                title_owned = match page {
                    1 => format!("Open issue {}{id} in your browser?", self.sigil(false)),
                    2 => format!(
                        "Open {} {}{id} in your browser?",
                        self.terms().pr_singular,
                        self.sigil(true)
                    ),
                    _ => "Open this item in your browser?".to_owned(),
                };
                title = &title_owned;
                fields = vec![];
            }
            Action::Web(..) => {
                title = "Open this in your browser?";
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
                // Enter on the open commit (`‹ All commits`) goes back to the list; a commit
                // row anywhere else (the Conversation's too) opens it on the Commits tab.
                // Beside the commit list (any reader but a Narrow one) there is no list to go
                // back to: the open commit stays.
                let back = self.pr_tab == pr::COMMITS
                    && self.selected_commit.as_ref() == Some(sha)
                    && self.reading_position.narrow_pr;
                if self.page == 2 {
                    self.pr_tab = pr::COMMITS;
                    self.detail_cursor = 0;
                    self.detail_scroll = 0;
                    self.horizontal = 0;
                    self.detail_rows.clear();
                }
                self.selected_commit = if back { None } else { Some(sha.clone()) };
                self.ensure(false);
                return Ok(None);
            }
            Action::PrTab(tab) => {
                self.focus = 2;
                // The Conversation reads from its top; Commits, Files and Checks (lists of
                // things to open) start with the cursor on their rows.
                self.reading =
                    self.page == 1 || !matches!(*tab, pr::COMMITS | pr::FILES | pr::CHECKS);
                if self.page == 1 {
                    self.issue_tab = *tab;
                } else {
                    self.pr_tab = *tab;
                }
                self.detail_cursor = 0;
                self.detail_scroll = 0;
                self.horizontal = 0;
                self.detail_rows.clear();
                // Files and Checks open on their selected file or job.
                self.reveal_file = *tab == 3 || self.page == 2 && *tab == pr::CHECKS;
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
                // From a pull request's check: Esc in the run comes back to it (R9-05).
                self.remember_return();
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
                let mut form = self.form(
                    &title_owned,
                    action.clone(),
                    vec![field("File", &name, true, false)],
                );
                form.title = title_owned;
                return Ok(Some(form));
            }
            Action::Download(_, name) => {
                let caps = self.capabilities();
                if let Some(reason) = caps.artifacts_download.reason() {
                    return Err(reason.into());
                }
                title_owned = format!("Download artifact · {name}");
                // The path the download writes, as the receipt names it.
                let mut form = self.form(
                    &title_owned,
                    action.clone(),
                    vec![field("Destination folder", "./artifacts", true, false)],
                );
                // The top bar names the repository; a sheet does not name it again (A3-08).
                form.title = title_owned;
                return Ok(Some(form));
            }
            Action::ClearFilter => {
                self.apply_view(String::new(), "open".into(), None)?;
                return Ok(None);
            }
            Action::Views => return self.views_form().map(Some),
            Action::Facets => return Ok(Some(self.facets_form())),
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
                let query = if query.is_empty() {
                    String::new()
                } else {
                    format!("{query} ")
                };
                fields = vec![
                    field(
                        &format!("Search ({} query)", self.forge_name()),
                        &query,
                        false,
                        false,
                    ),
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
                title_owned = format!(
                    "Create linked branch on {} (current checkout stays unchanged)",
                    self.forge_name()
                );
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
                // The item's own title beside its number.
                let name = self
                    .value(&Key::Item(*page, *number))
                    .map(|value| string(value, "title").to_owned())
                    .unwrap_or_default();
                let verb = if *closed { "Close" } else { "Reopen" };
                title_owned = if name.is_empty() {
                    format!("{verb} {noun} {}{number}?", self.sigil(*page == 2))
                } else {
                    format!("{verb} {noun} {}{number} · {name}", self.sigil(*page == 2))
                };
                title = &title_owned;
                // Closing an issue says why; a forge without the reason says so. An
                // optional closing comment follows, posted before the issue closes.
                fields = if *closed && *page == 1 {
                    let mut fields = Vec::new();
                    if self.capabilities().issue_not_planned.allowed() {
                        fields.push(choice("Reason", "Completed", &["Completed", "Not planned"]));
                    }
                    fields.push(field("Comment (optional)", "", false, true));
                    fields
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
            Action::Review(id) => {
                // The verdicts this forge offers; the summary follows.
                let caps = self.capabilities();
                let mut verdicts = vec!["Comment"];
                if caps.review_approve.allowed() {
                    verdicts.push("Approve");
                }
                if caps.review_request_changes.allowed() {
                    verdicts.push("Request changes");
                }
                title_owned = format!(
                    "Review {}{id} {}",
                    self.sigil(true),
                    self.item_title(2, *id)
                );
                title = &title_owned;
                fields = vec![
                    choice("Decision", "Comment", &verdicts),
                    field("Summary", "", false, true),
                ];
            }
            Action::Merge(id, _) => {
                // The methods this forge offers, then what happens to the branch.
                let caps = self.capabilities();
                let methods = self.merge_methods();
                // The merge box's `Method` row chose it; the sheet still lets it change.
                let first = self.merge_method();
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
                fields.push(field(
                    "Commit title",
                    &format!("{name} ({}{id})", self.sigil(true)),
                    false,
                    false,
                ));
            }
            Action::Checkout(_) => {
                title = "Check out pull request in this repository?";
                fields = vec![];
            }
            Action::Worktree(_) => {
                title = "Check out pull request in a separate worktree?";
                fields = vec![];
            }
            Action::Inline {
                number, path, line, ..
            } => {
                // Where the forge batches reviews, a comment can start a review
                // (held as pending) instead of notifying at once, in GitHub's words; once a
                // review is pending, the next comment joins it unless changed.
                let mut fields = vec![field("Comment", "", true, true)];
                if self.capabilities().batched_review.allowed() {
                    let waiting = self
                        .inline_drafts
                        .get(number)
                        .is_some_and(|drafts| !drafts.is_empty());
                    fields.push(if waiting {
                        choice("Send", REVIEW_COMMENT, &[REVIEW_COMMENT, SINGLE_COMMENT])
                    } else {
                        choice("Send", SINGLE_COMMENT, &[SINGLE_COMMENT, START_REVIEW])
                    });
                }
                return Ok(Some(self.form(
                    &format!("Comment on {path}:{line}"),
                    action,
                    fields,
                )));
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
                    "Resolve conversation?"
                } else {
                    "Unresolve conversation?"
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
                if definition
                    .as_ref()
                    .is_some_and(|definition| !definition.dispatch)
                {
                    return Err("Can't run manually · no workflow_dispatch trigger".into());
                }
                let refs = self.dispatch_refs();
                let mut reference = field(
                    "Branch or tag",
                    refs.first().map_or("", String::as_str),
                    true,
                    false,
                );
                reference.custom_choices = true;
                reference.choices = refs;
                let mut inputs = vec![reference];
                for input in definition
                    .map(|definition| definition.inputs)
                    .unwrap_or_default()
                {
                    let mut f = field(&input.name, &input.default, input.required, false);
                    f.choices = input.choices;
                    inputs.push(f);
                }
                // GitLab runs the project's one pipeline; elsewhere the sheet names the workflow.
                title_owned = if self.terms().run_noun == "pipeline" {
                    "Run pipeline".to_owned()
                } else {
                    format!("Run {}", self.dispatch_name(id))
                };
                return Ok(Some(self.form(&title_owned, Action::Dispatch(id), inputs)));
            }
            Action::Cancel(id) => {
                let caps = self.capabilities();
                if let Some(reason) = caps.cancel_run.reason() {
                    return Err(reason.into());
                }
                let (object, number, run) = self.sheet_run(*id).ok_or("The run is loading")?;
                if self.run_finished(run) {
                    return Err(format!("{} #{number} already finished", self.run_word()));
                }
                // Destructive: focus starts on `Keep running`, so a stray Enter cancels nothing.
                let mut form = self.form(&format!("Cancel {object}?"), action.clone(), vec![]);
                form.selected = 1;
                return Ok(Some(form));
            }
            Action::Rerun(id) => {
                let caps = self.capabilities();
                if let Some(reason) = caps
                    .rerun_all
                    .reason()
                    .filter(|_| !caps.rerun_failed.allowed())
                {
                    return Err(reason.into());
                }
                let (object, number, run) = self.sheet_run(*id).ok_or("The run is loading")?;
                if !self.run_finished(run) {
                    return Err(format!(
                        "{} #{number} is still running · x cancels it",
                        self.run_word()
                    ));
                }
                // Its jobs decide the choices: the sheet opens when they arrive (`poll_actions`).
                // A read that failed opens it with all jobs, rather than never.
                if !self.cache.contains_key(&Key::Run(*id, self.attempt))
                    && !self.cache.contains_key(&Key::Run(*id, 0))
                {
                    self.actions.pending_rerun = Some(*id);
                    return Ok(None);
                }
                let (failed, all) = self.run_jobs(*id);
                let mut scopes = Vec::new();
                if caps.rerun_failed.allowed() && failed > 0 {
                    scopes.push(format!("Failed jobs only ({failed})"));
                }
                // On a job, that job alone too: GitHub re-runs one job from its own row.
                self.actions.rerun_job = None;
                if caps.rerun_job.allowed()
                    && let Some((job, name)) = self.cursor_job(*id)
                {
                    self.actions.rerun_job = Some(job);
                    scopes.push(format!("This job ({name})"));
                }
                if caps.rerun_all.allowed() {
                    scopes.push(if all > 0 {
                        format!("All jobs ({all})")
                    } else {
                        "All jobs".into()
                    });
                }
                if scopes.is_empty() {
                    // Jobs that never loaded are not "nothing failed".
                    if self
                        .value(&Key::Run(*id, self.attempt))
                        .or_else(|| self.value(&Key::Run(*id, 0)))
                        .is_none()
                    {
                        return Err("Couldn't load this run's jobs · R to retry".into());
                    }
                    return Err(caps
                        .rerun_failed
                        .reason()
                        .unwrap_or("Nothing failed to rerun")
                        .into());
                }
                let scope_refs = scopes.iter().map(String::as_str).collect::<Vec<_>>();
                // A choice of one asks nothing: the note says what reruns (A3-04).
                let mut fields_now = Vec::new();
                if scope_refs.len() > 1 {
                    // Closed choices always hold a value: no required mark.
                    let mut jobs = choice("Jobs", scope_refs[0], &scope_refs);
                    jobs.required = false;
                    fields_now.push(jobs);
                }
                if self
                    .forge_kind()
                    .is_none_or(|kind| kind == crate::forge::provider::ProviderKind::GitHub)
                {
                    let mut debug = choice("Debug logging", "Off", &["Off", "On"]);
                    debug.required = false;
                    fields_now.push(debug);
                }
                // `Re-run #10 · …`, not `Re-run run #10`; a pipeline
                // keeps its noun.
                title_owned = format!("Re-run {}", object.strip_prefix("run ").unwrap_or(&object));
                return Ok(Some(self.form(&title_owned, action.clone(), fields_now)));
            }
            Action::Approve(id) => {
                let caps = self.capabilities();
                if let Some(reason) = caps.approve_fork_run.reason() {
                    return Err(reason.into());
                }
                let (object, number, run) = self.sheet_run(*id).ok_or("The run is loading")?;
                if run["dusty"]["needsApproval"].as_bool() != Some(true)
                    && string(run, "conclusion") != "action_required"
                {
                    return Err(format!(
                        "{} #{number} is not waiting for approval",
                        self.run_word()
                    ));
                }
                // A fork's code with the repository's secrets: focus starts on Cancel.
                let mut form = self.form(
                    &format!("{} {object}?", self.terms().approve_verb),
                    action.clone(),
                    vec![],
                );
                form.selected = 1;
                return Ok(Some(form));
            }
            // Answered by `activate_feature` above.
            Action::InlineRange { .. }
            | Action::Draft(..)
            | Action::CommitSuggestion(..)
            | Action::CreatePullRequestWeb
            | Action::Check(..)
            | Action::MergeMethod(_) => {
                return Ok(None);
            }
        }
        Ok(Some(self.form(title, action, fields)))
    }
    pub fn submit(&mut self, action: Action, values: Vec<String>) -> Result<(), String> {
        if let Action::SaveLog(job) = &action {
            return self.save_log(*job, &values);
        }
        if self.writing() {
            return Err("Wait for the current action to finish".into());
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
        if let Some(result) = self.submit_feature(&action, &values) {
            return result;
        }
        let write = match &action {
            Action::Facets => return self.apply_facets(&values),
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
            Action::Web(page, id, url) => Write::OpenInBrowser {
                kind: kind(*page),
                number: *id,
                url: Some(url.clone()),
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
                self.queue.push_back(Request {
                    key: Key::Write,
                    op,
                });
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
                // Closing an issue: `[Reason,] Comment`; the reason is the first of two.
                reason: match values
                    .first()
                    .filter(|_| *page != 1 || values.len() == 2)
                    .map(String::as_str)
                {
                    Some("Not planned") if *closed => {
                        Some(crate::forge::model::ClosedReason::NotPlanned)
                    }
                    Some("Completed") if *closed => {
                        Some(crate::forge::model::ClosedReason::Completed)
                    }
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
            Action::Review(id) => {
                // Drafts make a review on their own; otherwise a comment needs a summary.
                if values[0] != "Approve"
                    && values[1].trim().is_empty()
                    && self.review_drafts(*id).is_empty()
                {
                    return Err("A comment or request for changes needs a summary".into());
                }
                let login = crate::forge::host_state::account(
                    &crate::forge::transport::HostKey::new(&self.host),
                )
                .and_then(|account| account.login);
                let author = self
                    .value(&Key::Item(2, *id))
                    .and_then(|value| value["author"]["login"].as_str());
                if values[0] == "Approve" && login.is_some() && login.as_deref() == author {
                    return Err(
                        "Approve is unavailable on your own pull request; leave a comment instead"
                            .into(),
                    );
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
                    head: self
                        .inline_drafts
                        .get(id)
                        .and_then(|drafts| drafts.first())
                        .map(|(_, head)| head.clone()),
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
                let delete_branch = values
                    .iter()
                    .any(|value| value == "Delete it after the merge");
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
                let side = if *side == "LEFT" {
                    Side::Left
                } else {
                    Side::Right
                };
                if values.get(1).is_some_and(|send| send != SINGLE_COMMENT) {
                    let body = values[0].trim();
                    if body.is_empty() {
                        return Err("Comment is required".into());
                    }
                    let drafts = self.inline_drafts.entry(*number).or_default();
                    drafts.push((
                        crate::forge::request::InlineDraft {
                            path: path.clone(),
                            line: u64::from(*line),
                            start_line: None,
                            side,
                            body: body.to_owned(),
                        },
                        head.clone(),
                    ));
                    let count = drafts.len();
                    self.drafts.remove(&self.draft_key(&action));
                    self.notice_error = false;
                    self.notice =
                        format!("Added to your review · {}", super::drafts::pending(count));
                    self.notice_transient = true;
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
            Action::CommitSuggestion(number, thread) => {
                self.suggestion_write(*number, thread, &values)?
            }
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
            Action::Rerun(_)
                if values
                    .first()
                    .is_some_and(|value| value.starts_with("This job"))
                    && self.actions.rerun_job.is_some() =>
            {
                Write::RerunJob(self.actions.rerun_job.unwrap_or_default().into())
            }
            Action::Rerun(id) => Write::Rerun {
                run: (*id).into(),
                scope: if values
                    .first()
                    .is_some_and(|value| value.starts_with("Failed"))
                {
                    RerunScope::Failed
                } else {
                    RerunScope::All
                },
                // `Jobs` is left out when there is one scope: the toggle is the only `On`.
                debug: values.iter().any(|value| value == "On"),
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
                    // `./artifacts` is the root's `artifacts`, not `root/./artifacts`.
                    let relative = std::path::Path::new(folder)
                        .components()
                        .filter(|part| !matches!(part, std::path::Component::CurDir))
                        .collect::<PathBuf>();
                    self.root.join(relative)
                };
                Write::DownloadArtifact {
                    artifact: (*artifact).into(),
                    name: name.clone(),
                    directory,
                }
            }
            _ => return Err("This action has no form".into()),
        };
        // A closing comment is posted first; the state change follows once it is saved.
        let closing = match &action {
            Action::State(1, id, true) => values
                .last()
                .filter(|comment| !comment.trim().is_empty())
                .map(|comment| (*id, comment.clone())),
            _ => None,
        };
        self.then_write = None;
        let write = match closing {
            Some((number, body)) => {
                self.then_write = Some(write);
                Write::Comment {
                    kind: kind(1),
                    number,
                    body,
                }
            }
            None => write,
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
