//! A pull request row's review fact: the decision's own word, or the counts once they say
//! more (`✓ 1 approval · 1 pending` after alex approved while sam is still asked), as GitHub's
//! `reviewDecision` weighs each reviewer's latest review. A forge whose list carries only the
//! open requests (Gitea, Forgejo) counts from the item once it has been opened.
use super::super::shell::chrome::{Span, st};
use super::*;
use crate::config::ExplorerIconMode;
use crate::forge::model::ReviewState;
use crate::forge::records::{self, DetailRecord, ItemRecord};
use crate::ui::{GitRole, GitSurface};

impl Workspace {
    /// The row's review fact as spans, or None when it has none ("Not requested" is none).
    pub(super) fn review_spans(&self, record: &ItemRecord, mode: ExplorerIconMode, surface: GitSurface) -> Option<Vec<Span>> {
        // An opened pull request of this repository (a cross-repository row may share its number).
        let opened = record
            .tally
            .is_none()
            .then(|| record.repo.is_none().then(|| self.value(&Key::Item(2, record.number))).flatten())
            .flatten()
            .map(|value| records::tally_of(&DetailRecord::from_value(value).reviewers()));
        let (state, words) = record.review_fact(opened)?;
        let (mark, word, role) = pr::review_mark(state, mode);
        Some(match words {
            // `✓ 1 approval` in the verdict's colour, ` · 1 pending` muted.
            Some(words) => match words.split_once(" · ") {
                Some((counted, rest)) => vec![
                    (format!("{mark} {counted}"), st(role, surface)),
                    (format!(" · {rest}"), st(GitRole::Muted, surface)),
                ],
                None => vec![(format!("{mark} {words}"), st(role, surface))],
            },
            None => {
                let word = if state == ReviewState::Requested { self.requested_words().0 } else { word };
                vec![(format!("{mark} {word}"), st(role, surface))]
            }
        })
    }
}
