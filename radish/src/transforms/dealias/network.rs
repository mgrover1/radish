//! Region/edge network reduction: ports Py-ART's `_RegionTracker`,
//! `_EdgeTracker`, and `_combine_regions`
//! (`pyart/correct/region_dealias.py`) — replication points 5, 6 and 7 of
//! the 8 listed in `super`'s (`dealias/mod.rs`) module doc.
//!
//! Node ids are region labels directly (`1..=n_features`), plus an unused
//! index `0` — mirroring the source's `nnodes = nfeatures + 1` /
//! `nregions = len(region_sizes) + 1` sizing exactly, so a region's label
//! (from [`super::regions::find_regions`]) is always a valid node index
//! with no translation needed.

use super::edges::AggregatedEdge;

/// Tracks which original regions are merged into each node, each node's
/// total gate count, and the accumulated unfold count for every
/// *original* region — ports `_RegionTracker`.
pub(crate) struct RegionTracker {
    node_size: Vec<i64>,
    regions_in_node: Vec<Vec<usize>>,
    /// Accumulated fold count per *original* region id (index 0 unused).
    /// Public within the crate: `sweep.rs` reads the final array and
    /// applies the Phase 5 "centered" global offset directly to it.
    pub(crate) unwrap_number: Vec<i32>,
}

impl RegionTracker {
    pub(crate) fn new(region_sizes: &[i64]) -> Self {
        let n = region_sizes.len() + 1;
        let mut node_size = vec![0i64; n];
        node_size[1..].copy_from_slice(region_sizes);
        let regions_in_node = (0..n).map(|i| vec![i]).collect();
        Self {
            node_size,
            regions_in_node,
            unwrap_number: vec![0i32; n],
        }
    }

    fn get_node_size(&self, node: usize) -> i64 {
        self.node_size[node]
    }

    /// Merge node `b` into node `a` — moves `b`'s regions and gate count
    /// onto `a`, leaving `b` empty (matching the source: it stays a
    /// zeroed, no-longer-referenced slot rather than being removed).
    fn merge_nodes(&mut self, a: usize, b: usize) {
        let regions_to_merge = std::mem::take(&mut self.regions_in_node[b]);
        self.regions_in_node[a].extend(regions_to_merge);
        self.node_size[a] += self.node_size[b];
        self.node_size[b] = 0;
    }

    /// Add `nwrap` folds to every *original* region currently merged into
    /// `node`.
    fn unwrap_node(&mut self, node: usize, nwrap: i32) {
        if nwrap == 0 {
            return;
        }
        for &region in &self.regions_in_node[node] {
            self.unwrap_number[region] += nwrap;
        }
    }
}

/// Tracks the dynamic network of edges between region-nodes as they merge
/// — ports `_EdgeTracker`. `node_alpha[e]`/`node_beta[e]` are the two
/// endpoints of edge `e`; a "dead" (already consumed/merged-away) edge is
/// marked by `weight[e] = -999` (matching the source's sentinel exactly —
/// not a magic-number smell, [`pop_edge`](Self::pop_edge) relies on this
/// specific negative value being smaller than any real `weight`, which is
/// always a non-negative gate count).
pub(crate) struct EdgeTracker {
    node_alpha: Vec<i32>,
    node_beta: Vec<i32>,
    sum_diff: Vec<f32>,
    weight: Vec<i32>,
    common_finder: Vec<bool>,
    common_index: Vec<usize>,
    last_base_node: i64,
    edges_in_node: Vec<Vec<usize>>,
}

const DEAD_WEIGHT: i32 = -999;

impl EdgeTracker {
    /// `nyquist_interval` and the per-edge `sum_diff` division both run in
    /// `f64` before narrowing to the `f32` storage the source's
    /// `self.sum_diff` array uses (`np.zeros(nedges, dtype=np.float32)`)
    /// — replication point 4's continuation from `edges.rs`.
    ///
    /// Only keeps the `index1 >= index2` direction of each unordered
    /// region pair (`if i < j: continue` in the source) — see
    /// `edges.rs`'s `edge_sum_and_count` doc for why both directions
    /// exist in `edges` and why discarding one here is not a data loss
    /// (both directions carry equivalent aggregated information).
    pub(crate) fn new(edges: &[AggregatedEdge], nyquist_interval: f64, n_nodes: usize) -> Self {
        let mut node_alpha = Vec::new();
        let mut node_beta = Vec::new();
        let mut sum_diff = Vec::new();
        let mut weight = Vec::new();
        let mut edges_in_node = vec![Vec::new(); n_nodes];

        for e in edges {
            if e.index1 < e.index2 {
                continue;
            }
            let edge_idx = node_alpha.len();
            node_alpha.push(e.index1);
            node_beta.push(e.index2);
            let diff = (e.vel1 - e.vel2) / nyquist_interval;
            sum_diff.push(diff as f32);
            weight.push(e.count);
            edges_in_node[e.index1 as usize].push(edge_idx);
            edges_in_node[e.index2 as usize].push(edge_idx);
        }

        Self {
            node_alpha,
            node_beta,
            sum_diff,
            weight,
            common_finder: vec![false; n_nodes],
            common_index: vec![0; n_nodes],
            last_base_node: -1,
            edges_in_node,
        }
    }

