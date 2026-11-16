//! F-ISSUES / F-ISSUEDISC reader rows: one Conversation page as on
//! GitHub (`conversation.rs`), and the helpers it shares with pull requests: link rows, the
//! one-line events, the unavailable banner.
use super::super::shell::glyphs::{Glyph, glyph};
use super::lines::Line;
use super::prose::{self, Rich, blank, muted, role, strong};
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::records::{self, DetailRecord, LinkRecord, TimelineEntry};
use crate::ui::{GitRole, GitSurface};

/// Opens a linked issue or pull request in place (or in its own repository from the inbox).
pub(super) fn link_action(workspace: &Workspace, link: &LinkRecord, page: usize) -> Option<Action> {
    if workspace.inbox.enabled
        || !link.repo.is_empty() && !link.repo.eq_ignore_ascii_case(&workspace.repo)
    {
        let target = inbox::Target::from_url(&link.url, &workspace.host, page, link.number).ok()?;
        return Some(Action::OpenRemote(target));
    }
    Some(Action::OpenItem(page, link.number))
}

/// A detail read that failed: what is shown instead, why, and how to fix it.
pub(super) fn unavailable(workspace: &Workspace, rows: &mut Vec<Line>, error: &str, refresh: &str) {
    // A banner above the body: the failure and the way out on a tinted band, then
    // the list's description underneath, untouched.
    // In words with one key: the reason, then its hint; `R` retries everywhere.
    let _ = refresh;
    let words = workspace.explain(error);
    let mut head = Rich::new()
        .plain(" ")
        .push("✕ ", strong(GitRole::Removed))
        .push("Details unavailable", strong(GitRole::Removed))
        .sep()
        .push(&words.headline, role(GitRole::Text))
        .line();
    head.band = Some(GitSurface::ReceiptErr);
    rows.push(head);
    let mut hint = Rich::new()
        .plain("   ")
        .push(&words.hint, role(GitRole::Text))
        .push(" · below: the list's description", role(GitRole::Muted))
        .line();
    hint.band = Some(GitSurface::ReceiptErr);
    rows.push(hint);
}

/// How this forge's own CLI signs in, and the token variable Dusty reads instead.
pub(super) fn sign_in(
    kind: Option<crate::forge::provider::ProviderKind>,
) -> (&'static str, &'static str) {
    use crate::forge::provider::ProviderKind;
    match kind {
        Some(ProviderKind::GitLab) => ("glab auth login", "GITLAB_TOKEN"),
        Some(ProviderKind::Gitea) => ("tea login add", "GITEA_TOKEN"),
        Some(ProviderKind::Forgejo) => ("fj auth login", "FORGEJO_TOKEN"),
        Some(ProviderKind::GitHub) | None => ("gh auth login", "GH_TOKEN"),
    }
}

fn loading_or_error(workspace: &Workspace, key: &Key, rows: &mut Vec<Line>) -> bool {
    match workspace.cache.get(key) {
        Some(Ok(_)) => false,
        Some(Err(error)) => {
            let words = workspace.explain(error);
            rows.push(
                Rich::new()
                    .push("✕ ", role(GitRole::Removed))
                    .push(&words.headline, role(GitRole::Removed))
                    .line(),
            );
            rows.push(muted(&words.hint));
            true
        }
        None => {
            rows.push(muted("Loading…"));
            true
        }
    }
}

/// Every comment and event of the issue, oldest first.
pub(super) fn entries(workspace: &Workspace, id: u64) -> Vec<TimelineEntry> {
    workspace
        .value(&Key::Timeline(id))
        .map(records::timeline)
        .unwrap_or_default()
}

pub(super) fn discussion_count(workspace: &Workspace, id: u64, record: &DetailRecord) -> u64 {
    let loaded = entries(workspace, id)
        .iter()
        .filter(|entry| matches!(entry, TimelineEntry::Comment(_)))
        .count() as u64;
    loaded.max(record.row.comments)
}

