//! NEXRAD Level 3 (NIDS) backend — single-tilt, single-moment digital
//! radial products.
//!
//! See `docs/NEXRAD_LEVEL3_WASM.md` for the design this backend
//! implements (why it exists, the byte-level contract, what's
//! deliberately NOT read from the file) and what's tested against the
//! byte-level oracle.
//!
//! Scope: packet 16 (digital radial data array) and packet `AF1F` (RLE
//! radials, the legacy 8/16-level product family) — 19 of 26 message
//! codes from xradar #392's product table decode past the PDB stage; the
//! rest (packet 28/XDR + 5 surface precip codes) return a named
//! `UnsupportedProduct` error — see `decode::products`'s module doc.

mod adapter;
pub(crate) mod decode;
mod sniff;

use std::path::Path;

use crate::backends::RadarBackend;
use crate::{RadishError, Result, SweepData, VolumeData, VolumeMetadata};

/// NEXRAD Level 3 (NIDS) backend. Reads single-tilt, single-moment digital
/// radial products (packet 16) — see the module docs for scope.
#[derive(Debug, Default, Clone, Copy)]
pub struct NexradLevel3Backend;

impl NexradLevel3Backend {
    /// Create a new backend instance. Stateless — every method takes the
    /// bytes/path it needs directly.
    pub fn new() -> Self {
        Self
    }
}

/// Decode + normalize in one step — the only code path every
/// `RadarBackend` method funnels through. NIDS products are small
/// (real-world max ~1.3 MB decompressed) and single-message, so unlike
/// NEXRAD Level 2's multi-record volumes there's no cheaper
/// metadata-only path to offer `scan_file`.
fn decode_and_convert(bytes: &[u8]) -> Result<VolumeData> {
    let product = decode::decode(bytes).map_err(|e| RadishError::Decode(e.to_string()))?;
    Ok(adapter::convert(product))
}

impl RadarBackend for NexradLevel3Backend {
    fn name(&self) -> &str {
        "nexrad_level3"
    }

    fn description(&self) -> &str {
        "NEXRAD Level 3 (NIDS) digital radial products"
    }

    fn supported_extensions(&self) -> &[&str] {
        // NIDS files have no reliable extension — detection is purely
        // content-based (`sniff::can_read`/`can_read_bytes`).
        &[]
    }

    fn can_read(&self, path: &Path) -> bool {
        sniff::can_read(path)
    }

    fn can_read_bytes(&self, head: &[u8]) -> bool {
        sniff::can_read_bytes(head)
    }

    fn scan_file(&self, path: &Path) -> Result<VolumeMetadata> {
        let bytes = std::fs::read(path)?;
        Ok(decode_and_convert(&bytes)?.metadata)
    }

    fn read_sweep(&self, path: &Path, sweep_idx: usize) -> Result<SweepData> {
        // NIDS products are single-sweep; index 0 is the only valid one.
        // Checked before touching disk.
        if sweep_idx != 0 {
            return Err(RadishError::InvalidSweepIndex(sweep_idx));
        }
        let bytes = std::fs::read(path)?;
        let mut volume = decode_and_convert(&bytes)?;
        Ok(volume.sweeps.remove(0))
    }

    fn read_volume(&self, path: &Path) -> Result<VolumeData> {
        let bytes = std::fs::read(path)?;
        decode_and_convert(&bytes)
    }

