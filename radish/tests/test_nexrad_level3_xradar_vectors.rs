//! NEXRAD Level 3 (NIDS) decode parity — Tier 2: value parity against
//! [openradar/xradar PR #392][xr392]'s own synthetic, byte-built test
//! vectors (`tests/io/test_nexrad_level3.py`, fetched from the PR branch
//! this session — not vendored, see the URL below).
//!
//! Tracked/advisory, not CI-blocking the way `test_nexrad_level3_parity.rs`
//! (Tier 1, byte-exact against an independent decode oracle on real
//! fixtures) is — see `docs/NEXRAD_LEVEL3_WASM.md`. These tests still run
//! on every `cargo test`, unconditionally (pure synthetic bytes, no
//! fixtures needed); "advisory" describes the strength of the evidence a
//! pass/fail here carries, not whether the test executes.
//!
//! **Scope**: every scheme this backend implements — `LinearHw`/
//! `FloatScale`/`Legacy16`/`ClassInt` (packet 16 + AF1F) plus, since plan
//! 0012 closed the last 7 deferred codes, `Precip` (packet 16) and `Rate`
//! (packet 28/XDR) — see `decode::products::PRODUCTS`. There is no longer
//! a deferred/unimplemented code in the product table to cross-check a
//! clean rejection for (an EARLIER version of this file did exactly that,
//! for 170/176/177 — see `deferred_message_codes_still_reject_cleanly`
//! below for what replaced it once those codes started succeeding
//! instead).
//!
//! **One deliberate NON-port, documented rather than silently skipped**:
//! xradar's `_radial_packet16`/`_radial_packet_af1f` test helpers assign
//! `azimuth = start_angle / 10.0` (no half-beamwidth offset) — e.g.
//! `test_metadata`'s `assert_allclose(f.get_azimuth(), [0.0, 90.0, 180.0,
//! 270.0])` with `delta=5` (0.5 deg) would need `azimuth = (start_angle +
//! delta/2) / 10.0` to produce 0.25 instead of 0.0 for the first ray if it
//! centered. radish's azimuth (`(start_angle + delta/2.0) / 10.0`, ray
//! **center**) is verified byte-exact against 7 REAL fixtures and the real
//! Python oracle in `test_nexrad_level3_parity.rs` — stronger evidence
//! than a synthetic test helper's convention. Porting xradar's azimuth
//! numbers here would mean asserting radish is wrong where Tier 1 already
//! proves it's right, so this file only ports scale/value-conversion
//! vectors, which don't depend on the azimuth convention at all.
//!
//! [xr392]: https://github.com/openradar/xradar/pull/392

use radish::backends::{NexradLevel3Backend, RadarBackend};

const KT_TO_MS: f32 = 0.514_444;
const IN_TO_MM: f32 = 25.4;

/// Assembles a complete, minimal, valid NIDS product byte-for-byte —
/// text header, MHB, PDB, and one symbology layer wrapping the caller's
/// packet body. Mirrors `decode::mod::build_minimal_n0b`'s byte layout
/// exactly (that helper is `pub(crate)`, invisible to this integration
/// test, so this is a deliberate, verified-identical duplicate) and
/// xradar's own `build_level3_file` header/PDB layout independently.
fn build_product(message_code: i16, threshold: [u8; 32], packet_body: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    raw.extend_from_slice(b"N0ZLOT\r\r\n");
    while raw.len() < 30 {
        raw.push(b' ');
    }
    let mhb = raw.len();
    raw.extend_from_slice(&message_code.to_be_bytes());
    raw.extend_from_slice(&[0u8; 16]);
    assert_eq!(raw.len(), mhb + 18);

    let pdb = raw.len();
    let mut pdb_bytes = vec![0u8; 102];
    pdb_bytes[0..2].copy_from_slice(&(-1i16).to_be_bytes()); // divider
    pdb_bytes[2..6].copy_from_slice(&41_881i32.to_be_bytes()); // lat x1000
    pdb_bytes[6..10].copy_from_slice(&(-88_084i32).to_be_bytes()); // lon x1000
    pdb_bytes[10..12].copy_from_slice(&650i16.to_be_bytes()); // height, ft
    pdb_bytes[12..14].copy_from_slice(&message_code.to_be_bytes()); // product code
    pdb_bytes[16..18].copy_from_slice(&215i16.to_be_bytes()); // vcp
    pdb_bytes[22..24].copy_from_slice(&20000u16.to_be_bytes()); // scan days
    pdb_bytes[24..28].copy_from_slice(&3600i32.to_be_bytes()); // scan seconds
    pdb_bytes[40..42].copy_from_slice(&5i16.to_be_bytes()); // elevation x10 = 0.5deg
    pdb_bytes[42..74].copy_from_slice(&threshold); // halfwords 22-37
    raw.extend_from_slice(&pdb_bytes);
    assert_eq!(raw.len(), pdb + 102);

    // Symbology block: divider(-1) + block id(1) + 12 unchecked bytes
    // (block length / layer count / layer divider / layer length —
    // `decode_symbology` only validates the first two halfwords, see
    // `symbology.rs`'s `base = 16` comment), then the caller's packet.
    raw.extend_from_slice(&(-1i16).to_be_bytes());
    raw.extend_from_slice(&1i16.to_be_bytes());
    raw.extend_from_slice(&[0u8; 6]);
    raw.extend_from_slice(&[0u8; 6]);
    raw.extend_from_slice(packet_body);
    raw
}

