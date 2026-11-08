use super::*;
use pretty_assertions::assert_eq;

/// Results come back in the items' order, whichever thread made them.
#[test]
fn results_keep_the_items_order() {
    let items: Vec<u64> = (0..1_000).collect();
    let mapped = par_map(&items, |n| {
        // Uneven work, so the threads finish out of order.
        std::thread::sleep(std::time::Duration::from_micros(n % 7 * 50));
        n * 2
    });
    assert_eq!(mapped, items.iter().map(|n| n * 2).collect::<Vec<_>>());
    assert_eq!(par_map(&[] as &[u64], |n| *n), Vec::<u64>::new());
}

/// A panic in the map is raised to the caller.
#[test]
fn a_panic_reaches_the_caller() {
    let items: Vec<u32> = (0..64).collect();
    let result = std::panic::catch_unwind(|| {
        par_map(&items, |n| {
            assert!(*n != 40, "boom");
            *n
        })
    });
    assert!(result.is_err());
}
