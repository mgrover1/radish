//! Byte-cursor primitive used by every NEXRAD message parser.
//!
//! Why a custom reader instead of `bytes::Buf` or `std::io::Cursor`:
//! the load-bearing operation in NEXRAD decoding is **resync to a
//! declared message boundary** after a parser may have under- or
//! over-read its payload. `try_skip_to(target)` is the operation we
//! need, and neither stdlib option exposes it as a single call.
//!
//! The cursor is `&[u8]`-backed and zero-copy — every primitive
//! either returns a typed value (big-endian decode) or a borrowed
//! sub-slice. No allocations on the hot path.

use byteorder::{BigEndian, ByteOrder};

use super::error::{NexradDecodeError, Result};

/// A forward-only byte cursor over `&[u8]`. All primitives advance
/// the cursor on success; on `UnexpectedEof` the cursor is left at
/// its pre-call position so a caller can switch strategies (e.g. log
/// the position and `break`).
#[derive(Debug)]
pub(crate) struct SliceReader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> SliceReader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    /// Cursor offset from the start of the input.
    pub(crate) fn position(&self) -> usize {
        self.pos
    }

    /// Slice from the cursor to the end of the input.
    pub(crate) fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.pos..]
    }

    /// Take the next `n` bytes as a borrowed slice and advance.
    /// Returns `UnexpectedEof` if fewer than `n` bytes remain.
    pub(crate) fn take_bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| NexradDecodeError::UnexpectedEof {
                offset: self.pos,
                needed: n,
                available: self.bytes.len().saturating_sub(self.pos),
            })?;
        if end > self.bytes.len() {
            return Err(NexradDecodeError::UnexpectedEof {
                offset: self.pos,
                needed: n,
                available: self.bytes.len().saturating_sub(self.pos),
            });
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    /// Advance the cursor by `n` bytes (no read). Errors on overflow
    /// past the end of the input.
    pub(crate) fn advance(&mut self, n: usize) -> Result<()> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| NexradDecodeError::UnexpectedEof {
                offset: self.pos,
                needed: n,
                available: self.bytes.len().saturating_sub(self.pos),
            })?;
        if end > self.bytes.len() {
            return Err(NexradDecodeError::UnexpectedEof {
                offset: self.pos,
                needed: n,
                available: self.bytes.len().saturating_sub(self.pos),
            });
        }
        self.pos = end;
        Ok(())
    }

    /// Snap the cursor to `target`, advancing forward. Idempotent if
    /// the cursor is already at or past the target. Errors only when
    /// `target > input.len()` — the file is truncated below the size
    /// the previous header declared.
    ///
    /// This is the operation that fixes the upstream
    /// `nexrad-decode 1.0.0-rc.3` boundary-resync bug — every
    /// variable-length parse re-syncs to the declared message size,
    /// so an under-read parser can't poison the next header.
    pub(crate) fn try_skip_to(&mut self, target: usize) -> Result<()> {
        if target > self.bytes.len() {
            return Err(NexradDecodeError::UnexpectedEof {
                offset: self.pos,
                needed: target.saturating_sub(self.pos),
                available: self.bytes.len().saturating_sub(self.pos),
            });
        }
        if target > self.pos {
            self.pos = target;
        }
        Ok(())
    }

    pub(crate) fn read_u8(&mut self) -> Result<u8> {
        let b = self.take_bytes(1)?;
        Ok(b[0])
    }

    pub(crate) fn read_u16_be(&mut self) -> Result<u16> {
        let b = self.take_bytes(2)?;
        Ok(BigEndian::read_u16(b))
    }

    pub(crate) fn read_u32_be(&mut self) -> Result<u32> {
        let b = self.take_bytes(4)?;
        Ok(BigEndian::read_u32(b))
    }

    pub(crate) fn read_i16_be(&mut self) -> Result<i16> {
        let b = self.take_bytes(2)?;
        Ok(BigEndian::read_i16(b))
    }

    pub(crate) fn read_i32_be(&mut self) -> Result<i32> {
        let b = self.take_bytes(4)?;
        Ok(BigEndian::read_i32(b))
    }

    pub(crate) fn read_f32_be(&mut self) -> Result<f32> {
        let b = self.take_bytes(4)?;
        Ok(BigEndian::read_f32(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_starts_at_zero_and_advances_on_take() {
        let mut r = SliceReader::new(&[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(r.position(), 0);
        assert_eq!(r.take_bytes(2).unwrap(), &[0x01, 0x02]);
        assert_eq!(r.position(), 2);
    }

    #[test]
    fn take_bytes_errors_on_short_input() {
        let mut r = SliceReader::new(&[0x01]);
        let err = r.take_bytes(2).unwrap_err();
        // Cursor stays put on error so the caller can recover.
        assert_eq!(r.position(), 0);
        let NexradDecodeError::UnexpectedEof {
            offset,
            needed,
            available,
        } = err
        else {
            panic!("expected UnexpectedEof, got {err:?}")
        };
        assert_eq!((offset, needed, available), (0, 2, 1));
    }

    #[test]
    fn read_u32_be_decodes_high_byte_first() {
        let mut r = SliceReader::new(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(r.read_u32_be().unwrap(), 0xDEAD_BEEF);
        assert_eq!(r.position(), 4);
    }

    #[test]
    fn read_f32_be_round_trip() {
        // f32::from_bits(0x40490FDB) ≈ pi
        let bytes = std::f32::consts::PI.to_be_bytes();
        let mut r = SliceReader::new(&bytes);
        assert!((r.read_f32_be().unwrap() - std::f32::consts::PI).abs() < 1e-6);
    }

    #[test]
    fn try_skip_to_idempotent_when_already_past() {
        let mut r = SliceReader::new(&[0u8; 16]);
        r.advance(8).unwrap();
        r.try_skip_to(4).unwrap(); // already past 4 — must be no-op
        assert_eq!(r.position(), 8);
    }

    #[test]
    fn try_skip_to_advances_forward() {
        let mut r = SliceReader::new(&[0u8; 16]);
        r.advance(4).unwrap();
        r.try_skip_to(12).unwrap();
        assert_eq!(r.position(), 12);
    }

    #[test]
    fn try_skip_to_errors_when_target_past_end() {
        let mut r = SliceReader::new(&[0u8; 8]);
        let err = r.try_skip_to(16).unwrap_err();
        assert!(
            matches!(err, NexradDecodeError::UnexpectedEof { .. }),
            "expected UnexpectedEof, got {err:?}"
        );
    }

    #[test]
    fn advance_at_exact_end_succeeds_and_remaining_is_empty() {
        let mut r = SliceReader::new(&[0u8; 8]);
        r.advance(8).unwrap();
        assert!(r.remaining().is_empty());
    }
}
