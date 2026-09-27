//! Real `wasm-bindgen-test` coverage for `decode_nexrad_level3` — this
//! crate had ZERO tests of any kind before this file (confirmed via
//! `find wasm -iname "*test*"` returning nothing). Runs the ACTUAL
//! compiled wasm binary under Node (this crate's `wasm-bindgen-test`
//! default — no `wasm_bindgen_test_configure!` call needed, see below),
//! not a native-target stub — `js_sys` compiles on the native target but
//! ABORTS at runtime there (no real JS engine backing it, confirmed by
//! direct probe), so this is the only way to genuinely exercise
//! `codes()`/`codesU16()`/`codesWidth` rather than just their Rust
//! source.
//!
//! Synthetic, ICD-shaped byte sequences, not real S3 fixtures — the
//! wasm32-unknown-unknown target has no runtime filesystem access, so a
//! real fixture has to be embedded at COMPILE time instead; see
//! `real_fixtures.rs` (this directory) for that, feature-gated separately
//! so a default `cargo test` here never needs a fixture directory to even
//! compile. These two are complementary, not redundant: this file proves
//! the wasm-facing width/codesWidth logic itself is correct on every
//! build; `real_fixtures.rs` additionally proves it against bytes this
//! crate didn't construct.
//!
//! Byte layouts mirror `radish::backends::nexrad_level3::decode::mod`'s
//! own `build_minimal_n0b`/`build_minimal_rate176` test helpers
//! (`pub(crate)`, so not reachable across the crate boundary — hand-built
//! again here, the same duplication this backend's OTHER cross-crate test
//! files already accept, e.g. `test_nexrad_level3_xradar_oracle.rs` vs.
//! `test_nexrad_level3_xradar_vectors.rs` each defining their own
//! synthetic-body builders).

use wasm_bindgen_test::*;

use radish_wasm::decode_nexrad_level3;

// No `wasm_bindgen_test_configure!` call needed: Node (not a browser) is
// this crate's own `wasm-bindgen-test` default already — `run_in_browser`
// is what OPTS IN to browser testing, there is no `run_in_node` (it isn't
// needed, and doesn't exist as a macro arm).

/// A minimal, valid packet-16 product (message code 153, `N0B`
/// reflectivity) — `u8` raw codes, `LinearHw` scale form.
fn build_minimal_n0b() -> Vec<u8> {
    let mut raw = Vec::new();
    raw.extend_from_slice(b"N0BLOT\r\r\n");
    while raw.len() < 30 {
        raw.push(b' ');
    }
    let mhb = raw.len();
    raw.extend_from_slice(&153i16.to_be_bytes());
    raw.extend_from_slice(&[0u8; 16]);
    assert_eq!(raw.len(), mhb + 18);

    let pdb = raw.len();
    let mut pdb_bytes = vec![0u8; 102];
    pdb_bytes[0..2].copy_from_slice(&(-1i16).to_be_bytes());
    pdb_bytes[2..6].copy_from_slice(&41_881i32.to_be_bytes());
    pdb_bytes[6..10].copy_from_slice(&(-88_084i32).to_be_bytes());
    pdb_bytes[10..12].copy_from_slice(&650i16.to_be_bytes());
    pdb_bytes[12..14].copy_from_slice(&153i16.to_be_bytes());
    pdb_bytes[16..18].copy_from_slice(&215i16.to_be_bytes());
    pdb_bytes[22..24].copy_from_slice(&20000u16.to_be_bytes());
    pdb_bytes[24..28].copy_from_slice(&3600i32.to_be_bytes());
    pdb_bytes[40..42].copy_from_slice(&5i16.to_be_bytes()); // elevation x10
    pdb_bytes[42..44].copy_from_slice(&(-320i16).to_be_bytes()); // value_min x10
    pdb_bytes[44..46].copy_from_slice(&5i16.to_be_bytes()); // value_increment x10
    pdb_bytes[46..48].copy_from_slice(&254i16.to_be_bytes()); // n_levels
    raw.extend_from_slice(&pdb_bytes);
    assert_eq!(raw.len(), pdb + 102);

    let mut body = Vec::new();
    body.extend_from_slice(&(-1i16).to_be_bytes());
    body.extend_from_slice(&1i16.to_be_bytes());
    body.extend_from_slice(&[0u8; 6]);
    body.extend_from_slice(&[0u8; 6]);
    body.extend_from_slice(&16u16.to_be_bytes()); // packet_code
    body.extend_from_slice(&0u16.to_be_bytes()); // first_bin
    body.extend_from_slice(&4u16.to_be_bytes()); // n_bins
    body.extend_from_slice(&999i16.to_be_bytes());
    body.extend_from_slice(&998i16.to_be_bytes());
    body.extend_from_slice(&999u16.to_be_bytes());
    body.extend_from_slice(&1u16.to_be_bytes()); // n_radials
    body.extend_from_slice(&4u16.to_be_bytes()); // n_bytes for the one radial
    body.extend_from_slice(&0u16.to_be_bytes()); // start_angle
    body.extend_from_slice(&10u16.to_be_bytes()); // delta (1.0 deg)
    body.extend_from_slice(&[2, 3, 4, 5]); // codes
    raw.extend_from_slice(&body);

    raw
}

