//! Format detection for IRIS / Sigmet RAW files.
//!
//! IRIS RAW has no canonical filename pattern (sites use varied
//! extensions like `.RAW`, `.RAWF3W5`, `.RAWE5P1`), so detection relies
//! on the 2-byte little-endian `structure_identifier` at offset 0:
//!
//! * `23` (0x0017) = `INGEST_HEADER` — bare ingest, common for live capture
//! * `27` (0x001B) = `PRODUCT_HDR`   — wrapped, common for post-processed files
//!
//! Both are valid IRIS RAW files; we accept either.

use std::path::Path;

use crate::backends::common::{looks_like, looks_like_bytes, SniffConfig};

/// File extensions explicitly registered for IRIS. In practice site-
/// specific extensions like `.RAWF3W5` won't match the list, so the
/// magic-byte branch carries most of the load.
pub(crate) const EXTENSIONS: &[&str] = &["raw", "iris"];

/// `INGEST_HEADER` magic — `structure_identifier = 23` LE.
pub(crate) const INGEST_HEADER_MAGIC: &[u8; 2] = &[0x17, 0x00];
/// `PRODUCT_HDR` magic — `structure_identifier = 27` LE.
pub(crate) const PRODUCT_HDR_MAGIC: &[u8; 2] = &[0x1b, 0x00];

/// Centralised sniff config consumed by the common dispatcher.
pub(crate) const SIGMET_SNIFF: SniffConfig = SniffConfig {
    extensions: EXTENSIONS,
    magic_prefixes: &[INGEST_HEADER_MAGIC, PRODUCT_HDR_MAGIC],
    filename_pattern: None,
    head_pattern: None,
};

/// Combined check: extension OR magic.
pub(crate) fn looks_like_iris(path: &Path) -> bool {
    looks_like(path, &SIGMET_SNIFF)
}

/// In-memory magic-byte check.
pub(crate) fn looks_like_iris_bytes(head: &[u8]) -> bool {
    looks_like_bytes(head, &SIGMET_SNIFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_iris_bytes_accepts_both_magic_ids() {
        // INGEST_HEADER (id=23 LE)
        assert!(looks_like_iris_bytes(&[0x17, 0x00, 0x04, 0x00]));
        // PRODUCT_HDR (id=27 LE)
        assert!(looks_like_iris_bytes(&[0x1b, 0x00, 0x08, 0x00]));
    }

    #[test]
    fn looks_like_iris_bytes_rejects_unrelated_magic() {
        assert!(!looks_like_iris_bytes(b"AR2V"));
        assert!(!looks_like_iris_bytes(b"\x89HDF"));
        assert!(!looks_like_iris_bytes(b"GARBAGE!"));
        assert!(!looks_like_iris_bytes(b""));
        assert!(!looks_like_iris_bytes(b"\x17"));
    }
}
