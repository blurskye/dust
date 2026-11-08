//! The shared native Git application. Hosts own terminal lifecycle and opening files/workspaces.
mod github;
mod jobs;
mod preview;
mod render;
mod tutor;

use super::*;
use crate::editor::input::{KeyCode, KeyEvent};
use crate::ui::{Frame, KeyInputKind, PromptKind, PromptState, UiEvent};
use jobs::{Jobs, Request};
use preview::Preview;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Eq, PartialEq)]
pub enum ScreenOutcome {
    Continue,
    Close,
    OpenFile(PathBuf),
    OpenRepository(PathBuf),
}

#[derive(Clone)]
struct Row {
    label: String,
    value: PathBuf,
    directory: bool,
}

#[derive(Clone)]
enum FormAction {
    Commit,
    Stash,
    Worktree,
    Confirm(Vec<OsString>),
    Github(github::Action),
    Command,
    Conflict(PathBuf),
    ContinueMerge,
}
struct Field {
    label: String,
    input: PromptState,
    required: bool,
    multiline: bool,
    choices: Vec<String>,
    custom_choices: bool,
    read_only: bool,
}
impl Field {
    fn new(label: &str, value: String) -> Self {
        let mut input = PromptState::new(PromptKind::Text);
        input.cursor = value.len();
        input.input = value;
        Self {
            label: label.into(),
            input,
            required: true,
            multiline: false,
            choices: Vec::new(),
            custom_choices: false,
            read_only: false,
        }
    }
}
struct Form {
    title: String,
    action: FormAction,
    fields: Vec<Field>,
    selected: usize,
}

/// One controller and renderer, used in the editor and the standalone executable.
pub struct GitScreen {
    root: PathBuf,
    page: usize,
    github: github::Workspace,
    status: GitStatusWorker,
    snapshot: Option<GitStatusSnapshot>,
    branches: GitBranchWorker,
    refs: Option<GitBranchList>,
    mutation: GitMutationWorker,
    jobs: Jobs,
    history: Vec<(String, String)>,
    history_target: Option<(PathBuf, OsString)>,
    stashes: Vec<String>,
    tags: Vec<String>,
    remotes: Vec<String>,
    focus: usize,
    last_list: usize,
    subview: usize,
    selected: [usize; 7],
    offsets: [usize; 4],
    collapsed: BTreeSet<PathBuf>,
    preview: Preview,
    preview_line: usize,
    preview_column: usize,
    split_diff: bool,
    preview_rows: usize,
    staged: bool,
    all_files: bool,
    files: BTreeSet<PathBuf>,
    tutor: Option<tutor::Session>,
    branch: String,
    notice: String,
    form: Option<Form>,
    help: bool,
    busy: bool,
    branch_mutation: bool,
    refreshing: bool,
    /// The embedded host updates this before every input event.
    pub write_refusal: Option<String>,
}

