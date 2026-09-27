//! Nyquist-interval segmentation, porting Py-ART's
//! `_find_sweep_interval_splits` (`pyart/correct/region_dealias.py`)
//! byte-for-byte in arithmetic terms.
//!
//! Runs entirely in `f64`, matching the source: Py-ART's `nyquist_vel` is
//! a Python float (== float64) and `np.linspace` returns `float64` by
//! default, even though the velocity *data* array being segmented is
//! `f32`. Widening the public API's `nyquist: f32` to `f64` here is
//! lossless (every `f32` value has an exact `f64` representation) —
//! narrowing would not be, which is why this crosses the boundary in this
//! direction and not the other.

use crate::{RadishError, Result};

/// Hard cap on how many interval limits [`find_sweep_interval_splits`]
/// will ever request. Far larger than any legitimate use needs
/// (`interval_splits` itself is typically `3`; even generous widening
/// for real out-of-Nyquist data needs at most a handful of extra
/// intervals) — this exists purely so a crafted `(velocity, nyquist,
/// interval_splits)` combination reachable from an untrusted wasm/PyO3
/// caller can't drive `add_start`/`add_end`/`interval_splits` into the
/// millions-to-billions and make [`linspace`] attempt an unbounded
/// `Vec<f64>` allocation — confirmed via adversarial review this session
/// to abort the whole process (not a catchable `Result`) on allocation
/// failure, exactly the class of bug
/// `docs/NEXRAD_LEVEL3_WASM.md` §4.7's decompression-bomb guard exists to
/// prevent elsewhere in this crate. Refuse rather than silently clamp —
/// matches this crate's "refuse, never silently rescale" convention
/// (`docs/NEXRAD_LEVEL3_WASM.md` §4.2): a clamped, degraded interval set
/// would still "succeed" while quietly computing on the wrong intervals.
const MAX_INTERVAL_LIMITS: i64 = 10_000;

/// Interval limits for [`super::regions::find_regions`], matching
/// `_find_sweep_interval_splits` exactly: normally `interval_splits + 1`
/// evenly spaced limits spanning `[-nyquist, nyquist]`, widened
/// (asymmetrically, if needed) to also cover any gates whose value
/// exceeds `±nyquist` — real data can do this even before dealiasing
/// starts, and Py-ART's own comment says so.
///
/// `valid_velocities` must already be filtered to the sweep's unmasked
/// gates only (`sdata[~sfilter]` in the source) — an empty slice mirrors
/// "no change from default if all gates filtered" in the source exactly
/// (`add_start = add_end = 0`, no widening).
///
/// # Errors
///
/// Returns [`RadishError::Dealias`] if the requested number of interval
/// limits would exceed [`MAX_INTERVAL_LIMITS`] — see its doc for why this
/// guard exists.
pub(crate) fn find_sweep_interval_splits(
    nyquist: f32,
    interval_splits: usize,
    valid_velocities: &[f32],
) -> Result<Vec<f64>> {
    let nyquist = nyquist as f64;
    let interval = (2.0 * nyquist) / interval_splits as f64;

    let mut add_start: i64 = 0;
    let mut add_end: i64 = 0;
    if let Some((min_vel, max_vel)) = min_max_f64(valid_velocities) {
        if max_vel > nyquist || min_vel < -nyquist {
            add_start = ((max_vel - nyquist) / interval).ceil() as i64;
            add_end = (-(min_vel + nyquist) / interval).ceil() as i64;
        }
    }

    let start = -nyquist - add_start as f64 * interval;
    let end = nyquist + add_end as f64 * interval;
    let num = interval_splits as i64 + 1 + add_start + add_end;
    if num > MAX_INTERVAL_LIMITS {
        return Err(RadishError::Dealias(format!(
            "requested {num} Nyquist interval limits (interval_splits={interval_splits}, \
             velocity range [{min_max}]), exceeding the {MAX_INTERVAL_LIMITS} cap — check \
             nyquist and interval_splits are physically sane for this velocity data",
            min_max = min_max_f64(valid_velocities)
                .map(|(lo, hi)| format!("{lo}, {hi}"))
                .unwrap_or_else(|| "no valid gates".to_string()),
        )));
    }
    Ok(linspace(start, end, num.max(0) as usize))
}

/// `(min, max)` of `values`, upcast to `f64` — matches `.max()`/`.min()`
/// on a float32 numpy array being compared against float64 scalars
/// (`max_vel > nyquist`): the comparison itself upcasts, so computing the
/// min/max in f64 from the start changes nothing observable but keeps
/// every step in the f64 domain the source's arithmetic lives in.
fn min_max_f64(values: &[f32]) -> Option<(f64, f64)> {
    if values.is_empty() {
        return None;
    }
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for &v in values {
        let v = v as f64;
        if v < min {
            min = v;
        }
        if v > max {
            max = v;
        }
    }
    Some((min, max))
}

