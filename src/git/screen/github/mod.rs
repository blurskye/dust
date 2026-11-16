//! GitHub workspace state. GitHub CLI owns authentication; the existing forge worker
//! owns process limits and cancellation. Every response is keyed to its requested item.
mod accounts;
mod actions;
mod checks;
mod conversation;
mod create;
mod disk;
mod drafts;
mod files;
mod find;
mod forms;
mod fresh;
mod inbox;
mod issue;
pub(super) mod keymap;
mod keys;
mod lanes;
mod lines;
mod logs;
pub(super) mod notifications;
pub(super) mod open;
mod paint;
mod pr;
mod prose;
mod pull_request;
mod review_row;
mod row_checks;
mod runs;
mod sections;
mod states;
mod suggestion;
pub(super) mod tutor;
mod views;
mod workflow;
use super::{Field, Form, FormAction};
use crate::editor::input::{KeyCode, KeyEvent};
use crate::forge::legacy::Shape;
use crate::forge::model::ItemKind;
use crate::forge::request::{Filter, Page, Read, RunFilter, StateFilter, Write};
use crate::forge::{ForgeReply, ForgeTarget};
pub(super) use actions::Action;
pub(super) use prose::configure_tables;
pub(super) use pull_request::template_chosen;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
enum Key {
    Repository,
    PullRequestDefaults,
    List(usize, String, String),
    Inbox(String, usize, String, String),
    Item(usize, u64),
    Timeline(u64),
    Threads(u64),
    Viewed(u64),
    Diff(u64),
    Commit(String),
    Workflows,
    Workflow(u64, String),
    Runs(u64),
    Run(u64, u32),
    /// The visible Gitea/Forgejo list rows' combined status (`row_checks`).
    RowChecks(Vec<u64>),
    Log(u64),
    Notifications,
    Artifacts(u64),
    ArtifactFiles(u64),
    Write,
    WorktreeWrite(u64),
}
struct Reply {
    text: String,
    value: Value,
}
/// What a queued request asks the forge worker for.
enum Op {
    Read(ForgeTarget, Read),
    Write(ForgeTarget, Write),
}
struct Request {
    key: Key,
    op: Op,
}
impl Request {
    fn list_limit(&self) -> Option<usize> {
        match &self.op {
            Op::Read(_, Read::List { page, .. } | Read::Search { page, .. }) => {
                Some(page.per_page as usize)
            }
            _ => None,
        }
    }
    #[cfg(test)]
    fn placeholder(key: Key) -> Self {
        Self {
            key,
            op: Op::Read(ForgeTarget::remote(), Read::Repository),
        }
    }
}
/// The gh command output a key's legacy value stands in for (`forge::legacy`).
fn shape(key: &Key) -> Shape {
    let kind = |page: usize| {
        if page == 1 {
            ItemKind::Issue
        } else {
            ItemKind::PullRequest
        }
    };
    match key {
        Key::Repository => Shape::Repository,
        Key::PullRequestDefaults => Shape::RepositoryRest,
        Key::List(page, ..) => Shape::List(kind(*page)),
        Key::Inbox(_, page, ..) => Shape::Search(kind(*page)),
        Key::Item(..) => Shape::Detail,
        Key::Timeline(_) => Shape::Timeline,
        Key::Threads(_) => Shape::Threads,
        Key::Viewed(_) => Shape::Viewed,
        Key::Workflows => Shape::Workflows,
        Key::Runs(_) => Shape::Runs,
        Key::Run(..) => Shape::Run,
        Key::Diff(_)
        | Key::Commit(_)
        | Key::Workflow(..)
        | Key::RowChecks(_)
        | Key::Log(_)
        | Key::Notifications
        | Key::Artifacts(_)
        | Key::ArtifactFiles(_)
        | Key::Write
        | Key::WorktreeWrite(_) => Shape::Text,
    }
}
const LIST_BATCH: usize = 100;
const LIST_CEILING: usize = 1000;
/// The pull request a deep link left from: Enter on a check opens its run in
/// Actions, and Esc there comes back to the same pull request, tab and line.
pub(super) struct ReturnTarget {
    pub page: usize,
    pub number: u64,
    /// The pull request's reader tab (`pr_tab`), Checks when it came from a check.
    pub tab: usize,
    /// `#18`, or `!18` on GitLab: what the footer's `Esc back to …` names.
    pub label: String,
    opened: Option<(usize, u64)>,
    selected: [usize; 2],
    focus: usize,
    last_list: usize,
    reading: bool,
    detail_cursor: usize,
    detail_scroll: usize,
    /// The reader's rows and reading anchors as they were, so the line comes back by its
    /// identity (the check row), not by an index a first paint would clamp to 0.
    rows: Vec<lines::Line>,
    position: lines::ReadingPosition,
}

