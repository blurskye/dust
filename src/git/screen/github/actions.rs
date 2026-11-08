use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::git::screen) enum Action {
    OpenItem(usize, u64),
    Filter,
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
    Resolve(String, bool),
    Viewed(u64, String),
    Collapse(u64, String),
    Run(u64),
    Job(u64),
    Step(u64, u64),
    FullLog(u64),
    Dispatch(u64),
    Cancel(u64),
    Rerun(u64),
    Attempt(u32),
}
impl Action {
    pub fn writes(&self) -> bool {
        !matches!(
            self,
            Self::OpenItem(..)
                | Self::Filter
                | Self::Commit(_)
                | Self::PrTab(_)
                | Self::Viewed(..)
                | Self::Collapse(..)
                | Self::Run(_)
                | Self::Job(_)
                | Self::Step(..)
                | Self::FullLog(_)
                | Self::Attempt(_)
        )
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
fn kind(page: usize) -> &'static str {
    if page == 1 { "issue" } else { "pr" }
}
impl Workspace {
    pub(in crate::git::screen) fn activate(
        &mut self,
        action: Action,
    ) -> Result<Option<Form>, String> {
        if self.repo.is_empty() {
            return Err(
                "GitHub repository is not loaded; check gh authentication and refresh".into(),
            );
        }
        if action != Action::CreatePullRequest {
            self.pending_pr = false;
            self.ready_form = None;
        }
        let fields;
        let title;
        match &action {
            Action::OpenItem(page, id) => {
                self.page = *page;
                self.opened = Some((*page, *id));
                self.focus = 2;
                self.detail_cursor = 0;
                self.ensure();
                return Ok(None);
            }
            Action::Commit(sha) => {
                git2::Oid::from_str(sha).map_err(|_| "Invalid commit identifier")?;
                self.selected_commit = Some(sha.clone());
                let mut args = self.api(&format!("commits/{sha}"));
                args.extend([
                    "--header".into(),
                    "Accept: application/vnd.github.diff".into(),
                ]);
                self.enqueue(Key::Commit(sha.clone()), args);
                return Ok(None);
            }
            Action::PrTab(tab) => {
                self.pr_tab = *tab;
                self.detail_cursor = 0;
                self.ensure();
                return Ok(None);
            }
            Action::Viewed(id, path) => {
                let value = (*id, path.clone());
                if !self.viewed.remove(&value) {
                    self.viewed.insert(value);
                }
                return Ok(None);
            }
            Action::Collapse(id, path) => {
                let value = (*id, path.clone());
                if !self.collapsed.remove(&value) {
                    self.collapsed.insert(value);
                }
                return Ok(None);
            }
            Action::Run(id) => {
                self.page = 3;
                self.run_id = Some(*id);
                self.attempt = 0;
                self.last_list = 1;
                self.focus = 2;
                self.detail_cursor = 0;
                self.ensure();
                return Ok(None);
            }
            Action::Attempt(attempt) => {
                self.attempt = *attempt;
                self.detail_cursor = 0;
                self.ensure();
                return Ok(None);
            }
            Action::Job(id) => {
                if !self.expanded_jobs.remove(id) {
                    self.expanded_jobs.insert(*id);
                }
                return Ok(None);
            }
            Action::Step(job, step) => {
                if !self.expanded_steps.remove(&(*job, *step)) {
                    self.expanded_steps.insert((*job, *step));
                }
                self.ensure();
                return Ok(None);
            }
            Action::FullLog(job) => {
                if !self.full_logs.remove(job) {
                    self.full_logs.insert(*job);
                }
                self.ensure();
                return Ok(None);
            }
            Action::Filter => {
                title = "Filter";
                let index = self.page.saturating_sub(1).min(1);
                fields = vec![
                    field("Search (GitHub query)", &self.queries[index], false, false),
                    choice("State", &self.states[index], &["open", "closed", "all"]),
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
                title = "Create linked branch on GitHub (current checkout stays unchanged)";
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
            Action::State(_, _, closed) => {
                title = if *closed {
                    "Close this item?"
                } else {
                    "Reopen this item?"
                };
                fields = vec![];
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
            Action::Review(_) => {
                title = "Submit review";
                fields = vec![
                    choice(
                        "Decision",
                        "Comment",
                        &["Comment", "Approve", "Request changes"],
                    ),
                    field("Review body", "", false, true),
                ];
            }
            Action::Merge(_, _) => {
                title = "Merge pull request (server branch rules apply)";
                fields = vec![choice("Method", "Squash", &["Squash", "Merge", "Rebase"])];
            }
            Action::Checkout(_) => {
                title = "Check out pull request in this repository?";
                fields = vec![];
            }
            Action::Worktree(_) => {
                title = "Check out pull request in a separate worktree?";
                fields = vec![];
            }
            Action::Inline { path, line, .. } => {
                return Ok(Some(self.form(
                    &format!("Comment on {path}:{line}"),
                    action,
                    vec![field("Comment", "", true, true)],
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
                let yaml = self
                    .raw(&Key::Workflow(*id, String::new()))
                    .ok_or("Select a workflow and wait for its definition")?;
                let definition = workflow::Definition::parse(yaml)?;
                if !definition.dispatch {
                    return Err("This workflow has no workflow_dispatch trigger".into());
                }
                let branch = self
                    .value(&Key::Repository)
                    .and_then(|value| value["defaultBranchRef"]["name"].as_str())
                    .unwrap_or("");
                let mut inputs = vec![field("Branch or tag", branch, true, false)];
                for input in definition.inputs {
                    let mut f = field(&input.name, &input.default, input.required, false);
                    f.choices = input.choices;
                    inputs.push(f);
                }
                return Ok(Some(self.form("Run workflow", action, inputs)));
            }
            Action::Cancel(_) => {
                title = "Cancel workflow run?";
                fields = vec![];
            }
            Action::Rerun(_) => {
                title = "Re-run workflow";
                fields = vec![choice("Jobs", "Failed jobs", &["Failed jobs", "All jobs"])];
            }
        }
        Ok(Some(self.form(title, action, fields)))
    }
    pub fn submit(&mut self, action: Action, values: Vec<String>) -> Result<(), String> {
        if self.writing() {
            return Err("Wait for the current GitHub write".into());
        }
        self.save_draft(&action, values.clone());
        let mut args;
        match &action {
            Action::Filter => {
                let index = self.page.saturating_sub(1).min(1);
                self.queries[index] = values[0].clone();
                self.states[index] = values[1].clone();
                self.selected[index] = 0;
                self.cache.remove(&Key::List(self.page));
                self.ensure();
                return Ok(());
            }
            Action::CreateIssue => {
                args = self.cli(&[
                    "issue", "create", "--title", &values[0], "--body", &values[1],
                ]);
                for (flag, value) in [("--label", &values[2]), ("--assignee", &values[3])] {
                    if !value.is_empty() {
                        args.extend([flag.into(), value.clone()]);
                    }
                }
            }
            Action::ConfirmPullRequest(request) => {
                self.writing_draft = Some(format!("{:?}", Action::CreatePullRequest));
                self.queue.push_back(Request {
                    key: Key::Write,
                    args: Vec::new(),
                    pull_request: Some((**request).clone()),
                });
                self.notice = if request.push.is_some() {
                    "Pushing the reviewed branch, then creating the PR…"
                } else {
                    "Creating pull request…"
                }
                .into();
                return Ok(());
            }
            Action::Develop(id) => {
                args = self.cli(&[
                    "issue",
                    "develop",
                    &id.to_string(),
                    "--name",
                    &values[0],
                    "--base",
                    &values[1],
                ]);
            }
            Action::Edit(page, id) => {
                args = self.cli(&[
                    kind(*page),
                    "edit",
                    &id.to_string(),
                    "--title",
                    &values[0],
                    "--body",
                    &values[1],
                ])
            }
            Action::Comment(page, id) => {
                args = self.cli(&[
                    kind(*page),
                    "comment",
                    &id.to_string(),
                    "--body",
                    &values[0],
                ])
            }
            Action::State(page, id, closed) => {
                args = self.cli(&[
                    kind(*page),
                    if *closed { "close" } else { "reopen" },
                    &id.to_string(),
                ])
            }
            Action::Metadata(page, id, name) => {
                let value = self
                    .value(&Key::Item(*page, *id))
                    .ok_or("Refresh the item before editing")?;
                args = self.cli(&[kind(*page), "edit", &id.to_string()]);
                let new = values[0]
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .collect::<BTreeSet<_>>();
                let (old, flag) = match *name {
                    "labels" => (
                        array(value, "labels")
                            .iter()
                            .map(|v| string(v, "name"))
                            .collect::<BTreeSet<_>>(),
                        "label",
                    ),
                    "assignees" => (
                        array(value, "assignees")
                            .iter()
                            .map(|v| string(v, "login"))
                            .collect(),
                        "assignee",
                    ),
                    "projects" => (
                        array(value, "projectItems")
                            .iter()
                            .filter_map(|v| {
                                v["title"]
                                    .as_str()
                                    .or_else(|| v["project"]["title"].as_str())
                            })
                            .collect(),
                        "project",
                    ),
                    "reviewers" => (BTreeSet::new(), "reviewer"),
                    "milestone" => {
                        if values[0].is_empty() {
                            args.push("--remove-milestone".into());
                        } else {
                            args.extend(["--milestone".into(), values[0].clone()]);
                        }
                        (BTreeSet::new(), "")
                    }
                    _ => return Err("Unknown metadata field".into()),
                };
                if !flag.is_empty() {
                    for removed in old.difference(&new) {
                        args.extend([format!("--remove-{flag}"), (*removed).into()]);
                    }
                    for added in new.difference(&old) {
                        args.extend([format!("--add-{flag}"), (*added).into()]);
                    }
                }
                if args.len() == 5 {
                    return Ok(());
                }
            }
            Action::Subscription(page, id) => {
                let node = string(
                    self.value(&Key::Item(*page, *id))
                        .ok_or("Item is loading")?,
                    "id",
                );
                args=self.graphql("mutation($subject:ID!,$state:SubscriptionState!){updateSubscription(input:{subscribableId:$subject,state:$state}){subscribable{id}}}");
                args.extend([
                    "-f".into(),
                    format!("subject={node}"),
                    "-f".into(),
                    format!("state={}", values[0]),
                ]);
            }
            Action::Review(id) => {
                if values[0] != "Approve" && values[1].trim().is_empty() {
                    return Err("A comment or request for changes needs a review body".into());
                }
                args = self.cli(&[
                    "pr",
                    "review",
                    &id.to_string(),
                    match values[0].as_str() {
                        "Approve" => "--approve",
                        "Request changes" => "--request-changes",
                        _ => "--comment",
                    },
                ]);
                if !values[1].is_empty() {
                    args.extend(["--body".into(), values[1].clone()]);
                }
            }
            Action::Merge(id, head) => {
                if head.is_empty() {
                    return Err("Refresh the pull request before merging".into());
                }
                args = self.cli(&[
                    "pr",
                    "merge",
                    &id.to_string(),
                    "--match-head-commit",
                    head,
                    match values[0].as_str() {
                        "Merge" => "--merge",
                        "Rebase" => "--rebase",
                        _ => "--squash",
                    },
                ]);
            }
            Action::Checkout(id) => args = self.cli(&["pr", "checkout", &id.to_string()]),
            Action::Worktree(id) => {
                self.queue.push_back(Request {
                    key: Key::WorktreeWrite(*id),
                    args: vec![format!("{}/{}", self.host, self.repo)],
                    pull_request: None,
                });
                self.writing_draft = Some(format!("{action:?}"));
                return Ok(());
            }
            Action::Inline {
                number,
                path,
                line,
                side,
                head,
            } => {
                args = self.api(&format!("pulls/{number}/comments"));
                args.extend([
                    "--method".into(),
                    "POST".into(),
                    "-f".into(),
                    format!("body={}", values[0]),
                    "-f".into(),
                    format!("path={path}"),
                    "-F".into(),
                    format!("line={line}"),
                    "-f".into(),
                    format!("side={side}"),
                    "-f".into(),
                    format!("commit_id={head}"),
                ]);
            }
            Action::Resolve(id, resolved) => {
                let query = if *resolved {
                    "mutation($thread:ID!){resolveReviewThread(input:{threadId:$thread}){thread{id isResolved}}}"
                } else {
                    "mutation($thread:ID!){unresolveReviewThread(input:{threadId:$thread}){thread{id isResolved}}}"
                };
                args = self.graphql(query);
                args.extend(["-f".into(), format!("thread={id}")]);
            }
            Action::Dispatch(id) => {
                let definition = workflow::Definition::parse(
                    self.raw(&Key::Workflow(*id, String::new()))
                        .ok_or("Workflow definition is not loaded")?,
                )?;
                args = self.cli(&["workflow", "run", &id.to_string(), "--ref", &values[0]]);
                for (input, value) in definition.inputs.iter().zip(values.iter().skip(1)) {
                    if input.required || !value.is_empty() {
                        args.extend(["--raw-field".into(), format!("{}={value}", input.name)]);
                    }
                }
            }
            Action::Cancel(id) => args = self.cli(&["run", "cancel", &id.to_string()]),
            Action::Rerun(id) => {
                args = self.cli(&["run", "rerun", &id.to_string()]);
                if values[0] == "Failed jobs" {
                    args.push("--failed".into());
                }
            }
            _ => return Err("This action has no form".into()),
        }
        self.writing_draft = Some(format!("{action:?}"));
        self.cache.remove(&Key::Write);
        self.enqueue(Key::Write, args);
        self.notice = "Sending to GitHub…".into();
        Ok(())
    }
}
