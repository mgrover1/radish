//! Shared format-detection scaffolding.
//!
//! Every backend's `RadarBackend::can_read` / `can_read_bytes` boils down
//! to the same three checks:
//!
//! 1. Does the path's extension match a known list?
//! 2. Does the file (or buffer's prefix) start with a known magic prefix?
//! 3. Does the basename match a canonical filename pattern?
//!
//! [`SniffConfig`] bundles those three signals; [`looks_like`] /
//! [`looks_like_bytes`] are the OR-them-together drivers. Backends declare
//! a `static SNIFF_CONFIG: SniffConfig = SniffConfig { ... }` and delegate
//! their `can_read*` methods to one-liners that pass that config in.
//!
//! The path-based check reads the first `MAGIC_PEEK_BYTES` of the file
//! when no extension/pattern hits — same as the Python side's behaviour
//! before the refactor. Errors (file not found, permission denied) just
//! return `false`; the path simply doesn't look like one of our formats.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// How many bytes to read from the head of a file when sniffing magic on
/// disk. 128 — not just the longest fixed-offset magic (HDF5 = 8 bytes) —
/// because NEXRAD Level 3's `head_pattern` scans a WMO/AWIPS text header
/// for a 6-character product token that can sit anywhere in the first 128
/// bytes (a NOAAPORT-style prefix line can precede it; see
/// `docs/NEXRAD_LEVEL3_WASM.md` §6). Every `head_pattern` implementation
/// must already tolerate a shorter slice (the in-memory
/// `auto_backend_for_bytes` path hands over whatever the caller has), so
/// widening this disk-read peek is backward compatible.
const MAGIC_PEEK_BYTES: usize = 128;

/// Per-backend sniff configuration. Construct as a `static` with hard-coded
/// `&'static` arrays / function pointers so the runtime cost is one slice
/// scan plus an optional file-head read.
#[derive(Clone, Copy)]
pub(crate) struct SniffConfig {
    /// Lower-case file extensions (without the leading dot) that this
    /// backend handles. Comparison is ASCII-case-insensitive.
    pub extensions: &'static [&'static str],
    /// Magic byte prefixes any of which would indicate this backend's
    /// format. Each prefix may be of any length; matching is `head.starts_with`.
    pub magic_prefixes: &'static [&'static [u8]],
    /// Optional filename-pattern test — used when the file lacks an
    /// extension altogether (e.g. NEXRAD's `KLOT20260310_231412_V06`).
    pub filename_pattern: Option<fn(&Path) -> bool>,
    /// Optional head-bytes test for magics that are not plain prefixes
    /// (e.g. NEXRAD real-time chunk records: a 4-byte LDM control word
    /// and *then* `BZh`). Receives up to [`MAGIC_PEEK_BYTES`] from the
    /// head — implementations must handle shorter slices.
    pub head_pattern: Option<fn(&[u8]) -> bool>,
}

/// Path-based sniff: extension match → magic-byte read → filename pattern.
/// Any positive signal wins; everything else is `false`.
pub(crate) fn looks_like(path: &Path, cfg: &SniffConfig) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if cfg.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            return true;
        }
    }
    if let Some(pattern) = cfg.filename_pattern {
        if pattern(path) {
            return true;
        }
    }
    if !cfg.magic_prefixes.is_empty() || cfg.head_pattern.is_some() {
        if let Ok(mut file) = File::open(path) {
            let mut buf = [0u8; MAGIC_PEEK_BYTES];
            if let Ok(n) = file.read(&mut buf) {
                if looks_like_bytes(&buf[..n], cfg) {
                    return true;
                }
            }
        }
    }
    false
}

