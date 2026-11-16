//! Independent requests of one read sent at once instead of one after another (FORGE.md §7):
//! a round trip to a distant forge costs the same whether one request or several are in
//! flight, so a pull request's files, commits, reviews and checks arrive together. The
//! transport still bounds how many requests a host sees at once.
use std::thread::ScopedJoinHandle;

/// The most requests one read sends at once, below every transport's per-host bound.
pub(super) const WIDTH: usize = 4;

/// The value of a scoped thread, re-raising its panic here.
pub(super) fn joined<T>(handle: ScopedJoinHandle<'_, T>) -> T {
    handle.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// `work` over every item, at most `WIDTH` at once, results in the items' order.
pub(super) fn map<T: Sync, R: Send>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let work = &work;
    let mut results = Vec::with_capacity(items.len());
    for chunk in items.chunks(WIDTH) {
        std::thread::scope(|scope| {
            let handles = chunk.iter().map(|item| scope.spawn(move || work(item))).collect::<Vec<_>>();
            results.extend(handles.into_iter().map(joined));
        });
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn map_keeps_order_and_bounds_how_many_run_at_once() {
        let (now, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let items = (0..10).collect::<Vec<_>>();
        let results = map(&items, |item| {
            let running = now.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(running, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            now.fetch_sub(1, Ordering::SeqCst);
            item * 2
        });
        assert_eq!(results, items.iter().map(|item| item * 2).collect::<Vec<_>>());
        let most = most.load(Ordering::SeqCst);
        assert!((2..=WIDTH).contains(&most), "{most} at once");
    }
}
