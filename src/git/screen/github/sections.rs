//! The sections row of a forge list (DESIGN.md §3.26/§3.33): built-in triage sections, then
//! the saved views of this repository, each with a count. `[`/`]` move between them.
use super::views::SavedView;
use super::*;

pub(super) struct Section {
    pub name: String,
    pub query: String,
    pub state: String,
    pub saved: Option<SavedView>,
}

/// Bug and feature labels by convention: GitHub's defaults, Gitea/Forgejo's `Kind/*`, and
/// GitLab's scoped `type::`/`kind::` labels. The first one the repository uses names a bucket.
const BUG_LABELS: [&str; 6] = ["bug", "kind/bug", "type::bug", "kind::bug", "type/bug", "bugfix"];
const FEATURE_LABELS: [&str; 10] = [
    "enhancement",
    "feature",
    "kind/feature",
    "kind/enhancement",
    "type::feature",
    "type::enhancement",
    "kind::feature",
    "type/feature",
    "feature request",
    "new feature",
];
const PULLS: [(&str, &str); 4] = [
    ("Review", "review-requested:@me"),
    ("Mine", "author:@me"),
    ("Drafts", "draft:true"),
    ("All", ""),
];

impl Workspace {
    pub(super) fn sections(&self) -> Vec<Section> {
        let builtins: Vec<(String, String)> = if self.page == 2 {
            PULLS.iter().map(|(name, query)| ((*name).into(), (*query).into())).collect()
        } else {
            self.issue_buckets()
        };
        let mut sections = builtins
            .into_iter()
            .map(|(name, query)| Section {
                name,
                query,
                state: "open".into(),
                saved: None,
            })
            .collect::<Vec<_>>();
        // Saved views follow the scope: the repository's own, or the host's inbox views.
        for view in self.scoped_views().unwrap_or_default() {
            sections.push(Section {
                name: view.name.clone(),
                query: view.query.clone(),
                state: view.state.clone(),
                saved: Some(view),
            });
        }
        sections
    }

    /// All, then Bugs/Features/Other from the labels this repository uses (`Kind/Bug` on
    /// Forgejo, `type::bug` on GitLab, `bug` on GitHub). A bucket no open issue can fall in is
    /// left out rather than shown as a permanent 0.
    fn issue_buckets(&self) -> Vec<(String, String)> {
        let mut buckets = vec![("All".to_owned(), String::new())];
        let used = self.list_rows_for("open", "").map(|rows| {
            rows.iter()
                .flat_map(|row| array(row, "labels").iter().map(|label| string(label, "name").to_owned()))
                .collect::<std::collections::BTreeSet<_>>()
        });
        let pick = |candidates: &[&str]| -> Option<String> {
            match &used {
                Some(used) => candidates.iter().find_map(|candidate| {
                    used.iter()
                        .find(|name| name.eq_ignore_ascii_case(candidate) && !name.contains(' '))
                        .cloned()
                }),
                // Until the list arrives, the provider's convention stands in.
                None => Some(
                    match self.forge_kind() {
                        Some(crate::forge::provider::ProviderKind::Gitea | crate::forge::provider::ProviderKind::Forgejo) => {
                            if candidates[0] == "bug" { "Kind/Bug" } else { "Kind/Feature" }
                        }
                        _ => candidates[0],
                    }
                    .to_owned(),
                ),
            }
        };
        let bug = pick(&BUG_LABELS);
        let feature = pick(&FEATURE_LABELS);
        let mut other = String::new();
        for (name, label) in [("Bugs", &bug), ("Features", &feature)] {
            if let Some(label) = label {
                buckets.push((name.to_owned(), format!("label:{label}")));
                other.push_str(&format!("{}-label:{label}", if other.is_empty() { "" } else { " " }));
            }
        }
        if !other.is_empty() {
            buckets.push(("Other".to_owned(), other));
        }
        buckets
    }

