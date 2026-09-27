//! Format detection for NEXRAD Level 2 (Archive II / AR2V) files.
//!
//! Three signals callers can OR together (extension, magic, canonical
//! filename pattern) — same shape as every other backend, so the actual
//! pipeline lives in `crate::backends::common::sniff`. This module just
//! declares the NEXRAD-specific config and the strict ICAO extractor that
//! `adapter.rs` reuses outside the sniff path.

use std::path::Path;

use crate::backends::common::{looks_like, looks_like_bytes, SniffConfig};

/// Length of the canonical filename core: 4 ICAO + 8 date + `_` + 6 time.
const NEXRAD_NAME_CORE_LEN: usize = 19;
/// Length of the canonical filename plus a `_V##` suffix.
const NEXRAD_NAME_WITH_VOLUME_LEN: usize = 23;

/// File extensions for NEXRAD Level 2 archive files.
pub(crate) const EXTENSIONS: &[&str] = &["ar2", "ar2v"];

/// Volume-header magic at byte 0 of every Archive II file.
pub(crate) const AR2V_MAGIC: &[u8; 4] = b"AR2V";

/// Gzip header magic — older Archive II volumes (e.g. pre-2016 `*.gz` files
/// from NOAA's archive bucket) wrap the AR2V buffer with gzip compression.
/// Either prefix is enough to identify the buffer as a NEXRAD volume because
/// the upstream `File::decompress()` transparently inflates gzip-wrapped data.
pub(crate) const GZIP_MAGIC: &[u8; 2] = &[0x1f, 0x8b];

/// Centralised sniff config consumed by the common dispatcher. Adding a new
/// signal (more extensions, a different magic) is a one-line edit here.
///
/// `const` rather than `static` because `SniffConfig` is `Copy` and the
/// literal contains only `&'static` references plus a function pointer —
/// the compile-time form makes it explicit that no addressable storage
/// lives at runtime.
pub(crate) const NEXRAD_SNIFF: SniffConfig = SniffConfig {
    extensions: EXTENSIONS,
    magic_prefixes: &[AR2V_MAGIC, GZIP_MAGIC],
    filename_pattern: Some(matches_nexrad_filename),
    head_pattern: Some(looks_like_chunk_bytes),
};

/// Returns `true` if the path's file name matches the canonical NEXRAD naming
/// convention (e.g. `KLOT20260310_231412_V06`) or the real-time chunk object
/// convention (e.g. `20260328-201457-015-I`).
pub(crate) fn matches_nexrad_filename(path: &Path) -> bool {
    nexrad_icao_from_name(path).is_some() || matches_chunk_object_name(path)
}

/// Real-time chunk record magic: a 4-byte big-endian LDM control word (the
/// record's compressed size; the sign bit marks a volume's last record)
/// followed by a bzip2 stream header `BZh<level>`. Headerless `I`/`E`
/// objects from the `unidata-nexrad-level2-chunks` bucket start exactly
/// like this — no `AR2V` prefix for the plain-magic path to hit.
///
/// Stronger than the upstream `nexrad-data` check (`data[4..6] == "BZ"`),
/// which runs in a chunk-only context: radish sniffs arbitrary buffers, so
/// we also require the bzip2 level digit and a plausible record size.
/// Real compressed LDM records run from a few hundred bytes (`S`-chunk
/// metadata records) to a few MB.
pub(crate) fn looks_like_chunk_bytes(head: &[u8]) -> bool {
    if head.len() < 8 || &head[4..7] != b"BZh" || !head[7].is_ascii_digit() {
        return false;
    }
    let size = i32::from_be_bytes([head[0], head[1], head[2], head[3]]).unsigned_abs();
    (64..=16 * 1024 * 1024).contains(&size)
}

