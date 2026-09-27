//! NEXRAD Level 3 (NIDS) decode parity — value-parity against xradar's
//! `NEXRADLevel3File.raw_data`, on real fixtures, for the two product
//! families the local byte-exact oracle (`test_nexrad_level3_parity.rs`'s
//! Tier 1) cannot cover at all: it hard-rejects anything but packet 16
//! (see `docs/NEXRAD_LEVEL3_WASM.md`), so packet `AF1F` (N0S) has no
//! Tier-1 real-file coverage, and this adds two more packet-16 tilts
//! (N1B/N2B) for grid-shape diversity.
//!
//! **This is a distinct evidence tier from Tier 1**, not a stronger or
//! weaker version of it: the local oracle in `nexrad_level3.py` is a
//! from-scratch, independently authored decoder radish is checked
//! byte-for-byte against; xradar's reader is a *different* independent
//! implementation of the same ICD, used here only for the fields both
//! sides can define unambiguously — the raw per-gate byte grid
//! (`raw_data`/`raw_codes`, identical before either side applies a
//! scale or category table) and the ray-center azimuth array (both
//! readers use the same `start + width/2` convention). It does **not**
//! cross-check physical values, units, or scale/offset — xradar
//! deliberately diverges from Py-ART on some of those (see xradar#392's
//! own PR body), and reconciling that is out of scope here.
//!
//! **One documented divergence, not a bug on radish's side**: on an odd
//! `n_bins`, NEXRAD ICD 2620001AC Figure 3-11c Note 1 says packet 16 pads
//! each radial to a halfword boundary with one inert trailing byte — the
//! packet header's `n_bins` is still the true gate count. radish honors
//! this (see `decode_symbology_tolerates_pad_byte_beyond_n_bins` in
//! `decode/symbology.rs`); xradar's PR-392 branch (commit `9c8826c`)
//! does not, and reports the padded, one-wider shape instead. Confirmed
//! against real data before trusting either side: for both odd-`n_bins`
//! fixtures in this corpus (N1B, N3B — see `test_nexrad_level3_parity.rs`
//! for N3B), the "extra" byte is 0 on every single radial, and 11 other
//! real fixtures with even `n_bins` show zero header/byte-count
//! disagreement at all — a clean, ICD-explained pattern, not sampling
//! noise. `generate_expected_xradar.py` trims that documented pad column
//! before hashing, so the sidecars here already reflect the ICD-correct
//! value; no special-casing needed in the comparison below.
//!
//! **Pinned to a specific commit, not a live dependency.** The
//! `expected/*.xradar.json` sidecars are committed output — see
//! `radish/tests/fixtures/nexrad_level3/generate_expected_xradar.py`'s
//! module doc for the exact pinned commit and how to regenerate. CI
//! never builds or runs xradar's PR branch; these tests read only the
//! committed JSON.
//!
//! **Extended by plan 0012** (closing the 7 previously-deferred codes):
//! `DAA` (170, `Precip`) and `HHC` (177, `ClassInt` — confirmed via real
//! fixtures to arrive via packet 16, NOT packet 28 as originally assumed;
//! see `products.rs`'s module doc) reuse this exact `u8`/`raw_codes`
//! comparison, added to `decode_matches_xradar_raw_data`'s cases below.
//! `DPR` (176, `Rate`, packet 28/XDR) needs a SEPARATE comparison
//! (`decode_matches_xradar_raw_data_u16`) since its raw codes are `u16`
//! (`raw_codes_u16`, not `raw_codes`) — see
//! `generate_expected_xradar_u16.py`'s module doc for why that's a
//! distinct generator script, not a branch in this one. All three used to
//! be covered by `unimplemented_real_products_reject_cleanly` below,
//! before this pass — moved out of it now that they decode (a stale
//! "this rejects" case testing dead code is worse than no test).
//!
//! Fixture-parity cases are `#[ignore]`d and skip cleanly (not fail) when
//! `RADISH_NEXRAD_LEVEL3_FIXTURE_DIR` is unset or a file is missing —
//! same discipline as `test_nexrad_level3_parity.rs`.

mod common;