    /// NIDS decode is naturally a bytes-in operation — no filename needed
    /// anywhere in the format — so this is the same code path as
    /// [`RadarBackend::read_volume`], just skipping the file read. This is
    /// also the entry point the `wasm` crate (Phase 7) binds directly.
    fn read_bytes_volume(&self, data: Vec<u8>) -> Result<VolumeData> {
        decode_and_convert(&data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_description_are_static() {
        let backend = NexradLevel3Backend::new();
        assert_eq!(backend.name(), "nexrad_level3");
        assert!(!backend.description().is_empty());
    }

    #[test]
    fn supported_extensions_is_empty_detection_is_content_based() {
        assert!(NexradLevel3Backend::new().supported_extensions().is_empty());
    }

    #[test]
    fn can_read_bytes_recognises_a_nids_awips_token_with_a_message_header() {
        // Sniffing requires both a well-formed token AND a validating
        // message header (see `sniff.rs`'s module doc) — pad the prefix to
        // 30 bytes so `find_message_header`'s scan window reaches it.
        let mut buf = b"N0BLOT\r\r\n".to_vec();
        while buf.len() < 30 {
            buf.push(b' ');
        }
        buf.extend_from_slice(&153i16.to_be_bytes());
        buf.extend_from_slice(&[0u8; 16]);
        buf.extend_from_slice(&(-1i16).to_be_bytes());
        buf.extend_from_slice(&[0u8; 8]); // trailing slack, see sniff.rs's helper

        let backend = NexradLevel3Backend::new();
        assert!(backend.can_read_bytes(&buf));
        assert!(!backend.can_read_bytes(b"AR2V0006.001"));
    }

    #[test]
    fn read_sweep_rejects_nonzero_index_without_touching_disk() {
        // sweep_idx is checked before the file read, so a nonexistent path
        // still exercises the right error rather than an I/O one.
        let backend = NexradLevel3Backend::new();
        let err = backend
            .read_sweep(Path::new("/no/such/file"), 1)
            .unwrap_err();
        match err {
            RadishError::InvalidSweepIndex(1) => {}
            other => panic!("expected InvalidSweepIndex(1), got {other:?}"),
        }
    }

    #[test]
    fn read_bytes_volume_rejects_garbage() {
        let backend = NexradLevel3Backend::new();
        let err = backend.read_bytes_volume(b"not a nids product".to_vec());
        assert!(err.is_err());
    }

    #[test]
    fn read_bytes_volume_decodes_a_synthetic_product_end_to_end() {
        // The actual success path — `read_bytes_volume` -> `decode_and_convert`
        // -> `decode::decode` -> `adapter::convert` -> `VolumeData` — had
        // zero test coverage before this: the sibling `decode`-layer tests
        // only exercise `decode::decode`, never `adapter::convert`, and the
        // previous version of this test only ever fed it garbage. This is
        // the one place the full public bytes-in -> `VolumeData`-out
        // contract the `wasm` crate (Phase 7) will call directly is
        // checked.
        let backend = NexradLevel3Backend::new();
        let volume = backend
            .read_bytes_volume(decode::build_minimal_n0b())
            .expect("a valid synthetic N0B product should decode");

        assert_eq!(volume.num_sweeps(), 1);
        assert_eq!(volume.metadata.instrument_name, "LOT");
        assert_eq!(volume.metadata.site_name.as_deref(), Some("LOT"));
        assert!((volume.metadata.latitude - 41.881).abs() < 1e-6);
        assert!((volume.metadata.longitude - (-88.084)).abs() < 1e-6);

        let sweep = &volume.sweeps[0];
        assert_eq!(sweep.metadata.sweep_number, 0);
        let nids = sweep.metadata.nids.as_ref().expect("nids attrs present");
        assert_eq!(nids.site, "LOT");
        assert_eq!(nids.awips_id, "N0B");
        assert_eq!(nids.message_code, 153);
        assert_eq!(nids.vcp, 215);
        assert_eq!(nids.tilt, Some(0));

        let moment = sweep.moments.get("DBZH").expect("DBZH moment present");
        assert_eq!(moment.units, "dBZ");
        let scale = moment
            .declared_scale
            .expect("linear-coded moment carries a declared scale");
        assert_eq!(scale.value_min, -32.0);
        assert_eq!(scale.value_increment, 0.5);
        let codes = moment.raw_codes.as_ref().expect("raw_codes populated");
        assert_eq!(codes.row(0).to_vec(), vec![2, 3, 4, 5]);
        assert_eq!(
            moment.data.row(0).to_vec(),
            vec![-32.0, -31.5, -31.0, -30.5]
        );

        assert_eq!(sweep.coordinates.range[0], 125.0); // first_gate_m
        assert_eq!(
            sweep.coordinates.range[1] - sweep.coordinates.range[0],
            250.0
        );
        assert!((sweep.coordinates.azimuth[0] - 0.5).abs() < 1e-6);
    }
}
