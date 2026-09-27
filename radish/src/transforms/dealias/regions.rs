//! Region finding: partition a sweep's velocities into same-Nyquist-band
//! connected regions. Ports Py-ART's `_find_regions`
//! (`pyart/correct/region_dealias.py`).

use ndarray::Array2;

use super::label::label_4connected;

/// For each consecutive pair of `limits`, find the 4-connected regions of
/// gates whose velocity falls in `[lmin, lmax)` AND are valid (`valid`,
/// **true** = usable gate — the inverse polarity of Py-ART's `gfilter`,
/// which excludes on `true`; see `mod.rs`'s public API doc for why this
/// crate uses the opposite convention). Each interval's regions are
/// numbered independently by [`label_4connected`] and then offset to
/// continue the running total, exactly matching the source's
/// `limit_label[nonzero] += nfeatures; label += limit_label`.
///
/// `vel` is compared against `limits` (`f64`) by upcasting each `f32`
/// gate value to `f64` — matching NumPy's automatic promotion when an
/// `f32` array is compared against `f64` scalars (see `intervals.rs`'s
/// module doc for why the limits are `f64` in the first place).
///
/// Returns `(labels, n_features)`: `labels[[r, c]] == 0` for masked gates
/// or gates that fell in none of the intervals (shouldn't happen for a
/// `limits` array spanning the true value range, but not assumed here);
/// `1..=n_features` otherwise.
pub(crate) fn find_regions(
    vel: &Array2<f32>,
    valid: &Array2<bool>,
    limits: &[f64],
) -> (Array2<i32>, usize) {
    let shape = vel.dim();
    let mut labels = Array2::<i32>::zeros(shape);
    let mut n_features = 0usize;

    for window in limits.windows(2) {
        let (lmin, lmax) = (window[0], window[1]);
        let inp = Array2::from_shape_fn(shape, |idx| {
            valid[idx] && {
                let v = vel[idx] as f64;
                lmin <= v && v < lmax
            }
        });
        let (limit_labels, limit_n) = label_4connected(&inp);
        for (out, &lbl) in labels.iter_mut().zip(limit_labels.iter()) {
            if lbl != 0 {
                *out = lbl + n_features as i32;
            }
        }
        n_features += limit_n;
    }

    (labels, n_features)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_interval_matches_plain_labeling() {
        let vel = Array2::from_shape_vec((2, 2), vec![1.0f32, 1.0, 5.0, 5.0]).unwrap();
        let valid = Array2::from_elem((2, 2), true);
        let (labels, n) = find_regions(&vel, &valid, &[0.0, 10.0]);
        assert_eq!(n, 1);
        assert!(labels.iter().all(|&l| l == 1));
    }

    #[test]
    fn separate_intervals_get_disjoint_running_label_ids() {
        // Two gates in [0,5) forming one region, two gates in [5,10)
        // forming a second, disconnected region -> ids 1 and 2, not two
        // independent 1s.
        let vel = Array2::from_shape_vec((1, 4), vec![1.0f32, 1.0, 7.0, 7.0]).unwrap();
        let valid = Array2::from_elem((1, 4), true);
        let (labels, n) = find_regions(&vel, &valid, &[0.0, 5.0, 10.0]);
        assert_eq!(n, 2);
        assert_eq!(
            labels,
            Array2::from_shape_vec((1, 4), vec![1, 1, 2, 2]).unwrap()
        );
    }

    #[test]
    fn invalid_gates_are_excluded_regardless_of_velocity() {
        let vel = Array2::from_shape_vec((1, 3), vec![1.0f32, 1.0, 1.0]).unwrap();
        let valid = Array2::from_shape_vec((1, 3), vec![true, false, true]).unwrap();
        let (labels, n) = find_regions(&vel, &valid, &[0.0, 10.0]);
        // The masked middle gate breaks 4-connectivity in a 1-row array.
        assert_eq!(n, 2);
        assert_eq!(labels[[0, 0]], 1);
        assert_eq!(labels[[0, 1]], 0);
        assert_eq!(labels[[0, 2]], 2);
    }

    #[test]
    fn upper_bound_is_exclusive_lower_bound_inclusive() {
        let vel = Array2::from_shape_vec((1, 2), vec![5.0f32, 10.0]).unwrap();
        let valid = Array2::from_elem((1, 2), true);
        // [0,5) then [5,10): gate 0 (v=5.0) belongs to the SECOND
        // interval (lower-inclusive), gate 1 (v=10.0) belongs to NEITHER
        // (upper-exclusive on the last interval too) -> stays 0/background.
        let (labels, n) = find_regions(&vel, &valid, &[0.0, 5.0, 10.0]);
        assert_eq!(n, 1);
        assert_eq!(labels[[0, 0]], 1);
        assert_eq!(labels[[0, 1]], 0);
    }
}
