//! Real-fixture confirmation for `decode_nexrad_level3`, complementing
//! `decode.rs`'s synthetic-byte suite in this same directory (see that
//! file's module doc for why both exist).
//!
//! Gated behind `--cfg has_real_fixtures` (see `wasm/build.rs`), OFF by
//! default — `include_bytes!`/`include_str!` need the fixture path at
//! COMPILE time (wasm32-unknown-unknown has no runtime filesystem access,
//! unlike the native-target Rust integration tests'
//! `RADISH_NEXRAD_LEVEL3_FIXTURE_DIR`-at-test-time convention in
//! `radish/tests/`), so a plain `cargo test -p radish-wasm --target
//! wasm32-unknown-unknown` must never require a fixture directory to even
//! compile. Run explicitly:
//!
//! ```text
//! RADISH_NEXRAD_LEVEL3_FIXTURE_DIR=~/.cache/radish/fixtures/nexrad_level3 \
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!     cargo test -p radish-wasm --target wasm32-unknown-unknown \
//!         --test real_fixtures
//! ```
//!
//! Fixture names/hashes match `radish/tests/fixtures/CORPUS.md`'s xradar
//! cross-check corpus exactly — no new fixtures introduced, no new
//! provenance to track. The oracle sidecars under
//! `radish/tests/fixtures/nexrad_level3/expected/` are read directly
//! (`include_str!`) rather than re-deriving separate expected values here,
//! so this suite can't drift from what
//! `test_nexrad_level3_xradar_oracle.rs` already asserts against xradar.

#![cfg(has_real_fixtures)]

use serde::Deserialize;
use sha2::{Digest, Sha256};
use wasm_bindgen_test::*;

use radish_wasm::decode_nexrad_level3;

// Node is this crate's `wasm-bindgen-test` default already — see
// decode.rs's matching note.

/// `include_bytes!`/`include_str!` a fixture (or its sidecar) by name from
/// `RADISH_NEXRAD_LEVEL3_FIXTURE_DIR`/the committed `expected/` dir — both
/// are compile-time macros, so this has to be a `macro_rules!` (a `const
/// fn`/helper can't expand `include_bytes!`'s path argument at the call
/// site the way this needs).
macro_rules! fixture_bytes {
    ($name:literal) => {
        include_bytes!(concat!(
            env!("RADISH_NEXRAD_LEVEL3_FIXTURE_DIR"),
            "/",
            $name
        ))
    };
}

macro_rules! expected_json {
    ($filename:literal) => {
        include_str!(concat!(
            "../../radish/tests/fixtures/nexrad_level3/expected/",
            $filename
        ))
    };
}

const DAA_BYTES: &[u8] = fixture_bytes!("LOT_DAA_2026_07_17_19_30_15");
const DPR_BYTES: &[u8] = fixture_bytes!("LOT_DPR_2026_07_17_19_30_15");
const HHC_BYTES: &[u8] = fixture_bytes!("LOT_HHC_2026_07_17_19_30_15");

const DAA_EXPECTED_JSON: &str = expected_json!("LOT_DAA_2026_07_17_19_30_15.xradar.json");
const DPR_EXPECTED_JSON: &str = expected_json!("LOT_DPR_2026_07_17_19_30_15.xradar.u16.json");
const HHC_EXPECTED_JSON: &str = expected_json!("LOT_HHC_2026_07_17_19_30_15.xradar.json");

/// Only the fields this suite checks — same shape as
/// `test_nexrad_level3_xradar_oracle.rs`'s `XradarExpected`/
/// `XradarExpectedU16`, trimmed to what's used here.
#[derive(Deserialize)]
struct Expected {
    fixture: String,
    fixture_sha256: String,
    codes_sha256: String,
}

fn load_expected(json: &str) -> Expected {
    serde_json::from_str(json).expect("committed oracle sidecar should parse")
}

fn hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}")).unwrap();
    }
    s
}

/// Same computation as the oracle test's `codes_sha256` — genuinely
/// sensitive to a divergence (including reordering) anywhere in the array,
/// unlike a permutation-invariant sum.
fn codes_sha256_u8(arr: &js_sys::Uint8Array) -> String {
    hex32(&Sha256::digest(arr.to_vec()).into())
}

/// `u16` counterpart — hashes each element's BIG-ENDIAN bytes, matching
/// `generate_expected_xradar_u16.py`'s `raw_data.astype(">u2").tobytes()`
/// and the oracle test's `codes_sha256_u16` exactly (see that function's
/// doc for why native/little-endian byte order would silently fail here).
fn codes_sha256_u16(arr: &js_sys::Uint16Array) -> String {
    let values = arr.to_vec();
    let mut be_bytes = Vec::with_capacity(values.len() * 2);
    for v in values {
        be_bytes.extend_from_slice(&v.to_be_bytes());
    }
    hex32(&Sha256::digest(&be_bytes).into())
}

