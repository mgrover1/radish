//! IRIS / Sigmet RAW backend (Vaisala SIGMET binary format).
//!
//! Reads the on-wire binary defined in Vaisala ICD 2620002AA, ported from
//! xradar's `xradar/io/backends/iris.py`. The format wraps a `PRODUCT_HDR`
//! (id=27) around a `INGEST_HEADER` (id=23) followed by `INGEST_DATA_HEADER`
//! / `RAW_PROD_BHDR` records that carry the actual sweep data — every ray
//! has its bins RLE-compressed and per-data-type calibrated (different
//! scale/offset rules per moment).
//!
//! Module layout mirrors the NEXRAD backend:
//!
//! * `mod.rs` — `SigmetBackend` struct and `RadarBackend` trait impl.
//! * `sniff.rs` — magic-byte / extension-based format detection.
//! * `structs.rs` — `#[repr(C, packed)]` `bytemuck::Pod` definitions for
//!   the on-wire fixed-layout structs.
//! * `decode.rs` — top-level walker (`parse_volume`), per-record / per-ray
//!   parsers, and the RLE decompressor.
//! * `calibration.rs` — per-data-type `decode_*` helpers (DBZ, VEL,
//!   PHIDP, …) matching xradar's `SIGMET_DATA_TYPES.func` column.
//! * `mapping.rs` — `DB_DBZ → DBZH` etc. CF strings come from the
//!   shared `backends::common::metadata` table.
//! * `adapter.rs` — converts the decoded structures into radish's
//!   `VolumeData` via `backends::common::*` helpers.

use std::path::Path;

use crate::{backends::RadarBackend, RadishError, Result, SweepData, VolumeData, VolumeMetadata};

mod adapter;
mod calibration;
mod decode;
mod mapping;
mod sniff;
mod structs;

/// Backend for IRIS / Sigmet RAW files (Vaisala SIGMET / IRIS binary).
pub struct SigmetBackend;

impl SigmetBackend {
    /// Create a new backend instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for SigmetBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RadarBackend for SigmetBackend {
    fn name(&self) -> &str {
        "sigmet"
    }

    fn description(&self) -> &str {
        "Vaisala SIGMET / IRIS RAW (ICD 2620002AA)"
    }

    fn supported_extensions(&self) -> &[&str] {
        sniff::EXTENSIONS
    }

    /// IRIS files are commonly distributed with site-specific extensions
    /// (e.g. `.RAWE5P1` for the CHI / Chiriquí radar) so we can't rely on
    /// extension matching alone — the sniff pipeline also checks the
    /// 2-byte LE structure-identifier at offset 0.
    fn can_read(&self, path: &Path) -> bool {
        sniff::looks_like_iris(path)
    }

    fn can_read_bytes(&self, head: &[u8]) -> bool {
        sniff::looks_like_iris_bytes(head)
    }

    fn read_volume(&self, path: &Path) -> Result<VolumeData> {
        let data = std::fs::read(path)?;
        let decoded = decode::parse_volume(&data)?;
        adapter::convert_volume(decoded, path)
    }

    fn read_bytes_volume(&self, data: Vec<u8>) -> Result<VolumeData> {
        let decoded = decode::parse_volume(&data)?;
        adapter::convert_volume(decoded, Path::new("<bytes>"))
    }

    fn scan_file(&self, path: &Path) -> Result<VolumeMetadata> {
        // MVP: full decode then drop sweeps. A cheap path would only parse
        // INGEST_HEADER (skip the LDM body), but VolumeMetadata also wants
        // sweep_fixed_angles per sweep, which forces at least one
        // INGEST_DATA_HEADER per sweep. Defer the optimisation.
        let data = std::fs::read(path)?;
        let decoded = decode::parse_volume(&data)?;
        adapter::build_volume_metadata(&decoded, path)
    }

    fn read_sweep(&self, path: &Path, sweep_idx: usize) -> Result<SweepData> {
        let data = std::fs::read(path)?;
        let decoded = decode::parse_volume(&data)?;
        adapter::convert_sweep_at(&decoded, sweep_idx)
            .ok_or(RadishError::InvalidSweepIndex(sweep_idx))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_description_are_static() {
        let b = SigmetBackend::new();
        assert_eq!(b.name(), "sigmet");
        assert!(b.description().contains("SIGMET"));
    }

    #[test]
    fn cannot_read_random_extension() {
        let b = SigmetBackend::new();
        assert!(!b.can_read(Path::new("/data/foo.txt")));
    }

    #[test]
    fn can_read_bytes_recognises_iris_magic() {
        let b = SigmetBackend::new();
        // INGEST_HEADER id=23 (0x0017 LE)
        assert!(b.can_read_bytes(&[0x17, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00]));
        // PRODUCT_HDR id=27 (0x001b LE)
        assert!(b.can_read_bytes(&[0x1b, 0x00, 0x08, 0x00, 0x00, 0xe8, 0x62, 0x00]));
        // Garbage
        assert!(!b.can_read_bytes(b"GARBAGE!"));
        // AR2V (NEXRAD, not IRIS)
        assert!(!b.can_read_bytes(b"AR2V0006.001"));
    }

    /// `read_sweep` with a fixture path but an out-of-range sweep_idx
    /// must surface `InvalidSweepIndex`, not panic. Gated on
    /// `RADISH_SIGMET_FIXTURE`; cheap when set.
    #[test]
    fn read_sweep_invalid_index_returns_error() {
        let Some(path) = std::env::var_os("RADISH_SIGMET_FIXTURE") else {
            eprintln!("skipping: RADISH_SIGMET_FIXTURE not set");
            return;
        };
        let b = SigmetBackend::new();
        let err = b
            .read_sweep(Path::new(&path), 9999)
            .expect_err("must reject out-of-range sweep_idx");
        match err {
            RadishError::InvalidSweepIndex(9999) => (),
            other => panic!("expected InvalidSweepIndex(9999), got {other:?}"),
        }
    }
}
