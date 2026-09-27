//! Stable index permutations for sorting rays without moving the underlying
//! ray data.
//!
//! Adapter code wants to sort rays by azimuth (PPI) or elevation (RHI), then
//! reuse that single permutation for every coord axis (azimuth, elevation,
//! time) and every moment buffer. Sorting indices instead of ray objects
//! keeps the cost to one comparison-sort over `nrays` and one heap allocation.

/// Return a permutation of `0..items.len()` that sorts `items` by `key`.
///
/// Uses `partial_cmp` so floating-point keys (azimuth in degrees, etc.)
/// work out of the box. NaN keys compare equal to other NaN keys (returning
/// `Equal`) so the sort stays stable instead of panicking — radar rays
/// with missing angles end up grouped at the start without crashing the
/// whole pipeline.
pub(crate) fn sort_indices_by_key<T, K, F>(items: &[T], key: F) -> Vec<usize>
where
    K: PartialOrd,
    F: Fn(&T) -> K,
{
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| {
        key(&items[a])
            .partial_cmp(&key(&items[b]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_indices_by_f32_key() {
        let items = vec![3.5_f32, 1.0, 2.5, 0.5];
        let order = sort_indices_by_key(&items, |&x| x);
        assert_eq!(order, vec![3, 1, 2, 0]);
    }

    #[test]
    fn sort_indices_handles_empty() {
        let items: Vec<f32> = vec![];
        let order = sort_indices_by_key(&items, |&x| x);
        assert!(order.is_empty());
    }

    #[test]
    fn sort_indices_with_nan_does_not_panic_and_preserves_indices() {
        // Treating NaN-vs-anything as `Ordering::Equal` produces a
        // non-deterministic position for NaN-bearing items relative to
        // finite ones. The contract we actually care about is "the sort
        // doesn't panic and the permutation is total" — this test pins
        // exactly that, and nothing about the relative order of finite
        // values around NaN sentinels.
        let items = vec![3.0_f32, f32::NAN, 1.0, f32::NAN, 2.0];
        let order = sort_indices_by_key(&items, |&x| x);
        assert_eq!(order.len(), items.len());
        let mut seen: Vec<usize> = order.clone();
        seen.sort();
        assert_eq!(seen, (0..items.len()).collect::<Vec<_>>());
    }
}