pub(super) fn rows(workspace: &Workspace, width: usize, mode: ExplorerIconMode) -> Vec<Line> {
    let id = workspace.number();
    let mut rows = Vec::new();
    if id == 0 {
        return empty(workspace, width);
    }
    let key = Key::Item(1, id);
    let loaded = workspace.item();
    let Some(value) = loaded.or_else(|| workspace.list_item()) else {
        loading_or_error(workspace, &key, &mut rows);
        return rows;
    };
    let mut record = DetailRecord::from_value(value);
    if loaded.is_none()
        && let Some(Err(error)) = workspace.cache.get(&key)
    {
        unavailable(workspace, &mut rows, error, "r");
        rows.push(blank());
    }
    // A list row that omitted the body says nothing about it: no "No description".
    let unknown_body = loaded.is_none() && value["body"].is_null();
    if unknown_body {
        record.body.clear();
    }
    super::conversation::issue(
        workspace,
        id,
        &record,
        loaded.is_some(),
        !unknown_body,
        &mut rows,
        width,
        records::now(),
        mode,
    );
    rows
}

/// One event on the Conversation rail: `● alex added [bug] · 1h ago`.
/// One timeline event in `width` cells. Its age is reserved first, at the row's end; a long
/// linked title is cut to what is left, so no event loses its time.
pub(super) fn event_row(
    workspace: &Workspace,
    entry: &TimelineEntry,
    now: i64,
    width: usize,
    mode: ExplorerIconMode,
) -> Option<Vec<Line>> {
    events_row(workspace, &[entry], now, width, mode)
}