pub(super) struct Workspace {
    root: PathBuf,
    /// The reads (and the one write) running now (`lanes.rs`).
    active: lanes::Lanes,
    queue: VecDeque<Request>,
    cache: HashMap<Key, Result<Reply, String>>,
    repo: String,
    host: String,
    inbox: inbox::Inbox,
    scope_epoch: u64,
    page: usize,
    selected: [usize; 2],
    opened: Option<(usize, u64)>,
    /// Where Esc goes from a run opened off a pull request's check.
    pub(super) return_to: Option<ReturnTarget>,
    /// While a filter is typed, the first match: the reader follows it.
    live_pick: Option<u64>,
    detail_scroll: usize,
    horizontal: usize,
    queries: [String; 2],
    states: [String; 2],
    active_views: [Option<views::SavedView>; 2],
    list_selections: HashMap<Key, (String, usize)>,
    // Successful CLI depths only; the list/search identity never includes a limit.
    list_limits: HashMap<Key, usize>,
    // What the forge counted for a list (its total, or the other state's in the same read).
    list_totals: HashMap<Key, u64>,
    focus: usize,
    last_list: usize,
    detail_cursor: usize,
    /// Rows the reader's body showed when last painted: `Ctrl-d`/`Ctrl-u` move half of it.
    detail_visible: usize,
    /// `m` from the list: the next paint puts the cursor on the merge box.
    /// `n`/`N` jumped to a review thread: the next frame scrolls it to the top of the reader,
    /// so the thread reads whole (its suggestion too), as GitHub's jump does.
    reveal_top: bool,
    reveal_merge: bool,
    /// A reader just opened reads: no row is highlighted until a movement key or a
    /// control key asks for the cursor.
    reading: bool,
    detail_rows: Vec<lines::Line>,
    detail_find: find::DetailFind,
    reading_position: lines::ReadingPosition,
    detail_width: usize,
    pr_tab: usize,
    /// Issues reader tab: Overview, Discussion, Activity.
    issue_tab: usize,
    /// The Files diff pans long lines sideways instead of wrapping them (`w`, for
    /// this session).
    diff_nowrap: bool,
    /// `toggle_sidebar` (`keymap.rs`): the left column is hidden on the forge screens.
    hide_list: bool,
    /// The shared key table's preset and leader (`keymap.rs`).
    keys: keymap::Keys,
    /// A pressed leader waiting for the rest of its chord (`keymap.rs`).
    leader_pending: bool,
    /// The icon set the last frame painted with; find rebuilds rows with the same glyphs.
    icons: crate::config::ExplorerIconMode,
    selected_commit: Option<String>,
    /// The merge box's chosen method (`Squash`, `Merge`, `Rebase`), until another item.
    merge_method: Option<&'static str>,
    workflow_selected: usize,
    run_selected: usize,
    run_id: Option<u64>,
    attempt: u32,
    /// Actions: runs page, drilldown, logs and artifacts (`runs.rs`).
    actions: runs::View,
    refreshed_at: std::time::Instant,
    collapsed: BTreeSet<(u64, String)>,
    selected_files: BTreeMap<u64, String>,
    /// The Checks tab's selected check, by pull request (`checks.rs`).
    checks_selected: BTreeMap<u64, String>,
    reveal_file: bool,
    drafts: BTreeMap<String, Vec<String>>,
    notice: String,
    notice_error: bool,
    /// The notice lasts until the next key (`Added to your review`, r15 prs-07): the file
    /// header badge and the `Review changes` row keep the count.
    notice_transient: bool,
    /// The write that follows the one in flight once it is saved (a closing comment, then
    /// the close).
    then_write: Option<Write>,
    writing_draft: Option<String>,
    pending_pr: bool,
    /// A deep link (`dusty-git --page prs --open 13`) waiting for the repository to resolve.
    deep_link: Option<(usize, u64)>,
    ready_form: Option<Form>,
    /// Reads a refresh (`R`) found already on their way: asked again once they land, so the
    /// refresh shows what changed after the first one left.
    again: HashMap<Key, (ForgeTarget, Read)>,
    fresh: fresh::Fresh,
    accounts: accounts::Accounts,
    notify: notifications::Inbox,
    /// Inline comments held for the next review of each pull request: sent
    /// with it, as one batch, when the forge batches reviews.
    inline_drafts: BTreeMap<u64, Vec<(crate::forge::request::InlineDraft, String)>>,
    /// Where those drafts are saved, and a range's first line (`drafts.rs`).
    review: drafts::Review,
    /// Create pull request from a branch (`create.rs`).
    compose: create::Compose,
    /// A deep link waiting for the repository (`open.rs`).
    link: open::Opening,
}
impl Workspace {
    pub fn new(root: PathBuf) -> Self {
        let hide_list = views::list_hidden_preference(&root);
        Self {
            root,
            active: lanes::Lanes::default(),
            queue: VecDeque::new(),
            cache: HashMap::new(),
            repo: String::new(),
            host: String::new(),
            inbox: inbox::Inbox::default(),
            scope_epoch: 0,
            page: 0,
            selected: [0; 2],
            opened: None,
            return_to: None,
            live_pick: None,
            detail_scroll: 0,
            horizontal: 0,
            queries: Default::default(),
            states: ["open".into(), "open".into()],
            active_views: Default::default(),
            list_selections: HashMap::new(),
            list_limits: HashMap::new(),
            list_totals: HashMap::new(),
            focus: 0,
            last_list: 0,
            detail_cursor: 0,
            detail_visible: 20,
            reveal_merge: false,
            reveal_top: false,
            reading: false,
            detail_rows: Vec::new(),
            detail_find: find::DetailFind::default(),
            reading_position: lines::ReadingPosition::default(),
            detail_width: 100,
            pr_tab: 0,
            issue_tab: 0,
            diff_nowrap: false,
            hide_list,
            keys: keymap::Keys::default(),
            leader_pending: false,
            icons: crate::config::ExplorerIconMode::Unicode,
            selected_commit: None,
            merge_method: None,
            workflow_selected: 0,
            run_selected: 0,
            run_id: None,
            attempt: 0,
            actions: runs::View::default(),
            refreshed_at: std::time::Instant::now(),
            collapsed: BTreeSet::new(),
            selected_files: BTreeMap::new(),
            checks_selected: BTreeMap::new(),
            reveal_file: false,
            drafts: BTreeMap::new(),
            notice: String::new(),
            notice_error: false,
            notice_transient: false,
            then_write: None,
            writing_draft: None,
            pending_pr: false,
            deep_link: None,
            ready_form: None,
            again: HashMap::new(),
            fresh: fresh::Fresh::new(),
            accounts: accounts::Accounts::default(),
            notify: notifications::Inbox::default(),
            inline_drafts: BTreeMap::new(),
            review: drafts::Review::default(),
            compose: create::Compose::default(),
            link: open::Opening::default(),
        }
    }
    pub fn writing(&self) -> bool {
        self.active
            .requests()
            .any(|request| matches!(request.key, Key::Write | Key::WorktreeWrite(_)))
            || self
                .queue
                .iter()
                .any(|request| matches!(request.key, Key::Write | Key::WorktreeWrite(_)))
    }
    pub fn page(&mut self, page: usize) {
        if !matches!(page, 1 | 2) && self.inbox.enabled {
            // Workflows and local Git always use the original repository.
            let _ = self.toggle_inbox();
        }
        self.remember_list_selection();
        self.drop_ghosts();
        // Changing page by hand ends a deep link's way back.
        self.return_to = None;
        self.notify.back = false;
        self.pending_pr = false;
        self.ready_form = None;
        self.actions.pending_rerun = None;
        self.notice.clear();
        self.notice_error = false;
        self.page = page;
        self.detail_find = find::DetailFind::default();
        if matches!(page, 1 | 2) {
            self.restore_list_selection();
        }
        self.focus = usize::from(page == 3);
        self.last_list = self.focus;
        self.detail_cursor = 0;
        self.detail_scroll = 0;
        self.horizontal = 0;
        self.opened = None;
        self.selected_commit = None;
        self.detail_rows.clear();
        self.reveal_file = false;
        self.ensure(false);
    }
    /// Before a deep link leaves a pull request's reader (Enter on a check opens its run),
    /// keep where it was so Esc can come back; anywhere else there is nothing to return to.
    pub(super) fn remember_return(&mut self) {
        let number = self.number();
        if self.page != 2 || number == 0 {
            self.return_to = None;
            return;
        }
        let label = format!("{}{number}", self.sigil(true));
        self.return_to = Some(ReturnTarget {
            page: self.page,
            number,
            tab: self.pr_tab,
            label,
            opened: self.opened,
            selected: self.selected,
            focus: self.focus,
            last_list: self.last_list,
            reading: self.reading,
            detail_cursor: self.detail_cursor,
            detail_scroll: self.detail_scroll,
            // Opening the run replaces the rows anyway; they come back with Esc.
            rows: std::mem::take(&mut self.detail_rows),
            position: self.reading_position.clone(),
        });
    }

    /// Back to the pull request [`Self::remember_return`] kept: same page, item, tab, line
    /// and focus. False when there is nowhere to go back to.
    pub(super) fn go_back(&mut self) -> bool {
        let Some(target) = self.return_to.take() else {
            return false;
        };
        self.drop_ghosts();
        self.ready_form = None;
        self.actions.pending_rerun = None;
        self.notice.clear();
        self.notice_error = false;
        self.page = target.page;
        self.opened = target.opened.or(Some((target.page, target.number)));
        self.selected = target.selected;
        self.focus = target.focus;
        self.last_list = target.last_list;
        self.reading = target.reading;
        self.pr_tab = target.tab;
        self.detail_find = find::DetailFind::default();
        self.detail_cursor = target.detail_cursor;
        self.detail_scroll = target.detail_scroll;
        self.horizontal = 0;
        self.selected_commit = None;
        // The rows and anchors the pull request was read with: the next paint finds the
        // same line in the rebuilt rows (an empty `detail_rows` would restart at row 0).
        self.detail_rows = target.rows;
        self.reading_position = target.position;
        self.reveal_file = false;
        self.ensure(false);
        true
    }

    /// What this workspace's forge calls things: the kind the worker detected
    /// for the host, else a well-known host's, else GitHub's words.
    /// No forge read is queued or in flight: what tests wait on instead of a fixed pause.
    pub(crate) fn quiet(&self) -> bool {
        self.queue.is_empty() && self.active.idle()
    }

