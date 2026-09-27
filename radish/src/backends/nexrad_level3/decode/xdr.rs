//! A minimal XDR (RFC 1832 External Data Representation) unpacker — the
//! subset symbology packet 28 (the generic data packet: `RATE`/code 176,
//! and any future packet-28 product) actually uses. Written from the RFC,
//! cross-checked against TWO independent readers of the same format
//! (xradar #392's `_XDRUnpacker`/`_Level3XDRParser` and MetPy's
//! `Level3XDRParser`, `metpy/io/nexrad.py`) rather than ported from either
//! line for line — see plan 0012 §3.1/§3.3 step 3.
//!
//! **Every primitive here bounds its own allocation against an untrusted
//! length prefix BEFORE allocating, not after** — added by a 2026-08-09
//! adversarial review pass (plan 0012 §0) that found the original design
//! named-error'd on an unknown component type but never bounded
//! `unpack_string`/`unpack_int_array`'s own length prefixes, the exact
//! "small crafted input can expand without bound" scenario
//! `docs/NEXRAD_LEVEL3_WASM.md` §4.7 already treats as security-relevant
//! for this backend's OTHER untrusted-length-prefix source
//! (`symbology::decompress_bzip2_capped`). Same discipline, extended here.

use super::error::{Level3DecodeError, Result};

/// Cap on a single XDR string's declared byte length. Real packet-28
/// strings (product name/description, radar name, per-radial attributes)
/// are well under 256 bytes on every fixture this backend has decoded —
/// this gives >10x headroom while keeping a crafted 4-byte length prefix
/// from demanding an arbitrarily large read, the same reasoning as
/// [`super::symbology::MAX_DECOMPRESSED_BYTES`] applied to a different
/// untrusted-length source.
pub(crate) const MAX_XDR_STRING_LEN: u32 = 4096;

/// Cap on a single XDR int array's declared element count. The largest
/// real per-radial `data` array observed (`DPR`, 250 m gate spacing to the
/// ICD's surface-product range) is 920 elements; this gives >100x
/// headroom.
pub(crate) const MAX_XDR_ARRAY_LEN: u32 = 100_000;

/// Cap on a "counted list" element count (`parameters`/`components`) —
/// real files carry 0-1 elements in both lists on every fixture this
/// backend has decoded. Generous headroom, not a real-world estimate:
/// this exists so a corrupt/crafted count fails fast with a named error
/// instead of looping (each iteration still consumes real, finite bytes
/// either way — see [`XdrCursor::unpack_counted_list`]'s doc for why an
/// unbounded loop here is self-limiting even without this cap, but an
/// explicit refusal is still better than an implicit one, per this
/// crate's no-panics-on-untrusted-input discipline).
pub(crate) const MAX_XDR_LIST_LEN: i32 = 10_000;

/// Cap on a radial component's declared `num_rads` — real products carry
/// 360 or 720. Checked explicitly, fast, BEFORE the per-radial parse loop
/// even starts (rather than relying only on the loop dying once the
/// (already-capped, see `symbology::MAX_DECOMPRESSED_BYTES`) decompressed
/// buffer runs out of real bytes) — a clear named refusal instead of a
/// slow-but-bounded spin through truncation errors.
pub(crate) const MAX_XDR_RADIALS: i32 = 100_000;

