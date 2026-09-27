//! Optional 24-byte Volume Header (ICD §3.4.1).
//!
//! Modern Archive II files start with this; some sources (truncated
//! chunk uploads, pre-Build-19 archives) omit it. The decoder
//! tolerates absence — tries to parse the leading 24 bytes, falls
//! back to assuming an LDM record stream if the magic doesn't match.

use byteorder::{BigEndian, ByteOrder};

/// Decoded Volume Header. All fields are big-endian on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VolumeHeader {
    /// `AR2V` magic plus a 5-byte version (e.g. `AR2V0006`).
    pub(crate) tape_filename: [u8; 9],
    /// Pre-pad space.
    pub(crate) extension_number: [u8; 3],
    /// Days since Jan 1, 1970 — the file's nominal collection date.
    pub(crate) modified_julian_date: u32,
    /// Milliseconds past midnight of `modified_julian_date`.
    pub(crate) milliseconds: u32,
    /// 4-byte ASCII ICAO identifier (e.g. `KLOT`).
    pub(crate) icao: [u8; 4],
}

/// Try to parse the leading 24 bytes as a Volume Header. Returns
/// `None` if the magic bytes don't match `AR2V` — caller should
/// treat the whole input as an LDM record stream.
pub(crate) fn parse(bytes: &[u8]) -> Option<VolumeHeader> {
    if bytes.len() < 24 || &bytes[..4] != b"AR2V" {
        return None;
    }
    let mut tape_filename = [0u8; 9];
    tape_filename.copy_from_slice(&bytes[0..9]);
    let mut extension_number = [0u8; 3];
    extension_number.copy_from_slice(&bytes[9..12]);
    let modified_julian_date = BigEndian::read_u32(&bytes[12..16]);
    let milliseconds = BigEndian::read_u32(&bytes[16..20]);
    let mut icao = [0u8; 4];
    icao.copy_from_slice(&bytes[20..24]);
    Some(VolumeHeader {
        tape_filename,
        extension_number,
        modified_julian_date,
        milliseconds,
        icao,
    })
}

impl VolumeHeader {
    /// ICAO identifier as a UTF-8 string. Returns the raw bytes
    /// lossily — NEXRAD ICAOs are pure ASCII so this is safe in
    /// practice.
    pub(crate) fn icao_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.icao)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_header(icao: &[u8; 4], jd: u32, ms: u32) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(24);
        bytes.extend_from_slice(b"AR2V0006.");
        bytes.extend_from_slice(&[0x30, 0x30, 0x31]); // extension_number "001"
        bytes.extend_from_slice(&jd.to_be_bytes());
        bytes.extend_from_slice(&ms.to_be_bytes());
        bytes.extend_from_slice(icao);
        debug_assert_eq!(bytes.len(), 24);
        bytes
    }

    #[test]
    fn parse_returns_some_for_valid_header() {
        let bytes = fake_header(b"KLOT", 19_000, 12_345_678);
        let header = parse(&bytes).expect("valid header should parse");
        assert_eq!(&header.tape_filename, b"AR2V0006.");
        assert_eq!(header.modified_julian_date, 19_000);
        assert_eq!(header.milliseconds, 12_345_678);
        assert_eq!(&header.icao, b"KLOT");
        assert_eq!(header.icao_str(), "KLOT");
    }

    #[test]
    fn parse_returns_none_for_missing_magic() {
        let bytes = vec![0u8; 24];
        assert!(parse(&bytes).is_none());
    }

    #[test]
    fn parse_returns_none_for_short_input() {
        let bytes = b"AR2V0006".to_vec();
        assert!(parse(&bytes).is_none());
    }
}