impl GitScreen {
    pub fn open(path: &Path) -> Result<Self, String> {
        let repo = git2::Repository::discover(path).map_err(|e| e.message().to_owned())?;
        let root = repo
            .workdir()
            .ok_or("Bare repositories do not have a working tree")?
            .to_path_buf();
        let mut screen = Self {
            github: github::Workspace::new(root.clone()),
            root,
            page: 0,
            status: GitStatusWorker::default(),
            snapshot: None,
            branches: GitBranchWorker::default(),
            refs: None,
            mutation: GitMutationWorker::default(),
            jobs: Jobs::default(),
            history: Vec::new(),
            history_target: None,
            stashes: Vec::new(),
            tags: Vec::new(),
            remotes: Vec::new(),
            focus: 0,
            last_list: 0,
            subview: 0,
            selected: [0; 7],
            offsets: [0; 4],
            collapsed: BTreeSet::new(),
            preview: Preview::default(),
            preview_line: 0,
            preview_column: 0,
            split_diff: true,
            preview_rows: 0,
            staged: false,
            all_files: false,
            files: BTreeSet::new(),
            tutor: None,
            branch: String::new(),
            notice: String::new(),
            form: None,
            help: false,
            busy: false,
            branch_mutation: false,
            refreshing: false,
            write_refusal: None,
        };
        screen.refresh()?;
        Ok(screen)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn busy(&self) -> bool {
        self.busy || self.github.writing()
    }
    fn slot(&self, pane: usize) -> usize {
        if pane == 1 {
            1 + self.subview
        } else if pane >= 2 {
            pane + 3
        } else {
            0
        }
    }
    fn rows(&self, pane: usize) -> Vec<Row> {
        let plain = |label: String, value: PathBuf| Row {
            label,
            value,
            directory: false,
        };
        match pane {
            0 => {
                let mut rows = Vec::new();
                let mut seen = BTreeSet::new();
                if let Some(snapshot) = &self.snapshot {
                    let mut paths = snapshot
                        .entries
                        .iter()
                        .map(|entry| (entry.relative_path.clone(), entry.change_code()))
                        .collect::<std::collections::BTreeMap<_, _>>();
                    if self.all_files {
                        for path in &self.files {
                            paths.entry(path.clone()).or_insert("  ".into());
                        }
                    }
                    for (path, code) in paths {
                        let parts = path.components().collect::<Vec<_>>();
                        let mut parent = PathBuf::new();
                        let mut hidden = false;
                        for part in parts.iter().take(parts.len().saturating_sub(1)) {
                            parent.push(part.as_os_str());
                            if seen.insert(parent.clone()) {
                                let depth = parent.components().count().saturating_sub(1);
                                rows.push(Row {
                                    label: format!(
                                        "{}{} {}/",
                                        "  ".repeat(depth),
                                        if self.collapsed.contains(&parent) {
                                            "▸"
                                        } else {
                                            "▾"
                                        },
                                        part.as_os_str().to_string_lossy()
                                    ),
                                    value: parent.clone(),
                                    directory: true,
                                });
                            }
                            if self.collapsed.contains(&parent) {
                                hidden = true;
                                break;
                            }
                        }
                        if !hidden {
                            rows.push(plain(
                                format!(
                                    "{}{} {}",
                                    "  ".repeat(parts.len().saturating_sub(1)),
                                    code,
                                    path.file_name().unwrap_or_default().to_string_lossy()
                                ),
                                path.clone(),
                            ));
                        }
                    }
                }
                rows
            }
            1 => match self.subview {
                0 => self
                    .refs
                    .as_ref()
                    .map(|r| {
                        r.branches
                            .iter()
                            .map(|b| {
                                plain(
                                    format!(
                                        "{}{}",
                                        b.name,
                                        if b.checked_out.is_some() { "  wt" } else { "" }
                                    ),
                                    b.name.clone().into(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                1 => self
                    .refs
                    .as_ref()
                    .map(|r| {
                        r.worktrees
                            .iter()
                            .map(|w| {
                                plain(
                                    format!(
                                        "{}  {}{}",
                                        w.branch.as_deref().unwrap_or("detached"),
                                        w.path.file_name().unwrap_or_default().to_string_lossy(),
                                        if w.dirty { " *" } else { "" }
                                    ),
                                    w.path.clone(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                2 => self
                    .remotes
                    .iter()
                    .map(|r| plain(r.clone(), r.split_whitespace().next().unwrap_or(r).into()))
                    .collect(),
                _ => self
                    .tags
                    .iter()
                    .map(|r| plain(r.clone(), r.into()))
                    .collect(),
            },
            2 => self
                .history
                .iter()
                .map(|(sha, title)| {
                    plain(
                        format!("{} {}", &sha[..sha.len().min(7)], title),
                        sha.into(),
                    )
                })
                .collect(),
            _ => self
                .stashes
                .iter()
                .map(|r| plain(r.clone(), r.split(':').next().unwrap_or(r).into()))
                .collect(),
        }
    }
    fn selected_row(&self) -> Option<Row> {
        self.rows(self.last_list)
            .get(self.selected[self.slot(self.last_list)])
            .cloned()
    }
    fn run(&mut self, request: Request, args: Vec<OsString>) -> Result<(), String> {
        self.jobs.start(&self.root, request, args)
    }
    fn refresh(&mut self) -> Result<(), String> {
        self.jobs.cancel_reads();
        self.refreshing = true;
        self.status
            .start(GitStatusPolicy {
                root: self.root.clone(),
                root_revision: 0,
                maximum_files: 100_000,
                maximum_path_bytes: 16 * 1024 * 1024,
                deadline: Duration::from_secs(10),
            })
            .map_err(|e| e.to_string())?;
        self.branches
            .start(GitBranchRequest::List(GitBranchListRequest {
                repository: self.root.clone(),
                deadline: Duration::from_secs(10),
            }))
            .map_err(|e| e.to_string())?;
        if self.all_files {
            self.list_files()?;
        }
        self.branch = git2::Repository::open(&self.root)
            .ok()
            .and_then(|r| {
                r.find_reference("HEAD").ok().and_then(|h| {
                    h.symbolic_target()
                        .map(|s| s.trim_start_matches("refs/heads/").to_owned())
                })
            })
            .unwrap_or_else(|| "detached HEAD".into());
        self.reload_history()?;
        for (request, args) in [
            (Request::Stashes, vec!["stash", "list"]),
            (Request::Tags, vec!["tag", "--list"]),
            (Request::Remotes, vec!["remote", "-v"]),
        ] {
            self.run(request, args.into_iter().map(Into::into).collect())?;
        }
        Ok(())
    }
    /// Poll existing bounded workers; returns whether the screen needs repainting.
    pub fn poll(&mut self) -> bool {
        let selections = (0..4)
            .map(|pane| {
                self.rows(pane)
                    .get(self.selected[self.slot(pane)])
                    .map(|row| row.value.clone())
            })
            .collect::<Vec<_>>();
        let github_changed = self.github.poll();
        if self.form.is_none()
            && let Some(form) = self.github.take_form()
        {
            self.form = Some(form);
        }
        let mut changed = false;
        match self.status.poll() {
            Ok(Some(snapshot)) => {
                self.snapshot = Some(snapshot.clone());
                self.refreshing = false;
                changed = true;
            }
            Err(e) => {
                self.notice = e.to_string();
                self.refreshing = false;
                changed = true;
            }
            _ => {}
        }
        match self.branches.poll() {
            Ok(Some(GitBranchResult::List(list))) => {
                self.refs = Some(list.clone());
                changed = true;
            }
            Ok(Some(GitBranchResult::Changed)) => {
                self.branch_mutation = false;
                self.busy = false;
                let result = self.refresh();
                self.report(result);
                changed = true;
            }
            Err(e) => {
                if self.branch_mutation {
                    self.busy = false;
                    self.branch_mutation = false;
                }
                self.notice = e.to_string();
                changed = true;
            }
            _ => {}
        }
        match self.mutation.poll() {
            Ok(Some(_)) => {
                self.busy = false;
                let result = self.refresh();
                self.report(result);
                changed = true;
            }
            Err(e) => {
                self.busy = false;
                self.notice = e.to_string();
                changed = true;
            }
            _ => {}
        }
        let mut reload_preview = changed;
        for (request, result) in self.jobs.poll() {
            changed = true;
            match (request, result) {
                (Request::Preview, Ok(text)) => {
                    let path = self
                        .selected_row()
                        .filter(|_| self.last_list == 0)
                        .map(|row| row.value);
                    self.preview = Preview::git(&text, path.as_deref());
                }
                (Request::Preview, Err(error)) => self.preview = error.into(),
                (Request::Write, result) => {
                    self.busy = false;
                    match result {
                        Ok(text) => {
                            self.notice = if text.trim().is_empty() {
                                "Git operation completed".into()
                            } else {
                                text.trim().into()
                            };
                            let result = self.refresh();
                            self.report(result);
                        }
                        Err(e) => self.notice = e,
                    };
                }
                (Request::History, Ok(text)) => {
                    self.history = text
                        .lines()
                        .filter_map(|l| l.split_once('\t').map(|(a, b)| (a.into(), b.into())))
                        .collect();
                    reload_preview = true;
                }
                (Request::Stashes, Ok(text)) => {
                    self.stashes = text.lines().map(Into::into).collect();
                    reload_preview = true;
                }
                (Request::Files, Ok(text)) => {
                    self.files = text.split_terminator('\0').map(PathBuf::from).collect();
                    reload_preview = true;
                }
                (Request::Tags, Ok(text)) => {
                    self.tags = text.lines().map(Into::into).collect();
                    reload_preview = true;
                }
                (Request::Remotes, Ok(text)) => {
                    self.remotes = text.lines().map(Into::into).collect();
                    reload_preview = true;
                }
                (Request::History, Err(_))
                    if self
                        .snapshot
                        .as_ref()
                        .is_some_and(|_| git_head_id(&self.root).ok() == Some(None)) =>
                {
                    self.history.clear()
                }
                (_, Err(e)) => self.notice = e,
            }
        }
        if reload_preview {
            for (pane, value) in selections.iter().enumerate() {
                if let Some(value) = value
                    && let Some(index) = self.rows(pane).iter().position(|row| &row.value == value)
                {
                    let slot = self.slot(pane);
                    self.selected[slot] = index;
                }
            }
            self.clamp();
            self.load_preview();
        }
        let tutor_changed = if changed { self.advance_tutor() } else { false };
        changed || github_changed || tutor_changed
    }
    fn clamp(&mut self) {
        for pane in 0..4 {
            let slot = self.slot(pane);
            self.selected[slot] = self.selected[slot].min(self.rows(pane).len().saturating_sub(1));
        }
    }
    fn report(&mut self, result: Result<(), String>) {
        if let Err(e) = result {
            self.notice = e;
        }
    }
    fn load_preview(&mut self) {
        self.jobs.cancel_preview();
        let Some(row) = self.selected_row() else {
            self.preview = if self.last_list == 0 {
                "Working tree clean"
            } else {
                "No entries"
            }
            .into();
            return;
        };
        if row.directory {
            self.preview = format!(
                "{}\n\nEnter expands or collapses this directory.",
                row.value.display()
            )
            .into();
            return;
        }
        let args: Vec<OsString> = match self.last_list {
            0 => {
                let entry = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.entries.iter().find(|e| e.relative_path == row.value));
                let untracked = entry.is_some_and(|e| e.status == GitFileStatus::Untracked);
                if self.all_files
                    || (untracked
                        || entry.is_some_and(|entry| entry.status == GitFileStatus::Conflicted))
                        && !self.staged
                {
                    if !std::fs::symlink_metadata(self.root.join(&row.value))
                        .is_ok_and(|m| m.file_type().is_file())
                    {
                        self.preview = "Preview unavailable for a symlink or special file".into();
                        return;
                    }
                    match std::fs::File::open(self.root.join(&row.value)) {
                        Ok(file) => {
                            use std::io::Read;
                            let mut text = String::new();
                            let _ = file.take(128 * 1024).read_to_string(&mut text);
                            self.preview = Preview::source(&row.value, &text);
                        }
                        Err(e) => self.preview = e.to_string().into(),
                    }
                    return;
                }
                let mut a = vec![
                    "diff".into(),
                    "--no-ext-diff".into(),
                    "--no-textconv".into(),
                ];
                if self.staged {
                    a.push("--cached".into())
                }
                a.push("--".into());
                a.push(row.value.into_os_string());
                a
            }
            1 if self.subview == 1 => vec![
                "-C".into(),
                row.value.into_os_string(),
                "log".into(),
                "-40".into(),
                "--decorate".into(),
                "--format=commit %h %d%nAuthor: %an%nDate: %ad%n%n    %s%n".into(),
                "--date=short".into(),
            ],
            1 if self.subview == 2 => vec![
                "remote".into(),
                "get-url".into(),
                "--all".into(),
                row.value.into_os_string(),
            ],
            1 => vec![
                "log".into(),
                "-40".into(),
                "--decorate".into(),
                "--format=commit %h %d%nAuthor: %an%nDate: %ad%n%n    %s%n".into(),
                "--date=short".into(),
                row.value.into_os_string(),
                "--".into(),
            ],
            2 => vec![
                "show".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--stat".into(),
                "--patch".into(),
                row.value.into_os_string(),
                "--".into(),
            ],
            _ => vec![
                "stash".into(),
                "show".into(),
                "--include-untracked".into(),
                "--no-ext-diff".into(),
                "--no-textconv".into(),
                "--patch".into(),
                row.value.into_os_string(),
            ],
        };
        self.preview = "Loading…".into();
        let result = self.run(Request::Preview, args);
        self.report(result);
    }
    fn reload_history(&mut self) -> Result<(), String> {
        let (root, reference) = self
            .history_target
            .clone()
            .unwrap_or_else(|| (self.root.clone(), "HEAD".into()));
        self.jobs.start(
            &root,
            Request::History,
            vec![
                "log".into(),
                "-100".into(),
                "--format=%H%x09%s".into(),
                reference,
                "--".into(),
            ],
        )
    }
    fn select_history(&mut self) {
        if self.last_list != 1 || self.subview == 2 {
            return;
        }
        let Some(row) = self.selected_row() else {
            return;
        };
        let target = if self.subview == 1 {
            (row.value, "HEAD".into())
        } else {
            (self.root.clone(), row.value.into_os_string())
        };
        if self.history_target.as_ref() == Some(&target) {
            return;
        }
        self.history_target = Some(target);
        self.history.clear();
        self.selected[5] = 0;
        let result = self.reload_history();
        self.report(result);
    }
    fn list_files(&mut self) -> Result<(), String> {
        self.run(
            Request::Files,
            [
                "ls-files",
                "--cached",
                "--others",
                "--exclude-standard",
                "-z",
            ]
            .into_iter()
            .map(Into::into)
            .collect(),
        )
    }
    fn allow_write(&self) -> Result<(), String> {
        if self.busy {
            Err("Wait for the current Git operation to finish".into())
        } else if let Some(reason) = &self.write_refusal
            && self.tutor.is_none()
        {
            Err(reason.clone())
        } else {
            Ok(())
        }
    }
    fn write(&mut self, args: Vec<OsString>) -> Result<(), String> {
        self.allow_write()?;
        self.run(Request::Write, args)?;
        self.busy = true;
        self.notice = "Running Git…".into();
        Ok(())
    }
    fn stage(&mut self) -> Result<(), String> {
        self.allow_write()?;
        let row = self
            .selected_row()
            .filter(|r| self.last_list == 0 && !r.directory)
            .ok_or("Select a changed file")?;
        let snapshot = self.snapshot.as_ref().ok_or("Status is loading")?;
        let kind = if self.staged {
            GitMutationKind::UnstageFile
        } else {
            GitMutationKind::StageFile
        };
        self.mutation
            .start(GitMutationRequest::Plan(GitChangePlanRequest {
                kind,
                policy: GitHunkPolicy {
                    scope_root: self.root.clone(),
                    repository_id: snapshot.repository_id,
                    status_revision: snapshot.revision,
                    root_revision: 0,
                    file: row.value,
                    layer: if self.staged {
                        GitDiffLayer::Staged
                    } else {
                        GitDiffLayer::Unstaged
                    },
                    maximum_hunks: 10_000,
                    maximum_lines: 100_000,
                    maximum_bytes: 8 * 1024 * 1024,
                    deadline: Duration::from_secs(5),
                },
                hunk_snapshot_revision: 0,
                hunk_id: None,
                selected: None,
                selected_lines: Vec::new(),
                expected_index: Some(git_index_fingerprint(&self.root).map_err(|e| e.to_string())?),
                expected_worktree: None,
                expected_head: match git_head_id(&self.root).map_err(|e| e.to_string())? {
                    Some(id) => GitHeadExpectation::At(id),
                    None => GitHeadExpectation::Unborn,
                },
                expected_document_revision: None,
                unsaved_documents: Vec::new(),
                maximum_patch_bytes: 1024 * 1024,
                deadline: Duration::from_secs(5),
            }))
            .map_err(|e| e.to_string())?;
        self.busy = true;
        Ok(())
    }
    fn form(&mut self, title: &str, action: FormAction, fields: Vec<(&str, String)>) {
        self.form = Some(Form {
            title: title.into(),
            action,
            fields: fields
                .into_iter()
                .map(|(label, value)| Field::new(label, value))
                .collect(),
            selected: 0,
        });
    }
    fn conflict_form(&mut self, path: PathBuf, side: &str) {
        let mut choice = Field::new("Keep side (Up/Down selects)", side.into());
        choice.choices = vec!["ours".into(), "theirs".into()];
        self.form = Some(Form {
            title: format!("Replace {} with one side; stage separately", path.display()),
            action: FormAction::Conflict(path),
            fields: vec![choice],
            selected: 0,
        });
    }
    fn submit(&mut self) -> Result<(), String> {
        let form = self.form.as_ref().ok_or("No open form")?;
        if !matches!(&form.action, FormAction::Command)
            && !matches!(&form.action, FormAction::Github(action) if !action.writes())
        {
            self.allow_write()?;
        }
        let values = form
            .fields
            .iter()
            .map(|field| field.input.input.clone())
            .collect::<Vec<_>>();
        for field in &form.fields {
            if field.required && field.input.input.trim().is_empty() {
                return Err(format!("{} is required", field.label));
            }
            if !field.custom_choices
                && !field.choices.is_empty()
                && !field.choices.contains(&field.input.input)
            {
                return Err(format!("Choose a valid {} with Up/Down", field.label));
            }
        }
        match &form.action {
            FormAction::Commit => {
                self.write(vec!["commit".into(), "-m".into(), values[0].clone().into()])?
            }
            FormAction::Stash => self.write(vec![
                "stash".into(),
                "push".into(),
                "--include-untracked".into(),
                "-m".into(),
                values[0].clone().into(),
            ])?,
            FormAction::Worktree => {
                if let Some(tutor) = &self.tutor {
                    tutor.check_destination(Path::new(&values[1]))?;
                }
                self.branches
                    .start(GitBranchRequest::CreateWorktree {
                        repository: self.root.clone(),
                        name: Path::new(&values[1])
                            .file_name()
                            .and_then(|name| name.to_str())
                            .ok_or("Choose a worktree destination with a directory name")?
                            .to_owned(),
                        path: PathBuf::from(&values[1]),
                        reference: values[0].clone(),
                        deadline: Duration::from_secs(15),
                    })
                    .map_err(|e| e.to_string())?;
                self.busy = true;
                self.branch_mutation = true;
            }
            FormAction::Confirm(args) => self.write(args.clone())?,
            FormAction::Github(github::Action::CreatePullRequest) => {
                self.form = Some(self.github.review_pr(values)?);
                self.notice.clear();
                return Ok(());
            }
            FormAction::Github(action) => self.github.submit(action.clone(), values)?,
            FormAction::Command => {
                match values[0].trim() {
                    "tutor git" => self.start_tutor()?,
                    "tutor quit" => self.stop_tutor()?,
                    _ => return Err("Available commands: tutor git · tutor quit".into()),
                }
                return Ok(());
            }
            FormAction::Conflict(path) => {
                let choice = GitConflictChoice::parse(&values[0]).ok_or("Choose ours or theirs")?;
                let plan = plan_replacement(&self.root, std::slice::from_ref(path), choice)
                    .map_err(|error| error.to_string())?;
                apply_replacement(&plan).map_err(|error| error.to_string())?;
                self.refresh()?;
            }
            FormAction::ContinueMerge => {
                let outcome =
                    continue_operation(&self.root, "").map_err(|error| error.to_string())?;
                if let Some(warning) = outcome.cleanup_warning {
                    return Err(warning);
                }
                self.refresh()?;
            }
        }
        self.form = None;
        self.notice.clear();
        Ok(())
    }
    fn form_key(&mut self, key: &KeyEvent) -> Result<(), String> {
        if key.code == KeyCode::Escape {
            if self.form.as_ref().is_some_and(|form| {
                matches!(
                    form.action,
                    FormAction::Github(github::Action::ConfirmPullRequest(_))
                )
            }) {
                self.form = self.github.activate(github::Action::CreatePullRequest)?;
            } else {
                self.save_form_draft();
                self.form = None;
            }
            return Ok(());
        }
        if key.code == KeyCode::Enter
            && (key.modifiers.control
                || self
                    .form
                    .as_ref()
                    .is_some_and(|form| matches!(form.action, FormAction::Command)))
        {
            return self.submit();
        }
        let form = self.form.as_mut().expect("checked form");
        let count = form.fields.len() + 2;
        if key.code == KeyCode::Tab {
            form.selected =
                (form.selected + if key.modifiers.shift { count - 1 } else { 1 }) % count;
            return Ok(());
        }
        if form.selected >= form.fields.len() {
            if key.code == KeyCode::Enter {
                if form.selected == form.fields.len() {
                    return self.submit();
                }
                return self.form_key(&KeyEvent::plain(KeyCode::Escape));
            }
            return Ok(());
        }
        let field = &mut form.fields[form.selected];
        if field.read_only
            && !matches!(
                key.code,
                KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Home
                    | KeyCode::End
            )
        {
            return Ok(());
        }
        if !field.choices.is_empty() && matches!(key.code, KeyCode::Up | KeyCode::Down) {
            let index = field
                .choices
                .iter()
                .position(|value| value == &field.input.input)
                .unwrap_or(0);
            let index = if key.code == KeyCode::Up {
                (index + field.choices.len() - 1) % field.choices.len()
            } else {
                (index + 1) % field.choices.len()
            };
            field.input.input = field.choices[index].clone();
            field.input.cursor = field.input.input.len();
            return Ok(());
        }
        let input = &mut field.input;
        match key.code {
            KeyCode::Char(c) if !key.modifiers.control && !key.modifiers.alt => {
                input.insert_text(&c.to_string())
            }
            KeyCode::Backspace => input.delete_previous_grapheme(),
            KeyCode::Left => {
                input.cursor = input.input[..input.cursor]
                    .grapheme_indices(true)
                    .next_back()
                    .map_or(0, |(i, _)| i)
            }
            KeyCode::Right => {
                input.cursor += input.input[input.cursor..]
                    .graphemes(true)
                    .next()
                    .map_or(0, str::len)
            }
            KeyCode::Up | KeyCode::Down if field.multiline => {
                input.move_vertical(key.code == KeyCode::Up);
            }
            KeyCode::Home if field.multiline && !key.modifiers.control => {
                input.cursor = input.input[..input.cursor]
                    .rfind('\n')
                    .map_or(0, |at| at + 1);
            }
            KeyCode::End if field.multiline && !key.modifiers.control => {
                input.cursor = input.input[input.cursor..]
                    .find('\n')
                    .map_or(input.input.len(), |at| input.cursor + at);
            }
            KeyCode::Home => input.cursor = 0,
            KeyCode::End => input.cursor = input.input.len(),
            KeyCode::Enter if field.multiline => input.insert_text("\n"),
            KeyCode::Enter => {
                form.selected = (form.selected + 1) % count;
            }
            _ => {}
        }
        Ok(())
    }
    pub fn event(&mut self, event: UiEvent) -> ScreenOutcome {
        let previous = (self.last_list, self.subview, self.selected, self.staged);
        match event {
            UiEvent::Paste(text) => {
                if let Some(form) = &mut self.form
                    && let Some(field) = form.fields.get_mut(form.selected)
                    && !field.read_only
                {
                    let text = if field.multiline {
                        text.replace("\r\n", "\n").replace('\r', "\n")
                    } else {
                        text.replace(['\r', '\n'], " ")
                    };
                    field.input.insert_text(&text);
                }
            }
            UiEvent::Key { key, kind } if kind != KeyInputKind::Release => {
                if self.help {
                    if matches!(key.code, KeyCode::Escape | KeyCode::Char('?' | 'q')) {
                        self.help = false;
                    }
                    return ScreenOutcome::Continue;
                }
                if self.form.is_none() && key.code == KeyCode::Char('?') {
                    self.help = true;
                    return ScreenOutcome::Continue;
                }
                if self.form.is_some() {
                    let result = self.form_key(&key);
                    self.report(result);
                    return ScreenOutcome::Continue;
                }
                if !key.modifiers.control && key.code == KeyCode::Char(':') {
                    self.form(
                        "Command",
                        FormAction::Command,
                        vec![("Command", String::new())],
                    );
                    return ScreenOutcome::Continue;
                }
                if !key.modifiers.control
                    && matches!(key.code, KeyCode::Char('[' | ']'))
                    && !self.busy()
                {
                    if self.tutor.is_some() {
                        self.notice = "Use :tutor quit before opening GitHub pages".into();
                        return ScreenOutcome::Continue;
                    }
                    self.page =
                        (self.page + if key.code == KeyCode::Char(']') { 1 } else { 3 }) % 4;
                    self.github.page(self.page);
                    if self.page == 0 {
                        let result = self.refresh();
                        self.report(result);
                    }
                    return ScreenOutcome::Continue;
                }
                if self.page != 0 {
                    self.notice.clear();
                    let result = self.github.event(&key);
                    self.page = self.github.current_page();
                    match result {
                        Ok(Some(form)) => self.form = Some(form),
                        Ok(None) => {}
                        Err(error) if error == "close" && !self.busy() => {
                            return ScreenOutcome::Close;
                        }
                        Err(error) => self.notice = error,
                    }
                    return ScreenOutcome::Continue;
                }
                if key.modifiers.control {
                    if let KeyCode::Char(c) = key.code {
                        match c.to_ascii_lowercase() {
                            'h' => self.focus = self.last_list,
                            'l' => self.focus = 4,
                            'j' if self.focus < 4 => self.focus = (self.focus + 1).min(3),
                            'k' if self.focus < 4 => self.focus = self.focus.saturating_sub(1),
                            _ => {}
                        }
                    }
                } else {
                    match key.code {
                        KeyCode::Char('q') => {
                            if self.tutor.is_some() {
                                let result = self.stop_tutor();
                                self.report(result);
                                return ScreenOutcome::Continue;
                            }
                            if !self.busy {
                                return ScreenOutcome::Close;
                            }
                            self.notice =
                                "Wait for the Git operation to finish before closing".into();
                        }
                        KeyCode::Escape => {
                            if self.focus == 4 {
                                self.focus = self.last_list
                            } else if self.tutor.is_some() {
                                let result = self.stop_tutor();
                                self.report(result);
                                return ScreenOutcome::Continue;
                            } else if !self.busy {
                                return ScreenOutcome::Close;
                            }
                        }
                        KeyCode::Char(c @ '1'..='4') => self.focus = (c as u8 - b'1') as usize,
                        KeyCode::Tab => {
                            self.focus = (self.focus + if key.modifiers.shift { 4 } else { 1 }) % 5
                        }
                        KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
                        KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
                        KeyCode::PageDown => self.move_selection(15),
                        KeyCode::PageUp => self.move_selection(-15),
                        KeyCode::Char('H' | 'L') if self.focus == 1 => {
                            self.subview = (self.subview
                                + if key.code == KeyCode::Char('L') { 1 } else { 3 })
                                % 4;
                            self.offsets[1] = 0;
                        }
                        KeyCode::Char('s') if self.focus == 4 => {
                            self.split_diff = !self.split_diff;
                            self.preview_line = 0;
                            self.preview_column = 0;
                        }
                        KeyCode::Left | KeyCode::Char('h') if self.focus == 4 => {
                            self.preview_column = self.preview_column.saturating_sub(8);
                        }
                        KeyCode::Right | KeyCode::Char('l') if self.focus == 4 => {
                            use unicode_width::UnicodeWidthStr;
                            let maximum = self
                                .preview
                                .rows
                                .iter()
                                .map(|row| row.text.width())
                                .max()
                                .unwrap_or(0);
                            self.preview_column = self
                                .preview_column
                                .saturating_add(8)
                                .min(maximum.saturating_sub(1));
                        }
                        KeyCode::Char('h' | 'l') if self.focus == 0 => {
                            if let Some(row) = self.selected_row()
                                && row.directory
                            {
                                if key.code == KeyCode::Char('h') {
                                    self.collapsed.insert(row.value);
                                } else {
                                    self.collapsed.remove(&row.value);
                                }
                            }
                        }
                        KeyCode::Char('v') if self.last_list == 0 => {
                            self.all_files = !self.all_files;
                            self.staged = false;
                            if self.all_files {
                                let result = self.list_files();
                                self.report(result);
                            }
                            self.load_preview();
                        }
                        KeyCode::Char('i') if self.last_list == 0 => {
                            self.staged = !self.staged;
                            self.all_files = false;
                        }
                        KeyCode::Char(' ') if self.last_list == 0 => {
                            let result = self.stage();
                            self.report(result);
                        }
                        KeyCode::Char('o' | 't') if self.last_list == 0 => {
                            if let Some(row) = self.selected_row()
                                && !row.directory
                            {
                                self.conflict_form(
                                    row.value,
                                    if key.code == KeyCode::Char('o') {
                                        "ours"
                                    } else {
                                        "theirs"
                                    },
                                );
                            }
                        }
                        KeyCode::Char('C') => self.form(
                            "Continue the resolved merge?",
                            FormAction::ContinueMerge,
                            Vec::new(),
                        ),
                        KeyCode::Char('c') => self.form(
                            "Commit staged changes",
                            FormAction::Commit,
                            vec![("Message", String::new())],
                        ),
                        KeyCode::Char('n') if self.focus == 3 => self.form(
                            "Save stash (includes untracked files)",
                            FormAction::Stash,
                            vec![("Message", String::new())],
                        ),
                        KeyCode::Char('n') if self.focus == 1 => self.form(
                            "Create worktree (existing or new branch)",
                            FormAction::Worktree,
                            vec![
                                ("Branch", String::new()),
                                (
                                    "Path",
                                    self.root
                                        .parent()
                                        .unwrap_or(&self.root)
                                        .join("review")
                                        .display()
                                        .to_string(),
                                ),
                            ],
                        ),
                        KeyCode::Char('a' | 'p' | 'd') if self.focus == 3 => {
                            if let Some(row) = self.selected_row() {
                                let action = match key.code {
                                    KeyCode::Char('a') => "apply",
                                    KeyCode::Char('p') => "pop",
                                    _ => "drop",
                                };
                                self.form(
                                    &format!("{} {}?", action, row.value.display()),
                                    FormAction::Confirm(vec![
                                        "stash".into(),
                                        action.into(),
                                        row.value.into_os_string(),
                                    ]),
                                    Vec::new(),
                                );
                            }
                        }
                        KeyCode::Char('P') => self.form(
                            "Push current branch to its configured upstream?",
                            FormAction::Confirm(vec!["push".into()]),
                            Vec::new(),
                        ),
                        KeyCode::Char('r') if !self.busy => {
                            let result = self.refresh();
                            self.report(result);
                        }
                        KeyCode::Enter => {
                            if let Some(row) = self.selected_row() {
                                if row.directory {
                                    if !self.collapsed.remove(&row.value) {
                                        self.collapsed.insert(row.value);
                                    }
                                } else if self.last_list == 0 {
                                    if self.snapshot.as_ref().is_some_and(|snapshot| {
                                        snapshot.entries.iter().any(|entry| {
                                            entry.relative_path == row.value
                                                && entry.status == GitFileStatus::Conflicted
                                        })
                                    }) {
                                        self.conflict_form(row.value, "ours");
                                    } else if self.tutor.is_some() {
                                        self.focus = 4;
                                    } else {
                                        return ScreenOutcome::OpenFile(self.root.join(row.value));
                                    }
                                } else if self.last_list == 1 && self.subview == 1 && !self.busy {
                                    if self.tutor.is_some() {
                                        let result = self.tutor_worktree(&row.value);
                                        self.report(result);
                                        return ScreenOutcome::Continue;
                                    }
                                    return ScreenOutcome::OpenRepository(row.value);
                                } else if self.last_list == 1 && self.subview == 0 {
                                    self.form(
                                        &format!("Switch to {}?", row.value.display()),
                                        FormAction::Confirm(vec![
                                            "switch".into(),
                                            "--".into(),
                                            row.value.into_os_string(),
                                        ]),
                                        Vec::new(),
                                    );
                                } else {
                                    self.focus = 4
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if self.focus < 4 {
                    self.last_list = self.focus;
                    self.clamp();
                }
                if previous != (self.last_list, self.subview, self.selected, self.staged) {
                    self.select_history();
                    self.preview_line = 0;
                    self.preview_column = 0;
                    self.load_preview();
                }
            }
            _ => {}
        }
        self.advance_tutor();
        ScreenOutcome::Continue
    }
    fn move_selection(&mut self, delta: isize) {
        if self.focus == 4 {
            self.preview_line = self
                .preview_line
                .saturating_add_signed(delta)
                .min(self.preview_rows.saturating_sub(1));
        } else {
            let slot = self.slot(self.focus);
            self.selected[slot] = self.selected[slot]
                .saturating_add_signed(delta)
                .min(self.rows(self.focus).len().saturating_sub(1));
        }
    }
    fn save_form_draft(&mut self) {
        if let Some(form) = &self.form
            && let FormAction::Github(action) = &form.action
            && !matches!(action, github::Action::ConfirmPullRequest(_))
        {
            self.github.save_draft(
                action,
                form.fields
                    .iter()
                    .map(|field| field.input.input.clone())
                    .collect(),
            );
        }
    }
    pub fn frame(&mut self, width: u16, height: u16) -> Frame {
        let mut frame = if self.page == 0 {
            render::frame(self, width, height)
        } else {
            github::view::frame(self, width, height)
        };
        if self.help {
            render::help(&mut frame, width, height, self.page);
        }
        frame
    }

    pub fn window_frame(
        &mut self,
        width: u16,
        height: u16,
        config: &crate::config::GitWindowConfig,
    ) -> (crate::windows::WindowRect, Frame) {
        let (w, h) = if config.placement == crate::config::GitPlacement::Fullscreen {
            (width, height)
        } else {
            (
                ((u32::from(width) * u32::from(config.width_percent) / 100) as u16)
                    .max(64)
                    .min(width),
                ((u32::from(height) * u32::from(config.height_percent) / 100) as u16)
                    .max(20)
                    .min(height),
            )
        };
        (
            crate::windows::WindowRect {
                x: (width - w) / 2,
                y: (height - h) / 2,
                width: w,
                height: h,
            },
            self.frame(w, h),
        )
    }

    /// Place the shared surface over the host's frame without hiding the editor around it.
    pub fn compose(background: &mut Frame, rect: crate::windows::WindowRect, foreground: &Frame) {
        for y in 0..rect.height {
            for x in 0..rect.width {
                if let Some(cell) = foreground.grid.get(x, y)
                    && !cell.continuation
                {
                    background
                        .grid
                        .set_cell(rect.x + x, rect.y + y, &cell.symbol, cell.style);
                }
            }
        }
        background.cursor = foreground
            .cursor
            .map(|(x, y, style)| (rect.x + x, rect.y + y, style));
    }
}
