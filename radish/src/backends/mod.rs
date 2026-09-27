//! Backend system for reading different radar formats.

use crate::{Result, SweepData, VolumeData, VolumeMetadata};
use std::path::Path;

// `cfradial1` is the only backend that directly links `hdf5`/`netcdf` (C
// libraries with no `wasm32-unknown-unknown` build story — see
// `docs/NEXRAD_LEVEL3_WASM.md` §5), so it's gated behind the `native`
// feature. `nexrad`, `sigmet`, and (once added) `nexrad_level3` are pure
// byte-parsing backends with no C dependency and stay unconditional; only
// their `rayon` usage is feature-gated (`backends::common::parallel`).
#[cfg(feature = "native")]
pub mod cfradial1;
pub(crate) mod common;
pub mod nexrad;
pub mod nexrad_level3;
pub mod sigmet;

#[cfg(feature = "native")]
pub use cfradial1::CfRadial1Backend;
pub use nexrad::{IncompleteSweepPolicy, NexradBackend};
pub use nexrad_level3::NexradLevel3Backend;
pub use sigmet::SigmetBackend;

/// Trait for radar file format backends
///
/// Each backend implements parsing for a specific file format (CfRadial1, IRIS, etc.)
/// and normalizes the data to the common data model.
pub trait RadarBackend: Send + Sync {
    /// Backend name (e.g., "cfradial1", "iris", "nexrad")
    fn name(&self) -> &str;

    /// Backend description
    fn description(&self) -> &str;

    /// Supported file extensions (e.g., &["nc", "nc4"])
    fn supported_extensions(&self) -> &[&str];

    /// Scan file to extract volume metadata without reading all data
    ///
    /// This is useful for quickly determining what's in a file before
    /// committing to reading the full volume.
    fn scan_file(&self, path: &Path) -> Result<VolumeMetadata>;

    /// Read a specific sweep from the file
    ///
    /// This allows lazy loading of sweep data.
    fn read_sweep(&self, path: &Path, sweep_idx: usize) -> Result<SweepData>;

    /// Read the entire volume including all sweeps
    ///
    /// This is the primary method for loading radar data.
    fn read_volume(&self, path: &Path) -> Result<VolumeData>;

    /// Check if this backend can read the given file
    ///
    /// Default implementation checks file extension.
    fn can_read(&self, path: &Path) -> bool {
        if let Some(ext) = path.extension() {
            if let Some(ext_str) = ext.to_str() {
                return self.supported_extensions().contains(&ext_str);
            }
        }
        false
    }

    /// Check whether a byte-prefix (head) plausibly belongs to this format.
    ///
    /// Used by the central [`auto_backend_for_bytes`] dispatcher when the
    /// caller has the file in memory (S3 fetch, HTTP body, etc.) instead of
    /// on disk. Default returns `false`; override per-backend with cheap
    /// magic-byte checks (no I/O).
    fn can_read_bytes(&self, _head: &[u8]) -> bool {
        false
    }

    /// Decode a volume from a single in-memory byte buffer.
    ///
    /// Default returns [`crate::RadishError::Unsupported`] — only formats whose
    /// upstream parsers accept owned byte buffers (currently NEXRAD via
    /// `nexrad-data`) override this. CfRadial1 stays at the default because
    /// `libnetcdf` needs a filename and doesn't expose an in-memory open.
    fn read_bytes_volume(&self, _data: Vec<u8>) -> Result<VolumeData> {
        Err(crate::RadishError::Unsupported(format!(
            "{}: in-memory bytes input not supported",
            self.name()
        )))
    }
}

/// Get all available backends
///
/// Under `--no-default-features` (wasm builds), `cfradial1` is absent — it
/// links `hdf5`/`netcdf`, which don't target `wasm32-unknown-unknown`. Every
/// other backend is unconditional.
pub fn available_backends() -> Vec<Box<dyn RadarBackend>> {
    vec![
        #[cfg(feature = "native")]
        Box::new(CfRadial1Backend::new()),
        Box::new(NexradBackend::new()),
        Box::new(SigmetBackend::new()),
        // Last: its sniff is content-based (scans up to 128 bytes for an
        // AWIPS token) rather than a cheap extension/magic-prefix check,
        // so formats with a fast positive signal get first refusal.
        Box::new(NexradLevel3Backend::new()),
    ]
}

/// Automatically select the appropriate backend for a file
pub fn auto_backend(path: &Path) -> Result<Box<dyn RadarBackend>> {
    for backend in available_backends() {
        if backend.can_read(path) {
            return Ok(backend);
        }
    }

    Err(crate::RadishError::InvalidFormat(format!(
        "No backend found for file: {}",
        path.display()
    )))
}

/// Automatically select the appropriate backend for an in-memory byte buffer.
///
/// Iterates [`available_backends`] in declaration order and returns the first
/// whose [`RadarBackend::can_read_bytes`] accepts the prefix. Fails with
/// [`crate::RadishError::InvalidFormat`] if no backend recognises the buffer.
///
/// Mirrors [`auto_backend`] for the file-path path. The two are kept in sync
/// so the Python `_open.py` dispatch table can pick the right shape→reader
/// combination from one shared backend list.
pub fn auto_backend_for_bytes(head: &[u8]) -> Result<Box<dyn RadarBackend>> {
    for backend in available_backends() {
        if backend.can_read_bytes(head) {
            return Ok(backend);
        }
    }

    Err(crate::RadishError::InvalidFormat(format!(
        "No backend found for in-memory buffer (first {} bytes)",
        head.len().min(8)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_backend_for_bytes_routes_ar2v_to_nexrad() {
        let head = b"AR2V0006.001....";
        let backend = auto_backend_for_bytes(head).expect("AR2V → some backend");
        assert_eq!(backend.name(), "nexrad_level2");
    }

    #[test]
    fn auto_backend_for_bytes_routes_gzip_to_nexrad() {
        // Gzip-wrapped Archive II (older `*.gz` archive volumes) — leading
        // gzip magic is enough; the upstream nexrad-data decompresses.
        let head = &[0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00];
        let backend = auto_backend_for_bytes(head).expect("gzip → some backend");
        assert_eq!(backend.name(), "nexrad_level2");
    }

    #[test]
    #[cfg(feature = "native")]
    fn auto_backend_for_bytes_routes_hdf5_to_cfradial1() {
        let head = b"\x89HDF\r\n\x1a\nrest";
        let backend = auto_backend_for_bytes(head).expect("HDF5 → some backend");
        assert_eq!(backend.name(), "cfradial1");
    }

    #[test]
    fn auto_backend_for_bytes_errors_on_unknown_magic() {
        // `Box<dyn RadarBackend>` doesn't implement `Debug`, so we can't use
        // the convenience `.unwrap_err()` here — match the result manually.
        match auto_backend_for_bytes(b"GARBAGE!") {
            Ok(_) => panic!("expected InvalidFormat, got Ok"),
            Err(crate::RadishError::InvalidFormat(_)) => (),
            Err(other) => panic!("expected InvalidFormat, got {other:?}"),
        }
    }

    #[test]
    #[cfg(feature = "native")]
    fn cfradial1_default_read_bytes_returns_unsupported() {
        // Documents the contract: cfradial1 accepts the bytes-sniff but
        // decoding from a buffer is not supported (libnetcdf needs a file).
        let backend = CfRadial1Backend::new();
        let err = backend
            .read_bytes_volume(b"\x89HDF\r\n\x1a\n".to_vec())
            .unwrap_err();
        match err {
            crate::RadishError::Unsupported(msg) => assert!(msg.contains("cfradial1")),
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }
}
