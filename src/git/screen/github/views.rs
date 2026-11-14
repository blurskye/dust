//! Locally stored triage preferences, scoped by host/repository (or host-wide `*` inbox)
//! and item kind. Only the Local Git config backend is writable.
use super::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

const CONFIG_KEY: &str = "dusty-git.githubViews";
const MAX_BYTES: usize = 256 * 1024;
const MAX_VIEWS: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::git::screen) struct Scope {
    host: String,
    repo: String,
    page: usize,
}
impl Scope {
    fn new(host: &str, repo: &str, page: usize) -> Result<Self, String> {
        if host.is_empty()
            || host.chars().any(|c| c.is_whitespace() || c.is_control())
            || host.contains(['/', '\\', '@', '?', '#'])
        {
            return Err("Choose a valid GitHub host for saved views".into());
        }
        let url = reqwest::Url::parse(&format!("https://{host}"))
            .map_err(|_| "Choose a valid GitHub host for saved views")?;
        if !matches!(page, 1 | 2)
            || (repo != "*" && !crate::forge::pull_request::valid_repository(repo))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("Saved views require a GitHub host, OWNER/REPO and Issues or PRs".into());
        }
        let host = url
            .host_str()
            .ok_or("Saved views require a GitHub host")?
            .trim_end_matches('.')
            .to_ascii_lowercase();
        if host.is_empty() {
            return Err("Saved views require a GitHub host".into());
        }
        Ok(Self {
            host,
            repo: repo.to_ascii_lowercase(),
            page,
        })
    }
    fn contains(&self, view: &SavedView) -> bool {
        self.host == view.host && self.repo == view.repo && self.page == view.page
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub(super) struct SavedView {
    host: String,
    repo: String,
    page: usize,
    pub name: String,
    pub query: String,
    pub state: String,
}
impl SavedView {
    fn new(scope: &Scope, name: &str, query: &str, state: &str) -> Result<Self, String> {
        if name.chars().any(char::is_control) {
            return Err("View name must not contain control characters".into());
        }
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            return Err("View name must be 1–80 characters, without control characters".into());
        }
        validate_filter(scope.page, query, state)?;
        Ok(Self {
            host: scope.host.clone(),
            repo: scope.repo.clone(),
            page: scope.page,
            name: name.into(),
            query: query.into(),
            state: state.into(),
        })
    }
    fn label(&self) -> String {
        format!("Saved: {}", self.name)
    }
}