    /// The section the list shows now; `None` for a custom filter.
    pub(super) fn active_section(&self, sections: &[Section]) -> Option<usize> {
        let at = self.page.saturating_sub(1).min(1);
        if let Some(view) = &self.active_views[at]
            && let Some(index) = sections
                .iter()
                .position(|section| section.saved.as_ref().is_some_and(|saved| saved.name == view.name))
        {
            return Some(index);
        }
        sections.iter().position(|section| {
            section.saved.is_none()
                && section.query == self.queries[at]
                && section.state == self.states[at]
        })
    }

    fn section_key(&self, state: &str, query: &str) -> Key {
        if self.inbox.enabled {
            Key::Inbox(self.host.clone(), self.page, state.into(), query.into())
        } else {
            Key::List(self.page, state.into(), query.into())
        }
    }

    fn list_rows_for(&self, state: &str, query: &str) -> Option<&Vec<Value>> {
        self.value(&self.section_key(state, query)).and_then(Value::as_array)
    }

    /// Lists of the sections whose count the shown list cannot give (`@me`, forge-only
    /// qualifiers): read in the background so every tab carries a number.
    pub(super) fn section_keys(&self) -> Vec<Key> {
        // Only the inbox reads sections behind the shown list; elsewhere a list read for
        // a section that is no longer shown is pruned like any other.
        if !matches!(self.page, 1 | 2) || !self.inbox.enabled {
            return Vec::new();
        }
        self.sections()
            .iter()
            .filter(|section| !self.locally_countable(&section.query))
            .map(|section| self.section_key(&section.state, &section.query))
            .filter(|key| *key != self.list_key())
            .collect()
    }

    pub(super) fn section_reads(&self) -> Vec<(Key, Op)> {
        self.sections()
            .iter()
            .filter(|section| !self.locally_countable(&section.query))
            .filter(|section| self.list_rows_for(&section.state, &section.query).is_none())
            .map(|section| {
                (
                    self.section_key(&section.state, &section.query),
                    self.list_read_for(&section.state, &section.query, LIST_BATCH),
                )
            })
            .filter(|(key, _)| *key != self.list_key())
            .collect()
    }

    /// Whether the cached "all open" list answers this query (the local matcher knows it).
    fn locally_countable(&self, query: &str) -> bool {
        !query.contains("@me")
            && !query.split_whitespace().any(|term| {
                let term = term.trim_start_matches('-');
                term.split_once(':').is_some_and(|(name, _)| {
                    !matches!(name, "label" | "is" | "state" | "draft" | "no" | "author")
                })
            })
    }

    /// A section's count: its own cached list, else what the cached "all open" list says
    /// for qualifiers the local search understands. `None` when nothing is known.
    pub(super) fn section_count(&self, section: &Section) -> Option<usize> {
        if let Some(rows) = self.list_rows_for(&section.state, &section.query) {
            return Some(rows.iter().filter(|row| row["dustyGone"].is_null()).count());
        }
        if !self.locally_countable(&section.query) {
            return None;
        }
        let all = self.list_rows_for("open", "")?;
        Some(
            all.iter()
                .filter(|row| row["dustyGone"].is_null() && fresh::matches(row, &section.query, &section.state))
                .count(),
        )
    }

    /// The count on the top bar's tab: open items in the repository, when known.
    pub(in crate::git::screen) fn tab_count(&self, page: usize) -> Option<usize> {
        if self.repo.is_empty() || self.inbox.enabled {
            return None;
        }
        self.value(&Key::List(page, "open".into(), String::new()))
            .and_then(Value::as_array)
            .map(|rows| rows.iter().filter(|row| row["dustyGone"].is_null()).count())
    }