/// `YYYYMMDD-HHMMSS-NNN-[SIE]` — the object naming convention of the
/// `unidata-nexrad-level2-chunks` bucket (volume start time, 1-based
/// chunk sequence number, chunk type).
fn matches_chunk_object_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let b = name.as_bytes();
    b.len() == 21
        && b[..8].iter().all(|c| c.is_ascii_digit())
        && b[8] == b'-'
        && b[9..15].iter().all(|c| c.is_ascii_digit())
        && b[15] == b'-'
        && b[16..19].iter().all(|c| c.is_ascii_digit())
        && b[19] == b'-'
        && matches!(b[20], b'S' | b'I' | b'E')
}

/// Extracts the 4-letter ICAO from a path whose file name matches the
/// canonical NEXRAD naming convention. Returns `None` for any non-matching
/// name (this is the strict counterpart to a regex match — there is no
/// "best effort" path that could leak a non-NEXRAD prefix as an ICAO).
pub(crate) fn icao_from_filename(path: &Path) -> Option<&str> {
    nexrad_icao_from_name(path)
}

fn nexrad_icao_from_name(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    let bytes = name.as_bytes();
    if bytes.len() != NEXRAD_NAME_CORE_LEN && bytes.len() != NEXRAD_NAME_WITH_VOLUME_LEN {
        return None;
    }
    // `AAAAYYYYMMDD_HHMMSS` core; optional `_VNN` tail.
    let core_ok = bytes[..4].iter().all(|&b| b.is_ascii_uppercase())
        && bytes[4..12].iter().all(|&b| b.is_ascii_digit())
        && bytes[12] == b'_'
        && bytes[13..19].iter().all(|&b| b.is_ascii_digit());
    if !core_ok {
        return None;
    }
    if bytes.len() == NEXRAD_NAME_WITH_VOLUME_LEN {
        let tail_ok = bytes[19] == b'_'
            && bytes[20] == b'V'
            && bytes[21..23].iter().all(|&b| b.is_ascii_digit());
        if !tail_ok {
            return None;
        }
    }
    // Safe: we just verified the first four bytes are ASCII uppercase, so the
    // string slice falls on a UTF-8 boundary.
    Some(&name[..4])
}

/// Combined check: extension OR magic OR filename pattern. One-line delegate
/// to the shared `common::sniff::looks_like` driver.
pub(crate) fn looks_like_nexrad(path: &Path) -> bool {
    looks_like(path, &NEXRAD_SNIFF)
}