/// One packet-16 (digital radial) body: header + `rows.len()` radials,
/// each `rows[i].len()` bins wide, ray center `(i + 0.5) * 360 / n_radials`
/// (irrelevant to the value-parity assertions in this file — only the
/// codes matter here, not azimuth; see the module doc's non-port note).
fn packet16(rows: &[&[u8]]) -> Vec<u8> {
    let n_radials = rows.len() as u16;
    let n_bins = rows[0].len() as u16;
    let mut body = Vec::new();
    body.extend_from_slice(&16u16.to_be_bytes()); // packet_code
    body.extend_from_slice(&0u16.to_be_bytes()); // first_bin (must be 0)
    body.extend_from_slice(&n_bins.to_be_bytes());
    body.extend_from_slice(&999i16.to_be_bytes()); // i_center (unread)
    body.extend_from_slice(&998i16.to_be_bytes()); // j_center (unread)
    body.extend_from_slice(&999u16.to_be_bytes()); // range_scale_raw (unread for elevation products)
    body.extend_from_slice(&n_radials.to_be_bytes());
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.len(), n_bins as usize, "ragged rows not supported here");
        body.extend_from_slice(&(row.len() as u16).to_be_bytes()); // n_bytes
        let start_angle = (i as u32 * 3600 / n_radials as u32) as u16;
        body.extend_from_slice(&start_angle.to_be_bytes());
        body.extend_from_slice(&10u16.to_be_bytes()); // delta = 1.0 deg
        body.extend_from_slice(row);
    }
    body
}

/// One AF1F (RLE) body: header + `rle_radials.len()` radials, each a
/// pre-built `(run<<4 | color)` nibble-pair byte string, halfword-padded.
fn packet_af1f(n_bins: u16, rle_radials: &[&[u8]]) -> Vec<u8> {
    let n_radials = rle_radials.len() as u16;
    let mut body = Vec::new();
    body.extend_from_slice(&(-20705i16).to_be_bytes()); // AF1F packet code
    body.extend_from_slice(&0u16.to_be_bytes()); // first_bin (unchecked for AF1F)
    body.extend_from_slice(&n_bins.to_be_bytes());
    body.extend_from_slice(&999i16.to_be_bytes());
    body.extend_from_slice(&998i16.to_be_bytes());
    body.extend_from_slice(&999u16.to_be_bytes());
    body.extend_from_slice(&n_radials.to_be_bytes());
    for (i, rle) in rle_radials.iter().enumerate() {
        assert_eq!(rle.len() % 2, 0, "RLE byte strings must be halfword-padded");
        body.extend_from_slice(&((rle.len() / 2) as u16).to_be_bytes()); // n_bytes, in HALFWORDS
        let start_angle = (i as u32 * 3600 / n_radials as u32) as u16;
        body.extend_from_slice(&start_angle.to_be_bytes());
        body.extend_from_slice(&10u16.to_be_bytes());
        body.extend_from_slice(rle);
    }
    body
}

/// `threshold_data` for `LinearHw` (halfwords 22/23/24): min x10,
/// increment x10, n_levels. Rest of the 32 bytes unread by this scheme.
fn linear_hw_threshold(value_min: f32, value_increment: f32, n_levels: i16) -> [u8; 32] {
    let mut t = [0u8; 32];
    t[0..2].copy_from_slice(&((value_min * 10.0).round() as i16).to_be_bytes());
    t[2..4].copy_from_slice(&((value_increment * 10.0).round() as i16).to_be_bytes());
    t[4..6].copy_from_slice(&n_levels.to_be_bytes());
    t
}

