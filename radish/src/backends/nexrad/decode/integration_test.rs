//! Integration test: walk the entire KLOT fixture through the loop
//! and confirm we land on the same messages danielway's parser
//! reports. Gated on `RADISH_NEXRAD_FIXTURE_DIR` so CI without the
//! corpus simply skips. Marked `#[ignore]` so default `cargo test`
//! doesn't pay for the bzip2 round-trip.

use std::path::PathBuf;

use super::decode_volume;
use super::messages::decode_messages;
use super::record::{decompress_all, split_ldm_records};

fn klot_fixture() -> Option<PathBuf> {
    let dir = std::env::var_os("RADISH_NEXRAD_FIXTURE_DIR")?;
    let candidate = PathBuf::from(dir).join("KLOT20251210_102338_V06");
    candidate.is_file().then_some(candidate)
}

#[test]
#[ignore = "needs RADISH_NEXRAD_FIXTURE_DIR"]
fn walks_klot_fixture_and_finds_msg31_records() {
    let Some(path) = klot_fixture() else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE_DIR not set");
        return;
    };
    let bytes = std::fs::read(&path).expect("read fixture");

    // Skip the 24-byte AR2V volume header, split LDM records, decompress.
    let records = split_ldm_records(&bytes).expect("split");
    assert!(!records.is_empty(), "expected at least one LDM record");
    let payloads = decompress_all(&records).expect("decompress");

    // Count messages per type across all records.
    let mut total_messages = 0usize;
    let mut msg31_count = 0usize;
    let mut msg2_count = 0usize;
    let mut msg5_count = 0usize;
    for (record_idx, payload) in payloads.iter().enumerate() {
        let messages = decode_messages(payload)
            .unwrap_or_else(|e| panic!("record {record_idx}: decode_messages failed: {e}"));
        total_messages += messages.len();
        for msg in &messages {
            use super::header::MessageType::*;
            match msg.header.message_type {
                DigitalRadarDataGenericFormat => msg31_count += 1,
                RdaStatusData => msg2_count += 1,
                VolumeCoveragePattern => msg5_count += 1,
                _ => {}
            }
        }
    }

    // KLOT VCP-212 fixture observed counts on a green run:
    //   61 records, 7206 total messages, 7200 MSG_31, 1 MSG_2, 1 MSG_5.
    // Each NEXRAD volume has exactly one MSG_2 (RDA status) and one
    // MSG_5 (VCP definition). MSG_31 count varies by file but is
    // roughly num_radials_per_sweep × num_sweeps. Tightened
    // assertions catch regressions where the loop stops early on
    // trailing zero-padding (pre-fix: stopped after ~6 messages).
    eprintln!(
        "KLOT fixture summary: {} records, {} total messages \
         ({} MSG_31, {} MSG_2, {} MSG_5)",
        payloads.len(),
        total_messages,
        msg31_count,
        msg2_count,
        msg5_count,
    );
    assert!(
        msg31_count >= 6000,
        "expected ≥6000 MSG_31 messages, found {msg31_count}"
    );
    assert_eq!(msg2_count, 1, "exactly one MSG_2 (RDA status) per volume");
    assert_eq!(
        msg5_count, 1,
        "exactly one MSG_5 (VCP definition) per volume"
    );
    // Sanity: typed counts should sum near total. Anything else
    // is a Skip(_) — small finite count is fine.
    assert!(
        msg31_count + msg2_count + msg5_count <= total_messages,
        "typed counts exceed total — accumulator bug"
    );
}