pub(super) fn validate_filter(page: usize, query: &str, state: &str) -> Result<(), String> {
    if !matches!(page, 1 | 2)
        || (!matches!(state, "open" | "closed" | "all") && !(page == 2 && state == "merged"))
    {
        return Err("Choose a valid state for this repository view".into());
    }
    if query.len() > 8192 || query.chars().any(char::is_control) {
        return Err(
            "GitHub query must be one line, at most 8192 bytes, without control characters".into(),
        );
    }
    Ok(())
}
fn config_error(error: impl std::fmt::Display) -> String {
    format!(
        "Saved views: {error} — inspect/repair local Git config {CONFIG_KEY}; no preferences overwritten"
    )
}
fn local_config(root: &Path) -> Result<git2::Config, String> {
    let repository = git2::Repository::open(root).map_err(config_error)?;
    repository
        .config()
        .map_err(config_error)?
        .open_level(git2::ConfigLevel::Local)
        .map_err(config_error)
}
/// §3.29: the Files tab's Unified/Split choice, kept beside the saved views in this
/// repository's local Git config so the next session opens the same way.
const SPLIT_KEY: &str = "dusty-git.forgeSplitDiff";
pub(super) fn split_preference(root: &Path) -> bool {
    local_config(root)
        .ok()
        .and_then(|config| config.get_bool(SPLIT_KEY).ok())
        .unwrap_or(false)
}
pub(super) fn set_split_preference(root: &Path, split: bool) {
    if let Ok(mut config) = local_config(root) {
        let _ = config.set_bool(SPLIT_KEY, split);
    }
}
fn parse(text: &str) -> Result<Vec<SavedView>, String> {
    if text.len() > MAX_BYTES {
        return Err(config_error("saved-view list exceeds 256 KiB"));
    }
    let mut views: Vec<SavedView> = serde_json::from_str(text).map_err(config_error)?;
    if views.len() > MAX_VIEWS {
        return Err(config_error("saved-view list exceeds 512 entries"));
    }
    let mut names = BTreeSet::new();
    for view in &mut views {
        let scope = Scope::new(&view.host, &view.repo, view.page).map_err(config_error)?;
        *view =
            SavedView::new(&scope, &view.name, &view.query, &view.state).map_err(config_error)?;
        if !names.insert((
            view.host.clone(),
            view.repo.clone(),
            view.page,
            view.name.clone(),
        )) {
            return Err(config_error(
                "duplicate saved-view name in the same host/repository/page",
            ));
        }
    }
    Ok(views)
}
fn read_config(config: &git2::Config) -> Result<Vec<SavedView>, String> {
    // Reject ambiguous multivars, rather than quietly discarding another list.
    let mut entries = config
        .entries(Some("^dusty-git\\.githubviews$"))
        .map_err(config_error)?;
    let mut count = 0;
    while let Some(entry) = entries.next() {
        entry.map_err(config_error)?;
        count += 1;
    }
    if count > 1 {
        return Err(config_error(
            "multiple saved-view lists; keep exactly one local entry",
        ));
    }
    match config.get_string(CONFIG_KEY) {
        Ok(text) => parse(&text),
        Err(error) if error.code() == git2::ErrorCode::NotFound && count == 0 => Ok(Vec::new()),
        Err(error) => Err(config_error(error)),
    }
}
pub(super) fn load(root: &Path) -> Result<Vec<SavedView>, String> {
    read_config(&local_config(root)?)
}
fn store(
    root: &Path,
    scope: &Scope,
    name: &str,
    replacement: Option<SavedView>,
) -> Result<(), String> {
    // Re-read on every edit: another screen/worktree may have saved other scopes.
    let mut config = local_config(root)?;
    let mut views = read_config(&config)?;
    let at = views
        .iter()
        .position(|view| scope.contains(view) && view.name == name);
    match (at, replacement) {
        (Some(at), Some(view)) => views[at] = view,
        (None, Some(view)) => views.push(view),
        (Some(at), None) => {
            views.remove(at);
        }
        (None, None) => return Err("This saved view no longer exists; reopen v views".into()),
    }
    let text = serde_json::to_string(&views).map_err(config_error)?;
    // Validate limits before asking libgit2 to lock and atomically replace local config.
    parse(&text)?;
    config.set_str(CONFIG_KEY, &text).map_err(config_error)
}

const BUILTINS: [(&str, &str); 5] = [
    ("All open", ""),
    ("Created by me", "author:@me"),
    ("Assigned to me", "assignee:@me"),
    ("Mentioning me", "mentions:@me"),
    ("Review requested", "review-requested:@me"),
];

