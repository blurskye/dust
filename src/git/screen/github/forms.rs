//! Forge sheets that take the reader's place (DESIGN.md §3.30, §3.32): Submit review and
//! Merge. The form's fields drive them; choices read as radio rows, the verdict or method a
//! filled chip, and what the forge cannot do stays visible, muted, with its reason.
use super::super::shell::chrome::{self, Span, bold, st};
use super::super::shell::glyphs::{Glyph, glyph};
use super::super::shell::layout::Rect;
use super::super::{Field, Form, FormAction};
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::records::DetailRecord;
use crate::ui::{Frame, GitChip, GitRole, GitSurface};
use unicode_width::UnicodeWidthStr;

const PANE: GitSurface = GitSurface::Pane;

/// Whether this form is drawn by `paint` in place of the reader.
pub(super) fn owns(form: &Form) -> bool {
    matches!(&form.action, FormAction::Github(Action::Review(_) | Action::Merge(..)))
}

fn rule(frame: &mut Frame, x: u16, right: u16, y: u16) {
    for column in x..right {
        frame.grid.set_cell(column, y, "─", st(GitRole::Border, PANE));
    }
}

fn heading(frame: &mut Frame, x: u16, right: u16, y: u16, text: &str, note: &str) {
    let spans: Vec<Span> = vec![
        (text.to_owned(), bold(GitRole::Focus, PANE)),
        (if note.is_empty() { String::new() } else { format!("  {note}") }, st(GitRole::Muted, PANE)),
    ];
    chrome::put_spans(&mut frame.grid, x, y, &spans, right);
}

/// `○ Comment  ○ Approve  ● Request changes`; `all` lists the options the forge knows of,
/// the field's own choices are the ones it allows.
fn radio(
    frame: &mut Frame,
    field: &Field,
    all: &[(&str, GitChip, GitRole)],
    refusals: &[(&str, &str)],
    x: u16,
    y: u16,
    right: u16,
    focused: bool,
) {
    let current = field.input.input.as_str();
    let mut at = x;
    for (name, hue, role) in all {
        let allowed = field.choices.iter().any(|choice| choice == name);
        let chosen = allowed && current == *name;
        let (text, style) = if chosen {
            (format!(" ● {name} "), bold(*role, GitSurface::Chip(*hue)))
        } else if allowed {
            (format!(" ○ {name} "), st(GitRole::Text, PANE))
        } else {
            (format!(" ○ {name} "), st(GitRole::Muted, PANE))
        };
        at = frame.grid.put_text_clipped(at, y, &text, style, right) + 1;
    }
    let reason = refusals
        .iter()
        .find(|(name, _)| !field.choices.iter().any(|choice| choice == name))
        .map(|(_, reason)| *reason);
    let note = match (focused, reason) {
        (_, Some(reason)) => reason.to_owned(),
        (true, None) => "↑/↓ choose".to_owned(),
        _ => String::new(),
    };
    if !note.is_empty() && right > at + 4 {
        let note = chrome::ellipsize(&note, usize::from(right - at - 2));
        frame.grid.put_text_clipped(at + 1, y, &note, st(GitRole::Muted, PANE), right);
    }
    if focused {
        frame.cursor = Some((x + 1, y, crossterm::cursor::SetCursorStyle::SteadyBlock));
    }
}

fn label(frame: &mut Frame, x: u16, y: u16, right: u16, text: &str, focused: bool) -> u16 {
    frame.grid.put_text_clipped(
        x,
        y,
        text,
        if focused { bold(GitRole::Focus, PANE) } else { st(GitRole::Muted, PANE) },
        right,
    )
}