use ndarray::Array2;
use radish::backends::{NexradLevel3Backend, RadarBackend};
use rstest::rstest;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use common::{fixture_path, hex32, load_expected_file};

/// Mirrors the fields `generate_expected_xradar.py` writes.
#[derive(Debug, Deserialize)]
struct XradarExpected {
    fixture: String,
    fixture_sha256: String,
    message_code: u16,
    n_radials: usize,
    n_bins: usize,
    azimuths: Vec<f32>,
    codes_sha256: String,
}

fn expected_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nexrad_level3/expected")
}

/// Same computation as `test_nexrad_level3_parity.rs`'s `codes_sha256` —
/// duplicated rather than shared across test binaries (each integration
/// test binary compiles independently; `tests/common/mod.rs` is the
/// established sharing mechanism, and this one function isn't worth
/// promoting into it for two call sites).
fn codes_sha256(codes: &Array2<u8>) -> String {
    let flat = codes
        .as_slice()
        .expect("freshly-decoded Array2<u8> is always contiguous, row-major");
    hex32(&Sha256::digest(flat).into())
}

/// The actual comparison `decode_matches_xradar_raw_data` performs —
/// pulled out so `sabotage_codes_comparison_catches_a_mismatch` below
/// exercises this exact code path, not just `codes_sha256` in isolation.
/// A wiring bug here (e.g. accidentally comparing a value against
/// itself) would make the real, fixture-gated test vacuously pass; with
/// the assertion itself extracted, that same bug makes the sabotage
/// test's `#[should_panic]` stop panicking — which runs on every
/// `cargo test`, fixtures or not.
fn assert_codes_match(name: &str, actual: &Array2<u8>, expected_sha256: &str) {
    let actual_sha = codes_sha256(actual);
    assert_eq!(
        actual_sha, expected_sha256,
        "{name}: codes array diverges from xradar's raw_data (SHA-256 mismatch)"
    );
}

#[rstest]
#[case::n0s_af1f("LOT_N0S_2026_07_17_19_30_15")]
#[case::n0h_classint("LOT_N0H_2026_07_17_19_30_15")]
#[case::n1b("LOT_N1B_2026_07_17_19_30_15")]
#[case::n2b("LOT_N2B_2026_07_17_19_30_15")]
#[case::daa_precip("LOT_DAA_2026_07_17_19_30_15")]
#[case::hhc_classint_via_packet16("LOT_HHC_2026_07_17_19_30_15")]
#[case::du3_precip("LOT_DU3_2026_08_09_21_12_25")]
#[ignore = "needs RADISH_NEXRAD_LEVEL3_FIXTURE_DIR; see CORPUS.md"]
fn decode_matches_xradar_raw_data(#[case] name: &str) {
    let Some(path) = fixture_path("RADISH_NEXRAD_LEVEL3_FIXTURE_DIR", name) else {
        eprintln!("skipping {name}: RADISH_NEXRAD_LEVEL3_FIXTURE_DIR not set or file missing");
        return;
    };
    let expected: XradarExpected =
        load_expected_file(&expected_dir(), &format!("{name}.xradar.json"));
    assert_eq!(expected.fixture, name, "sidecar/fixture name mismatch");

    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{name}: read fixture: {e}"));
    // Guards against a stale or wrong file sitting at this path under
    // RADISH_NEXRAD_LEVEL3_FIXTURE_DIR — without this, a fixture-identity
    // drift would just decode as SOME product and could pass or fail the
    // comparisons below for the wrong reason entirely.
    assert_eq!(
        hex32(&Sha256::digest(&bytes).into()),
        expected.fixture_sha256,
        "{name}: on-disk fixture doesn't match the sidecar's recorded fixture_sha256 \
         (wrong or stale file at this path?)"
    );
    let backend = NexradLevel3Backend::new();
    let volume = backend
        .read_bytes_volume(bytes)
        .unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
    let sweep = &volume.sweeps[0];

    let nids = sweep
        .metadata
        .nids
        .as_ref()
        .unwrap_or_else(|| panic!("{name}: sweep.metadata.nids is None"));
    assert_eq!(
        nids.message_code, expected.message_code,
        "{name}: message_code"
    );
    assert_eq!(sweep.num_rays(), expected.n_radials, "{name}: n_radials");
    assert_eq!(sweep.num_gates(), expected.n_bins, "{name}: n_bins");
    assert_eq!(
        sweep.coordinates.azimuth, expected.azimuths,
        "{name}: azimuth array diverges from xradar's raw_data-derived azimuths"
    );

    // Exactly one moment per NIDS product — grab it without assuming its
    // canonical name, since this test spans two different families.
    assert_eq!(
        sweep.moments.len(),
        1,
        "{name}: NIDS products carry exactly one moment"
    );
    let (_, moment) = sweep
        .moments
        .iter()
        .next()
        .unwrap_or_else(|| panic!("{name}: no moment decoded"));
    let codes = moment
        .raw_codes
        .as_ref()
        .unwrap_or_else(|| panic!("{name}: raw_codes is None"));
    assert_codes_match(name, codes, &expected.codes_sha256);
}

