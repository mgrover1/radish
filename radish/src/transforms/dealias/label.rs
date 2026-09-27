//! 4-neighbor connected-component labeling, numbered to match
//! `scipy.ndimage.label`'s exact convention bit-for-bit — replication
//! point 1 of the 8 listed in `super`'s (`dealias/mod.rs`) module doc.
//!
//! scipy's default structuring element for a 2D array is the 4-connected
//! cross (no diagonals) — confirmed empirically this session, not assumed:
//! a checkerboard-diagonal 2x2 test array produces 2 separate labels under
//! `ndimage.label`'s default, not 1. Final label ids are assigned `1..=n`
//! in the order each component's **topologically-first pixel** (minimal
//! row, then minimal column within that row — i.e. raster-scan discovery
//! order) is encountered — verified empirically with a synthetic
//! two-pronged shape that only becomes one component after its third row.

use ndarray::Array2;
use std::collections::HashMap;

/// Union-find over provisional labels `1..=n_provisional` (index 0 is an
/// unused sentinel so provisional labels can double as their own 1-based
/// indices — background stays `0` throughout and is never unioned).
struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new() -> Self {
        // parent[0] is a sentinel; real ids start at 1.
        Self { parent: vec![0] }
    }

    /// Allocate a new singleton set, returning its id.
    fn make_set(&mut self) -> usize {
        let id = self.parent.len();
        self.parent.push(id);
        id
    }

    fn find(&mut self, x: usize) -> usize {
        let mut root = x;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        // Path compression.
        let mut cur = x;
        while self.parent[cur] != root {
            let next = self.parent[cur];
            self.parent[cur] = root;
            cur = next;
        }
        root
    }

    /// Union the sets containing `a` and `b`. Which side becomes root is
    /// arbitrary — final numbering is re-derived from raster-scan
    /// discovery order in a second pass, independent of the underlying
    /// union-find tree shape (see the module doc).
    fn union(&mut self, a: usize, b: usize) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

