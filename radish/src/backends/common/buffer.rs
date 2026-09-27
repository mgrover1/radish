//! Generic `(nrays × max_gates)` buffer scaffold used by every adapter that
//! lays down a per-moment `Array2<f32>`.
//!
//! The signature is generic on the per-ray item type (`R`) so it works for
//! NEXRAD's `Radial`, IRIS's `RawRay`, ODIM's `RayDataset`, etc. The closure
//! controls the per-ray fill; everything around it (NaN pre-fill, row
//! offsetting, final `Array2::from_shape_vec`) is shared.

use ndarray::Array2;

use crate::{RadishError, Result};

/// Lay down a `(nrays × max_gates)` `Array2<f32>` from a sequence of rays.
///
/// * `items` — the per-ray records the caller already has in hand.
/// * `order` — a permutation of `0..items.len()` deciding the row order
///   (e.g. azimuth-sorted for PPI, elevation-sorted for RHI).
/// * `nrays` — must equal `order.len()`; supplied separately so we can size
///   the buffer once without iterating the order slice.
/// * `ngates` — number of gates this moment actually has. The closure is
///   handed a `&mut [f32]` of exactly this length per row, even when the
///   sweep's canonical axis is larger.
/// * `max_gates` — width of the resulting array. When `ngates < max_gates`
///   the trailing cells of each row stay NaN (the per-moment gate-count is
///   shorter than the sweep's canonical range axis).
/// * `fill_row` — runs once per ray. Cells the closure doesn't overwrite
///   keep their NaN initial value, so a missing-product row or a moment
///   that yields fewer items than `ngates` produces NaN tails automatically.
///
/// Returns `RadishError::Conversion` if the buffer's length does not equal
/// `nrays * max_gates` (which `from_shape_vec` would reject).
pub(crate) fn decode_into_array<R, F>(
    items: &[R],
    order: &[usize],
    nrays: usize,
    ngates: usize,
    max_gates: usize,
    fill_row: F,
) -> Result<Array2<f32>>
where
    F: Fn(&R, &mut [f32]),
{
    debug_assert_eq!(order.len(), nrays, "order length must match nrays");
    debug_assert!(ngates <= max_gates, "ngates must not exceed max_gates");

    let mut buf: Vec<f32> = vec![f32::NAN; nrays * max_gates];
    for (row, &item_idx) in order.iter().enumerate() {
        let dst_off = row * max_gates;
        let dst = &mut buf[dst_off..dst_off + ngates];
        fill_row(&items[item_idx], dst);
    }
    Array2::from_shape_vec((nrays, max_gates), buf)
        .map_err(|e| RadishError::Conversion(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinning the contract: closure-untouched cells stay NaN, even when
    /// the closure intentionally writes to a partial row.
    #[test]
    fn untouched_cells_stay_nan() {
        // 3 rays, 5 gates each, but our closure only writes the first 3.
        let items = vec![1u8, 2, 3];
        let order = vec![2, 0, 1]; // reverse-ish permutation
        let arr = decode_into_array(&items, &order, 3, 3, 5, |&v, dst| {
            for slot in dst.iter_mut() {
                *slot = v as f32;
            }
        })
        .unwrap();
        assert_eq!(arr.shape(), &[3, 5]);
        // Row 0 was item 2 (val 3), gates 0..2 = 3.0, gates 3..4 = NaN
        assert_eq!(arr[(0, 0)], 3.0);
        assert_eq!(arr[(0, 1)], 3.0);
        assert_eq!(arr[(0, 2)], 3.0);
        assert!(arr[(0, 3)].is_nan());
        assert!(arr[(0, 4)].is_nan());
        // Row 1 was item 0 (val 1)
        assert_eq!(arr[(1, 0)], 1.0);
        // Row 2 was item 1 (val 2)
        assert_eq!(arr[(2, 0)], 2.0);
    }

    #[test]
    fn empty_order_produces_zero_row_array() {
        let items: Vec<u8> = vec![];
        let order: Vec<usize> = vec![];
        let arr = decode_into_array(&items, &order, 0, 0, 5, |_, _| ()).unwrap();
        assert_eq!(arr.shape(), &[0, 5]);
    }

    #[test]
    fn closure_decides_how_to_fill_each_cell() {
        // Closure that writes the index of the cell, ignoring item value.
        let items = vec!["a", "b"];
        let order = vec![0, 1];
        let arr = decode_into_array(&items, &order, 2, 4, 4, |_, dst| {
            for (i, slot) in dst.iter_mut().enumerate() {
                *slot = i as f32;
            }
        })
        .unwrap();
        assert_eq!(arr[(0, 0)], 0.0);
        assert_eq!(arr[(0, 3)], 3.0);
        assert_eq!(arr[(1, 0)], 0.0);
        assert_eq!(arr[(1, 3)], 3.0);
    }

    /// Property-based tests covering the contract surface beyond the
    /// hand-picked unit tests. Adapter ports (sigmet, etc.) lean on these
    /// invariants when they wire `decode_into_array` into a new format —
    /// random shapes catch off-by-ones and panic-on-edge bugs the explicit
    /// tests above don't.
    mod properties {
        use super::super::*;
        use proptest::prelude::*;

        proptest! {
            /// For any well-formed `(nrays, ngates, max_gates)` triple
            /// (with ngates ≤ max_gates) and any closure that doesn't
            /// touch the buffer, every cell stays NaN and the resulting
            /// shape is exactly `(nrays × max_gates)`.
            #[test]
            fn untouched_buffer_is_all_nan_with_correct_shape(
                nrays in 0usize..16,
                max_gates in 1usize..32,
                ngates_extra in 0usize..32,
            ) {
                let ngates = (ngates_extra % max_gates).min(max_gates);
                let items: Vec<u8> = (0..nrays).map(|i| i as u8).collect();
                let order: Vec<usize> = (0..nrays).collect();
                let arr = decode_into_array::<u8, _>(
                    &items, &order, nrays, ngates, max_gates,
                    |_, _dst| { /* deliberately untouched */ },
                ).unwrap();
                prop_assert_eq!(arr.shape(), &[nrays, max_gates]);
                prop_assert!(arr.iter().all(|v| v.is_nan()));
            }

            /// A closure that fills exactly the first `ngates` cells with
            /// a sentinel leaves the trailing `max_gates - ngates` cells
            /// NaN on every row, regardless of the permutation order.
            #[test]
            fn trailing_gates_stay_nan_when_closure_fills_only_ngates(
                nrays in 1usize..8,
                max_gates in 2usize..16,
                ngates_extra in 0usize..16,
            ) {
                let ngates = ((ngates_extra % max_gates).max(1)).min(max_gates);
                let items: Vec<f32> = (0..nrays).map(|i| (i + 1) as f32 * 10.0).collect();
                // Reverse order so the test isn't trivially the identity.
                let order: Vec<usize> = (0..nrays).rev().collect();
                let arr = decode_into_array(
                    &items, &order, nrays, ngates, max_gates,
                    |&v, dst| {
                        for slot in dst.iter_mut() { *slot = v; }
                    },
                ).unwrap();
                for row in 0..nrays {
                    // Filled prefix matches the item the closure saw.
                    let expected = items[order[row]];
                    for col in 0..ngates {
                        prop_assert_eq!(arr[(row, col)], expected);
                    }
                    // Trailing tail is NaN.
                    for col in ngates..max_gates {
                        prop_assert!(arr[(row, col)].is_nan());
                    }
                }
            }
        }
    }
}