/// Mirrors [`XradarExpected`] for the `u16` sidecar shape
/// `generate_expected_xradar_u16.py` writes — same fields, no
/// `codes_sha256` reuse across the two structs since the `u8`/`u16` hash
/// inputs are computed differently (see [`codes_sha256_u16`]'s doc).
#[derive(Debug, Deserialize)]
struct XradarExpectedU16 {
    fixture: String,
    fixture_sha256: String,
    message_code: u16,
    n_radials: usize,
    n_bins: usize,
    azimuths: Vec<f32>,
    codes_sha256: String,
}

/// `u16` counterpart to `codes_sha256` — hashes each element's BIG-ENDIAN
/// bytes explicitly, not `codes.as_slice()`'s raw memory (native-endian,
/// i.e. little-endian on every real deployment target this crate ships
/// for: x86_64, aarch64, wasm32). `generate_expected_xradar_u16.py`
/// hashes `raw_data.astype(">u2").tobytes()` — this must match that exact
/// byte order or the comparison would fail on every platform even when
/// the DATA is identical, for a reason that has nothing to do with a real
/// decode bug.
fn codes_sha256_u16(codes: &Array2<u16>) -> String {
    let flat = codes
        .as_slice()
        .expect("freshly-decoded Array2<u16> is always contiguous, row-major");
    let mut be_bytes = Vec::with_capacity(flat.len() * 2);
    for &v in flat {
        be_bytes.extend_from_slice(&v.to_be_bytes());
    }
    hex32(&Sha256::digest(&be_bytes).into())
}

/// [`RATE`]/code 176 (`DPR`) — packet 28 (XDR), the one packet family this
/// backend produces `u16` raw codes for (`raw_codes_u16`, not
/// `raw_codes`). Same structure as [`decode_matches_xradar_raw_data`], a
/// separate function because the field it reads off `MomentData` and the
/// hash computation both genuinely differ by width — not because the
/// comparison itself is conceptually different.
#[rstest]
#[case::dpr_rate_packet28("LOT_DPR_2026_07_17_19_30_15")]
#[ignore = "needs RADISH_NEXRAD_LEVEL3_FIXTURE_DIR; see CORPUS.md"]
fn decode_matches_xradar_raw_data_u16(#[case] name: &str) {
    let Some(path) = fixture_path("RADISH_NEXRAD_LEVEL3_FIXTURE_DIR", name) else {
        eprintln!("skipping {name}: RADISH_NEXRAD_LEVEL3_FIXTURE_DIR not set or file missing");
        return;
    };
    let expected: XradarExpectedU16 =
        load_expected_file(&expected_dir(), &format!("{name}.xradar.u16.json"));
    assert_eq!(expected.fixture, name, "sidecar/fixture name mismatch");

    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{name}: read fixture: {e}"));
    assert_eq!(
        hex32(&Sha256::digest(&bytes).into()),
        expected.fixture_sha256,
        "{name}: on-disk fixture doesn't match the sidecar's recorded fixture_sha256 \
         (wrong or stale file at this path?)"
    );
    let backend = NexradLevel3Backend::new();
    let volume = backend
        .read_bytes_volume(bytes)
        .unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));
    let sweep = &volume.sweeps[0];

    let nids = sweep
        .metadata
        .nids
        .as_ref()
        .unwrap_or_else(|| panic!("{name}: sweep.metadata.nids is None"));
    assert_eq!(
        nids.message_code, expected.message_code,
        "{name}: message_code"
    );
    assert_eq!(sweep.num_rays(), expected.n_radials, "{name}: n_radials");
    assert_eq!(sweep.num_gates(), expected.n_bins, "{name}: n_bins");
    assert_eq!(
        sweep.coordinates.azimuth, expected.azimuths,
        "{name}: azimuth array diverges from xradar's raw_data-derived azimuths \
         (see plan 0012 §3.3 step 6 for the leading-edge-correction convention \
         both sides must apply identically here)"
    );

    assert_eq!(
        sweep.moments.len(),
        1,
        "{name}: NIDS products carry exactly one moment"
    );
    let (_, moment) = sweep
        .moments
        .iter()
        .next()
        .unwrap_or_else(|| panic!("{name}: no moment decoded"));
    let codes = moment
        .raw_codes_u16
        .as_ref()
        .unwrap_or_else(|| panic!("{name}: raw_codes_u16 is None"));
    let actual_sha = codes_sha256_u16(codes);
    assert_eq!(
        actual_sha, expected.codes_sha256,
        "{name}: codes array diverges from xradar's raw_data (SHA-256 mismatch)"
    );
}

