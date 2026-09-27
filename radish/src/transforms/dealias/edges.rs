//! Edge finding between adjacent regions: ports Py-ART's Cython
//! `_fast_edge_finder` (`pyart/correct/_fast_edge_finder.pyx`) and the
//! lexsort/dedup half of `_edge_sum_and_count`
//! (`pyart/correct/region_dealias.py`) — replication points 2, 3 and 4
//! of the 8 listed in `super`'s (`dealias/mod.rs`) module doc.

use ndarray::Array2;

/// One raw (gate, neighbor) adjacency, exactly as `_EdgeCollector.add_edge`
/// records it — `vel1`/`vel2` are individually `f32` gate values widened
/// to `f64` at the point of storage, matching the Cython collector's
/// `float32 -> float64` implicit upcast into its `np.float64_t` buffers
/// (replication point 4's first hop in the dtype chain).
#[derive(Debug, Clone, Copy)]
struct RawEdge {
    index1: i32,
    index2: i32,
    vel1: f64,
    vel2: f64,
}

/// Record an edge unless it's circular (`neighbor == label`) or points at
/// a masked gate (`neighbor == 0`) — matches `_EdgeCollector.add_edge`'s
/// only guard exactly.
fn maybe_add_edge(edges: &mut Vec<RawEdge>, label: i32, neighbor: i32, vel: f32, nvel: f32) {
    if neighbor == label || neighbor == 0 {
        return;
    }
    edges.push(RawEdge {
        index1: label,
        index2: neighbor,
        vel1: vel as f64,
        vel2: nvel as f64,
    });
}

/// Port of `_fast_edge_finder`: for every foreground gate, record an edge
/// to each of its 4 neighbors, resolving through masked gaps up to
/// `max_gap_x` (ray axis, index 0) / `max_gap_y` (gate axis, index 1)
/// steps. Azimuth wrap-around (`rays_wrap_around`) applies ONLY to the
/// ray axis (left/right) — the gate/range axis never wraps, matching the
/// source's `top`/`bottom` blocks having no wrap logic at all.
///
/// Iteration is raster-scan (`x` outer, `y` inner — i.e. ray-major, the
/// same order `Array2`'s row-major storage already iterates in), and
/// **must** stay in this exact order: it's the tiebreak `Vec` order feeds
/// into the stable sort in [`edge_sum_and_count`], which in turn is the
/// tiebreak numpy's `lexsort` (also stable) uses — see that function's
/// doc for why this determinism actually matters for bit-exactness, not
/// just aesthetics.
fn fast_edge_finder(
    labels: &Array2<i32>,
    data: &Array2<f32>,
    rays_wrap_around: bool,
    max_gap_x: i32,
    max_gap_y: i32,
) -> Vec<RawEdge> {
    let (n_rays, n_bins) = labels.dim();
    if n_rays == 0 || n_bins == 0 {
        return Vec::new();
    }
    let right = n_rays as i64 - 1;
    let bottom = n_bins as i64 - 1;
    let mut edges = Vec::new();

    for x in 0..n_rays as i64 {
        for y in 0..n_bins as i64 {
            let (xu, yu) = (x as usize, y as usize);
            let label = labels[[xu, yu]];
            if label == 0 {
                continue;
            }
            let vel = data[[xu, yu]];

            // left
            let mut x_check = x - 1;
            if x_check == -1 && rays_wrap_around {
                x_check = right;
            }
            if x_check != -1 {
                let mut neighbor = labels[[x_check as usize, yu]];
                let mut nvel = data[[x_check as usize, yu]];
                if neighbor == 0 {
                    for _ in 0..max_gap_x {
                        x_check -= 1;
                        if x_check == -1 {
                            if rays_wrap_around {
                                x_check = right;
                            } else {
                                break;
                            }
                        }
                        neighbor = labels[[x_check as usize, yu]];
                        nvel = data[[x_check as usize, yu]];
                        if neighbor != 0 {
                            break;
                        }
                    }
                }
                maybe_add_edge(&mut edges, label, neighbor, vel, nvel);
            }

            // right
            let mut x_check = x + 1;
            if x_check == right + 1 && rays_wrap_around {
                x_check = 0;
            }
            if x_check != right + 1 {
                let mut neighbor = labels[[x_check as usize, yu]];
                let mut nvel = data[[x_check as usize, yu]];
                if neighbor == 0 {
                    for _ in 0..max_gap_x {
                        x_check += 1;
                        if x_check == right + 1 {
                            if rays_wrap_around {
                                x_check = 0;
                            } else {
                                break;
                            }
                        }
                        neighbor = labels[[x_check as usize, yu]];
                        nvel = data[[x_check as usize, yu]];
                        if neighbor != 0 {
                            break;
                        }
                    }
                }
                maybe_add_edge(&mut edges, label, neighbor, vel, nvel);
            }

            // top (gate axis: never wraps)
            let mut y_check = y - 1;
            if y_check != -1 {
                let mut neighbor = labels[[xu, y_check as usize]];
                let mut nvel = data[[xu, y_check as usize]];
                if neighbor == 0 {
                    for _ in 0..max_gap_y {
                        y_check -= 1;
                        if y_check == -1 {
                            break;
                        }
                        neighbor = labels[[xu, y_check as usize]];
                        nvel = data[[xu, y_check as usize]];
                        if neighbor != 0 {
                            break;
                        }
                    }
                }
                maybe_add_edge(&mut edges, label, neighbor, vel, nvel);
            }

            // bottom
            let mut y_check = y + 1;
            if y_check != bottom + 1 {
                let mut neighbor = labels[[xu, y_check as usize]];
                let mut nvel = data[[xu, y_check as usize]];
                if neighbor == 0 {
                    for _ in 0..max_gap_y {
                        y_check += 1;
                        if y_check == bottom + 1 {
                            break;
                        }
                        neighbor = labels[[xu, y_check as usize]];
                        nvel = data[[xu, y_check as usize]];
                        if neighbor != 0 {
                            break;
                        }
                    }
                }
                maybe_add_edge(&mut edges, label, neighbor, vel, nvel);
            }
        }
    }

    edges
}