/// `threshold_data` for `FloatScale` (halfwords 22-25): scale, offset as
/// big-endian `f32`. Rest unread.
fn float_scale_threshold(scale: f32, offset: f32) -> [u8; 32] {
    let mut t = [0u8; 32];
    t[0..4].copy_from_slice(&scale.to_be_bytes());
    t[4..8].copy_from_slice(&offset.to_be_bytes());
    t
}

/// `threshold_data` for `Legacy16`: 16 `(flag, value)` byte pairs.
fn legacy16_threshold(pairs: &[(u8, u8)]) -> [u8; 32] {
    assert_eq!(pairs.len(), 16);
    let mut t = [0u8; 32];
    for (i, &(flag, value)) in pairs.iter().enumerate() {
        t[2 * i] = flag;
        t[2 * i + 1] = value;
    }
    t
}

fn decode(raw: Vec<u8>) -> radish::VolumeData {
    NexradLevel3Backend::new()
        .read_bytes_volume(raw)
        .expect("synthetic product should decode")
}

// --- LinearHw --------------------------------------------------------

/// ~ xradar's `TestParser.test_linear_hw_decode`: raw codes
/// `[0, 1, 2, 100, 255]`, `value_min=-32.0, increment=0.5` ->
/// `physical = (raw - 2) * 0.5 - 32.0` for raw >= 2, `NaN` below.
/// Also closes the "before Phase 4 Tier 2 claims full parity" note in
/// the plan's Phase 3 exit notes on the missing `range_folded` mask:
/// codes 0 (below-threshold) and 1 (range-folded) are NOT
/// distinguishable in `moment.data` (both `NaN`, matching the original
/// 6 products' existing behavior) but ARE distinguishable in
/// `moment.raw_codes` — no information is actually lost, just
/// represented differently (verbatim code) than xradar's separate
/// boolean mask.
#[test]
fn linear_hw_value_parity_and_raw_codes_preserve_the_range_folded_distinction() {
    let threshold = linear_hw_threshold(-32.0, 0.5, 254);
    let raw = build_product(153, threshold, &packet16(&[&[0, 1, 2, 100, 255]]));
    let volume = decode(raw);
    let moment = volume.sweeps[0].moments.get("DBZH").unwrap();

    let codes = moment.raw_codes.as_ref().unwrap();
    assert_eq!(codes.row(0).to_vec(), vec![0, 1, 2, 100, 255]);

    let data = moment.data.row(0).to_vec();
    assert!(data[0].is_nan(), "code 0 (below threshold) -> NaN");
    assert!(data[1].is_nan(), "code 1 (range folded) -> NaN");
    assert_eq!(data[2], -32.0);
    assert_eq!(data[3], 17.0); // (100 - 2) * 0.5 - 32.0
    assert_eq!(data[4], 94.5); // (255 - 2) * 0.5 - 32.0
}

// --- FloatScale --------------------------------------------------------

/// ~ xradar's `TestParser.test_float_scale_decode`: ZDR (msg 159),
/// scale=16.0, offset=128.0 -> `physical = (raw - offset) / scale`.
#[test]
fn float_scale_value_parity_matches_xradar_formula() {
    let threshold = float_scale_threshold(16.0, 128.0);
    let raw = build_product(159, threshold, &packet16(&[&[0, 1, 130, 200]]));
    let volume = decode(raw);
    let moment = volume.sweeps[0].moments.get("ZDR").unwrap();

    let data = moment.data.row(0).to_vec();
    assert!(data[0].is_nan());
    assert!(data[1].is_nan());
    assert!((data[2] - 0.125).abs() < 1e-6); // (130 - 128.0) / 16.0
    assert!((data[3] - 4.5).abs() < 1e-6); // (200 - 128.0) / 16.0
}

// --- Precip (closed by plan 0012) -----------------------------------------

/// `threshold_data` for `Precip`/`Rate`: the same 8-byte float32
/// scale/offset pair as `FloatScale` (halfwords 22-25), plus the
/// product-family flag-count field `pdb::precip_family_scale` reads
/// further into the PDB (halfwords 27-29: max_val `u16`, leading/trailing
/// `i16` each) — see that function's doc for the full derivation.
fn precip_threshold(
    scale: f32,
    offset: f32,
    max_val: u16,
    leading: i16,
    trailing: i16,
) -> [u8; 32] {
    let mut t = [0u8; 32];
    t[0..4].copy_from_slice(&scale.to_be_bytes());
    t[4..8].copy_from_slice(&offset.to_be_bytes());
    t[10..12].copy_from_slice(&max_val.to_be_bytes());
    t[12..14].copy_from_slice(&leading.to_be_bytes());
    t[14..16].copy_from_slice(&trailing.to_be_bytes());
    t
}

