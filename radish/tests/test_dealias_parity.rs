//! Velocity dealiasing parity — bit-exact vs. a real Py-ART install, on
//! real NEXRAD Level 2 velocity sweeps. See
//! `radish/tests/fixtures/dealias/generate_expected.py` (the golden-
//! corpus generator) for how the `expected/*.json` sidecars were built,
//! and `radish/tests/fixtures/CORPUS.md` for the corpus itself.
//!
//! **Py-ART version pinned**: `2.2.0`. Re-run the generator against a
//! Py-ART install of that version and update this comment if the pinned
//! version changes.
//!
//! Fixture-parity cases are `#[ignore]`d and skip cleanly (not fail) when
//! `RADISH_NEXRAD_FIXTURE_DIR` is unset or the fixture is missing — same
//! discipline as `test_nexrad_level3_parity.rs`. The sabotage-verify test
//! is not `#[ignore]`d and needs no fixtures.

mod common;

use ndarray::Array2;
use radish::backends::{NexradBackend, RadarBackend};
use radish::transforms::{dealias_region_based, DealiasOptions};
use rstest::rstest;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use common::{fixture_path, hex32, load_expected};

#[derive(Debug, Deserialize)]
struct Expected {
    fixture: String,
    sweep_index: usize,
    moment: String,
    nyquist: f32,
    rays_wrap_around: bool,
    shape: (usize, usize),
    n_valid_gates: usize,
    folds_sha256: String,
}

fn expected_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dealias/expected")
}

/// SHA-256 of a fold-count array's full row-major `int32` bytes — the
/// exact computation both [`run_case`]'s real comparison and
/// `sabotage_folds_sha256_mismatch_is_detected` use, so the sabotage test
/// actually exercises this function rather than a hand-rolled parallel
/// one (an adversarial review this session noted the previous version of
/// the sabotage test only proved SHA-256's generic diffusion property on
/// a bare `Vec<i32>`, not that this file's own comparison wiring works).
fn folds_sha256(folds: &Array2<i32>) -> String {
    let flat = folds
        .as_slice()
        .expect("freshly-computed Array2<i32> is always contiguous, row-major");
    let flat_bytes: Vec<u8> = flat.iter().flat_map(|v| v.to_ne_bytes()).collect();
    hex32(&Sha256::digest(&flat_bytes).into())
}

/// One golden-corpus case: decode a real fixture's sweep, dealias it, and
/// compare the fold-count array's SHA-256 against the sidecar Py-ART
/// itself produced (see `generate_expected.py`).
fn run_case(sidecar_name: &str) {
    let expected: Expected = load_expected(&expected_dir(), sidecar_name);
    let Some(path) = fixture_path("RADISH_NEXRAD_FIXTURE_DIR", &expected.fixture) else {
        eprintln!(
            "skipping {sidecar_name}: RADISH_NEXRAD_FIXTURE_DIR not set or {} missing",
            expected.fixture
        );
        return;
    };

    let volume = NexradBackend::new()
        .read_volume(&path)
        .unwrap_or_else(|e| panic!("{sidecar_name}: decode failed: {e}"));
    let sweep = volume
        .sweeps
        .get(expected.sweep_index)
        .unwrap_or_else(|| panic!("{sidecar_name}: sweep {} missing", expected.sweep_index));
    let moment = sweep
        .moments
        .get(&expected.moment)
        .unwrap_or_else(|| panic!("{sidecar_name}: moment {} missing", expected.moment));

    let velocity = &moment.data;
    assert_eq!(
        (velocity.nrows(), velocity.ncols()),
        expected.shape,
        "{sidecar_name}: velocity shape drifted from the fixture the sidecar was generated on \
         (re-run generate_expected.py if this is an intentional decoder change)"
    );

    // Matches `generate_expected.py`'s `np.isfinite(velocity)` exactly:
    // NaN marks a missing/below-threshold gate in radish's decoded
    // output, same convention on both sides of this gate.
    let valid: Array2<bool> = velocity.mapv(|v| v.is_finite());
    let n_valid = valid.iter().filter(|&&v| v).count();
    assert_eq!(
        n_valid, expected.n_valid_gates,
        "{sidecar_name}: valid-gate count drifted from the sidecar"
    );

    let folds = dealias_region_based(
        velocity,
        &valid,
        expected.nyquist,
        expected.rays_wrap_around,
        DealiasOptions::default(),
    )
    .unwrap_or_else(|e| panic!("{sidecar_name}: dealias_region_based failed: {e}"));

    let actual_sha = folds_sha256(&folds);
    assert_eq!(
        actual_sha, expected.folds_sha256,
        "{sidecar_name}: fold-count array diverges from Py-ART (SHA-256 mismatch)"
    );
}

#[rstest]
#[case::sweep1("KLOT20251210_102338_V06_sweep1")]
#[case::sweep9("KLOT20251210_102338_V06_sweep9")]
#[ignore = "needs RADISH_NEXRAD_FIXTURE_DIR; see CORPUS.md and fixtures/dealias/generate_expected.py"]
fn klot_matches_pyart(#[case] sidecar_name: &str) {
    run_case(sidecar_name);
}

/// Sabotage-verify: computes [`folds_sha256`] (the SAME function
/// [`run_case`]'s real comparison uses) on a known array, perturbs one
/// fold, and confirms the digest changes. Runs unconditionally — no
/// fixtures needed.
#[test]
fn sabotage_folds_sha256_mismatch_is_detected() {
    let good = Array2::from_shape_vec((2, 4), vec![0, 1, -1, 2, 0, -2, 1, 1]).unwrap();
    let mut sabotaged = good.clone();
    sabotaged[[0, 3]] += 1; // perturb one fold count

    let good_hash = folds_sha256(&good);
    let sabotaged_hash = folds_sha256(&sabotaged);

    assert_ne!(
        good_hash, sabotaged_hash,
        "a single-fold perturbation must change the SHA-256 digest"
    );
}