/// Sabotage-verify for [`codes_sha256`] — same discipline as
/// `test_nexrad_level3_parity.rs`'s version, duplicated for the same
/// reason that function is (see its doc comment).
#[test]
fn sabotage_codes_sha256_mismatch_is_detected() {
    let good = Array2::from_shape_vec((2, 4), vec![2u8, 3, 4, 5, 6, 7, 8, 9]).unwrap();
    let mut sabotaged = good.clone();
    sabotaged[[0, 3]] ^= 0xFF;

    let good_hash = codes_sha256(&good);
    let sabotaged_hash = codes_sha256(&sabotaged);

    assert_ne!(
        good_hash, sabotaged_hash,
        "a single-byte perturbation must change the SHA-256 digest"
    );
}

/// Sabotage-verify for [`assert_codes_match`] itself — NOT just
/// `codes_sha256` above. `decode_matches_xradar_raw_data` is
/// `#[ignore]`d (needs real fixtures), so CI never runs the real
/// comparison; this test runs unconditionally and calls the exact same
/// `assert_codes_match` function that test calls, with a deliberately
/// wrong expected hash. If a future refactor ever made that comparison
/// vacuous (e.g. comparing a value against itself), this stops
/// panicking and fails — catching the class of bug the sibling test
/// above, which only checks `codes_sha256`'s sensitivity to input
/// changes, cannot.
#[test]
#[should_panic(expected = "codes array diverges from xradar's raw_data")]
fn sabotage_codes_comparison_catches_a_mismatch() {
    let codes = Array2::from_shape_vec((2, 4), vec![2u8, 3, 4, 5, 6, 7, 8, 9]).unwrap();
    let deliberately_wrong_sha256 = "0".repeat(64);
    assert_codes_match("sabotage", &codes, &deliberately_wrong_sha256);
}

/// Sabotage-verify for [`codes_sha256_u16`] — same discipline as
/// `codes_sha256`'s sibling above, and also locks in that a value change
/// ABOVE 255 (impossible to represent, let alone perturb, in the `u8`
/// version) is caught — the entire reason this function exists instead of
/// reusing `codes_sha256` with a cast.
#[test]
fn sabotage_codes_sha256_u16_mismatch_is_detected() {
    let good = Array2::from_shape_vec((2, 2), vec![0u16, 300, 7874, 65535]).unwrap();
    let mut sabotaged = good.clone();
    sabotaged[[1, 1]] -= 1; // 65535 -> 65534, a change only representable in u16

    let good_hash = codes_sha256_u16(&good);
    let sabotaged_hash = codes_sha256_u16(&sabotaged);

    assert_ne!(
        good_hash, sabotaged_hash,
        "a single-element perturbation must change the SHA-256 digest"
    );
}