/// Tier 2 value-parity for `Precip` (code 170, `DAA`) — the formula plan
/// 0012 §2.1 derived from xradar #392's `get_scale_offset`/
/// `get_flag_counts`: `physical = code * (factor/scale) +
/// (-offset*factor/scale)`, masked below `leading` and above
/// `max_val - trailing` when the flag-count field is plausible. Real
/// values (`scale=1.2555609941482544, offset=0.8744438886642456,
/// leading=1`) read off a live `LOT_DAA` fixture (plan 0012 §2.4 step 2).
#[test]
fn precip_value_parity_matches_xradar_formula() {
    let threshold = precip_threshold(1.255_561, 0.874_444, 255, 1, 0);
    let raw = build_product(170, threshold, &packet16(&[&[0, 1, 2, 255]]));
    let volume = decode(raw);
    let moment = volume.sweeps[0].moments.get("ACCUM").unwrap();

    let data = moment.data.row(0).to_vec();
    const PRECIP_FACTOR: f32 = 0.01 * 25.4; // IN_TO_MM
    let out_scale = PRECIP_FACTOR / 1.255_561;
    let out_offset = -0.874_444 * PRECIP_FACTOR / 1.255_561;
    assert!(data[0].is_nan(), "code 0 is below the floor (leading=1)");
    assert!((data[1] - (1.0 * out_scale + out_offset)).abs() < 1e-5);
    assert!((data[2] - (2.0 * out_scale + out_offset)).abs() < 1e-5);
    assert!((data[3] - (255.0 * out_scale + out_offset)).abs() < 1e-4);

    let scale = moment.declared_scale.unwrap();
    assert_eq!(
        scale.data_floor_code, 1,
        "floor must be read from the PDB (1), not DATA_FLOOR_CODE's universal 2"
    );
}

// --- Legacy16 (AF1F) -----------------------------------------------------

/// ~ xradar's `TestParser.test_legacy16_decode_per_flag_scale`: legacy
/// reflectivity (msg 19), per-level flag byte selects sign/scale —
/// `0x10` -> /10, `0x01` -> sign, `0x20` -> /20, `0x80` -> no data.
#[test]
fn legacy16_flag_scale_value_parity_matches_xradar() {
    let mut pairs = vec![(0x80u8, 0u8), (0x10, 5), (0x01, 10), (0x20, 30)];
    pairs.extend(std::iter::repeat_n((0x80u8, 0u8), 12));
    let threshold = legacy16_threshold(&pairs);
    // RLE: 1x level0, 2x level1, 1x level2, 1x level3 -> 5 bins.
    let rle: &[u8] = &[0x10, 0x21, 0x12, 0x13];
    let raw = build_product(19, threshold, &packet_af1f(5, &[rle]));
    let volume = decode(raw);
    let moment = volume.sweeps[0].moments.get("DBZH").unwrap();

    let data = moment.data.row(0).to_vec();
    assert!(data[0].is_nan());
    assert!((data[1] - 0.5).abs() < 1e-6);
    assert!((data[2] - 0.5).abs() < 1e-6);
    assert!((data[3] - (-10.0)).abs() < 1e-6);
    assert!((data[4] - 1.5).abs() < 1e-6);
}

/// ~ xradar's `TestParser.test_legacy16_velocity_knots_to_ms`: legacy
/// velocity (msg 27) applies `KT_TO_MS` post-scale to the flag-decoded
/// value — matches `products::PRODUCTS`'s `post_scale: KT_TO_MS` for
/// code 27 exactly (same constant, 0.514444).
#[test]
fn legacy16_velocity_knots_to_ms_value_parity() {
    let mut pairs = vec![(0x80u8, 0u8), (0x01, 10), (0x00, 10)];
    pairs.extend(std::iter::repeat_n((0x80u8, 0u8), 13));
    let threshold = legacy16_threshold(&pairs);
    let rle: &[u8] = &[0x11, 0x22]; // level1, then 2x level2
    let raw = build_product(27, threshold, &packet_af1f(3, &[rle]));
    let volume = decode(raw);
    let moment = volume.sweeps[0].moments.get("VRADH").unwrap();

    let data = moment.data.row(0).to_vec();
    assert!((data[0] - (-10.0 * KT_TO_MS)).abs() < 1e-4);
    assert!((data[1] - (10.0 * KT_TO_MS)).abs() < 1e-4);
    assert!((data[2] - (10.0 * KT_TO_MS)).abs() < 1e-4);
}

