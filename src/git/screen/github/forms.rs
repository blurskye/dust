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

/// A comment's first two wrapped lines from `x` (after `lead`, the author), the second cut
/// with `…` when more follows (r15 prs-06); the row after the last one drawn.
fn preview(frame: &mut Frame, x: u16, right: u16, mut y: u16, bottom: u16, body: &str, lead: &[Span]) -> u16 {
    let lines = preview_lines(body, usize::from(right.saturating_sub(x)), chrome::spans_width(lead).into());
    for (index, line) in lines.into_iter().enumerate() {
        if y >= bottom {
            break;
        }
        let mut spans: Vec<Span> = if index == 0 { lead.to_vec() } else { Vec::new() };
        spans.push((line, st(GitRole::Text, PANE)));
        chrome::put_spans(&mut frame.grid, x, y, &spans, right);
        y += 1;
    }
    y
}

/// `body`'s words in at most two rows of `width` cells, the first `lead` cells shorter.
fn preview_lines(body: &str, width: usize, lead: usize) -> Vec<String> {
    let words = body.split_whitespace().collect::<Vec<_>>();
    let mut rows: Vec<String> = vec![String::new()];
    let mut rest = 0;
    for (index, word) in words.iter().enumerate() {
        let room = if rows.len() == 1 { width.saturating_sub(lead) } else { width };
        let row = rows.last_mut().expect("a row");
        let needed = if row.is_empty() { word.width() } else { row.width() + 1 + word.width() };
        if needed <= room || row.is_empty() {
            if !row.is_empty() {
                row.push(' ');
            }
            row.push_str(word);
        } else if rows.len() < 2 {
            rows.push((*word).to_owned());
        } else {
            rest = index;
            break;
        }
    }
    if rest > 0 {
        let last = rows.last_mut().expect("a row");
        *last = chrome::ellipsize(&format!("{last} {}", words[rest]), width);
    }
    for (index, row) in rows.iter_mut().enumerate() {
        let room = if index == 0 { width.saturating_sub(lead) } else { width };
        if row.width() > room {
            *row = chrome::ellipsize(row, room);
        }
    }
    rows.retain(|row| !row.is_empty());
    rows
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
    // Only why a choice is missing: the footer says `↑/↓ choose` while the row has focus.
    let note = reason.map(str::to_owned).unwrap_or_default();
    if !note.is_empty() && right > at + 4 {
        let note = chrome::ellipsize(&note, usize::from(right - at - 2));
        frame.grid.put_text_clipped(at + 1, y, &note, st(GitRole::Muted, PANE), right);
    }
    // No terminal caret on a choice: it sat on the chip's padding as a black block (r7
    // N-14); the filled chip and the footer's `↑/↓ choose` already mark the focus.
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
            if let (Some(add), Some(del)) = (record.row.additions, record.row.deletions)
                && add + del > 0
            {
                // A zero side is no fact (r11): `+13`, not `+13 −0`.
                facts.push((" · ".into(), st(GitRole::Muted, PANE)));
                if add > 0 {
                    facts.push((format!("+{add}"), st(GitRole::Added, PANE)));
                }
                if del > 0 {
                    facts.push((format!("{}−{del}", if add > 0 { " " } else { "" }), st(GitRole::Removed, PANE)));
                }
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
                let refusal = [("Rebase", "Rebase and merge isn't available here"), ("Squash", "Squash and merge isn't available here")];
                radio(frame, field, &options, &refusal, at, y, right);
                y += 2;
            }
            "Summary" => {
                heading(frame, x, right, y, "Summary", "Markdown");
                y += 1;
                // The optional summary takes four to six rows (r15 prs-06): the room is for the
                // pending comments and threads below, what is re-read before Ctrl+Enter.
                let room = content_bottom.saturating_sub(y);
                let threads = workspace.review_threads(workspace.number()).len() as u16;
                let drafts = workspace.inline_drafts.get(&workspace.number()).map_or(0, Vec::len) as u16;
                let below = if threads > 0 { 3 + 4 * threads } else { 0 } + if drafts > 0 { 3 + 2 * drafts } else { 0 };
                let rows = room.saturating_sub(below + 1).clamp(4, 6).min(room.max(1));
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
                "Refresh the pull request before merging".to_owned()
            } else if caps.merge_pinned_head.allowed() {
                format!("Merges {sha} · new commits pushed before then cancel it")
            } else {
                "Merges the latest commit on the branch".to_owned()
            };
            frame.grid.put_text_clipped(x, y, &text, st(GitRole::Muted, PANE), right);
            y += 2;
        }
        let _ = y;
    } else {
        // Inline drafts waiting for this review (§3.30): included when it is submitted.
        let drafts = workspace.inline_drafts.get(&workspace.number()).map(Vec::as_slice).unwrap_or_default();
        if !drafts.is_empty() && y + 2 < content_bottom {
            rule(frame, x, right, y);
            y += 1;
            heading(frame, x, right, y, &format!("Pending comments ({})", drafts.len()), "included when you submit");
            y += 1;
            for (draft, _) in drafts {
                if y + 1 >= content_bottom {
                    break;
                }
                let place = format!("{}:{}", draft.path, super::drafts::draft_lines(draft));
                let spans: Vec<Span> = vec![
                    (format!("{} ", glyph(Glyph::Comment, mode)), st(GitRole::Modified, PANE)),
                    (place, st(GitRole::Link, PANE)),
                ];
                chrome::put_spans(&mut frame.grid, x, y, &spans, right);
                y += 1;
                y = preview(frame, x + 2, right, y, content_bottom, &draft.body, &[]);
            }
            y += 1;
        }
        // Inline review threads on this pull request.
        let threads = workspace.review_threads(workspace.number());
        if !threads.is_empty() && y + 2 < content_bottom {
            rule(frame, x, right, y);
            y += 1;
            let open = threads.iter().filter(|thread| thread["isResolved"].as_bool() == Some(false)).count();
            heading(frame, x, right, y, "Review threads", &format!("{open} of {} unresolved", threads.len()));
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
                // The words and colours the Activity tab and the diff use (r7 N-12).
                spans.push(if resolved {
                    ("● Resolved".into(), st(GitRole::Added, PANE))
                } else {
                    ("◔ Unresolved".into(), st(GitRole::Modified, PANE))
                });
                chrome::put_spans(&mut frame.grid, x, y, &spans, right);
                y += 1;
                let body = first["body"].as_str().unwrap_or_default();
                let author = first["author"]["login"].as_str().unwrap_or("ghost");
                y = preview(frame, x + 2, right, y, content_bottom, body, &[(format!("{author} "), st(GitRole::Author, PANE))]);
                y += 1;
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
    // A review's verdict is the radio row above, so the strip names only where it goes and
    // what it carries (r11 prs-09); a merge's chip is its only statement of the method.
    let mut at = if merge {
        let at = frame.grid.put_text_clipped(x, y + 1, &chip, bold(role, GitSurface::Chip(hue)), right);
        frame.grid.put_text_clipped(at, y + 1, " · ", st(GitRole::Border, PANE), right)
    } else {
        let _ = (chip, hue, role);
        frame.grid.put_text_clipped(x + 1, y + 1, &format!("Submits to {}{}", workspace.sigil(true), workspace.number()), st(GitRole::Text, PANE), right)
    };
    let detail = if merge {
        let delete = form.fields.iter().any(|field| field.input.input == "Delete it after the merge");
        if delete { "the source branch is deleted afterwards" } else { "the source branch stays" }.to_owned()
    } else {
        let drafts = workspace.inline_drafts.get(&workspace.number()).map_or(0, Vec::len);
        // What the send carries; the threads above are already counted there.
        let detail = match drafts {
            0 => "summary only, no pending comments".to_owned(),
            n => super::drafts::pending(n),
        };
        at = frame.grid.put_text_clipped(at, y + 1, " · ", st(GitRole::Border, PANE), right);
        detail
    };
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

#[cfg(test)]
mod tests {
    #[test]
    fn a_review_preview_wraps_to_two_rows_and_cuts_the_rest() {
        let body = "This could be a named constant next to RETRY_LIMIT so the cap reads in one place";
        let rows = super::preview_lines(body, 30, 5);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert!(rows[0].len() <= 25 && rows[0].starts_with("This could be"), "{rows:?}");
        assert!(rows[1].ends_with('…'), "{rows:?}");
        assert_eq!(super::preview_lines("Short note", 30, 5), vec!["Short note".to_owned()]);
    }
}
