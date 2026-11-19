//! Host-scoped search lists. Detail caches remain single-repository: switching a
//! remote row invalidates them, never the local Git root or a confirmed write.
use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::screen) struct Target {
    pub host: String,
    pub repo: String,
    pub page: usize,
    pub number: u64,
}
impl Target {
    pub(super) fn from_url(
        text: &str,
        host: &str,
        page: usize,
        number: u64,
    ) -> Result<Self, String> {
        let url = reqwest::Url::parse(text).map_err(|_| "Search returned an invalid item URL")?;
        let parts = url.path().trim_matches('/').split('/').collect::<Vec<_>>();
        if url.scheme() != "https"
            || url.host_str() != Some(host)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || parts.len() != 4
            || parts[2] != if page == 1 { "issues" } else { "pull" }
            || parts[3].parse::<u64>().ok() != Some(number)
            || number == 0
        {
            return Err("Search returned an item outside the requested host/type".into());
        }
        let repo = format!("{}/{}", parts[0], parts[1]).to_ascii_lowercase();
        if !crate::forge::pull_request::valid_repository(&repo) {
            return Err("Search returned an invalid repository".into());
        }
        Ok(Self {
            host: host.into(),
            repo,
            page,
            number,
        })
    }
    pub(super) fn from_row(value: &Value, host: &str, page: usize) -> Result<Self, String> {
        let target = Self::from_url(string(value, "url"), host, page, number(value, "number"))?;
        if !string(&value["repository"], "nameWithOwner").eq_ignore_ascii_case(&target.repo) {
            return Err("Search item URL and repository disagree".into());
        }
        Ok(target)
    }
    fn identity(&self) -> String {
        format!("{}/{}/{}/{}", self.host, self.repo, self.page, self.number)
    }
}

#[derive(Default)]
struct Settings {
    queries: [String; 2],
    states: [String; 2],
    views: [Option<views::SavedView>; 2],
    selected: [usize; 2],
}
#[derive(Default)]
pub(super) struct Inbox {
    pub enabled: bool,
    home: String,
    local: Option<Settings>,
    remote: Option<Settings>,
    pub origin_page: Option<usize>,
}

pub(super) fn validate_rows(
    value: &mut Value,
    host: &str,
    page: usize,
    state: &str,
) -> Result<(), String> {
    let rows = value
        .as_array_mut()
        .ok_or("Search did not return an item array")?;
    if rows.len() > LIST_CEILING {
        return Err("Search exceeded the bounded result limit".into());
    }
    let mut identities = BTreeSet::new();
    for row in rows {
        let target = Target::from_row(row, host, page)?;
        if !identities.insert(target.identity()) {
            return Err("Search returned duplicate item identities; reload the section".into());
        }
        // Search exports closed for both closed-unmerged and merged PRs.
        // Only the explicit --merged section supplies the missing merge fact;
        // never infer it for ordinary closed/all searches or for issues.
        if let Some(row_state) = row.get_mut("state") {
            let mut normalized = row_state.as_str().unwrap_or("").to_ascii_uppercase();
            if page == 2 && state == "merged" {
                if !matches!(normalized.as_str(), "CLOSED" | "MERGED") {
                    return Err("Merged search returned a non-closed pull request".into());
                }
                normalized = "MERGED".into();
            }
            *row_state = Value::String(normalized);
        } else if page == 2 && state == "merged" {
            return Err("Merged search omitted pull request state".into());
        }
    }
    Ok(())
}

