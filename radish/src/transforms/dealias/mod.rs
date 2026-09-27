//! Region-based velocity dealiasing — a Rust port of Py-ART's
//! `pyart.correct.dealias_region_based` (`region_dealias.py` +
//! `_fast_edge_finder.pyx`), reachable from both the `python` and `wasm`
//! bindings. `docs/ARCHITECTURE.md` already named `transforms/` as
//! dealiasing's eventual home; this is its first real content.
//!
//! See `docs/NEXRAD_LEVEL3_WASM.md` for the full design and why dealiasing
//! is in scope at all — a raw NEXRAD Level 3 velocity product is genuinely
//! folded, and a serverless, browser-only deployment (no server to run
//! Py-ART on) needs the same unfolding algorithm reachable from wasm. The
//! 8 correctness-critical replication points this module's submodules
//! each own:
//!
//! 1. [`label`] — 4-connected component labeling, numbered like
//!    `scipy.ndimage.label`.
//! 2. [`edges`] — the edge-finding scan order, gap tolerance, and azimuth
//!    wrap-around.
//! 3. [`edges`] — edge dedup via a stable sort + sequential summation,
//!    matching `np.lexsort` + `np.add.reduceat`.
//! 4. Spread across [`edges`]/[`network`] — the exact `f32 -> f64 -> f32`
//!    dtype chain the source's velocity sums go through.
//! 5. [`network`] — banker's rounding (`f64::round_ties_even`) everywhere
//!    the source's `np.round`/Python `round()` round.
//! 6. [`network`] — first-occurrence argmax tie-breaking.
//! 7. [`network`] — the edge/region merge rules.
//! 8. [`sweep`] — edge cases (all-masked, single-region, no-edges).
//!
//! **Deliberately not ported**: the `ref_vel_field` sounding-anchored
//! path (L-BFGS-B reference-velocity fitting) — rarely used, and no
//! obvious pure-Rust/wasm-friendly story.
//!
//! **Not a decode-time output.** Unlike `MomentData::raw_codes` etc.,
//! dealiasing is a transform a caller runs on demand — there is no new
//! field on any decode-side model type. This module returns the
//! load-bearing, bit-exact-with-Py-ART primitive (per-gate fold counts);
//! turning that into a corrected velocity array
//! (`raw + fold_count as f32 * 2.0 * nyquist`) is one cheap multiply-add a
//! caller does itself.

mod edges;
mod intervals;
mod label;
mod network;
mod regions;
mod sweep;

use ndarray::Array2;

use crate::{RadishError, Result};

/// Tuning parameters for [`dealias_region_based`]. Defaults match
/// Py-ART's own (`pyart.correct.dealias_region_based`'s keyword defaults).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DealiasOptions {
    /// Number of equal-sized segments to split the Nyquist interval into
    /// when finding same-velocity regions. More splits finds more,
    /// smaller initial regions (slower, can dealias more precisely);
    /// fewer splits is faster but coarser. Must be `>= 1`.
    pub interval_splits: usize,
    /// Maximum number of consecutive masked/invalid gates to skip over
    /// when looking for a region to connect to across a ray boundary
    /// (the azimuth axis). `0` disables bridging across masked gates in
    /// this direction entirely.
    pub skip_between_rays: i32,
    /// Same as [`skip_between_rays`](Self::skip_between_rays), but along
    /// a ray (the range/gate axis).
    pub skip_along_ray: i32,
    /// Re-center the sweep after unfolding so the average fold count is
    /// as close to zero as possible (round-half-to-even), rather than
    /// leaving the whole sweep systematically over- or under-folded by a
    /// constant offset.
    pub centered: bool,
}

impl Default for DealiasOptions {
    fn default() -> Self {
        Self {
            interval_splits: 3,
            skip_between_rays: 100,
            skip_along_ray: 100,
            centered: true,
        }
    }
}