    pub(crate) fn terms(&self) -> &'static crate::forge::capability::Terms {
        crate::forge::capability::terms_for(self.forge_kind())
    }

    /// How this forge names a pull request's CI: (tab, field, the things counted). GitLab
    /// runs a pipeline of jobs; GitHub and Gitea/Forgejo report checks.
    pub(super) fn ci_words(&self) -> (&'static str, &'static str, &'static str) {
        match self.forge_kind() {
            Some(crate::forge::provider::ProviderKind::GitLab) => ("Pipelines", "Pipeline", "jobs"),
            _ => ("Checks", "Checks", "checks"),
        }
    }

    /// A requested review in this forge's words, (long, short): GitLab shows a reviewer
    /// as awaiting review; GitHub and Gitea/Forgejo request one.
    /// One word for a pending review request on every forge: the state is the
    /// same thing whether GitLab calls the person a reviewer or GitHub a requested one.
    pub(super) fn requested_words(&self) -> (&'static str, &'static str) {
        ("Review requested", "Requested")
    }

    /// The number's sigil: GitLab's merge requests are `!13`, everything else `#13`.
    pub(super) fn sigil(&self, pull: bool) -> &'static str {
        if pull { self.terms().pr_glyph } else { "#" }
    }

    pub(super) fn forge_kind(&self) -> Option<crate::forge::provider::ProviderKind> {
        let host = crate::forge::transport::HostKey::new(&self.host);
        crate::forge::host_state::account(&host)
            .and_then(|account| account.kind)
            .or_else(|| crate::forge::detect::known_host(&host))
    }

    /// The forge's display name (`GitHub`, `GitLab`, `Gitea`, `Forgejo`) for words in forms.
    pub(super) fn forge_name(&self) -> &'static str {
        self.forge_kind()
            .unwrap_or(crate::forge::provider::ProviderKind::GitHub)
            .name()
    }

    fn change_repository(&mut self, repo: String, host: String) {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if self.repo.eq_ignore_ascii_case(&repo) && self.host == host {
            return;
        }
        self.scope_epoch = self
            .scope_epoch
            .checked_add(1)
            .expect("GitHub scope generation exhausted");
        self.again.clear();
        self.inbox = inbox::Inbox::default();
        self.repo = repo;
        self.host = host;
        self.detail_find = find::DetailFind::default();
        self.notice.clear();
        self.notice_error = false;
        self.cache.clear();
        self.fresh.forget();
        self.queue
            .retain(|request| matches!(request.key, Key::Write | Key::WorktreeWrite(_)));
        self.queries = Default::default();
        self.states = ["open".into(), "open".into()];
        self.active_views = Default::default();
        self.list_selections.clear();
        self.list_limits.clear();
        self.selected = [0; 2];
        self.opened = None;
        self.selected_commit = None;
        self.detail_rows.clear();
        self.detail_cursor = 0;
        self.detail_scroll = 0;
        self.horizontal = 0;
        self.selected_files.clear();
        self.pr_tab = 0;
        self.reveal_file = false;
        self.workflow_selected = 0;
        self.run_selected = 0;
        self.run_id = None;
        self.attempt = 0;
        self.actions = runs::View::default();
        self.collapsed.clear();
        self.ready_form = None;
        self.pending_pr = false;
        if let Err(error) = views::load(&self.root) {
            self.notice = error;
            self.notice_error = true;
        }
        if let Some((page, id)) = self.deep_link.take() {
            self.open_link(page, id);
        }
    }

    /// Opens item `id` on page 1 (issues) or 2 (pull requests) as Enter on its row would,
    /// once the repository is known; until then the link waits.
    pub(in crate::git::screen) fn open_link(&mut self, page: usize, id: u64) {
        if !matches!(page, 1 | 2) || id == 0 {
            return;
        }
        if self.repo.is_empty() {
            self.deep_link = Some((page, id));
            return;
        }
        if let Err(error) = self.activate(Action::OpenItem(page, id)) {
            self.notice = error;
            self.notice_error = true;
        }
    }
    fn pending(&self, key: &Key) -> bool {
        self.active.contains(key) || self.queue.iter().any(|request| &request.key == key)
    }
    fn target(&self) -> ForgeTarget {
        ForgeTarget::repository(&self.host, &self.repo)
    }
    fn read(&self, read: Read) -> Op {
        Op::Read(self.target(), read)
    }
    fn enqueue(&mut self, key: Key, op: Op, refresh: bool) {
        // A memory miss is filled from disk and painted at once; stale entries are
        // revalidated in the background while they stay on screen. The disk is read by a
        // worker: a key or a poll waits for it at most `DISK_BUDGET` in all, and a read that
        // takes longer (or one for a row the cursor is passing over) paints when it lands.
        if !refresh {
            // Still on its way: `ensure` runs again when it lands (`poll_disk`).
            let answer = if self.fresh.holding {
                self.load_disk_later(&key, &op)
            } else {
                self.load_disk_soon(&key, &op)
            };
            if answer.is_none() {
                return;
            }
        }
        // A row the cursor is passing over: its details come from the cache only.
        if self.fresh.holding
            && matches!(
                key,
                Key::Item(..)
                    | Key::Timeline(_)
                    | Key::Threads(_)
                    | Key::Diff(_)
                    | Key::Viewed(_)
                    | Key::Commit(_)
            )
        {
            return;
        }
        if self.cache.contains_key(&key) && !refresh {
            if self.needs_check(&key) {
                self.revalidate(key, op);
            }
            return;
        }
        self.fresh.implicit.remove(&key);
        // Already on its way, it may answer with what was there before the refresh.
        if refresh
            && let Op::Read(target, read) = &op
            && (self.active.contains(&key) || self.fresh.warm_pending(&key))
        {
            self.again
                .insert(key.clone(), (target.clone(), read.clone()));
        }
        self.enqueue_refresh(key, op);
    }
    /// The read a refresh joined has landed: ask for it once more (`again`).
    fn ask_again(&mut self, key: &Key) {
        if let Some((target, read)) = self.again.remove(key) {
            self.enqueue_refresh(key.clone(), Op::Read(target, read));
        }
    }
    fn enqueue_refresh(&mut self, key: Key, op: Op) {
        // The warm worker already asks for it: one request, two consumers.
        if self.fresh.warm_pending(&key) {
            return;
        }
        self.fresh.drop_warm(&key);
        if self.pending(&key) {
            return;
        }
        self.queue.push_back(Request { key, op });
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
    fn list_key(&self) -> Key {
        let index = self.page.saturating_sub(1).min(1);
        if self.inbox.enabled {
            return Key::Inbox(
                self.host.clone(),
                self.page,
                self.states[index].clone(),
                self.queries[index].clone(),
            );
        }
        Key::List(
            self.page,
            self.states[index].clone(),
            self.queries[index].clone(),
        )
    }
    fn list_limit(&self) -> usize {
        self.list_limits
            .get(&self.list_key())
            .copied()
            .unwrap_or(LIST_BATCH)
    }
    fn list_may_have_more(&self) -> bool {
        matches!(self.page, 1 | 2)
            && self.list_limits.get(&self.list_key()).is_some_and(|limit| {
                self.value(&self.list_key())
                    .and_then(Value::as_array)
                    .is_some_and(|items| items.len() >= *limit)
            })
    }
    fn loading_more(&self) -> bool {
        let key = self.list_key();
        self.active
            .requests()
            .chain(self.queue.iter())
            .any(|request| {
                request.key == key
                    && request
                        .list_limit()
                        .is_some_and(|limit| limit > self.list_limit())
            })
    }
    fn list_read(&self, limit: usize) -> Op {
        let at = self.page - 1;
        self.list_read_for(&self.states[at], &self.queries[at], limit)
    }
    /// The read behind one list (the shown one, or a section counted in the background).
    fn list_read_for(&self, state: &str, query: &str, limit: usize) -> Op {
        let kind = if self.page == 1 {
            ItemKind::Issue
        } else {
            ItemKind::PullRequest
        };
        let filter = Filter {
            state: StateFilter::parse(state),
            query: query.to_owned(),
        };
        let page = Page::first(limit.min(LIST_CEILING) as u32);
        if self.inbox.enabled {
            return Op::Read(
                ForgeTarget::host(&self.host),
                Read::Search { kind, filter, page },
            );
        }
        self.read(Read::List { kind, filter, page })
    }
    fn load_more(&mut self) {
        let key = self.list_key();
        if self.pending(&key) || !self.list_may_have_more() {
            return;
        }
        if self.list_limit() >= LIST_CEILING {
            self.notice = "1000-result limit · / refine filter; more may exist".into();
            self.notice_error = false;
            return;
        }
        self.notice.clear();
        self.notice_error = false;
        self.enqueue_refresh(
            key,
            self.list_read((self.list_limit() + LIST_BATCH).min(LIST_CEILING)),
        );
        self.reconcile_reads();
    }
    fn items(&self) -> Vec<&Value> {
        self.value(&self.list_key())
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
        if let Some(number) = self.live_pick.filter(|_| matches!(self.page, 1 | 2)) {
            return number;
        }
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
    // A list description is readable prose, not loaded metadata. Keep it in the
    // current query/state/page cache, scoped to this workspace's host/repository.
    fn list_item(&self) -> Option<&Value> {
        if !matches!(self.page, 1 | 2) {
            return None;
        }
        let id = self.number();
        self.value(&self.list_key())?
            .as_array()?
            .iter()
            .find(|item| {
                number(item, "number") == id
                    && (!self.inbox.enabled
                        || inbox::Target::from_row(item, &self.host, self.page)
                            .is_ok_and(|target| target.repo == self.repo))
            })
    }
    fn file_viewed(&self, id: u64, path: &str) -> Option<bool> {
        let value = self.value(&Key::Viewed(id))?;
        let pages = value
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_else(|| std::slice::from_ref(value));
        pages
            .iter()
            .find_map(|page| {
                array(&page["data"]["repository"]["pullRequest"]["files"], "nodes")
                    .iter()
                    .find(|file| string(file, "path") == path)
                    .map(|file| string(file, "viewerViewedState") == "VIEWED")
            })
            // Gitea, Forgejo and GitLab keep viewed files on this computer and list only the
            // viewed ones: a file not listed is not viewed yet.
            .or_else(|| (!self.read_only()).then_some(false))
    }
    fn review_threads(&self, id: u64) -> Vec<&Value> {
        let Some(value) = self.value(&Key::Threads(id)) else {
            return Vec::new();
        };
        let pages = value
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_else(|| std::slice::from_ref(value));
        pages
            .iter()
            .flat_map(|page| {
                array(
                    &page["data"]["repository"]["pullRequest"]["reviewThreads"],
                    "nodes",
                )
            })
            .collect()
    }
    fn selected_file<'a>(&'a self, id: u64, item: &'a Value) -> &'a str {
        self.selected_files
            .get(&id)
            .map(String::as_str)
            .filter(|path| {
                array(item, "files")
                    .iter()
                    .any(|file| string(file, "path") == *path)
            })
            .unwrap_or_else(|| {
                array(item, "files")
                    .first()
                    .map(|file| string(file, "path"))
                    .unwrap_or("")
            })
    }
    // Reconcile only pending reads. The active request may finish into its keyed
    // cache, and navigation must never cancel or discard a confirmed write.
    /// Whether the screen still shows what a read is for.
    fn wanted(&self, key: &Key) -> bool {
        match key {
            Key::Write | Key::WorktreeWrite(_) | Key::Repository | Key::PullRequestDefaults => true,
            // The unread badge is read behind every forge page.
            Key::Notifications => self.page != 0,
            Key::List(..) | Key::Inbox(..) => {
                matches!(self.page, 1 | 2)
                    && (*key == self.list_key()
                        || self.fresh.live_key.as_ref() == Some(key)
                        || self.section_keys().contains(key))
            }
            Key::Item(page, id) => *page == self.page && *id == self.number(),
            Key::Timeline(id) => {
                *id == self.number()
                    && (self.page == 1 || self.page == 2 && matches!(self.pr_tab, 0 | 4))
            }
            Key::Threads(id) => {
                self.page == 2 && *id == self.number() && matches!(self.pr_tab, 0 | 3 | 4 | 5)
            }
            Key::Viewed(id) | Key::Diff(id) => {
                self.page == 2 && self.pr_tab == 3 && *id == self.number()
            }
            Key::Commit(sha) => {
                self.page == 2 && self.pr_tab == 1 && self.selected_commit.as_ref() == Some(sha)
            }
            Key::Workflows => self.page == 3,
            // Notifications reads the newest runs for its check-suite rows.
            Key::Runs(id) => {
                self.page == 3 && *id == self.workflow_id() || *id == 0 && self.notify_wants_runs()
            }
            Key::Run(id, attempt) => {
                self.page == 3
                    && Some(*id) == self.run_id
                    && (*attempt == self.attempt || self.wanted_attempts().contains(attempt))
                    || self.check_wants(key)
            }
            Key::Workflow(id, revision) => {
                self.page == 3
                    && revision.is_empty()
                    && (*id == self.workflow_id() || self.pending_dispatch() == Some(*id))
            }
            Key::Log(id) => self.wanted_logs().contains(id) || self.check_wants(key),
            Key::Artifacts(id) => self.wanted_artifacts() == Some(*id),
            Key::ArtifactFiles(id) => self.wanted_artifact_files() == Some(*id),
            // The warm worker's; never queued here.
            Key::RowChecks(_) => false,
        }
    }
    fn reconcile_reads(&mut self) {
        let mut queue = std::mem::take(&mut self.queue);
        let dropped = queue
            .iter()
            .filter(|request| !self.wanted(&request.key))
            .map(|request| request.key.clone())
            .collect::<Vec<_>>();
        for key in dropped {
            // A dropped background check is asked again when the screen comes back to it.
            if self.fresh.implicit.remove(&key)
                && let Some(meta) = self.fresh.meta.get_mut(&key)
            {
                meta.checked = None;
            }
        }
        queue.retain(|request| self.wanted(&request.key));
        // A detail read for a row the cursor has left is abandoned: its lane takes the
        // next read as soon as its worker stops instead of after a stale answer.
        let abandoned = self
            .active
            .requests()
            .filter(|request| {
                matches!(
                    request.key,
                    Key::Item(..)
                        | Key::Timeline(_)
                        | Key::Threads(_)
                        | Key::Diff(_)
                        | Key::Viewed(_)
                        | Key::Commit(_)
                ) && !self.wanted(&request.key)
            })
            .map(|request| request.key.clone())
            .collect::<Vec<_>>();
        for key in abandoned {
            self.active.cancel(&key);
            if self.fresh.implicit.remove(&key)
                && let Some(meta) = self.fresh.meta.get_mut(&key)
            {
                meta.checked = None;
            }
        }
        // Stable ordering keeps writes safe and lets the visible body/source win
        // over list refreshes, activity, threads and viewed-state annotations.
        // A read somebody waits for goes ahead of every cached entry's check.
        queue.make_contiguous().sort_by_key(|request| {
            let rank = match request.key {
                Key::Write | Key::WorktreeWrite(_) | Key::Repository => 0,
                _ if self.fresh.live_key.as_ref() == Some(&request.key) => 0,
                Key::PullRequestDefaults => 1,
                Key::Item(..) | Key::Diff(_) | Key::Commit(_) | Key::Run(..) | Key::Log(_) => 2,
                Key::Workflow(_, _) if self.run_id.is_none() => 2,
                Key::List(..) | Key::Inbox(..) if request.key != self.list_key() => 5,
                Key::List(..) | Key::Inbox(..) | Key::Workflows | Key::Runs(_) => 3,
                Key::Notifications if self.page != notifications::PAGE => 6,
                _ => 4,
            };
            (self.fresh.implicit.contains(&request.key), rank)
        });
        self.queue = queue;
    }
    fn ensure(&mut self, refresh: bool) {
        // While `j`/`k` move through the list the reader paints what the cache holds; the
        // forge is asked once the cursor dwells on a row.
        self.fresh.holding = !refresh && self.cursor_moving();
        self.ensure_reads(refresh);
        self.fresh.holding = false;
        self.reconcile_reads();
    }
    fn ensure_reads(&mut self, refresh: bool) {
        self.sync_inbox_repository();
        if self.page == 0 {
            return;
        }
        // A cached repository still goes through `enqueue`, which checks it once it is stale: a
        // disk hit that landed after a poll's budget fills the key without its check being sent.
        let cached = !self.repo.is_empty() && self.cache.contains_key(&Key::Repository);
        let target = if self.repo.is_empty() {
            ForgeTarget::remote()
        } else {
            self.target()
        };
        self.enqueue(
            Key::Repository,
            Op::Read(target, Read::Repository),
            refresh && !cached,
        );
        if self.repo.is_empty() || !self.cache.contains_key(&Key::Repository) {
            return;
        }
        self.badge_read();
        self.actions_badge_read();
        match self.page {
            notifications::PAGE => self.notification_reads(refresh),
            1 | 2 => {
                self.enqueue(self.list_key(), self.list_read(self.list_limit()), refresh);
                // Every inbox section tab shows a count: the ones the shown list
                // cannot answer locally (`@me`) are read behind it. In a repository the
                // budget is one list read per visit, so a section's count comes from its
                // own cache once it has been opened (and from the open list otherwise).
                if self.inbox.enabled && self.value(&self.list_key()).is_some() {
                    for (key, op) in self.section_reads() {
                        self.enqueue(key, op, false);
                    }
                }
                let id = self.number();
                if id == 0
                    || self.inbox.enabled && self.opened.is_none() && self.list_item().is_none()
                {
                    return;
                }
                let item = if self.page == 1 {
                    Read::Issue(id)
                } else {
                    Read::PullRequest(id)
                };
                self.enqueue(Key::Item(self.page, id), self.read(item), refresh);
                if self.page == 1 || matches!(self.pr_tab, 0 | 4) {
                    self.enqueue(Key::Timeline(id), self.read(Read::Timeline(id)), refresh);
                }
                if self.page == 2 {
                    if self.pr_tab == 1
                        && let Some(sha) = self.selected_commit.clone()
                    {
                        let read = self.read(Read::CommitDiff(sha.clone()));
                        self.enqueue(Key::Commit(sha), read, refresh);
                    }
                    if self.pr_tab == 3 {
                        self.enqueue(Key::Diff(id), self.read(Read::Diff(id)), refresh);
                        self.enqueue(Key::Viewed(id), self.read(Read::Viewed(id)), refresh);
                    }
                    if self.pr_tab == pr::CHECKS {
                        self.check_reads(refresh);
                    }
                    if !matches!(self.pr_tab, 0 | 3 | 4 | 5) {
                        return;
                    }
                    self.enqueue(Key::Threads(id), self.read(Read::Threads(id)), refresh);
                }
            }
            3 => {
                self.enqueue(Key::Workflows, self.read(Read::Workflows), refresh);
                let workflow = self.workflow_id();
                // A workflow's definition is read when `d` asks for it, not each time the
                // Workflows panel passes over it.
                if let Some(source) = self.pending_dispatch() {
                    self.enqueue(
                        Key::Workflow(source, String::new()),
                        self.read(Read::WorkflowSource {
                            id: source.into(),
                            reference: None,
                        }),
                        refresh,
                    );
                }
                self.enqueue(Key::Runs(workflow), self.runs_read(workflow), refresh);
                if let Some(id) = self.run_id {
                    self.enqueue(Key::Run(id, self.attempt), self.run_read(id), refresh);
                    for attempt in self.wanted_attempts() {
                        let read = self.read(Read::Run {
                            id: id.into(),
                            attempt: Some(attempt),
                        });
                        self.enqueue(Key::Run(id, attempt), read, refresh);
                    }
                    for job in self.wanted_logs() {
                        self.enqueue(Key::Log(job), self.read(Read::JobLog(job.into())), refresh);
                    }
                    if let Some(run) = self.wanted_artifacts() {
                        self.enqueue(
                            Key::Artifacts(run),
                            self.read(Read::Artifacts(run.into())),
                            refresh,
                        );
                    }
                    if let Some(artifact) = self.wanted_artifact_files() {
                        self.enqueue(
                            Key::ArtifactFiles(artifact),
                            self.read(Read::ArtifactFiles(artifact.into())),
                            refresh,
                        );
                    }
                }
            }
            _ => {}
        }
    }
    fn runs_read(&self, workflow: u64) -> Op {
        self.read(Read::Runs {
            filter: RunFilter {
                workflow: (workflow != 0).then(|| workflow.into()),
                ..RunFilter::default()
            },
            page: Page::first(100),
        })
    }
    fn run_read(&self, id: u64) -> Op {
        self.read(Read::Run {
            id: id.into(),
            attempt: (self.attempt > 0).then_some(self.attempt),
        })
    }
    pub fn poll(&mut self) -> bool {
        self.fresh.disk_until = None;
        // Actions re-reads running work every 8 s; `DUSTY_FORGE_ACTIONS_POLL_SECS` stretches
        // it (a read-only smoke against a real forge sets an hour).
        static POLL: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
        let every = *POLL.get_or_init(|| {
            std::env::var("DUSTY_FORGE_ACTIONS_POLL_SECS")
                .ok()
                .and_then(|seconds| seconds.parse().ok())
                .map_or(
                    std::time::Duration::from_secs(8),
                    std::time::Duration::from_secs,
                )
        });
        if self.page == 3
            && self.refreshed_at.elapsed() >= every
            && self.active.idle()
            && self.queue.is_empty()
        {
            self.refreshed_at = std::time::Instant::now();
            if let Some(id) = self.run_id
                && self
                    .value(&Key::Run(id, self.attempt))
                    .is_some_and(|run| string(run, "status") != "completed")
            {
                self.enqueue_refresh(Key::Run(id, self.attempt), self.run_read(id));
            }
            for job in self.live_logs() {
                self.enqueue_refresh(Key::Log(job), self.read(Read::JobLog(job.into())));
            }
            let workflow = self.workflow_id();
            self.enqueue_refresh(Key::Runs(workflow), self.runs_read(workflow));
        }
        let mut changed = false;
        for (running, reply) in self.active.replies() {
            let request = running.request;
            if !self.accept_scope(&request.key, &running.scope) {
                changed = true;
                self.ensure(false);
                continue;
            }
            let refresh = matches!(request.key, Key::Write | Key::WorktreeWrite(_));
            match reply {
                ForgeReply::Written(write, result) => {
                    changed = true;
                    self.accounts.observe(&self.host, result.as_ref().err());
                    self.written(&write, result.map_err(|error| error.to_string()));
                    self.ensure(refresh);
                }
                ForgeReply::Fetched(_, result) => {
                    changed = true;
                    self.accounts.observe(&self.host, result.as_ref().err());
                    let key = request.key.clone();
                    self.apply(request, result.map_err(|error| error.to_string()), false);
                    self.ask_again(&key);
                }
                _ => {}
            }
        }
        self.reconcile_reads();
        changed |= self.start_reads();
        changed |= self.poll_fresh();
        self.poll_pr_form();
        changed |= self.poll_features();
        if self.page == 3 {
            self.poll_actions();
        }
        changed
    }
    /// Start what the queue holds on the free lanes, in its order.
    ///
    /// - A write runs alone: it waits for every lane to empty, and nothing behind it starts
    ///   until it is answered, so writes never overlap each other or a read of what they change.
    /// - A background check (`fresh.implicit`) never takes the last free lane: the read the user
    ///   is waiting for always finds one.
    fn start_reads(&mut self) -> bool {
        let mut changed = false;
        let mut at = 0;
        while at < self.queue.len() && !self.active.writing() {
            let free = self.active.free();
            if free == 0 {
                break;
            }
            let request = &self.queue[at];
            if matches!(request.op, Op::Write(..)) {
                if !self.active.idle() || free < self.active.count() {
                    break;
                }
            } else if self.fresh.implicit.contains(&request.key) && free <= self.active.reserve() {
                at += 1;
                continue;
            }
            let request = self.queue.remove(at).expect("in range");
            let conditional = match &request.op {
                Op::Read(_, read) => self.conditional(&request.key, read),
                Op::Write(..) => Default::default(),
            };
            let scope = (self.host.clone(), self.repo.clone(), self.scope_epoch);
            let background = self.fresh.implicit.contains(&request.key);
            // The tab says what the numbered item is; the timeline read is told, not left to
            // guess from whatever was read last under that number (GitLab).
            let kind = match (&request.key, self.page) {
                (Key::Timeline(_), 1) => Some(ItemKind::Issue),
                (Key::Timeline(_), 2) => Some(ItemKind::PullRequest),
                _ => None,
            };
            if let Err((request, error)) = self.active.start(
                self.root.clone(),
                request,
                conditional,
                scope,
                background,
                kind,
            ) {
                if self.cache.get(&request.key).is_some_and(Result::is_ok) {
                    let age = self
                        .fresh
                        .meta
                        .get(&request.key)
                        .and_then(|meta| meta.fetched.elapsed().ok());
                    self.notice = self.stale_notice(&error, age);
                } else {
                    self.notice = self.explain(&error).line();
                    self.cache.insert(request.key, Err(error));
                }
                self.notice_error = true;
            }
            changed = true;
        }
        changed
    }
    /// A confirmed write finished: notice, draft and queue bookkeeping.
    fn written(&mut self, write: &Write, result: Result<crate::forge::request::Receipt, String>) {
        match result {
            Ok(receipt) => {
                self.actions_written(write, &receipt.summary);
                self.settle(true);
                self.notice_error = false;
                if let Write::CreatePullRequest(pr) = write {
                    self.change_repository(pr.base_repository.clone(), pr.host.clone());
                    self.cache.clear();
                    if let Some(number) = receipt.created_number {
                        self.opened = Some((2, number));
                        self.focus = 2;
                        self.detail_cursor = 0;
                    }
                    self.notice = receipt.web_url.unwrap_or_default();
                } else {
                    self.notice = if matches!(write, Write::OpenInBrowser { .. }) {
                        "Opened in browser".into()
                    } else if let Some(receipt) = self.notification_written(write) {
                        receipt
                    } else if matches!(write, Write::DownloadArtifact { .. } | Write::CancelRun(_))
                    {
                        // Nothing was saved on the forge: the run header says it (the
                        // receipt, `Cancellation requested`).
                        String::new()
                    } else if let Write::SubmitReview { number, drafts, .. } = write
                        && !drafts.is_empty()
                    {
                        self.inline_drafts.remove(number);
                        format!(
                            "Review sent with {} inline comment{}",
                            drafts.len(),
                            if drafts.len() == 1 { "" } else { "s" }
                        )
                    } else {
                        let kind = self.forge_kind();
                        format!(
                            "Saved on {}",
                            kind.unwrap_or(crate::forge::provider::ProviderKind::GitHub)
                                .name()
                        )
                    };
                }
                if let Some(key) = self.writing_draft.take() {
                    self.drafts.remove(&key);
                }
                self.queue.retain(|request| {
                    matches!(
                        request.key,
                        Key::Write
                            | Key::WorktreeWrite(_)
                            | Key::Repository
                            | Key::PullRequestDefaults
                    )
                });
                if let Some(next) = self.then_write.take() {
                    self.cache.remove(&Key::Write);
                    self.optimistic(&next);
                    self.enqueue(Key::Write, Op::Write(self.target(), next), false);
                    self.notice = format!("Sending to {}…", self.forge_name());
                }
            }
            Err(error) => {
                self.then_write = None;
                self.settle(false);
                self.notice_error = true;
                let words = self.explain(&error.to_string());
                self.notice = format!("{} · your draft is kept", words.headline);
            }
        }
    }
    fn draft_key(&self, action: &Action) -> String {
        if let Action::Scoped {
            host, repo, action, ..
        } = action
        {
            return format!("{host}/{repo}/0/{action:?}");
        }
        let page = if matches!(
            action,
            Action::Filter | Action::Views | Action::SaveView(..) | Action::DeleteView(_)
        ) {
            self.page
        } else {
            0
        };
        let (host, repo) = match action {
            Action::Reply { host, repo, .. } => (host.as_str(), repo.as_str()),
            _ if self.inbox.enabled
                && matches!(
                    action,
                    Action::Filter | Action::Views | Action::SaveView(..) | Action::DeleteView(_)
                ) =>
            {
                (self.host.as_str(), "*")
            }
            _ => (self.host.as_str(), self.repo.as_str()),
        };
        let query = if matches!(action, Action::SaveView(..)) {
            format!("/{:?}", self.list_key())
        } else {
            String::new()
        };
        format!("{host}/{repo}/{page}/{action:?}{query}")
    }
    pub fn save_draft(&mut self, action: &Action, values: Vec<String>) {
        if !matches!(
            action,
            Action::ConfirmPullRequest(_) | Action::Views | Action::Facets | Action::DeleteView(_)
        ) {
            self.drafts.insert(self.draft_key(action), values);
        }
    }
    fn form(&self, title: &str, action: Action, mut fields: Vec<Field>) -> Form {
        if let Some(draft) = self.drafts.get(&self.draft_key(&action)) {
            for (field, value) in fields.iter_mut().zip(draft) {
                if !field.read_only {
                    field.input.input = value.clone();
                    field.input.cursor = value.len();
                }
            }
        }
        let context = if self.inbox.enabled {
            if action.item_scoped() {
                format!(
                    "{}/{} · {} #{}",
                    self.host,
                    self.repo,
                    if self.page == 1 { "Issue" } else { "PR" },
                    self.number()
                )
            } else {
                format!("{} · inbox", self.host)
            }
        } else if matches!(
            action,
            Action::CreatePullRequest | Action::ConfirmPullRequest(_)
        ) {
            self.host.clone()
        } else if matches!(
            action,
            Action::Cancel(_) | Action::Rerun(_) | Action::Approve(_) | Action::Dispatch(_)
        ) {
            // A run sheet names the run as its header does; the repository is above it.
            String::new()
        } else {
            self.repo.clone()
        };
        let action = if self.inbox.enabled && action.item_scoped() {
            Action::Scoped {
                host: self.host.clone(),
                repo: self.repo.clone(),
                page: self.page,
                number: self.number(),
                action: Box::new(action),
            }
        } else {
            action
        };
        Form {
            title: if context.is_empty() {
                title.to_owned()
            } else {
                format!("{title} · {context}")
            },
            action: FormAction::Github(action),
            fields,
            selected: 0,
        }
    }
    pub fn event(
        &mut self,
        key: &KeyEvent,
        write_refusal: Option<&str>,
    ) -> Result<Option<Form>, String> {
        self.fresh.disk_until = None;
        if std::mem::take(&mut self.notice_transient) {
            self.notice.clear();
        }
        if key.code == KeyCode::Escape && self.pending_pr {
            self.pending_pr = false;
            self.notice.clear();
            return Ok(None);
        }
        // In a run's drilldown (or typing a find) `a` is attempts, not accounts.
        let actions_first = !self.accounts.open
            && (self.actions_typing()
                || self.page == 3 && (self.actions_drilled() || self.leader_pending));
        if !actions_first && let Some(handled) = self.accounts_key(key) {
            return handled;
        }
        if self.page == notifications::PAGE {
            return self.notification_key(key);
        }
        if self.page == 3
            && let Some(handled) = self.actions_key(key, write_refusal)
        {
            return handled;
        }
        if actions_first && let Some(handled) = self.accounts_key(key) {
            return handled;
        }
        // The shared key table (`keymap.rs`): `Space e` hides the list beside the pull
        // request and issue readers (the reader takes the width) and brings it back.
        if matches!(self.page, 1 | 2) {
            match self.chord_key(key) {
                keymap::Step::Wait | keymap::Step::Cancel => return Ok(None),
                keymap::Step::Run(keymap::Named::ToggleSidebar) => {
                    self.toggle_sidebar();
                    if self.sidebar_hidden() && self.focus != 2 {
                        self.last_list = self.focus;
                        self.focus = 2;
                    }
                    return Ok(None);
                }
                // lazygit's Space: what Enter does here.
                keymap::Step::Run(keymap::Named::Toggle) => {
                    return self.event(&KeyEvent::plain(KeyCode::Enter), write_refusal);
                }
                keymap::Step::Pass => {}
            }
        }
        if self.focus == 2 && self.reading && !key.modifiers.control {
            match key.code {
                // The first step shows the cursor where reading left it.
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j' | 'k') => {
                    self.reading = false;
                    return Ok(None);
                }
                // Nothing is selected, so there is nothing to activate yet.
                KeyCode::Enter => return Ok(None),
                KeyCode::Tab
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown => {
                    self.reading = false;
                }
                _ => {}
            }
        }
        if self.focus == 2 && key.modifiers.control && key.code == KeyCode::Char('l') {
            self.reading = false;
        }
        if let Some(form) = self.detail_find_key(key) {
            return Ok(Some(form));
        }
        if self.step_or_clear_detail_find(key) {
            return Ok(None);
        }
        if self.focus == 2
            && !key.modifiers.control
            && matches!(
                key.code,
                KeyCode::Home
                    | KeyCode::End
                    | KeyCode::PageDown
                    | KeyCode::PageUp
                    | KeyCode::Tab
                    | KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Char('h' | 'j' | 'k' | 'l')
            )
        {
            // Moving leaves the find match: its highlight and caret.
            self.detail_find.leave_match();
        }
        if let Some(result) = self.conversation_key(key) {
            return result;
        }
        if key.modifiers.control {
            match key.code {
                KeyCode::Char('h') => {
                    self.focus = self.last_list;
                    self.return_inbox_list();
                }
                KeyCode::Char('l') => self.focus = 2,
                KeyCode::Char('n') if !key.modifiers.alt && matches!(self.page, 1 | 2) => {
                    self.load_more();
                }
                KeyCode::Char('j') if self.page == 3 && self.focus < 2 => self.focus = 1,
                KeyCode::Char('k') if self.page == 3 && self.focus < 2 => self.focus = 0,
                _ => {}
            }
        } else {
            if matches!(self.page, 1 | 2)
                && !key.modifiers.alt
                && let Some(result) = self.forge_key(key, write_refusal)
            {
                return result;
            }
            match key.code {
                KeyCode::Char('q') => return Err("close".into()),
                // A reader opened from Notifications goes back there.
                KeyCode::Escape if self.focus == 2 && self.notify.back => {
                    self.back_to_notifications()
                }
                KeyCode::Escape if self.focus == 2 => {
                    self.focus = self.last_list;
                    self.return_inbox_list();
                }
                KeyCode::Char('i') if matches!(self.page, 1 | 2) => self.toggle_inbox()?,
                KeyCode::Char('o') if matches!(self.page, 1 | 2) => {
                    return self.activate(Action::Browser(self.page, self.number()));
                }
                KeyCode::Char('r') => {
                    if self.writing() {
                        return Err("Wait for the confirmed operation before refreshing".into());
                    }
                    self.notice.clear();
                    self.notice_error = false;
                    self.ensure(true);
                }
                KeyCode::Char('/') => return self.activate(Action::Filter),
                KeyCode::Char('v') if self.page < 3 => return self.activate(Action::Views),
                // GitHub's list header: the Open/Closed toggle and the filter menus.
                KeyCode::Char('s') if self.page < 3 && self.focus != 2 => self.toggle_state()?,
                KeyCode::Char('F') if self.page < 3 => return self.activate(Action::Facets),
                KeyCode::Char('S') if self.page < 3 => return self.save_view_form().map(Some),
                KeyCode::Char('D') if self.page < 3 => return self.delete_view_form().map(Some),
                KeyCode::Char(',' | '.') if self.page < 3 => {
                    self.cycle_view(key.code == KeyCode::Char('.'))?;
                }
                KeyCode::Char('n') if self.page == 1 => return self.activate(Action::CreateIssue),
                KeyCode::Char('n') if self.page == 2 => {
                    return self.activate(Action::CreatePullRequest);
                }
                KeyCode::Home if self.focus == 2 => self.detail_cursor = 0,
                KeyCode::End if self.focus == 2 => {
                    self.detail_cursor = self.detail_rows.len().saturating_sub(1)
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
                    let action = if self.focus == 2 {
                        self.detail_rows
                            .get(self.detail_cursor)
                            .and_then(|row| {
                                row.reply.as_ref().or_else(|| {
                                    row.action
                                        .as_ref()
                                        .filter(|action| matches!(action, Action::Inline { .. }))
                                })
                            })
                            .cloned()
                    } else {
                        None
                    };
                    return self
                        .activate(action.unwrap_or(Action::Comment(self.page, self.number())));
                }
                KeyCode::Char('f') if self.page == 2 && self.pr_tab == 3 => {
                    return self.activate(Action::Files(self.number()));
                }
                KeyCode::Char('e') if self.page < 3 => {
                    return self.activate(Action::Edit(self.page, self.number()));
                }
                KeyCode::Char('H' | 'L') if self.page == 2 => {
                    let tab =
                        (self.pr_tab + if key.code == KeyCode::Char('L') { 1 } else { 3 }) % 4;
                    return self.activate(Action::PrTab(tab));
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
                KeyCode::Char('j')
                | KeyCode::Down
                | KeyCode::Char('k')
                | KeyCode::Up
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageDown
                | KeyCode::PageUp => {
                    let delta = match key.code {
                        KeyCode::Home => isize::MIN,
                        KeyCode::End => isize::MAX,
                        KeyCode::PageDown => 20,
                        KeyCode::PageUp => -20,
                        KeyCode::Char('j') | KeyCode::Down => 1,
                        _ => -1,
                    };
                    if self.focus == 2 {
                        // A wrapped diff line is one stop: `j` steps past its rows.
                        let from = if delta > 0 {
                            files::line_end(&self.detail_rows, self.detail_cursor)
                        } else {
                            self.detail_cursor
                        };
                        let to = from
                            .saturating_add_signed(delta)
                            .min(self.detail_rows.len().saturating_sub(1));
                        self.detail_cursor = files::line_start(&self.detail_rows, to);
                    } else if self.page < 3 {
                        self.opened = None;
                        self.selected_commit = None;
                        let max = self.items().len().saturating_sub(1);
                        let at = self.page - 1;
                        self.selected[at] = self.selected[at].saturating_add_signed(delta).min(max);
                        self.detail_cursor = 0;
                        self.detail_scroll = 0;
                        self.horizontal = 0;
                        self.detail_rows.clear();
                        self.reveal_file = self.page == 2 && self.pr_tab == 3;
                        self.ensure(false);
                    } else if self.focus == 0 {
                        self.workflow_selected = self
                            .workflow_selected
                            .saturating_add_signed(delta)
                            .min(self.workflows().len());
                        self.run_selected = 0;
                        self.run_id = None;
                        self.detail_cursor = 0;
                        self.ensure(false);
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
                        self.ensure(false);
                    }
                }
                KeyCode::Enter => {
                    if self.focus == 2 {
                        if let Some(action) = self
                            .detail_rows
                            .get(self.detail_cursor)
                            .and_then(|row| row.action.clone())
                        {
                            if matches!(action, Action::Viewed(..))
                                && let Some(reason) = write_refusal
                            {
                                return Err(reason.into());
                            }
                            return self.activate(action);
                        }
                    } else {
                        if self.page == 3 && self.focus == 1 {
                            self.run_id = self
                                .runs()
                                .get(self.run_selected)
                                .map(|run| number(run, "databaseId"));
                            self.attempt = 0;
                            self.ensure(false);
                        }
                        self.last_list = self.focus;
                        self.focus = 2;
                        if self.page != 1 {
                            self.detail_cursor = 0;
                        }
                        // A reader opened from its top reads; one the user already moved
                        // through comes back with its cursor where it was.
                        self.reading = self.page < 3 && self.detail_cursor == 0;
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
/// Pull requests and Issues paint on the shell (`paint.rs`); Actions on `runs.rs`.
pub(in crate::git::screen) fn frame(
    app: &mut super::GitScreen,
    width: u16,
    height: u16,
) -> crate::ui::Frame {
    if matches!(app.github.page, 1 | 2) {
        paint::frame(app, width, height)
    } else if app.github.page == notifications::PAGE {
        notifications::frame(app, width, height)
    } else {
        runs::frame(app, width, height)
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

#[cfg(test)]
mod list_tests {
    use super::*;

    fn cached_workspace() -> Workspace {
        let mut workspace = Workspace::new(PathBuf::new());
        workspace.page = 1;
        workspace.host = "github.com".into();
        workspace.repo = "example/demo".into();
        workspace.cache.insert(
            Key::Repository,
            Ok(Reply {
                text: String::new(),
                value: Value::Null,
            }),
        );
        let key = workspace.list_key();
        workspace.cache.insert(
            key.clone(),
            Ok(Reply {
                text: String::new(),
                value: Value::Array(
                    (1..=100)
                        .map(|id| serde_json::json!({"number":id}))
                        .collect(),
                ),
            }),
        );
        workspace.list_limits.insert(key, 100);
        workspace
    }

    #[test]
    fn pr_tab_shortcuts_use_the_same_native_detail_entry_as_tab_actions() {
        fn pr_workspace() -> Workspace {
            let mut workspace = cached_workspace();
            workspace.page = 2;
            workspace.opened = Some((2, 3));
            workspace.focus = 0;
            workspace.last_list = 0;
            workspace.drafts.insert(
                "unrelated scoped draft".into(),
                vec!["retain exactly".into()],
            );
            workspace
        }
        let mut keyboard = pr_workspace();
        let mut action = pr_workspace();
        let before = keyboard
            .cache
            .get(&Key::Repository)
            .unwrap()
            .as_ref()
            .unwrap()
            .value
            .clone();
        for (key, expected) in [('L', 1), ('L', 2), ('L', 3), ('H', 2), ('H', 1), ('H', 0)] {
            keyboard.focus = 0; // The shortcut deliberately enters the chosen detail tab.
            keyboard.detail_cursor = 17;
            keyboard.detail_scroll = 9;
            keyboard.horizontal = 32;
            action.focus = 0;
            action.detail_cursor = 17;
            action.detail_scroll = 9;
            action.horizontal = 32;
            assert!(
                keyboard
                    .event(&KeyEvent::plain(KeyCode::Char(key)), None)
                    .unwrap()
                    .is_none()
            );
            assert!(action.activate(Action::PrTab(expected)).unwrap().is_none());
            assert_eq!(keyboard.pr_tab, expected);
            assert_eq!(keyboard.focus, 2);
            assert_eq!(keyboard.last_list, 0);
            assert_eq!(
                (
                    keyboard.detail_cursor,
                    keyboard.detail_scroll,
                    keyboard.horizontal
                ),
                (0, 0, 0)
            );
            // Files reveals its file; Checks its selected job (F-PRCHECKS).
            assert_eq!(
                keyboard.reveal_file,
                expected == pr::FILES || expected == pr::CHECKS
            );
            assert_eq!(keyboard.drafts, action.drafts);
            assert_eq!(keyboard.opened, Some((2, 3)));
            assert_eq!(
                keyboard
                    .queue
                    .iter()
                    .map(|request| &request.key)
                    .collect::<Vec<_>>(),
                action
                    .queue
                    .iter()
                    .map(|request| &request.key)
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                keyboard
                    .cache
                    .get(&Key::Repository)
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .value,
                before
            );
            assert!(
                keyboard
                    .event(
                        &KeyEvent {
                            code: KeyCode::Char('h'),
                            modifiers: crate::editor::input::Modifiers {
                                control: true,
                                ..Default::default()
                            }
                        },
                        None
                    )
                    .unwrap()
                    .is_none()
            );
            assert_eq!(keyboard.focus, 0);
            assert_eq!(keyboard.pr_tab, expected);
        }
    }

    #[test]
    fn queued_more_is_coalesced_and_pruning_does_not_advance_the_successful_depth() {
        let mut workspace = cached_workspace();
        let key = workspace.list_key();
        for _ in 0..20 {
            workspace.load_more();
        }
        assert_eq!(workspace.queue.len(), 1);
        assert_eq!(workspace.queue[0].list_limit(), Some(200));
        assert_eq!(workspace.list_limit(), 100);
        assert!(workspace.loading_more());
        workspace
            .apply_view("author:@me".into(), "open".into(), None)
            .unwrap();
        assert!(!workspace.pending(&key));
        assert_eq!(workspace.list_limit(), 100);
        let list = workspace
            .queue
            .iter()
            .find(|request| request.key == workspace.list_key())
            .unwrap();
        assert_eq!(list.list_limit(), Some(100));
        workspace
            .apply_view(String::new(), "open".into(), None)
            .unwrap();
        assert_eq!(workspace.list_limit(), 100);
        let list = workspace
            .queue
            .iter()
            .find(|request| request.key == key)
            .unwrap();
        assert_eq!(list.list_limit(), Some(100));
        assert!(!workspace.loading_more());
        assert!(
            workspace
                .cache
                .keys()
                .all(|key| !matches!(key, Key::Item(..)))
        );
    }

    #[test]
    fn list_depth_is_repository_host_scoped_like_the_existing_cache() {
        for (repo, host) in [
            ("other/demo", "github.com"),
            ("example/demo", "git.example.test"),
        ] {
            let mut workspace = cached_workspace();
            workspace.list_limits.insert(workspace.list_key(), 700);
            assert_eq!(workspace.list_limit(), 700);
            workspace.change_repository(repo.into(), host.into());
            assert!(workspace.list_limits.is_empty());
            assert!(workspace.cache.is_empty());
            assert_eq!(workspace.list_limit(), 100);
            assert!(!workspace.list_may_have_more());
        }
    }
}
