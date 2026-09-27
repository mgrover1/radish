//! NEXRAD message header — 28 bytes total per ICD 2620002AA on the wire.
//!
//! Two pieces, in order:
//!
//! 1. **TCM Message Header** (ICD §3.1.3) — 12 bytes of session-level
//!    framing (`Message Type` / `Type-Dependent` / `Data Size`). In
//!    Level II Archive files these come through as zero-filled in
//!    practice; danielway's `nexrad-decode` calls them
//!    `rpg_unknown`.
//! 2. **Message Header per Table II** (ICD §3.2.4.1) — the 16-byte
//!    logical header that downstream parsers read for size, type,
//!    and segmentation:
//!
//!    | Offset | Bytes | Field                                          |
//!    |-------:|------:|------------------------------------------------|
//!    |     12 |     2 | Message Size (halfwords; `0xFFFF` = sentinel)  |
//!    |     14 |     1 | RDA Redundant Channel                          |
//!    |     15 |     1 | Message Type (Table I)                         |
//!    |  16-17 |     2 | I.D. Sequence Number                           |
//!    |  18-19 |     2 | Modified Julian Date                           |
//!    |  20-23 |     4 | Milliseconds of Day                            |
//!    |  24-25 |     2 | Number of Message Segments                     |
//!    |  26-27 |     2 | Message Segment Number                         |
//!
//! Per ICD §3.2.4.1 Note 7: when `Message Size == 0xFFFF`, halfwords
//! 12-15 (i.e. `segment_count` and `segment_number`) are repurposed
//! as a 32-bit byte count. The message is assumed to be a single
//! segment (variable-length).
//!
//! `message_size_bytes()` returns the **logical** message size
//! (16-byte Table II header + payload). It excludes the 12-byte TCM
//! prefix — so per-message wire stride for variable-length messages
//! is `12 + message_size_bytes()`.

use super::error::Result;
use super::reader::SliceReader;

/// Combined width of the TCM prefix + Table II logical header.
pub(crate) const SIZE: usize = 28;

/// TCM Message Header width (ICD §3.1.3): 3 × 4-octet fields.
pub(crate) const TCM_PREFIX_SIZE: usize = 12;

/// Table II logical header width (ICD §3.2.4.1).
pub(crate) const LOGICAL_HEADER_SIZE: usize = 16;

const _: () = assert!(SIZE == TCM_PREFIX_SIZE + LOGICAL_HEADER_SIZE);

/// Frame width for fixed-segment (segmented) messages (ICD §3.1.3
/// — the wire-level frame that holds one segment plus padding).
/// Variable-length messages (`segment_size == 0xFFFF` and Type 31)
/// are not bound by this — they consume `12 + message_size_bytes()`
/// bytes per message.
pub(crate) const SEGMENT_FRAME_SIZE: usize = 2432;

/// Sentinel value in `Message Size` that signals the variable-length
/// extended encoding (ICD §3.2.4.1 Note 6 + Note 7).
pub(crate) const VARIABLE_LENGTH_MESSAGE_SIZE: u16 = 0xFFFF;

/// Decoded message header. Fields hold the **post-decode** values
/// (e.g. byte-arithmetic units already converted from halfwords);
/// raw fields are kept where the consumer cares about the wire bits
/// (sentinel detection, segment counting).
///
/// The 12-byte TCM prefix (ICD §3.1.3) is consumed by `read` but
/// not stored — no downstream consumer of radish reads it, and
/// keeping it would add 12 bytes per header for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MessageHeader {
    /// Raw Message Size halfword count from the Table II header
    /// (ICD §3.2.4.1). `0xFFFF` triggers the variable-length
    /// extended size encoding via `segment_count`/`segment_number`.
    /// Use `message_size_bytes()` for byte arithmetic.
    pub(crate) message_size_halfwords: u16,
    pub(crate) channel: u8,
    pub(crate) message_type: MessageType,
    pub(crate) sequence_number: u16,
    pub(crate) julian_date: u16,
    pub(crate) milliseconds: u32,
    pub(crate) segment_count: u16,
    pub(crate) segment_number: u16,
}

impl MessageHeader {
    pub(crate) fn read(reader: &mut SliceReader<'_>) -> Result<Self> {
        // Consume + discard the 12-byte TCM prefix (ICD §3.1.3).
        // We just need the cursor advanced; no consumer reads it.
        reader.advance(TCM_PREFIX_SIZE)?;

        let message_size_halfwords = reader.read_u16_be()?;
        let channel = reader.read_u8()?;
        let type_byte = reader.read_u8()?;
        let message_type = MessageType::from_u8(type_byte);
        let sequence_number = reader.read_u16_be()?;
        let julian_date = reader.read_u16_be()?;
        let milliseconds = reader.read_u32_be()?;
        let segment_count = reader.read_u16_be()?;
        let segment_number = reader.read_u16_be()?;
        Ok(Self {
            message_size_halfwords,
            channel,
            message_type,
            sequence_number,
            julian_date,
            milliseconds,
            segment_count,
            segment_number,
        })
    }

