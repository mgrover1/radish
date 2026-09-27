//! Per-sweep orchestration: wires `intervals` -> `regions` -> `edges` ->
//! `network` into the same pipeline `dealias_region_based`'s sweep loop
//! runs (`pyart/correct/region_dealias.py`), producing per-gate fold
//! counts. The `ref_vel_field` sounding-anchoring path (L-BFGS-B) is
//! deliberately NOT ported — it's rarely used and has no obvious
//! wasm-friendly pure-Rust story (see `super`'s (`dealias/mod.rs`) module
//! doc for what is in scope).

use ndarray::Array2;

use crate::Result;

use super::edges::edge_sum_and_count;
use super::intervals::find_sweep_interval_splits;
use super::network::{combine_regions, EdgeTracker, RegionTracker};
use super::regions::find_regions;
use super::DealiasOptions;

/// Per-gate fold counts for one sweep. `valid[[r, c]] == true` means the
/// gate is usable; `false` (matching a source `gfilter` exclusion) always
/// gets fold `0` and never participates in region-finding.
///
/// Mirrors the source's early-outs exactly: fewer than 2 regions, or zero
/// edges between regions, means no unfolding is possible or needed for
/// this sweep — both return an all-zero fold array rather than doing any
/// network reduction.
///
/// # Errors
///
/// Propagates [`find_sweep_interval_splits`]'s [`RadishError::Dealias`](crate::RadishError::Dealias)
/// if the requested interval limits would be unbounded (see that
/// function's doc) — the only fallible step in this pipeline.
pub(crate) fn dealias_sweep(
    velocity: &Array2<f32>,
    valid: &Array2<bool>,
    nyquist: f32,
    rays_wrap_around: bool,
    opts: &DealiasOptions,
) -> Result<Array2<i32>> {
    let shape = velocity.dim();
    let mut folds = Array2::<i32>::zeros(shape);

    let nyquist_interval = nyquist as f64 * 2.0;

    let valid_velocities: Vec<f32> = velocity
        .iter()
        .zip(valid.iter())
        .filter_map(|(&v, &ok)| ok.then_some(v))
        .collect();
    let limits = find_sweep_interval_splits(nyquist, opts.interval_splits, &valid_velocities)?;

    let (labels, n_features) = find_regions(velocity, valid, &limits);
    if n_features < 2 {
        return Ok(folds);
    }

    // `np.bincount(labels.ravel())[1:]` — gate count per region label,
    // 1..=n_features (every ndimage.label-style region has >= 1 gate by
    // construction, so no zero-sized entries here).
    let mut region_sizes = vec![0i64; n_features + 1];
    for &l in labels.iter() {
        if l != 0 {
            region_sizes[l as usize] += 1;
        }
    }
    let region_sizes = &region_sizes[1..];

    let aggregated = edge_sum_and_count(
        &labels,
        velocity,
        rays_wrap_around,
        opts.skip_between_rays,
        opts.skip_along_ray,
    );
    if aggregated.is_empty() {
        return Ok(folds);
    }

    let mut region_tracker = RegionTracker::new(region_sizes);
    let mut edge_tracker = EdgeTracker::new(&aggregated, nyquist_interval, n_features + 1);
    while !combine_regions(&mut region_tracker, &mut edge_tracker) {}

    if opts.centered {
        let gates_dealiased: i64 = region_sizes.iter().sum();
        let total_folds: i64 = region_sizes
            .iter()
            .zip(region_tracker.unwrap_number[1..].iter())
            .map(|(&size, &unwrap)| size * unwrap as i64)
            .sum();
        // Python's built-in `round()` on a float (NOT `np.round`, but
        // the same round-half-to-even semantics since Python 3) —
        // replication point 5 again, at the sweep-centering step.
        let sweep_offset = (total_folds as f64 / gates_dealiased as f64).round_ties_even() as i32;
        if sweep_offset != 0 {
            for u in region_tracker.unwrap_number.iter_mut() {
                *u -= sweep_offset;
            }
        }
    }

    // `nwrap = np.take(region_tracker.unwrap_number, labels)`, EXCEPT for
    // one deliberate departure: a masked gate is forced to fold `0`
    // rather than reading `unwrap_number[0]` verbatim.
    //
    // This is NOT the same as "node 0 is never touched" (an assumption an
    // earlier version of this function relied on and was wrong to rely
    // on) — the `centered` step just above mutates `unwrap_number` as a
    // WHOLE array (`region_tracker.unwrap_number -= sweep_offset`, no
    // index-0 exclusion, exactly matching the source's
    // `region_tracker.unwrap_number -= sweep_offset`), so `unwrap_number[0]`
    // ends up equal to `-sweep_offset` whenever centering applies a
    // nonzero offset — verified empirically against a real Py-ART run
    // this session (`unwrap_number` ending in `[1, 1, 0, 1, 1, 0, 0, 0,
    // 0]` for a case with `sweep_offset = -1`, i.e. index 0 is NOT 0).
    //
    // Py-ART's own output doesn't surface this: the final field is
    // wrapped in `np.ma.array(data, mask=gfilter)`, so a masked gate's
    // underlying value (built from this same leftover `unwrap_number[0]`)
    // is simply never read by any well-behaved consumer — masked-gate
    // bit-exactness is explicitly outside this port's contract (see
    // `mod.rs`'s doc — the bar is unmasked gates only). Forcing `0` here
    // instead of leaking that internal leftover value is a deliberate,
    // small improvement: a caller that reads `folds` without checking
    // `mask` first gets a harmless, deterministic `0` rather than an
    // implementation-detail-dependent number.
    for ((r, c), out) in folds.indexed_iter_mut() {
        *out = if valid[[r, c]] {
            region_tracker.unwrap_number[labels[[r, c]] as usize]
        } else {
            0
        };
    }

    Ok(folds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> DealiasOptions {
        DealiasOptions::default()
    }

    #[test]
    fn all_masked_sweep_yields_all_zero_folds() {
        let velocity = Array2::from_elem((4, 4), 0.0f32);
        let valid = Array2::from_elem((4, 4), false);
        let folds = dealias_sweep(&velocity, &valid, 10.0, true, &opts()).unwrap();
        assert!(folds.iter().all(|&f| f == 0));
    }

    #[test]
    fn single_gate_sweep_does_not_panic() {
        // A 1x1 grid forces every one of fast_edge_finder's four
        // neighbor branches (left/right/top/bottom) into its boundary
        // condition simultaneously, including the ray-axis self-wrap
        // (with rays_wrap_around=true, the single ray's "left" and
        // "right" neighbor both wrap around to itself).
        let velocity = Array2::from_elem((1, 1), 3.0f32);
        let valid = Array2::from_elem((1, 1), true);
        let folds = dealias_sweep(&velocity, &valid, 10.0, true, &opts()).unwrap();
        assert_eq!(folds, Array2::from_elem((1, 1), 0));
    }

    #[test]
    fn single_region_sweep_yields_all_zero_folds() {
        // Every valid gate the same velocity -> one region -> n_features
        // < 2 -> early return, no unfolding attempted.
        let velocity = Array2::from_elem((4, 4), 3.0f32);
        let valid = Array2::from_elem((4, 4), true);
        let folds = dealias_sweep(&velocity, &valid, 10.0, true, &opts()).unwrap();
        assert!(folds.iter().all(|&f| f == 0));
    }

    #[test]
    fn two_regions_straddling_a_fold_get_unfolded_toward_each_other() {
        // A ray of gates aliased across the Nyquist boundary: left half
        // near +Nyquist, right half near -Nyquist (a classic single-fold
        // pattern). With centered=true the two regions should end up
        // folded toward a consistent, continuous velocity.
        let nyquist = 10.0f32;
        let mut velocity = Array2::<f32>::zeros((1, 8));
        for c in 0..4 {
            velocity[[0, c]] = 9.0; // near +Nyquist
        }
        for c in 4..8 {
            velocity[[0, c]] = -9.0; // near -Nyquist (aliased -11 folded up)
        }
        let valid = Array2::from_elem((1, 8), true);
        let folds = dealias_sweep(&velocity, &valid, nyquist, false, &opts()).unwrap();

        // Bit-exact against a REAL Py-ART run on this exact input this
        // session (not hand-derived): importing
        // `pyart.correct.region_dealias`'s internal
        // `_find_regions`/`_edge_sum_and_count`/`_RegionTracker`/
        // `_EdgeTracker`/`_combine_regions` directly and driving them the
        // same way `dealias_region_based`'s sweep loop does, on this
        // exact `vel`/`nyquist`/`interval_splits` (defaults: 3 splits,
        // skip=100/100, centered=true) produces
        // `folds = [[-1,-1,-1,-1,0,0,0,0]]` — the left region (velocity
        // +9, aliased) unfolds by -1 (-> corrected -11), the right region
        // (velocity -9) stays put. This is the actual Phase 5 exit
        // criterion: a known fold pattern, verified against Py-ART
        // itself rather than just "looks continuous".
        let expected = Array2::from_shape_vec((1, 8), vec![-1, -1, -1, -1, 0, 0, 0, 0]).unwrap();
        assert_eq!(folds, expected);
    }

    #[test]
    fn masked_gates_always_fold_zero_even_inside_a_dealiased_sweep() {
        let nyquist = 10.0f32;
        let mut velocity = Array2::<f32>::zeros((1, 8));
        for c in 0..4 {
            velocity[[0, c]] = 9.0;
        }
        for c in 4..8 {
            velocity[[0, c]] = -9.0;
        }
        let mut valid = Array2::from_elem((1, 8), true);
        valid[[0, 2]] = false;
        let folds = dealias_sweep(&velocity, &valid, nyquist, false, &opts()).unwrap();
        assert_eq!(folds[[0, 2]], 0);
    }

    /// A larger, denser cross-check against real Py-ART, exercising more
    /// of the pipeline than the single-ray tests above: 6 rays x 10 bins,
    /// a sinusoidal velocity field that aliases across the Nyquist
    /// boundary multiple times (8 regions), a masked patch NOT touching
    /// any edge, a masked gate at ray 5 bin 0 (right where ray wrap-around
    /// matters), and `rays_wrap_around = true`. Generated this session by
    /// driving `pyart.correct.region_dealias`'s actual internal functions
    /// (`_find_regions`/`_edge_sum_and_count`/`_RegionTracker`/
    /// `_EdgeTracker`/`_combine_regions`) on this exact array — not
    /// hand-derived — so this is real evidence the lexsort/dedup,
    /// tie-breaking, and merge-rule replication points hold on a case
    /// with actual ties and multi-region merges, ahead of Phase 6's
    /// formal golden-corpus gate.
    ///
    /// This exact case is what caught a real bug during development: the
    /// centering step's `sweep_offset` here is `-1` (nonzero), which
    /// exposed that `unwrap_number[0]` does NOT stay `0` in Py-ART's own
    /// arrays once centering applies (it's a whole-array subtraction, no
    /// index-0 exclusion) — the earlier single-ray tests above all
    /// happen to have `sweep_offset == 0`, so they couldn't have caught
    /// this. `dealias_sweep`'s final assignment loop now masks explicitly
    /// rather than relying on that false assumption — see its comment.
    #[test]
    // These literals are copy-pasted verbatim from a real numpy float32
    // array's repr (see the doc comment above) — deliberately NOT
    // truncated to clippy's suggested shorter form, since this test's
    // whole point is reproducing the exact input Py-ART was run on.
    #[allow(clippy::excessive_precision)]
    fn matches_a_real_pyart_run_on_a_denser_multi_region_wrapped_sweep() {
        #[rustfmt::skip]
        let velocity = Array2::from_shape_vec((6, 10), vec![
            0.000000, 3.546242, 6.775710, 9.399923, 11.184469, 11.969940, 11.686172, 10.358512, 8.105558, 5.128559,
            10.392304, 11.701269, 11.964994, 11.159922, 9.357966, 6.720092, 3.481932, -0.067258, -3.610441, -6.831114,
            10.392304, 8.155026, 5.189284, 1.759999, -1.826502, -5.249847, -8.204239, -10.425771, -11.716000, -11.959673,
            0.000000, -3.546242, -6.775710, -9.399923, -11.184469, -11.969940, -11.686172, -10.358512, -8.105558, -5.128559,
            -10.392304, -11.701269, -11.964994, -11.159922, -9.357966, -6.720092, -3.481932, 0.067258, 3.610441, 6.831114,
            -10.392304, -8.155026, -5.189284, -1.759999, 1.826502, 5.249847, 8.204239, 10.425771, 11.716000, 11.959673,
        ]).unwrap();
        #[rustfmt::skip]
        let valid = Array2::from_shape_vec((6, 10), vec![
            true, true, true, true, true, true, true, true, true, true,
            true, true, true, true, true, true, true, true, true, true,
            true, true, true, false, false, false, true, true, true, true,
            true, true, true, true, true, true, true, true, true, true,
            true, true, true, true, true, true, true, true, true, true,
            false, true, true, true, true, true, true, true, true, true,
        ]).unwrap();

        let folds = dealias_sweep(&velocity, &valid, 15.0, true, &opts()).unwrap();

        #[rustfmt::skip]
        let expected = Array2::from_shape_vec((6, 10), vec![
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 1, 1, 1, 1,
            0, 0, 0, 0, 0, 0, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 0, 0, 0, 0,
            0, 1, 1, 0, 0, 0, 0, 0, 0, 0,
        ]).unwrap();
        assert_eq!(folds, expected);
    }
}
