//! GitHub workspace state. GitHub CLI owns authentication; the existing forge worker
//! owns process limits and cancellation. Every response is keyed to its requested item.
mod actions;
pub(super) mod view;
mod workflow;
use super::{Field, Form, FormAction};
use crate::editor::input::{KeyCode, KeyEvent};
use crate::forge::{ForgeJobs, ForgeReply};
pub(super) use actions::Action;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum Key {
    Repository,
    List(usize),
    Item(usize, u64),
    Timeline(u64),
    Threads(u64),
    Diff(u64),
    Commit(String),
    Workflows,
    Workflow(u64, String),
    Runs(u64),
    Run(u64, u32),
    Log(u64),
    Write,
    WorktreeWrite(u64),
}
struct Reply {
    text: String,
    value: Value,
}
struct Request {
    key: Key,
    args: Vec<String>,
}
pub(super) struct Workspace {
    root: PathBuf,
    jobs: ForgeJobs,
    next: u64,
    active: Option<Request>,
    queue: VecDeque<Request>,
    cache: HashMap<Key, Result<Reply, String>>,
    repo: String,
    host: String,
    page: usize,
    selected: [usize; 2],
    opened: Option<(usize, u64)>,
    detail_scroll: usize,
    horizontal: usize,
    queries: [String; 2],
    states: [String; 2],
    focus: usize,
    last_list: usize,
    detail_cursor: usize,
    detail_rows: Vec<view::Line>,
    pr_tab: usize,
    selected_commit: Option<String>,
    workflow_selected: usize,
    run_selected: usize,
    run_id: Option<u64>,
    attempt: u32,
    expanded_jobs: BTreeSet<u64>,
    expanded_steps: BTreeSet<(u64, u64)>,
    viewed: BTreeSet<(u64, String)>,
    collapsed: BTreeSet<(u64, String)>,
    drafts: BTreeMap<String, Vec<String>>,
    notice: String,
    writing_draft: Option<String>,
}
impl Workspace {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            jobs: ForgeJobs::default(),
            next: 0,
            active: None,
            queue: VecDeque::new(),
            cache: HashMap::new(),
            repo: String::new(),
            host: String::new(),
            page: 0,
            selected: [0; 2],
            opened: None,
            detail_scroll: 0,
            horizontal: 0,
            queries: Default::default(),
            states: ["open".into(), "open".into()],
            focus: 0,
            last_list: 0,
            detail_cursor: 0,
            detail_rows: Vec::new(),
            pr_tab: 0,
            selected_commit: None,
            workflow_selected: 0,
            run_selected: 0,
            run_id: None,
            attempt: 0,
            expanded_jobs: BTreeSet::new(),
            expanded_steps: BTreeSet::new(),
            viewed: BTreeSet::new(),
            collapsed: BTreeSet::new(),
            drafts: BTreeMap::new(),
            notice: String::new(),
            writing_draft: None,
        }
    }
    pub fn writing(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|request| matches!(request.key, Key::Write | Key::WorktreeWrite(_)))
            || self
                .queue
                .iter()
                .any(|request| matches!(request.key, Key::Write | Key::WorktreeWrite(_)))
    }
    pub fn page(&mut self, page: usize) {
        self.page = page;
        self.focus = 0;
        self.detail_cursor = 0;
        self.detail_scroll = 0;
        self.horizontal = 0;
        self.opened = None;
        self.queue
            .retain(|request| matches!(request.key, Key::Write | Key::WorktreeWrite(_)));
        if page != 0 {
            self.ensure();
        }
    }
    fn cli(&self, args: &[&str]) -> Vec<String> {
        let mut args = args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        args.extend(["--repo".into(), format!("{}/{}", self.host, self.repo)]);
        args
    }
    fn api(&self, path: &str) -> Vec<String> {
        vec![
            "api".into(),
            "--hostname".into(),
            self.host.clone(),
            format!("repos/{}/{}", self.repo, path),
        ]
    }
    fn graphql(&self, query: &str) -> Vec<String> {
        let (owner, name) = self.repo.split_once('/').unwrap_or_default();
        vec![
            "api".into(),
            "graphql".into(),
            "--hostname".into(),
            self.host.clone(),
            "-f".into(),
            format!("query={query}"),
            "-f".into(),
            format!("owner={owner}"),
            "-f".into(),
            format!("name={name}"),
        ]
    }
    fn enqueue(&mut self, key: Key, args: Vec<String>) {
        if self.cache.contains_key(&key)
            || self
                .active
                .as_ref()
                .is_some_and(|request| request.key == key)
            || self.queue.iter().any(|request| request.key == key)
        {
            return;
        }
        self.queue.push_back(Request { key, args });
    }
    fn value(&self, key: &Key) -> Option<&Value> {
        self.cache.get(key)?.as_ref().ok().map(|reply| &reply.value)
    }
    fn raw(&self, key: &Key) -> Option<&str> {
        self.cache
            .get(key)?
            .as_ref()
            .ok()
            .map(|reply| reply.text.as_str())
    }
    fn items(&self) -> Vec<&Value> {
        self.value(&Key::List(self.page))
            .and_then(Value::as_array)
            .map(|items| items.iter().collect())
            .unwrap_or_default()
    }
    fn workflows(&self) -> Vec<&Value> {
        self.value(&Key::Workflows)
            .and_then(|value| value["workflows"].as_array())
            .map(|items| items.iter().collect())
            .unwrap_or_default()
    }
    fn workflow_id(&self) -> u64 {
        self.workflow_selected
            .checked_sub(1)
            .and_then(|index| self.workflows().get(index).map(|value| number(value, "id")))
            .unwrap_or(0)
    }
    fn runs(&self) -> Vec<&Value> {
        self.value(&Key::Runs(self.workflow_id()))
            .and_then(Value::as_array)
            .map(|items| items.iter().collect())
            .unwrap_or_default()
    }
    pub fn current_page(&self) -> usize {
        self.page
    }
    fn number(&self) -> u64 {
        if let Some((page, number)) = self.opened
            && page == self.page
        {
            return number;
        }
        self.items()
            .get(self.selected[self.page.saturating_sub(1).min(1)])
            .map(|value| number(value, "number"))
            .unwrap_or(0)
    }
    fn item(&self) -> Option<&Value> {
        self.value(&Key::Item(self.page, self.number()))
    }
    fn ensure(&mut self) {
        if self.repo.is_empty() {
            self.enqueue(
                Key::Repository,
                vec![
                    "repo".into(),
                    "view".into(),
                    "--json".into(),
                    "nameWithOwner,url,defaultBranchRef".into(),
                ],
            );
            return;
        }
        match self.page {
            1 | 2 => {
                let kind = if self.page == 1 { "issue" } else { "pr" };
                let at = self.page - 1;
                self.enqueue(
                    Key::List(self.page),
                    self.cli(&[
                        kind,
                        "list",
                        "--state",
                        &self.states[at],
                        "--search",
                        &self.queries[at],
                        "--limit",
                        "100",
                        "--json",
                        "number,title,state,author,labels,updatedAt",
                    ]),
                );
                let id = self.number();
                if id == 0 {
                    return;
                }
                let fields = if self.page == 1 {
                    "id,number,title,body,state,author,createdAt,updatedAt,assignees,labels,milestone,projectItems,closedByPullRequestsReferences"
                } else {
                    "id,number,title,body,state,author,createdAt,assignees,labels,milestone,projectItems,baseRefName,headRefName,headRefOid,isDraft,commits,files,reviewDecision,mergeStateStatus,mergeable,reviews,statusCheckRollup,closingIssuesReferences"
                };
                self.enqueue(
                    Key::Item(self.page, id),
                    self.cli(&[kind, "view", &id.to_string(), "--json", fields]),
                );
                let mut timeline = self.api(&format!("issues/{id}/timeline?per_page=100"));
                timeline.extend(["--paginate".into(), "--slurp".into()]);
                self.enqueue(Key::Timeline(id), timeline);
                if self.page == 2 {
                    if self.pr_tab == 3 {
                        self.enqueue(
                            Key::Diff(id),
                            self.cli(&["pr", "diff", &id.to_string(), "--color", "never"]),
                        );
                    }
                    let query = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){pullRequest(number:$number){reviewThreads(first:100){nodes{id isResolved comments(first:100){nodes{author{login} body path line originalLine diffSide commit{oid}}}}}}}}";
                    let mut args = self.graphql(query);
                    args.extend(["-F".into(), format!("number={id}")]);
                    self.enqueue(Key::Threads(id), args);
                }
            }
            3 => {
                self.enqueue(Key::Workflows, self.api("actions/workflows?per_page=100"));
                let workflow = self.workflow_id();
                let mut args=self.cli(&["run","list","--limit","100","--json","databaseId,displayTitle,workflowName,status,conclusion,headBranch,headSha,event,createdAt,attempt,workflowDatabaseId"]);
                if workflow != 0 {
                    args.extend(["--workflow".into(), workflow.to_string()]);
                    self.enqueue(
                        Key::Workflow(workflow, String::new()),
                        self.cli(&["workflow", "view", &workflow.to_string(), "--yaml"]),
                    );
                }
                self.enqueue(Key::Runs(workflow), args);
                if let Some(id) = self.run_id {
                    let mut args=self.cli(&["run","view",&id.to_string(),"--json","databaseId,displayTitle,workflowName,status,conclusion,headBranch,headSha,event,attempt,jobs,workflowDatabaseId"]);
                    if self.attempt > 0 {
                        args.extend(["--attempt".into(), self.attempt.to_string()]);
                    }
                    self.enqueue(Key::Run(id, self.attempt), args);
                    if let Some(run) = self.value(&Key::Run(id, self.attempt)) {
                        let workflow = number(run, "workflowDatabaseId");
                        let revision = string(run, "headSha").to_owned();
                        self.enqueue(
                            Key::Workflow(workflow, revision.clone()),
                            self.cli(&[
                                "workflow",
                                "view",
                                &workflow.to_string(),
                                "--yaml",
                                "--ref",
                                &revision,
                            ]),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(event) = self.jobs.try_reply() {
            if event.request_id != self.next {
                continue;
            }
            let Some(request) = self.active.take() else {
                continue;
            };
            if let ForgeReply::Workspace(result) = event.reply {
                changed = true;
                if matches!(request.key, Key::Write | Key::WorktreeWrite(_)) {
                    match result {
                        Ok(text) => {
                            self.notice = if text.trim().is_empty() {
                                "Saved on GitHub".into()
                            } else {
                                text.trim().into()
                            };
                            if let Some(key) = self.writing_draft.take() {
                                self.drafts.remove(&key);
                            }
                            self.cache.retain(|key, _| matches!(key, Key::Repository));
                            self.queue.clear();
                        }
                        Err(error) => self.notice = format!("{error} — your draft is preserved"),
                    }
                } else {
                    let parsed = result.and_then(|text| {
                        let value = if matches!(
                            request.key,
                            Key::Diff(_) | Key::Commit(_) | Key::Workflow(..) | Key::Log(_)
                        ) {
                            Value::Null
                        } else {
                            serde_json::from_str(&text)
                                .map_err(|error| format!("Invalid GitHub response: {error}"))?
                        };
                        Ok(Reply { value, text })
                    });
                    if request.key == Key::Repository
                        && let Ok(reply) = &parsed
                    {
                        self.repo = string(&reply.value, "nameWithOwner").into();
                        self.host = string(&reply.value, "url")
                            .strip_prefix("https://")
                            .unwrap_or("")
                            .split('/')
                            .next()
                            .unwrap_or("")
                            .into();
                    }
                    self.cache.insert(request.key, parsed);
                }
                self.ensure();
            }
        }
        if self.active.is_none()
            && let Some(request) = self.queue.pop_front()
        {
            self.next += 1;
            let started = match request.key {
                Key::WorktreeWrite(number) => self.jobs.workspace_worktree(
                    self.root.clone(),
                    self.next,
                    number,
                    request.args[0].clone(),
                ),
                _ => self
                    .jobs
                    .workspace(self.root.clone(), self.next, request.args.clone()),
            };
            match started {
                Ok(()) => self.active = Some(request),
                Err(error) => {
                    self.cache.insert(request.key, Err(error.clone()));
                    self.notice = error;
                }
            }
            changed = true;
        }
        changed
    }
    pub fn save_draft(&mut self, action: &Action, values: Vec<String>) {
        self.drafts.insert(format!("{action:?}"), values);
    }
    fn form(&self, title: &str, action: Action, mut fields: Vec<Field>) -> Form {
        if let Some(draft) = self.drafts.get(&format!("{action:?}")) {
            for (field, value) in fields.iter_mut().zip(draft) {
                field.input.input = value.clone();
                field.input.cursor = value.len();
            }
        }
        Form {
            title: format!("{} · {}", title, self.repo),
            action: FormAction::Github(action),
            fields,
            selected: 0,
        }
    }
    pub fn event(&mut self, key: &KeyEvent) -> Result<Option<Form>, String> {
        if key.modifiers.control {
            match key.code {
                KeyCode::Char('h') => self.focus = self.last_list,
                KeyCode::Char('l') => self.focus = 2,
                KeyCode::Char('j') if self.page == 3 && self.focus < 2 => self.focus = 1,
                KeyCode::Char('k') if self.page == 3 && self.focus < 2 => self.focus = 0,
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Char('q') => return Err("close".into()),
                KeyCode::Escape if self.focus == 2 => self.focus = self.last_list,
                KeyCode::Char('r') => {
                    self.cache.clear();
                    self.queue.clear();
                    self.ensure();
                }
                KeyCode::Char('/') => return self.activate(Action::Filter),
                KeyCode::Char('n') if self.page == 1 => return self.activate(Action::CreateIssue),
                KeyCode::Char('n') if self.page == 2 => {
                    return self.activate(Action::CreatePullRequest);
                }
                KeyCode::PageDown if self.focus == 2 => {
                    self.detail_cursor = self
                        .detail_cursor
                        .saturating_add(20)
                        .min(self.detail_rows.len().saturating_sub(1))
                }
                KeyCode::PageUp if self.focus == 2 => {
                    self.detail_cursor = self.detail_cursor.saturating_sub(20)
                }
                KeyCode::Char('c') if self.page < 3 => {
                    return self.activate(Action::Comment(self.page, self.number()));
                }
                KeyCode::Char('e') if self.page < 3 => {
                    return self.activate(Action::Edit(self.page, self.number()));
                }
                KeyCode::Char('H' | 'L') if self.page == 2 => {
                    self.pr_tab =
                        (self.pr_tab + if key.code == KeyCode::Char('L') { 1 } else { 3 }) % 4;
                    self.detail_cursor = 0;
                    self.ensure();
                }
                KeyCode::Char('h') | KeyCode::Left if self.focus == 2 => {
                    self.horizontal = self.horizontal.saturating_sub(8)
                }
                KeyCode::Char('l') | KeyCode::Right if self.focus == 2 => {
                    self.horizontal = self.horizontal.saturating_add(8).min(4096)
                }
                KeyCode::Char('1') => self.focus = 0,
                KeyCode::Char('2') if self.page == 3 => self.focus = 1,
                KeyCode::Tab => {
                    if self.focus != 2 {
                        self.last_list = self.focus;
                        self.focus = 2;
                    }
                    let actions = self
                        .detail_rows
                        .iter()
                        .enumerate()
                        .filter_map(|(i, row)| row.action.as_ref().map(|_| i))
                        .collect::<Vec<_>>();
                    if !actions.is_empty() {
                        self.detail_cursor = if key.modifiers.shift {
                            *actions
                                .iter()
                                .rev()
                                .find(|&&i| i < self.detail_cursor)
                                .unwrap_or(actions.last().unwrap())
                        } else {
                            *actions
                                .iter()
                                .find(|&&i| i > self.detail_cursor)
                                .unwrap_or(&actions[0])
                        };
                    }
                }
                KeyCode::Char('j') | KeyCode::Down | KeyCode::Char('k') | KeyCode::Up => {
                    let delta = if matches!(key.code, KeyCode::Char('j') | KeyCode::Down) {
                        1
                    } else {
                        -1
                    };
                    if self.focus == 2 {
                        self.detail_cursor = self
                            .detail_cursor
                            .saturating_add_signed(delta)
                            .min(self.detail_rows.len().saturating_sub(1));
                    } else if self.page < 3 {
                        self.opened = None;
                        self.selected_commit = None;
                        let max = self.items().len().saturating_sub(1);
                        let at = self.page - 1;
                        self.selected[at] = self.selected[at].saturating_add_signed(delta).min(max);
                        self.detail_cursor = 0;
                        self.ensure();
                    } else if self.focus == 0 {
                        self.workflow_selected = self
                            .workflow_selected
                            .saturating_add_signed(delta)
                            .min(self.workflows().len());
                        self.run_selected = 0;
                        self.run_id = None;
                        self.detail_cursor = 0;
                        self.ensure();
                    } else {
                        self.run_selected = self
                            .run_selected
                            .saturating_add_signed(delta)
                            .min(self.runs().len().saturating_sub(1));
                        self.run_id = self
                            .runs()
                            .get(self.run_selected)
                            .map(|run| number(run, "databaseId"));
                        self.attempt = 0;
                        self.detail_cursor = 0;
                        self.ensure();
                    }
                }
                KeyCode::Enter => {
                    if self.focus == 2 {
                        if let Some(action) = self
                            .detail_rows
                            .get(self.detail_cursor)
                            .and_then(|row| row.action.clone())
                        {
                            return self.activate(action);
                        }
                    } else {
                        if self.page == 3 && self.focus == 1 {
                            self.run_id = self
                                .runs()
                                .get(self.run_selected)
                                .map(|run| number(run, "databaseId"));
                            self.attempt = 0;
                            self.ensure();
                        }
                        self.last_list = self.focus;
                        self.focus = 2;
                        self.detail_cursor = 0;
                    }
                }
                _ => {}
            }
        }
        if self.focus < 2 {
            self.last_list = self.focus;
        }
        Ok(None)
    }
}
fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn number(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or(0)
}
fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map(Vec::as_slice).unwrap_or_default()
}
fn login(value: &Value) -> &str {
    value["author"]["login"]
        .as_str()
        .or_else(|| value["user"]["login"].as_str())
        .or_else(|| value["actor"]["login"].as_str())
        .unwrap_or("unknown")
}