    /// Logical message size in bytes — the Table II header (16) plus
    /// payload, **not** including the 12-byte TCM prefix.
    ///
    /// Per ICD §3.2.4.1 Note 7: when the on-wire halfword field is
    /// `0xFFFF`, the size is reconstructed from
    /// `segment_count`/`segment_number` as a 32-bit byte count. The
    /// message is assumed to be a single segment (variable-length).
    pub(crate) fn message_size_bytes(&self) -> usize {
        if self.message_size_halfwords < VARIABLE_LENGTH_MESSAGE_SIZE {
            usize::from(self.message_size_halfwords) * 2
        } else {
            // Halfwords 12-15 (`segment_count` MSB | `segment_number` LSB)
            // → 32-bit byte count.
            ((u32::from(self.segment_count) << 16) | u32::from(self.segment_number)) as usize
        }
    }

    /// Whether this message uses fixed-segment framing (one or more
    /// 2432-byte frames per ICD §3.1.3) rather than the
    /// variable-length encoding.
    ///
    /// Type 31 (Digital Radar Data Generic Format) is **always**
    /// variable-length even when its `Message Size` halfword field
    /// holds the actual halfword count rather than the `0xFFFF`
    /// sentinel — matching danielway/nexrad's interpretation.
    pub(crate) fn segmented(&self) -> bool {
        self.message_size_halfwords < VARIABLE_LENGTH_MESSAGE_SIZE
            && self.message_type != MessageType::DigitalRadarDataGenericFormat
    }
}

/// Message type codes per ICD 2620002AA Table I. Variants are
/// limited to the ones radish actually consumes; everything else
/// folds into `Skip(u8)` so the loop walks the bytes without
/// pretending we understand them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageType {
    /// Type 1 — legacy radial data (pre-Build-12, March 2012).
    /// Parsed by `messages::msg1::Msg1` (plan 0006).
    DigitalRadarDataLegacy,
    /// Type 2 — RDA Status data.
    RdaStatusData,
    /// Type 5 — Volume Coverage Pattern.
    VolumeCoveragePattern,
    /// Type 15 — Clutter Filter Map (segmented; not consumed by
    /// radish today but emitted by some files).
    ClutterFilterMap,
    /// Type 18 — RDA Adaptation Data.
    RdaAdaptationData,
    /// Type 31 — Generic format radial data (modern, post-2008).
    /// **The variable-length type that Phase 3 will parse.**
    DigitalRadarDataGenericFormat,
    /// Anything else: zero-padding, control records, types we
    /// haven't implemented. The loop walks past these via the
    /// declared message size.
    Skip(u8),
}

impl MessageType {
    pub(crate) fn from_u8(code: u8) -> Self {
        match code {
            1 => Self::DigitalRadarDataLegacy,
            2 => Self::RdaStatusData,
            5 => Self::VolumeCoveragePattern,
            15 => Self::ClutterFilterMap,
            18 => Self::RdaAdaptationData,
            31 => Self::DigitalRadarDataGenericFormat,
            other => Self::Skip(other),
        }
    }
}