/// A cursor over an XDR-encoded byte slice — big-endian, 4-byte-aligned
/// primitives per RFC 1832.
pub(crate) struct XdrCursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> XdrCursor<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bytes remaining, for diagnostics/error context only.
    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn take(&mut self, context: &'static str, n: usize) -> Result<&'a [u8]> {
        let have = self.remaining();
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or(Level3DecodeError::XdrTruncated {
                context,
                have,
                need: n,
            })?;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    pub(crate) fn unpack_int(&mut self) -> Result<i32> {
        let b = self.take("int", 4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn unpack_uint(&mut self) -> Result<u32> {
        let b = self.take("uint", 4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn unpack_float(&mut self) -> Result<f32> {
        let b = self.take("float", 4)?;
        Ok(f32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// `u32` length prefix + that many bytes, padded to the next 4-byte
    /// boundary (RFC 1832 §3.10) — padding bytes are consumed but not
    /// returned. Length is checked against [`MAX_XDR_STRING_LEN`] BEFORE
    /// the byte read, so a crafted length can't drive an oversized read
    /// attempt even transiently.
    pub(crate) fn unpack_string(&mut self) -> Result<String> {
        let len = self.unpack_uint()?;
        if len > MAX_XDR_STRING_LEN {
            return Err(Level3DecodeError::XdrStringTooLong {
                len,
                max: MAX_XDR_STRING_LEN,
            });
        }
        let len = len as usize;
        let bytes = self.take("string body", len)?;
        let padded = (len + 3) & !3;
        if padded > len {
            self.take("string padding", padded - len)?;
        }
        // Lossy, deliberately: radar/product name strings are ASCII in
        // practice (xradar and MetPy both decode them the same way,
        // `errors="replace"`/ASCII-only respectively), and a decode
        // boundary is the wrong place to hard-fail on a cosmetic metadata
        // field once its length is already bounds-checked above.
        Ok(String::from_utf8_lossy(bytes)
            .trim_end_matches('\0')
            .to_string())
    }

    /// `u32` count + that many big-endian `i32`s — no padding needed
    /// (already a multiple of 4 bytes). Count checked against
    /// [`MAX_XDR_ARRAY_LEN`] BEFORE the byte read, same discipline as
    /// [`Self::unpack_string`].
    pub(crate) fn unpack_int_array(&mut self) -> Result<Vec<i32>> {
        let len = self.unpack_uint()?;
        if len > MAX_XDR_ARRAY_LEN {
            return Err(Level3DecodeError::XdrArrayTooLong {
                len,
                max: MAX_XDR_ARRAY_LEN,
            });
        }
        let byte_len = len as usize * 4;
        let bytes = self.take("int array body", byte_len)?;
        Ok(bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_be_bytes(*c))
            .collect())
    }

    /// Walk an XDR "counted list with list-linkage pointers" —
    /// undocumented-in-the-ICD `pointer` `i32`s that appear once right
    /// after the count (before the first element) and once between every
    /// pair of adjacent elements (never after the last one). Confirmed
    /// against two independent readers of the same format (xradar #392's
    /// `_unpack_parameters`/`_unpack_components`, MetPy's
    /// `Level3XDRParser._unpack_parameters`/`_unpack_components`) and
    /// against a real `DPR` fixture, where this exact shape consumes the
    /// XDR buffer to EXACTLY zero bytes remaining — see plan 0012 §3's
    /// implementation notes. Shared by the product-level `parameters` list
    /// and the `components` list, which otherwise read completely
    /// different element shapes — `read_item` supplies the per-element
    /// read.
    ///
    /// A negative or zero count (malformed input, or the real and common
    /// "no parameters" case, which the ICD encodes as `count == 0` rather
    /// than omitting the field) returns an empty `Vec` without attempting
    /// any element reads. [`MAX_XDR_LIST_LEN`] rejects an implausibly
    /// large count fast — see that constant's own doc for why this is
    /// belt-and-suspenders rather than the only thing preventing runaway
    /// looping (each iteration still needs real bytes to succeed).
    pub(crate) fn unpack_counted_list<T>(
        &mut self,
        mut read_item: impl FnMut(&mut Self, usize) -> Result<T>,
    ) -> Result<Vec<T>> {
        let count = self.unpack_int()?;
        let _leading_pointer = self.unpack_int()?; // list-linkage garbage, always present
        if count <= 0 {
            return Ok(Vec::new());
        }
        if count > MAX_XDR_LIST_LEN {
            return Err(Level3DecodeError::XdrListTooLong {
                len: count,
                max: MAX_XDR_LIST_LEN,
            });
        }
        let count = count as usize;
        let mut items = Vec::new();
        for i in 0..count {
            items.push(read_item(self, i)?);
            if i + 1 < count {
                let _pointer = self.unpack_int()?; // list-linkage garbage between elements
            }
        }
        Ok(items)
    }

    /// Bytes remaining after the last successful read — used by
    /// `decode_packet28` to confirm the parse consumed the declared XDR
    /// payload exactly (the same "parser and buffer agree on the end"
    /// check both xradar and MetPy get for free from their respective
    /// higher-level frameworks; this backend checks it explicitly).
    pub(crate) fn bytes_remaining(&self) -> usize {
        self.remaining()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpack_int_reads_big_endian_signed() {
        let buf = (-5i32).to_be_bytes();
        let mut c = XdrCursor::new(&buf);
        assert_eq!(c.unpack_int().unwrap(), -5);
    }

    #[test]
    fn unpack_uint_reads_big_endian_unsigned() {
        let buf = 42u32.to_be_bytes();
        let mut c = XdrCursor::new(&buf);
        assert_eq!(c.unpack_uint().unwrap(), 42);
    }

    #[test]
    fn unpack_float_reads_big_endian_f32() {
        let buf = 1.5f32.to_be_bytes();
        let mut c = XdrCursor::new(&buf);
        assert_eq!(c.unpack_float().unwrap(), 1.5);
    }

    #[test]
    fn unpack_string_reads_and_skips_padding() {
        // "KLOT" (4 bytes, already 4-byte aligned -> zero padding).
        let mut buf = 4u32.to_be_bytes().to_vec();
        buf.extend_from_slice(b"KLOT");
        let mut c = XdrCursor::new(&buf);
        assert_eq!(c.unpack_string().unwrap(), "KLOT");
        assert_eq!(c.bytes_remaining(), 0);
    }

    #[test]
    fn unpack_string_pads_odd_length_to_4_byte_boundary() {
        // "abc" (3 bytes) pads to 4; one pad byte must be consumed.
        let mut buf = 3u32.to_be_bytes().to_vec();
        buf.extend_from_slice(b"abc\0"); // 3 real bytes + 1 pad byte
        buf.extend_from_slice(&99u32.to_be_bytes()); // sentinel: next read
        let mut c = XdrCursor::new(&buf);
        assert_eq!(c.unpack_string().unwrap(), "abc");
        assert_eq!(c.unpack_uint().unwrap(), 99); // cursor landed correctly
    }

    #[test]
    fn unpack_string_rejects_a_length_prefix_over_the_cap_before_reading() {
        // A length prefix far larger than the (tiny) buffer behind it —
        // must be rejected by the cap, not attempted as a read.
        let buf = (MAX_XDR_STRING_LEN + 1).to_be_bytes();
        let mut c = XdrCursor::new(&buf);
        assert!(matches!(
            c.unpack_string(),
            Err(Level3DecodeError::XdrStringTooLong { .. })
        ));
    }

    #[test]
    fn unpack_int_array_reads_n_big_endian_ints() {
        let mut buf = 3u32.to_be_bytes().to_vec();
        buf.extend_from_slice(&1i32.to_be_bytes());
        buf.extend_from_slice(&(-2i32).to_be_bytes());
        buf.extend_from_slice(&3i32.to_be_bytes());
        let mut c = XdrCursor::new(&buf);
        assert_eq!(c.unpack_int_array().unwrap(), vec![1, -2, 3]);
    }

    #[test]
    fn unpack_int_array_rejects_a_count_over_the_cap_before_allocating() {
        let buf = (MAX_XDR_ARRAY_LEN + 1).to_be_bytes();
        let mut c = XdrCursor::new(&buf);
        assert!(matches!(
            c.unpack_int_array(),
            Err(Level3DecodeError::XdrArrayTooLong { .. })
        ));
    }

    #[test]
    fn unpack_int_array_rejects_truncated_body_without_panicking() {
        // Declares 10 elements but the buffer only has 2.
        let mut buf = 10u32.to_be_bytes().to_vec();
        buf.extend_from_slice(&1i32.to_be_bytes());
        buf.extend_from_slice(&2i32.to_be_bytes());
        let mut c = XdrCursor::new(&buf);
        assert!(matches!(
            c.unpack_int_array(),
            Err(Level3DecodeError::XdrTruncated { .. })
        ));
    }

    #[test]
    fn unpack_counted_list_reads_zero_pointer_then_n_items_with_inter_item_pointers() {
        // count=2, leading pointer, item0, inter-item pointer, item1 (no
        // trailing pointer after the last item).
        let mut buf = 2i32.to_be_bytes().to_vec();
        buf.extend_from_slice(&0i32.to_be_bytes()); // leading pointer
        buf.extend_from_slice(&10i32.to_be_bytes()); // item 0
        buf.extend_from_slice(&0i32.to_be_bytes()); // inter-item pointer
        buf.extend_from_slice(&20i32.to_be_bytes()); // item 1
        let mut c = XdrCursor::new(&buf);
        let items = c.unpack_counted_list(|c, _i| c.unpack_int()).unwrap();
        assert_eq!(items, vec![10, 20]);
        assert_eq!(c.bytes_remaining(), 0);
    }

    #[test]
    fn unpack_counted_list_returns_empty_for_zero_count_without_reading_items() {
        // count=0, leading pointer, nothing else — matches the real
        // no-parameters case this format uses instead of omitting the
        // field entirely.
        let mut buf = 0i32.to_be_bytes().to_vec();
        buf.extend_from_slice(&0i32.to_be_bytes());
        let mut c = XdrCursor::new(&buf);
        let items: Vec<i32> = c.unpack_counted_list(|c, _i| c.unpack_int()).unwrap();
        assert!(items.is_empty());
        assert_eq!(c.bytes_remaining(), 0);
    }

    #[test]
    fn unpack_counted_list_treats_negative_count_as_empty() {
        let mut buf = (-1i32).to_be_bytes().to_vec();
        buf.extend_from_slice(&0i32.to_be_bytes());
        let mut c = XdrCursor::new(&buf);
        let items: Vec<i32> = c.unpack_counted_list(|c, _i| c.unpack_int()).unwrap();
        assert!(items.is_empty());
    }

    #[test]
    fn unpack_counted_list_rejects_an_implausible_count_before_looping() {
        let mut buf = (MAX_XDR_LIST_LEN + 1).to_be_bytes().to_vec();
        buf.extend_from_slice(&0i32.to_be_bytes());
        let mut c = XdrCursor::new(&buf);
        assert!(matches!(
            c.unpack_counted_list(|c, _i| c.unpack_int()),
            Err(Level3DecodeError::XdrListTooLong { .. })
        ));
    }

    #[test]
    fn take_rejects_a_length_that_would_overflow_the_cursor_position() {
        // pos + n overflowing usize must be a clean error, not a panic —
        // exercised directly since no public primitive can construct this
        // from realistic input (every length is bounded well below
        // usize::MAX by MAX_XDR_STRING_LEN/MAX_XDR_ARRAY_LEN first), but
        // `take` is the shared primitive every one of them funnels through.
        let mut c = XdrCursor::new(b"abcd");
        assert!(matches!(
            c.take("test", usize::MAX),
            Err(Level3DecodeError::XdrTruncated { .. })
        ));
    }
}