    fn reverse_edge_direction(&mut self, edge: usize) {
        std::mem::swap(&mut self.node_alpha[edge], &mut self.node_beta[edge]);
        self.sum_diff[edge] = -self.sum_diff[edge];
    }

    /// Add `weight[edge] * nwrap` folds to every edge touching `node`,
    /// signed by which side of the edge `node` is on — matches the
    /// source's `assert self.node_beta[edge] == node` invariant with a
    /// `debug_assert!` (the source's `assert` is unconditional; radish's
    /// own convention elsewhere in this crate uses debug-only asserts for
    /// internal invariants, checked cheaply in test/debug builds without
    /// adding release-build overhead to a per-gate-scale hot path).
    fn unwrap_node(&mut self, node: usize, nwrap: i32) {
        if nwrap == 0 {
            return;
        }
        // Disjoint-field borrow, no clone needed: this loop only ever
        // touches `self.weight`/`self.node_alpha`/`self.node_beta`/
        // `self.sum_diff`, never `self.edges_in_node` itself — matches
        // `RegionTracker::unwrap_node`'s sibling method just above,
        // which already does this without cloning.
        for &edge in &self.edges_in_node[node] {
            let weight = self.weight[edge] as i64;
            let delta = weight * nwrap as i64;
            if node as i32 == self.node_alpha[edge] {
                self.sum_diff[edge] += delta as f32;
            } else {
                debug_assert_eq!(self.node_beta[edge], node as i32);
                self.sum_diff[edge] += -(delta as f32);
            }
        }
    }

    fn combine_edges(
        &mut self,
        base_edge: usize,
        merge_edge: usize,
        merge_node: i32,
        neighbor_node: i32,
    ) {
        self.weight[base_edge] += self.weight[merge_edge];
        self.weight[merge_edge] = DEAD_WEIGHT;
        self.sum_diff[base_edge] += self.sum_diff[merge_edge];
        remove_value(&mut self.edges_in_node[merge_node as usize], merge_edge);
        remove_value(&mut self.edges_in_node[neighbor_node as usize], merge_edge);
    }

    /// Merge `merge_node` into `base_node`, having already removed the
    /// direct edge between them (`foo_edge`) — ports `_EdgeTracker.merge_nodes`.
    fn merge_nodes(&mut self, base_node: i32, merge_node: i32, foo_edge: usize) {
        self.weight[foo_edge] = DEAD_WEIGHT;
        remove_value(&mut self.edges_in_node[merge_node as usize], foo_edge);
        remove_value(&mut self.edges_in_node[base_node as usize], foo_edge);
        self.common_finder[merge_node as usize] = false;

        let edges_in_merge = self.edges_in_node[merge_node as usize].clone();

        if self.last_base_node != base_node as i64 {
            self.common_finder.iter_mut().for_each(|f| *f = false);
            // Index-based, not a `.clone()`: unlike the `edges_in_merge`
            // loop below (which mutates `self.edges_in_node[merge_node]`
            // in place via `combine_edges`'s `remove_value` calls, so a
            // live borrow would desync mid-iteration), this loop never
            // touches `self.edges_in_node[base_node]` itself — only
            // `common_finder`/`common_index`/`node_alpha`/`node_beta` —
            // so re-reading the index each iteration is sound and avoids
            // the heap allocation.
            let len = self.edges_in_node[base_node as usize].len();
            for i in 0..len {
                let edge_num = self.edges_in_node[base_node as usize][i];
                if self.node_beta[edge_num] == base_node {
                    self.reverse_edge_direction(edge_num);
                }
                debug_assert_eq!(self.node_alpha[edge_num], base_node);
                let neighbor = self.node_beta[edge_num];
                self.common_finder[neighbor as usize] = true;
                self.common_index[neighbor as usize] = edge_num;
            }
        }

        for edge_num in edges_in_merge {
            if self.node_beta[edge_num] == merge_node {
                self.reverse_edge_direction(edge_num);
            }
            debug_assert_eq!(self.node_alpha[edge_num], merge_node);
            self.node_alpha[edge_num] = base_node;
            let neighbor = self.node_beta[edge_num];
            if self.common_finder[neighbor as usize] {
                let base_edge_num = self.common_index[neighbor as usize];
                self.combine_edges(base_edge_num, edge_num, merge_node, neighbor);
            } else {
                self.common_finder[neighbor as usize] = true;
                self.common_index[neighbor as usize] = edge_num;
            }
        }

        let moved = std::mem::take(&mut self.edges_in_node[merge_node as usize]);
        self.edges_in_node[base_node as usize].extend(moved);
        self.last_base_node = base_node as i64;
    }

