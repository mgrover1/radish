//! NEXRAD message-stream iterator with explicit boundary resync.
//!
//! This module is the load-bearing fix for the upstream
//! `nexrad-decode 1.0.0-rc.3` phantom-radial bug
//! (`/tmp/radish-phantom-radials-bug.md`). The upstream loop trusts
//! every variable-length parser to leave the byte cursor at exactly
//! `offset + message_size_bytes()`. Our loop calls
//! `reader.try_skip_to(target)` after every parse, idempotent on
//! exact-match, snap-forward on under-read, error on past-buffer.
//!
//! Phase 2 scope: the loop walks every message and emits a typed
//! header plus a borrowed-payload slice. **The actual MSG_31 / MSG_2
//! / MSG_5 parsers are stubbed** (Phase 3+4 wires them up). This is
//! deliberate — the boundary fix lives in the loop, not in any
//! particular parser, and we want the loop tested in isolation
//! before parser bugs can confound the picture.

pub(crate) mod msg1;
pub(crate) mod msg2;
pub(crate) mod msg31;
pub(crate) mod msg5;

use super::error::{NexradDecodeError, Result};
use super::header::{
    MessageHeader, MessageType, LOGICAL_HEADER_SIZE, SEGMENT_FRAME_SIZE, SIZE as HEADER_SIZE,
    TCM_PREFIX_SIZE,
};
use super::reader::SliceReader;

/// One decoded message: its header plus a payload slice. Payload
/// boundaries are determined by `header.message_size_bytes()` (or
/// the segment-frame width for fixed-frame messages); the loop
/// guarantees the slice lands exactly on the boundary the header
/// declared.
#[derive(Debug)]
pub(crate) struct Message<'a> {
    pub(crate) header: MessageHeader,
    pub(crate) payload: MessagePayload<'a>,
    /// Byte offset of the message in the input stream (start of
    /// the header).
    pub(crate) offset: usize,
    /// Total span of the message in bytes (header + payload),
    /// equal to `target - offset` for variable-length and equal
    /// to `SEGMENT_FRAME_SIZE` for non-final segmented messages.
    pub(crate) size: usize,
}