/// Whether `next` reads as one line with `previous` (GitHub's grouping): the same actor doing
/// the same labelling, assigning or review request, at the same shown age.
pub(super) fn joins(previous: &TimelineEntry, next: &TimelineEntry, now: i64) -> bool {
    let (
        TimelineEntry::Event {
            actor: a,
            kind: ka,
            at: ta,
            link: None,
            ..
        },
        TimelineEntry::Event {
            actor: b,
            kind: kb,
            at: tb,
            link: None,
            ..
        },
    ) = (previous, next)
    else {
        return false;
    };
    a == b
        && ka == kb
        && matches!(
            ka.as_str(),
            "labeled" | "unlabeled" | "assigned" | "unassigned" | "review_requested"
        )
        && fresh::relative(ta, now) == fresh::relative(tb, now)
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Events that [`joins`] made one, on one line, in GitHub's words: `● maya added the [bug]
/// label`, `● maya added [bug] [priority: high] labels`, `● maya requested review from alex
/// and sam`, `● maya self-assigned this`.
/// The event's row, and a second row under it when a link target does not fit beside its
/// words.
pub(super) fn events_row(
    workspace: &Workspace,
    entries: &[&TimelineEntry],
    now: i64,
    width: usize,
    mode: ExplorerIconMode,
) -> Option<Vec<Line>> {
    let Some(TimelineEntry::Event {
        actor,
        verb,
        link,
        at,
        kind,
        ..
    }) = entries.first().copied()
    else {
        return None;
    };
    let tone = match kind.as_str() {
        "closed" => GitRole::Removed,
        "reopened" => GitRole::Added,
        "merged" => GitRole::Ref,
        "labeled" | "unlabeled" => GitRole::Modified,
        "assigned" | "unassigned" | "review_requested" => GitRole::Author,
        _ => GitRole::Muted,
    };
    let labels = entries
        .iter()
        .filter_map(|entry| match entry {
            TimelineEntry::Event {
                label: Some(label), ..
            } => Some(label),
            _ => None,
        })
        .collect::<Vec<_>>();
    let objects = entries
        .iter()
        .filter_map(|entry| match entry {
            TimelineEntry::Event {
                label: None,
                object: Some(object),
                ..
            } if !object.is_empty() => Some(object.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let wording = wording(workspace, kind, verb, actor, &objects, labels.len());
    let link = link.clone().or(wording.link);
    let mut rich = Rich::new()
        .push(format!("{} ", glyph(Glyph::Dot, mode)), role(tone))
        .push(actor, strong(GitRole::Author))
        .plain(" ")
        .push(&wording.verb, role(GitRole::Muted));
    // What follows the verb: its labels or objects, then the suffix (`into main`, `label`).
    let mut tail: Vec<(String, crate::ui::Style)> = Vec::new();
    if !labels.is_empty() {
        for label in labels {
            tail.push((format!(" {} ", label.name), prose::label_style(label)));
        }
    } else if !objects.is_empty() && wording.objects {
        let role_of = if kind == "merged"
            || kind.contains("commit")
            || kind == "referenced"
            || kind == "closed"
        {
            GitRole::Hash
        } else {
            GitRole::Text
        };
        tail.push((and_list(&objects), role(role_of)));
    }
    if let Some(suffix) = &wording.suffix {
        tail.push((suffix.clone(), role(GitRole::Muted)));
    }
    let when = fresh::relative(at, now);
    let reserved = if when.is_empty() {
        0
    } else {
        unicode_width::UnicodeWidthStr::width(when.as_str()) + 3
    };
    let tail_width: usize = tail
        .iter()
        .map(|(text, _)| 1 + unicode_width::UnicodeWidthStr::width(text.as_str()))
        .sum();
    // A sentence too long for the row (`force-pushed the feature/native branch from 1a2b3c4
    // to 5d6e7f8`, `locked as off-topic and limited conversation to collaborators`) wraps at
    // its words onto rows under the actor, the age ending the last.
    if link.is_none() && rich.width() + tail_width + reserved > width {
        let head = Rich::new()
            .push(format!("{} ", glyph(Glyph::Dot, mode)), role(tone))
            .push(actor, strong(GitRole::Author));
        let words = wording
            .verb
            .split(' ')
            .map(|word| (word.to_owned(), role(GitRole::Muted)));
        let tokens = words.chain(tail.iter().cloned()).collect::<Vec<_>>();
        let mut rows = Vec::new();
        let mut current = head;
        let mut fresh = false;
        for (index, (text, style)) in tokens.iter().enumerate() {
            let last = index + 1 == tokens.len();
            let need = 1
                + unicode_width::UnicodeWidthStr::width(text.as_str())
                + if last { reserved } else { 0 };
            if !fresh && current.width() + need > width {
                rows.push(std::mem::replace(&mut current, Rich::new().plain("  ")).line());
                fresh = true;
            }
            current = if fresh {
                current.push(text, *style)
            } else {
                current.plain(" ").push(text, *style)
            };
            fresh = false;
        }
        if !when.is_empty() {
            current = current.sep().push(&when, role(GitRole::Muted));
        }
        rows.push(current.line());
        return Some(rows);
    }
    for (text, style) in &tail {
        rich = rich.plain(" ").push(text, *style);
    }
    let mut action = None;
    let mut below = None;
    if let Some((link, pull)) = &link {
        let mark = glyph(
            if *pull {
                Glyph::PullRequest
            } else {
                Glyph::Issue
            },
            mode,
        );
        let text = format!(
            "{mark} {}{} {}",
            workspace.sigil(*pull),
            link.number,
            link.title
        );
        let room = width.saturating_sub(rich.width() + 1 + reserved);
        let link_action = link_action(workspace, link, if *pull { 2 } else { 1 });
        if room >= unicode_width::UnicodeWidthStr::width(text.as_str()).min(24) {
            rich = rich.plain(" ").push(
                super::super::shell::chrome::ellipsize(&text, room),
                role(GitRole::Link),
            );
            action = link_action;
        } else {
            // GitHub's long link sentences (`…may be closed by this pull request:`) leave no
            // room for the target: it reads on the row below, under the verb, with the age.
            let mut next = Rich::new().plain("  ").push(
                super::super::shell::chrome::ellipsize(
                    &text,
                    width.saturating_sub(2 + reserved).max(8),
                ),
                role(GitRole::Link),
            );
            if !when.is_empty() {
                next = next.sep().push(&when, role(GitRole::Muted));
            }
            let mut next = next.line();
            next.action = link_action;
            below = Some(next);
        }
    }
    if !when.is_empty() && below.is_none() {
        rich = rich.sep().push(&when, role(GitRole::Muted));
    }
    let mut row = rich.line();
    row.action = action;
    Some(std::iter::once(row).chain(below).collect())
}

/// An event's words on this page.
struct Wording {
    verb: String,
    /// Whether the event's objects (a sha, a login) follow the verb.
    objects: bool,
    /// Words after the objects or labels: `into main`, `label`.
    suffix: Option<String>,
    /// The item a link event points at, when the event itself does not say (REST's
    /// `connected` carries no target; the page's Development links do).
    link: Option<(LinkRecord, bool)>,
}

/// GitHub's wording for an event: what the record says, with what only the
/// page knows, whether it is a pull request or an issue, its branches and its links.
fn wording(
    workspace: &Workspace,
    kind: &str,
    verb: &str,
    actor: &str,
    objects: &[&str],
    labels: usize,
) -> Wording {
    // The page's own record, read only for the events that need it.
    let pull = workspace.page == 2;
    let record = || workspace.item().map(DetailRecord::from_value);
    let head = || record().map(|record| record.head).unwrap_or_default();
    let mut wording = Wording {
        verb: verb.to_owned(),
        objects: true,
        suffix: None,
        link: None,
    };
    let branch = |name: &str, fallback: &str| {
        if name.is_empty() {
            fallback.to_owned()
        } else {
            format!("the {name} branch")
        }
    };
    match kind {
        // "maya linked an issue that may be closed by this pull request: #1 Queue stalls…"
        "connected" | "disconnected" => {
            let target = if pull { "an issue" } else { "a pull request" };
            wording.verb = match (kind, pull) {
                ("connected", true) => {
                    "linked an issue that may be closed by this pull request".to_owned()
                }
                ("connected", false) => {
                    "linked a pull request that will close this issue".to_owned()
                }
                _ => format!("removed a link to {target}"),
            };
            let links = record().map(|record| record.links).unwrap_or_default();
            if let [only] = links.as_slice() {
                wording.verb.push(':');
                wording.link = Some((only.clone(), !pull));
            }
        }
        "labeled" | "unlabeled" => {
            let added = kind == "labeled";
            wording.verb = match (added, labels) {
                (true, 1) => "added the",
                (false, 1) => "removed the",
                (true, _) => "added",
                (false, _) => "removed",
            }
            .to_owned();
            wording.suffix = Some(if labels == 1 { "label" } else { "labels" }.to_owned());
        }
        "unassigned" if objects == [actor] => {
            wording.verb = "removed their assignment".to_owned();
            wording.objects = false;
        }
        "assigned" if objects == [actor] => {
            wording.verb = "self-assigned this".to_owned();
            wording.objects = false;
        }
        "merged" => {
            let base = record().map(|record| record.base).unwrap_or_default();
            if !base.is_empty() {
                wording.suffix = Some(format!("into {base}"));
            }
        }
        "head_ref_force_pushed" => {
            wording.verb = format!("force-pushed {}", branch(&head(), "the head branch"))
        }
        "head_ref_deleted" | "delete branch" => {
            wording.verb = format!("deleted {}", branch(&head(), "the head branch"))
        }
        "head_ref_restored" => {
            wording.verb = format!("restored {}", branch(&head(), "the head branch"))
        }
        "referenced" => {
            wording.verb = format!(
                "added a commit that referenced this {}",
                if pull { "pull request" } else { "issue" }
            );
        }
        _ => {}
    }
    wording
}

/// X-EMPTY for a list with nothing in it: what the view is and what to do next.
pub(super) fn empty(workspace: &Workspace, _width: usize) -> Vec<Line> {
    let mut rows = Vec::new();
    let what = if workspace.page == 1 {
        "issues"
    } else {
        workspace.terms().pr_plural
    };
    let what = what.to_lowercase();
    if workspace.inbox.enabled {
        rows.push(Rich::new().push("No matches", strong(GitRole::Text)).line());
        rows.push(blank());
        rows.push(muted("/ filter · r reload"));
        return rows;
    }
    // One line of cause and one action. A typed filter or the closed state
    // offers the way back first, as GitHub's "Clear current search query, filters, and sorts".
    let filtered = workspace.filtered();
    rows.push(
        Rich::new()
            .push(
                &if filtered {
                    format!("No {what} match this filter")
                } else {
                    format!("No {what} match this view")
                },
                strong(GitRole::Text),
            )
            .line(),
    );
    rows.push(blank());
    if filtered {
        let mut clear = Rich::new()
            .push("› ", role(GitRole::Focus))
            .push("Clear the filter", role(GitRole::Link))
            .push(format!(" · every open {}", what), role(GitRole::Muted))
            .line();
        clear.action = Some(Action::ClearFilter);
        rows.push(clear);
    }
    let mut create = Rich::new()
        .push("› ", role(GitRole::Focus))
        .push(
            if workspace.page == 1 {
                "New issue".to_owned()
            } else {
                format!("New {}", workspace.terms().pr_singular.to_lowercase())
            }
            .as_str(),
            role(GitRole::Link),
        )
        .line();
    create.action = Some(if workspace.page == 1 {
        Action::CreateIssue
    } else {
        Action::CreatePullRequest
    });
    rows.push(create);
    rows
}