    /// Pop the edge with the largest weight — ties broken by **first**
    /// occurrence (matching `np.argmax`'s documented tie-break; Rust's
    /// `Iterator::max_by_key` breaks ties on the *last* occurrence, the
    /// opposite, so this is a manual scan — replication point 6).
    /// Returns `None` once every edge is dead (`weight < 0`, i.e. only
    /// [`DEAD_WEIGHT`] sentinels remain) — the algorithm's termination
    /// condition.
    fn pop_edge(&self) -> Option<PoppedEdge> {
        // Not reached by `sweep.rs`'s own pipeline (it early-returns
        // before constructing an `EdgeTracker` at all when there are no
        // edges — see `sweep::dealias_sweep`), but `pop_edge`/
        // `combine_regions` are exercised directly in this module's own
        // tests, and a genuinely empty network has no edge to pop —
        // `None` is the only sound answer, not a panic on `self.weight[0]`.
        if self.weight.is_empty() {
            return None;
        }
        let mut best = 0usize;
        for i in 1..self.weight.len() {
            if self.weight[i] > self.weight[best] {
                best = i;
            }
        }
        let weight = self.weight[best];
        if weight < 0 {
            return None;
        }
        Some(PoppedEdge {
            node1: self.node_alpha[best],
            node2: self.node_beta[best],
            weight,
            diff: self.sum_diff[best] as f64 / weight as f64,
            edge_number: best,
        })
    }
}

/// The edge [`EdgeTracker::pop_edge`] selected — its two endpoints, its
/// weight (unused by [`combine_regions`] itself but part of the source's
/// own return tuple, and useful for tests), the normalized velocity
/// difference driving the fold decision, and its own index (needed by
/// [`EdgeTracker::merge_nodes`] to retire it).
struct PoppedEdge {
    node1: i32,
    node2: i32,
    // Only read from `#[cfg(test)]` code (`combine_regions` discards it,
    // matching the source's own `_weight` local) — kept on the struct for
    // fidelity with `_combine_regions`'s full unpacked tuple and so tests
    // can assert on it directly.
    #[allow(dead_code)]
    weight: i32,
    diff: f64,
    edge_number: usize,
}

/// Remove the first occurrence of `value` from `v`, preserving the
/// relative order of the rest — matches Python `list.remove`.
fn remove_value(v: &mut Vec<usize>, value: usize) {
    if let Some(pos) = v.iter().position(|&x| x == value) {
        v.remove(pos);
    }
}

