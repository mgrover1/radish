//! Integration test for the Sigmet/IRIS RAW backend on a real fixture.
//!
//! Skipped unless `RADISH_SIGMET_FIXTURE` points at an IRIS RAW file.
//! The skip is implemented as an early `return` rather than `#[ignore]`
//! so CI (which sets the env var) sees these as a passing test rather
//! than a silently-ignored one — same convention as `test_nexrad.rs`.

use std::path::Path;

use radish::backends::{RadarBackend, SigmetBackend};

fn fixture() -> Option<std::path::PathBuf> {
    std::env::var_os("RADISH_SIGMET_FIXTURE").map(Into::into)
}

#[test]
fn read_volume_on_real_fixture() {
    let Some(path) = fixture() else {
        eprintln!("skipping: RADISH_SIGMET_FIXTURE not set");
        return;
    };
    let backend = SigmetBackend::new();
    assert!(
        backend.can_read(Path::new(&path)),
        "SigmetBackend.can_read should accept the fixture's extension"
    );

    let volume = backend.read_volume(&path).expect("decode failed");
    let m = &volume.metadata;

    assert!(
        !m.instrument_name.is_empty(),
        "instrument_name should be populated from INGEST_CONFIGURATION"
    );
    assert!(
        volume.num_sweeps() >= 1,
        "expected at least 1 sweep, got {}",
        volume.num_sweeps()
    );
    assert_eq!(m.sweep_group_names.len(), volume.num_sweeps());
    assert!((-90.0..=90.0).contains(&m.latitude), "latitude in range");
    assert!(
        (-180.0..=180.0).contains(&m.longitude),
        "longitude in range"
    );
    assert!(m.sigmet.is_some(), "sigmet attrs should be populated");
    let sigmet = m.sigmet.as_ref().unwrap();
    assert!(matches!(sigmet.scan_mode.as_str(), "PPI" | "RHI" | "OTHER"));

    for (i, sweep) in volume.sweeps.iter().enumerate() {
        let coords = &sweep.coordinates;
        assert_eq!(
            coords.azimuth.len(),
            coords.elevation.len(),
            "sweep {i}: az/el ray count mismatch"
        );
        assert!(!coords.range.is_empty(), "sweep {i} has no range gates");
        // Should carry at least one of the typical Sigmet moments.
        assert!(
            sweep.moments.contains_key("DBZH")
                || sweep.moments.contains_key("DBTH")
                || sweep.moments.contains_key("VRADH"),
            "sweep {i} has none of DBZH/DBTH/VRADH (got: {:?})",
            sweep.moments.keys().collect::<Vec<_>>()
        );
        assert!(
            sweep.metadata.sigmet.is_some(),
            "sweep {i} missing sigmet sweep attrs"
        );
    }
}

#[test]
fn read_bytes_volume_round_trip_matches_path() {
    let Some(path) = fixture() else {
        eprintln!("skipping: RADISH_SIGMET_FIXTURE not set");
        return;
    };
    let backend = SigmetBackend::new();
    let from_path = backend.read_volume(&path).expect("read_volume failed");
    let bytes = std::fs::read(&path).expect("read fixture bytes");
    assert!(backend.can_read_bytes(&bytes[..16]));
    let from_bytes = backend
        .read_bytes_volume(bytes)
        .expect("read_bytes_volume failed");

    assert_eq!(
        from_path.num_sweeps(),
        from_bytes.num_sweeps(),
        "sweep count must match between path and bytes paths"
    );
    assert_eq!(
        from_path.metadata.instrument_name, from_bytes.metadata.instrument_name,
        "instrument_name must match"
    );
    assert_eq!(
        from_path.metadata.sweep_group_names, from_bytes.metadata.sweep_group_names,
        "sweep group names must match"
    );
}
