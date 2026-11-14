//! The shared native Git application. Hosts own terminal lifecycle and opening files/workspaces.
mod commit;
mod filter;
mod github;
mod jobs;
mod preview;
mod reflog;
mod refs;
mod render;
mod rewrite;
mod search;
mod selection;
mod shell;
mod sidebar;
mod staging;
mod tree;
mod tutor;

use super::*;
use crate::editor::input::{KeyCode, KeyEvent};
use crate::ui::{Frame, KeyInputKind, PromptKind, PromptState, UiEvent};
use jobs::{Jobs, Request};
use preview::Preview;
use shell::chrome::ReceiptTone;

const DIFF_REFRESHING: &str = "The diff is refreshing; wait before changing code";
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
    /// Indentation in terminal cells, after compacting directory chains.
    indent: usize,
}
impl Row {
    fn plain(label: String, value: PathBuf) -> Self {
        Self {
            label,
            value,
            directory: false,
            indent: 0,
        }
    }
}

#[derive(Clone)]
enum FormAction {
    Commit,
    Rewrite(Box<super::rewrite::Plan>),
    Branch(Option<String>),
    Stash,
    Worktree,
    CheckoutDetached(String),
    DetachedWorktree(String),
    RemoveWorktree(String),
    Reference(refs::Action),
    Confirm(Vec<OsString>),
    Discard(Box<GitResetHunkRequest>),
    /// Changes-pane `d`/`D`: whole files' worktree changes, captured when the sheet opened.
    DiscardFiles(Box<staging::FileDiscard>),
    Github(github::Action),
    Command,
    SourceSearch,
    Conflict(PathBuf),
    Operation {
        action: GitOperationAction,
        operation: String,
        head: refs::Target,
        incoming: Vec<git2::Oid>,
        rebase_identity: Option<RebaseIdentity>,
    },
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct RebaseIdentity {
    backend: &'static str,
    original: Vec<u8>,
    onto: Vec<u8>,
    branch: Vec<u8>,
}

/// Snapshot operation identity while refreshing, never during rendering. Unknown or
/// malformed metadata still identifies the rebase, but cannot supply a branch name.
fn rebase_head_label(repository: &git2::Repository) -> Option<String> {
    if !matches!(
        repository.state(),
        git2::RepositoryState::Rebase
            | git2::RepositoryState::RebaseInteractive
            | git2::RepositoryState::RebaseMerge
    ) {
        return None;
    }
    let recorded = || -> Option<String> {
        use std::io::Read;
        let directories = ["rebase-merge", "rebase-apply"].map(|name| repository.path().join(name));
        let mut directories = directories.iter().filter(|path| path.is_dir());
        let directory = directories.next()?;
        if directories.next().is_some() {
            return None;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(directory.join("head-name"))
            .ok()?
            .take(4097)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.is_empty() || bytes.len() > 4096 {
            return None;
        }
        let text = std::str::from_utf8(&bytes).ok()?;
        let name = text.strip_suffix('\n').unwrap_or(text);
        if name == "detached HEAD" {
            return Some(name.into());
        }
        let branch = name.strip_prefix("refs/heads/")?;
        (git2::Reference::is_valid_name(name) && !branch.is_empty()).then(|| branch.to_owned())
    };
    Some(format!(
        "rebase · {}",
        recorded().unwrap_or_else(|| "HEAD".into())
    ))
}

/// `git commit` prints `[branch abc1234] subject` then a stat line; turn it into
/// the reader's receipt band text.
fn commit_receipt(text: &str) -> Option<(ReceiptTone, String)> {
    let rest = text.trim().strip_prefix('[')?;
    let (head, tail) = rest.split_once("] ")?;
    let hash = head.rsplit(' ').next()?;
    let mut lines = tail.lines();
    let subject = lines.next()?.trim();
    let stats = lines
        .next()
        .map(|line| {
            line.trim()
                .replace(" insertions(+)", "")
                .replace(" insertion(+)", "")
                .replace(" deletions(-)", "")
                .replace(" deletion(-)", "")
        })
        .unwrap_or_default();
    Some((
        ReceiptTone::Ok,
        format!("Committed {hash} · {subject}{}{stats}", if stats.is_empty() { "" } else { " · " }),
    ))
}

/// A prepared ref relationship for the existing preview header, independent of
/// body columns and native cursor coordinates. Names come from exact refs, not %d.
struct RefPreviewContext {
    selected: String,
    current: Option<String>,
    head_at_tip: bool,
    selected_is_current: bool,
    tip: git2::Oid,
}

impl RefPreviewContext {
    fn capture(repository: &git2::Repository, reference: &str, tip: git2::Oid) -> Option<Self> {
        // A ref can move while log is running. Do not decorate the old log with
        // an unverified relationship to the newly selected tip.
        if repository
            .find_reference(reference)
            .ok()?
            .peel_to_commit()
            .ok()?
            .id()
            != tip
        {
            return None;
        }
        let head = repository.head().ok()?;
        let head_id = head.peel_to_commit().ok()?.id();
        let raw_head = repository.find_reference("HEAD").ok()?;
        let symbolic = raw_head.symbolic_target();
        let current =
            symbolic.map(|name| name.strip_prefix("refs/heads/").unwrap_or(name).to_owned());
        let selected = ["refs/heads/", "refs/remotes/", "refs/tags/"]
            .into_iter()
            .find_map(|prefix| reference.strip_prefix(prefix))?
            .to_owned();
        Some(Self {
            selected,
            current,
            head_at_tip: head_id == tip,
            selected_is_current: symbolic == Some(reference),
            tip,
        })
    }
}

/// Display-only identity of the actual paused replay, never an operation target.
struct PausedReplay {
    id: git2::Oid,
    subject: String,
}

impl PausedReplay {
    fn capture(repository: &git2::Repository) -> Option<Self> {
        if !matches!(
            repository.state(),
            git2::RepositoryState::Rebase
                | git2::RepositoryState::RebaseInteractive
                | git2::RepositoryState::RebaseMerge
        ) {
            return None;
        }
        let directories = ["rebase-merge", "rebase-apply"].map(|name| repository.path().join(name));
        let mut active = directories
            .iter()
            .enumerate()
            .filter(|(_, path)| path.is_dir());
        let (backend, directory) = active.next()?;
        if active.next().is_some() {
            return None;
        }
        let oid = |name: &str| -> Option<git2::Oid> {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(directory.join(name))
                .ok()?
                .take(42)
                .read_to_end(&mut bytes)
                .ok()?;
            let text = std::str::from_utf8(&bytes).ok()?;
            let text = text.strip_suffix('\n').unwrap_or(text);
            if text.len() != 40 {
                return None;
            }
            git2::Oid::from_str(text).ok()
        };
        let id = oid(if backend == 0 {
            "stopped-sha"
        } else {
            "original-commit"
        })?;
        match repository.find_reference("REBASE_HEAD") {
            Ok(reference) if reference.target() == Some(id) => {}
            Err(error)
                if backend == 1
                    && error.code() == git2::ErrorCode::NotFound
                    && std::fs::symlink_metadata(repository.path().join("REBASE_HEAD"))
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
            _ => return None,
        }
        let original = oid("orig-head")?;
        let onto = oid("onto")?;
        let head = repository.head().ok()?.peel_to_commit().ok()?.id();
        // Metadata left over from another replay or an external checkout is not
        // permission to substitute a plausible commit from ordinary history.
        if (original != id && !repository.graph_descendant_of(original, id).ok()?)
            || (head != onto && !repository.graph_descendant_of(head, onto).ok()?)
        {
            return None;
        }
        repository.find_commit(original).ok()?;
        repository.find_commit(onto).ok()?;
        let commit = repository.find_commit(id).ok()?;
        Some(Self {
            id,
            subject: commit.summary().unwrap_or_default().to_owned(),
        })
    }
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
    project: String,
    pub(super) icon_mode: crate::config::ExplorerIconMode,
    page: usize,
    github: github::Workspace,
    status: GitStatusWorker,
    snapshot: Option<GitStatusSnapshot>,
    operation_bar: Option<GitOperationBar>,
    branches: GitBranchWorker,
    refs: Option<GitBranchList>,
    mutation: GitMutationWorker,
    rewrite: super::rewrite::Worker,
    /// The last unacknowledged rewrite failure, independent of refresh notices.
    rewrite_diagnostic: Option<String>,
    hunks: GitHunkWorker,
    hunk_policy: Option<GitHunkPolicy>,
    hunk_snapshot: Option<GitHunkSnapshot>,
    hunk_head: Option<GitHeadExpectation>,
    hunk_error: Option<String>,
    jobs: Jobs,
    history: Vec<(String, String)>,
    /// Parents, author, date and decorations for each `history` row, same order.
    history_info: Vec<shell::HistoryInfo>,
    history_graph: Vec<shell::graph::GraphRow>,
    history_loading: bool,
    /// `+added −removed` per changed path; `None` for binary files.
    numstat: std::collections::HashMap<PathBuf, Option<(u32, u32)>>,
    /// Index against HEAD, for the commit sheet.
    staged_numstat: std::collections::HashMap<PathBuf, Option<(u32, u32)>>,
    staged_patch: String,
    /// The commit sheet's toggles: sign-off, amend, skip hooks.
    pub(super) commit_options: [bool; 3],
    /// Flat list unless toggled with `t` (or more than 40 paths changed).
    changes_tree: bool,
    /// The last action's receipt, shown at the top of the reader until the next action.
    receipt: Option<(ReceiptTone, String)>,
    reflog: Vec<sidebar::ReflogEntry>,
    reflog_loading: bool,
    reflog_error: Option<String>,
    /// Keep identity-bound recovery/reference forms until the worker accepts the effect.
    reflog_submitting: bool,
    history_target: Option<(PathBuf, OsString)>,
    stashes: Vec<String>,
    tags: Vec<String>,
    remotes: Vec<String>,
    focus: usize,
    last_list: usize,
    subview: usize,
    files_view: usize,
    history_view: usize,
    selected: [usize; 9],
    filters: [filter::ListFilter; 9],
    offsets: [usize; 4],
    collapsed: BTreeSet<PathBuf>,
    changes_collapsed: bool,
    file_selection: selection::FileSelection,
    preview: Preview,
    preview_args: Option<Vec<OsString>>,
    ref_preview_target: Option<String>,
    ref_preview: Option<RefPreviewContext>,
    commit: Option<commit::Commit>,
    commit_open: bool,
    preview_offset: usize,
    preview_cursor: Option<usize>,
    preview_anchor: Option<usize>,
    preview_split: bool,
    preview_column: usize,
    split_diff: bool,
    preview_rows: usize,
    preview_visible: usize,
    message_scroll: Option<usize>,
    message_visible: usize,
    staged: bool,
    all_files: bool,
    files: BTreeSet<PathBuf>,
    tutor: Option<tutor::Session>,
    branch: String,
    /// Display-only context; never use the recorded rebase branch as the actual HEAD.
    head_label: String,
    paused_replay: Option<PausedReplay>,
    notice: String,
    form: Option<Form>,
    help: bool,
    busy: bool,
    branch_mutation: bool,
    refreshing: bool,
    refs_loading: bool,
    refreshing_after_write: bool,
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
        let project = git2::Repository::open(repo.commondir())
            .ok()
            .and_then(|main| {
                main.workdir()
                    .and_then(Path::file_name)
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| {
                root.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            });
        let mut screen = Self {
            project,
            icon_mode: crate::config::ExplorerIconMode::Unicode,
            github: github::Workspace::new(root.clone()),
            root,
            page: 0,
            status: GitStatusWorker::default(),
            snapshot: None,
            operation_bar: None,
            branches: GitBranchWorker::default(),
            refs: None,
            mutation: GitMutationWorker::default(),
            rewrite: super::rewrite::Worker::default(),
            rewrite_diagnostic: None,
            hunks: GitHunkWorker::default(),
            hunk_policy: None,
            hunk_snapshot: None,
            hunk_head: None,
            hunk_error: None,
            jobs: Jobs::default(),
            history: Vec::new(),
            history_info: Vec::new(),
            history_graph: Vec::new(),
            history_loading: false,
            numstat: std::collections::HashMap::new(),
            staged_numstat: std::collections::HashMap::new(),
            staged_patch: String::new(),
            commit_options: [false; 3],
            changes_tree: false,
            receipt: None,
            reflog: Vec::new(),
            reflog_loading: false,
            reflog_error: None,
            reflog_submitting: false,
            history_target: None,
            stashes: Vec::new(),
            tags: Vec::new(),
            remotes: Vec::new(),
            focus: 0,
            last_list: 0,
            subview: 0,
            files_view: 0,
            history_view: 0,
            selected: [0; 9],
            filters: std::array::from_fn(|_| filter::ListFilter::default()),
            offsets: [0; 4],
            collapsed: BTreeSet::new(),
            changes_collapsed: false,
            file_selection: selection::FileSelection::default(),
            preview: Preview::default(),
            preview_args: None,
            ref_preview_target: None,
            ref_preview: None,
            commit: None,
            commit_open: false,
            preview_offset: 0,
            preview_cursor: None,
            preview_anchor: None,
            preview_split: false,
            preview_column: 0,
            split_diff: false,
            preview_rows: 0,
            preview_visible: 0,
            message_scroll: None,
            message_visible: 0,
            staged: false,
            all_files: false,
            files: BTreeSet::new(),
            tutor: None,
            branch: String::new(),
            head_label: String::new(),
            paused_replay: None,
            notice: String::new(),
            form: None,
            help: false,
            busy: false,
            branch_mutation: false,
            refreshing: false,
            refs_loading: false,
            refreshing_after_write: false,
            write_refusal: None,
        };
        screen.refresh()?;
        Ok(screen)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn set_icon_mode(&mut self, mode: crate::config::ExplorerIconMode) {
        self.icon_mode = mode;
    }
    /// A deep link: start on a top tab (`git`, `prs`/`mrs`, `issues`, `actions`) and, on
    /// Pull requests or Issues, open item `item` in the reader once the repository resolves.
    pub fn open_target(&mut self, page: &str, item: Option<u64>) {
        let page = match page {
            "prs" | "mrs" => 2,
            "issues" => 1,
            "actions" => 3,
            "notifications" | "todos" => github::notifications::PAGE,
            _ => 0,
        };
        if page == 0 || self.tutor.is_some() {
            return;
        }
        self.clear_file_selection();
        self.page = page;
        self.github.page(page);
        if let Some(item) = item {
            self.github.open_link(page, item);
        }
    }
    pub fn busy(&self) -> bool {
        self.busy || self.refreshing_after_write || self.github.writing()
    }
    /// What this checkout stands on, exactly as the screen must say it: the
    /// paused replay's recorded branch, a detach, or the plain branch name.
    pub(super) fn head_label(&self) -> &str {
        &self.head_label
    }
    /// The checked-out branch's own ahead/behind, taken from the last reference
    /// listing. Zero while that listing is absent or still loading.
    pub(super) fn divergence(&self) -> (u32, u32) {
        self.refs
            .as_ref()
            .and_then(|refs| refs.branches.iter().find(|branch| branch.head))
            .map_or((0, 0), |branch| (branch.ahead, branch.behind))
    }
    fn slot(&self, pane: usize) -> usize {
        match pane {
            0 => [0, 2, 7][self.files_view],
            1 => 1 + self.subview,
            2 if self.history_view == 1 => 8,
            2 => 5,
            _ => 6,
        }
    }
    fn rows(&self, pane: usize) -> Vec<Row> {
        let plain = Row::plain;
        let filter = &self.filters[self.slot(pane)];
        let rows = match pane {
            0 if self.files_view == 1 => self.worktree_rows(),
            0 if self.files_view == 2 => self.submodule_rows(),
            0 => {
                if self.changes_collapsed && !filter.active() {
                    return Vec::new();
                }
                if let Some(snapshot) = &self.snapshot {
                    let mut paths = snapshot
                        .entries
                        .iter()
                        .map(|entry| {
                            let marker = match (entry.sources.staged, entry.sources.unstaged) {
                                (true, false) => '✓',
                                (true, true) => '◐',
                                _ => ' ',
                            };
                            (
                                entry.relative_path.clone(),
                                format!("{marker} {:<2}", entry.change_code().trim()),
                            )
                        })
                        .collect::<std::collections::BTreeMap<_, _>>();
                    if self.all_files {
                        for path in &self.files {
                            paths.entry(path.clone()).or_insert("    ".into());
                        }
                    }
                    if self.changes_flat() {
                        paths
                            .into_keys()
                            .filter(|path| !filter.active() || filter.matches(&path.to_string_lossy()))
                            .map(|path| Row::plain(path.display().to_string(), path))
                            .collect()
                    } else {
                        filter.tree(paths, &self.collapsed)
                    }
                } else {
                    Vec::new()
                }
            }
            1 => match self.subview {
                0 => self
                    .refs
                    .as_ref()
                    .map(|r| {
                        r.branches
                            .iter()
                            .filter(|branch| !branch.remote)
                            .map(|b| {
                                plain(
                                    format!(
                                        "{}{}",
                                        if b.name == self.branch { "● " } else { "  " },
                                        b.name
                                    ),
                                    b.name.clone().into(),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                2 => self.remote_rows(),
                _ => self
                    .tags
                    .iter()
                    .map(|r| plain(r.clone(), r.into()))
                    .collect(),
            },
            2 if self.history_view == 1 => self
                .reflog
                .iter()
                .map(|entry| plain(entry.label.clone(), entry.key.clone()))
                .collect(),
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
        };
        if pane == 0 && self.files_view == 0 {
            rows
        } else {
            filter.rows(rows)
        }
    }
    fn selected_row(&self) -> Option<Row> {
        self.rows(self.last_list)
            .get(self.selected[self.slot(self.last_list)])
            .cloned()
    }
    fn preview_staged(&self) -> bool {
        self.selected_row()
            .and_then(|row| {
                self.snapshot
                    .as_ref()?
                    .entries
                    .iter()
                    .find(|entry| entry.relative_path == row.value)
            })
            .is_some_and(|entry| entry.sources.staged && (self.staged || !entry.sources.unstaged))
    }
    fn run(&mut self, request: Request, args: Vec<OsString>) -> Result<(), String> {
        self.jobs.start(&self.root, request, args)
    }
    fn refresh(&mut self) -> Result<(), String> {
        if self
            .history_target
            .as_ref()
            .is_some_and(|(root, _)| root != &self.root && !root.is_dir())
        {
            self.history_target = None;
        }
        self.jobs.cancel_reads();
        self.ref_preview = None;
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
        let refs = self
            .branches
            .start(GitBranchRequest::List(GitBranchListRequest {
                repository: self.root.clone(),
                deadline: Duration::from_secs(10),
            }))
            .map_err(|e| e.to_string());
        self.refs_loading = refs.is_ok();
        refs?;
        if self.all_files {
            self.list_files()?;
        }
        self.refresh_head_context();
        self.reload_history()?;
        if self.history_view == 1 {
            self.reload_reflog()?;
        }
        for (request, args) in [
            (
                Request::Numstat,
                vec!["diff", "--numstat", "--no-renames", "HEAD", "--"],
            ),
            (
                Request::StagedNumstat,
                vec!["diff", "--cached", "--numstat", "--no-renames", "--"],
            ),
            (
                Request::StagedPatch,
                vec![
                    "diff",
                    "--cached",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-renames",
                    "-U1",
                    "--",
                ],
            ),
            (Request::Stashes, vec!["stash", "list"]),
            (Request::Tags, vec!["tag", "--list"]),
            (Request::Remotes, vec!["remote", "-v"]),
        ] {
            self.run(request, args.into_iter().map(Into::into).collect())?;
        }
        Ok(())
    }
    fn refresh_head_context(&mut self) {
        let repository = git2::Repository::open(&self.root).ok();
        self.branch = repository
            .as_ref()
            .and_then(|repository| {
                repository.find_reference("HEAD").ok().and_then(|head| {
                    head.symbolic_target()
                        .map(|name| name.trim_start_matches("refs/heads/").to_owned())
                })
            })
            .unwrap_or_else(|| "detached HEAD".into());
        self.head_label = repository
            .as_ref()
            .and_then(rebase_head_label)
            .unwrap_or_else(|| self.branch.clone());
        self.paused_replay = repository.as_ref().and_then(PausedReplay::capture);
        if let Some(commit) = &mut self.commit {
            let selected_ref = self
                .history_target
                .as_ref()
                .and_then(|(_, reference)| reference.to_str());
            commit.decorations = repository
                .as_ref()
                .map(|repository| commit::tip_decorations(repository, &commit.id, selected_ref))
                .unwrap_or_default();
        }
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
        let mut reload_preview = false;
        let history_was_loading = self.history_loading;
        match self.status.poll() {
            Ok(Some(snapshot)) => {
                self.snapshot = Some(snapshot.clone());
                self.refresh_head_context();
                self.operation_bar = match read_conflict_state(&self.root) {
                    Ok(state) => GitOperationBar::of(&state),
                    Err(error) => {
                        self.notice = format!("Cannot read Git operation: {error}");
                        None
                    }
                };
                self.reconcile_file_selection();
                self.count_untracked();
                self.refreshing = false;
                if !self.refs_loading {
                    self.refreshing_after_write = false;
                }
                reload_preview = self.last_list == 0;
                changed = true;
            }
            Err(e) => {
                self.notice = e.to_string();
                self.refreshing = false;
                if !self.refs_loading {
                    self.refreshing_after_write = false;
                }
                changed = true;
            }
            _ => {}
        }
        match self.branches.poll() {
            Ok(Some(GitBranchResult::List(list))) => {
                self.refs_loading = false;
                if !self.refreshing {
                    self.refreshing_after_write = false;
                }
                self.refs = Some(list.clone());
                reload_preview |=
                    self.last_list == 1 || self.last_list == 0 && self.files_view != 0;
                changed = true;
            }
            Ok(Some(GitBranchResult::Changed)) => {
                self.finish_reflog_form(true);
                self.branch_mutation = false;
                self.busy = false;
                self.notice.clear();
                let result = self.refresh();
                self.refreshing_after_write = result.is_ok();
                self.report(result);
                changed = true;
            }
            Err(e) => {
                self.refs_loading = false;
                if !self.refreshing {
                    self.refreshing_after_write = false;
                }
                if self.branch_mutation {
                    self.finish_reflog_form(false);
                    self.busy = false;
                    self.branch_mutation = false;
                }
                self.notice = e.to_string();
                changed = true;
            }
            _ => {}
        }
        match self.hunks.poll() {
            Ok(Some(snapshot)) => {
                self.hunk_snapshot = Some(snapshot.clone());
                changed = true;
            }
            Err(error) => {
                self.hunk_error = Some(error.to_string());
                changed = true;
            }
            _ => {}
        }
        match self.mutation.poll() {
            Ok(Some(mutation)) => {
                let discarded = mutation.kind == GitMutationKind::ResetUnstagedHunk;
                self.busy = false;
                self.notice = if discarded {
                    // A reset changes disk, not the index. Do not retain obsolete
                    // source while status and the new diff are being read.
                    self.clear_hunks();
                    self.preview_args = None;
                    self.preview_cursor = None;
                    self.preview_anchor = None;
                    self.preview = "Loading…".into();
                    "Working tree updated · index unchanged".into()
                } else {
                    String::new()
                };
                let result = self.refresh();
                self.refreshing_after_write = result.is_ok();
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
        changed |= self.poll_rewrite();
        for (request, result) in self.jobs.poll() {
            changed = true;
            match (request, result) {
                (Request::Preview, Ok(text)) => {
                    let path = self
                        .selected_row()
                        .filter(|_| self.last_list == 0)
                        .map(|row| row.value);
                    let preview = if let Some(reference) = &self.ref_preview_target {
                        let entries = commit::RefLogEntry::parse(&text);
                        self.ref_preview = entries.first().and_then(|entry| {
                            let repository = git2::Repository::open(&self.root).ok()?;
                            RefPreviewContext::capture(&repository, reference, entry.id)
                        });
                        commit::RefLogEntry::preview(&entries)
                    } else {
                        Preview::git(&text, path.as_deref())
                    };
                    self.update_preview(preview);
                    self.preview_anchor = None;
                    if self.notice == DIFF_REFRESHING {
                        self.notice.clear();
                    }
                }
                (Request::Preview, Err(error)) => {
                    if !self.preview.patch {
                        self.preview = error.clone().into();
                    }
                    self.notice = error;
                }
                (Request::CommitInfo, Ok(text)) => {
                    let selected_ref = self
                        .history_target
                        .as_ref()
                        .and_then(|(_, reference)| reference.to_str());
                    if let Some(commit) = commit::Commit::parse(&text, selected_ref)
                        && self.last_list == 2
                        && self
                            .commit
                            .as_ref()
                            .is_some_and(|current| current.id == commit.id)
                    {
                        self.commit = Some(commit);
                        self.load_commit_file();
                    }
                }
                (Request::CommitInfo, Err(error)) => {
                    if self.last_list == 2 {
                        self.preview = error.into();
                    }
                }
                (Request::Write, result) => {
                    self.receipt = result.as_ref().ok().and_then(|text| commit_receipt(text));
                    self.finish_reflog_form(result.is_ok());
                    self.busy = false;
                    self.notice = match result {
                        // Silent success: the refreshed panes are the receipt, and a
                        // lingering "completed" line would hide the footer's keys.
                        Ok(text) if text.trim().is_empty() => String::new(),
                        Ok(text) => text.trim().into(),
                        Err(error) => error.replace(['\r', '\n'], " "),
                    };
                    // A stopped merge can still have updated the index and working files.
                    let result = self.refresh();
                    self.refreshing_after_write = result.is_ok();
                    self.report(result);
                }
                (Request::History, Ok(text)) => {
                    self.history_loading = false;
                    let (history, info) = shell::parse_history(&text);
                    self.history_graph = shell::graph::rows(
                        &info
                            .iter()
                            .zip(&history)
                            .map(|(info, (id, _))| (id.as_str(), info.parents.iter().map(String::as_str).collect()))
                            .collect::<Vec<_>>(),
                    );
                    self.history = history;
                    self.history_info = info;
                    reload_preview |= self.last_list == 2 && self.history_view == 0;
                }
                (Request::Reflog, Ok(text)) => {
                    self.reflog_loading = false;
                    self.reflog_error = None;
                    self.reflog = sidebar::ReflogEntry::parse(&text);
                    reload_preview |= self.last_list == 2 && self.history_view == 1;
                }
                (Request::Reflog, Err(error)) => {
                    self.reflog_loading = false;
                    if git_head_id(&self.root).ok() == Some(None) {
                        self.reflog.clear();
                    } else {
                        self.reflog_error = Some(error.clone());
                        self.notice = error;
                    }
                    reload_preview |= self.last_list == 2 && self.history_view == 1;
                }
                (Request::Numstat, Ok(text)) => {
                    self.numstat = shell::parse_numstat(&text);
                    self.count_untracked();
                }
                (Request::Numstat, Err(_)) => self.numstat.clear(),
                (Request::StagedNumstat, Ok(text)) => {
                    self.staged_numstat = shell::parse_numstat(&text);
                }
                (Request::StagedNumstat, Err(_)) => self.staged_numstat.clear(),
                (Request::StagedPatch, Ok(text)) => self.staged_patch = text,
                (Request::StagedPatch, Err(_)) => self.staged_patch.clear(),
                (Request::Stashes, Ok(text)) => {
                    self.stashes = text.lines().map(Into::into).collect();
                    reload_preview |= self.last_list == 3;
                }
                (Request::Files, Ok(text)) => {
                    self.files = text.split_terminator('\0').map(PathBuf::from).collect();
                    reload_preview |= self.last_list == 0 && self.all_files;
                }
                (Request::Tags, Ok(text)) => {
                    self.tags = text.lines().map(Into::into).collect();
                    reload_preview |= self.last_list == 1 && self.subview == 3;
                }
                (Request::Remotes, Ok(text)) => {
                    self.remotes = text.lines().map(Into::into).collect();
                    reload_preview |= self.last_list == 1 && self.subview == 2;
                }
                (Request::History, Err(_))
                    if self
                        .snapshot
                        .as_ref()
                        .is_some_and(|_| git_head_id(&self.root).ok() == Some(None)) =>
                {
                    self.history_loading = false;
                    self.history.clear();
                }
                (Request::History, Err(error)) => {
                    self.history_loading = false;
                    self.notice = error;
                }
                (_, Err(e)) => self.notice = e,
            }
        }
        if changed {
            for (pane, value) in selections.iter().enumerate() {
                if pane == 2 && history_was_loading && self.history_view == 0 {
                    continue;
                }
                if let Some(value) = value
                    && let Some(index) = self.rows(pane).iter().position(|row| &row.value == value)
                {
                    let slot = self.slot(pane);
                    self.selected[slot] = index;
                }
            }
            self.clamp();
        }
        if reload_preview {
            self.load_preview();
        }
        let tutor_changed = if changed { self.advance_tutor() } else { false };
        self.restore_rewrite_diagnostic();
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
            self.notice = self.rewrite_diagnostic.clone().unwrap_or(e);
        }
    }
    fn load_git_preview(&mut self, args: Vec<OsString>) {
        if self.preview_args.as_ref() != Some(&args) {
            self.preview = "Loading…".into();
            self.preview_cursor = None;
            self.preview_anchor = None;
            self.preview_offset = 0;
            self.preview_column = 0;
        }
        self.preview_args = Some(args.clone());
        let result = self.run(Request::Preview, args);
        if let Err(error) = &result {
            self.preview = error.clone().into();
        }
        self.report(result);
    }
    fn load_preview(&mut self) {
        self.ref_preview_target = None;
        self.ref_preview = None;
        if !self.file_selection_context() {
            self.clear_file_selection();
        }
        self.clear_hunks();
        self.preview_anchor = None;
        if self.last_list == 2 {
            if let Some(row) = self.selected_row() {
                let id = if self.history_view == 1 {
                    self.reflog
                        .iter()
                        .find(|entry| entry.key == row.value)
                        .map(|entry| entry.id.clone())
                } else {
                    Some(row.value.to_string_lossy().into_owned())
                };
                if let Some(id) = id {
                    self.load_commit(id);
                }
            } else if !(self.history_view == 0 && self.history_loading
                || self.history_view == 1 && self.reflog_loading)
            {
                self.commit = None;
                self.jobs.cancel_preview();
                self.preview_args = None;
                self.preview_cursor = None;
                self.preview = if self.history_view == 1 && self.reflog_error.is_some() {
                    self.reflog_error.as_deref().unwrap_or("Reflog unavailable")
                } else if self.filters[self.slot(2)].active() {
                    "No history matches · / edit filter · Esc clear"
                } else if self.history_view == 1 {
                    "No reflog entries"
                } else {
                    "No commits yet"
                }
                .into();
            }
            return;
        }
        self.commit = None;
        self.jobs.cancel_preview();
        let Some(row) = self.selected_row() else {
            self.preview_args = None;
            self.preview_cursor = None;
            self.preview = if self.filters[self.slot(self.last_list)].active() {
                "No matches · / edit filter · Esc clear"
            } else if self.last_list == 0 && self.files_view != 0 && self.refs.is_none() {
                "Loading repository data…"
            } else if self.last_list == 0 && self.files_view == 1 {
                "No worktrees"
            } else if self.last_list == 0 && self.files_view == 2 {
                self.refs
                    .as_ref()
                    .and_then(|refs| refs.submodules.as_ref().err())
                    .map(String::as_str)
                    .unwrap_or("No submodules")
            } else if self.last_list == 0 && self.changes_collapsed {
                "Changes folded · l / Enter expand"
            } else if self.last_list == 0 {
                "Working tree clean"
            } else {
                "No entries"
            }
            .into();
            return;
        };
        if self.last_list == 0 && self.files_view == 2 {
            self.preview_args = None;
            self.preview_cursor = None;
            self.submodule_preview(&row.value);
            return;
        }
        if self.last_list == 1 && self.subview == 2 && !self.remote_branch(&row) {
            self.preview_args = None;
            self.preview_cursor = None;
            self.preview = self
                .remotes
                .iter()
                .filter(|line| {
                    line.split_whitespace()
                        .next()
                        .is_some_and(|name| Path::new(name) == row.value)
                })
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
                .into();
            return;
        }
        if row.directory {
            self.preview_args = None;
            self.preview_cursor = None;
            self.preview = format!(
                "{}\n\nSpace toggles marked files if any; otherwise changed descendants.\nA mixed set is staged; a fully staged set is unstaged.\nEnter expands or collapses this directory.",
                row.value.display()
            )
            .into();
            return;
        }
        self.start_hunks(&row);
        let args: Vec<OsString> = match self.last_list {
            0 if self.files_view == 1 => vec![
                "-C".into(),
                row.value.into_os_string(),
                "log".into(),
                "-40".into(),
                "--format=%h  %ad  %s".into(),
                "--date=short".into(),
            ],
            0 => {
                let entry = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.entries.iter().find(|e| e.relative_path == row.value));
                let untracked = entry.is_some_and(|e| e.status == GitFileStatus::Untracked);
                if self.all_files
                    || (untracked
                        || entry.is_some_and(|entry| entry.status == GitFileStatus::Conflicted))
                        && !self.preview_staged()
                {
                    self.preview_args = None;
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
                            self.update_preview(Preview::source(&row.value, &text));
                        }
                        Err(e) => self.preview = e.to_string().into(),
                    }
                    return;
                }
                let mut a = vec![
                    "--literal-pathspecs".into(),
                    "diff".into(),
                    "--no-ext-diff".into(),
                    "--no-textconv".into(),
                ];
                if self.preview_staged() {
                    a.push("--cached".into())
                }
                a.push("--".into());
                a.push(row.value.into_os_string());
                a
            }
            1 => {
                let namespace = match self.subview {
                    2 => "remotes",
                    3 => "tags",
                    _ => "heads",
                };
                let reference = format!("refs/{namespace}/{}", row.value.display());
                self.ref_preview_target = Some(reference.clone());
                vec![
                    "log".into(),
                    "-40".into(),
                    "--format=%H%x00%an%x00%as%x00%s".into(),
                    "--no-decorate".into(),
                    "--end-of-options".into(),
                    reference.into(),
                    "--".into(),
                ]
            }

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
        self.load_git_preview(args);
    }
    fn reload_history(&mut self) -> Result<(), String> {
        let (root, reference) = self
            .history_target
            .clone()
            .unwrap_or_else(|| (self.root.clone(), "HEAD".into()));
        let result = self.jobs.start(
            &root,
            Request::History,
            vec![
                "log".into(),
                "-300".into(),
                "--date-order".into(),
                "--date=short".into(),
                "--format=%H%x09%P%x09%an%x09%ad%x09%D%x09%s".into(),
            ]
            .into_iter()
            // `--branches` is an option, so it must come before `--end-of-options`.
            .chain((reference == "HEAD").then(|| "--branches".into()))
            .chain(["--end-of-options".into(), reference.clone(), "--".into()])
            .collect(),
        );
        self.history_loading = result.is_ok();
        result
    }
    fn select_history(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let target = if self.last_list == 0 && self.files_view == 1 {
            (row.value, "HEAD".into())
        } else if self.last_list == 1 {
            if self.subview == 2 && !self.remote_branch(&row) {
                return;
            }
            (self.root.clone(), row.value.into_os_string())
        } else {
            return;
        };
        if self.history_target.as_ref() == Some(&target) {
            return;
        }
        self.history_target = Some(target);
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
        if self.busy || self.refreshing_after_write {
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
        self.write_with_input(args, None)
    }
    fn write_with_input(
        &mut self,
        args: Vec<OsString>,
        stdin: Option<Vec<u8>>,
    ) -> Result<(), String> {
        self.allow_write()?;
        self.jobs
            .start_with_input(&self.root, Request::Write, args, stdin)?;
        self.busy = true;
        self.notice = "Running Git…".into();
        Ok(())
    }
    fn stage(&mut self) -> Result<(), String> {
        self.allow_write()?;
        if self.focus == 0
            && self.file_selection_context()
            && !self.file_selection.marked.is_empty()
        {
            return self.stage_marked();
        }
        let row = self
            .selected_row()
            .filter(|_| self.last_list == 0)
            .ok_or("Select a changed file or directory")?;
        if self.files_view == 1 {
            return Err("Open the worktree before staging its changes".into());
        }
        if self.files_view == 2
            || self
                .refs
                .as_ref()
                .and_then(|refs| refs.submodules.as_ref().ok())
                .is_some_and(|modules| modules.iter().any(|module| module.path == row.value))
        {
            return self.toggle_submodule(row.value);
        }
        if row.directory {
            return self.stage_directory(&row.value);
        }
        let snapshot = self.snapshot.as_ref().ok_or("Status is loading")?;
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.relative_path == row.value)
            .ok_or("Select a changed file")?;
        let unstage = entry.sources.staged && !entry.sources.unstaged;
        let kind = if unstage {
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
                    layer: if unstage {
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
    /// G-COMMIT (DESIGN.md §3.4): refuses to open with nothing staged.
    fn commit_form(&mut self) {
        let staged = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.entries.iter().any(|entry| entry.sources.staged));
        if !staged {
            self.notice = "Nothing staged · Space stages the selected change".into();
            return;
        }
        self.commit_options = [false; 3];
        self.form(
            "Commit staged changes",
            FormAction::Commit,
            vec![("Subject", String::new()), ("Body", String::new())],
        );
        if let Some(form) = &mut self.form {
            form.fields[1].required = false;
            form.fields[1].multiline = true;
        }
    }
    /// Amending starts from HEAD's message when the sheet is still empty.
    fn prefill_amend_message(&mut self) {
        let Some(form) = &mut self.form else { return };
        if form.fields.iter().any(|field| !field.input.input.trim().is_empty()) {
            return;
        }
        let Some(message) = git2::Repository::open(&self.root).ok().and_then(|repository| {
            Some(repository.head().ok()?.peel_to_commit().ok()?.message()?.to_owned())
        }) else {
            return;
        };
        let (subject, body) = message.split_once('\n').unwrap_or((&message, ""));
        for (field, text) in form.fields.iter_mut().zip([subject.trim(), body.trim()]) {
            field.input.input = text.to_owned();
            field.input.cursor = field.input.input.len();
        }
    }
    fn form(&mut self, title: &str, action: FormAction, fields: Vec<(&str, String)>) {
        // A fresh sheet starts without the previous action's notice; its status line
        // then only ever reports this sheet's own refusals.
        self.notice.clear();
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
    fn operation_target(
        &self,
        operation: &str,
    ) -> Result<(refs::Target, Vec<git2::Oid>, Option<RebaseIdentity>), String> {
        let repository = git2::Repository::open(&self.root).map_err(|error| error.to_string())?;
        let head = refs::head_target(&repository)?;
        let incoming = match operation {
            "merge" => super::conflict::recorded_merge_heads(&repository)
                .map_err(|error| error.to_string())?,
            "rebase" => repository
                .refname_to_id("REBASE_HEAD")
                .ok()
                .into_iter()
                .collect(),
            "cherry-pick" => repository
                .refname_to_id("CHERRY_PICK_HEAD")
                .ok()
                .into_iter()
                .collect(),
            "revert" => repository
                .refname_to_id("REVERT_HEAD")
                .ok()
                .into_iter()
                .collect(),
            _ => Vec::new(),
        };
        let rebase_identity = if operation == "rebase" {
            use std::io::Read;
            let (backend, directory) = ["rebase-merge", "rebase-apply"]
                .into_iter()
                .map(|name| (name, repository.path().join(name)))
                .find(|(_, path)| path.is_dir())
                .ok_or("Rebase metadata is unavailable")?;
            let read = |name: &str| -> Result<Vec<u8>, String> {
                let mut bytes = Vec::new();
                std::fs::File::open(directory.join(name))
                    .map_err(|error| format!("Cannot read rebase {name}: {error}"))?
                    .take(4097)
                    .read_to_end(&mut bytes)
                    .map_err(|error| format!("Cannot read rebase {name}: {error}"))?;
                if bytes.is_empty() || bytes.len() > 4096 {
                    return Err(format!("Invalid rebase {name} metadata"));
                }
                Ok(bytes)
            };
            Some(RebaseIdentity {
                backend,
                original: read("orig-head")?,
                onto: read("onto")?,
                branch: read("head-name")?,
            })
        } else {
            None
        };
        Ok((head, incoming, rebase_identity))
    }
    fn operation_form(&mut self, action: GitOperationAction) -> Result<(), String> {
        let state = read_conflict_state(&self.root).map_err(|error| error.to_string())?;
        let bar = GitOperationBar::of(&state).ok_or("No Git operation is paused")?;
        if !bar.offers(action) {
            return Err(
                if action == GitOperationAction::Continue && !bar.blockers.is_empty() {
                    "Resolve and stage the remaining conflicts before continuing".into()
                } else {
                    format!("{} for {} is not supported", action.name(), bar.operation)
                },
            );
        }
        let (head, incoming, rebase_identity) = self.operation_target(&bar.operation)?;
        let repository = git2::Repository::open(&self.root).map_err(|error| error.to_string())?;
        let describe = |id: git2::Oid| -> String {
            let subject = repository
                .find_commit(id)
                .ok()
                .and_then(|commit| commit.summary().map(str::to_owned))
                .unwrap_or_default();
            format!("{}  {subject}", &id.to_string()[..8])
        };
        let detached = rebase_identity.as_ref().is_some_and(|identity| {
            String::from_utf8_lossy(&identity.branch).trim() == "detached HEAD"
        }) || (rebase_identity.is_none()
            && repository.head_detached().unwrap_or(false));
        let branch = rebase_identity
            .as_ref()
            .map(|identity| {
                String::from_utf8_lossy(&identity.branch)
                    .trim()
                    .trim_start_matches("refs/heads/")
                    .to_owned()
            })
            .unwrap_or_else(|| head.name.trim_start_matches("refs/heads/").to_owned());
        let (title, mut context) = match action {
            GitOperationAction::Abort => {
                let start = if let Some(identity) = &rebase_identity {
                    git2::Oid::from_str(String::from_utf8_lossy(&identity.original).trim())
                        .map_err(|error| error.to_string())?
                } else {
                    repository
                        .refname_to_id("ORIG_HEAD")
                        .map_err(|error| error.to_string())?
                };
                (
                    format!("Abort {}?", bar.operation),
                    vec![
                        (
                            if detached {
                                "Return to"
                            } else {
                                "Return to branch"
                            },
                            branch,
                        ),
                        ("Restore commit", describe(start)),
                        ("Conflict edits", "Discard".into()),
                    ],
                )
            }
            GitOperationAction::Skip => (
                "Skip current rebase commit?".into(),
                vec![(
                    "Drop commit",
                    describe(*incoming.first().ok_or("No current rebase commit")?),
                )],
            ),
            GitOperationAction::Continue => (
                format!("Continue resolved {}?", bar.operation),
                vec![
                    (if detached { "Checkout" } else { "Branch" }, branch),
                    (
                        "Record resolution",
                        describe(incoming.first().copied().unwrap_or(head.id)),
                    ),
                ],
            ),
            GitOperationAction::Resolve => return Err("Select a conflicted file to resolve".into()),
        };
        if bar.operation == "rebase" {
            let blockers = super::conflict::rebase_action_blockers(&self.root, action)
                .map_err(|error| error.to_string())?;
            if !blockers.is_empty() {
                context.push(("Unrelated changes", blockers.join(", ")));
            }
        }
        let fields = context
            .into_iter()
            .map(|(label, value)| {
                let mut field = Field::new(label, value);
                field.read_only = true;
                field.input.cursor = 0;
                field
            })
            .collect::<Vec<_>>();
        let blocked = fields
            .iter()
            .any(|field| field.label == "Unrelated changes");
        self.form = Some(Form {
            title,
            action: FormAction::Operation {
                action,
                operation: bar.operation.clone(),
                head,
                incoming,
                rebase_identity,
            },
            selected: fields.len() + usize::from(blocked),
            fields,
        });
        self.operation_bar = Some(bar);
        self.notice.clear();
        Ok(())
    }

    fn conflict_form(&mut self, path: PathBuf, side: &str) {
        let mut choice = Field::new("Keep side (Up/Down selects)", side.into());
        choice.choices = vec!["ours".into(), "theirs".into()];
        self.notice.clear();
        self.form = Some(Form {
            title: format!("Replace {} with one side; stage separately", path.display()),
            action: FormAction::Conflict(path),
            fields: vec![choice],
            selected: 0,
        });
    }
    fn submit(&mut self) -> Result<(), String> {
        let form = self.form.as_ref().ok_or("No open form")?;
        if !matches!(&form.action, FormAction::Command | FormAction::SourceSearch)
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
        let keep_form = matches!(
            &form.action,
            FormAction::Branch(Some(_))
                | FormAction::CheckoutDetached(_)
                | FormAction::DetachedWorktree(_)
                | FormAction::Reference(_)
                | FormAction::Rewrite(_)
        ) && !matches!(
            &form.action,
            FormAction::Reference(
                refs::Action::MergeBranch { .. } | refs::Action::RebaseBranch { .. }
            )
        );
        match &form.action {
            FormAction::Rewrite(plan) => {
                self.rewrite
                    .start(plan.as_ref().clone(), values[0].clone())?;
                self.rewrite_diagnostic = None;
                self.busy = true;
            }
            FormAction::SourceSearch => self.find_source(values[0].clone())?,
            FormAction::Commit => {
                let subject = values[0].trim();
                let body = values.get(1).map_or("", |body| body.trim());
                let message = if body.is_empty() {
                    subject.to_owned()
                } else {
                    format!("{subject}\n\n{body}")
                };
                let mut args: Vec<OsString> = vec!["commit".into()];
                let [signoff, amend, skip_hooks] = self.commit_options;
                if signoff {
                    args.push("--signoff".into());
                }
                if amend {
                    args.push("--amend".into());
                }
                if skip_hooks {
                    args.push("--no-verify".into());
                }
                args.extend(["-m".into(), message.into()]);
                self.write(args)?
            }
            FormAction::Branch(target) => {
                let name = &values[0];
                if name.starts_with('-')
                    || !git2::Reference::is_valid_name(&format!("refs/heads/{name}"))
                {
                    return Err("Choose a valid branch name".into());
                }
                let mut args = vec!["branch".into(), "--".into(), name.clone().into()];
                if let Some(id) = target {
                    args.push(id.clone().into());
                }
                self.write(args)?;
            }
            FormAction::CheckoutDetached(id) => {
                self.write(vec![
                    "checkout".into(),
                    "--detach".into(),
                    id.clone().into(),
                    "--".into(),
                ])?;
            }
            FormAction::DetachedWorktree(id) => {
                let (path, name) = self.reflog_worktree_path(&values[0])?;
                if let Some(tutor) = &self.tutor {
                    tutor.check_destination(&path)?;
                }
                self.branches
                    .start(GitBranchRequest::CreateDetachedWorktree {
                        repository: self.root.clone(),
                        name,
                        path,
                        revision: id.clone(),
                        deadline: Duration::from_secs(15),
                    })
                    .map_err(|error| error.to_string())?;
                self.busy = true;
                self.branch_mutation = true;
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
            FormAction::RemoveWorktree(name) => {
                self.branches
                    .start(GitBranchRequest::RemoveWorktree {
                        repository: self.root.clone(),
                        name: name.clone(),
                        deadline: Duration::from_secs(15),
                    })
                    .map_err(|error| error.to_string())?;
                self.busy = true;
                self.branch_mutation = true;
            }
            FormAction::Reference(action) => self.submit_reference(action.clone(), &values)?,
            FormAction::Confirm(args) => self.write(args.clone())?,
            FormAction::Discard(request) => {
                // allow_write above rechecks the host and post-write readiness.
                // Submit the dialog's immutable capture, never the current row.
                self.mutation
                    .start(GitMutationRequest::ResetUnstagedHunk(
                        request.as_ref().clone(),
                    ))
                    .map_err(|error| error.to_string())?;
                self.busy = true;
                self.preview_anchor = None;
            }
            FormAction::DiscardFiles(plan) => {
                // allow_write above rechecked the host; the plan rechecks every path.
                let plan = plan.as_ref().clone();
                self.discard_files(&plan)?;
            }
            FormAction::Github(github::Action::CreatePullRequest) => {
                self.form = Some(self.github.review_pr(values)?);
                self.notice.clear();
                return Ok(());
            }
            FormAction::Github(action) => self.github.submit(action.clone(), values)?,
            FormAction::Command => {
                self.tutor_command(values[0].trim())?;
                return Ok(());
            }
            FormAction::Conflict(path) => {
                let choice = GitConflictChoice::parse(&values[0]).ok_or("Choose ours or theirs")?;
                let plan = plan_replacement(&self.root, std::slice::from_ref(path), choice)
                    .map_err(|error| error.to_string())?;
                apply_replacement(&plan).map_err(|error| error.to_string())?;
                self.refresh()?;
            }
            FormAction::Operation {
                action,
                operation,
                head,
                incoming,
                rebase_identity,
            } => {
                let state = read_conflict_state(&self.root).map_err(|error| error.to_string())?;
                if state.operation.as_ref() != Some(operation)
                    || self.operation_target(operation)?
                        != (head.clone(), incoming.clone(), rebase_identity.clone())
                {
                    return Err(
                        "The paused operation changed; cancel and reopen this action".into(),
                    );
                }
                let outcome = match action {
                    GitOperationAction::Continue => continue_operation(&self.root, ""),
                    GitOperationAction::Abort => abort_operation(&self.root),
                    GitOperationAction::Skip => skip_operation(&self.root),
                    GitOperationAction::Resolve => {
                        return Err("Select a conflicted file to resolve".into());
                    }
                };
                let done = {
                    let mut name = operation.chars();
                    let name = name
                        .next()
                        .map(|first| first.to_uppercase().chain(name).collect::<String>())
                        .unwrap_or_default();
                    if *action == GitOperationAction::Skip {
                        format!("{name} commit skipped")
                    } else {
                        format!("{name} continued")
                    }
                };
                let refreshed = self.refresh();
                let outcome = outcome.map_err(|error| error.to_string())?;
                // The bar describes the repository now, not the step just confirmed.
                self.operation_bar = read_conflict_state(&self.root)
                    .ok()
                    .and_then(|state| GitOperationBar::of(&state));
                // A replay that moved on and stopped on the next conflict did what was
                // confirmed: close the sheet and say where it stopped, without an error.
                self.receipt = outcome
                    .stopped
                    .as_ref()
                    .map(|stop| (ReceiptTone::Notice, format!("{done} · {}", stop.summary())));
                refreshed?;
                if let Some(warning) = outcome.cleanup_warning {
                    return Err(warning);
                }
            }
        }
        if keep_form {
            self.reflog_submitting = true;
        } else {
            self.form = None;
        }
        self.notice.clear();
        Ok(())
    }
    fn form_key(&mut self, key: &KeyEvent) -> Result<(), String> {
        if self.reflog_submitting {
            return Ok(());
        }
        if key.code == KeyCode::Escape {
            if self.form.as_ref().is_some_and(|form| {
                matches!(
                    form.action,
                    FormAction::Github(github::Action::ConfirmPullRequest(_))
                )
            }) {
                self.form = self.github.activate(github::Action::CreatePullRequest)?;
            } else {
                if self
                    .form
                    .as_ref()
                    .is_some_and(|form| matches!(form.action, FormAction::Rewrite(_)))
                {
                    self.rewrite_diagnostic = None;
                    self.notice.clear();
                }
                self.save_form_draft();
                self.form = None;
            }
            return Ok(());
        }
        if key.code == KeyCode::Enter
            && (key.modifiers.control
                || self.form.as_ref().is_some_and(|form| {
                    matches!(form.action, FormAction::Command | FormAction::SourceSearch)
                        && form.selected <= form.fields.len()
                }))
        {
            return self.submit();
        }
        if key.modifiers.alt
            && let KeyCode::Char(letter @ ('s' | 'a' | 'n')) = key.code
            && self
                .form
                .as_ref()
                .is_some_and(|form| matches!(form.action, FormAction::Commit))
        {
            let index = match letter {
                's' => 0,
                'a' => 1,
                _ => 2,
            };
            self.commit_options[index] = !self.commit_options[index];
            if index == 1 && self.commit_options[1] {
                self.prefill_amend_message();
            }
            return Ok(());
        }
        let form = self.form.as_mut().expect("checked form");
        let count = form.fields.len() + 2;
        if key.code == KeyCode::Tab && matches!(form.action, FormAction::Operation { .. }) {
            form.selected = if form.selected == form.fields.len() {
                form.fields.len() + 1
            } else {
                form.fields.len()
            };
            return Ok(());
        }
        if key.code == KeyCode::Tab {
            loop {
                form.selected =
                    (form.selected + if key.modifiers.shift { count - 1 } else { 1 }) % count;
                if !matches!(form.action, FormAction::Rewrite(_))
                    || form.selected >= form.fields.len()
                    || !form.fields[form.selected].read_only
                {
                    break;
                }
            }
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
            if field.label == "Template" {
                github::template_chosen(&self.github, form);
            }
            return Ok(());
        }
        let input = &mut field.input;
        match key.code {
            KeyCode::Char('a') if key.modifiers.control => {
                input.cursor = input.input[..input.cursor]
                    .rfind('\n')
                    .map_or(0, |at| at + 1);
            }
            KeyCode::Char('e') if key.modifiers.control => {
                input.cursor = input.input[input.cursor..]
                    .find('\n')
                    .map_or(input.input.len(), |at| input.cursor + at);
            }
            KeyCode::Char('u') if key.modifiers.control => {
                let start = input.input[..input.cursor]
                    .rfind('\n')
                    .map_or(0, |at| at + 1);
                input.input.drain(start..input.cursor);
                input.cursor = start;
            }
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
        let outcome = self.handle_event(event);
        // After every input, including keys a sheet, filter or overlay consumed early.
        self.advance_tutor();
        outcome
    }
    fn handle_event(&mut self, event: UiEvent) -> ScreenOutcome {
        let previous = (self.last_list, self.subview, self.selected, self.staged);
        match event {
            UiEvent::Paste(text) => {
                if self.reflog_submitting {
                    return ScreenOutcome::Continue;
                }
                if self.paste_filter(&text) {
                    return ScreenOutcome::Continue;
                }
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
                if self.message_key(&key) {
                    return ScreenOutcome::Continue;
                }
                if self.help {
                    if matches!(key.code, KeyCode::Escape | KeyCode::Char('?' | 'q')) {
                        self.help = false;
                    }
                    return ScreenOutcome::Continue;
                }
                if self.form.is_none() && self.file_selection_key(&key) {
                    return ScreenOutcome::Continue;
                }
                if self.form.is_none() && self.filter_key(&key) {
                    return ScreenOutcome::Continue;
                }
                if self.form.is_none() && self.search_key(&key) {
                    return ScreenOutcome::Continue;
                }
                if self.form.is_none() && self.github.actions_typing() {
                    // An Actions find or filter row takes every key, `?` and `:` included.
                    return self.github_key(&key);
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
                if self.rewrite_key(&key) {
                    return ScreenOutcome::Continue;
                }
                if self.page == 0
                    && self.last_list == 2
                    && self.commit.is_some()
                    && key.code == KeyCode::Char('m')
                    && !key.modifiers.control
                {
                    self.message_scroll = Some(0);
                    return ScreenOutcome::Continue;
                }
                if self.page == 0
                    && self.focus < 3
                    && !key.modifiers.control
                    && !key.modifiers.alt
                    && matches!(key.code, KeyCode::Char('H' | 'L'))
                {
                    self.switch_sidebar_tab(key.code == KeyCode::Char('L'));
                    return ScreenOutcome::Continue;
                }
                if self.page == 0
                    && (self.focus == 4 || self.commit_open)
                    && !key.modifiers.control
                    && !key.modifiers.alt
                    && matches!(key.code, KeyCode::Char('d' | 'D'))
                {
                    // Historical file navigation consumes all other keys; refuse
                    // discard here too rather than silently ignoring it there.
                    let result = self.discard_diff(key.code == KeyCode::Char('D'));
                    self.report(result);
                    return ScreenOutcome::Continue;
                }
                if self.page == 0
                    && !key.modifiers.control
                    && !key.modifiers.alt
                    && matches!(key.code, KeyCode::Char('C' | 'X' | 'S'))
                {
                    let action = match key.code {
                        KeyCode::Char('C') => GitOperationAction::Continue,
                        KeyCode::Char('S') => GitOperationAction::Skip,
                        _ => GitOperationAction::Abort,
                    };
                    let result = self.operation_form(action);
                    self.report(result);
                    return ScreenOutcome::Continue;
                }
                if self.commit_key(&key) {
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
                if self.page == 0
                    && !key.modifiers.control
                    && !key.modifiers.alt
                    && matches!(key.code, KeyCode::Char('[' | ']'))
                {
                    // Sub-tabs of the focused pane; in the reader, Working tree ↔ Staged.
                    if self.focus < 3 {
                        self.switch_sidebar_tab(key.code == KeyCode::Char(']'));
                    } else if self.focus == 4 && self.last_list == 0 && self.files_view == 0 {
                        self.clear_file_selection();
                        self.staged = !self.staged;
                        self.all_files = false;
                        self.load_preview();
                    }
                    return ScreenOutcome::Continue;
                }
                if !key.modifiers.control
                    && matches!(key.code, KeyCode::Char('[' | ']' | '<' | '>'))
                    // On Pull requests and Issues `[`/`]` move between sections and tabs.
                    && !((matches!(self.page, 1 | 2 | github::notifications::PAGE) || self.github.actions_brackets())
                        && matches!(key.code, KeyCode::Char('[' | ']')))
                    && !self.busy()
                {
                    let forward = matches!(key.code, KeyCode::Char(']' | '>'));
                    if self.tutor.is_some() {
                        self.notice = "Use :tutor quit before opening GitHub pages".into();
                        return ScreenOutcome::Continue;
                    }
                    self.notice.clear();
                    self.clear_file_selection();
                    // Top tabs in display order: Git, Pull requests, Issues, Actions, then
                    // Notifications where the forge offers them to this session.
                    let tabs = Self::TAB_PAGES
                        .iter()
                        .copied()
                        .filter(|&page| page != github::notifications::PAGE || self.github.notifications_available())
                        .collect::<Vec<_>>();
                    let at = tabs.iter().position(|&page| page == self.page).unwrap_or(0);
                    self.page = tabs[(at + if forward { 1 } else { tabs.len() - 1 }) % tabs.len()];
                    self.github.page(self.page);
                    if self.page == 0 {
                        let result = self.refresh();
                        self.report(result);
                    }
                    return ScreenOutcome::Continue;
                }
                if self.page != 0 {
                    return self.github_key(&key);
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
                            if self.receipt.take().is_some() {
                                // Dismiss the receipt band first.
                            } else if self.preview_anchor.take().is_some() {
                                self.notice.clear();
                                // Cancel a native diff range before changing focus.
                            } else if !self.file_selection.marked.is_empty() {
                                self.clear_file_selection();
                                self.notice.clear();
                            } else if self.focus == 4 {
                                self.focus = self.last_list
                            } else if self.last_list == 2 {
                                self.focus = 0;
                                self.last_list = 0;
                            } else if self.tutor.is_some() {
                                let result = self.stop_tutor();
                                self.report(result);
                                return ScreenOutcome::Continue;
                            } else if !self.busy {
                                return ScreenOutcome::Close;
                            }
                        }
                        KeyCode::Char('0') => self.focus = 4,
                        KeyCode::Char(c @ '2'..='5') => self.focus = (c as u8 - b'2') as usize,
                        KeyCode::Char('1') => {
                            self.notice = "Repository switcher: open another repository with :open".into()
                        }
                        KeyCode::Tab => {
                            self.focus = (self.focus + if key.modifiers.shift { 4 } else { 1 }) % 5
                        }
                        KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
                        KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
                        KeyCode::PageDown => self.move_selection(if self.focus == 4 {
                            self.preview_visible.max(1) as isize
                        } else {
                            15
                        }),
                        KeyCode::PageUp => self.move_selection(if self.focus == 4 {
                            -(self.preview_visible.max(1) as isize)
                        } else {
                            -15
                        }),
                        KeyCode::Home if self.focus == 4 => self.preview_edge(false),
                        KeyCode::End if self.focus == 4 => self.preview_edge(true),
                        KeyCode::Home => self.move_selection(isize::MIN),
                        KeyCode::End => self.move_selection(isize::MAX),
                        KeyCode::Char('s') if self.focus == 4 => {
                            self.split_diff = !self.split_diff;
                            self.preview_offset = 0;
                            if self.preview.cursor_byte.is_none() {
                                self.preview_cursor = None;
                                self.preview_column = 0;
                            }
                            if self.preview_anchor.take().is_some() {
                                self.notice.clear();
                            }
                        }
                        KeyCode::Char(' ' | 'a' | 'V')
                            if self.focus == 4 && self.last_list == 0 =>
                        {
                            let result = if self.jobs.preview_loading() {
                                Err(DIFF_REFRESHING.into())
                            } else if key.code == KeyCode::Char('V') {
                                self.start_line_range()
                            } else {
                                self.stage_diff(key.code == KeyCode::Char('a'))
                            };
                            self.report(result);
                        }
                        KeyCode::Left | KeyCode::Char('h') if self.focus == 4 => {
                            self.preview.search.leave_match();
                            self.preview.cursor_byte = None;
                            self.preview_column = self.preview_column.saturating_sub(8);
                        }
                        KeyCode::Right | KeyCode::Char('l') if self.focus == 4 => {
                            self.preview.search.leave_match();
                            self.preview.cursor_byte = None;
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
                        KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l')
                            if self.focus == 0 && self.files_view == 0 =>
                        {
                            self.finish_file_range();
                            let close = matches!(key.code, KeyCode::Left | KeyCode::Char('h'));
                            let was_collapsed = self.changes_collapsed;
                            if self.filters[0].active() {
                                self.notice = "Matching paths stay expanded; Esc clears the filter before folding".into();
                            } else if self.changes_collapsed {
                                self.changes_collapsed = close;
                            } else if let Some(row) = self.selected_row() {
                                if row.directory && (!close || !self.collapsed.contains(&row.value))
                                {
                                    if close {
                                        self.collapsed.insert(row.value);
                                    } else {
                                        self.collapsed.remove(&row.value);
                                    }
                                } else if close {
                                    if let Some(parent) = tree::parent(&self.rows(0), &row.value) {
                                        self.collapsed.insert(parent.clone());
                                        self.selected[0] = self
                                            .rows(0)
                                            .iter()
                                            .position(|row| row.value == parent)
                                            .unwrap_or(0);
                                    } else {
                                        self.changes_collapsed = true;
                                    }
                                }
                            }
                            // Folding everything away leaves nothing selected; the reader
                            // must not keep the hidden file, and unfolding brings it back.
                            if self.changes_collapsed != was_collapsed {
                                self.load_preview();
                            }
                        }
                        // `v`: stage by lines in the reader; `F` browses every file.
                        KeyCode::Char('v')
                            if self.focus == 0
                                && self.last_list == 0
                                && self.files_view == 0
                                && self.preview.patch =>
                        {
                            self.focus = 4;
                        }
                        KeyCode::Char('t')
                            if self.focus == 0
                                && self.last_list == 0
                                && self.files_view == 0
                                && !self
                                    .selected_row()
                                    .and_then(|row| {
                                        self.snapshot.as_ref()?.entries.iter().find(|entry| {
                                            entry.relative_path == row.value
                                        })
                                    })
                                    .is_some_and(|entry| entry.status == GitFileStatus::Conflicted) =>
                        {
                            self.changes_tree = !self.changes_tree;
                            self.clamp();
                        }
                        KeyCode::Char('F') if self.last_list == 0 && self.files_view == 0 => {
                            self.clear_file_selection();
                            self.all_files = !self.all_files;
                            self.staged = false;
                            if self.all_files {
                                let result = self.list_files();
                                self.report(result);
                            }
                            self.load_preview();
                        }
                        // lazygit `a`: stage everything, or unstage everything when all is staged.
                        KeyCode::Char('a')
                            if self.focus == 0
                                && self.last_list == 0
                                && self.files_view == 0
                                && !self.all_files =>
                        {
                            let entries = self
                                .snapshot
                                .as_ref()
                                .map_or(&[][..], |snapshot| snapshot.entries.as_slice());
                            let all_staged = !entries.is_empty()
                                && entries
                                    .iter()
                                    .all(|entry| entry.sources.staged && !entry.sources.unstaged);
                            let has_head = git2::Repository::open(&self.root)
                                .is_ok_and(|repository| repository.head().is_ok());
                            let result = if entries.is_empty() {
                                Err("Working tree clean".into())
                            } else if all_staged && has_head {
                                self.write(vec!["reset".into(), "-q".into(), "--".into()])
                            } else if all_staged {
                                self.write(vec![
                                    "rm".into(),
                                    "-r".into(),
                                    "-q".into(),
                                    "--cached".into(),
                                    "--".into(),
                                    ".".into(),
                                ])
                            } else {
                                self.write(vec!["add".into(), "-A".into(), "--".into()])
                            };
                            self.report(result);
                        }
                        KeyCode::Char('i') if self.last_list == 0 && self.files_view == 0 => {
                            self.clear_file_selection();
                            self.staged = !self.staged;
                            self.all_files = false;
                        }
                        KeyCode::Char(' ') if self.last_list == 0 => {
                            let result = self.stage();
                            self.report(result);
                        }
                        KeyCode::Char('o' | 't') if self.last_list == 0 && self.files_view == 0 => {
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
                        KeyCode::Char('c') => self.commit_form(),
                        KeyCode::Char('n' | ' ' | 'w')
                            if self.focus == 2 && self.history_view == 1 && !key.modifiers.alt =>
                        {
                            self.reflog_form(&key.code);
                        }
                        KeyCode::Char('n') if self.focus == 3 => self.form(
                            "Save stash (includes untracked files)",
                            FormAction::Stash,
                            vec![("Message", String::new())],
                        ),
                        KeyCode::Char('d') if self.focus == 0 && self.files_view == 1 => {
                            self.remove_sidebar_worktree()
                        }
                        KeyCode::Char('u') if self.focus == 0 && self.files_view == 2 => {
                            self.initialize_submodule()
                        }
                        KeyCode::Char('f') if self.focus == 1 && self.subview == 2 => {
                            self.fetch_sidebar_remote()
                        }
                        KeyCode::Char('e' | 'u' | 'd' | 'm' | 'R')
                            if self.focus == 1
                                && (self.subview == 0
                                    || self.subview == 3 && key.code == KeyCode::Char('d')) =>
                        {
                            let result = self.reference_form(&key.code);
                            self.report(result);
                        }
                        KeyCode::Char('n') if self.focus == 1 && self.subview == 0 => self.form(
                            "Create local branch",
                            FormAction::Branch(None),
                            vec![("Name", String::new())],
                        ),
                        KeyCode::Char('n') if self.focus == 0 && self.files_view == 1 => self.form(
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
                        KeyCode::Char(code @ ('d' | 'D'))
                            if self.focus == 0 && self.files_view == 0 =>
                        {
                            let result = self.discard_files_form(code == 'D');
                            self.report(result);
                        }
                        KeyCode::Char('d' | 'D') => {
                            self.notice = "Discard works on Changes rows, or on an unstaged hunk or diff-line range in the reader".into();
                        }
                        KeyCode::Char('r') if !self.busy => {
                            self.notice.clear();
                            let result = self.refresh();
                            self.report(result);
                        }
                        KeyCode::Enter
                            if self.focus == 0
                                && self.files_view == 0
                                && self.changes_collapsed =>
                        {
                            self.changes_collapsed = false;
                            self.load_preview();
                        }
                        KeyCode::Enter => {
                            if let Some(row) = self.selected_row() {
                                if row.directory {
                                    self.finish_file_range();
                                    if self.filters[0].active() {
                                        self.notice = "Matching paths stay expanded; Esc clears the filter before folding".into();
                                    } else if !self.collapsed.remove(&row.value) {
                                        self.collapsed.insert(row.value);
                                    }
                                } else if self.last_list == 0 && self.files_view != 0 {
                                    return self.open_sidebar_repository(row);
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
                                } else if self.last_list == 2 {
                                    self.commit_open = true;
                                    self.focus = 0;
                                } else if self.last_list == 1
                                    && self.subview == 2
                                    && self.remote_branch(&row)
                                {
                                    self.form(
                                        &format!(
                                            "Create a local branch tracking {}?",
                                            row.value.display()
                                        ),
                                        FormAction::Confirm(vec![
                                            "switch".into(),
                                            "--track".into(),
                                            "--".into(),
                                            row.value.into_os_string(),
                                        ]),
                                        Vec::new(),
                                    );
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
                if self.focus == 0 && self.last_list == 2 && self.commit.is_some() {
                    self.commit_open = true;
                }
                if self.focus < 4 && !self.commit_open {
                    self.last_list = self.focus;
                    self.clamp();
                }
                if self.focus != 0 {
                    self.finish_file_range();
                }
                if self.last_list != 0 {
                    self.clear_file_selection();
                }
                if previous != (self.last_list, self.subview, self.selected, self.staged) {
                    self.select_history();
                    self.preview_offset = 0;
                    self.preview_column = 0;
                    self.load_preview();
                }
            }
            _ => {}
        }
        ScreenOutcome::Continue
    }
    fn move_selection(&mut self, delta: isize) {
        if self.focus == 4 {
            self.move_preview(delta);
        } else {
            let slot = self.slot(self.focus);
            self.selected[slot] = self.selected[slot]
                .saturating_add_signed(delta)
                .min(self.rows(self.focus).len().saturating_sub(1));
            if self.focus == 0 && self.file_selection_context() {
                self.extend_file_range();
            }
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
    /// A key for the forge tabs; `open:<path>` from Actions opens a file named in a log.
    fn github_key(&mut self, key: &KeyEvent) -> ScreenOutcome {
        self.notice.clear();
        let result = self.github.event(key, self.write_refusal.as_deref());
        self.page = self.github.current_page();
        match result {
            Ok(Some(form)) => self.form = Some(form),
            Ok(None) => {}
            Err(error) if error == "close" && !self.busy() => {
                return ScreenOutcome::Close;
            }
            Err(error) if error.starts_with("open:") => {
                return ScreenOutcome::OpenFile(PathBuf::from(&error["open:".len()..]));
            }
            Err(error) => self.notice = error,
        }
        ScreenOutcome::Continue
    }
    pub fn frame(&mut self, width: u16, height: u16) -> Frame {
        self.restore_rewrite_diagnostic();
        let mut frame = if self.page == 0 {
            shell::frame(self, width, height)
        } else {
            github::frame(self, width, height)
        };
        if self.message_scroll.is_some() {
            commit::message(&mut frame, self, width, height);
        }
        if self.help && self.page <= 3 {
            shell::help::paint(self, &mut frame, width, height);
        } else if self.help {
            render::help(
                &mut frame,
                width,
                height,
                if self.commit_open {
                    4
                } else if self.page == 0 && self.focus == 2 && self.history_view == 1 {
                    5
                } else if self.page == 0 && self.focus == 1 && self.subview == 0 {
                    6
                } else if self.page == 0 && self.focus == 1 && self.subview == 3 {
                    7
                } else {
                    self.page
                },
            );
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

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn prepared_ref_context_uses_exact_namespace_actual_head_and_rejects_a_moved_tip() {
        let directory = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(directory.path()).unwrap();
        let signature = git2::Signature::now("Ref context", "context@example.invalid").unwrap();
        let tree_id = repository.treebuilder(None).unwrap().write().unwrap();
        let tree = repository.find_tree(tree_id).unwrap();
        let tip = repository
            .commit(
                Some("refs/heads/main"),
                &signature,
                &signature,
                "Visible subject",
                &tree,
                &[],
            )
            .unwrap();
        repository.set_head("refs/heads/main").unwrap();
        let selected = "refs/heads/feature/commas,(punctuation)";
        repository
            .reference(selected, tip, false, "selected")
            .unwrap();
        let parent = repository.find_commit(tip).unwrap();
        let other = repository
            .commit(None, &signature, &signature, "Other", &tree, &[&parent])
            .unwrap();
        repository
            .reference(
                "refs/tags/feature/commas,(punctuation)",
                other,
                false,
                "collision",
            )
            .unwrap();
        let shared = RefPreviewContext::capture(&repository, selected, tip).unwrap();
        assert_eq!(shared.selected, "feature/commas,(punctuation)");
        assert_eq!(shared.current.as_deref(), Some("main"));
        assert!(shared.head_at_tip && !shared.selected_is_current);
        assert_eq!(shared.tip, tip);
        repository
            .reference(selected, other, true, "external move")
            .unwrap();
        assert!(RefPreviewContext::capture(&repository, selected, tip).is_none());
        let unshared = RefPreviewContext::capture(&repository, selected, other).unwrap();
        assert!(!unshared.head_at_tip && !unshared.selected_is_current);
        repository.set_head_detached(other).unwrap();
        let detached = RefPreviewContext::capture(&repository, selected, other).unwrap();
        assert!(
            detached.current.is_none() && detached.head_at_tip && !detached.selected_is_current
        );
        repository.set_head(selected).unwrap();
        let current = RefPreviewContext::capture(&repository, selected, other).unwrap();
        assert!(current.selected_is_current && current.head_at_tip);
    }

    #[test]
    fn stale_rebase_head_without_an_active_rebase_is_not_replay_context() {
        let directory = tempfile::tempdir().unwrap();
        let repository = git2::Repository::init(directory.path()).unwrap();
        std::fs::write(
            repository.path().join("REBASE_HEAD"),
            format!("{}\n", "a".repeat(40)),
        )
        .unwrap();
        assert!(PausedReplay::capture(&repository).is_none());
    }
}
