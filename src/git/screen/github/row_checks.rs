//! The check mark on Gitea's and Forgejo's pull request rows (r15 gitea-forgejo): their list
//! API carries no CI state, so once the list is painted the rows on screen are asked for their
//! head's combined status, lazily, on the warm worker (one batch, at most
//! `provider::parallel::WIDTH` requests at once). A known state stays on its row while it is
//! asked again (stale-while-revalidate): after `TTL`, or as soon as the row's `updatedAt`
//! changes (a push or an edit). A row the forge did not answer for is not asked again before
//! `TTL` either, so a failing forge is not hammered.
use super::*;
use crate::forge::model::CiState;
use crate::forge::provider::ProviderKind;
use crate::forge::records::ItemRecord;
use crate::forge::request::Payload;
use std::cell::RefCell;
use std::time::{Duration, Instant};

/// How long a row's mark is trusted before the forge is asked again.
const TTL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct Rows {
    /// Pull request numbers on screen at the last paint of the list.
    visible: RefCell<Vec<u64>>,
    /// Per number: the state (None: no checks), the row's `updatedAt` when asked, and when.
    known: HashMap<u64, (Option<CiState>, String, Instant)>,
    /// Per number: the `updatedAt` it was last asked with, and when.
    asked: HashMap<u64, (String, Instant)>,
}

impl Rows {
    /// The list's paint: these rows are on screen.
    pub(super) fn show(&self, numbers: Vec<u64>) {
        *self.visible.borrow_mut() = numbers;
    }

    /// The row's checks with what the forge said, when the list row carries none itself.
    pub(super) fn overlay(&self, record: &mut ItemRecord) {
        if record.checks.total() > 0 {
            return;
        }
        if let Some((Some(state), ..)) = self.known.get(&record.number) {
            record.checks.add(*state);
        }
    }
}

impl Workspace {
    fn row_checks_wanted(&self) -> bool {
        self.page == 2
            && !self.inbox.enabled
            && matches!(
                self.forge_kind(),
                Some(ProviderKind::Gitea | ProviderKind::Forgejo)
            )
    }

    /// Take the warm worker's answers and ask for the visible rows still unknown or stale.
    pub(super) fn poll_row_checks(&mut self) -> bool {
        let mut changed = false;
        let answered = self
            .cache
            .keys()
            .filter(|key| matches!(key, Key::RowChecks(_)))
            .cloned()
            .collect::<Vec<_>>();
        for key in answered {
            // From disk (a restart): shown, and asked again as if its budget ran out.
            let verified = self
                .fresh
                .meta
                .remove(&key)
                .is_some_and(|meta| meta.verified);
            let seen = if verified {
                Instant::now()
            } else {
                Instant::now().checked_sub(TTL).unwrap_or_else(Instant::now)
            };
            let Some(Ok(reply)) = self.cache.remove(&key) else {
                continue;
            };
            let Key::RowChecks(numbers) = &key else {
                continue;
            };
            let Ok(Payload::Fingerprints(prints)) = serde_json::from_value::<Payload>(reply.value)
            else {
                continue;
            };
            for number in numbers {
                let updated = self
                    .fresh
                    .rows
                    .asked
                    .get(number)
                    .map(|(updated, _)| updated.clone())
                    .unwrap_or_default();
                let state = prints
                    .iter()
                    .find(|(each, _)| each == number)
                    .and_then(|(_, print)| print.rollup);
                if self.fresh.rows.known.get(number).map(|(known, ..)| *known) != Some(state) {
                    changed = true;
                }
                self.fresh
                    .rows
                    .known
                    .insert(*number, (state, updated, seen));
            }
        }
        if !self.row_checks_wanted() || self.fresh.warm_has(|key| matches!(key, Key::RowChecks(_)))
        {
            return changed;
        }
        let updated = self
            .items()
            .iter()
            .map(|item| (number(item, "number"), string(item, "updatedAt").to_owned()))
            .collect::<HashMap<_, _>>();
        let rows = &self.fresh.rows;
        let visible = rows.visible.borrow().clone();
        let ask = visible
            .into_iter()
            .filter(|number| {
                let Some(now) = updated.get(number) else {
                    return false;
                };
                let fresh = |seen: &String, at: &Instant| seen == now && at.elapsed() < TTL;
                let known = rows
                    .known
                    .get(number)
                    .is_some_and(|(_, seen, at)| fresh(seen, at));
                let asked = rows
                    .asked
                    .get(number)
                    .is_some_and(|(seen, at)| fresh(seen, at));
                !known && !asked
            })
            .collect::<Vec<_>>();
        if ask.is_empty() {
            return changed;
        }
        for number in &ask {
            self.fresh
                .rows
                .asked
                .insert(*number, (updated[number].clone(), Instant::now()));
        }
        let key = Key::RowChecks(ask.clone());
        let op = self.read(Read::Fingerprints(ask));
        // A restart paints the marks the last session saw while the forge is asked.
        changed |= self.load_disk(&key, &op);
        self.fresh.push_warm(Request { key, op });
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_known_state_marks_a_row_without_checks_only() {
        let mut rows = Rows::default();
        rows.known
            .insert(7, (Some(CiState::Failed), String::new(), Instant::now()));
        let mut bare = ItemRecord::from_row(&json!({ "number": 7 }));
        rows.overlay(&mut bare);
        assert_eq!(bare.ci(), Some(CiState::Failed));
        let mut own = ItemRecord::from_row(&json!({
            "number": 7,
            "statusCheckRollup": [{ "__typename": "StatusContext", "state": "SUCCESS" }],
        }));
        let before = own.ci();
        rows.overlay(&mut own);
        assert_eq!(own.ci(), before);
        let mut other = ItemRecord::from_row(&json!({ "number": 8 }));
        rows.overlay(&mut other);
        assert_eq!(other.ci(), None);
    }
}
