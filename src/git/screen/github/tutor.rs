//! What the Git tutor's forge lessons check (`src/git/screen/tutor`): a read-only view of
//! the forge tabs, so a lesson ends only when the screen shows the user did the thing.
use super::*;
use crate::forge::host_state::HostState;
use crate::forge::transport::TransportKind;

pub(in crate::git::screen) struct TutorView {
    pub page: usize,
    /// The reader (right pane) has the keys.
    pub reader: bool,
    /// The item open in the reader on this page.
    pub opened: Option<u64>,
    /// The list's built-in or saved section, `None` for a custom filter.
    pub section: Option<String>,
    pub query: String,
    /// The pull request reader's tab: Conversation, Commits, Checks or Files changed.
    pub pr_tab: &'static str,
    pub accounts_open: bool,
    pub notice: String,
    /// Direct HTTP with a credential.
    pub signed_in: bool,
    pub runs: runs::TutorRuns,
    /// The reader's cursor rests on a link (a row Enter opens), not reading from the top.
    pub link: bool,
    /// The reader's find (`Ctrl-f`) query, while one is active.
    pub find: String,
    /// The list beside the reader (Actions: the left column) is hidden.
    pub list_hidden: bool,
    /// The list's Open/Closed state: `open`, `closed`, `merged` or `all`.
    pub state: String,
    /// The saved views (sections) of this page and repository.
    pub views: Vec<String>,
    /// The cross-repository inbox (`i`) is on.
    pub inbox: bool,
    /// Files changed pans long lines sideways instead of wrapping them (`w`).
    pub unwrapped: bool,
    /// The changed file Files shows for the open pull request.
    pub file: Option<String>,
    /// The run attempt shown (`0` the latest).
    pub attempt: u32,
    /// Notifications: only unread threads, and the sidebar has the keys.
    pub unread_only: bool,
    pub sidebar: bool,
    /// Notifications' `/` filter and whether it is still being typed.
    pub thread_filter: String,
    pub thread_filtering: bool,
}

impl Workspace {
    pub(in crate::git::screen) fn tutor_view(&self) -> TutorView {
        let at = self.page.saturating_sub(1).min(1);
        let section = if matches!(self.page, 1 | 2) {
            let sections = self.sections();
            self.active_section(&sections)
                .and_then(|index| sections.into_iter().nth(index))
                .map(|section| section.name)
        } else {
            None
        };
        let state = self.host_state();
        let notes = self.tutor_notifications();
        TutorView {
            page: self.page,
            reader: self.focus == 2,
            opened: self
                .opened
                .filter(|(page, _)| *page == self.page)
                .map(|(_, number)| number),
            section,
            query: self.queries[at].clone(),
            pr_tab: ["Conversation", "Commits", "Checks", "Files changed"]
                .get(self.pr_tab)
                .copied()
                .unwrap_or(""),
            accounts_open: self.accounts.open,
            notice: self.notice.clone(),
            signed_in: matches!(
                state,
                Some(HostState::Ready {
                    transport: TransportKind::Http { .. },
                    ..
                })
            ),
            runs: self.tutor_runs(),
            link: self.focus == 2
                && !self.reading
                && self
                    .detail_rows
                    .get(self.detail_cursor)
                    .is_some_and(|row| row.action.is_some()),
            find: if self.detail_find.search.active() {
                self.detail_find.search.query.clone()
            } else {
                String::new()
            },
            list_hidden: self.sidebar_hidden(),
            state: self.states[at].clone(),
            views: self
                .scoped_views()
                .unwrap_or_default()
                .into_iter()
                .map(|view| view.name)
                .collect(),
            inbox: self.inbox.enabled,
            unwrapped: self.diff_nowrap,
            file: (self.page == 2)
                .then(|| self.item())
                .flatten()
                .map(|item| self.selected_file(self.number(), item).to_owned())
                .filter(|path| !path.is_empty()),
            attempt: self.attempt,
            unread_only: notes.0,
            sidebar: notes.1,
            thread_filter: self.notify.filter.query.clone(),
            thread_filtering: self.notify.filter.editing,
        }
    }
}