/// A minimal, valid packet-28/XDR product (message code 176, `DPR` rate)
/// — `u16` raw codes, `Rate` scale form. Field order/shape verified
/// byte-exact against a real `DPR` fixture in
/// `radish::backends::nexrad_level3::decode::mod`'s own copy of this
/// builder (`decode_rate_product_end_to_end`).
fn build_minimal_rate176() -> Vec<u8> {
    fn xdr_string(s: &str) -> Vec<u8> {
        let mut out = (s.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(s.as_bytes());
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out
    }
    fn xdr_empty_list() -> Vec<u8> {
        let mut out = 0i32.to_be_bytes().to_vec();
        out.extend(0i32.to_be_bytes());
        out
    }

    let mut raw = Vec::new();
    raw.extend_from_slice(b"DPRLOT\r\r\n");
    while raw.len() < 30 {
        raw.push(b' ');
    }
    let mhb = raw.len();
    raw.extend_from_slice(&176i16.to_be_bytes());
    raw.extend_from_slice(&[0u8; 16]);
    assert_eq!(raw.len(), mhb + 18);

    let pdb = raw.len();
    let mut pdb_bytes = vec![0u8; 102];
    pdb_bytes[0..2].copy_from_slice(&(-1i16).to_be_bytes());
    pdb_bytes[2..6].copy_from_slice(&41_881i32.to_be_bytes());
    pdb_bytes[6..10].copy_from_slice(&(-88_084i32).to_be_bytes());
    pdb_bytes[10..12].copy_from_slice(&650i16.to_be_bytes());
    pdb_bytes[12..14].copy_from_slice(&176i16.to_be_bytes());
    pdb_bytes[16..18].copy_from_slice(&215i16.to_be_bytes());
    pdb_bytes[22..24].copy_from_slice(&20000u16.to_be_bytes());
    pdb_bytes[24..28].copy_from_slice(&3600i32.to_be_bytes());
    pdb_bytes[42..46].copy_from_slice(&1000.0f32.to_be_bytes()); // scale, real DPR value
    pdb_bytes[46..50].copy_from_slice(&0.0f32.to_be_bytes()); // offset
    pdb_bytes[52..54].copy_from_slice(&65535u16.to_be_bytes()); // max_val
    pdb_bytes[54..56].copy_from_slice(&0i16.to_be_bytes()); // leading
    pdb_bytes[56..58].copy_from_slice(&0i16.to_be_bytes()); // trailing
    raw.extend_from_slice(&pdb_bytes);
    assert_eq!(raw.len(), pdb + 102);

    let mut xdr = Vec::new();
    for _ in 0..18 {
        xdr.extend(0i32.to_be_bytes()); // 18 throwaway product-desc fields
    }
    xdr.extend(xdr_empty_list()); // product-level parameters
    xdr.extend(1i32.to_be_bytes()); // components count
    xdr.extend(0i32.to_be_bytes()); // leading pointer
    xdr.extend(1i32.to_be_bytes()); // component type = radial
    xdr.extend(xdr_string("")); // description
    xdr.extend(250.0f32.to_be_bytes()); // gate_width
    xdr.extend(125.0f32.to_be_bytes()); // first_gate
    xdr.extend(xdr_empty_list()); // radial-level parameters
    xdr.extend(2i32.to_be_bytes()); // num_rads
    for (az, data) in [(0.0f32, [0i32, 100, 401, 0]), (1.0, [16, 0, 0, 0])] {
        xdr.extend(az.to_be_bytes());
        xdr.extend(0.0f32.to_be_bytes()); // elevation
        xdr.extend(1.0f32.to_be_bytes()); // width
        xdr.extend(4i32.to_be_bytes()); // num_bins
        xdr.extend(xdr_string("")); // attributes
        xdr.extend(4u32.to_be_bytes()); // data array count
        for v in data {
            xdr.extend(v.to_be_bytes());
        }
    }

    let mut body = Vec::new();
    body.extend_from_slice(&(-1i16).to_be_bytes());
    body.extend_from_slice(&1i16.to_be_bytes());
    body.extend_from_slice(&[0u8; 6]);
    body.extend_from_slice(&[0u8; 6]);
    body.extend_from_slice(&28i16.to_be_bytes()); // GENERIC_PACKET_CODE
    body.extend_from_slice(&0i16.to_be_bytes()); // reserved
    body.extend_from_slice(&(xdr.len() as i32).to_be_bytes()); // num_bytes
    body.extend_from_slice(&xdr);
    raw.extend_from_slice(&body);

    raw
}

#[wasm_bindgen_test]
fn u8_product_exposes_codes_not_codes_u16() {
    let product = decode_nexrad_level3(&build_minimal_n0b()).expect("decode should succeed");

    assert_eq!(product.codes_width(), 8);
    assert_eq!(product.n_radials(), 1);
    assert_eq!(product.n_bins(), 4);

    let codes = product.codes();
    assert_eq!(codes.to_vec(), vec![2u8, 3, 4, 5]);

    // The "wrong width" accessor must be empty, not throw or return
    // garbage — the documented contract for both `codes()` and
    // `codesU16()` when the product isn't that width.
    let codes_u16 = product.codes_u16();
    assert_eq!(codes_u16.length(), 0);

    assert_eq!(product.value_min(), Some(-32.0));
    assert_eq!(product.value_increment(), Some(0.5));
}

#[wasm_bindgen_test]
fn u16_product_exposes_codes_u16_not_codes() {
    let product = decode_nexrad_level3(&build_minimal_rate176()).expect("decode should succeed");

    assert_eq!(product.codes_width(), 16);
    assert_eq!(product.n_radials(), 2);
    assert_eq!(product.n_bins(), 4);

    let codes_u16 = product.codes_u16();
    assert_eq!(codes_u16.to_vec(), vec![0u16, 100, 401, 0, 16, 0, 0, 0]);

    // The "wrong width" accessor must be empty here too.
    let codes = product.codes();
    assert_eq!(codes.length(), 0);

    // Centred, not the stored leading edge (radials built with
    // az=0.0/1.0, width=1.0 — see docs/NEXRAD_LEVEL3_WASM.md §4.5/§11 for
    // why packet 28 needs this exact correction).
    let az = product.azimuths();
    assert_eq!(az.to_vec(), vec![0.5, 1.5]);

    assert_eq!(product.gate_spacing_m(), 250.0);
    assert_eq!(product.first_gate_m(), 125.0);
}

#[wasm_bindgen_test]
fn decode_rejects_garbage_bytes_without_panicking() {
    // `DecodedProduct` isn't `Debug` (a wasm-bindgen struct, not meant for
    // Rust-side formatting) — match instead of `.expect_err()`.
    let err = match decode_nexrad_level3(b"not a nids product") {
        Err(e) => e,
        Ok(_) => panic!("garbage bytes must be rejected, not decoded"),
    };
    // Not just "some .name exists" — the SPECIFIC value a JS caller would
    // actually branch on (radish_err's own doc comment: "Decode = the
    // input bytes are bad"). "not a nids product" has no recognisable
    // message header at all, so this is `Level3DecodeError::NoMessageHeader`
    // -> `RadishError::Decode` -> `.name = "Decode"`.
    let name = js_sys::Reflect::get(&err, &"name".into())
        .ok()
        .and_then(|v| v.as_string());
    assert_eq!(name.as_deref(), Some("Decode"));
}
