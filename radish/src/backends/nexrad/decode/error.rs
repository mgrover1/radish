//! Errors raised by the in-tree NEXRAD Level 2 byte-level decoder.
//!
//! Library-style — typed variants via `thiserror`, no `anyhow`. Each
//! variant carries enough context (offsets, message types, expected vs
//! actual sizes) for upstream callers to surface a useful diagnostic
//! without having to re-walk the file.

use thiserror::Error;

/// Errors that can surface during NEXRAD Level 2 byte-level decoding.
#[derive(Debug, Error)]
pub enum NexradDecodeError {
    /// Reader ran out of bytes mid-record. Carries the offset where
    /// the read started and how many bytes were requested so the
    /// caller can pinpoint truncation.
    #[error("unexpected EOF at offset {offset}: needed {needed} bytes, {available} available")]
    UnexpectedEof {
        offset: usize,
        needed: usize,
        available: usize,
    },

    /// `MessageHeader.message_type` was not one of the values listed
    /// in ICD 2620002AA Table II. The byte is preserved so a caller
    /// can match against the ICD if extending support.
    #[error("invalid NEXRAD message type byte: {code} (0x{code:02x})")]
    InvalidMessageType { code: u8 },

    /// A data block pointer in MSG_31 referenced an offset outside
    /// the message payload. `block` is the static block name
    /// ("REF", "VOL", ...) for log triage.
    #[error("invalid MSG_31 data block pointer for {block}: offset={offset}, message_size={message_size}")]
    InvalidPointerOffset {
        block: &'static str,
        offset: u32,
        message_size: u32,
    },

    /// bzip2 decompression of an LDM record failed. The record's
    /// position in the file is captured so callers can isolate the
    /// bad segment.
    #[error("bzip2 decompression failed at record offset {offset}: {source}")]
    Decompression {
        offset: usize,
        #[source]
        source: std::io::Error,
    },

    /// A struct's bytes failed self-consistency checks (out-of-range
    /// enum tag, impossible length, etc.). `reason` is a static
    /// string so we don't allocate a `String` on every parser hop.
    #[error("malformed header at offset {offset}: {reason}")]
    MalformedHeader { offset: usize, reason: &'static str },

    /// File is missing a Volume Coverage Pattern (MSG_5). Without
    /// it we don't know how to group radials into sweeps.
    #[error("MSG_5 (Volume Coverage Pattern) missing from file")]
    MissingCoveragePattern,

    /// File doesn't appear to be an LDM (modern) Archive II record
    /// stream. CTM (legacy 2432-byte fixed frames, pre-2016) is not
    /// supported by this decoder yet — see plan 0004.
    #[error("unsupported NEXRAD record format (LDM expected; CTM not yet supported)")]
    UnsupportedRecordFormat,
}

/// Convenience alias used throughout the decode module.
pub(crate) type Result<T> = std::result::Result<T, NexradDecodeError>;