impl Workspace {
    fn take_list_settings(&mut self) -> Settings {
        Settings {
            queries: std::mem::take(&mut self.queries),
            states: std::mem::take(&mut self.states),
            views: std::mem::take(&mut self.active_views),
            selected: std::mem::take(&mut self.selected),
        }
    }
    fn put_list_settings(&mut self, settings: Settings) {
        self.queries = settings.queries;
        self.states = settings.states;
        self.active_views = settings.views;
        self.selected = settings.selected;
    }
    pub(super) fn toggle_inbox(&mut self) -> Result<(), String> {
        if self.writing() {
            return Err("Wait for the current action to finish".into());
        }
        if self.repo.is_empty() || self.host.is_empty() {
            return Err("Load a GitHub repository before opening i inbox".into());
        }
        if !self.inbox.enabled {
            self.view_scope()?;
        }
        self.remember_list_selection();
        if self.inbox.enabled {
            self.inbox.remote = Some(self.take_list_settings());
            self.inbox.enabled = false;
            let home = self.inbox.home.clone();
            self.switch_detail_repository(home);
            let local = self.inbox.local.take().unwrap_or_default();
            self.put_list_settings(local);
        } else {
            self.inbox.home = self.repo.clone();
            self.inbox.local = Some(self.take_list_settings());
            let settings = self.inbox.remote.take().unwrap_or_else(|| Settings {
                queries: ["assignee:@me".into(), "review-requested:@me".into()],
                states: ["open".into(), "open".into()],
                ..Settings::default()
            });
            self.put_list_settings(settings);
            self.inbox.enabled = true;
        }
        self.inbox.origin_page = None;
        self.opened = None;
        self.focus = 0;
        self.reset_remote_detail();
        if matches!(self.page, 1 | 2) {
            self.restore_list_selection();
        }
        self.notice.clear();
        self.notice_error = false;
        self.ensure(false);
        Ok(())
    }
    pub(super) fn reset_remote_detail(&mut self) {
        self.opened = None;
        self.selected_commit = None;
        self.detail_rows.clear();
        self.detail_cursor = 0;
        self.detail_scroll = 0;
        self.horizontal = 0;
        self.detail_find = find::DetailFind::default();
        self.reading_position = lines::ReadingPosition::default();
        self.selected_files.clear();
        self.collapsed.clear();
        self.reveal_file = self.page == 2 && self.pr_tab == 3;
        self.pending_pr = false;
        self.ready_form = None;
    }
    pub(super) fn switch_detail_repository(&mut self, repo: String) {
        if self.repo.eq_ignore_ascii_case(&repo) {
            return;
        }
        self.scope_epoch = self
            .scope_epoch
            .checked_add(1)
            .expect("GitHub scope generation exhausted");
        self.repo = repo;
        // Repository metadata belongs to home; inbox creation/develop are disabled.
        // Lists have independent identities; only number-keyed detail state is evicted.
        self.cache
            .retain(|key, _| matches!(key, Key::Repository | Key::List(..) | Key::Inbox(..)));
        self.queue.retain(|request| {
            matches!(
                request.key,
                Key::Write | Key::WorktreeWrite(_) | Key::Inbox(..)
            )
        });
        self.reset_remote_detail();
    }
    pub(super) fn sync_inbox_repository(&mut self) {
        if !self.inbox.enabled || !matches!(self.page, 1 | 2) || self.opened.is_some() {
            return;
        }
        let target = self
            .items()
            .get(self.selected[self.page - 1])
            .and_then(|row| Target::from_row(row, &self.host, self.page).ok());
        if let Some(target) = target {
            self.switch_detail_repository(target.repo);
        }
    }
    pub(super) fn return_inbox_list(&mut self) {
        if self.inbox.enabled && self.opened.is_some() {
            if let Some(page) = self.inbox.origin_page.take() {
                self.page = page;
            }
            self.opened = None;
            self.reset_remote_detail();
            self.restore_list_selection();
            self.ensure(false);
        }
    }
    pub(super) fn row_identity(&self, item: &Value) -> String {
        if self.inbox.enabled {
            return Target::from_row(item, &self.host, self.page)
                .map(|target| target.identity())
                .unwrap_or_default();
        }
        format!(
            "{}/{}/{}/{}",
            self.host,
            self.repo,
            self.page,
            number(item, "number")
        )
    }
    /// Whether a reply started under `scope` still belongs on screen.
    pub(super) fn accept_scope(&self, key: &Key, scope: &super::lanes::Scope) -> bool {
        match key {
            Key::Write | Key::WorktreeWrite(_) => true,
            Key::Inbox(host, ..) => host == &self.host,
            _ => scope == &(self.host.clone(), self.repo.clone(), self.scope_epoch),
        }
    }
    /// A remote inbox item of another repository: this checkout does not hold its files.
    pub(super) fn foreign_inbox_item(&self) -> bool {
        self.inbox.enabled && !self.repo.eq_ignore_ascii_case(&self.inbox.home)
    }
    pub(super) fn check_inbox_action(&self, action: &Action) -> Result<(), String> {
        if self.inbox.enabled {
            if matches!(
                action,
                Action::CreateIssue
                    | Action::CreatePullRequest
                    | Action::Develop(_)
                    | Action::Run(_)
            ) {
                return Err("Press i to go back to this repository first".into());
            }
            if matches!(action, Action::Checkout(_) | Action::Worktree(_))
                && !self.repo.eq_ignore_ascii_case(&self.inbox.home)
            {
                return Err("Remote inbox item: open its local repository before checkout".into());
            }
            if action.item_scoped() && self.number() == 0 {
                return Err("Select an inbox item first".into());
            }
            let target = match action {
                Action::Browser(page, id)
                | Action::Web(page, id, _)
                | Action::Edit(page, id)
                | Action::Comment(page, id)
                | Action::State(page, id, _)
                | Action::Metadata(page, id, _) => Some((*page, *id)),
                Action::Review(id)
                | Action::Merge(id, _)
                | Action::Checkout(id)
                | Action::Worktree(id)
                | Action::Files(id)
                | Action::Viewed(id, _)
                | Action::Inline { number: id, .. }
                | Action::Reply { number: id, .. } => Some((2, *id)),
                _ => None,
            };
            if target.is_some_and(|target| target != (self.page, self.number())) {
                return Err(
                    "Selected item changed; reopen the original action (draft preserved)".into(),
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(repo: &str, page: usize) -> Value {
        json!({"number":7,"repository":{"nameWithOwner":repo},
            "url":format!("https://github.com/{repo}/{}/7", if page == 1 { "issues" } else { "pull" }),
            "title":format!("{repo} seven"),"body":format!("Source bytes for {repo}"),"state":"open"})
    }
    fn workspace(root: PathBuf) -> Workspace {
        let mut workspace = Workspace::new(root);
        workspace.host = "github.com".into();
        workspace.repo = "local/root".into();
        workspace.page = 2;
        workspace.states = ["open".into(), "open".into()];
        workspace.inbox.enabled = true;
        workspace.inbox.home = "local/root".into();
        workspace.cache.insert(
            Key::Repository,
            Ok(Reply {
                value: Value::Null,
                text: String::new(),
            }),
        );
        let mut rows = json!([row("alpha/one", 2), row("beta/two", 2)]);
        validate_rows(&mut rows, "github.com", 2, "open").unwrap();
        workspace.cache.insert(
            workspace.list_key(),
            Ok(Reply {
                text: rows.to_string(),
                value: rows,
            }),
        );
        workspace.ensure(false);
        workspace
    }
    fn form_action(form: Form) -> Action {
        let FormAction::Github(action) = form.action else {
            panic!("GitHub form expected")
        };
        action
    }

    #[test]
    fn same_number_rows_have_scoped_reads_forms_drafts_and_writes() {
        let owner = tempfile::tempdir().unwrap();
        git2::Repository::init(owner.path()).unwrap();
        std::fs::write(owner.path().join("source.txt"), "never change me\n").unwrap();
        let mut workspace = workspace(owner.path().into());
        assert_eq!(workspace.repo, "alpha/one");
        let first = form_action(workspace.activate(Action::Comment(2, 7)).unwrap().unwrap());
        workspace.save_draft(&first, vec!["alpha draft 界".into()]);
        workspace.cache.insert(
            Key::Item(2, 7),
            Ok(Reply {
                value: json!({"number":7,"body":"alpha only"}),
                text: String::new(),
            }),
        );
        workspace.selected_files.insert(7, "alpha.rs".into());
        workspace.selected[1] = 1;
        workspace.ensure(false);
        assert_eq!(workspace.repo, "beta/two");
        assert!(workspace.item().is_none());
        assert!(workspace.selected_files.is_empty());
        assert_eq!(
            string(workspace.list_item().unwrap(), "body"),
            "Source bytes for beta/two"
        );
        let second_form = workspace.activate(Action::Comment(2, 7)).unwrap().unwrap();
        assert!(second_form.fields[0].input.input.is_empty());
        assert!(second_form.title.contains("github.com/beta/two · PR #7"));
        let second = form_action(second_form);
        assert_ne!(workspace.draft_key(&first), workspace.draft_key(&second));
        assert!(
            workspace
                .submit(first.clone(), vec!["still alpha".into()])
                .unwrap_err()
                .contains("source changed")
        );
        assert!(!workspace.writing());
        workspace
            .submit(second, vec!["beta comment".into()])
            .unwrap();
        let write = workspace
            .queue
            .iter()
            .find(|request| request.key == Key::Write)
            .unwrap();
        let Op::Write(target, comment) = &write.op else {
            panic!("a comment is a write")
        };
        assert_eq!(target, &ForgeTarget::repository("github.com", "beta/two"));
        assert!(
            matches!(comment, Write::Comment { number: 7, body, .. } if body == "beta comment")
        );
        assert!(workspace.check_inbox_action(&Action::Checkout(7)).is_err());
        assert!(workspace.check_inbox_action(&Action::Worktree(7)).is_err());
        workspace.queue.clear();
        workspace.selected[1] = 0;
        workspace.ensure(false);
        let restored = workspace.activate(Action::Comment(2, 7)).unwrap().unwrap();
        assert_eq!(restored.fields[0].input.input, "still alpha");
        assert_eq!(workspace.root, owner.path());
        assert_eq!(
            std::fs::read_to_string(owner.path().join("source.txt")).unwrap(),
            "never change me\n"
        );
        assert!(
            git2::Repository::open(owner.path())
                .unwrap()
                .head()
                .is_err()
        );
    }

    #[test]
    fn stale_same_number_reply_and_repository_round_trip_do_not_replace_current_source() {
        let mut workspace = workspace(PathBuf::new());
        let first_epoch = workspace.scope_epoch;
        let scope: super::super::lanes::Scope =
            ("github.com".into(), "alpha/one".into(), first_epoch);
        workspace.selected[1] = 1;
        workspace.ensure(false);
        assert!(!workspace.accept_scope(&Key::Item(2, 7), &scope));
        workspace.selected[1] = 0;
        workspace.ensure(false);
        assert!(
            !workspace.accept_scope(&Key::Item(2, 7), &scope),
            "A → B → A is a new detail generation"
        );
        workspace.cache.insert(
            Key::Item(2, 7),
            Ok(Reply {
                value: json!({"body":"current alpha"}),
                text: String::new(),
            }),
        );
        workspace.queue.clear();
        workspace.page = 0; // Consume only the injected reply; never launch a CLI in this state test.
        workspace.active.answer_for_test(
            Request::placeholder(Key::Item(2, 7)),
            scope,
            ForgeReply::Fetched(
                Read::PullRequest(7),
                Ok(crate::forge::provider::Fetched::payload(
                    crate::forge::request::Payload::Text("stale alpha".into()),
                )),
            ),
        );
        assert!(workspace.poll());
        assert_eq!(
            string(workspace.value(&Key::Item(2, 7)).unwrap(), "body"),
            "current alpha"
        );
        assert!(workspace.active.idle());
    }

    #[test]
    fn section_and_link_return_restore_repository_qualified_cursor_not_number() {
        let mut workspace = workspace(PathBuf::new());
        workspace.selected[1] = 1;
        workspace.ensure(false);
        workspace.remember_list_selection();
        let origin = workspace.list_key();
        let target =
            Target::from_url("https://github.com/alpha/one/issues/7", "github.com", 1, 7).unwrap();
        workspace.activate(Action::OpenRemote(target)).unwrap();
        assert_eq!((workspace.page, workspace.repo.as_str()), (1, "alpha/one"));
        workspace.return_inbox_list();
        assert_eq!(workspace.page, 2);
        assert_eq!(workspace.list_key(), origin);
        assert_eq!(workspace.selected[1], 1);
        assert_eq!(workspace.repo, "beta/two");
        workspace
            .apply_view("author:@me".into(), "closed".into(), None)
            .unwrap();
        workspace
            .apply_view(String::new(), "open".into(), None)
            .unwrap();
        assert_eq!(workspace.repo, "beta/two");
        workspace.value(&origin).unwrap();
        let reply = workspace.cache.get_mut(&origin).unwrap().as_mut().unwrap();
        reply.value.as_array_mut().unwrap().reverse();
        workspace.restore_list_selection();
        workspace.ensure(false);
        assert_eq!(workspace.selected[1], 0);
        assert_eq!(workspace.repo, "beta/two");
    }

    #[test]
    fn saved_sections_are_host_and_kind_scoped_and_preserve_local_views() {
        let owner = tempfile::tempdir().unwrap();
        git2::Repository::init(owner.path()).unwrap();
        let mut workspace = workspace(owner.path().into());
        let inbox_scope = workspace.view_scope().unwrap();
        workspace
            .save_view(
                &inbox_scope,
                &[
                    "Reviews".into(),
                    "review-requested:@me".into(),
                    "open".into(),
                ],
            )
            .unwrap();
        workspace.inbox.enabled = false;
        workspace.repo = "local/root".into();
        let local_scope = workspace.view_scope().unwrap();
        workspace
            .save_view(
                &local_scope,
                &["Reviews".into(), "label:local".into(), "all".into()],
            )
            .unwrap();
        workspace.inbox.enabled = true;
        workspace.host = "git.example.test".into();
        let other_host = workspace.view_scope().unwrap();
        workspace
            .save_view(
                &other_host,
                &["Reviews".into(), "author:@me".into(), "closed".into()],
            )
            .unwrap();
        let persisted = views::load(owner.path()).unwrap();
        let encoded = serde_json::to_value(persisted).unwrap();
        assert_eq!(encoded.as_array().unwrap().len(), 3);
        assert!(
            encoded
                .as_array()
                .unwrap()
                .iter()
                .any(|view| view["repo"] == "*" && view["host"] == "github.com")
        );
        assert!(
            encoded
                .as_array()
                .unwrap()
                .iter()
                .any(|view| view["repo"] == "local/root" && view["query"] == "label:local")
        );
        let mut reopened = workspace;
        reopened.host = "github.com".into();
        reopened.choose_view("Saved: Reviews").unwrap();
        assert_eq!(reopened.queries[1], "review-requested:@me");
        assert!(
            reopened
                .save_view(
                    &other_host,
                    &["wrong host".into(), String::new(), "open".into()]
                )
                .is_err()
        );
    }

    #[test]
    fn merged_section_context_does_not_relabel_ordinary_closed_prs_or_issues() {
        let mut rows = json!([row("alpha/one", 2)]);
        rows[0]["state"] = "closed".into();
        for (state, expected) in [
            ("merged", "MERGED"),
            ("closed", "CLOSED"),
            ("all", "CLOSED"),
        ] {
            let mut reply = rows.clone();
            validate_rows(&mut reply, "github.com", 2, state).unwrap();
            assert_eq!(reply[0]["state"], expected);
        }
        let mut issue = json!([row("alpha/one", 1)]);
        issue[0]["state"] = "closed".into();
        validate_rows(&mut issue, "github.com", 1, "closed").unwrap();
        assert_eq!(issue[0]["state"], "CLOSED");
        let mut wrong_state = json!([row("alpha/one", 2)]);
        assert!(validate_rows(&mut wrong_state, "github.com", 2, "merged").is_err());
        let mut workspace = workspace(PathBuf::new());
        workspace.states[1] = "merged".into();
        let Op::Read(_, Read::Search { filter, .. }) = workspace.list_read(100) else {
            panic!("inbox reads search")
        };
        assert_eq!(filter.state, StateFilter::Merged);
    }

    #[test]
    fn native_screen_refuses_stale_remote_checkout_and_worktree_controls() {
        use crate::screen::GitScreen;
        use crate::ui::{KeyInputKind, UiEvent};
        let owner = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(owner.path()).unwrap();
        std::fs::write(owner.path().join("tracked.txt"), "committed\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("tracked.txt")).unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let signature = git2::Signature::now("Inbox Test", "inbox@example.invalid").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "initial", &tree, &[])
            .unwrap();
        std::fs::write(owner.path().join("tracked.txt"), "staged\n").unwrap();
        index.add_path(std::path::Path::new("tracked.txt")).unwrap();
        index.write().unwrap();
        std::fs::write(owner.path().join("tracked.txt"), "dirty working copy\n").unwrap();
        let mut screen = GitScreen::open(owner.path()).unwrap();
        let root = screen.root().to_path_buf();
        let head = repo.head().unwrap().target().unwrap();
        let head_file = std::fs::read(repo.path().join("HEAD")).unwrap();
        let index_file = std::fs::read(repo.path().join("index")).unwrap();
        let working = std::fs::read(owner.path().join("tracked.txt")).unwrap();
        screen.page = 2;
        screen.github = workspace(owner.path().into());
        screen.github.cache.insert(Key::Item(2, 7), Ok(Reply {
            value: json!({"number":7,"title":"remote seven","state":"OPEN","body":"remote body","files":[]}),
            text: String::new(),
        }));
        let rows = lines::detail_lines(&screen.github, 100);
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row.action, Some(Action::Checkout(_) | Action::Worktree(_))))
        );
        for action in [Action::Checkout(7), Action::Worktree(7)] {
            // Model an already-rendered control arriving after a remote retarget.
            // The native Enter path must reject it even though normal rendering hides it.
            let mut rows = lines::detail_lines(&screen.github, 100);
            rows[0].action = Some(action);
            screen.github.detail_rows = rows;
            screen.github.detail_cursor = 0;
            screen.github.focus = 2;
            screen.github.queue.clear();
            screen.event(UiEvent::Key {
                key: KeyEvent::plain(KeyCode::Enter),
                kind: KeyInputKind::Press,
            });
            assert!(
                screen
                    .notice
                    .contains("open its local repository before checkout")
            );
            assert!(screen.form.is_none());
            assert!(!screen.github.writing());
            assert!(screen.github.queue.is_empty() && screen.github.active.idle());
            assert_eq!(screen.root(), root.as_path());
            assert_eq!(repo.head().unwrap().target(), Some(head));
            assert_eq!(std::fs::read(repo.path().join("HEAD")).unwrap(), head_file);
            assert_eq!(
                std::fs::read(repo.path().join("index")).unwrap(),
                index_file
            );
            assert_eq!(
                std::fs::read(owner.path().join("tracked.txt")).unwrap(),
                working
            );
            assert_eq!(repo.worktrees().unwrap().len(), 0);
        }
    }

    #[test]
    fn bounded_search_uses_only_supported_fields_and_rejects_wrong_host_or_identity() {
        let workspace = workspace(PathBuf::new());
        let Op::Read(_, Read::Search { kind, page, .. }) = workspace.list_read(200) else {
            panic!("inbox reads search")
        };
        assert_eq!((kind, page.per_page), (ItemKind::PullRequest, 200));
        let Op::Read(target, _) = workspace.list_read(200) else {
            panic!("inbox list is a read")
        };
        assert_eq!(
            target,
            ForgeTarget::host("github.com"),
            "search is host-wide"
        );
        let mut invalid = json!([row("alpha/one", 2)]);
        invalid[0]["url"] = "https://elsewhere.test/alpha/one/pull/7".into();
        assert!(validate_rows(&mut invalid, "github.com", 2, "open").is_err());
        let mut duplicate = json!([row("alpha/one", 2), row("alpha/one", 2)]);
        assert!(validate_rows(&mut duplicate, "github.com", 2, "open").is_err());
        let mut disagreement = json!([row("alpha/one", 2)]);
        disagreement[0]["repository"]["nameWithOwner"] = "beta/two".into();
        assert!(validate_rows(&mut disagreement, "github.com", 2, "open").is_err());
    }
}
