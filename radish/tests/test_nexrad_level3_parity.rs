//! NEXRAD Level 3 (NIDS) decode parity — Tier 1: byte-exact vs. an
//! independent Python decode oracle.
//!
//! See `radish/tests/fixtures/CORPUS.md`'s "NEXRAD Level 3 (NIDS) corpus"
//! section for how the fixtures and their `expected/*.json` sidecars were
//! produced, and `docs/NEXRAD_LEVEL3_WASM.md` for why this is the
//! CI-blocking gate for the 6 byte-verified products (plus N3B, a
//! same-family grid-shape case).
//!
//! Fixture-parity cases are `#[ignore]`d and skip cleanly (not fail) when
//! `RADISH_NEXRAD_LEVEL3_FIXTURE_DIR` is unset or a file is missing — the
//! sabotage-verify test at the bottom is the one case here that always
//! runs.

mod common;

use ndarray::Array2;
use radish::backends::{NexradLevel3Backend, RadarBackend};
use rstest::rstest;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use common::{fixture_path, hex32, load_expected};

/// Mirrors the fields `generate_expected.py` writes. `#[serde(default)]`
/// isn't used anywhere here deliberately — a missing field should fail
/// deserialization loudly rather than silently compare against `0`/`""`.
#[derive(Debug, Deserialize)]
struct Expected {
    fixture: String,
    site: String,
    product: String,
    moment: String,
    tilt: u8,
    message_code: u16,
    vcp: u16,
    elevation_deg: f64,
    scan_time_unix: f64,
    lat: f64,
    lon: f64,
    height_m: f64,
    n_radials: usize,
    n_bins: usize,
    first_gate_m: f32,
    gate_spacing_m: f32,
    value_min: f32,
    value_increment: f32,
    n_levels: u16,
    azimuths: Vec<f32>,
    codes_sha256: String,
}

fn expected_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nexrad_level3/expected")
}

/// SHA-256 of a decoded codes array's full row-major bytes — the exact
/// computation both the real comparison below and
/// `sabotage_codes_sha256_mismatch_is_detected` use, so the sabotage test
/// exercises this function rather than a hand-rolled parallel one.
fn codes_sha256(codes: &Array2<u8>) -> String {
    let flat = codes
        .as_slice()
        .expect("freshly-decoded Array2<u8> is always contiguous, row-major");
    hex32(&Sha256::digest(flat).into())
}