/// One merge step: pop the network's largest-weight edge and fold its
/// smaller region into its larger one. Returns `true` when done (no real
/// edges remain) — ports `_combine_regions`.
pub(crate) fn combine_regions(regions: &mut RegionTracker, edges: &mut EdgeTracker) -> bool {
    let Some(PoppedEdge {
        node1,
        node2,
        weight: _,
        diff,
        edge_number,
    }) = edges.pop_edge()
    else {
        return true;
    };

    // Banker's rounding (round-half-to-even), matching `np.round` exactly
    // — replication point 5. `f64::round_ties_even` is stable since Rust
    // 1.77; this crate's pinned toolchain is newer (verified this
    // session).
    let rdiff = diff.round_ties_even() as i64;

    let (node1_u, node2_u) = (node1 as usize, node2 as usize);
    let node1_size = regions.get_node_size(node1_u);
    let node2_size = regions.get_node_size(node2_u);

    let (base_node, merge_node, rdiff) = if node1_size > node2_size {
        (node1, node2, rdiff)
    } else {
        (node2, node1, -rdiff)
    };

    if rdiff != 0 {
        let rdiff32 = rdiff as i32;
        regions.unwrap_node(merge_node as usize, rdiff32);
        edges.unwrap_node(merge_node as usize, rdiff32);
    }

    regions.merge_nodes(base_node as usize, merge_node as usize);
    edges.merge_nodes(base_node, merge_node, edge_number);

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(index1: i32, index2: i32, count: i32, vel1: f64, vel2: f64) -> AggregatedEdge {
        AggregatedEdge {
            index1,
            index2,
            count,
            vel1,
            vel2,
        }
    }

    #[test]
    fn edge_tracker_keeps_only_the_index1_ge_index2_direction() {
        let edges = vec![edge(1, 2, 3, 10.0, 20.0), edge(2, 1, 3, 20.0, 10.0)];
        let tracker = EdgeTracker::new(&edges, 2.0, 3);
        assert_eq!(tracker.node_alpha, vec![2]);
        assert_eq!(tracker.node_beta, vec![1]);
        // sum_diff = (20 - 10) / 2.0 = 5.0
        assert_eq!(tracker.sum_diff, vec![5.0]);
        assert_eq!(tracker.weight, vec![3]);
    }

    #[test]
    fn pop_edge_breaks_ties_on_first_occurrence() {
        let edges = vec![edge(1, 0, 5, 1.0, 0.0), edge(2, 0, 5, 3.0, 0.0)];
        let tracker = EdgeTracker::new(&edges, 1.0, 3);
        let popped = tracker.pop_edge().unwrap();
        // Both edges have weight 5 -> first (index 0, node1=1) must win.
        assert_eq!(
            (
                popped.node1,
                popped.node2,
                popped.weight,
                popped.edge_number
            ),
            (1, 0, 5, 0)
        );
    }

    #[test]
    fn pop_edge_breaks_a_three_way_tie_on_first_occurrence() {
        // A 2-way tie can't distinguish "first-wins" logic from a
        // coincidentally-correct 2-element linear scan (e.g. a buggy
        // `iter().max_by_key()`, which breaks ties on the LAST element,
        // would still happen to return index 0 given only 2 equal
        // candidates in a particular order). 3 equal-weight edges makes
        // "first" and "last" observably different regardless of
        // implementation strategy.
        let edges = vec![
            edge(1, 0, 7, 1.0, 0.0),
            edge(2, 0, 7, 2.0, 0.0),
            edge(3, 0, 7, 3.0, 0.0),
        ];
        let tracker = EdgeTracker::new(&edges, 1.0, 4);
        let popped = tracker.pop_edge().unwrap();
        assert_eq!(
            (
                popped.node1,
                popped.node2,
                popped.weight,
                popped.edge_number
            ),
            (1, 0, 7, 0),
            "three equal-weight edges must still pick the FIRST (index 0), not the last"
        );
    }

    #[test]
    fn pop_edge_reports_none_once_every_edge_is_dead() {
        let edges = vec![edge(1, 0, 5, 1.0, 0.0)];
        let mut tracker = EdgeTracker::new(&edges, 1.0, 2);
        tracker.weight[0] = DEAD_WEIGHT;
        assert!(tracker.pop_edge().is_none());
    }

    #[test]
    fn combine_regions_merges_smaller_into_larger_and_records_unfold() {
        // Two regions: region1 has 10 gates, region2 has 2. An edge with
        // a diff of 1.4 rounds to 1 fold applied to the SMALLER region.
        let mut regions = RegionTracker::new(&[10, 2]);
        let edges = vec![edge(2, 1, 4, 1.4, 0.0)]; // sum_diff/weight = 0.35 * 4? use direct diff
        let mut tracker = EdgeTracker::new(&edges, 1.0, 3);
        // Force a known diff directly for a deterministic assertion,
        // bypassing the vel1/vel2 arithmetic: sum_diff[0]/weight[0]
        // must equal 1.4 -> sum_diff = 1.4 * 4 = 5.6.
        tracker.sum_diff[0] = 5.6;
        let done = combine_regions(&mut regions, &mut tracker);
        assert!(!done);
        // region2 (smaller, size 2) merges into region1 (size 10); its
        // rdiff sign flips because node2 (region1, size 10) > node1
        // (region2, size 2) triggered the "else" branch. round(1.4) = 1.
        assert_eq!(regions.unwrap_number[2], -1);
        assert_eq!(regions.unwrap_number[1], 0);
        assert_eq!(regions.get_node_size(1), 12);
        assert_eq!(regions.get_node_size(2), 0);
    }

    #[test]
    fn combine_regions_terminates_when_no_real_edges_remain() {
        let mut regions = RegionTracker::new(&[5]);
        let mut tracker = EdgeTracker::new(&[], 1.0, 2);
        assert!(combine_regions(&mut regions, &mut tracker));
    }

    #[test]
    fn banker_s_rounding_matches_round_half_to_even() {
        // 0.5 rounds to 0 (even), 1.5 rounds to 2 (even), 2.5 rounds to 2.
        assert_eq!(0.5f64.round_ties_even(), 0.0);
        assert_eq!(1.5f64.round_ties_even(), 2.0);
        assert_eq!(2.5f64.round_ties_even(), 2.0);
        assert_eq!((-0.5f64).round_ties_even(), 0.0);
    }
}
