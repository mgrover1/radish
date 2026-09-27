//! Integration test for the NEXRAD Level 2 backend on a real fixture.
//!
//! Skipped unless either `RADISH_NEXRAD_FIXTURE` (single-file legacy
//! convention) or `RADISH_NEXRAD_FIXTURE_DIR` (corpus directory; see
//! `radish/tests/fixtures/CORPUS.md`) is set.

use std::path::{Path, PathBuf};

use radish::backends::{NexradBackend, RadarBackend};

/// Resolve the happy-path KLOT fixture. Prefers the legacy
/// `RADISH_NEXRAD_FIXTURE` env var when set; falls back to
/// `RADISH_NEXRAD_FIXTURE_DIR/KLOT20251210_102338_V06`.
fn fixture() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("RADISH_NEXRAD_FIXTURE") {
        return Some(PathBuf::from(p));
    }
    let dir = std::env::var_os("RADISH_NEXRAD_FIXTURE_DIR")?;
    let candidate = PathBuf::from(dir).join("KLOT20251210_102338_V06");
    candidate.is_file().then_some(candidate)
}

/// Resolve the KILX phantom-radial divergence fixture. Always rooted
/// at `RADISH_NEXRAD_FIXTURE_DIR` because the divergence test isn't
/// meaningful on any other file.
#[allow(dead_code)] // Will be consumed by phase 2's regression test.
fn kilx_fixture() -> Option<PathBuf> {
    let dir = std::env::var_os("RADISH_NEXRAD_FIXTURE_DIR")?;
    let candidate = PathBuf::from(dir).join("KILX20230629_154426_V06");
    candidate.is_file().then_some(candidate)
}

/// SHA-256 sums for every file in the documented corpus. Source of
/// truth: `radish/tests/fixtures/CORPUS.md`. The values are mirrored
/// here so the test fails loudly if a maintainer updates the docs but
/// not the code (or vice versa).
const CORPUS_SHA256: &[(&str, &str)] = &[
    (
        "KLOT20251210_102338_V06",
        "a5ed05d7dceaaceeb5adfb08601f10276a77a161ffdae7f302c49626e16cca81",
    ),
    (
        "KILX20230629_154426_V06",
        "715c3c18691f6efe87a27127d631add8d90fd92c66a019a17965b624757180da",
    ),
    // The KVNX cross-RDA-build pair — 8-bit vs 16-bit ZDR encoding
    // either side of the 2020-06-02 upgrade outage. Regression gate for
    // the per-moment decoders' remap logic (issue #32).
    (
        "KVNX20200602_123502_V06",
        "fde3fda1ca80e7fc3d2d859cc591ee7c4da7a80b17c2166a19f6f7047950bd1c",
    ),
    (
        "KVNX20200602_201830_V06",
        "cea716258763881b28f57483b65b144526e554bfe773aaa1df942c4a3024b855",
    ),
];

/// Hex-format a SHA-256 digest without pulling in the `hex` crate.
fn hex32(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        std::fmt::Write::write_fmt(&mut s, format_args!("{b:02x}")).unwrap();
    }
    s
}

/// Pin the corpus's bytes against the SHA-256 sums published in
/// `CORPUS.md`. Skips when `RADISH_NEXRAD_FIXTURE_DIR` is unset
/// (matches the rest of this file). When the env var IS set but a
/// file's contents drift from the documented sum, fail loudly so
/// downstream parity tests don't run against unverified data.
#[test]
fn corpus_sha256s_match_documentation() {
    use sha2::{Digest, Sha256};

    let Some(dir) = std::env::var_os("RADISH_NEXRAD_FIXTURE_DIR") else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE_DIR not set");
        return;
    };
    let dir = PathBuf::from(dir);

    for (name, expected_hex) in CORPUS_SHA256 {
        let path = dir.join(name);
        if !path.is_file() {
            eprintln!("skipping {name}: not present at {}", path.display());
            continue;
        }
        let bytes = std::fs::read(&path).expect("read fixture");
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let actual_hex = hex32(&digest);
        assert_eq!(
            &actual_hex,
            expected_hex,
            "{name} sha256 mismatch — file at {} has been replaced or \
             corrupted; re-acquire from the URL in CORPUS.md or update \
             both the docs AND CORPUS_SHA256 if this is intentional",
            path.display(),
        );
    }
}