/// In-memory magic-byte check (raw `AR2V` or gzip-wrapped). Delegates to the
/// shared driver; kept as its own function so other modules in the NEXRAD
/// backend can call it without going through `NEXRAD_SNIFF` indirection.
pub(crate) fn looks_like_ar2v_bytes(head: &[u8]) -> bool {
    looks_like_bytes(head, &NEXRAD_SNIFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn filename_pattern_matches_canonical() {
        assert!(matches_nexrad_filename(&PathBuf::from(
            "KLOT20260310_231412_V06"
        )));
        assert!(matches_nexrad_filename(&PathBuf::from(
            "/some/path/KATX20230520_201643"
        )));
    }

    #[test]
    fn filename_pattern_rejects_non_nexrad() {
        assert!(!matches_nexrad_filename(&PathBuf::from("foo.nc")));
        assert!(!matches_nexrad_filename(&PathBuf::from(
            "klot20260310_231412_V06" // lowercase prefix
        )));
        assert!(!matches_nexrad_filename(&PathBuf::from(
            "KLOT20260310-231412" // wrong separator
        )));
    }

    #[test]
    fn icao_from_filename_extracts_klot() {
        assert_eq!(
            icao_from_filename(&PathBuf::from("/x/KLOT20260310_231412_V06")),
            Some("KLOT")
        );
        assert_eq!(
            icao_from_filename(&PathBuf::from("KATX20230520_201643")),
            Some("KATX")
        );
    }

    #[test]
    fn icao_from_filename_rejects_non_nexrad() {
        assert_eq!(icao_from_filename(&PathBuf::from("klot...")), None);
        assert_eq!(icao_from_filename(&PathBuf::from("AAAAfoo")), None);
        assert_eq!(icao_from_filename(&PathBuf::from("KATX-not-nexrad")), None);
    }

    #[test]
    fn looks_like_returns_false_for_missing_unknown_file() {
        // Path doesn't exist and has no NEXRAD-like name; the shared
        // dispatcher's three signals all fall through cleanly.
        assert!(!looks_like_nexrad(&PathBuf::from("/no/such/file/here.txt")));
    }

    #[test]
    fn chunk_object_names_match() {
        assert!(matches_nexrad_filename(&PathBuf::from(
            "/vol/20260328-201457-015-I"
        )));
        assert!(matches_nexrad_filename(&PathBuf::from(
            "20260328-201457-001-S"
        )));
        assert!(matches_nexrad_filename(&PathBuf::from(
            "20260328-201457-055-E"
        )));
        // Wrong type letter, wrong separators, wrong lengths.
        assert!(!matches_nexrad_filename(&PathBuf::from(
            "20260328-201457-055-X"
        )));
        assert!(!matches_nexrad_filename(&PathBuf::from(
            "20260328_201457_055_I"
        )));
        assert!(!matches_nexrad_filename(&PathBuf::from(
            "2026028-201457-055-I"
        )));
    }

    #[test]
    fn chunk_bytes_magic_requires_control_word_and_bzh() {
        // Real `I`-chunk head from the KLOT fixture: size 0x00015f05,
        // then the bzip2 stream header.
        assert!(looks_like_chunk_bytes(&[
            0x00, 0x01, 0x5f, 0x05, b'B', b'Z', b'h', b'9'
        ]));
        // Negative control word (`E` chunk, last record of the volume).
        assert!(looks_like_chunk_bytes(&[
            0xff, 0xff, 0x7b, 0xb8, b'B', b'Z', b'h', b'9'
        ]));
        // `BZh` at offset 4 but an absurd record size — rejected.
        assert!(!looks_like_chunk_bytes(&[
            0x7f, 0xff, 0xff, 0xff, b'B', b'Z', b'h', b'9'
        ]));
        // Bare bzip2 stream (no control word) — not a chunk record.
        assert!(!looks_like_chunk_bytes(b"BZh91AY&SY"));
        // Missing level digit / short buffers — rejected, no panic.
        assert!(!looks_like_chunk_bytes(&[
            0x00, 0x01, 0x5f, 0x05, b'B', b'Z', b'h', b'!'
        ]));
        assert!(!looks_like_chunk_bytes(&[0x00, 0x01, 0x5f]));
        assert!(!looks_like_chunk_bytes(b""));
        // And the sniff-config plumbing routes it through the shared
        // in-memory driver.
        assert!(looks_like_ar2v_bytes(&[
            0x00, 0x01, 0x5f, 0x05, b'B', b'Z', b'h', b'9'
        ]));
    }

    #[test]
    fn looks_like_ar2v_bytes_accepts_raw_and_gzipped() {
        // Raw Archive II header
        assert!(looks_like_ar2v_bytes(b"AR2V0006.001"));
        // Gzip-wrapped volume (e.g. pre-2016 `*.gz` archive files)
        assert!(looks_like_ar2v_bytes(&[0x1f, 0x8b, 0x08, 0x00]));
        // Garbage / netCDF / arbitrary
        assert!(!looks_like_ar2v_bytes(b"\x89HDF\r\n\x1a\n"));
        assert!(!looks_like_ar2v_bytes(b"CDF\x01"));
        assert!(!looks_like_ar2v_bytes(b"hello"));
        // Short buffers don't panic
        assert!(!looks_like_ar2v_bytes(b""));
        assert!(!looks_like_ar2v_bytes(b"AR"));
    }
}