#[rstest]
#[case::n0b("LOT_N0B_2026_07_31_13_06_53")]
#[case::n3b("LOT_N3B_2026_07_31_13_02_14")]
#[case::n0x("LOT_N0X_2020_03_30_00_02_07")]
#[case::n0c("LOT_N0C_2020_03_31_00_05_24")]
#[case::n0k("LOT_N0K_2020_03_31_00_05_24")]
#[case::n0g("LOT_N0G_2026_08_04_00_09_57")]
#[case::n2u("LOT_N2U_2026_08_04_00_09_57")]
#[ignore = "needs RADISH_NEXRAD_LEVEL3_FIXTURE_DIR; see CORPUS.md"]
fn decode_matches_python_oracle_byte_exact(#[case] name: &str) {
    let Some(path) = fixture_path("RADISH_NEXRAD_LEVEL3_FIXTURE_DIR", name) else {
        eprintln!("skipping {name}: RADISH_NEXRAD_LEVEL3_FIXTURE_DIR not set or file missing");
        return;
    };
    let expected: Expected = load_expected(&expected_dir(), name);
    assert_eq!(expected.fixture, name, "sidecar/fixture name mismatch");

    let bytes = std::fs::read(&path).expect("read fixture");
    let backend = NexradLevel3Backend::new();
    let volume = backend
        .read_bytes_volume(bytes)
        .unwrap_or_else(|e| panic!("{name}: decode failed: {e}"));

    assert_eq!(
        volume.num_sweeps(),
        1,
        "{name}: NIDS products are single-sweep"
    );
    let sweep = &volume.sweeps[0];

    // Volume-level georeference — the oracle's site fields.
    assert_eq!(volume.metadata.latitude, expected.lat, "{name}: latitude");
    assert_eq!(volume.metadata.longitude, expected.lon, "{name}: longitude");
    assert_eq!(
        volume.metadata.altitude, expected.height_m,
        "{name}: height_m"
    );
    assert_eq!(
        volume.metadata.time_coverage_start.timestamp() as f64,
        expected.scan_time_unix,
        "{name}: scan_time"
    );

    // NIDS attrs.
    let nids = sweep
        .metadata
        .nids
        .as_ref()
        .unwrap_or_else(|| panic!("{name}: sweep.metadata.nids is None"));
    assert_eq!(nids.site, expected.site, "{name}: site");
    assert_eq!(nids.awips_id, expected.product, "{name}: awips_id/product");
    assert_eq!(
        nids.message_code, expected.message_code,
        "{name}: message_code"
    );
    assert_eq!(nids.vcp, expected.vcp, "{name}: vcp");
    assert_eq!(
        nids.tilt.map(u32::from),
        Some(u32::from(expected.tilt)),
        "{name}: tilt"
    );
    assert_eq!(
        sweep.metadata.fixed_angle, expected.elevation_deg,
        "{name}: elevation_deg"
    );

    // Geometry + azimuths.
    assert_eq!(sweep.num_rays(), expected.n_radials, "{name}: n_radials");
    assert_eq!(sweep.num_gates(), expected.n_bins, "{name}: n_bins");
    assert_eq!(
        sweep.coordinates.range[0], expected.first_gate_m,
        "{name}: first_gate_m"
    );
    assert_eq!(
        sweep.coordinates.range[1] - sweep.coordinates.range[0],
        expected.gate_spacing_m,
        "{name}: gate_spacing_m"
    );
    assert_eq!(
        sweep.coordinates.azimuth, expected.azimuths,
        "{name}: azimuth array"
    );

    // The moment itself — declared scale, exact, and the codes checksum.
    let moment = sweep
        .moments
        .get(&expected.moment)
        .unwrap_or_else(|| panic!("{name}: moment {} not present", expected.moment));
    let scale = moment
        .declared_scale
        .unwrap_or_else(|| panic!("{name}: declared_scale is None"));
    assert_eq!(scale.value_min, expected.value_min, "{name}: value_min");
    assert_eq!(
        scale.value_increment, expected.value_increment,
        "{name}: value_increment"
    );
    assert_eq!(scale.n_levels, expected.n_levels, "{name}: n_levels");

    let codes = moment
        .raw_codes
        .as_ref()
        .unwrap_or_else(|| panic!("{name}: raw_codes is None"));
    let actual_sha = codes_sha256(codes);
    assert_eq!(
        actual_sha, expected.codes_sha256,
        "{name}: codes array diverges from the Python oracle (SHA-256 mismatch)"
    );
}

/// Sabotage-verify: computes [`codes_sha256`] (the SAME function the
/// real comparison above uses) on a known array, flips one byte, and
/// confirms the comparison actually reports inequality. Runs
/// unconditionally — no fixtures needed — so a change that accidentally
/// made the comparison above vacuously true (e.g. comparing a value
/// against itself) still gets caught by CI.
#[test]
fn sabotage_codes_sha256_mismatch_is_detected() {
    let good = Array2::from_shape_vec((2, 4), vec![2u8, 3, 4, 5, 6, 7, 8, 9]).unwrap();
    let mut sabotaged = good.clone();
    sabotaged[[0, 3]] ^= 0xFF; // flip one byte

    let good_hash = codes_sha256(&good);
    let sabotaged_hash = codes_sha256(&sabotaged);

    assert_ne!(
        good_hash, sabotaged_hash,
        "a single-byte perturbation must change the SHA-256 digest"
    );
}