/// One deduplicated, aggregated edge — every raw adjacency sharing the
/// same `(index1, index2)` ordered pair summed together. Still
/// **undirected-pair-duplicated**: both `(a, b)` and `(b, a)` can be
/// present, exactly matching `_edge_sum_and_count`'s return value before
/// `_EdgeTracker.__init__`'s `if i < j: continue` picks one direction
/// per unordered pair (that filter lives in `network.rs`, matching the
/// source's own module boundary between the function and the class).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AggregatedEdge {
    pub index1: i32,
    pub index2: i32,
    pub count: i32,
    pub vel1: f64,
    pub vel2: f64,
}

/// Port of `_edge_sum_and_count`'s edge-finding + dedup: find every raw
/// adjacency via [`fast_edge_finder`], then collapse duplicates of the
/// same ordered `(index1, index2)` pair (multiple physical gate
/// boundaries between the same two regions) by summing their `vel1`/
/// `vel2` in encounter order — matching `np.lexsort((index1, index2))`
/// (primary key `index2`, secondary `index1` — NumPy's `lexsort` treats
/// the LAST key as primary) followed by `np.add.reduceat`, whose
/// left-to-right sequential summation this replicates exactly (floating
/// point addition is not associative, so this is bit-exactness-relevant,
/// not just a style choice — replication point 3).
///
/// Rust's `[T]::sort_by` is a stable sort (like NumPy's `lexsort`), so
/// sorting by `(index2, index1)` alone reproduces the same relative
/// ordering among exact ties that `lexsort` gives — no explicit tertiary
/// "original index" key is needed.
pub(crate) fn edge_sum_and_count(
    labels: &Array2<i32>,
    data: &Array2<f32>,
    rays_wrap_around: bool,
    max_gap_x: i32,
    max_gap_y: i32,
) -> Vec<AggregatedEdge> {
    let mut raw = fast_edge_finder(labels, data, rays_wrap_around, max_gap_x, max_gap_y);
    if raw.is_empty() {
        return Vec::new();
    }

    raw.sort_by(|a, b| a.index2.cmp(&b.index2).then(a.index1.cmp(&b.index1)));

    let mut out: Vec<AggregatedEdge> = Vec::new();
    for e in raw {
        match out.last_mut() {
            Some(last) if last.index1 == e.index1 && last.index2 == e.index2 => {
                last.vel1 += e.vel1;
                last.vel2 += e.vel2;
                last.count += 1;
            }
            _ => out.push(AggregatedEdge {
                index1: e.index1,
                index2: e.index2,
                count: 1,
                vel1: e.vel1,
                vel2: e.vel2,
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(rows: &[&[i32]]) -> Array2<i32> {
        let n_rows = rows.len();
        let n_cols = rows[0].len();
        Array2::from_shape_fn((n_rows, n_cols), |(r, c)| rows[r][c])
    }

    fn velgrid(rows: &[&[f32]]) -> Array2<f32> {
        let n_rows = rows.len();
        let n_cols = rows[0].len();
        Array2::from_shape_fn((n_rows, n_cols), |(r, c)| rows[r][c])
    }

    #[test]
    fn two_adjacent_regions_produce_both_directed_edges() {
        // ray 0: region 1 (vel 5), ray 1: region 2 (vel 7), same gate.
        let labels = grid(&[&[1], &[2]]);
        let data = velgrid(&[&[5.0], &[7.0]]);
        let edges = edge_sum_and_count(&labels, &data, false, 0, 0);
        // Both (1,2) [from region 1's gate scanning right] and (2,1)
        // [from region 2's gate scanning left] must be present.
        assert!(edges
            .iter()
            .any(|e| e.index1 == 1 && e.index2 == 2 && e.vel1 == 5.0 && e.vel2 == 7.0));
        assert!(edges
            .iter()
            .any(|e| e.index1 == 2 && e.index2 == 1 && e.vel1 == 7.0 && e.vel2 == 5.0));
    }

    #[test]
    fn no_edge_between_masked_gates_or_same_region() {
        let labels = grid(&[&[1, 1, 0]]);
        let data = velgrid(&[&[5.0, 5.0, 0.0]]);
        let edges = edge_sum_and_count(&labels, &data, false, 0, 0);
        // Both gates are region 1 -> circular edges suppressed; the
        // masked gate contributes no edge either.
        assert!(edges.is_empty());
    }

    #[test]
    fn duplicate_ordered_pairs_are_summed_not_overwritten() {
        // Two separate ray-adjacent boundaries between region 1 and
        // region 2 (a 2-row-tall boundary) must sum both contributions.
        let labels = grid(&[&[1, 2], &[1, 2]]);
        let data = velgrid(&[&[3.0, 4.0], &[5.0, 6.0]]);
        let edges = edge_sum_and_count(&labels, &data, false, 0, 0);
        let e12 = edges
            .iter()
            .find(|e| e.index1 == 1 && e.index2 == 2)
            .unwrap();
        // region 1's two gates (rays 0 and 1, bin 0) each border region
        // 2's gate in the same ray (bin 1): count = 2, vel1 = 3+5 = 8,
        // vel2 = 4+6 = 10.
        assert_eq!(e12.count, 2);
        assert_eq!(e12.vel1, 8.0);
        assert_eq!(e12.vel2, 10.0);
    }

    #[test]
    fn ray_wrap_around_connects_first_and_last_ray() {
        let labels = grid(&[&[1], &[0], &[2]]); // 3 rays, 1 bin
        let data = velgrid(&[&[1.0], &[0.0], &[2.0]]);
        let no_wrap = edge_sum_and_count(&labels, &data, false, 0, 0);
        assert!(
            no_wrap.is_empty(),
            "ray 0 and ray 2 aren't adjacent without wrap"
        );

        let wrapped = edge_sum_and_count(&labels, &data, true, 0, 0);
        assert!(wrapped
            .iter()
            .any(|e| e.index1 == 1 && e.index2 == 2 || e.index1 == 2 && e.index2 == 1));
    }

    #[test]
    fn single_ray_with_wrap_around_wraps_to_itself_but_produces_no_circular_edge() {
        // With n_rays == 1 and rays_wrap_around == true, the left-neighbor
        // check computes x_check = -1, wraps to `right` (== 0, i.e. the
        // SAME ray), the exact self-wrap path
        // `ray_wrap_around_connects_first_and_last_ray` (3 rays) never
        // exercises. Safe only because `maybe_add_edge` discards
        // `neighbor == label`; this pins that specifically for the n=1
        // case rather than relying on it holding "by construction".
        let labels = grid(&[&[1]]);
        let data = velgrid(&[&[5.0]]);
        let edges = edge_sum_and_count(&labels, &data, true, 0, 0);
        assert!(
            edges.is_empty(),
            "a single ray must never produce an edge to itself"
        );
    }

    #[test]
    fn gate_axis_never_wraps_even_when_rays_wrap_around_is_true() {
        let labels = grid(&[&[1], &[0], &[2]]).reversed_axes(); // 1 ray, 3 bins
        let data = velgrid(&[&[1.0], &[0.0], &[2.0]]).reversed_axes();
        let edges = edge_sum_and_count(&labels, &data, true, 0, 0);
        assert!(edges.is_empty(), "the gate/range axis must never wrap");
    }

    #[test]
    fn gap_skipping_bridges_masked_gates_up_to_the_limit_on_the_ray_axis() {
        // 4 rays, 1 bin: region1, masked, masked, region2 — a gap of 2
        // along the RAY axis, so this exercises max_gap_x specifically
        // (max_gap_y is irrelevant with only 1 bin).
        let labels = grid(&[&[1], &[0], &[0], &[2]]);
        let data = velgrid(&[&[1.0], &[0.0], &[0.0], &[2.0]]);
        let too_small = edge_sum_and_count(&labels, &data, false, 1, 0);
        assert!(too_small.is_empty(), "gap of 2 exceeds max_gap_x=1");

        let enough = edge_sum_and_count(&labels, &data, false, 2, 0);
        assert!(enough
            .iter()
            .any(|e| e.index1 == 1 && e.index2 == 2 && e.vel1 == 1.0 && e.vel2 == 2.0));
    }

    #[test]
    fn gap_skipping_bridges_masked_gates_up_to_the_limit_on_the_gate_axis() {
        // 1 ray, 4 bins: region1, masked, masked, region2 — a gap of 2
        // along the GATE axis, exercising max_gap_y specifically.
        let labels = grid(&[&[1, 0, 0, 2]]);
        let data = velgrid(&[&[1.0, 0.0, 0.0, 2.0]]);
        let too_small = edge_sum_and_count(&labels, &data, false, 0, 1);
        assert!(too_small.is_empty(), "gap of 2 exceeds max_gap_y=1");

        let enough = edge_sum_and_count(&labels, &data, false, 0, 2);
        assert!(enough
            .iter()
            .any(|e| e.index1 == 1 && e.index2 == 2 && e.vel1 == 1.0 && e.vel2 == 2.0));
    }

    #[test]
    fn empty_grid_produces_no_edges_without_panicking() {
        let labels = Array2::<i32>::zeros((0, 0));
        let data = Array2::<f32>::zeros((0, 0));
        assert!(edge_sum_and_count(&labels, &data, true, 5, 5).is_empty());
    }
}
