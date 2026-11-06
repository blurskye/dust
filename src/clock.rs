//! Time and monotonic identifier sources.

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

pub trait Clock: Send + Sync + 'static {
    fn system_time(&self) -> SystemTime;
    fn monotonic(&self) -> Instant;
}
#[derive(Default)]
pub struct SystemClock;
impl Clock for SystemClock {
    fn system_time(&self) -> SystemTime {
        SystemTime::now()
    }
    fn monotonic(&self) -> Instant {
        Instant::now()
    }
}
/// Explicitly advanced clock for deterministic session tests; reads never advance time.
pub struct ManualClock {
    system: SystemTime,
    monotonic: Instant,
    elapsed: RwLock<Duration>,
}
impl ManualClock {
    pub fn new(system: SystemTime, monotonic: Instant) -> Self {
        Self {
            system,
            monotonic,
            elapsed: RwLock::new(Duration::ZERO),
        }
    }
    pub fn advance(&self, elapsed: Duration) {
        let mut current = self.elapsed.write().expect("manual clock lock");
        *current = current.saturating_add(elapsed);
    }
}
impl Clock for ManualClock {
    fn system_time(&self) -> SystemTime {
        self.system + *self.elapsed.read().expect("manual clock lock")
    }
    fn monotonic(&self) -> Instant {
        self.monotonic + *self.elapsed.read().expect("manual clock lock")
    }
}

pub struct FixedClock {
    system: SystemTime,
    monotonic: Instant,
    step: Duration,
    ticks: AtomicU64,
}
impl FixedClock {
    pub fn new(system: SystemTime, monotonic: Instant, step: Duration) -> Self {
        Self {
            system,
            monotonic,
            step,
            ticks: AtomicU64::new(0),
        }
    }
    fn elapsed(&self) -> Duration {
        let ticks = self
            .ticks
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |ticks| {
                Some(ticks.saturating_add(1))
            })
            .expect("clock tick update");
        let nanos = self
            .step
            .as_nanos()
            .saturating_mul(u128::from(ticks))
            .min(Duration::MAX.as_nanos());
        Duration::new(
            (nanos / 1_000_000_000) as u64,
            (nanos % 1_000_000_000) as u32,
        )
    }
}
impl Clock for FixedClock {
    fn system_time(&self) -> SystemTime {
        self.system + self.elapsed()
    }
    fn monotonic(&self) -> Instant {
        self.monotonic + self.elapsed()
    }
}
fn owner() -> &'static RwLock<Arc<dyn Clock>> {
    static CLOCK: OnceLock<RwLock<Arc<dyn Clock>>> = OnceLock::new();
    CLOCK.get_or_init(|| RwLock::new(Arc::new(SystemClock)))
}
pub fn system_time() -> SystemTime {
    owner().read().expect("clock lock").system_time()
}
pub fn monotonic() -> Instant {
    owner().read().expect("clock lock").monotonic()
}
pub fn replace(clock: Arc<dyn Clock>) -> Arc<dyn Clock> {
    std::mem::replace(&mut *owner().write().expect("clock lock"), clock)
}
pub trait IdSource: Send + Sync {
    fn next(&self) -> u64;
}
pub struct AtomicIds {
    next: AtomicU64,
}
impl AtomicIds {
    pub const fn new(first: u64) -> Self {
        Self {
            next: AtomicU64::new(first),
        }
    }
}
impl IdSource for AtomicIds {
    fn next(&self) -> u64 {
        self.next.fetch_add(1, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use crate::clock::*;

    #[test]
    fn fixed_clock_keeps_all_tick_bits_and_saturates() {
        let clock = FixedClock::new(SystemTime::now(), Instant::now(), Duration::from_nanos(1));
        clock.ticks.store(u64::from(u32::MAX), Ordering::SeqCst);
        assert_eq!(clock.elapsed(), Duration::from_nanos(u64::from(u32::MAX)));
        assert_eq!(clock.elapsed(), Duration::from_nanos(1u64 << 32));
        clock.ticks.store(u64::MAX, Ordering::SeqCst);
        assert_eq!(clock.elapsed(), Duration::from_nanos(u64::MAX));
        assert_eq!(clock.elapsed(), Duration::from_nanos(u64::MAX));

        let clock = FixedClock::new(SystemTime::now(), Instant::now(), Duration::MAX);
        clock.ticks.store(2, Ordering::SeqCst);
        assert_eq!(clock.elapsed(), Duration::MAX);
    }
}