/// Payload variants carried alongside the header.
///
/// Typed variants land per-phase: `Msg31` in Phase 3, `Msg2` and
/// `Msg5` in Phase 4, `Msg1` in plan 0006. `Raw` / `Reassembled`
/// remain for messages we don't decode (MSG_15 clutter filter map,
/// MSG_18 adaptation data, `Skip(_)` types).
#[derive(Debug)]
#[allow(dead_code)] // SegmentedFragment / Reassembled used by tests + later phases.
pub(crate) enum MessagePayload<'a> {
    /// Raw bytes from the message header to the message boundary.
    Raw(&'a [u8]),
    /// Decoded MSG_1 (legacy Digital Radar Data) message — pre-Build-12.
    Msg1(Box<msg1::Msg1>),
    /// Decoded MSG_31 (Digital Radar Data Generic Format) message.
    Msg31(Box<msg31::Msg31<'a>>),
    /// Decoded MSG_2 (RDA Status Data) message.
    Msg2(Box<msg2::Msg2>),
    /// Decoded MSG_5 (Volume Coverage Pattern) message.
    Msg5(Box<msg5::Msg5>),
    /// One frame of a fixed-segment message that hasn't been
    /// reassembled yet. The accumulator hands it back to the loop
    /// when it isn't ready to emit the assembled message.
    SegmentedFragment {
        segment_number: u16,
        segment_count: u16,
        bytes: &'a [u8],
    },
    /// Final reassembled payload for a multi-segment message we
    /// don't typed-decode (e.g. MSG_15) — every fragment's bytes
    /// concatenated in segment order.
    Reassembled(Vec<u8>),
}

/// Walk an in-memory NEXRAD message stream end-to-end. Allocates
/// once for the result vector; payloads are borrowed from `bytes`
/// where possible (Reassembled is the one allocation per multi-
/// segment message).
pub(crate) fn decode_messages(bytes: &[u8]) -> Result<Vec<Message<'_>>> {
    let mut reader = SliceReader::new(bytes);
    let mut messages = Vec::new();
    let mut accumulator = SegmentAccumulator::new();

    while reader.remaining().len() >= HEADER_SIZE {
        let offset = reader.position();
        let header = MessageHeader::read(&mut reader)?;
        // Per-message wire stride for variable-length messages is
        // `TCM_PREFIX_SIZE + message_size_bytes` — the 12-byte TCM
        // prefix is in addition to the size declared by the Table II
        // header (ICD §3.2.4.1 footnote). For fixed-frame messages
        // the stride is `SEGMENT_FRAME_SIZE` regardless of the
        // declared message_size.
        let target = if header.segmented() {
            offset.saturating_add(SEGMENT_FRAME_SIZE)
        } else {
            offset.saturating_add(TCM_PREFIX_SIZE + header.message_size_bytes())
        };

        if !header.segmented() {
            // Variable-length path. MSG_31 is the typed-parse case
            // (Phase 3); everything else falls through to a Raw
            // payload-bytes capture (Phase 4 will add MSG_2, MSG_5
            // typed variants).
            let payload = match header.message_type {
                MessageType::DigitalRadarDataGenericFormat => {
                    // `msg31::parse` resolves block pointers as
                    // offsets from the MSG_31 wire-body start
                    // (= reader.position() right after the 28-byte
                    // TCM + Table II header was consumed). This
                    // matches `danielway/nexrad`'s `start_position`
                    // and xradar's `block_pointer + 12 + LEN_MSG_HEADER`
                    // arithmetic. **Not** the start of the message
                    // including the TCM prefix.
                    let body_start = reader.position();
                    let msg = msg31::parse(&mut reader, body_start)?;
                    MessagePayload::Msg31(Box::new(msg))
                }
                _ => {
                    let payload_bytes =
                        take_payload_bytes(&mut reader, &header, offset, bytes.len())?;
                    MessagePayload::Raw(payload_bytes)
                }
            };
            // ─── THE FIX ───
            // Always resync to the declared boundary, regardless of
            // whether the parser consumed exactly that many bytes.
            // Idempotent if we landed exactly on it; snaps forward
            // on under-read; errors `UnexpectedEof` if the declared
            // size exceeds the buffer length.
            reader.try_skip_to(target)?;
            messages.push(Message {
                header,
                payload,
                offset,
                size: target - offset,
            });
            continue;
        }

        // Fixed-segment path. The declared `message_size` is the
        // logical Table II message size (16-byte header + payload);
        // the on-wire frame is `SEGMENT_FRAME_SIZE = 2432` bytes
        // including the TCM prefix and trailing padding.
        let logical_size = header.message_size_bytes();
        if logical_size < LOGICAL_HEADER_SIZE {
            // Zero/too-small declared size: a real LDM record has
            // trailing zero-padded 2432-byte frames after the last
            // semantically-meaningful message (the bzip2-block
            // alignment). Walk past them without yielding a Message
            // — they're not malformed, just empty.
            reader.try_skip_to(target)?;
            continue;
        }
        // Single-segment vs multi-segment payload sizing. Legacy
        // MSG_1 (and MSG_18 in some files) under-declares
        // `message_size_halfwords` relative to what the message
        // actually occupies — the wire layout is "fixed 2432-byte
        // frame, declared size is just the metadata header." For
        // single-segment fixed-frame messages we give the parser
        // the full frame content (2404 bytes after the 28-byte
        // header) and let it stop at its real boundary; for
        // multi-segment ones (MSG_5, MSG_15) the declared size is
        // load-bearing (the accumulator concatenates per-segment
        // payloads and the parser reads from the assembled buffer).
        // Mirrors danielway/nexrad's `decode_messages` shape.
        let payload_bytes = if header.segment_count > 1 {
            let payload_size = logical_size - LOGICAL_HEADER_SIZE;
            let bytes = reader.take_bytes(payload_size)?;
            reader.try_skip_to(target)?;
            bytes
        } else {
            let frame_content_size = SEGMENT_FRAME_SIZE - HEADER_SIZE;
            let bytes = reader.take_bytes(frame_content_size)?;
            // Already at the SEGMENT_FRAME_SIZE boundary after the
            // take above (28 + 2404 == 2432) but call try_skip_to
            // for symmetry + a no-op safety net if `target` ever
            // diverges.
            reader.try_skip_to(target)?;
            bytes
        };

        // Single-segment fixed-frame messages emit immediately;
        // multi-segment ones go through the accumulator. In both
        // cases we typed-dispatch by message type at emit time.
        if header.segment_count <= 1 {
            let payload = parse_fixed_frame_payload(&header, payload_bytes)?;
            messages.push(Message {
                header,
                payload,
                offset,
                size: SEGMENT_FRAME_SIZE,
            });
            continue;
        }

        if let Some(reassembled) = accumulator.feed(header, payload_bytes)? {
            let payload = parse_reassembled_payload(&header, reassembled)?;
            messages.push(Message {
                header,
                payload,
                offset,
                size: SEGMENT_FRAME_SIZE,
            });
        }
    }

    Ok(messages)
}