#[test]
fn read_volume_on_real_fixture() {
    let Some(path) = fixture() else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE not set");
        return;
    };
    let backend = NexradBackend::new();
    assert!(backend.can_read(Path::new(&path)));

    let volume = backend.read_volume(&path).expect("decode failed");
    let m = &volume.metadata;

    assert_eq!(m.instrument_name.len(), 4, "ICAO must be 4 chars");
    assert!(
        volume.num_sweeps() >= 5,
        "expected at least 5 sweeps, got {}",
        volume.num_sweeps()
    );
    assert_eq!(m.sweep_group_names.len(), volume.num_sweeps());
    assert_eq!(m.sweep_fixed_angles.len(), volume.num_sweeps());
    // Every sweep_fixed_angle should be a finite, plausible elevation
    // (range covers WSR-88D's full 0..20° envelope plus headroom). A
    // NaN here means the cut→median→NaN fallback chain in
    // `fixed_angle_for` short-circuited; a value outside [-1, 25] means
    // we read the wrong MSG_5 field. Both regression-prone after the
    // commanded-vs-achieved fix.
    for (i, &angle) in m.sweep_fixed_angles.iter().enumerate() {
        assert!(angle.is_finite(), "sweep_fixed_angles[{i}] is NaN");
        assert!(
            (-1.0..=25.0).contains(&angle),
            "sweep_fixed_angles[{i}] = {angle} out of plausible WSR-88D range"
        );
    }
    // Volume-level sweep_fixed_angles must agree with the per-sweep
    // SweepMetadata.fixed_angle. The two come from different code
    // paths (build_volume_metadata vs convert_sweep) and a future
    // refactor could let them drift; this catches it.
    for (i, sweep) in volume.sweeps.iter().enumerate() {
        let vol_angle = m.sweep_fixed_angles[i];
        let sweep_angle = sweep.metadata.fixed_angle;
        assert!(
            (vol_angle - sweep_angle).abs() < 1e-6,
            "sweep {i}: volume={vol_angle} but per-sweep={sweep_angle} — fixed_angle_for path drift"
        );
    }
    assert!((-90.0..=90.0).contains(&m.latitude), "latitude in range");
    assert!(
        (-180.0..=180.0).contains(&m.longitude),
        "longitude in range"
    );

    // Every sweep should have at least DBZH and consistent ray-shaped coords.
    for (i, sweep) in volume.sweeps.iter().enumerate() {
        let coords = &sweep.coordinates;
        assert_eq!(
            coords.azimuth.len(),
            coords.elevation.len(),
            "sweep {i}: az/el ray count mismatch"
        );
        assert_eq!(
            coords.azimuth.len(),
            coords.time.len(),
            "sweep {i}: az/time ray count mismatch"
        );
        assert!(!coords.range.is_empty(), "sweep {i} has no range gates");
        assert!(
            sweep.moments.contains_key("DBZH") || sweep.moments.contains_key("VRADH"),
            "sweep {i} has neither DBZH nor VRADH (moment names: {:?})",
            sweep.moments.keys().collect::<Vec<_>>()
        );

        // Every moment must match the (rays, gates) shape.
        for (name, m) in &sweep.moments {
            let (r, g) = m.shape();
            assert_eq!(
                r,
                coords.azimuth.len(),
                "sweep {i} moment {name}: ray count mismatch"
            );
            assert_eq!(
                g,
                coords.range.len(),
                "sweep {i} moment {name}: gate count mismatch"
            );
        }
    }
}

#[test]
fn scan_file_returns_metadata() {
    let Some(path) = fixture() else {
        return;
    };
    let m = NexradBackend::new().scan_file(&path).expect("scan failed");
    assert_eq!(m.instrument_name.len(), 4);
    assert!(!m.sweep_group_names.is_empty());
}

