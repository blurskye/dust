//! The shared native Git application. Hosts own terminal lifecycle and opening files/workspaces.
mod jobs;
mod render;

use super::*;
use crate::editor::input::{KeyCode, KeyEvent};
use crate::ui::{Frame, KeyInputKind, PromptKind, PromptState, UiEvent};
use jobs::{Jobs, Request};
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
}
struct Form {
    title: String,
    action: FormAction,
    fields: Vec<(String, PromptState)>,
    selected: usize,
}

/// One controller and renderer, used in the editor and the standalone executable.
pub struct GitScreen {
    root: PathBuf,
    status: GitStatusWorker,
    snapshot: Option<GitStatusSnapshot>,
    branches: GitBranchWorker,
    refs: Option<GitBranchList>,
    mutation: GitMutationWorker,
    jobs: Jobs,
    history: Vec<(String, String)>,
    stashes: Vec<String>,
    tags: Vec<String>,
    remotes: Vec<String>,
    focus: usize,
    last_list: usize,
    subview: usize,
    selected: [usize; 7],
    offsets: [usize; 4],
    collapsed: BTreeSet<PathBuf>,
    preview: String,
    preview_line: usize,
    staged: bool,
    branch: String,
    notice: String,
    form: Option<Form>,
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
            root,
            status: GitStatusWorker::default(),
            snapshot: None,
            branches: GitBranchWorker::default(),
            refs: None,
            mutation: GitMutationWorker::default(),
            jobs: Jobs::default(),
            history: Vec::new(),
            stashes: Vec::new(),
            tags: Vec::new(),
            remotes: Vec::new(),
            focus: 0,
            last_list: 0,
            subview: 0,
            selected: [0; 7],
            offsets: [0; 4],
            collapsed: BTreeSet::new(),
            preview: String::new(),
            preview_line: 0,
            staged: false,
            branch: String::new(),
            notice: String::new(),
            form: None,
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
        self.busy
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
                    for entry in &snapshot.entries {
                        let parts = entry.relative_path.components().collect::<Vec<_>>();
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
                                    entry.change_code(),
                                    entry
                                        .relative_path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                ),
                                entry.relative_path.clone(),
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
        self.branch = git2::Repository::open(&self.root)
            .ok()
            .and_then(|r| {
                r.find_reference("HEAD").ok().and_then(|h| {
                    h.symbolic_target()
                        .map(|s| s.trim_start_matches("refs/heads/").to_owned())
                })
            })
            .unwrap_or_else(|| "detached HEAD".into());
        for (request, args) in [
            (
                Request::History,
                vec!["log", "-100", "--format=%H%x09%s", "HEAD", "--"],
            ),
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
                (Request::Preview, Ok(text)) => self.preview = text,
                (Request::Preview, Err(error)) => self.preview = error,
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
        changed
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
            );
            return;
        }
        let args: Vec<OsString> = match self.last_list {
            0 => {
                let entry = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.entries.iter().find(|e| e.relative_path == row.value));
                let untracked = entry.is_some_and(|e| e.status == GitFileStatus::Untracked);
                if untracked && !self.staged {
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
                            self.preview = format!("{} (untracked)\n{}", row.value.display(), text);
                        }
                        Err(e) => self.preview = e.to_string(),
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
    fn allow_write(&self) -> Result<(), String> {
        if self.busy {
            Err("Wait for the current Git operation to finish".into())
        } else if let Some(reason) = &self.write_refusal {
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
                .map(|(label, value)| {
                    let mut input = PromptState::new(PromptKind::Text);
                    input.cursor = value.len();
                    input.input = value;
                    (label.into(), input)
                })
                .collect(),
            selected: 0,
        });
    }
    fn submit(&mut self) -> Result<(), String> {
        self.allow_write()?;
        let form = self.form.as_ref().ok_or("No open form")?;
        let values = form
            .fields
            .iter()
            .map(|(_, p)| p.input.clone())
            .collect::<Vec<_>>();
        if values.iter().any(|v| v.trim().is_empty()) {
            return Err("Fill in every field".into());
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
                self.branches
                    .start(GitBranchRequest::CreateWorktree {
                        repository: self.root.clone(),
                        name: values[0].clone(),
                        path: PathBuf::from(&values[1]),
                        reference: values[0].clone(),
                        deadline: Duration::from_secs(15),
                    })
                    .map_err(|e| e.to_string())?;
                self.busy = true;
                self.branch_mutation = true;
            }
            FormAction::Confirm(args) => self.write(args.clone())?,
        }
        self.form = None;
        Ok(())
    }
    fn form_key(&mut self, key: &KeyEvent) -> Result<(), String> {
        if key.code == KeyCode::Escape {
            self.form = None;
            return Ok(());
        }
        if key.code == KeyCode::Enter && key.modifiers.control {
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
                self.form = None;
            }
            return Ok(());
        }
        let input = &mut form.fields[form.selected].1;
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
            KeyCode::Home => input.cursor = 0,
            KeyCode::End => input.cursor = input.input.len(),
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
                    && let Some((_, p)) = form.fields.get_mut(form.selected)
                {
                    p.insert_text(&text.replace(['\r', '\n'], " "));
                }
            }
            UiEvent::Key { key, kind } if kind != KeyInputKind::Release => {
                if self.form.is_some() {
                    let result = self.form_key(&key);
                    self.report(result);
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
                            if !self.busy {
                                return ScreenOutcome::Close;
                            }
                            self.notice =
                                "Wait for the Git operation to finish before closing".into();
                        }
                        KeyCode::Escape => {
                            if self.focus == 4 {
                                self.focus = self.last_list
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
                        KeyCode::Char('v') if self.last_list == 0 => self.staged = !self.staged,
                        KeyCode::Char(' ') if self.last_list == 0 => {
                            let result = self.stage();
                            self.report(result);
                        }
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
                            "Create worktree from an existing branch",
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
                                    return ScreenOutcome::OpenFile(self.root.join(row.value));
                                } else if self.last_list == 1 && self.subview == 1 && !self.busy {
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
                        KeyCode::Char('?') => {
                            self.jobs.cancel_preview();
                            self.focus = 4;
                            self.preview="1 Changes · 2 Branches · 3 Commits · 4 Stashes\nCtrl+h/j/k/l: spatial pane focus\nTab / Shift+Tab: next / previous pane\nj/k or arrows: move · Enter: open\nh/l: collapse/expand directory · H/L: branch subview\nSpace: stage · v: switch working tree/index\nc: commit · P: push · r: refresh\nn: new worktree/stash · a/p/d: apply/pop/drop stash\nForms: type, arrows edit, Tab visits fields and buttons\nCtrl+Enter: submit · Esc: cancel/back · q: close".into();
                            return ScreenOutcome::Continue;
                        }
                        _ => {}
                    }
                }
                if self.focus < 4 {
                    self.last_list = self.focus;
                    self.clamp();
                    if previous != (self.last_list, self.subview, self.selected, self.staged) {
                        self.preview_line = 0;
                        self.load_preview();
                    }
                }
            }
            _ => {}
        }
        ScreenOutcome::Continue
    }
    fn move_selection(&mut self, delta: isize) {
        if self.focus == 4 {
            self.preview_line = self
                .preview_line
                .saturating_add_signed(delta)
                .min(self.preview.lines().count().saturating_sub(1));
        } else {
            let slot = self.slot(self.focus);
            self.selected[slot] = self.selected[slot]
                .saturating_add_signed(delta)
                .min(self.rows(self.focus).len().saturating_sub(1));
        }
    }
    pub fn frame(&mut self, width: u16, height: u16) -> Frame {
        render::frame(self, width, height)
    }
}