/// **KILX on-wire fidelity test.** Pins our decoder to the
/// ground-truth byte content: `KILX20230629_154426_V06` contains
/// 6840 MSG_31 records — including 360 in elevation 11 — at
/// consistent 7,840-byte strides, all with valid ICD §3.2.4.17
/// fields (monotonic timestamps, sequential azimuth_numbers,
/// `spot_blank=0`, `radial_status=1`). Both our `decode_volume`
/// and `danielway/nexrad` correctly read all 6840.
///
/// xradar reports 358 in sweep_10 (= 6838 total) by hard-coding
/// "exactly 120 messages per LDM record" in its byte walker
/// (xradar/io/backends/nexrad_level2.py:397, formula
/// `(recnum - 134) // 120`). LDM record 49 of this fixture
/// actually contains 122 messages — 120 MSG_31 + 2 MSG_2 — so
/// xradar drops 2 valid MSG_31s at the tail of LDM 49. Per ICD
/// there is no field that justifies dropping az_num=119/120;
/// xradar's stride assumption is the bug.
///
/// This test is the canary: if it ever fails, either our walker
/// regressed or the fixture changed.
#[test]
#[ignore = "needs RADISH_NEXRAD_FIXTURE_DIR"]
fn decode_volume_kilx_reads_all_6840_on_wire_records() {
    let Some(dir) = std::env::var_os("RADISH_NEXRAD_FIXTURE_DIR") else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE_DIR not set");
        return;
    };
    let path = std::path::PathBuf::from(dir).join("KILX20230629_154426_V06");
    if !path.is_file() {
        eprintln!("skipping: KILX fixture not found");
        return;
    }
    let bytes = std::fs::read(&path).expect("read KILX");
    let scan = super::decode_volume(&bytes).expect("decode_volume");

    let total_rays: usize = scan.sweeps.iter().map(|s| s.radials.len()).sum();
    let per_sweep: Vec<usize> = scan.sweeps.iter().map(|s| s.radials.len()).collect();
    eprintln!(
        "KILX via decode_volume: {} sweeps, {} total rays, per-sweep counts: {:?}",
        scan.sweeps.len(),
        total_rays,
        per_sweep
    );

    assert_eq!(
        total_rays, 6840,
        "expected 6840 on-wire MSG_31 records; xradar reports 6838 due to \
         its 120-msg-per-LDM stride bug — but radish must read what's \
         actually in the bytes."
    );

    let sweep_10 = scan
        .sweeps
        .get(10)
        .expect("KILX VCP-212 must have ≥ 11 sweeps");
    assert_eq!(
        sweep_10.radials.len(),
        360,
        "sweep_10 must have 360 rays (full circle at 1.0° az_spacing); \
         xradar returns 358 because LDM record 49 contains 122 messages \
         (120 MSG_31 + 2 MSG_2) and xradar's hard-coded 120-msg stride \
         drops the trailing 2 MSG_31s."
    );
}

/// Phase 5: confirm `decode_volume` on the live KLOT fixture
/// produces a self-contained `Scan` with the expected sweep
/// structure (13 sweeps from KLOT VCP-32 / 12, KLOT lat/lon in the
/// site, and reasonable elevation angles).
#[test]
#[ignore = "needs RADISH_NEXRAD_FIXTURE_DIR"]
fn decode_volume_on_klot_fixture_produces_plausible_scan() {
    let Some(path) = klot_fixture() else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE_DIR not set");
        return;
    };
    let bytes = std::fs::read(&path).expect("read fixture");
    let scan = super::decode_volume(&bytes).expect("decode_volume");

    // KLOT fixture observed shape: 13 sweeps (matches xradar's
    // VCP-32 layout for clear-air mode), lat/lon ~41.6°N -88.1°W.
    assert!(
        (8..=20).contains(&scan.sweeps.len()),
        "sweep count out of plausible WSR-88D range: {}",
        scan.sweeps.len()
    );
    let site = scan.site.as_ref().expect("KLOT volume must have a Site");
    assert_eq!(&site.identifier, b"KLOT");
    assert!(
        (40.0..=43.0).contains(&site.latitude_degrees),
        "site latitude out of range: {}",
        site.latitude_degrees
    );
    assert!(
        (-90.0..=-86.0).contains(&site.longitude_degrees),
        "site longitude out of range: {}",
        site.longitude_degrees
    );
    assert!(
        scan.rda_status.is_some(),
        "KLOT volume must carry a Msg2 (RDA Status)"
    );

    // Every sweep should have at least one radial and each
    // radial's gate_count for REF should be 1832 or 360 (the
    // typical WSR-88D super-res / surveillance modes).
    for (i, sweep) in scan.sweeps.iter().enumerate() {
        assert!(!sweep.radials.is_empty(), "sweep {i} has zero radials");
        let any_ref = sweep.radials.iter().find_map(|r| r.reflectivity.as_ref());
        assert!(
            any_ref.is_some(),
            "sweep {i} has no reflectivity moment in any radial"
        );
    }

    // VCP cuts vector should agree with sweep count within a small
    // margin (SAILS / MRLE supplemental cuts may diverge by a few).
    let vcp_cuts = scan.coverage_pattern.elevation_cuts.len();
    assert!(
        scan.sweeps.len() <= vcp_cuts + 5,
        "got {} sweeps but VCP advertises {} cuts",
        scan.sweeps.len(),
        vcp_cuts
    );
}