impl std::fmt::Display for MessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DigitalRadarDataLegacy => write!(f, "MSG_1 (legacy radial)"),
            Self::RdaStatusData => write!(f, "MSG_2 (RDA status)"),
            Self::VolumeCoveragePattern => write!(f, "MSG_5 (VCP)"),
            Self::ClutterFilterMap => write!(f, "MSG_15 (clutter filter map)"),
            Self::RdaAdaptationData => write!(f, "MSG_18 (RDA adaptation)"),
            Self::DigitalRadarDataGenericFormat => write!(f, "MSG_31 (generic radial)"),
            Self::Skip(code) => write!(f, "MSG_{code} (skipped)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct 28 bytes that decode to a header with the given
    /// type byte, message size halfwords, segment_count, and
    /// segment_number. The 12-byte TCM prefix is zero-filled.
    fn synth(type_byte: u8, halfwords: u16, segment_count: u16, segment_number: u16) -> [u8; 28] {
        let mut bytes = [0u8; 28];
        // bytes 0..12 are the zero-filled TCM prefix.
        bytes[12..14].copy_from_slice(&halfwords.to_be_bytes());
        bytes[14] = 0; // channel
        bytes[15] = type_byte;
        bytes[24..26].copy_from_slice(&segment_count.to_be_bytes());
        bytes[26..28].copy_from_slice(&segment_number.to_be_bytes());
        bytes
    }

    #[test]
    fn message_size_bytes_doubles_halfwords_on_normal_path() {
        let bytes = synth(31, 100, 0, 1);
        let mut r = SliceReader::new(&bytes);
        let h = MessageHeader::read(&mut r).unwrap();
        assert_eq!(h.message_size_bytes(), 200);
    }

    #[test]
    fn message_size_bytes_uses_extended_encoding_at_sentinel() {
        // ICD §3.2.4.1 Note 7: halfword 0 = 0xFFFF, then
        // halfwords 12-15 (segment_count MSB, segment_number LSB)
        // = 32-bit byte count. Pick segment_count=0x0001,
        // segment_number=0x2000 → 0x00012000 = 73728 bytes.
        let bytes = synth(31, VARIABLE_LENGTH_MESSAGE_SIZE, 0x0001, 0x2000);
        let mut r = SliceReader::new(&bytes);
        let h = MessageHeader::read(&mut r).unwrap();
        assert_eq!(h.message_size_bytes(), 0x00012000);
    }

    #[test]
    fn message_type_dispatch_matches_icd_table_i() {
        let cases = [
            (1u8, MessageType::DigitalRadarDataLegacy),
            (2, MessageType::RdaStatusData),
            (5, MessageType::VolumeCoveragePattern),
            (15, MessageType::ClutterFilterMap),
            (18, MessageType::RdaAdaptationData),
            (31, MessageType::DigitalRadarDataGenericFormat),
            (0, MessageType::Skip(0)),
            (3, MessageType::Skip(3)),
            (255, MessageType::Skip(255)),
        ];
        for (code, expected) in cases {
            assert_eq!(MessageType::from_u8(code), expected, "code {code}");
        }
    }

    #[test]
    fn segmented_distinguishes_msg31_and_sentinel() {
        let segmented_for = |type_byte: u8, halfwords: u16, sc: u16, sn: u16| {
            let bytes = synth(type_byte, halfwords, sc, sn);
            let mut r = SliceReader::new(&bytes);
            MessageHeader::read(&mut r).unwrap().segmented()
        };

        // MSG_31 is always variable-length, regardless of the size
        // halfword value (matches danielway/nexrad's interpretation).
        assert!(!segmented_for(31, 100, 0, 1));

        // Other types: variable-length only when sentinel is set.
        assert!(segmented_for(2, 100, 0, 1));
        assert!(!segmented_for(2, VARIABLE_LENGTH_MESSAGE_SIZE, 0, 100));
        assert!(segmented_for(5, 100, 0, 1));
        assert!(segmented_for(15, 100, 0, 1));

        // Skip(_) follows the sentinel rule.
        assert!(segmented_for(99, 100, 0, 1));
        assert!(!segmented_for(99, VARIABLE_LENGTH_MESSAGE_SIZE, 0, 50));
    }

    #[test]
    fn read_consumes_exactly_28_bytes() {
        let bytes = synth(31, 100, 0, 1);
        let mut r = SliceReader::new(&bytes);
        let _ = MessageHeader::read(&mut r).unwrap();
        assert_eq!(r.position(), SIZE);
    }

    #[test]
    fn read_errors_on_short_input() {
        let bytes = synth(31, 100, 0, 1);
        let mut r = SliceReader::new(&bytes[..16]);
        assert!(MessageHeader::read(&mut r).is_err());
    }

    /// Wire-format empirical fixture: matches the bytes observed at
    /// the start of LDM record 0 of `KLOT20251210_102338_V06`.
    #[test]
    fn read_decodes_klot_record_0_msg15_header() {
        // First 28 bytes of LDM record 0 (decompressed):
        //   [12 zero bytes][04 b8 08 0f 78 a8 4f d1 00 5c 4f 0a 00 05 00 01]
        let mut bytes = [0u8; 28];
        bytes[12..28].copy_from_slice(&[
            0x04, 0xB8, 0x08, 0x0F, 0x78, 0xA8, 0x4F, 0xD1, 0x00, 0x5C, 0x4F, 0x0A, 0x00, 0x05,
            0x00, 0x01,
        ]);
        let mut r = SliceReader::new(&bytes);
        let h = MessageHeader::read(&mut r).unwrap();
        assert_eq!(h.message_size_halfwords, 0x04B8);
        assert_eq!(h.message_size_bytes(), 0x04B8 * 2); // 2416
        assert_eq!(h.channel, 0x08);
        assert_eq!(h.message_type, MessageType::ClutterFilterMap); // 0x0F = 15
        assert_eq!(h.segment_count, 5);
        assert_eq!(h.segment_number, 1);
        assert!(h.segmented(), "MSG_15 should be fixed-frame");
    }
}