pub(super) fn paint(
    frame: &mut Frame,
    workspace: &Workspace,
    form: &Form,
    rect: Rect,
    mode: ExplorerIconMode,
    notice: &str,
) {
    chrome::fill(&mut frame.grid, rect, st(GitRole::Text, PANE));
    let merge = matches!(&form.action, FormAction::Github(Action::Merge(..)));
    let title: Vec<Span> = vec![
        (
            format!("{} ", glyph(if merge { Glyph::PullMerged } else { Glyph::File }, mode)),
            st(GitRole::Focus, PANE),
        ),
        (chrome::ellipsize(&form.title, usize::from(rect.w.saturating_sub(8))), bold(GitRole::Focus, PANE)),
    ];
    chrome::pane(&mut frame.grid, rect, &title, "", true);
    let inner = rect.inner();
    let (x, right) = (inner.x + 1, inner.right().saturating_sub(1));
    let bottom = inner.bottom();
    let record = workspace.item().map(DetailRecord::from_value);
    let login = crate::forge::host_state::account(&crate::forge::transport::HostKey::new(&workspace.host))
        .and_then(|account| account.login);
    let mut y = inner.y;
    // Who and where.
    let mut facts: Vec<Span> = Vec::new();
    if merge {
        if let Some(record) = &record {
            facts.push((record.base.clone(), st(GitRole::Muted, PANE)));
            facts.push((" ← ".into(), st(GitRole::Muted, PANE)));
            facts.push((record.head.clone(), st(GitRole::Ref, PANE)));
            facts.push((" · ".into(), st(GitRole::Muted, PANE)));
            facts.push((
                format!("{} commit{}", record.commits.len(), if record.commits.len() == 1 { "" } else { "s" }),
                st(GitRole::Muted, PANE),
            ));
            if let (Some(add), Some(del)) = (record.row.additions, record.row.deletions) {
                facts.push((" · ".into(), st(GitRole::Muted, PANE)));
                facts.push((format!("+{add}"), st(GitRole::Added, PANE)));
                facts.push((format!(" −{del}"), st(GitRole::Removed, PANE)));
            }
        }
    } else {
        facts.push(("Review by ".into(), st(GitRole::Muted, PANE)));
        facts.push((login.clone().unwrap_or_else(|| "you".into()), st(GitRole::Author, PANE)));
        facts.push((" · ".into(), st(GitRole::Muted, PANE)));
        facts.push(("Not submitted".into(), st(GitRole::Muted, PANE)));
    }
    chrome::put_spans(&mut frame.grid, x, y, &facts, right);
    y += 2;
    let caps = workspace.capabilities();
    let own = !merge
        && login.is_some()
        && record.as_ref().is_some_and(|record| Some(&record.row.author) == login.as_ref());
    let approve_reason = if own {
        Some("Approve is unavailable on your own pull request")
    } else {
        caps.review_approve.reason()
    };
    // Bottom block: summary row, hints.
    let footer_rows = 3u16;
    let content_bottom = bottom.saturating_sub(footer_rows);
    for (index, field) in form.fields.iter().enumerate() {
        if y >= content_bottom {
            break;
        }
        let focused = form.selected == index;
        match field.label.as_str() {
            "Decision" => {
                let mut refusals = Vec::new();
                if let Some(reason) = approve_reason {
                    refusals.push(("Approve", reason));
                }
                if let Some(reason) = caps.review_request_changes.reason() {
                    refusals.push(("Request changes", reason));
                }
                radio(
                    frame,
                    field,
                    &[
                        ("Comment", GitChip::Focus, GitRole::Focus),
                        ("Approve", GitChip::Added, GitRole::Added),
                        ("Request changes", GitChip::Modified, GitRole::Modified),
                    ],
                    &refusals,
                    x.saturating_sub(1),
                    y,
                    right,
                    focused,
                );
                y += 1;
                rule(frame, x, right, y);
                y += 1;
            }
            "Method" | "Source branch" => {
                let at = label(frame, x, y, right, &format!("{:<14}", field.label), focused);
                let options: Vec<(&str, GitChip, GitRole)> = if field.label == "Method" {
                    vec![
                        ("Squash", GitChip::Focus, GitRole::Focus),
                        ("Merge", GitChip::Focus, GitRole::Focus),
                        ("Rebase", GitChip::Focus, GitRole::Focus),
                    ]
                } else {
                    vec![
                        ("Keep it", GitChip::Focus, GitRole::Focus),
                        ("Delete it after the merge", GitChip::Removed, GitRole::Removed),
                    ]
                };
                let refusal = [("Rebase", "this forge does not rebase-merge"), ("Squash", "this forge does not squash")];
                radio(frame, field, &options, &refusal, at, y, right, focused);
                y += 2;
            }
            "Summary" => {
                heading(frame, x, right, y, "Summary", "Markdown");
                y += 1;
                let rows = (content_bottom.saturating_sub(y)).clamp(1, 6);
                let rows = if workspace.review_threads(workspace.number()).is_empty() {
                    rows
                } else {
                    rows.min(4)
                };
                super::super::shell::sheet::input(frame, field, Rect::new(x + 1, y, right.saturating_sub(x + 1), rows), focused);
                if field.input.input.is_empty() {
                    frame.grid.put_text_clipped(
                        x + 2,
                        y,
                        "Leave a summary for the author (optional)",
                        st(GitRole::Muted, if focused { GitSurface::CursorInactive } else { GitSurface::Canvas }),
                        right,
                    );
                }
                y += rows + 1;
            }
            _ => {
                label(frame, x, y, right, &field.label, focused);
                y += 1;
                let rows = if field.multiline { 4 } else { 1 };
                super::super::shell::sheet::input(frame, field, Rect::new(x + 1, y, right.saturating_sub(x + 1), rows), focused);
                y += rows + 1;
            }
        }
    }
    if merge {
        // §3.32: the merge is pinned to the reviewed head.
        if let Some(record) = &record
            && y < content_bottom
        {
            let sha: String = record.head_sha.chars().take(7).collect();
            let text = if sha.is_empty() {
                "Refresh the pull request to pin the merge to its head".to_owned()
            } else if caps.merge_pinned_head.allowed() {
                format!("Merge is pinned to {sha} · head changes require a refreshed review")
            } else {
                format!("Head {sha} · this forge does not pin merges to a head")
            };
            frame.grid.put_text_clipped(x, y, &text, st(GitRole::Muted, PANE), right);
            y += 2;
        }
        let _ = y;
    } else {
        // Inline drafts waiting for this review (§3.30): sent with it.
        let drafts = workspace.inline_drafts.get(&workspace.number()).map(Vec::as_slice).unwrap_or_default();
        if !drafts.is_empty() && y + 2 < content_bottom {
            rule(frame, x, right, y);
            y += 1;
            heading(frame, x, right, y, &format!("Inline drafts ({})", drafts.len()), "sent with this review");
            y += 1;
            for (draft, _) in drafts {
                if y + 1 >= content_bottom {
                    break;
                }
                let place = format!("{}:{}", draft.path, draft.line);
                let body = draft.body.lines().next().unwrap_or_default();
                let spans: Vec<Span> = vec![
                    (format!("{} ", glyph(Glyph::Comment, mode)), st(GitRole::Modified, PANE)),
                    (place.clone(), st(GitRole::Link, PANE)),
                    ("  ".into(), st(GitRole::Text, PANE)),
                    (
                        chrome::ellipsize(body, usize::from(right.saturating_sub(x)).saturating_sub(place.width() + 4)),
                        st(GitRole::Text, PANE),
                    ),
                ];
                chrome::put_spans(&mut frame.grid, x, y, &spans, right);
                y += 1;
            }
            y += 1;
        }
        // Inline review threads on this pull request.
        let threads = workspace.review_threads(workspace.number());
        if !threads.is_empty() && y + 2 < content_bottom {
            rule(frame, x, right, y);
            y += 1;
            let open = threads.iter().filter(|thread| thread["isResolved"].as_bool() == Some(false)).count();
            heading(frame, x, right, y, &format!("Inline threads ({})", threads.len()), &format!("{open} open"));
            y += 1;
            for thread in threads {
                if y + 1 >= content_bottom {
                    break;
                }
                let comments = thread["comments"]["nodes"].as_array().cloned().unwrap_or_default();
                let Some(first) = comments.first() else { continue };
                let path = first["path"].as_str().unwrap_or_default();
                let line = first["line"].as_u64().or_else(|| first["originalLine"].as_u64());
                let place = match line {
                    Some(line) => format!("{path}:{line}"),
                    None => path.to_owned(),
                };
                let resolved = thread["isResolved"].as_bool() == Some(true);
                let mut spans: Vec<Span> = vec![
                    (format!("{} ", glyph(Glyph::Comment, mode)), st(GitRole::Muted, PANE)),
                    (place, st(GitRole::Link, PANE)),
                    ("  ".into(), st(GitRole::Text, PANE)),
                ];
                spans.push(if resolved {
                    (" Resolved ".into(), st(GitRole::Muted, GitSurface::Chip(GitChip::Muted)))
                } else {
                    (" Open ".into(), st(GitRole::Modified, GitSurface::Chip(GitChip::Modified)))
                });
                chrome::put_spans(&mut frame.grid, x, y, &spans, right);
                y += 1;
                let body = first["body"].as_str().unwrap_or_default().lines().next().unwrap_or_default();
                let author = first["author"]["login"].as_str().unwrap_or("ghost");
                let spans: Vec<Span> = vec![
                    (format!("  {author} "), st(GitRole::Author, PANE)),
                    (chrome::ellipsize(body, usize::from(right.saturating_sub(x)).saturating_sub(author.width() + 4)), st(GitRole::Text, PANE)),
                ];
                chrome::put_spans(&mut frame.grid, x, y, &spans, right);
                y += 2;
            }
        }
    }
    // The summary of what Ctrl+Enter sends, then the keys.
    let y = bottom.saturating_sub(footer_rows);
    rule(frame, x, right, y);
    let first = form.fields.first().map(|field| field.input.input.clone()).unwrap_or_default();
    let (chip, hue, role) = if merge {
        (format!(" {} ", if first.is_empty() { "Merge".into() } else { first.clone() }), GitChip::Focus, GitRole::Focus)
    } else {
        match first.as_str() {
            "Approve" => (" Approve ".to_owned(), GitChip::Added, GitRole::Added),
            "Request changes" => (" Request changes ".to_owned(), GitChip::Modified, GitRole::Modified),
            _ => (" Comment ".to_owned(), GitChip::Focus, GitRole::Focus),
        }
    };
    let mut at = frame.grid.put_text_clipped(x, y + 1, &chip, bold(role, GitSurface::Chip(hue)), right);
    let detail = if merge {
        let delete = form.fields.iter().any(|field| field.input.input == "Delete it after the merge");
        if delete { "the source branch is deleted afterwards" } else { "the source branch stays" }.to_owned()
    } else {
        let threads = workspace.review_threads(workspace.number()).len();
        let drafts = workspace.inline_drafts.get(&workspace.number()).map_or(0, Vec::len);
        match (drafts, threads) {
            (0, 0) => "no inline threads".to_owned(),
            (0, 1) => "1 inline thread on this pull request".to_owned(),
            (0, n) => format!("{n} inline threads on this pull request"),
            (1, _) => "with 1 inline draft".to_owned(),
            (n, _) => format!("with {n} inline drafts"),
        }
    };
    at = frame.grid.put_text_clipped(at, y + 1, " · ", st(GitRole::Border, PANE), right);
    frame.grid.put_text_clipped(at, y + 1, &detail, st(GitRole::Muted, PANE), right);
    if !notice.is_empty() {
        frame.grid.put_text_clipped(
            x,
            y + 2,
            &chrome::ellipsize(notice, usize::from(right.saturating_sub(x))),
            st(GitRole::Removed, PANE),
            right,
        );
    }
    // The send and Esc keys are in the footer (§3.30), once.
}