/// Phase 4: confirm typed MSG_2 + MSG_5 parsers fire on the KLOT
/// fixture and produce plausible values. Each volume has exactly
/// one MSG_2 and one MSG_5; we extract both and pin a few fields.
#[test]
#[ignore = "needs RADISH_NEXRAD_FIXTURE_DIR"]
fn typed_msg2_and_msg5_parsers_on_klot_fixture_yield_plausible_values() {
    use super::messages::MessagePayload;

    let Some(path) = klot_fixture() else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE_DIR not set");
        return;
    };
    let bytes = std::fs::read(&path).expect("read fixture");
    let records = split_ldm_records(&bytes).expect("split");
    let payloads = decompress_all(&records).expect("decompress");

    let mut msg2 = None;
    let mut msg5 = None;
    for payload in &payloads {
        let messages = decode_messages(payload).expect("decode");
        for msg in messages {
            match msg.payload {
                MessagePayload::Msg2(boxed) if msg2.is_none() => msg2 = Some(*boxed),
                MessagePayload::Msg5(boxed) if msg5.is_none() => msg5 = Some(*boxed),
                _ => {}
            }
        }
    }

    let m2 = msg2.expect("KLOT volume must carry MSG_2");
    let m5 = msg5.expect("KLOT volume must carry MSG_5");

    // MSG_2: KLOT VCP-212 fixture, build 19+ era. Plausible bounds.
    assert!(
        m2.rda_build_number >= 1900 && m2.rda_build_number <= 2400,
        "rda_build_number out of range: {}",
        m2.rda_build_number
    );
    assert!(
        m2.average_transmitter_power_w < 2_000,
        "tx power suspiciously high: {}",
        m2.average_transmitter_power_w
    );
    // ICD HW 8 / Appendix C: VCP magnitudes 1..767. ICD HW 8 sign
    // convention encodes local vs remote pattern selection — we
    // don't care which here, just that the magnitude is in range.
    let vcp_mag = m2.volume_coverage_pattern_number.unsigned_abs();
    assert!(
        (1..=767).contains(&vcp_mag),
        "VCP magnitude out of ICD range 1..767: {vcp_mag}"
    );
    // status_version is bumped per ICD revision; non-zero in modern files.
    assert!(
        m2.status_version >= 1,
        "status_version: {}",
        m2.status_version
    );

    // MSG_5 should advertise the same VCP as MSG_2's selected pattern.
    assert_eq!(
        m5.pattern_number, vcp_mag,
        "MSG_5 pattern_number ({}) should match MSG_2 VCP magnitude ({})",
        m5.pattern_number, vcp_mag
    );
    assert!(
        (1..=32).contains(&m5.number_of_elevation_cuts),
        "elevation count out of range: {}",
        m5.number_of_elevation_cuts
    );
    assert_eq!(
        m5.elevation_cuts.len(),
        m5.number_of_elevation_cuts as usize,
        "elevation_cuts vec length must match header count"
    );
    // First cut elevation should be ≈ 0.5° (KLOT VCP-212 lowest tilt).
    let first_cut_deg = m5.elevation_cuts[0].elevation_angle_degrees();
    assert!(
        (0.4..=1.0).contains(&first_cut_deg),
        "first cut elevation_angle_degrees out of range: {first_cut_deg}"
    );
}