#[wasm_bindgen_test]
fn real_daa_decodes_precip_u8() {
    let expected = load_expected(DAA_EXPECTED_JSON);
    assert_eq!(expected.fixture, "LOT_DAA_2026_07_17_19_30_15");
    assert_eq!(
        hex32(&Sha256::digest(DAA_BYTES).into()),
        expected.fixture_sha256,
        "wrong or stale fixture at RADISH_NEXRAD_LEVEL3_FIXTURE_DIR"
    );

    let product = decode_nexrad_level3(DAA_BYTES).expect("real DAA object should decode");
    assert_eq!(product.moment(), "ACCUM");
    assert_eq!(product.message_code(), 170);
    assert_eq!(product.codes_width(), 8);
    assert_eq!(product.n_radials(), 360);
    assert_eq!(product.n_bins(), 920);
    let codes = product.codes();
    assert_eq!(codes.length(), 360 * 920);
    assert_eq!(
        codes_sha256_u8(&codes),
        expected.codes_sha256,
        "codes array diverges from xradar's raw_data (SHA-256 mismatch)"
    );
    assert_eq!(product.codes_u16().length(), 0);
}

#[wasm_bindgen_test]
fn real_dpr_decodes_rate_u16_via_packet28() {
    let expected = load_expected(DPR_EXPECTED_JSON);
    assert_eq!(expected.fixture, "LOT_DPR_2026_07_17_19_30_15");
    assert_eq!(
        hex32(&Sha256::digest(DPR_BYTES).into()),
        expected.fixture_sha256,
        "wrong or stale fixture at RADISH_NEXRAD_LEVEL3_FIXTURE_DIR"
    );

    let product = decode_nexrad_level3(DPR_BYTES).expect("real DPR object should decode");
    assert_eq!(product.moment(), "RATE");
    assert_eq!(product.message_code(), 176);
    assert_eq!(product.codes_width(), 16);
    assert_eq!(product.n_radials(), 360);
    assert_eq!(product.n_bins(), 920);
    assert_eq!(product.gate_spacing_m(), 250.0);
    assert_eq!(product.first_gate_m(), 125.0);
    let codes_u16 = product.codes_u16();
    assert_eq!(codes_u16.length(), 360 * 920);
    assert_eq!(
        codes_sha256_u16(&codes_u16),
        expected.codes_sha256,
        "codes array diverges from xradar's raw_data (SHA-256 mismatch)"
    );
    assert_eq!(product.codes().length(), 0);
    // Centred per NEXRAD ICD 2620001AC Appendix E Figure E-4 (see
    // docs/NEXRAD_LEVEL3_WASM.md §4.5/§11) — 360 radials, 1 deg apart,
    // starting at 0.5, not 0.0.
    let az = product.azimuths();
    assert!((az.get_index(0) - 0.5).abs() < 1e-6);
}

#[wasm_bindgen_test]
fn real_hhc_decodes_classint_via_packet16_not_packet28() {
    // The correction this pass made: HHC/177 is packet 16 (u8), not
    // packet 28 (u16) — this plan's own design doc originally assumed
    // the opposite. Locks the real-data behaviour in at the wasm
    // boundary specifically, not just the Rust-internal decode path
    // `radish/tests/test_nexrad_level3_xradar_oracle.rs` already covers.
    let expected = load_expected(HHC_EXPECTED_JSON);
    assert_eq!(expected.fixture, "LOT_HHC_2026_07_17_19_30_15");
    assert_eq!(
        hex32(&Sha256::digest(HHC_BYTES).into()),
        expected.fixture_sha256,
        "wrong or stale fixture at RADISH_NEXRAD_LEVEL3_FIXTURE_DIR"
    );

    let product = decode_nexrad_level3(HHC_BYTES).expect("real HHC object should decode");
    assert_eq!(product.moment(), "HCLASS");
    assert_eq!(product.message_code(), 177);
    assert_eq!(product.codes_width(), 8);
    let codes = product.codes();
    assert_eq!(codes.length(), 360 * 920);
    assert_eq!(
        codes_sha256_u8(&codes),
        expected.codes_sha256,
        "codes array diverges from xradar's raw_data (SHA-256 mismatch)"
    );
    assert_eq!(product.codes_u16().length(), 0);
}