    /// What this host's forge can do (FORGE.md §4.2); unavailable actions say why.
    pub(super) fn capabilities(&self) -> crate::forge::capability::Capabilities {
        use crate::forge::provider::{ProviderKind, gitea, gitlab};
        let caps = match self.forge_kind() {
            Some(ProviderKind::GitLab) => gitlab::capabilities(false),
            Some(kind @ (ProviderKind::Gitea | ProviderKind::Forgejo)) => {
                gitea::capabilities(gitea::Server::Unknown(kind))
            }
            _ => crate::forge::capability::Capabilities::github(true),
        };
        // Browsing without a credential: every write says how to lift it (FORGE.md §2.5).
        if self.read_only() {
            caps.read_only(self.forge_kind().unwrap_or(ProviderKind::GitHub))
        } else {
            caps
        }
    }

    /// This host is browsed anonymously: reads only.
    pub(in crate::git::screen) fn read_only(&self) -> bool {
        self.host_state().is_some_and(|state| state.read_only())
    }

    /// The registry's state for this host, once any request reached it.
    pub(super) fn host_state(&self) -> Option<crate::forge::host_state::HostState> {
        if self.host.is_empty() {
            return None;
        }
        crate::forge::host_state::account(&crate::forge::transport::HostKey::new(&self.host)).map(|account| account.state)
    }

    /// Read-only hosts keep their write rows visible, dimmed and saying why
    /// (`› Merge readiness  m · Sign in to merge`); activating one repeats the reason.
    pub(super) fn mark_refused(&self, rows: &mut [super::lines::Line]) {
        if !self.read_only() {
            return;
        }
        for row in rows {
            let Some(reason) = row.action.as_ref().and_then(|action| self.refusal(action)) else {
                continue;
            };
            for (_, style) in &mut row.paint {
                if let crate::ui::Style::Git { role, emphasis, .. } = style {
                    *role = crate::ui::GitRole::Muted;
                    *emphasis = crate::ui::Emphasis::None;
                }
            }
            let start = row.text.len();
            row.text.push_str(&format!("  · {reason}"));
            let end = row.text.len();
            row.paint.push((start..end, crate::ui::Style::Git {
                role: crate::ui::GitRole::Modified,
                surface: crate::ui::GitSurface::Pane,
                emphasis: crate::ui::Emphasis::None,
            }));
        }
    }

    /// Why `action` is refused on this host, from its capabilities; `None` when it may run.
    pub(super) fn refusal(&self, action: &Action) -> Option<&'static str> {
        use crate::forge::capability::Cap;
        if !self.read_only() {
            return None;
        }
        let caps = self.capabilities();
        let reason = |cap: Cap| cap.reason();
        match action {
            Action::Scoped { action, .. } => self.refusal(action),
            Action::Review(_) => reason(caps.review_approve),
            Action::Inline { .. } => reason(caps.inline_comment),
            Action::Reply { .. } | Action::Comment(..) => reason(caps.reply_in_thread),
            Action::Resolve(..) => reason(caps.resolve_thread),
            Action::Merge(..) => reason(caps.merge_pinned_head),
            Action::Viewed(..) => reason(caps.viewed_files),
            Action::Subscription(..) => reason(caps.subscribe),
            Action::Edit(..) | Action::State(..) | Action::Metadata(..) => Some("Sign in to edit"),
            Action::CreateIssue => Some("Sign in to open issues"),
            Action::CreatePullRequest | Action::ConfirmPullRequest(_) => reason(caps.draft_pr),
            Action::Dispatch(_) => reason(caps.dispatch),
            Action::Cancel(_) => reason(caps.cancel_run),
            Action::Rerun(_) => reason(caps.rerun_all),
            Action::Develop(_) => reason(caps.writes),
            // Local Git (checkout, worktrees, branches) needs no sign-in.
            _ => None,
        }
    }

    pub(super) fn cycle_section(&mut self, forward: bool) -> Result<(), String> {
        let sections = self.sections();
        if sections.is_empty() {
            return Ok(());
        }
        let count = sections.len();
        let next = match self.active_section(&sections) {
            Some(at) => (at + if forward { 1 } else { count - 1 }) % count,
            None => 0,
        };
        let section = sections.into_iter().nth(next).expect("section in range");
        self.apply_view(section.query, section.state, section.saved)
    }
}