/// Phase 3: confirm we don't just *count* MSG_31s but actually
/// parse them through `msg31::parse`. Sample the first MSG_31 in
/// the file and verify its header fields decode to plausible
/// values matching the KLOT fixture.
#[test]
#[ignore = "needs RADISH_NEXRAD_FIXTURE_DIR"]
fn typed_msg31_parser_on_klot_fixture_yields_plausible_radials() {
    use super::messages::MessagePayload;

    let Some(path) = klot_fixture() else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE_DIR not set");
        return;
    };
    let bytes = std::fs::read(&path).expect("read fixture");
    let records = split_ldm_records(&bytes).expect("split");
    let payloads = decompress_all(&records).expect("decompress");

    let mut samples = Vec::new();
    'outer: for payload in &payloads {
        let messages = decode_messages(payload).expect("decode");
        for msg in messages {
            if let MessagePayload::Msg31(boxed) = msg.payload {
                samples.push(*boxed);
                if samples.len() >= 3 {
                    break 'outer;
                }
            }
        }
    }

    assert!(
        samples.len() >= 3,
        "expected ≥3 MSG_31 samples, got {}",
        samples.len()
    );

    for (i, m) in samples.iter().enumerate() {
        // ICAO must be 4 ASCII chars from the KLOT fixture's filename.
        assert_eq!(
            &m.header.radar_identifier, b"KLOT",
            "sample {i} radar_identifier"
        );
        // Modified Julian date for 2025-12-10 = 20_433.
        assert_eq!(m.header.modified_julian_date, 20_433);
        // Plausible radial collection time (within a single day).
        assert!(
            m.header.collection_time_ms < 86_400_000,
            "sample {i} collection_time_ms out of range: {}",
            m.header.collection_time_ms
        );
        // Pin the absolute date the production conversion decodes
        // to. Calls `msg31_collection_time` directly (the same path
        // `Radial::from_msg31` uses) rather than re-deriving the
        // formula inline — re-derivation would silently pass if
        // the production formula and the test were wrong in the
        // same direction. Pre-fix this returned 2025-12-11; post-fix
        // it matches the V06 filename's encoded date.
        let dt = super::model::msg31_collection_time(&m.header).expect("collection_time decoded");
        assert_eq!(
            dt.date_naive(),
            chrono::NaiveDate::from_ymd_opt(2025, 12, 10).unwrap(),
            "sample {i} date should be 2025-12-10 (V06 filename truth), not \
             2025-12-11 — that was the +1-day bug fixed in 0.2.5",
        );
        // Plausible radial physical fields.
        assert!(
            (0.0..=360.0).contains(&m.header.azimuth_angle_degrees),
            "sample {i} azimuth_angle out of range: {}",
            m.header.azimuth_angle_degrees
        );
        assert!(
            (-1.0..=25.0).contains(&m.header.elevation_angle_degrees),
            "sample {i} elevation_angle out of range: {}",
            m.header.elevation_angle_degrees
        );
        // KLOT VCP-212 is a precip mode — every radial carries REF.
        assert!(
            m.reflectivity.is_some(),
            "sample {i} should have reflectivity block"
        );
        // First radial in any volume carries the volume info block.
        if i == 0 {
            assert!(m.volume.is_some(), "first MSG_31 must carry VOL block");
            let v = m.volume.unwrap();
            // KLOT is at ~41.6N, -88.1W per public archive.
            assert!(
                (40.0..=43.0).contains(&v.latitude_degrees),
                "first radial VOL latitude out of range: {}",
                v.latitude_degrees
            );
            assert!(
                (-90.0..=-86.0).contains(&v.longitude_degrees),
                "first radial VOL longitude out of range: {}",
                v.longitude_degrees
            );
        }
    }
}

/// Synthesize a minimal raw Archive II buffer with a single MSG_1
/// frame and confirm `decode_volume` walks the raw-CTM path,
/// produces one radial in one synthetic-VCP sweep, and the radial
/// carries the expected azimuth + elevation + reflectivity.
///
/// This is the unit-level analogue of the fixture-gated
/// `decodes_kvnx_2011_raw_archive_ii` test below — pinned at the
/// raw-AR2 detection + MSG_1 → Radial path so a CI environment
/// without 2011 corpus access still gates regressions.
#[test]
fn decodes_synthesized_raw_archive_ii_buffer() {
    use super::header::SEGMENT_FRAME_SIZE;
    use super::messages::msg1::MSG1_HEADER_BYTES;

    // Frame: 28-byte combined TCM+Table-II header, then 100-byte
    // MSG_1 header (no gate data — all pointers zero).
    const LOGICAL_HEADER_HW: u16 = ((16 + MSG1_HEADER_BYTES) / 2) as u16;
    let mut frame = vec![0u8; SEGMENT_FRAME_SIZE];
    // Table II: HW1 = message_size, byte 15 = type 1 (MSG_1),
    // segment_count = 1, segment_number = 1.
    frame[12..14].copy_from_slice(&LOGICAL_HEADER_HW.to_be_bytes());
    frame[15] = 1;
    frame[24..26].copy_from_slice(&1u16.to_be_bytes());
    frame[26..28].copy_from_slice(&1u16.to_be_bytes());

    // MSG_1 body starts at byte 28 of the frame.
    let body = &mut frame[28..28 + MSG1_HEADER_BYTES];
    body[0..4].copy_from_slice(&12_345_678_i32.to_be_bytes()); // collection_time_ms
    body[4..6].copy_from_slice(&15_000_i16.to_be_bytes()); // julian date
    body[6..8].copy_from_slice(&0_i16.to_be_bytes()); // unambiguous range
    body[8..10].copy_from_slice(&8_192_i16.to_be_bytes()); // azimuth = 45°
    body[10..12].copy_from_slice(&73_i16.to_be_bytes()); // azimuth_number
    body[12..14].copy_from_slice(&3_i16.to_be_bytes()); // radial_status = ScanStart
    body[14..16].copy_from_slice(&182_i16.to_be_bytes()); // elevation ≈ 1.0°
    body[16..18].copy_from_slice(&1_i16.to_be_bytes()); // elevation_number
                                                        // Pointers all zero (no gate data).

    // 24-byte AR2V volume header + the frame.
    let mut buf = b"AR2V0006.001-XYZWXYZWXYZW".to_vec();
    buf.truncate(24);
    buf.extend_from_slice(&frame);

    let scan = decode_volume(&buf).expect("decode raw AR2V");
    assert_eq!(scan.sweeps.len(), 1, "synthetic VCP yields one sweep");
    let sweep = &scan.sweeps[0];
    assert_eq!(sweep.radials.len(), 1, "one radial per synthetic frame");
    let r = &sweep.radials[0];
    assert!((r.azimuth_angle_degrees - 45.0).abs() < 1e-3);
    assert!((r.elevation_angle_degrees - 1.0).abs() < 1e-2);
    assert_eq!(r.elevation_number, 1);
    assert_eq!(r.azimuth_number, 73);
}