/// In-memory sniff: magic-prefix check plus the optional head pattern.
/// Path-only signals (extension, filename) don't apply to a buffer.
pub(crate) fn looks_like_bytes(head: &[u8], cfg: &SniffConfig) -> bool {
    cfg.magic_prefixes
        .iter()
        .any(|m| head.len() >= m.len() && &head[..m.len()] == *m)
        || cfg.head_pattern.is_some_and(|pattern| pattern(head))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn nexrad_pat(path: &Path) -> bool {
        // Mimic NEXRAD's canonical pattern (4 upper + 8 digits + _ + 6 digits)
        // just to cover the function-pointer branch in the smoke test.
        let n = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        n.len() >= 19
            && n.as_bytes()[..4].iter().all(|b| b.is_ascii_uppercase())
            && n.as_bytes()[4..12].iter().all(|b| b.is_ascii_digit())
            && n.as_bytes()[12] == b'_'
            && n.as_bytes()[13..19].iter().all(|b| b.is_ascii_digit())
    }

    #[test]
    fn extension_match_short_circuits_on_first_hit() {
        let cfg = SniffConfig {
            extensions: &["raw", "ar2v"],
            magic_prefixes: &[],
            filename_pattern: None,
            head_pattern: None,
        };
        assert!(looks_like(&PathBuf::from("/tmp/whatever.RAW"), &cfg));
        assert!(looks_like(&PathBuf::from("/tmp/whatever.ar2v"), &cfg));
        assert!(!looks_like(&PathBuf::from("/tmp/whatever.txt"), &cfg));
    }

    #[test]
    fn filename_pattern_recognises_canonical_name_without_extension() {
        let cfg = SniffConfig {
            extensions: &[],
            magic_prefixes: &[],
            filename_pattern: Some(nexrad_pat),
            head_pattern: None,
        };
        assert!(looks_like(
            &PathBuf::from("/data/KLOT20260310_231412_V06"),
            &cfg
        ));
        assert!(!looks_like(&PathBuf::from("/data/foo.txt"), &cfg));
    }

    #[test]
    fn looks_like_bytes_matches_first_prefix_only() {
        let cfg = SniffConfig {
            extensions: &[],
            magic_prefixes: &[b"AR2V", b"\x1f\x8b"],
            filename_pattern: None,
            head_pattern: None,
        };
        assert!(looks_like_bytes(b"AR2V0006.001", &cfg));
        assert!(looks_like_bytes(&[0x1f, 0x8b, 0x08, 0x00], &cfg));
        assert!(!looks_like_bytes(b"\x89HDF\r\n\x1a\n", &cfg));
        // Short buffer, no panic
        assert!(!looks_like_bytes(b"AR", &cfg));
        assert!(!looks_like_bytes(b"", &cfg));
    }

    #[test]
    fn missing_file_with_no_extension_returns_false() {
        let cfg = SniffConfig {
            extensions: &[],
            magic_prefixes: &[b"AR2V"],
            filename_pattern: None,
            head_pattern: None,
        };
        assert!(!looks_like(&PathBuf::from("/no/such/file"), &cfg));
    }

    /// Magic-byte sniff actually reads from disk. Pin the contract: a file
    /// whose name and extension don't match any signal but whose first
    /// bytes do, is recognised. Mirrors the `KLOT...` extension-less path
    /// the NEXRAD backend handles in production.
    #[test]
    fn magic_byte_read_from_disk_recognises_known_prefix() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        // Bytes the sniff should match (AR2V + filler).
        tmp.write_all(b"AR2V0006.001\x00\x00\x00\x00")
            .expect("write");
        tmp.flush().expect("flush");
        let cfg = SniffConfig {
            // Deliberately empty extensions + no filename pattern: only the
            // disk-read magic check can succeed here.
            extensions: &[],
            magic_prefixes: &[b"AR2V"],
            filename_pattern: None,
            head_pattern: None,
        };
        assert!(looks_like(tmp.path(), &cfg));
    }

    #[test]
    fn magic_byte_read_from_disk_rejects_unknown_prefix() {
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        tmp.write_all(b"GARBAGE!").expect("write");
        tmp.flush().expect("flush");
        let cfg = SniffConfig {
            extensions: &[],
            magic_prefixes: &[b"AR2V", b"\x89HDF"],
            filename_pattern: None,
            head_pattern: None,
        };
        assert!(!looks_like(tmp.path(), &cfg));
    }

    #[test]
    fn magic_byte_short_file_does_not_panic() {
        // File shorter than the longest configured magic — must not crash.
        use std::io::Write;
        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        tmp.write_all(b"AR").expect("write");
        tmp.flush().expect("flush");
        let cfg = SniffConfig {
            extensions: &[],
            magic_prefixes: &[b"AR2V"],
            filename_pattern: None,
            head_pattern: None,
        };
        assert!(!looks_like(tmp.path(), &cfg));
    }
}
