//! Format detection for CfRadial1 NetCDF files.
//!
//! Two on-disk encodings exist:
//!
//! * **HDF5-backed netCDF-4** — the modern default; magic is the 8-byte
//!   `\x89HDF\r\n\x1a\n` signature shared with raw HDF5.
//! * **Classic netCDF (CDF)** — older files; magic is `CDF\x01` (netCDF-3
//!   classic) or `CDF\x02` (netCDF-3 64-bit offset).
//!
//! We don't try to discriminate CfRadial vs. raw HDF5/netCDF here — that
//! distinction lives in CF attributes (`Conventions = "CF/Radial"`) and is
//! out of reach until `libnetcdf` opens the file. The sniff layer is just
//! an "is this plausibly a netCDF/HDF5 file?" check; if yes, we hand it to
//! the CfRadial1 reader and let that surface a richer error if the file
//! turns out to be a non-CfRadial netCDF.
//!
//! Note: `read_bytes_volume` on the CfRadial1 backend is intentionally
//! **not** supported — `libnetcdf` (and `hdf5-metno`) require a filename
//! and don't expose an in-memory buffer API. The sniff implementation only
//! supports `can_read_bytes` so the central `auto_backend_for_bytes`
//! dispatcher can still classify a buffer; the actual decode then surfaces
//! a clear `RadishError::Unsupported` instead of a confusing parser error.

/// HDF5 (and netCDF-4) magic bytes.
pub(crate) const HDF5_MAGIC: &[u8; 8] = b"\x89HDF\r\n\x1a\n";
/// Classic netCDF magic — netCDF-3 classic format.
pub(crate) const CDF1_MAGIC: &[u8; 4] = b"CDF\x01";
/// Classic netCDF magic — netCDF-3 64-bit offset format.
pub(crate) const CDF2_MAGIC: &[u8; 4] = b"CDF\x02";

/// Returns `true` if `head` starts with any of the netCDF / HDF5 magic
/// signatures. Cheap byte-prefix check; safe on any length of buffer.
pub(crate) fn looks_like_cfradial1(head: &[u8]) -> bool {
    if head.len() >= HDF5_MAGIC.len() && &head[..HDF5_MAGIC.len()] == HDF5_MAGIC {
        return true;
    }
    if head.len() >= 4 && (&head[..4] == CDF1_MAGIC || &head[..4] == CDF2_MAGIC) {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_cfradial1_accepts_hdf5_and_classic_netcdf() {
        assert!(looks_like_cfradial1(b"\x89HDF\r\n\x1a\nrest"));
        assert!(looks_like_cfradial1(b"CDF\x01rest"));
        assert!(looks_like_cfradial1(b"CDF\x02rest"));
    }

    #[test]
    fn looks_like_cfradial1_rejects_non_netcdf() {
        assert!(!looks_like_cfradial1(b"AR2V"));
        assert!(!looks_like_cfradial1(&[0x1f, 0x8b, 0x08, 0x00]));
        assert!(!looks_like_cfradial1(b"hello world"));
        // Short buffers don't panic
        assert!(!looks_like_cfradial1(b""));
        assert!(!looks_like_cfradial1(b"\x89HDF"));
        assert!(!looks_like_cfradial1(b"CDF"));
    }
}