/// Dispatch a single-segment fixed-frame payload to the right
/// typed parser. Falls back to `Raw` for message types we don't
/// decode (MSG_15, MSG_18, etc.).
fn parse_fixed_frame_payload<'a>(
    header: &MessageHeader,
    payload_bytes: &'a [u8],
) -> Result<MessagePayload<'a>> {
    // Pre-Build-12 raw Archive II files contain a lot of trailing
    // zero-padding + scattered garbage frames whose Table II header
    // bytes accidentally decode to known message types but with
    // payload sizes too small for the typed parser. Fall back to
    // Raw on any typed-parse error rather than aborting the whole
    // walk — matches `danielway/nexrad`'s `warn!` + `try_skip_to`
    // behaviour without the `tracing` dependency.
    fn try_typed<'a, T>(
        bytes: &'a [u8],
        parse: impl FnOnce(&mut SliceReader<'a>) -> Result<T>,
        wrap: impl FnOnce(T) -> MessagePayload<'a>,
    ) -> MessagePayload<'a> {
        let mut r = SliceReader::new(bytes);
        match parse(&mut r) {
            Ok(value) => wrap(value),
            Err(_) => MessagePayload::Raw(bytes),
        }
    }

    Ok(match header.message_type {
        MessageType::DigitalRadarDataLegacy => try_typed(payload_bytes, msg1::Msg1::read, |m| {
            MessagePayload::Msg1(Box::new(m))
        }),
        MessageType::RdaStatusData => try_typed(payload_bytes, msg2::Msg2::read, |m| {
            MessagePayload::Msg2(Box::new(m))
        }),
        MessageType::VolumeCoveragePattern => try_typed(payload_bytes, msg5::Msg5::read, |m| {
            MessagePayload::Msg5(Box::new(m))
        }),
        _ => MessagePayload::Raw(payload_bytes),
    })
}

/// Dispatch a multi-segment reassembled payload to the right typed
/// parser. MSG_5 occasionally spans 2-3 segments; MSG_15 is many
/// frames but radish doesn't decode it, so it stays `Reassembled`.
fn parse_reassembled_payload(
    header: &MessageHeader,
    reassembled: Vec<u8>,
) -> Result<MessagePayload<'static>> {
    match header.message_type {
        MessageType::RdaStatusData => {
            let mut r = SliceReader::new(&reassembled);
            let m = msg2::Msg2::read(&mut r)?;
            Ok(MessagePayload::Msg2(Box::new(m)))
        }
        MessageType::VolumeCoveragePattern => {
            let mut r = SliceReader::new(&reassembled);
            let m = msg5::Msg5::read(&mut r)?;
            Ok(MessagePayload::Msg5(Box::new(m)))
        }
        _ => Ok(MessagePayload::Reassembled(reassembled)),
    }
}