impl Workspace {
    /// A built-in view's name in this forge's words.
    fn builtin_name(&self, name: &'static str) -> &'static str {
        if name == "Review requested" { self.requested_words().0 } else { name }
    }
    pub(super) fn view_scope(&self) -> Result<Scope, String> {
        Scope::new(
            &self.host,
            if self.inbox.enabled { "*" } else { &self.repo },
            self.page,
        )
    }
    pub(super) fn scoped_views(&self) -> Result<Vec<SavedView>, String> {
        let scope = self.view_scope()?;
        Ok(load(&self.root)?
            .into_iter()
            .filter(|view| scope.contains(view))
            .collect())
    }
    fn check_view_scope(&self, scope: &Scope) -> Result<(), String> {
        if &self.view_scope()? != scope {
            return Err(
                "The saved-view repository/page changed; reopen the view form (draft preserved)"
                    .into(),
            );
        }
        Ok(())
    }
    pub(super) fn view_name(&self) -> String {
        let at = self.page.saturating_sub(1).min(1);
        if let Some(view) = &self.active_views[at] {
            return format!(
                "{}{}",
                view.name,
                if view.query == self.queries[at] && view.state == self.states[at] {
                    ""
                } else {
                    " *"
                }
            );
        }
        BUILTINS[..if self.page == 2 { 5 } else { 4 }]
            .iter()
            .find(|(_, query)| self.states[at] == "open" && *query == self.queries[at])
            .map_or_else(|| "Custom".into(), |(name, _)| self.builtin_name(name).into())
    }
    pub(super) fn views_form(&self) -> Result<Form, String> {
        let saved = self.scoped_views()?;
        let mut labels = BUILTINS[..if self.page == 2 { 5 } else { 4 }]
            .iter()
            .map(|(name, _)| self.builtin_name(name).to_owned())
            .collect::<Vec<_>>();
        labels.extend(saved.iter().map(SavedView::label));
        let at = self.page - 1;
        let current = self.active_views[at]
            .as_ref()
            .and_then(|view| saved.iter().find(|entry| entry.name == view.name))
            .map(SavedView::label)
            .unwrap_or_else(|| self.view_name());
        let selected = labels
            .iter()
            .find(|label| **label == current)
            .unwrap_or(&labels[0]);
        Ok(self.form(
            if self.inbox.enabled {
                "Inbox sections"
            } else {
                "Repository views"
            },
            Action::Views,
            vec![actions::choice(
                "View",
                selected,
                &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            )],
        ))
    }
    pub(super) fn save_view_form(&self) -> Result<Form, String> {
        self.scoped_views()?; // A malformed/unreadable list must never become an empty replacement.
        let at = self.page - 1;
        let current = self.active_views[at].as_ref().map(|view| view.name.clone());
        Ok(self.form(
            if self.inbox.enabled {
                "Save section"
            } else {
                "Save view locally"
            },
            Action::SaveView(self.view_scope()?, current.clone()),
            vec![
                actions::field("View name", current.as_deref().unwrap_or(""), true, false),
                actions::field(&format!("Search ({} query)", self.forge_name()), &self.queries[at], false, false),
                actions::choice(
                    "State",
                    &self.states[at],
                    if self.page == 2 {
                        &["open", "closed", "merged", "all"]
                    } else {
                        &["open", "closed", "all"]
                    },
                ),
            ],
        ))
    }
    pub(super) fn delete_view_form(&self) -> Result<Form, String> {
        let saved = self.scoped_views()?;
        if saved.is_empty() {
            return Err("No saved views on this page; S saves the current query locally".into());
        }
        let at = self.page - 1;
        let current = self.active_views[at]
            .as_ref()
            .and_then(|view| saved.iter().find(|entry| entry.name == view.name))
            .unwrap_or(&saved[0]);
        Ok(self.form(
            "Delete saved view locally? · Esc cancels",
            Action::DeleteView(self.view_scope()?),
            vec![
                actions::choice(
                    "Saved view",
                    &current.name,
                    &saved
                        .iter()
                        .map(|view| view.name.as_str())
                        .collect::<Vec<_>>(),
                ),
                actions::choice(
                    "Confirmation",
                    "Keep view",
                    &["Keep view", "Delete locally"],
                ),
            ],
        ))
    }
    pub(super) fn choose_view(&mut self, label: &str) -> Result<(), String> {
        if let Some((_, query)) = BUILTINS[..if self.page == 2 { 5 } else { 4 }]
            .iter()
            .find(|(name, _)| self.builtin_name(name) == label)
        {
            return self.apply_view(query.to_string(), "open".into(), None);
        }
        let view = self
            .scoped_views()?
            .into_iter()
            .find(|view| view.label() == label)
            .ok_or("This saved view no longer exists; reopen v views")?;
        self.apply_view(view.query.clone(), view.state.clone(), Some(view))
    }
    pub(super) fn cycle_view(&mut self, forward: bool) -> Result<(), String> {
        let saved = self.scoped_views()?;
        if saved.is_empty() {
            return Err("No saved views on this page; S saves the current query locally".into());
        }
        let current = self.active_views[self.page - 1]
            .as_ref()
            .and_then(|view| saved.iter().position(|entry| entry.name == view.name));
        let at = match current {
            Some(at) => (at + if forward { 1 } else { saved.len() - 1 }) % saved.len(),
            None => {
                if forward {
                    0
                } else {
                    saved.len() - 1
                }
            }
        };
        let view = saved[at].clone();
        self.apply_view(view.query.clone(), view.state.clone(), Some(view))
    }
    pub(super) fn save_view(&mut self, scope: &Scope, values: &[String]) -> Result<(), String> {
        self.check_view_scope(scope)?;
        let [name, query, state] = values else {
            return Err("Invalid saved-view form".into());
        };
        let view = SavedView::new(scope, name, query, state)?;
        store(&self.root, scope, &view.name, Some(view.clone()))?;
        self.apply_view(query.clone(), state.clone(), Some(view.clone()))?;
        self.notice_error = false;
        self.notice = format!("View saved locally: {}", view.name);
        Ok(())
    }
    pub(super) fn delete_view(&mut self, scope: &Scope, values: &[String]) -> Result<(), String> {
        self.check_view_scope(scope)?;
        let [name, confirm] = values else {
            return Err("Invalid delete-view form".into());
        };
        if confirm != "Delete locally" {
            return Err("Choose Delete locally to confirm, or Esc to cancel".into());
        }
        store(&self.root, scope, name, None)?;
        let at = self.page - 1;
        if self.active_views[at]
            .as_ref()
            .is_some_and(|view| view.name == *name)
        {
            // Deleting a preference does not discard the query/results currently being read.
            self.active_views[at] = None;
        }
        self.notice_error = false;
        self.notice = format!("View deleted locally: {name}; current query unchanged");
        Ok(())
    }
    pub(super) fn remember_list_selection(&mut self) {
        if matches!(self.page, 1 | 2)
            && let Some(item) = self.items().get(self.selected[self.page - 1])
        {
            let selection = (self.row_identity(item), self.selected[self.page - 1]);
            let key = self.list_key();
            self.list_selections.insert(key, selection);
        }
    }
    pub(super) fn restore_list_selection(&mut self) {
        let at = self.page - 1;
        self.selected[at] = self
            .list_selections
            .get(&self.list_key())
            .map(|(id, fallback)| {
                self.items()
                    .iter()
                    .position(|item| self.row_identity(item) == *id)
                    .unwrap_or_else(|| (*fallback).min(self.items().len().saturating_sub(1)))
            })
            .unwrap_or(0);
    }
    pub(super) fn apply_view(
        &mut self,
        query: String,
        state: String,
        view: Option<SavedView>,
    ) -> Result<(), String> {
        validate_filter(self.page, &query, &state)?;
        self.remember_list_selection();
        let at = self.page - 1;
        self.queries[at] = query;
        self.states[at] = state;
        self.active_views[at] = view;
        self.restore_list_selection();
        self.opened = None;
        self.inbox.origin_page = None;
        self.selected_commit = None;
        self.focus = 0;
        self.last_list = 0;
        self.detail_cursor = 0;
        self.detail_scroll = 0;
        self.horizontal = 0;
        self.detail_rows.clear();
        self.reveal_file = self.page == 2 && self.pr_tab == 3;
        self.notice.clear();
        self.notice_error = false;
        self.ensure(true);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_views_validate_canonical_scope_and_preserve_every_other_scope() {
        let owner = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(owner.path()).unwrap();
        let scopes = [
            Scope::new("GitHub.COM.", "Example/Demo", 1).unwrap(),
            Scope::new("github.com", "example/demo", 2).unwrap(),
            Scope::new("github.com", "other/demo", 1).unwrap(),
            Scope::new("git.example.test", "example/demo", 1).unwrap(),
        ];
        for scope in &scopes {
            let view =
                SavedView::new(scope, "Triage 界 🦀", "label:bug assignee:@me", "closed").unwrap();
            store(owner.path(), scope, &view.name, Some(view.clone())).unwrap();
        }
        let view = SavedView::new(&scopes[0], "Triage 界 🦀", "author:@me", "all").unwrap();
        store(owner.path(), &scopes[0], &view.name, Some(view.clone())).unwrap();
        let saved = load(owner.path()).unwrap();
        assert_eq!(saved.len(), 4);
        assert_eq!(
            saved
                .iter()
                .find(|view| scopes[0].contains(view))
                .unwrap()
                .query,
            "author:@me"
        );
        store(owner.path(), &scopes[0], &view.name, None).unwrap();
        assert_eq!(load(owner.path()).unwrap().len(), 3);
        let mut local = repo
            .config()
            .unwrap()
            .open_level(git2::ConfigLevel::Local)
            .unwrap();
        local.set_str(CONFIG_KEY, "not valid JSON").unwrap();
        assert!(
            store(owner.path(), &scopes[0], &view.name, Some(view.clone()))
                .unwrap_err()
                .contains("inspect/repair")
        );
        assert_eq!(local.get_string(CONFIG_KEY).unwrap(), "not valid JSON");
    }

    #[test]
    fn cached_view_selection_pending_refresh_and_repository_context_are_isolated() {
        let owner = tempfile::tempdir().unwrap();
        git2::Repository::init(owner.path()).unwrap();
        let mut workspace = Workspace::new(owner.path().into());
        workspace.page = 1;
        workspace.change_repository("example/demo".into(), "github.com".into());
        let scope = workspace.view_scope().unwrap();
        let first = SavedView::new(&scope, "Triage 界", "label:bug", "all").unwrap();
        let second = SavedView::new(&scope, "Inbox", "", "open").unwrap();
        for view in [&first, &second] {
            store(owner.path(), &scope, &view.name, Some(view.clone())).unwrap();
        }
        let values = serde_json::json!([{"number":7}, {"number":8}]);
        workspace.cache.insert(
            Key::Repository,
            Ok(Reply {
                value: Value::Null,
                text: String::new(),
            }),
        );
        for key in [
            Key::List(1, "all".into(), "label:bug".into()),
            workspace.list_key(),
        ] {
            workspace.cache.insert(
                key,
                Ok(Reply {
                    value: values.clone(),
                    text: values.to_string(),
                }),
            );
        }
        workspace.choose_view("Saved: Triage 界").unwrap();
        workspace.selected[0] = 1;
        workspace.choose_view("Saved: Inbox").unwrap();
        workspace.cycle_view(false).unwrap();
        assert_eq!(workspace.number(), 8);
        assert!(workspace.pending(&workspace.list_key()));
        assert_eq!(workspace.view_name(), "Triage 界");
        let list_key = workspace.list_key();
        workspace.page(2);
        let reordered = serde_json::json!([{"number":8}, {"number":7}]);
        workspace.cache.insert(
            list_key,
            Ok(Reply {
                value: reordered.clone(),
                text: reordered.to_string(),
            }),
        );
        workspace.page(1);
        assert_eq!(
            workspace.number(),
            8,
            "off-page refreshes must restore identity, not the old index"
        );
        workspace
            .submit(
                Action::Filter,
                vec!["label:bug author:@me".into(), "all".into()],
            )
            .unwrap();
        assert_eq!(workspace.view_name(), "Triage 界 *");
        let action = Action::SaveView(scope.clone(), Some(first.name.clone()));
        assert!(action.writes());
        assert!(Action::DeleteView(scope.clone()).writes());
        workspace.save_draft(
            &action,
            vec!["Draft 界".into(), "draft query".into(), "closed".into()],
        );
        assert_eq!(
            workspace.save_view_form().unwrap().fields[0].input.input,
            "Draft 界"
        );
        workspace.page = 2;
        assert!(
            workspace.save_view_form().unwrap().fields[0]
                .input
                .input
                .is_empty()
        );
        assert_eq!(workspace.views_form().unwrap().fields[0].choices.len(), 5);
        assert!(
            workspace
                .save_view(&scope, &["wrong page".into(), "".into(), "open".into()])
                .is_err()
        );
        workspace.change_repository("other/repository".into(), "github.com".into());
        assert!(workspace.cache.is_empty());
        assert!(workspace.list_selections.is_empty());
        assert!(workspace.active_views.iter().all(Option::is_none));
        assert!(
            workspace.save_view_form().unwrap().fields[0]
                .input
                .input
                .is_empty()
        );
        workspace.change_repository("example/demo".into(), "git.example.test".into());
        workspace.page = 1;
        assert_eq!(workspace.views_form().unwrap().fields[0].choices.len(), 4);
        assert!(
            workspace.save_view_form().unwrap().fields[0]
                .input
                .input
                .is_empty()
        );
        assert_eq!(load(owner.path()).unwrap().len(), 2);
        assert!(
            workspace
                .queue
                .iter()
                .all(|request| !matches!(request.key, Key::Write | Key::WorktreeWrite(_)))
        );
    }

    #[test]
    fn config_lock_and_ambiguous_lists_preserve_existing_preferences() {
        let owner = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(owner.path()).unwrap();
        let scope = Scope::new("github.com", "example/demo", 2).unwrap();
        let view = SavedView::new(&scope, "Review 🦀", "review-requested:@me", "merged").unwrap();
        store(owner.path(), &scope, &view.name, Some(view.clone())).unwrap();
        let before = std::fs::read(repo.commondir().join("config")).unwrap();
        let lock = repo.commondir().join("config.lock");
        std::fs::write(&lock, b"fixture-owned lock").unwrap();
        assert!(
            store(owner.path(), &scope, &view.name, None)
                .unwrap_err()
                .contains("inspect/repair")
        );
        assert_eq!(
            std::fs::read(repo.commondir().join("config")).unwrap(),
            before
        );
        std::fs::remove_file(lock).unwrap();
        let mut config = local_config(owner.path()).unwrap();
        config
            .set_multivar(CONFIG_KEY, "^never-match$", "[]")
            .unwrap();
        assert!(
            load(owner.path())
                .unwrap_err()
                .contains("multiple saved-view lists")
        );
        let before = std::fs::read(repo.commondir().join("config")).unwrap();
        assert!(store(owner.path(), &scope, &view.name, Some(view.clone())).is_err());
        assert_eq!(
            std::fs::read(repo.commondir().join("config")).unwrap(),
            before
        );
        // A value which libgit2 cannot decode is not equivalent to a missing key.
        let unreadable = b"[dusty-git]\n\tgithubViews = \"\xff\"\n";
        std::fs::write(repo.commondir().join("config"), unreadable).unwrap();
        assert!(load(owner.path()).unwrap_err().contains("inspect/repair"));
        assert!(store(owner.path(), &scope, &view.name, Some(view.clone())).is_err());
        assert_eq!(
            std::fs::read(repo.commondir().join("config")).unwrap(),
            unreadable
        );
    }

    #[test]
    fn invalid_records_are_not_silently_replaced() {
        let scope = Scope::new("github.com", "example/demo", 1).unwrap();
        for (name, query, state) in [
            ("", "", "open"),
            ("x\n", "", "merged"),
            ("ok", "bad\0query", "open"),
        ] {
            assert!(SavedView::new(&scope, name, query, state).is_err());
        }
        let view =
            SavedView::new(&scope, "With spaces 日本語", "label:\"help wanted\"", "all").unwrap();
        let text = serde_json::to_string(&vec![view.clone(), view]).unwrap();
        assert!(parse(&text).unwrap_err().contains("duplicate"));
        for text in ["null", "{}", "[] broken", "[{\"name\":\"incomplete\"}]"] {
            assert!(parse(text).is_err());
        }
    }
}
