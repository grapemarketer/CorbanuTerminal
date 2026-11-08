//! #1877: independent pieces of a check spread over a few threads, with the
//! results in order, so the check finds exactly what it found on one thread.
//! Plain `std`, so it is tested on every platform.

use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

/// Most threads [`par_map`] uses (as D312's read-mostly checks).
const MAX_THREADS: usize = 8;

/// `items` mapped by `map`, in order, on up to one thread per CPU (at most
/// [`MAX_THREADS`]; the calling thread is one of them). A panic in `map` is
/// raised again here once the other threads have stopped.
pub(crate) fn par_map<T: Sync, R: Send>(items: &[T], map: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .clamp(1, MAX_THREADS)
        .min(items.len());
    if threads <= 1 {
        return items.iter().map(map).collect();
    }
    // Each thread takes the next item in turn; each result keeps its place.
    let next = AtomicUsize::new(0);
    let work = || {
        let mut done = Vec::new();
        loop {
            let n = next.fetch_add(1, Ordering::Relaxed);
            let Some(item) = items.get(n) else {
                return done;
            };
            done.push((n, map(item)));
        }
    };
    let mut results: Vec<(usize, R)> = std::thread::scope(|scope| {
        // A thread that can't start just leaves more for the others.
        let others: Vec<_> = (1..threads)
            .filter_map(|_| std::thread::Builder::new().spawn_scoped(scope, &work).ok())
            .collect();
        let mut results = work();
        for other in others {
            match other.join() {
                Ok(more) => results.extend(more),
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        results
    });
    results.sort_unstable_by_key(|(n, _)| *n);
    results.into_iter().map(|(_, result)| result).collect()
}

#[cfg(test)]
#[path = "par_map_tests.rs"]
mod tests;