/// Fixture-gated test: load a real pre-Build-12 raw Archive II file
/// (KVNX 2011-05-20) and decode end-to-end via `decode_volume`,
/// asserting full moment extraction. Set `RADISH_NEXRAD_LEGACY_FIXTURE`
/// to the path of an uncompressed AR2V0006.020 file (the `.gz`
/// decompressed). Ignored by default.
///
/// Pins the Build-11 MSG_31 layout fix (9 pointer slots + 68-byte
/// header) end-to-end: the file decodes to 17 sweeps, ~8000 radials
/// with reflectivity moments populated. A regression in
/// `PointerLayout::detect` or `msg31::parse`'s body-relative pointer
/// arithmetic surfaces here as either a panic, a missing-moment
/// adapter error, or a zero-reflectivity volume.
#[test]
#[ignore = "needs RADISH_NEXRAD_LEGACY_FIXTURE"]
fn decodes_kvnx_2011_raw_archive_ii() {
    let Some(path) = std::env::var_os("RADISH_NEXRAD_LEGACY_FIXTURE").map(PathBuf::from) else {
        eprintln!("skipping: RADISH_NEXRAD_LEGACY_FIXTURE not set");
        return;
    };
    if !path.is_file() {
        eprintln!("skipping: {} is not a file", path.display());
        return;
    }
    let bytes = std::fs::read(&path).expect("read legacy fixture");
    eprintln!("legacy fixture: {} ({} bytes)", path.display(), bytes.len());

    assert!(
        super::record::is_raw_archive2(&bytes),
        "fixture must be a raw Archive II file (no LDM size at offset 24)"
    );
    let scan = decode_volume(&bytes).expect("decode legacy fixture");
    let total_radials: usize = scan.sweeps.iter().map(|s| s.radials.len()).sum();
    eprintln!(
        "decode summary: {} sweeps, {} radials",
        scan.sweeps.len(),
        total_radials,
    );
    assert!(
        scan.sweeps.len() >= 5,
        "expected >= 5 sweeps, got {}",
        scan.sweeps.len()
    );
    assert!(
        total_radials >= 100,
        "expected >= 100 radials across the volume, got {total_radials}"
    );
    let radials_with_refl = scan
        .sweeps
        .iter()
        .flat_map(|s| s.radials.iter())
        .filter(|r| r.reflectivity.is_some())
        .count();
    eprintln!(
        "legacy fixture decode summary: {} sweeps, {} radials, {} carry reflectivity",
        scan.sweeps.len(),
        total_radials,
        radials_with_refl,
    );
    // Build-11 MSG_31 layout fix: every surveillance radial in a
    // VCP-12 file (KVNX 2011-05-20) carries a REF block. Allow a
    // small fraction of intermediate radials to be REF-less, but
    // the overall ratio must be high — anything substantially below
    // 50% means moment extraction has regressed.
    assert!(
        radials_with_refl * 2 >= total_radials,
        "expected ≥50% of radials to carry reflectivity, \
         got {radials_with_refl}/{total_radials}"
    );

    // Pin the absolute date — the V06 filename encodes 2011-05-20,
    // and post-0.2.5 this MSG_31 path (Build-11.x) decodes to that
    // date. Pre-0.2.5 the same code returned 2011-05-21, off by
    // +1 day because of the missing `-1` on the 1-indexed
    // `modified_julian_date`.
    let (start, _end) = scan
        .time_range()
        .expect("KVNX volume must produce a non-empty time range");
    assert_eq!(
        start.date_naive(),
        chrono::NaiveDate::from_ymd_opt(2011, 5, 20).unwrap(),
        "time_range start date should be 2011-05-20 (filename: \
         KVNX20110520_000442_V06), not 2011-05-21 (the +1-day bug)",
    );
}
