//! The sections row of a forge list: built-in triage sections, then
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
const BUG_LABELS: [&str; 6] = [
    "bug",
    "kind/bug",
    "type::bug",
    "kind::bug",
    "type/bug",
    "bugfix",
];
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
            PULLS
                .iter()
                .map(|(name, query)| ((*name).into(), (*query).into()))
                .collect()
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
                .flat_map(|row| {
                    array(row, "labels")
                        .iter()
                        .map(|label| string(label, "name").to_owned())
                })
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
                        Some(
                            crate::forge::provider::ProviderKind::Gitea
                            | crate::forge::provider::ProviderKind::Forgejo,
                        ) => {
                            if candidates[0] == "bug" {
                                "Kind/Bug"
                            } else {
                                "Kind/Feature"
                            }
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
                other.push_str(&format!(
                    "{}-label:{label}",
                    if other.is_empty() { "" } else { " " }
                ));
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
            && let Some(index) = sections.iter().position(|section| {
                section
                    .saved
                    .as_ref()
                    .is_some_and(|saved| saved.name == view.name)
            })
        {
            return Some(index);
        }
        // A built-in section is its query; the Open/Closed toggle (`s`) keeps it selected,
        // as GitHub's filters keep through `is:open` and `is:closed`.
        sections
            .iter()
            .position(|section| section.saved.is_none() && section.query == self.queries[at])
    }

    /// The search box's query (GitHub's `is:pr is:open review-requested:@me`): the kind,
    /// the state unless it is every state, then the section's or typed query.
    pub(super) fn search_query(&self) -> String {
        let at = self.page.saturating_sub(1).min(1);
        let mut terms = vec![if self.page == 2 { "is:pr" } else { "is:issue" }.to_owned()];
        match self.states[at].as_str() {
            "all" => {}
            state => terms.push(format!("is:{state}")),
        }
        let query = self.queries[at].trim();
        if !query.is_empty() {
            terms.push(query.to_owned());
        }
        terms.join(" ")
    }

    /// How many items the shown query has in `state` (`open`, `closed`), once that list was
    /// read; `100+` when the list stopped at its limit.
    pub(super) fn state_count(&self, state: &str) -> Option<String> {
        let at = self.page.saturating_sub(1).min(1);
        let key = self.section_key(state, &self.queries[at]);
        let total = self.list_totals.get(&key).copied();
        let Some(rows) = self.value(&key).and_then(Value::as_array) else {
            // Not read yet: what the other state's read counted (GitHub), else lazy.
            return total.map(|total| total.to_string());
        };
        let count = rows.iter().filter(|row| row["dustyGone"].is_null()).count();
        let more = self
            .list_limits
            .get(&key)
            .is_some_and(|limit| rows.len() >= *limit);
        // A whole list counts its own rows; a list cut at its limit takes the forge's total.
        Some(match total.filter(|total| more && *total > count as u64) {
            Some(total) => total.to_string(),
            None => format!("{count}{}", if more { "+" } else { "" }),
        })
    }

    /// Remember what the forge counted for a list read: its own total, and the other
    /// state's (open ↔ closed) where the forge counted it in the same request.
    pub(super) fn note_totals(&mut self, key: &Key, payload: &crate::forge::request::Payload) {
        let (Key::List(page, state, query), crate::forge::request::Payload::Rows(rows)) =
            (key, payload)
        else {
            return;
        };
        match rows.total {
            Some(total) => self.list_totals.insert(key.clone(), total),
            None => self.list_totals.remove(key),
        };
        let other = match state.as_str() {
            "open" => "closed",
            "closed" | "merged" => "open",
            _ => return,
        };
        let other = Key::List(*page, other.into(), query.clone());
        match rows.other_total {
            Some(total) => self.list_totals.insert(other, total),
            // Only a read that counted can say; an older count stays until it is read.
            None => None,
        };
    }

    /// The filter menus whose qualifier the query uses (`author`, `label`, …, `sort`).
    pub(super) fn facets_in_use(&self) -> Vec<&'static str> {
        let at = self.page.saturating_sub(1).min(1);
        super::paint::FACETS
            .iter()
            .map(|(_, qualifier)| *qualifier)
            .filter(|qualifier| {
                self.queries[at]
                    .split_whitespace()
                    .any(|term| facet_of(term) == Some(*qualifier))
            })
            .collect()
    }

    /// `s`: the Open/Closed toggle. The section (or saved view) stays; only the state turns.
    pub(super) fn toggle_state(&mut self) -> Result<(), String> {
        let at = self.page.saturating_sub(1).min(1);
        // GitLab's merge requests step Open → Merged → Closed (R16 gitlab-05).
        let gitlab = self.page == 2 && self.terms().pr_singular.contains("merge");
        let state = match self.states[at].as_str() {
            "open" if gitlab => "merged",
            "merged" if gitlab => "closed",
            "open" => "closed",
            _ => "open",
        };
        let view = self.active_views[at].clone();
        self.apply_view(self.queries[at].clone(), state.into(), view)
    }

    /// `F`: the filter menus as one sheet, a field per menu (`Author`, `Label`, `Projects`,
    /// `Milestones`, `Assignee`, `Sort`), each offering what the list holds and taking any
    /// typed value.
    pub(super) fn facets_form(&self) -> Form {
        let at = self.page.saturating_sub(1).min(1);
        let query = &self.queries[at];
        let rows = self.items();
        let mut authors = std::collections::BTreeSet::new();
        let mut labels = std::collections::BTreeSet::new();
        for row in &rows {
            let author = login(row);
            if !author.is_empty() {
                authors.insert(author.to_owned());
            }
            for label in array(row, "labels") {
                labels.insert(string(label, "name").to_owned());
            }
        }
        let quoted = |value: &str| {
            if value.contains(' ') {
                format!("\"{value}\"")
            } else {
                value.to_owned()
            }
        };
        let mut fields = Vec::new();
        for (name, qualifier) in super::paint::FACETS {
            let mut choices: Vec<String> = vec![ANY.into()];
            match qualifier {
                "author" => {
                    choices.push("@me".into());
                    choices.extend(authors.iter().cloned());
                }
                "label" => {
                    choices.push(NONE.into());
                    choices.extend(labels.iter().map(|label| quoted(label)));
                }
                "assignee" => choices.extend([NONE.into(), "@me".into()]),
                "project" | "milestone" => choices.push(NONE.into()),
                _ => {
                    choices = SORTS.iter().map(|(words, _)| (*words).to_owned()).collect();
                }
            }
            let current = facet_value(query, qualifier).unwrap_or_else(|| choices[0].clone());
            let mut field = super::actions::field(name, &current, false, false);
            field.choices = choices;
            field.custom_choices = true;
            fields.push(field);
        }
        self.form(
            if self.page == 2 {
                "Filter pull requests"
            } else {
                "Filter issues"
            },
            Action::Facets,
            fields,
        )
    }

    /// The query with each menu's qualifier replaced by the sheet's answer.
    pub(super) fn apply_facets(&mut self, values: &[String]) -> Result<(), String> {
        let at = self.page.saturating_sub(1).min(1);
        let mut terms: Vec<String> = self.queries[at]
            .split_whitespace()
            .filter(|term| facet_of(term).is_none())
            .map(str::to_owned)
            .collect();
        for ((_, qualifier), value) in super::paint::FACETS.iter().zip(values) {
            let value = value.trim();
            if value.is_empty() || value == ANY {
                continue;
            }
            terms.push(match (*qualifier, value) {
                ("sort", words) => match SORTS.iter().find(|(name, _)| *name == words) {
                    Some((_, "")) => continue,
                    Some((_, order)) => format!("sort:{order}"),
                    None => format!("sort:{words}"),
                },
                (qualifier, NONE) => format!("no:{qualifier}"),
                (qualifier, value) if value.contains(' ') && !value.starts_with('"') => {
                    format!("{qualifier}:\"{value}\"")
                }
                (qualifier, value) => format!("{qualifier}:{value}"),
            });
        }
        let state = self.states[at].clone();
        self.apply_view(terms.join(" "), state, None)
    }

    fn section_key(&self, state: &str, query: &str) -> Key {
        if self.inbox.enabled {
            Key::Inbox(self.host.clone(), self.page, state.into(), query.into())
        } else {
            Key::List(self.page, state.into(), query.into())
        }
    }

    fn list_rows_for(&self, state: &str, query: &str) -> Option<&Vec<Value>> {
        self.value(&self.section_key(state, query))
            .and_then(Value::as_array)
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
        let all = self.list_rows_for(&section.state, "")?;
        Some(
            all.iter()
                .filter(|row| {
                    row["dustyGone"].is_null()
                        && fresh::matches(row, &section.query, &section.state)
                })
                .count(),
        )
    }

    /// Every section's count, or none (r11): a row never mixes counted and bare tabs, and
    /// what it shows never depends on which section is selected or was visited. The
    /// repository budget is one list read per visit, so where a section needs its own
    /// read (`@me`) the row is names only; the inbox reads them all behind the list.
    /// The counts follow the Open/Closed toggle, which keeps the section selected; a typed
    /// query no section claims leaves the row names only (R16 issues-05: never `All 10`
    /// over `0 Closed`).
    pub(super) fn section_counts(&self, sections: &[Section]) -> Vec<Option<usize>> {
        if self.active_section(sections).is_none() {
            return vec![None; sections.len()];
        }
        let state = &self.states[self.page.saturating_sub(1).min(1)];
        let sections = sections
            .iter()
            .map(|section| Section {
                name: section.name.clone(),
                query: section.query.clone(),
                state: if section.saved.is_none() {
                    state.clone()
                } else {
                    section.state.clone()
                },
                saved: section.saved.clone(),
            })
            .collect::<Vec<_>>();
        let sections = sections.as_slice();
        let all_known = (self.inbox.enabled
            || sections
                .iter()
                .all(|section| self.locally_countable(&section.query)))
            && sections
                .iter()
                .all(|section| self.section_count(section).is_some());
        sections
            .iter()
            .map(|section| {
                if all_known {
                    self.section_count(section)
                } else {
                    None
                }
            })
            .collect()
    }

    /// The count on the top bar's tab: open items in the repository, when known.
    pub(in crate::screen) fn tab_count(&self, page: usize) -> Option<usize> {
        if self.repo.is_empty() || self.inbox.enabled {
            return None;
        }
        self.value(&Key::List(page, "open".into(), String::new()))
            .and_then(Value::as_array)
            .map(|rows| rows.iter().filter(|row| row["dustyGone"].is_null()).count())
    }

    /// What this host's forge can do; unavailable actions say why.
    pub(super) fn capabilities(&self) -> crate::forge::capability::Capabilities {
        use crate::forge::provider::{ProviderKind, gitea, gitlab};
        let caps = match self.forge_kind() {
            Some(ProviderKind::GitLab) => gitlab::capabilities(false),
            Some(kind @ (ProviderKind::Gitea | ProviderKind::Forgejo)) => {
                gitea::capabilities(gitea::Server::Unknown(kind))
            }
            _ => crate::forge::capability::Capabilities::github(true),
        };
        // Browsing without a credential: every write says how to lift it.
        if self.read_only() {
            caps.read_only(self.forge_kind().unwrap_or(ProviderKind::GitHub))
        } else {
            caps
        }
    }

    /// This host is browsed anonymously: reads only.
    pub(in crate::screen) fn read_only(&self) -> bool {
        self.host_state().is_some_and(|state| state.read_only())
    }

    /// The registry's state for this host, once any request reached it.
    pub(super) fn host_state(&self) -> Option<crate::forge::host_state::HostState> {
        if self.host.is_empty() {
            return None;
        }
        crate::forge::host_state::account(&crate::forge::transport::HostKey::new(&self.host))
            .map(|account| account.state)
    }

    /// Read-only hosts keep their write rows visible, dimmed and saying why
    /// (`› Merge readiness  m · Sign in to merge`); activating one repeats the reason.
    pub(super) fn mark_refused(&self, rows: &mut [super::lines::Line]) {
        if !self.read_only() {
            return;
        }
        for row in rows {
            // Every diff line takes an inline comment: saying why on each would bury the code.
            // The key still says it once, when pressed.
            if matches!(row.action, Some(Action::Inline { .. })) {
                continue;
            }
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
            row.paint.push((
                start..end,
                crate::ui::Style::Git {
                    role: crate::ui::GitRole::Modified,
                    surface: crate::ui::GitSurface::Pane,
                    emphasis: crate::ui::Emphasis::None,
                },
            ));
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
            Action::CommitSuggestion(..) => reason(caps.writes),
            Action::Merge(..) => reason(caps.merge_pinned_head),
            Action::Viewed(..) => reason(caps.viewed_files),
            Action::Edit(..) | Action::State(..) | Action::Metadata(..) => Some("Sign in to edit"),
            Action::CreateIssue => Some("Sign in to create issues"),
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

/// A filter menu's answer that removes its qualifier.
const ANY: &str = "Any";
/// A filter menu's answer for `no:label`, `no:assignee`, `no:milestone`, `no:project`.
const NONE: &str = "None";
/// GitHub's sort menu, as words and `sort:` orders (the first is the forge's default).
pub(super) const SORTS: [(&str, &str); 6] = [
    ("Newest", ""),
    ("Oldest", "created-asc"),
    ("Most commented", "comments-desc"),
    ("Least commented", "comments-asc"),
    ("Recently updated", "updated-desc"),
    ("Least recently updated", "updated-asc"),
];

/// Which filter menu a query term belongs to (`label:bug`, `-label:bug`, `no:label` → `label`).
fn facet_of(term: &str) -> Option<&'static str> {
    let term = term.strip_prefix('-').unwrap_or(term);
    let (name, value) = term.split_once(':')?;
    let name = if name == "no" { value } else { name };
    super::paint::FACETS
        .iter()
        .map(|(_, qualifier)| *qualifier)
        .find(|qualifier| *qualifier == name || (*qualifier == "project" && name == "projects"))
}

/// A menu's current answer in a query: its value, `None` for `no:…`, the sort's words.
fn facet_value(query: &str, qualifier: &str) -> Option<String> {
    for term in query.split_whitespace() {
        if term.starts_with('-') || facet_of(term) != Some(qualifier) {
            continue;
        }
        let (name, value) = term.split_once(':')?;
        if name == "no" {
            return Some(NONE.into());
        }
        if qualifier == "sort" {
            return Some(
                SORTS
                    .iter()
                    .find(|(_, order)| *order == value)
                    .map_or(value.to_owned(), |(words, _)| (*words).to_owned()),
            );
        }
        return Some(value.to_owned());
    }
    None
}
