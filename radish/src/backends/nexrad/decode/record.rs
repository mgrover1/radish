//! LDM (Local Data Manager) record splitting + bzip2 decompression,
//! plus raw-CTM (pre-Build-12) 2432-byte frame walking.
//!
//! Build-12+ files (March 2012 onward) are written as:
//!
//! ```text
//! [optional 24-byte Volume Header]
//! [LDM record N]:  i32 size_be  ||  bzip2-compressed message stream
//! [LDM record N+1]: ...
//! ```
//!
//! Pre-Build-12 files (1991-March 2012) skip the LDM wrapper and
//! store messages as raw 2432-byte CTM frames back-to-back after the
//! 24-byte volume header. Detection: `u32_be(bytes[24..28]) == 0`
//! → raw; non-zero → LDM size prefix. (Matches xradar's
//! `nexrad_level2.py:309-319` and danielway/nexrad's
//! `volume/record.rs:139-156`.)
//!
//! The LDM `size_be` prefix is the compressed-payload length. A
//! negative sign-bit signals a "control" record (per ICD §3.5) —
//! we treat its absolute value as the size and decompress
//! unconditionally; the decompressed payload is normal NEXRAD
//! messages either way.
//!
//! The decompression path is wired through `backends::common::par_map`
//! (rayon under the `native` feature, serial on wasm) so we preserve the
//! ~6× speedup that `nexrad-data/parallel` provides upstream on native
//! builds (see `CLAUDE.md`'s performance gotcha note), while still
//! compiling for `wasm32-unknown-unknown`.

use std::io::Read;

use byteorder::{BigEndian, ByteOrder};
use bzip2::read::BzDecoder;

use crate::backends::common::par_map;

use super::error::{NexradDecodeError, Result};

/// Width of the optional AR2V volume header (ICD §3.4.1).
const VOLUME_HEADER_SIZE: usize = 24;

/// One LDM record's compressed payload, as a borrowed slice into the
/// caller's `&[u8]`. `offset` is the start of the size-prefix in the
/// original file, surfaced in errors so a caller can pinpoint a
/// specific bad record.
#[derive(Debug, Clone)]
pub(crate) struct LdmRecord<'a> {
    pub(crate) offset: usize,
    pub(crate) compressed: &'a [u8],
}

/// Split an LDM-formatted byte stream into a vector of records,
/// already pointing at each record's compressed payload. The
/// optional 24-byte Volume Header (if present at byte 0) is
/// **skipped**; callers parse it separately via
/// `volume::parse_volume_header(&bytes[..24])`.
///
/// LDM detection: the first 24 bytes look like a Volume Header if
/// they start with `AR2V` (per ICD §3.4.1). Otherwise we assume the
/// file starts directly at an LDM size-prefix.
pub(crate) fn split_ldm_records(bytes: &[u8]) -> Result<Vec<LdmRecord<'_>>> {
    let mut pos = if bytes.len() >= 4 && &bytes[..4] == b"AR2V" {
        24
    } else {
        0
    };

    if bytes.len() < pos + 4 {
        return Err(NexradDecodeError::UnsupportedRecordFormat);
    }

    let mut records = Vec::new();
    while pos + 4 <= bytes.len() {
        let raw_size = BigEndian::read_i32(&bytes[pos..pos + 4]);
        let size = raw_size.unsigned_abs() as usize;
        let payload_start = pos + 4;
        let payload_end =
            payload_start
                .checked_add(size)
                .ok_or(NexradDecodeError::MalformedHeader {
                    offset: pos,
                    reason: "LDM record size overflow",
                })?;
        if payload_end > bytes.len() {
            return Err(NexradDecodeError::UnexpectedEof {
                offset: pos,
                needed: 4 + size,
                available: bytes.len().saturating_sub(pos),
            });
        }
        records.push(LdmRecord {
            offset: pos,
            compressed: &bytes[payload_start..payload_end],
        });
        pos = payload_end;
    }
    Ok(records)
}

/// Decompress a single LDM record's payload. Returns the
/// uncompressed message stream as an owned `Vec<u8>`.
pub(crate) fn decompress(record: &LdmRecord<'_>) -> Result<Vec<u8>> {
    let mut decoder = BzDecoder::new(record.compressed);
    // Most modern records decompress to ~250 KB; preallocate to
    // dodge a couple of `Vec` regrowths on the hot path.
    let mut out = Vec::with_capacity(record.compressed.len() * 4);
    decoder
        .read_to_end(&mut out)
        .map_err(|source| NexradDecodeError::Decompression {
            offset: record.offset,
            source,
        })?;
    Ok(out)
}