/// Pull the variable-length payload out as a raw slice. Reader is
/// expected to be at `offset + HEADER_SIZE` (i.e. just past the
/// 28-byte combined TCM + Table II header). Returns
/// `message_size_bytes - LOGICAL_HEADER_SIZE` payload bytes — the
/// portion of the message after the Table II header.
///
/// Phase 3 replaces this with a typed dispatch over
/// `header.message_type()`.
fn take_payload_bytes<'a>(
    reader: &mut SliceReader<'a>,
    header: &MessageHeader,
    offset: usize,
    buf_len: usize,
) -> Result<&'a [u8]> {
    let logical_size = header.message_size_bytes();
    if logical_size < LOGICAL_HEADER_SIZE {
        return Err(NexradDecodeError::MalformedHeader {
            offset,
            reason: "declared message_size smaller than the 16-byte Table II header",
        });
    }
    let payload_size = logical_size - LOGICAL_HEADER_SIZE;
    // Total wire span = TCM prefix + logical message.
    let target = offset.saturating_add(TCM_PREFIX_SIZE + logical_size);
    if target > buf_len {
        return Err(NexradDecodeError::UnexpectedEof {
            offset,
            needed: TCM_PREFIX_SIZE + logical_size,
            available: buf_len.saturating_sub(offset),
        });
    }
    reader.take_bytes(payload_size)
}

/// Reassembles multi-segment fixed-frame messages (e.g. MSG_5 spans
/// 2-3 segments on most VCPs; MSG_15 spans many).
///
/// State machine:
///
/// * **Idle** — no message in progress. First fragment must be
///   `segment_number == 1`; otherwise we log a warning (via the
///   error path on `feed`) and stay idle.
/// * **In progress** — accumulating fragments in segment order. On
///   the final segment (`segment_number == segment_count`) yields
///   the assembled payload and returns to Idle.
struct SegmentAccumulator {
    expected_count: u16,
    next_segment: u16,
    sequence_number: u16,
    payloads: Vec<u8>,
}

impl SegmentAccumulator {
    fn new() -> Self {
        Self {
            expected_count: 0,
            next_segment: 0,
            sequence_number: 0,
            payloads: Vec::new(),
        }
    }

    fn is_active(&self) -> bool {
        self.expected_count > 0
    }

    /// Feed one segment. Returns `Some(reassembled_payload)` when
    /// the accumulator just finished a multi-segment message; `None`
    /// while still mid-stream.
    fn feed(&mut self, header: MessageHeader, payload: &[u8]) -> Result<Option<Vec<u8>>> {
        if !self.is_active() {
            // First segment opens the accumulation.
            if header.segment_number != 1 {
                // Fragment arrived without segment 1 — drop it. We
                // could surface a warning, but radish's downstream
                // doesn't care about MSG_15/18 reassembly today.
                return Ok(None);
            }
            self.expected_count = header.segment_count;
            self.next_segment = 1;
            self.sequence_number = header.sequence_number;
            self.payloads.clear();
        }

        if header.sequence_number != self.sequence_number
            || header.segment_count != self.expected_count
            || header.segment_number != self.next_segment
        {
            // Out-of-order or mismatched segment — abandon the
            // current accumulation and start over from the next
            // segment_number==1.
            self.reset();
            return Ok(None);
        }

        self.payloads.extend_from_slice(payload);
        self.next_segment += 1;

        if header.segment_number == header.segment_count {
            let out = std::mem::take(&mut self.payloads);
            self.reset();
            return Ok(Some(out));
        }
        Ok(None)
    }