/// Dealias one sweep's Doppler velocity using Py-ART's region-based
/// algorithm — bit-exact with `pyart.correct.dealias_region_based` on
/// every unmasked gate (see `radish/tests/test_dealias_parity.rs` for the
/// parity gate this is checked against).
///
/// Returns per-gate **fold counts**, not corrected velocities: gate
/// `(r, c)`'s corrected velocity is `velocity[[r, c]] + folds[[r, c]]
/// as f32 * 2.0 * nyquist`. Returning folds rather than the corrected
/// array keeps the bit-exactness contract on the integer-valued,
/// unambiguous quantity — multiplying by `2.0 * nyquist` afterward is
/// exact for any real fold count.
///
/// `valid_mask[[r, c]] == true` means gate `(r, c)` is **valid** and
/// should be dealiased — the opposite polarity of Py-ART's `gfilter`
/// (which excludes on `true`); chosen to match the more common Rust
/// convention of a mask marking usable data, not excluded data. A
/// masked-out (`false`) gate always gets fold `0`.
///
/// # Errors
///
/// Returns [`RadishError::Dealias`] if:
/// - `velocity` and `valid_mask` have different shapes,
/// - `nyquist` is not finite and positive,
/// - `opts.interval_splits == 0`,
/// - `opts.skip_between_rays`/`opts.skip_along_ray` are negative, or
///   exceed the sweep's own ray/gate count (a gap search wider than the
///   grid itself is never meaningful, and — found via adversarial review
///   this session — was otherwise an unbounded CPU-hang vector: these
///   options were previously unvalidated `i32`s feeding a raw gap-search
///   loop reachable straight from the wasm/PyO3 boundary),
/// - any velocity value at a valid gate is non-finite (`NaN`/`Infinity`
///   silently produced a wrong-but-plausible-looking fold count rather
///   than an error before this check existed — also found via
///   adversarial review), or
/// - the requested Nyquist-interval limits would be unbounded (see
///   [`intervals::find_sweep_interval_splits`]'s doc — the same review
///   pass found a single ordinary-but-extreme finite velocity, or an
///   oversized `interval_splits` alone, could otherwise request an
///   unbounded allocation and abort the process instead of returning a
///   catchable error).
pub fn dealias_region_based(
    velocity: &Array2<f32>,
    valid_mask: &Array2<bool>,
    nyquist: f32,
    rays_wrap_around: bool,
    opts: DealiasOptions,
) -> Result<Array2<i32>> {
    if velocity.dim() != valid_mask.dim() {
        return Err(RadishError::Dealias(format!(
            "velocity shape {:?} does not match mask shape {:?}",
            velocity.dim(),
            valid_mask.dim()
        )));
    }
    if !nyquist.is_finite() || nyquist <= 0.0 {
        return Err(RadishError::Dealias(format!(
            "nyquist must be finite and positive, got {nyquist}"
        )));
    }
    if opts.interval_splits == 0 {
        return Err(RadishError::Dealias(
            "interval_splits must be at least 1".to_string(),
        ));
    }
    let (n_rays, n_gates) = velocity.dim();
    if opts.skip_between_rays < 0 || opts.skip_along_ray < 0 {
        return Err(RadishError::Dealias(format!(
            "skip_between_rays and skip_along_ray must be non-negative, got {} and {}",
            opts.skip_between_rays, opts.skip_along_ray
        )));
    }
    // An empty sweep (either dimension 0) has no gates to gap-search over
    // regardless of the skip values, so the bound below is vacuous for
    // it — checking it anyway would reject the legitimate "decode an
    // empty sweep" case for exercising the DEFAULT skip values (100),
    // which are `> 0`.
    if n_rays > 0 && n_gates > 0 {
        if opts.skip_between_rays as usize > n_rays {
            return Err(RadishError::Dealias(format!(
                "skip_between_rays ({}) must not exceed this sweep's own ray count ({n_rays}) \
                 — a gap search wider than the grid itself is never meaningful",
                opts.skip_between_rays,
            )));
        }
        if opts.skip_along_ray as usize > n_gates {
            return Err(RadishError::Dealias(format!(
                "skip_along_ray ({}) must not exceed this sweep's own gate count ({n_gates}) \
                 — a gap search wider than the grid itself is never meaningful",
                opts.skip_along_ray,
            )));
        }
    }
    if velocity
        .iter()
        .zip(valid_mask.iter())
        .any(|(&v, &ok)| ok && !v.is_finite())
    {
        return Err(RadishError::Dealias(
            "velocity contains a NaN or infinite value at a valid (unmasked) gate — mask it \
             out instead of leaving it valid"
                .to_string(),
        ));
    }

    sweep::dealias_sweep(velocity, valid_mask, nyquist, rays_wrap_around, &opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_match_the_literals_pyo3_and_wasm_bindings_hardcode() {
        // `python/src/lib.rs`'s `dealias_region_based` pyfunction and
        // `wasm/src/lib.rs`'s `dealiasRegionBased` both hardcode
        // `interval_splits=3, skip_between_rays=100, skip_along_ray=100,
        // centered=true` as their own keyword/parameter defaults, kept in
        // sync with `Default` here BY HAND (documented at each call
        // site) — nothing else ties them together. This test at least
        // makes a change to `Default` here fail loudly in the one place
        // automation CAN check, rather than only silently drifting from
        // the two FFI layers' copies.
        assert_eq!(
            DealiasOptions::default(),
            DealiasOptions {
                interval_splits: 3,
                skip_between_rays: 100,
                skip_along_ray: 100,
                centered: true,
            }
        );
    }

    #[test]
    fn rejects_mismatched_shapes() {
        let velocity = Array2::<f32>::zeros((2, 2));
        let mask = Array2::<bool>::from_elem((3, 3), true);
        let err = dealias_region_based(&velocity, &mask, 10.0, true, DealiasOptions::default())
            .unwrap_err();
        assert!(matches!(err, RadishError::Dealias(_)));
    }

    #[test]
    fn rejects_non_finite_or_non_positive_nyquist() {
        let velocity = Array2::<f32>::zeros((2, 2));
        let mask = Array2::<bool>::from_elem((2, 2), true);
        for bad in [0.0f32, -5.0, f32::NAN, f32::INFINITY] {
            let err = dealias_region_based(&velocity, &mask, bad, true, DealiasOptions::default())
                .unwrap_err();
            assert!(matches!(err, RadishError::Dealias(_)), "nyquist={bad}");
        }
    }

    #[test]
    fn rejects_zero_interval_splits() {
        let velocity = Array2::<f32>::zeros((2, 2));
        let mask = Array2::<bool>::from_elem((2, 2), true);
        let opts = DealiasOptions {
            interval_splits: 0,
            ..Default::default()
        };
        let err = dealias_region_based(&velocity, &mask, 10.0, true, opts).unwrap_err();
        assert!(matches!(err, RadishError::Dealias(_)));
    }

    // The following regression tests pin the 3 findings from this
    // session's adversarial review that were reachable straight from the
    // untrusted wasm/PyO3 boundary — see the `# Errors` doc above.

    #[test]
    fn rejects_negative_skip_values() {
        let velocity = Array2::<f32>::zeros((4, 4));
        let mask = Array2::<bool>::from_elem((4, 4), true);
        let opts = DealiasOptions {
            skip_between_rays: -1,
            ..Default::default()
        };
        let err = dealias_region_based(&velocity, &mask, 10.0, true, opts).unwrap_err();
        assert!(matches!(err, RadishError::Dealias(_)));
    }

    #[test]
    fn rejects_skip_values_larger_than_the_sweep_itself() {
        // Found via adversarial review this session: skip_between_rays /
        // skip_along_ray fed an unbounded gap-search loop
        // (edges.rs::fast_edge_finder) with no upper bound at all — an
        // `i32::MAX` skip with a mostly-masked real-sized sweep was a
        // ~10^13-iteration CPU hang, not a crash, and not catchable by
        // the caller. Bounding skip to the sweep's own dimensions closes
        // this without changing behavior for any legitimate caller (the
        // Py-ART default is 100, far below any real sweep's ray/gate
        // count).
        let velocity = Array2::<f32>::zeros((4, 4));
        let mask = Array2::<bool>::from_elem((4, 4), true);
        let opts = DealiasOptions {
            skip_between_rays: i32::MAX,
            ..Default::default()
        };
        let err = dealias_region_based(&velocity, &mask, 10.0, true, opts).unwrap_err();
        assert!(matches!(err, RadishError::Dealias(_)));
    }

    #[test]
    fn empty_sweep_is_exempt_from_the_skip_vs_dimension_bound() {
        // A 0x0 sweep has no gates to gap-search over, so the default
        // skip values (100) — which exceed the (0, 0) "dimensions" —
        // must not be rejected; see mod.rs's `n_rays > 0 && n_gates > 0`
        // guard on the check above.
        let velocity = Array2::<f32>::zeros((0, 0));
        let mask = Array2::<bool>::from_elem((0, 0), true);
        let folds =
            dealias_region_based(&velocity, &mask, 10.0, true, DealiasOptions::default()).unwrap();
        assert_eq!(folds.dim(), (0, 0));
    }

    #[test]
    fn rejects_nan_or_infinite_velocity_at_a_valid_gate() {
        // Found via adversarial review this session: a NaN velocity at a
        // valid gate never lands in any interval bucket
        // (`lmin <= v && v < lmax` is always false for NaN), so it stays
        // labeled as background — indistinguishable from a masked gate —
        // and reads `unwrap_number[0]`, which is NOT guaranteed to be 0
        // once sweep-centering applies (see sweep.rs's doc). Net effect
        // before this check: a silently wrong, undocumented fold count
        // rather than an error, for exactly the "valid but NaN" input
        // class an untrusted wasm caller can send.
        let mut velocity = Array2::<f32>::zeros((4, 4));
        velocity[[1, 1]] = f32::NAN;
        let mask = Array2::<bool>::from_elem((4, 4), true);
        let err = dealias_region_based(&velocity, &mask, 10.0, true, DealiasOptions::default())
            .unwrap_err();
        assert!(matches!(err, RadishError::Dealias(_)));

        let mut velocity = Array2::<f32>::zeros((4, 4));
        velocity[[1, 1]] = f32::INFINITY;
        let err = dealias_region_based(&velocity, &mask, 10.0, true, DealiasOptions::default())
            .unwrap_err();
        assert!(matches!(err, RadishError::Dealias(_)));
    }

    #[test]
    fn nan_velocity_at_a_masked_gate_is_not_an_error() {
        // Only VALID gates are checked for finiteness — a NaN at a
        // masked-out gate is exactly what "no data here" looks like on
        // the wire (radish's own NEXRAD Level 3 decoder uses NaN for
        // below-threshold/range-folded codes) and must not be rejected.
        //
        // Sized 100x100 (not 4x4) so the default `skip_between_rays`/
        // `skip_along_ray` (100) satisfy the separate skip-vs-dimension
        // bound above and don't fail this test for an unrelated reason.
        let mut velocity = Array2::<f32>::zeros((100, 100));
        velocity[[1, 1]] = f32::NAN;
        let mut mask = Array2::<bool>::from_elem((100, 100), true);
        mask[[1, 1]] = false;
        dealias_region_based(&velocity, &mask, 10.0, true, DealiasOptions::default())
            .expect("NaN at a masked gate must not be rejected");
    }

    #[test]
    fn empty_sweep_returns_all_zero_folds_not_an_error() {
        let velocity = Array2::<f32>::zeros((0, 0));
        let mask = Array2::<bool>::from_elem((0, 0), true);
        let folds =
            dealias_region_based(&velocity, &mask, 10.0, true, DealiasOptions::default()).unwrap();
        assert_eq!(folds.dim(), (0, 0));
    }
}