/// `np.linspace(start, end, num, endpoint=True)`: `num` evenly spaced
/// points from `start` to `end` inclusive. `num == 0` and `num == 1`
/// match NumPy's own edge-case behavior (empty, and a single `start`
/// point respectively — NumPy does not error on `num == 0`).
fn linspace(start: f64, end: f64, num: usize) -> Vec<f64> {
    if num == 0 {
        return Vec::new();
    }
    if num == 1 {
        return vec![start];
    }
    let step = (end - start) / (num - 1) as f64;
    (0..num)
        .map(|i| {
            if i == num - 1 {
                end // exact endpoint, matching NumPy, not accumulated rounding
            } else {
                start + step * i as f64
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_widening_when_all_velocities_within_nyquist() {
        let limits = find_sweep_interval_splits(10.0, 3, &[-9.0, 0.0, 9.0]).unwrap();
        assert_eq!(limits.len(), 4);
        assert!((limits[0] - (-10.0)).abs() < 1e-9);
        assert!((limits[3] - 10.0).abs() < 1e-9);
        // Evenly spaced: interval = 20/3.
        assert!((limits[1] - (-10.0 + 20.0 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn empty_velocities_do_not_widen() {
        let limits = find_sweep_interval_splits(10.0, 3, &[]).unwrap();
        assert_eq!(limits.len(), 4);
        assert!((limits[0] - (-10.0)).abs() < 1e-9);
        assert!((limits[3] - 10.0).abs() < 1e-9);
    }

    #[test]
    fn widens_asymmetrically_for_out_of_nyquist_velocities() {
        // nyquist=10, splits=2 -> interval=10. A value of 25 is 15 above
        // nyquist -> add_start = ceil(15/10) = 2.
        //
        // Both add_start AND add_end are (re-)computed from the SAME
        // `if` branch once EITHER bound is exceeded — matching the
        // source's `_find_sweep_interval_splits` exactly, quirk
        // included: add_end is NOT clamped to >= 0, so a min_vel that's
        // comfortably inside the Nyquist interval (0, here) can still
        // pull add_end negative (ceil(-(0+10)/10) = ceil(-1.0) = -1),
        // net-shrinking the naive symmetric lower bound rather than
        // leaving it untouched. This was hand-verified against the
        // actual Python source, not assumed — a naively "fixed" version
        // (add_end clamped to 0) would silently diverge from Py-ART.
        let limits = find_sweep_interval_splits(10.0, 2, &[25.0, 0.0]).unwrap();
        // num = 2 + 1 + 2 + (-1) = 4, start = -10 - 2*10 = -30,
        // end = 10 + (-1)*10 = 0.
        assert_eq!(limits.len(), 4);
        assert!((limits[0] - (-30.0)).abs() < 1e-9);
        assert!((limits[3] - 0.0).abs() < 1e-9);
        // Cross-checked against a real `pyart.correct.region_dealias`
        // import this session: `_find_sweep_interval_splits(10.0, 2,
        // np.array([25.0, 0.0]), 0)` returns exactly `[-30, -20, -10, 0]`.
        assert_eq!(limits, vec![-30.0, -20.0, -10.0, 0.0]);
    }

    #[test]
    fn rejects_a_crafted_input_that_would_request_unbounded_interval_limits() {
        // Found via adversarial review this session: an ordinary finite
        // `f32` velocity outlier (no NaN/Infinity needed) drives
        // `add_start` past any reasonable bound once `nyquist` is small.
        // Before the MAX_INTERVAL_LIMITS guard this would have reached
        // `linspace`'s unconditional `Vec<f64>` allocation and aborted
        // the process instead of returning a catchable error.
        //
        // A second, in-bounds value (`0.0`) is deliberately included
        // alongside the `1e9` outlier: passing `&[1e9]` alone makes
        // `min_vel == max_vel == 1e9`, which drives `add_end` hugely
        // NEGATIVE (since `min_vel` is far ABOVE `-nyquist`, not below
        // it) at nearly the same magnitude `add_start` goes positive —
        // the two almost cancel in `num`'s sum, defeating the test's own
        // premise. `0.0` keeps `min_vel` inside `[-nyquist, nyquist]` so
        // only `add_start` blows up, matching the actual attack shape.
        let err = find_sweep_interval_splits(0.001, 3, &[1e9, 0.0]).unwrap_err();
        assert!(matches!(err, RadishError::Dealias(_)));
    }

    #[test]
    fn rejects_an_oversized_interval_splits_alone_even_with_ordinary_velocities() {
        // The other half of the same finding: `interval_splits` alone
        // (no extreme velocity needed) feeds directly into `num`.
        let err = find_sweep_interval_splits(10.0, 50_000, &[-9.0, 9.0]).unwrap_err();
        assert!(matches!(err, RadishError::Dealias(_)));
    }

    #[test]
    fn accepts_the_largest_interval_splits_that_stays_within_the_cap() {
        // MAX_INTERVAL_LIMITS is 10_000; interval_splits=9_999 with no
        // widening needed (num = 9_999 + 1 + 0 + 0 = 10_000) must still
        // succeed — the guard should reject only when the cap is
        // actually exceeded, not merely approached.
        let limits = find_sweep_interval_splits(10.0, 9_999, &[0.0]).unwrap();
        assert_eq!(limits.len(), 10_000);
    }

    #[test]
    fn linspace_matches_numpy_endpoint_true_semantics() {
        let v = linspace(-10.0, 10.0, 5);
        assert_eq!(v, vec![-10.0, -5.0, 0.0, 5.0, 10.0]);
    }

    #[test]
    fn linspace_num_zero_and_one_do_not_panic() {
        assert_eq!(linspace(0.0, 1.0, 0), Vec::<f64>::new());
        assert_eq!(linspace(0.0, 1.0, 1), vec![0.0]);
    }
}