    fn reset(&mut self) {
        self.expected_count = 0;
        self.next_segment = 0;
        self.sequence_number = 0;
        self.payloads.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::super::header::{
        MessageType, LOGICAL_HEADER_SIZE, TCM_PREFIX_SIZE, VARIABLE_LENGTH_MESSAGE_SIZE,
    };
    use super::*;
    use rstest::rstest;

    /// Build a single variable-length message of unknown type
    /// (`Skip(99)`) using the `0xFFFF` sentinel + extended size
    /// encoding (ICD Note 7). `logical_size` is the bytes following
    /// the 12-byte TCM prefix; on-wire span = `12 + logical_size`.
    ///
    /// Used by the loop-arithmetic tests so we don't depend on a
    /// real MSG_31 data-header layout (Phase 3 dispatches MSG_31 to
    /// the typed parser; Skip stays as `Raw` payload bytes).
    fn synthesize_skip_message(logical_size: u32) -> Vec<u8> {
        assert!(
            logical_size >= LOGICAL_HEADER_SIZE as u32,
            "logical_size must accommodate the 16-byte Table II header"
        );
        let total = TCM_PREFIX_SIZE + logical_size as usize;
        let mut buf = vec![0u8; total];
        // bytes 0..12 are the zero-filled TCM prefix.
        buf[12..14].copy_from_slice(&VARIABLE_LENGTH_MESSAGE_SIZE.to_be_bytes());
        buf[15] = 99; // unknown type → Skip(99)
        let segment_count = ((logical_size >> 16) & 0xFFFF) as u16;
        let segment_number = (logical_size & 0xFFFF) as u16;
        buf[24..26].copy_from_slice(&segment_count.to_be_bytes());
        buf[26..28].copy_from_slice(&segment_number.to_be_bytes());
        buf
    }

    fn synthesize_two_skip(decl1: u32, decl2: u32) -> Vec<u8> {
        let mut buf = synthesize_skip_message(decl1);
        buf.extend_from_slice(&synthesize_skip_message(decl2));
        buf
    }

    /// **The load-bearing test.** Pin variable-length boundary resync
    /// across pairs of `Skip(_)` messages with varying declared sizes.
    /// Each message's wire span = `12 + logical_size`, so the second
    /// message's offset must equal the first's wire span.
    #[rstest]
    #[case::equal_sizes(64, 64, TCM_PREFIX_SIZE + 64)]
    #[case::large_then_small(80, 64, TCM_PREFIX_SIZE + 80)]
    #[case::small_then_large(64, 80, TCM_PREFIX_SIZE + 64)]
    fn boundary_resync_pins_second_message_offset(
        #[case] first_logical: u32,
        #[case] second_logical: u32,
        #[case] expected_second_offset: usize,
    ) {
        let bytes = synthesize_two_skip(first_logical, second_logical);
        let messages = decode_messages(&bytes).expect("decode");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].offset, 0);
        assert_eq!(messages[1].offset, expected_second_offset);
        assert_eq!(messages[0].size, TCM_PREFIX_SIZE + first_logical as usize);
        assert_eq!(messages[1].size, TCM_PREFIX_SIZE + second_logical as usize);
    }

    #[test]
    fn boundary_resync_target_past_buffer_returns_unexpected_eof() {
        // logical_size = 200 → wire span 12 + 200 = 212. Truncate to
        // 50 to provoke EOF.
        let bytes = synthesize_skip_message(200);
        let truncated = &bytes[..50];
        let err = decode_messages(truncated).expect_err("should fail");
        assert!(
            matches!(err, NexradDecodeError::UnexpectedEof { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn empty_input_yields_empty_message_list() {
        assert!(decode_messages(&[]).unwrap().is_empty());
    }

    #[test]
    fn input_smaller_than_header_terminates_cleanly() {
        // 8 bytes < HEADER_SIZE (28) → loop never enters body.
        let bytes = vec![0u8; 8];
        assert!(decode_messages(&bytes).unwrap().is_empty());
    }

    /// `Skip(_)` with the variable-length sentinel set walks via the
    /// extended size encoding (`segment_count` MSB | `segment_number`
    /// LSB). The next message must read its header from the
    /// boundary the loop resynced to.
    #[test]
    fn skip_with_sentinel_advances_via_extended_size() {
        // First message: type 99 (Skip), segment_size = 0xFFFF, then
        // segment_count/segment_number encode 64 bytes wire-size of
        // the message (= 12 TCM prefix + 16 logical header + 36
        // payload? no — the extended size is the LOGICAL size; total
        // wire = 12 + 64 = 76).
        let logical_size: u32 = 64;
        let segment_count = ((logical_size >> 16) & 0xFFFF) as u16;
        let segment_number = (logical_size & 0xFFFF) as u16;
        let total_wire = TCM_PREFIX_SIZE + logical_size as usize;

        let mut buf = vec![0u8; total_wire];
        buf[12..14].copy_from_slice(&VARIABLE_LENGTH_MESSAGE_SIZE.to_be_bytes());
        buf[15] = 99; // unknown type → Skip(99)
        buf[24..26].copy_from_slice(&segment_count.to_be_bytes());
        buf[26..28].copy_from_slice(&segment_number.to_be_bytes());

        // Second message: another Skip-with-sentinel of size 32.
        buf.extend_from_slice(&synthesize_skip_message(32));

        let messages = decode_messages(&buf).expect("decode");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].header.message_type, MessageType::Skip(99));
        assert_eq!(messages[0].size, total_wire);
        assert_eq!(messages[1].offset, total_wire);
        assert_eq!(messages[1].header.message_type, MessageType::Skip(99));
    }

    /// Build one fixed-segment frame with the given type byte,
    /// segment_size halfwords, segment_count, and segment_number.
    /// Marks the first payload byte with `marker` so reassembly
    /// tests can detect ordering.
    fn synthesize_fixed_frame(
        type_byte: u8,
        seq_number: u16,
        size_halfwords: u16,
        segment_count: u16,
        segment_number: u16,
        marker: u8,
    ) -> Vec<u8> {
        let mut buf = vec![0u8; SEGMENT_FRAME_SIZE];
        // bytes 0..12 are zero TCM prefix.
        buf[12..14].copy_from_slice(&size_halfwords.to_be_bytes());
        buf[15] = type_byte;
        buf[16..18].copy_from_slice(&seq_number.to_be_bytes());
        buf[24..26].copy_from_slice(&segment_count.to_be_bytes());
        buf[26..28].copy_from_slice(&segment_number.to_be_bytes());
        // Marker at the first payload byte (right after the 28-byte
        // combined header).
        buf[HEADER_SIZE] = marker;
        buf
    }

    /// Lenient-fallback path: a fixed-frame message whose Table II
    /// header declares MSG_5 but whose payload is too small / garbage
    /// to parse must NOT error out the whole decode walk — the
    /// `parse_fixed_frame_payload` helper falls back to
    /// `MessagePayload::Raw` so the loop continues. Pre-Build-12 raw
    /// Archive II files have lots of garbage frames; without this
    /// lenience `decode_volume` would die on the first one.
    #[test]
    fn fixed_frame_typed_parse_error_falls_back_to_raw() {
        // Frame declares type=5 (MSG_5) with a logical size of 18
        // halfwords (= 36 bytes total, only 20 bytes of payload).
        // MSG_5's parser requires ≥22 bytes for its 11-halfword
        // header, so it errors. The lenient handler turns the
        // failure into Raw without aborting the walk.
        let segment_size_hw: u16 = ((LOGICAL_HEADER_SIZE + 20) / 2) as u16;
        let mut buf = synthesize_fixed_frame(5, 1, segment_size_hw, 1, 1, 0xAA);
        // Put random non-MSG_5-shaped bytes into the payload area so
        // a successful parse would be a coincidence.
        buf[HEADER_SIZE..HEADER_SIZE + 20].copy_from_slice(&[0xFFu8; 20]);
        // Append a real MSG_2 frame so we can confirm the walk
        // continues past the lenient fallback.
        buf.extend_from_slice(&synthesize_fixed_frame(2, 2, 60, 1, 1, 0xBB));

        let messages = decode_messages(&buf).expect("decode walk");
        assert_eq!(messages.len(), 2, "got {} messages", messages.len());
        assert!(
            matches!(messages[0].payload, MessagePayload::Raw(_)),
            "garbage MSG_5 frame should fall back to Raw, got {:?}",
            messages[0].payload
        );
        assert_eq!(messages[1].header.message_type, MessageType::RdaStatusData);
    }

    /// Reassembly sanity check: build a 2-segment MSG_15 (Clutter
    /// Filter Map). MSG_15 is fixed-frame and segmented like MSG_5
    /// but radish doesn't typed-decode it, so reassembly emits
    /// `MessagePayload::Reassembled`. We use MSG_15 here because
    /// MSG_5 is now typed-dispatched and would reject synthetic
    /// non-VCP bytes.
    #[test]
    fn segmented_message_reassembles_in_order() {
        // Each segment: 16-byte logical header + 2400-byte payload
        // = 2416 byte logical size. Total fixed-frame width = 2432.
        let segment_size_hw: u16 = ((LOGICAL_HEADER_SIZE + 2400) / 2) as u16;
        let mut buf = synthesize_fixed_frame(15, 77, segment_size_hw, 2, 1, 0xAA);
        buf.extend_from_slice(&synthesize_fixed_frame(15, 77, segment_size_hw, 2, 2, 0xBB));

        let messages = decode_messages(&buf).expect("decode");
        // The accumulator suppresses segment 1 (partial) and emits
        // segment 2 with the reassembled payload.
        assert_eq!(messages.len(), 1, "got {} messages", messages.len());
        let MessagePayload::Reassembled(p) = &messages[0].payload else {
            panic!("expected Reassembled, got {:?}", messages[0].payload);
        };
        assert_eq!(p.len(), 2400 + 2400, "two segments × 2400 byte payloads");
        assert_eq!(p[0], 0xAA, "segment 1 first byte preserved");
        assert_eq!(p[2400], 0xBB, "segment 2 starts at byte 2400 of reassembly");
    }

    /// Real LDM records have trailing zero-padded 2432-byte frames
    /// after the last semantically-meaningful message (bzip2
    /// block-alignment artifact). The loop must walk past them
    /// without yielding a Message and without erroring on the
    /// "declared message_size smaller than the 16-byte Table II
    /// header" check.
    #[test]
    fn trailing_zero_padded_frames_are_walked_silently() {
        // One real MSG_18 (RDA Adaptation Data — fixed-frame, not
        // typed-decoded by radish), then 3 frames of all zeros.
        // MSG_18 here instead of MSG_5 because Phase 4's typed
        // dispatch would reject the synthetic bytes.
        let segment_size_hw: u16 = ((LOGICAL_HEADER_SIZE + 100) / 2) as u16;
        let mut buf = synthesize_fixed_frame(18, 1, segment_size_hw, 1, 1, 0xAA);
        buf.extend(vec![0u8; SEGMENT_FRAME_SIZE * 3]);

        let messages = decode_messages(&buf).expect("decode");
        assert_eq!(
            messages.len(),
            1,
            "only the real MSG_18 should be yielded; zero-padded \
             trailers are walked silently"
        );
        assert_eq!(
            messages[0].header.message_type,
            MessageType::RdaAdaptationData
        );
    }

    #[test]
    fn proptest_decode_messages_never_panics_on_random_input() {
        // Property: for any byte input, decode_messages returns a
        // Result and doesn't panic. Binary parsers are notorious for
        // crashing on malformed input — pin the no-panic invariant.
        use proptest::prelude::*;
        proptest!(|(bytes in prop::collection::vec(any::<u8>(), 0..8192))| {
            let _ = decode_messages(&bytes);
        });
    }
}