/// HIGH-priority: pin the `scan_nexrad`-vs-`read_nexrad` agreement
/// for the per-sweep arrays surfaced for the raw2zarr bulk-ingest
/// path. Both code paths route through `volume_attrs`, so any future
/// refactor that decouples them must keep the surfaces identical.
///
/// Also asserts the length invariant against
/// `VolumeMetadata.sweep_fixed_angles` — same VCP-truncation edge
/// case the padding contract handles.
#[test]
fn scan_and_read_agree_on_per_sweep_attrs_and_time_ranges() {
    let Some(path) = fixture() else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE not set");
        return;
    };
    let backend = NexradBackend::new();
    let scanned = backend.scan_file(&path).expect("scan failed");
    let volume = backend.read_volume(&path).expect("read failed");

    let scanned_nx = scanned.nexrad.as_ref().expect("scan must populate nexrad");
    assert_eq!(
        scanned_nx.sweep_attrs.len(),
        scanned.sweep_fixed_angles.len(),
        "sweep_attrs len must match sweep_fixed_angles len",
    );
    assert_eq!(
        scanned_nx.sweep_time_ranges.len(),
        scanned.sweep_fixed_angles.len(),
        "sweep_time_ranges len must match sweep_fixed_angles len",
    );

    // scan vs read agreement: every per-sweep entry surfaced by
    // `scan_file` must equal the corresponding entry on the
    // fully-decoded `read_volume`'s per-sweep `nexrad_attrs`.
    for (i, sweep) in volume.sweeps.iter().enumerate() {
        let read_attrs = sweep
            .metadata
            .nexrad
            .as_ref()
            .expect("read_volume must attach per-sweep nexrad attrs");
        assert_eq!(
            &scanned_nx.sweep_attrs[i], read_attrs,
            "sweep {i}: scan_file vs read_volume per-sweep attrs drift",
        );
    }

    // Every time range that's `Some` must satisfy `start <= end`. We
    // don't require all sweeps to have timestamps — very old archives
    // can carry sweeps without `collection_time` — but if we did
    // produce a range, it must be ordered.
    for (i, range) in scanned_nx.sweep_time_ranges.iter().enumerate() {
        if let Some((start, end)) = range {
            assert!(start.is_finite(), "sweep {i} time_range start non-finite");
            assert!(end.is_finite(), "sweep {i} time_range end non-finite");
            assert!(
                start <= end,
                "sweep {i} time_range invariant: start={start} > end={end}",
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Plan 0009: real-time chunk fixture tests (S/I/E framing, incomplete
// sweeps, drop/pad policies). Gated on `RADISH_NEXRAD_CHUNKS_DIR` —
// see CORPUS.md "Real-time chunk fixture" for acquisition.
// ---------------------------------------------------------------------------

/// Load the KLOT chunk fixture in scan order (lexicographic file-name
/// sort == chunk sequence order for the bucket's naming convention).
fn chunk_fixture() -> Option<Vec<Vec<u8>>> {
    let dir = PathBuf::from(std::env::var_os("RADISH_NEXRAD_CHUNKS_DIR")?);
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file())
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort();
    names
        .iter()
        .map(|p| std::fs::read(p).ok())
        .collect::<Option<Vec<_>>>()
}

/// NaN-tolerant element equality for two moment views: equal when both
/// values match bitwise-meaningfully (x == y, or both NaN).
fn nan_eq<'a>(a: impl IntoIterator<Item = &'a f32>, b: impl IntoIterator<Item = &'a f32>) -> bool {
    a.into_iter()
        .zip(b)
        .all(|(x, y)| (x == y) || (x.is_nan() && y.is_nan()))
}

/// Byte-concatenating real S/I/E chunks must decode identically to
/// feeding the chunk list — same sweeps, same completeness, and
/// bit-identical moment arrays.
#[test]
fn real_chunks_match_concatenated_blob_decode() {
    let Some(chunks) = chunk_fixture() else {
        return;
    };
    let blob: Vec<u8> = chunks.concat();

    let backend = NexradBackend::new();
    let from_chunks = backend
        .read_chunks_volume(chunks)
        .expect("chunk-list decode");
    let from_blob = backend.read_bytes_volume(blob).expect("blob decode");

    assert_eq!(from_chunks.sweeps.len(), 12, "KLOT fixture has 12 sweeps");
    assert_eq!(from_chunks.sweeps.len(), from_blob.sweeps.len());
    assert!(from_chunks.metadata.incomplete_sweep_indices.is_empty());
    for (a, b) in from_chunks.sweeps.iter().zip(&from_blob.sweeps) {
        assert_eq!(a.coordinates.azimuth, b.coordinates.azimuth);
        assert!(a.metadata.is_complete);
        let (da, db) = (&a.moments["DBZH"].data, &b.moments["DBZH"].data);
        assert_eq!(da.shape(), db.shape());
        assert!(nan_eq(da.iter(), db.iter()));
    }
}

/// A partial chunk list (S + 10 I) truncates mid-sweep: the trailing
/// sweep is flagged incomplete under `Keep`, omitted (keeping original
/// accounting) under `Drop`, and reindexed onto the full grid under
/// `Pad` with observed rays bit-identical to the `Keep` output.
#[test]
fn partial_chunk_list_keep_drop_pad_semantics() {
    use radish::backends::IncompleteSweepPolicy;

    let Some(chunks) = chunk_fixture() else {
        return;
    };
    let partial: Vec<Vec<u8>> = chunks[..11].to_vec();
    let backend = NexradBackend::new();

    let keep = backend
        .read_chunks_volume_with(partial.clone(), IncompleteSweepPolicy::Keep)
        .expect("keep decode");
    assert_eq!(keep.sweeps.len(), 2);
    assert!(keep.sweeps[0].metadata.is_complete);
    assert!(!keep.sweeps[1].metadata.is_complete);
    assert_eq!(keep.metadata.incomplete_sweep_indices, vec![1]);
    let observed = keep.sweeps[1].coordinates.azimuth.len();
    assert!(
        observed < 720,
        "trailing sweep must be short, got {observed}"
    );

    let drop = backend
        .read_chunks_volume_with(partial.clone(), IncompleteSweepPolicy::Drop)
        .expect("drop decode");
    assert_eq!(drop.sweeps.len(), 1);
    assert_eq!(drop.metadata.sweep_group_names, vec!["sweep_0"]);
    assert_eq!(drop.metadata.incomplete_sweep_indices, vec![1]);

    let pad = backend
        .read_chunks_volume_with(partial, IncompleteSweepPolicy::Pad)
        .expect("pad decode");
    let padded = &pad.sweeps[1];
    let n = padded.coordinates.azimuth.len();
    assert_eq!(n, 720, "split cut pads to 0.5° spacing");
    for (i, az) in padded.coordinates.azimuth.iter().enumerate() {
        assert!((az - (i as f32 + 0.5) * 0.5).abs() < 1e-4);
    }
    // Every observed ray survives bit-identically at its grid slot.
    let keep_sweep = &keep.sweeps[1];
    let mut matched = 0usize;
    for (k, az) in keep_sweep.coordinates.azimuth.iter().enumerate() {
        let slot = ((az.rem_euclid(360.0) / 0.5) as usize).min(719);
        let padded_row = padded.moments["DBZH"].data.row(slot);
        let keep_row = keep_sweep.moments["DBZH"].data.row(k);
        if nan_eq(padded_row.iter(), keep_row.iter()) {
            matched += 1;
        }
    }
    assert_eq!(matched, observed, "all observed rays preserved by pad");
    // Missing rows are all-NaN.
    let nan_rows = (0..n)
        .filter(|&i| {
            padded.moments["DBZH"]
                .data
                .row(i)
                .iter()
                .all(|v| v.is_nan())
        })
        .count();
    assert_eq!(nan_rows, n - observed);
    // Times are finite and non-decreasing along rotation order from the
    // sweep's first observed slot.
    assert!(padded.coordinates.time.iter().all(|t| t.is_finite()));
}

/// An I-only stream (no S chunk) joins mid-rotation: it still decodes,
/// and the leading sweep is flagged incomplete.
#[test]
fn headerless_i_only_stream_flags_leading_sweep() {
    let Some(chunks) = chunk_fixture() else {
        return;
    };
    let backend = NexradBackend::new();
    let v = backend
        .read_chunks_volume(chunks[1..6].to_vec())
        .expect("headerless decode");
    assert!(!v.sweeps.is_empty());
    assert!(!v.sweeps[0].metadata.is_complete);
    assert_eq!(v.metadata.incomplete_sweep_indices, vec![0]);
}