/// Decompress every record in parallel via rayon. The output vector
/// preserves input order — `decompress_all(records)[i]` corresponds
/// to `records[i]`.
pub(crate) fn decompress_all(records: &[LdmRecord<'_>]) -> Result<Vec<Vec<u8>>> {
    par_map(records, decompress)
}

/// True when `bytes` look like a pre-Build-12 raw Archive II file:
/// AR2V volume header followed by zero `u32_be` at offset 24
/// (i.e. **not** an LDM size prefix). Returns false for any input
/// missing the AR2V magic — those are routed to LDM by default.
pub(crate) fn is_raw_archive2(bytes: &[u8]) -> bool {
    bytes.len() >= VOLUME_HEADER_SIZE + 4
        && &bytes[..4] == b"AR2V"
        && BigEndian::read_u32(&bytes[VOLUME_HEADER_SIZE..VOLUME_HEADER_SIZE + 4]) == 0
}

/// Return the message-stream body of a raw Archive II buffer (everything
/// after the 24-byte volume header).
///
/// Pre-Build-12 files **don't** chunk their body into independent
/// 2432-byte frames the way one might naively expect from the
/// segment-frame size — `danielway/nexrad`'s `split_ctm_frames`
/// (`volume/record.rs`) returns the entire body as one contiguous
/// record, and the message-walking loop relies on each Table II
/// header's declared size + framing flags to find the next message.
/// Mirroring that behavior is the only way the pre-existing
/// segmented + variable-length dispatch reaches the actual MSG_1
/// radials, which often share frames or span them.
pub(crate) fn raw_archive2_body(bytes: &[u8]) -> Result<&[u8]> {
    if bytes.len() < VOLUME_HEADER_SIZE {
        return Err(NexradDecodeError::UnexpectedEof {
            offset: 0,
            needed: VOLUME_HEADER_SIZE,
            available: bytes.len(),
        });
    }
    Ok(&bytes[VOLUME_HEADER_SIZE..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use bzip2::write::BzEncoder;
    use bzip2::Compression;
    use std::io::Write;

    /// Wrap `payload` as one LDM record (size-prefixed, bzip2-compressed).
    fn make_ldm_record(payload: &[u8]) -> Vec<u8> {
        let mut compressed = Vec::new();
        let mut enc = BzEncoder::new(&mut compressed, Compression::default());
        enc.write_all(payload).unwrap();
        enc.finish().unwrap();
        let mut frame = Vec::with_capacity(4 + compressed.len());
        frame.extend_from_slice(&(compressed.len() as i32).to_be_bytes());
        frame.extend_from_slice(&compressed);
        frame
    }

    #[test]
    fn split_one_record_with_no_volume_header() {
        let payload = b"hello, world";
        let frame = make_ldm_record(payload);
        let records = split_ldm_records(&frame).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].offset, 0);
    }

    #[test]
    fn split_skips_ar2v_volume_header() {
        let payload = b"hello";
        let frame = make_ldm_record(payload);
        let mut bytes = b"AR2V0006.001-XYZWXYZWXYZW".to_vec(); // 24 bytes
        bytes.truncate(24);
        bytes.extend_from_slice(&frame);
        let records = split_ldm_records(&bytes).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].offset, 24);
    }

    #[test]
    fn split_multiple_records_preserves_order() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&make_ldm_record(b"first"));
        let off1 = bytes.len();
        bytes.extend_from_slice(&make_ldm_record(b"second"));
        let off2 = bytes.len();
        bytes.extend_from_slice(&make_ldm_record(b"third"));
        let records = split_ldm_records(&bytes).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].offset, 0);
        assert_eq!(records[1].offset, off1);
        assert_eq!(records[2].offset, off2);
    }

    #[test]
    fn split_handles_negative_size_control_record() {
        // ICD §3.5: a negative sign-bit on the size signals a control
        // record, but the absolute value is still the payload length.
        let payload = b"control-record-payload";
        let mut compressed = Vec::new();
        let mut enc = BzEncoder::new(&mut compressed, Compression::default());
        enc.write_all(payload).unwrap();
        enc.finish().unwrap();
        let mut frame = Vec::with_capacity(4 + compressed.len());
        let signed_size = -(compressed.len() as i32);
        frame.extend_from_slice(&signed_size.to_be_bytes());
        frame.extend_from_slice(&compressed);
        let records = split_ldm_records(&frame).unwrap();
        assert_eq!(records.len(), 1);
        // Decompresses to the original payload regardless of sign.
        let out = decompress(&records[0]).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn split_errors_on_truncated_payload() {
        let mut frame = make_ldm_record(b"hello");
        frame.truncate(frame.len() - 4);
        let err = split_ldm_records(&frame).unwrap_err();
        assert!(
            matches!(err, NexradDecodeError::UnexpectedEof { .. }),
            "expected UnexpectedEof, got {err:?}"
        );
    }

    #[test]
    fn decompress_round_trips_payload() {
        let payload = b"the quick brown fox jumps over the lazy dog";
        let frame = make_ldm_record(payload);
        let records = split_ldm_records(&frame).unwrap();
        let out = decompress(&records[0]).unwrap();
        assert_eq!(out, payload);
    }

    #[test]
    fn decompress_all_preserves_order() {
        let payloads: Vec<&[u8]> = vec![b"alpha", b"bravo", b"charlie", b"delta"];
        let mut bytes = Vec::new();
        for p in &payloads {
            bytes.extend_from_slice(&make_ldm_record(p));
        }
        let records = split_ldm_records(&bytes).unwrap();
        let outs = decompress_all(&records).unwrap();
        for (i, want) in payloads.iter().enumerate() {
            assert_eq!(&outs[i][..], *want, "record {i} order");
        }
    }

    #[test]
    fn is_raw_archive2_detects_zero_size_prefix() {
        // 24-byte AR2V header + 4 zero bytes → raw CTM signal.
        let mut bytes = b"AR2V0006.001-XYZWXYZWXYZW".to_vec();
        bytes.truncate(24);
        bytes.extend_from_slice(&[0u8; 4]);
        assert!(is_raw_archive2(&bytes));
    }

    #[test]
    fn is_raw_archive2_rejects_ldm_size_prefix() {
        // 24-byte AR2V header + non-zero size → LDM-wrapped.
        let mut bytes = b"AR2V0006.001-XYZWXYZWXYZW".to_vec();
        bytes.truncate(24);
        bytes.extend_from_slice(&123_456_i32.to_be_bytes());
        assert!(!is_raw_archive2(&bytes));
    }

    #[test]
    fn is_raw_archive2_rejects_inputs_without_ar2v_magic() {
        let bytes = vec![0u8; 64];
        assert!(!is_raw_archive2(&bytes));
    }

    #[test]
    fn is_raw_archive2_rejects_short_inputs() {
        let bytes = b"AR2V0006".to_vec(); // 8 bytes, no room for the size field
        assert!(!is_raw_archive2(&bytes));
    }

    #[test]
    fn raw_archive2_body_skips_volume_header() {
        let mut bytes = b"AR2V0006.001-XYZWXYZWXYZW".to_vec();
        bytes.truncate(24);
        bytes.extend(vec![0xAAu8; 100]);
        let body = raw_archive2_body(&bytes).unwrap();
        assert_eq!(body.len(), 100);
        assert!(body.iter().all(|&b| b == 0xAA));
    }

    #[test]
    fn raw_archive2_body_errors_when_volume_header_missing() {
        let bytes = vec![0u8; 8];
        assert!(matches!(
            raw_archive2_body(&bytes).unwrap_err(),
            NexradDecodeError::UnexpectedEof { .. }
        ));
    }

    #[test]
    fn decompress_errors_on_bad_bzip2_stream() {
        // Construct a "record" whose payload isn't valid bzip2.
        let bytes = {
            let payload = vec![0xFFu8; 32];
            let mut frame = Vec::with_capacity(4 + payload.len());
            frame.extend_from_slice(&(payload.len() as i32).to_be_bytes());
            frame.extend_from_slice(&payload);
            frame
        };
        let records = split_ldm_records(&bytes).unwrap();
        assert!(matches!(
            decompress(&records[0]).unwrap_err(),
            NexradDecodeError::Decompression { .. }
        ));
    }
}