/// ~ xradar's `TestParser.test_legacy16_hundredths_scale_flag`: legacy
/// accumulation (msg 78), `0x40` flag scales by 1/100, `post_scale =
/// 25.4` (in -> mm) applied after.
#[test]
fn legacy16_hundredths_scale_flag_value_parity() {
    let mut pairs = vec![(0x80u8, 0u8), (0x40, 25)];
    pairs.extend(std::iter::repeat_n((0x80u8, 0u8), 14));
    let threshold = legacy16_threshold(&pairs);
    let rle: &[u8] = &[0x11, 0x21]; // 3x level1
    let raw = build_product(78, threshold, &packet_af1f(3, &[rle]));
    let volume = decode(raw);
    let moment = volume.sweeps[0].moments.get("ACCUM").unwrap();

    let data = moment.data.row(0).to_vec();
    // 0.25 inches -> mm.
    assert!((data[1] - (0.25 * IN_TO_MM)).abs() < 1e-3);
}

/// ~ xradar's `TestParser.test_af1f_expansion_halfword_sizing`: a
/// 3-byte run list padded to a full halfword (4 bytes) must not
/// misalign the expansion — pure RLE mechanics, independent of any
/// scale/threshold values (threshold is irrelevant here; only
/// `raw_codes` is checked, not `data`).
#[test]
fn af1f_expansion_halfword_sizing_matches_xradar() {
    let threshold = legacy16_threshold(&(0..16u8).map(|v| (0x00u8, v)).collect::<Vec<_>>());
    // run=3/color=1, run=2/color=2, run=1/color=0, then a zero-run pad
    // byte (0x00) completing the halfword — contributes nothing.
    let rle: &[u8] = &[0x31, 0x22, 0x10, 0x00];
    let raw = build_product(19, threshold, &packet_af1f(6, &[rle]));
    let volume = decode(raw);
    let moment = volume.sweeps[0].moments.get("DBZH").unwrap();

    let codes = moment.raw_codes.as_ref().unwrap();
    assert_eq!(codes.row(0).to_vec(), vec![1, 1, 1, 2, 2, 0]);
}

// --- ClassInt ------------------------------------------------------------

/// ~ xradar's `TestParser.test_class_int_decode`: hydrometeor
/// classification (msg 165) — the code IS the category index; code 0
/// means no data.
#[test]
fn class_int_value_parity_matches_xradar() {
    let raw = build_product(165, [0u8; 32], &packet16(&[&[0, 10, 60, 150]]));
    let volume = decode(raw);
    let moment = volume.sweeps[0].moments.get("HCLASS").unwrap();
    assert!(moment.declared_scale.is_none());

    let data = moment.data.row(0).to_vec();
    assert!(data[0].is_nan());
    assert_eq!(data[1], 10.0);
    assert_eq!(data[2], 60.0);
    assert_eq!(data[3], 150.0);
}

// --- Unrecognised message codes, through the public API -------------------

/// ~ xradar's `TestParser.test_unknown_product_raises`, but through
/// `NexradLevel3Backend`'s PUBLIC API (`read_bytes_volume` ->
/// `decode_and_convert` -> `adapter::convert`), not `decode::decode`
/// directly the way `decode/mod.rs`'s internal
/// `decode_rejects_an_unrecognised_message_code_end_to_end` test does — a
/// different code path, worth its own coverage since it's what the `wasm`
/// crate actually calls.
///
/// FORMERLY covered 170/176/177 rejecting as "known but not implemented"
/// (`deferred_packet28_and_precip_codes_reject_through_the_public_backend_api`)
/// — plan 0012 closed all 7 previously-deferred codes, so `PRODUCTS` no
/// longer has any code in that state; this test was rewritten rather than
/// left asserting a rejection that no longer happens (this file's own
/// module doc: a stale "this rejects" test testing dead code is worse
/// than no test). The one rejection this stage can still produce is for a
/// message code `PRODUCTS` doesn't recognise at all.
#[test]
fn unrecognised_message_code_rejects_through_the_public_backend_api() {
    let code = 9999i16;
    let mut raw = b"N0ZLOT\r\r\n".to_vec();
    while raw.len() < 30 {
        raw.push(b' ');
    }
    raw.extend_from_slice(&code.to_be_bytes());
    raw.extend_from_slice(&[0u8; 16]);
    raw.extend_from_slice(&(-1i16).to_be_bytes()); // PDB divider
    raw.extend_from_slice(&[0u8; 8]); // trailing slack for the scan window

    let err = NexradLevel3Backend::new()
        .read_bytes_volume(raw)
        .expect_err("an unrecognised message code should be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("no digital radial message header found"),
        "error should be NoMessageHeader: {msg}"
    );
}
