//! In-flight coalescing: identical reads that overlap share one fetch. The
//! interactive worker and the warm worker both read through here, so a prefetch the warm
//! worker already started is joined by the screen that opens the same item instead of being
//! sent twice. Identity is the forge (API base), the repository, the `Read` and its
//! `Conditional`; the first caller fetches and every caller that arrives before it finishes
//! receives a clone of the same result.

use crate::provider::Conditional;
use crate::provider::Fetched;
use crate::provider::ForgeContext;
use crate::provider::Provider;
use crate::request::ForgeError;
use crate::request::Read;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

type Shared = Result<Fetched, ForgeError>;

#[derive(Default)]
struct Slot {
    result: Mutex<Option<Shared>>,
    ready: Condvar,
    /// Callers waiting on this fetch instead of sending their own.
    followers: AtomicUsize,
}

fn flights() -> &'static Mutex<HashMap<String, Arc<Slot>>> {
    static FLIGHTS: OnceLock<Mutex<HashMap<String, Arc<Slot>>>> = OnceLock::new();
    FLIGHTS.get_or_init(|| Mutex::new(HashMap::new()))
}

static JOINED: AtomicU64 = AtomicU64::new(0);

/// How many reads this process answered by joining another's fetch.
pub fn joined() -> u64 {
    JOINED.load(Ordering::Relaxed)
}

fn key(ctx: &ForgeContext, read: &Read, cond: &Conditional) -> String {
    format!(
        "{}|{}|{}|{:?}",
        ctx.endpoints.api_base,
        ctx.repo
            .as_ref()
            .map(|repo| repo.full())
            .unwrap_or_default(),
        read.cache_key(),
        cond
    )
}

/// How many callers are waiting on the fetch of this read that is on its way now (0 when
/// none is). Unlike `joined`, which counts the whole process, it sees one flight only.
pub fn followers(ctx: &ForgeContext, read: &Read, cond: &Conditional) -> usize {
    flights()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key(ctx, read, cond))
        .map_or(0, |slot| slot.followers.load(Ordering::Acquire))
}

/// `provider.read`, shared with any identical read already on its way.
pub fn read(
    provider: &dyn Provider,
    ctx: &ForgeContext,
    read: &Read,
    cond: &Conditional,
    cancel: &AtomicBool,
) -> Shared {
    let key = key(ctx, read, cond);
    let (slot, leader) = {
        let mut flights = flights().lock().unwrap_or_else(|error| error.into_inner());
        match flights.get(&key) {
            Some(slot) => {
                slot.followers.fetch_add(1, Ordering::AcqRel);
                (slot.clone(), false)
            }
            None => {
                let slot = Arc::new(Slot::default());
                flights.insert(key.clone(), slot.clone());
                (slot, true)
            }
        }
    };
    if leader {
        let result = provider.read(ctx, read, cond, cancel);
        // A cancelled leader must not hand its cancellation to followers that still want
        // the answer; they retry on their own.
        let shared = match &result {
            Err(ForgeError::Cancelled) => None,
            result => Some(result.clone()),
        };
        flights()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&key);
        let mut stored = slot
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *stored = Some(shared.unwrap_or(Err(ForgeError::Cancelled)));
        slot.ready.notify_all();
        return result;
    }
    JOINED.fetch_add(1, Ordering::Relaxed);
    let mut stored = slot
        .result
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    loop {
        if let Some(result) = stored.as_ref() {
            let retry =
                matches!(result, Err(ForgeError::Cancelled)) && !cancel.load(Ordering::Acquire);
            if !retry {
                return result.clone();
            }
            drop(stored);
            return provider.read(ctx, read, cond, cancel);
        }
        if cancel.load(Ordering::Acquire) {
            return Err(ForgeError::Cancelled);
        }
        stored = slot
            .ready
            .wait_timeout(stored, Duration::from_millis(50))
            .unwrap_or_else(|error| error.into_inner())
            .0;
    }
}