/// 4-connected component labels of `mask` (`true` = foreground), numbered
/// `1..=n_features` in raster-scan discovery order; `0` marks background.
/// Matches `scipy.ndimage.label(mask)` (default structure) exactly,
/// including for `mask` of shape `(0, _)` / `(_, 0)` (zero features, an
/// all-zeros array of the input shape — scipy's behavior on an empty
/// array, not a panic).
pub(crate) fn label_4connected(mask: &Array2<bool>) -> (Array2<i32>, usize) {
    let (n_rows, n_cols) = mask.dim();
    let mut labels = Array2::<i32>::zeros((n_rows, n_cols));
    if n_rows == 0 || n_cols == 0 {
        return (labels, 0);
    }

    // Pass 1: provisional labels via up/left neighbors, unioning on
    // conflict (both up and left are foreground but currently carry
    // different provisional ids).
    let mut uf = UnionFind::new();
    let mut provisional = Array2::<usize>::zeros((n_rows, n_cols));
    for r in 0..n_rows {
        for c in 0..n_cols {
            if !mask[[r, c]] {
                continue;
            }
            let up = if r > 0 { provisional[[r - 1, c]] } else { 0 };
            let left = if c > 0 { provisional[[r, c - 1]] } else { 0 };
            provisional[[r, c]] = match (up, left) {
                (0, 0) => uf.make_set(),
                (u, 0) => u,
                (0, l) => l,
                (u, l) => {
                    if u != l {
                        uf.union(u, l);
                    }
                    u
                }
            };
        }
    }

    // Pass 2: relabel by root, assigning final ids in raster-scan order
    // of each root's first appearance — this is what makes the numbering
    // match scipy's "first pixel discovered" convention regardless of
    // which provisional id ended up as the union-find root.
    let mut root_to_final: HashMap<usize, i32> = HashMap::new();
    let mut next_final = 1i32;
    for r in 0..n_rows {
        for c in 0..n_cols {
            let p = provisional[[r, c]];
            if p == 0 {
                continue;
            }
            let root = uf.find(p);
            let final_label = *root_to_final.entry(root).or_insert_with(|| {
                let id = next_final;
                next_final += 1;
                id
            });
            labels[[r, c]] = final_label;
        }
    }

    (labels, (next_final - 1) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask_from(rows: &[&[u8]]) -> Array2<bool> {
        let n_rows = rows.len();
        let n_cols = rows[0].len();
        Array2::from_shape_fn((n_rows, n_cols), |(r, c)| rows[r][c] != 0)
    }

    #[test]
    fn empty_mask_yields_zero_features() {
        let mask = Array2::<bool>::from_elem((3, 3), false);
        let (labels, n) = label_4connected(&mask);
        assert_eq!(n, 0);
        assert!(labels.iter().all(|&v| v == 0));
    }

    #[test]
    fn zero_sized_mask_does_not_panic() {
        let mask = Array2::<bool>::from_elem((0, 5), false);
        let (labels, n) = label_4connected(&mask);
        assert_eq!(n, 0);
        assert_eq!(labels.dim(), (0, 5));
    }

    #[test]
    fn single_pixel_is_one_feature() {
        let mask = mask_from(&[&[0, 0], &[0, 1]]);
        let (labels, n) = label_4connected(&mask);
        assert_eq!(n, 1);
        assert_eq!(labels[[1, 1]], 1);
        assert_eq!(labels[[0, 0]], 0);
    }

    #[test]
    fn diagonal_touch_is_two_features_not_one() {
        // scipy's default structure is 4-connectivity: diagonal contact
        // alone must NOT merge two components.
        let mask = mask_from(&[&[1, 0], &[0, 1]]);
        let (labels, n) = label_4connected(&mask);
        assert_eq!(n, 2);
        assert_eq!(labels[[0, 0]], 1);
        assert_eq!(labels[[1, 1]], 2);
    }

    #[test]
    fn u_shape_is_one_feature_via_bottom_row_bridge() {
        // Matches the scipy cross-check performed this session:
        // [[1,0,1],[1,0,1],[1,1,1]] -> single label 1, not two labels
        // that only later get unioned.
        let mask = mask_from(&[&[1, 0, 1], &[1, 0, 1], &[1, 1, 1]]);
        let (labels, n) = label_4connected(&mask);
        assert_eq!(n, 1);
        assert!(labels.iter().zip(mask.iter()).all(|(&l, &m)| (l != 0) == m));
    }

    #[test]
    fn raster_scan_numbering_matches_scipy_reference() {
        // Matches the scipy cross-check performed this session exactly:
        // input [[0,1,0,1],[0,0,0,1],[1,0,0,0]] ->
        // [[0,1,0,2],[0,0,0,2],[3,0,0,0]], 3 features.
        let mask = mask_from(&[&[0, 1, 0, 1], &[0, 0, 0, 1], &[1, 0, 0, 0]]);
        let (labels, n) = label_4connected(&mask);
        assert_eq!(n, 3);
        let expected =
            Array2::from_shape_vec((3, 4), vec![0, 1, 0, 2, 0, 0, 0, 2, 3, 0, 0, 0]).unwrap();
        assert_eq!(labels, expected);
    }

    #[test]
    fn union_resolves_after_later_bridge_with_correct_first_discovery_numbering() {
        // Two provisional components (labeled 1 and 2 on discovery) that
        // merge on the second row must end up as ONE final label, numbered
        // by whichever pixel was discovered first (row 0) — not by
        // whichever provisional id happened to become the union-find
        // root. A prior version of this test only asserted
        // `n == 1` and "every label is 0 or 1" — with `n == 1` already
        // established, that second assertion is vacuous (a single-feature
        // labeling can only ever contain 0s and 1s, correct or not), so it
        // couldn't have caught a bug that mislabeled WHICH cells got 0 vs
        // 1 (e.g. a swapped background/foreground, or foreground cells
        // spuriously split across two different final ids). Asserting
        // labels match `mask` cell-for-cell (foreground -> 1, background
        // -> 0) actually checks that.
        let mask = mask_from(&[&[1, 0, 1], &[1, 1, 1]]);
        let (labels, n) = label_4connected(&mask);
        assert_eq!(n, 1);
        assert!(labels.iter().zip(mask.iter()).all(|(&l, &m)| (l != 0) == m));
    }
}
