//! Fan-out helper shared by every backend that walks a slice of records or
//! sweeps and wants "parallel where we can, serial where we can't."
//!
//! `rayon`'s thread pool lives behind the `native` feature (bundled with
//! `hdf5`/`netcdf` — see `radish/Cargo.toml`), because a `wasm32-unknown-unknown`
//! build has no threads without `wasm-bindgen-rayon` plus COOP/COEP response
//! headers, a deployment constraint this crate deliberately doesn't impose on
//! a static-hosted free tier (`docs/NEXRAD_LEVEL3_WASM.md` §5). Every backend
//! that used to call `.par_iter()` directly now calls [`par_map`] or
//! [`par_enumerate_map`] instead, so the wasm fallback lives in one place
//! rather than six `#[cfg(...)]` blocks scattered across `nexrad`/`sigmet`.
//!
//! Both helpers short-circuit on the first `Err` (matching `Iterator::collect`
//! / `rayon::iter::ParallelIterator::collect` into a `Result<Vec<_>, _>`) and
//! preserve input order either way.

/// Map `f` over `items`, parallel via rayon under the `native` feature,
/// serial otherwise. Short-circuits on the first `Err`, preserves order.
pub(crate) fn par_map<T, U, E, F>(items: &[T], f: F) -> Result<Vec<U>, E>
where
    T: Sync,
    U: Send,
    E: Send,
    F: Fn(&T) -> Result<U, E> + Sync + Send,
{
    #[cfg(feature = "native")]
    {
        use rayon::prelude::*;
        items.par_iter().map(f).collect()
    }
    #[cfg(not(feature = "native"))]
    {
        items.iter().map(f).collect()
    }
}

/// Same as [`par_map`], but `f` also receives each item's index — covers
/// call sites that need the item's position (e.g. per-sweep conversion,
/// where the sweep index feeds an elevation-cut lookup).
pub(crate) fn par_enumerate_map<T, U, E, F>(items: &[T], f: F) -> Result<Vec<U>, E>
where
    T: Sync,
    U: Send,
    E: Send,
    F: Fn(usize, &T) -> Result<U, E> + Sync + Send,
{
    #[cfg(feature = "native")]
    {
        use rayon::prelude::*;
        items
            .par_iter()
            .enumerate()
            .map(|(idx, item)| f(idx, item))
            .collect()
    }
    #[cfg(not(feature = "native"))]
    {
        items
            .iter()
            .enumerate()
            .map(|(idx, item)| f(idx, item))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn par_map_preserves_order_and_short_circuits() {
        let items = vec![1, 2, 3, 4, 5];
        let out: Result<Vec<i32>, &'static str> =
            par_map(&items, |x| if *x == 4 { Err("boom") } else { Ok(x * 2) });
        assert_eq!(out, Err("boom"));

        let ok: Result<Vec<i32>, &'static str> = par_map(&items, |x| Ok(x * 2));
        assert_eq!(ok, Ok(vec![2, 4, 6, 8, 10]));
    }

    #[test]
    fn par_enumerate_map_passes_index() {
        let items = vec!["a", "b", "c"];
        let out: Result<Vec<String>, &'static str> =
            par_enumerate_map(&items, |idx, item| Ok(format!("{idx}:{item}")));
        assert_eq!(
            out,
            Ok(vec![
                "0:a".to_string(),
                "1:b".to_string(),
                "2:c".to_string()
            ])
        );
    }
}
