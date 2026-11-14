//! Incremental refreshes (FORGE.md §7.5): a list the provider can filter by "updated since"
//! (GitHub notifications `since`, GitLab pipelines `updated_after`, Gitea notifications and
//! issues `since`) is refreshed by asking only for what changed after the cached copy was
//! fetched, then merging that into the cached copy. Every tenth refresh of the same list is a
//! full one, which drops rows that left the list (closed, read elsewhere) and resets any drift.

use crate::forge::cache::Cache;
use crate::forge::cache::CacheKey;
use crate::forge::model::Timestamp;
use crate::forge::provider::Conditional;
use crate::forge::provider::Fetched;
use crate::forge::provider::ForgeContext;
use crate::forge::provider::Provider;
use crate::forge::request::PageOf;
use crate::forge::request::Payload;
use crate::forge::request::Read;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::OnceLock;

/// One full refresh in this many.
pub const FULL_EVERY: u32 = 10;
/// Clock skew allowance: `since` is this much before the cached copy's fetch time.
const SKEW_SECONDS: u64 = 60;

fn refreshes() -> &'static Mutex<HashMap<String, u32>> {
    static REFRESHES: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    REFRESHES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether this refresh of `key` should be a full one (the first and every tenth).
pub fn full_refresh(key: &str) -> bool {
    let mut counts = refreshes()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let count = counts.entry(key.to_owned()).or_insert(0);
    let full = count.is_multiple_of(FULL_EVERY);
    *count = count.wrapping_add(1);
    full
}

/// An RFC 3339 UTC time from Unix seconds.
pub fn rfc3339(seconds: u64) -> Timestamp {
    crate::forge::model::logs::rfc3339(seconds as i64)
}

/// The conditional to send and, when the read goes out incrementally, the cached payload the
/// answer merges into.
pub fn prepare(
    provider: &dyn Provider,
    ctx: &ForgeContext,
    cache: &Cache,
    key: &CacheKey,
    read: &Read,
    cond: Conditional,
) -> (Conditional, Option<Payload>) {
    if cond.since.is_some() || !provider.incremental(ctx, read) {
        return (cond, None);
    }
    let identity = format!("{}|{:?}|{}", key.host, key.repo, key.key);
    if full_refresh(&identity) {
        return (cond, None);
    }
    let Some(envelope) = cache.get::<Payload>(key) else {
        return (cond, None);
    };
    let since = rfc3339((envelope.fetched_at / 1000).saturating_sub(SKEW_SECONDS));
    (
        Conditional {
            etag: None,
            since: Some(since),
            fingerprint: None,
        },
        Some(envelope.data),
    )
}

/// Rows from `fresh` replace the cached rows with the same id; rows the cache did not have
/// go to the top (every incremental list is newest-first).
fn rows<T: Clone>(base: &PageOf<T>, fresh: &PageOf<T>, id: impl Fn(&T) -> String) -> PageOf<T> {
    let mut items = Vec::with_capacity(base.items.len() + fresh.items.len());
    let mut changed = HashMap::new();
    for item in &fresh.items {
        changed.insert(id(item), item.clone());
    }
    let known = base.items.iter().map(&id).collect::<Vec<_>>();
    items.extend(
        fresh
            .items
            .iter()
            .filter(|item| !known.contains(&id(item)))
            .cloned(),
    );
    let added = items.len() as u64;
    for item in &base.items {
        items.push(changed.remove(&id(item)).unwrap_or_else(|| item.clone()));
    }
    PageOf {
        items,
        next: base.next,
        total: base.total.map(|total| total + added),
    }
}

/// Merge an incremental answer into the cached payload it was asked against.
pub fn merge(read: &Read, base: Payload, fetched: Fetched) -> Fetched {
    let fresh = match fetched {
        Fetched::NotModified => {
            return Fetched::payload(base);
        }
        Fetched::Payload { value, .. } => value,
    };
    let merged = match (&base, &fresh) {
        (Payload::Runs(base), Payload::Runs(fresh)) => {
            Payload::Runs(rows(base, fresh, |run| run.id.0.clone()))
        }
        (Payload::Rows(base), Payload::Rows(fresh)) => {
            Payload::Rows(rows(base, fresh, |row| row.number.to_string()))
        }
        (Payload::Notifications(base), Payload::Notifications(fresh)) => {
            let mut merged = rows(base, fresh, |item| item.id.0.clone());
            merged
                .items
                .sort_by(|left, right| right.updated.cmp(&left.updated));
            Payload::Notifications(merged)
        }
        _ => {
            let _ = read;
            fresh
        }
    };
    Fetched::payload(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::model::ItemId;
    use crate::forge::model::Run;
    use crate::forge::model::RunStatus;

    fn run(id: u64, status: RunStatus) -> Run {
        Run {
            id: ItemId::new(id),
            status,
            ..Run::default()
        }
    }

    #[test]
    fn changed_rows_replace_in_place_and_new_rows_lead() {
        let base = Payload::Runs(PageOf {
            items: vec![run(3, RunStatus::InProgress), run(2, RunStatus::Completed)],
            next: None,
            total: Some(2),
        });
        let fresh = Fetched::payload(Payload::Runs(PageOf {
            items: vec![run(4, RunStatus::Queued), run(3, RunStatus::Completed)],
            next: None,
            total: Some(2),
        }));
        let read = Read::Runs {
            filter: Default::default(),
            page: Default::default(),
        };
        let Fetched::Payload {
            value: Payload::Runs(merged),
            ..
        } = merge(&read, base, fresh)
        else {
            panic!("runs")
        };
        assert_eq!(
            merged
                .items
                .iter()
                .map(|run| (run.id.0.as_str(), run.status))
                .collect::<Vec<_>>(),
            [
                ("4", RunStatus::Queued),
                ("3", RunStatus::Completed),
                ("2", RunStatus::Completed)
            ]
        );
        assert_eq!(merged.total, Some(3));
        assert_eq!(rfc3339(1_763_100_000).as_str(), "2025-11-14T06:00:00Z");
    }

    #[test]
    fn every_tenth_refresh_is_full() {
        let fulls = (0..21).filter(|_| full_refresh("incremental-test")).count();
        assert_eq!(fulls, 3);
    }
}
