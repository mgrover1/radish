//! Per-sweep coordinate assembly given a sorted ray permutation and per-ray
//! getters.
//!
//! Every adapter that produces a [`crate::Coordinates`] from a list of rays
//! does the same dance:
//!
//! 1. Compute a sort order (azimuth-ascending for PPI, elevation-ascending
//!    for RHI).
//! 2. Walk the order and project each ray to azimuth, elevation, and a
//!    Unix-seconds-with-fractional time stamp.
//! 3. Combine those three vectors with the canonical range axis (already
//!    derived from [`super::geometry::MomentGeometry`]) into a
//!    [`crate::Coordinates`].
//!
//! This module owns the coordinate-vector projection step. The sort order
//! is supplied by the caller (typically via [`super::sort::sort_indices_by_key`]),
//! and the per-ray accessors are closures so it works for any decoded
//! ray type.

use crate::Coordinates;

/// Build a [`crate::Coordinates`] for a PPI sweep from a sorted ray
/// permutation and per-ray accessors.
///
/// * `items` — every ray in the sweep, in original (pre-sort) order.
/// * `order` — permutation of `0..items.len()` (azimuth-ascending for PPI).
/// * `range_axis` — pre-built per-gate range axis in metres.
/// * `azimuth_deg` / `elevation_deg` — per-ray angle accessors in degrees.
/// * `time_unix_secs` — per-ray timestamp as Unix seconds (fractional part
///   carrying microseconds is preferred). Use `f64::NAN` for rays without a
///   timestamp; downstream serialisers preserve the NaN.
///
/// The output's `time` / `azimuth` / `elevation` vectors are all in `order`
/// (post-sort) so they line up with whatever moment buffers the caller has
/// also assembled in the same permutation.
pub(crate) fn assemble_ppi_coordinates<T, FAz, FEl, FTime>(
    items: &[T],
    order: &[usize],
    range_axis: Vec<f32>,
    azimuth_deg: FAz,
    elevation_deg: FEl,
    time_unix_secs: FTime,
) -> Coordinates
where
    FAz: Fn(&T) -> f32,
    FEl: Fn(&T) -> f32,
    FTime: Fn(&T) -> f64,
{
    let azimuth: Vec<f32> = order.iter().map(|&i| azimuth_deg(&items[i])).collect();
    let elevation: Vec<f32> = order.iter().map(|&i| elevation_deg(&items[i])).collect();
    let time: Vec<f64> = order.iter().map(|&i| time_unix_secs(&items[i])).collect();
    Coordinates::new(time, range_axis, azimuth, elevation)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic ray for testing accessor wiring.
    struct Ray {
        az: f32,
        el: f32,
        t: f64,
    }

    #[test]
    fn assemble_uses_sort_order_for_each_axis() {
        let rays = vec![
            Ray {
                az: 90.0,
                el: 1.0,
                t: 100.0,
            },
            Ray {
                az: 10.0,
                el: 1.5,
                t: 110.0,
            },
            Ray {
                az: 50.0,
                el: 2.0,
                t: 120.0,
            },
        ];
        let order = vec![1, 2, 0]; // azimuth-sorted: 10 → 50 → 90
        let range_axis = vec![1000.0_f32, 1100.0, 1200.0];

        let coords = assemble_ppi_coordinates(
            &rays,
            &order,
            range_axis.clone(),
            |r| r.az,
            |r| r.el,
            |r| r.t,
        );
        assert_eq!(coords.azimuth, vec![10.0, 50.0, 90.0]);
        assert_eq!(coords.elevation, vec![1.5, 2.0, 1.0]);
        assert_eq!(coords.time, vec![110.0, 120.0, 100.0]);
        assert_eq!(coords.range, range_axis);
    }
}
