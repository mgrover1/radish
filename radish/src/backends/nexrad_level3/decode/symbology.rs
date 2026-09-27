//! bzip2 decompression (incremental, hard-capped) and symbology-block ->
//! packet-16 (digital radial data array) -> (azimuths, codes) decode.
//! Mirrors `nexrad_level3.py:468-638`.

use ndarray::Array2;

use super::bytes::{read_i16_be, read_i32_be, read_u16_be};
use super::error::{Level3DecodeError, Result};
use super::xdr::{XdrCursor, MAX_XDR_RADIALS};

/// The largest real product decompresses to ~1.3 MB. This bounds a
/// decompression bomb, not a guess at product size: a ~1.3 KB crafted
/// payload can expand to gigabytes otherwise, in a context (a browser tab)
/// where the attacker controls the input — see `docs/NEXRAD_LEVEL3_WASM.md`
/// §4.7. One-shot `read_to_end`-style decompression would reopen exactly
/// this hole; `nexrad::decode::record::decompress`'s pattern (fine for
/// Level 2's institutionally-trusted S3 objects) must NOT be reused here.
pub(crate) const MAX_DECOMPRESSED_BYTES: usize = 8 << 20;

/// Digital Radial Data Array Packet — raw per-gate bytes.
const RADIAL_PACKET_CODE: i16 = 16;

/// Run-Length-Encoded radial packet (`0xAF1F` as signed big-endian `i16`) —
/// legacy 8/16-level products. `0xAF1F` doesn't fit `u16`'s complement
/// story the way the ICD prints it; reading the packet-code halfword as
/// `i16` (matching every other signed halfword in this decoder) gives
/// `-20705`, which is what xradar #392 compares against too.
const AF1F_PACKET_CODE: i16 = -20705;

/// Generic Data Packet — XDR-encoded, `u16` raw levels. Codes 176 (`RATE`)
/// and 177 (`HCLASS`, best-tilt composite — see `decode_packet28`'s doc for
/// why 177 was independently confirmed to use packet 16 instead, per a
/// 2026-08-09 fixture-based re-check that overturned this plan's original
/// assumption).
const GENERIC_PACKET_CODE: i16 = 28;

/// The only XDR component type this backend decodes — "radial" data.
/// Cross-checked against xradar #392's and MetPy's readers of the same
/// format, both of which also implement exactly this one type code.
const RADIAL_COMPONENT_TYPE: i32 = 1;

/// WMO/NOAAPORT end-of-message trailer: the header separator plus ETX.
/// Present on the dual-pol products in the Unidata bucket, absent on
/// reflectivity.
const WMO_TERMINATOR: &[u8] = b"\r\r\n\x03";

/// Hard cap on `n_radials * n_bins` (the codes array's cell count) —
/// checked BEFORE `Array2::zeros` allocates, in `u64` so it can't wrap on
/// a 32-bit `usize` target (`wasm32-unknown-unknown`, this crate's actual
/// deployment target for this backend). The largest real super-resolution
/// product is 720 x 1840 = 1,324,800 cells; this gives >3x headroom for
/// products this backend doesn't cover yet while still keeping a crafted
/// `n_radials = n_bins = 65535` (up to ~4.3 billion cells) far out of
/// reach. A `u16 x u16` product can reach `u32::MAX` on its own — well
/// past what a 32-bit `usize` multiply survives without wrapping — so
/// this check must run in a wider type, not `usize`.
pub(crate) const MAX_GATE_COUNT: u64 = 4_000_000;

/// Decompress the payload after the PDB — either raw (no `BZ` magic) or a
/// bzip2 stream — capped at [`MAX_DECOMPRESSED_BYTES`] either way.
pub(crate) fn decompress_payload(payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() >= 2 && &payload[..2] == b"BZ" {
        decompress_bzip2_capped(payload, MAX_DECOMPRESSED_BYTES)
    } else if payload.len() > MAX_DECOMPRESSED_BYTES {
        // The cap otherwise only applies on the compressed branch, so an
        // uncompressed payload would be unbounded — not an amplification
        // bomb (the attacker has to send every byte), but nothing else
        // bounds the input size either (`nexrad_level3.py:469-476`).
        Err(Level3DecodeError::UncompressedPayloadTooLarge {
            len: payload.len(),
            cap: MAX_DECOMPRESSED_BYTES,
        })
    } else {
        Ok(payload.to_vec())
    }
}

/// Incremental bzip2 decompression with a hard output cap, checked
/// *before* it's exceeded — not a one-shot decompress-then-check.
///
/// Decompresses into a FIXED-SIZE scratch slice (`Decompress::decompress`,
/// not `decompress_vec`) rather than growing `output` via
/// `Vec::reserve`/`spare_capacity_mut`: `decompress_vec`'s own doc comment
/// says it "will fill the space after its current length up to its
/// capacity" — i.e. it isn't bounded by how much headroom this function
/// *asked* for, only by whatever `Vec::reserve`'s amortized growth
/// actually allocated, which can overshoot the requested amount by up to
/// ~2x. Decompressing into `&mut scratch[..want]` instead bounds each
/// call's output to *exactly* `want` bytes, so `output.len()` can only
/// ever grow by `want` (never more) per iteration, regardless of what
/// `Vec`'s allocator does internally.
fn decompress_bzip2_capped(payload: &[u8], cap: usize) -> Result<Vec<u8>> {
    use bzip2::{Decompress, Status};

    let mut decompressor = Decompress::new(false);
    let mut output: Vec<u8> = Vec::new();
    // Output requested per call, and the scratch buffer it's decompressed
    // into. Real products are well under 1.3 MB, so this finishes in a
    // handful of iterations.
    const CHUNK: usize = 64 * 1024;
    let mut scratch = vec![0u8; CHUNK];

    loop {
        if output.len() >= cap {
            return Err(Level3DecodeError::Bzip2ExceedsCap { cap });
        }
        let want = CHUNK.min(cap - output.len());
        let consumed_before = decompressor.total_in() as usize;
        let produced_before = decompressor.total_out() as usize;
        let remaining = payload.get(consumed_before..).unwrap_or(&[]);
        let status = decompressor
            .decompress(remaining, &mut scratch[..want])
            .map_err(|e| Level3DecodeError::Bzip2Corrupt(e.to_string()))?;
        // `total_out()` is cumulative across the whole stream; the delta
        // is exactly how many bytes THIS call wrote into `scratch`.
        let produced = decompressor.total_out() as usize - produced_before;
        output.extend_from_slice(&scratch[..produced]);

        if status == Status::StreamEnd {
            let consumed = decompressor.total_in() as usize;
            let trailing = payload.get(consumed..).unwrap_or(&[]);
            check_trailing(trailing)?;
            return Ok(output);
        }
        // Non-terminal status: either more input remains (loop again), or
        // the stream is genuinely truncated — it consumed every byte we
        // gave it without reaching its own end. THREE outcomes, matching
        // the oracle's `decompressor.eof` branch (`nexrad_level3.py:484-495`):
        // reached the end (handled above), truncated mid-stream (here), or
        // about to hit the cap (the check at the top of the next
        // iteration).
        let consumed = decompressor.total_in() as usize;
        if consumed >= payload.len() && output.len() < cap {
            return Err(Level3DecodeError::Bzip2Truncated);
        }
    }
}

/// A WMO/NOAAPORT end-of-message terminator after the bzip2 stream is NOT
/// a second stream. Matched exactly (after stripping NUL padding) rather
/// than trimmed to whatever is left, because a stream cut in half and
/// concatenated with something else must still be refused — see
/// `nexrad_level3.py:496-525`. Three forms tolerated: the full terminator,
/// a bare ETX (some products carry only that), or either followed by NUL
/// padding to a block boundary.
fn check_trailing(trailing: &[u8]) -> Result<()> {
    let trimmed = trim_trailing_nul(trailing);
    if trimmed.is_empty() || trimmed == WMO_TERMINATOR || trimmed == &WMO_TERMINATOR[3..] {
        Ok(())
    } else {
        Err(Level3DecodeError::UnexpectedTrailingBytes {
            trailing_len: trimmed.len(),
        })
    }
}

fn trim_trailing_nul(b: &[u8]) -> &[u8] {
    let mut end = b.len();
    while end > 0 && b[end - 1] == 0 {
        end -= 1;
    }
    &b[..end]
}

/// The 16-byte symbology block header + layer header + 14-byte radial
/// packet header, common to packet codes 16 and `AF1F` (ICD Figures 3-10
/// and 3-11c share this layout; only what follows byte 30 differs).
struct PacketHeader {
    packet_code: i16,
    first_bin: u16,
    n_bins: u16,
    n_radials: u16,
    /// The packet's own range-scale halfword, verbatim. For elevation-
    /// bearing products it's `floor(1000 * cos(elevation))`, NOT metres
    /// (`docs/NEXRAD_LEVEL3_WASM.md` §4.4) — those products' gate spacing
    /// comes from `products::ProductSpec::bin_size` instead. For surface
    /// products (`bin_size: None`) this field genuinely IS the range scale
    /// in metres, used directly.
    range_scale_raw: u16,
}

/// Parse the shared block/layer/packet header, ending right after the
/// packet header (byte offset 30) — where per-radial data begins for both
/// packet 16 and `AF1F`.
fn read_packet_header(body: &[u8]) -> Result<PacketHeader> {
    if body.len() < 30 {
        return Err(Level3DecodeError::SymbologyTruncated { len: body.len() });
    }
    let divider = read_i16_be(body, 0).expect("checked len >= 30 above");
    let block_id = read_i16_be(body, 2).expect("checked len >= 30 above");
    if divider != -1 || block_id != 1 {
        return Err(Level3DecodeError::BadSymbologyHeader { divider, block_id });
    }

    // Skip the block header (10 B) and the layer header (6 B).
    let base = 16;
    let packet_code = read_i16_be(body, base).expect("checked len >= 30 above");
    let first_bin = read_u16_be(body, base + 2).expect("checked len >= 30 above");
    let n_bins = read_u16_be(body, base + 4).expect("checked len >= 30 above");
    // `_i_c` (base+6, i16), `_j_c` (base+8, i16) — unread, matching the
    // oracle's discard.
    let range_scale_raw = read_u16_be(body, base + 10).expect("checked len >= 30 above");
    let n_radials = read_u16_be(body, base + 12).expect("checked len >= 30 above");

    if n_radials == 0 || n_bins == 0 {
        return Err(Level3DecodeError::EmptySweep { n_radials, n_bins });
    }
    // Bound the codes-array ALLOCATION ITSELF, before either packet-16 or
    // AF1F's own per-radial parsing runs — see `MAX_GATE_COUNT`'s doc
    // comment for why this can't be replaced by packet 16's "bytes
    // available" check alone (AF1F's RLE compression breaks that
    // equivalence).
    let cells = n_radials as u64 * n_bins as u64;
    if cells > MAX_GATE_COUNT {
        return Err(Level3DecodeError::GridTooLarge {
            n_radials,
            n_bins,
            cells,
            max: MAX_GATE_COUNT,
        });
    }
    Ok(PacketHeader {
        packet_code,
        first_bin,
        n_bins,
        n_radials,
        range_scale_raw,
    })
}

/// Peek the shared 16-byte symbology-block header (divider/block_id,
/// validated) and the packet-code halfword right after it — common to
/// EVERY packet family, before any family-specific header shape is
/// assumed. Packet 16/AF1F and packet 28 diverge completely in what
/// follows byte 18 (a 14-byte fixed radial-packet header vs. an 8-byte
/// `(packet_code, reserved, num_bytes)` header into an XDR payload), so
/// `decode_symbology` must know which family it's looking at before
/// calling either [`read_packet_header`] (packet 16/AF1F-shaped) or
/// [`decode_packet28`] (XDR-shaped).
fn peek_packet_code(body: &[u8]) -> Result<i16> {
    if body.len() < 18 {
        return Err(Level3DecodeError::SymbologyTruncated { len: body.len() });
    }
    let divider = read_i16_be(body, 0).expect("checked len >= 18 above");
    let block_id = read_i16_be(body, 2).expect("checked len >= 18 above");
    if divider != -1 || block_id != 1 {
        return Err(Level3DecodeError::BadSymbologyHeader { divider, block_id });
    }
    Ok(read_i16_be(body, 16).expect("checked len >= 18 above"))
}

/// Symbology block -> decoded raw codes, one of two widths depending on
/// packet family. Packet 16 (raw per-gate bytes) and `AF1F` (RLE-encoded
/// nibbles, expanded here to the same `u8`-per-gate shape) both produce
/// [`SymbologyResult::U8`]; packet 28 (XDR, generic data packet) produces
/// [`SymbologyResult::U16`] — a real geometry and raw-code-width
/// difference, not a variant on the other two (see `decode_packet28`'s
/// doc and plan 0012 §3 for the full derivation). Azimuths are computed in
/// `f64` to match the oracle's own `np.float64` arithmetic; ALL THREE
/// packet families store the ray's LEADING EDGE, not its centre, and get
/// the same `+ width/2` correction (packet 16/AF1F via
/// `azimuth_centre_deg`, packet 28 inline in `parse_radial_component` —
/// confirmed for 28 specifically against NEXRAD ICD 2620001AC Appendix E,
/// Figure E-4, not assumed from the other two; see `decode_packet28`'s
/// doc for why an earlier pass got this wrong). Mirrors
/// `nexrad_level3.py:560-638` (packet 16) and xradar #392's
/// `_read_packet16`/`_read_packet_af1f`/`_read_generic_packet`.
pub(crate) fn decode_symbology(body: &[u8]) -> Result<SymbologyResult> {
    match peek_packet_code(body)? {
        RADIAL_PACKET_CODE | AF1F_PACKET_CODE => {
            let header = read_packet_header(body)?;
            let (azimuths, codes) = match header.packet_code {
                RADIAL_PACKET_CODE => decode_packet16(body, &header)?,
                AF1F_PACKET_CODE => decode_packet_af1f(body, &header)?,
                _ => unreachable!("peek_packet_code only routed RADIAL/AF1F codes here"),
            };
            Ok(SymbologyResult::U8 {
                azimuths,
                codes,
                range_scale_raw: header.range_scale_raw,
            })
        }
        GENERIC_PACKET_CODE => {
            let (azimuths, codes, gate_width_m, first_gate_m) = decode_packet28(body)?;
            Ok(SymbologyResult::U16 {
                azimuths,
                codes,
                gate_width_m,
                first_gate_m,
            })
        }
        other => Err(Level3DecodeError::UnsupportedPacketCode { code: other }),
    }
}

/// Decoded symbology output — an enum, not a struct with optional fields,
/// because "which width" is never ambiguous (exactly one packet family
/// produced this result) and every downstream consumer (`decode::decode`,
/// `adapter::convert`) needs to branch on it anyway; making the two shapes
/// mutually exclusive at the type level closes off a "both fields
/// populated" or "neither populated" state a pair of `Option`s would
/// allow by construction. `U8` carries [`PacketHeader::range_scale_raw`]
/// (verbatim — see its own doc for when it's usable as metres vs.
/// `cos(elevation) * 1000`); `U16` carries the XDR radial component's own
/// `gate_width`/`first_gate` directly, since packet 28 declares real
/// metres unconditionally (no elevation-dependent packing to correct for
/// — verified against a real `DPR` fixture, plan 0012 §3.1).
pub(crate) enum SymbologyResult {
    U8 {
        azimuths: Vec<f64>,
        codes: Array2<u8>,
        range_scale_raw: u16,
    },
    U16 {
        azimuths: Vec<f64>,
        codes: Array2<u16>,
        gate_width_m: f32,
        first_gate_m: f32,
    },
}

/// Ray CENTRE in degrees — `start_angle`/`delta` are tenths of a degree.
/// Shared by packet 16 and `AF1F`'s per-radial header; see
/// `docs/NEXRAD_LEVEL3_WASM.md` §4.5 for why the centre, not the stored
/// leading edge.
fn azimuth_centre_deg(start_angle: u16, delta: u16) -> f64 {
    (start_angle as f64 + delta as f64 / 2.0) / 10.0
}

/// Read one radial's shared 6-byte per-radial header — `(count,
/// start_angle, delta)`, where `count` is a raw byte count for packet 16
/// but a halfword count for `AF1F` (each caller names it accordingly).
/// Advances `*off` past the header and validates the angular width before
/// returning, matching both callers' original per-radial order: read,
/// advance, then check width.
fn read_radial_prefix(
    body: &[u8],
    off: &mut usize,
    radial: u16,
    n_radials: u16,
) -> Result<(u16, u16, u16)> {
    if *off + 6 > body.len() {
        return Err(Level3DecodeError::TruncatedAtRadial { radial, n_radials });
    }
    let count = read_u16_be(body, *off).expect("checked off + 6 <= len above");
    let start_angle = read_u16_be(body, *off + 2).expect("checked off + 6 <= len above");
    let delta = read_u16_be(body, *off + 4).expect("checked off + 6 <= len above");
    *off += 6;

    // Tenths of a degree; a radial wider than 10 deg is not a radial.
    if !(delta > 0 && delta <= 100) {
        return Err(Level3DecodeError::ImplausibleRadialWidth {
            radial,
            width_deg: delta as f32 / 10.0,
        });
    }
    Ok((count, start_angle, delta))
}

/// Packet 16: raw per-gate bytes, `n_bytes` (>= `n_bins`) declared per
/// radial ahead of its data.
fn decode_packet16(body: &[u8], header: &PacketHeader) -> Result<(Vec<f64>, Array2<u8>)> {
    let PacketHeader {
        first_bin,
        n_bins,
        n_radials,
        ..
    } = *header;
    if first_bin != 0 {
        return Err(Level3DecodeError::NonZeroFirstBin(first_bin));
    }

    // Bound the declaration against the bytes that must fill it, BEFORE
    // allocating: both dimensions are u16 straight off the wire, so an
    // 180-byte input could otherwise demand up to 65535 x 65535 = 4 GiB.
    // Computed in u64 — `read_packet_header`'s `MAX_GATE_COUNT` check
    // already rejects a grid this large before this function is even
    // called, but this stays overflow-safe independent of that ordering:
    // a raw `usize` multiply here would wrap on a 32-bit target (wasm32)
    // and silently pass a `needed` far smaller than the real requirement.
    let mut off = 30;
    let needed_u64 = n_radials as u64 * (6 + n_bins as u64);
    let available = body.len().saturating_sub(off);
    if needed_u64 > available as u64 {
        return Err(Level3DecodeError::AllocationExceedsBody {
            n_radials,
            n_bins,
            needed: needed_u64.min(usize::MAX as u64) as usize,
            available,
        });
    }

    let mut azimuths = vec![0.0f64; n_radials as usize];
    let mut codes = Array2::<u8>::zeros((n_radials as usize, n_bins as usize));

    for radial in 0..n_radials {
        let (n_bytes, start_angle, delta) = read_radial_prefix(body, &mut off, radial, n_radials)?;
        if n_bytes < n_bins || off + n_bytes as usize > body.len() {
            return Err(Level3DecodeError::RadialByteCountMismatch {
                radial,
                n_bytes,
                n_bins,
            });
        }
        azimuths[radial as usize] = azimuth_centre_deg(start_angle, delta);
        let row = &body[off..off + n_bins as usize];
        codes
            .row_mut(radial as usize)
            .as_slice_mut()
            .expect("row of a freshly-allocated Array2::zeros is always contiguous")
            .copy_from_slice(row);
        off += n_bytes as usize;
    }
    Ok((azimuths, codes))
}

/// Packet `AF1F`: run-length-encoded nibbles. Each radial declares its RLE
/// span as a **halfword count** (`n_bytes * 2` = the byte span), not a
/// direct byte count like packet 16. Each RLE byte packs a 4-bit colour
/// (low nibble, the raw code 0-15) and a 4-bit run length (high nibble);
/// expanding every byte's run must land on exactly `n_bins` gates per
/// radial. Mirrors xradar #392's `_read_packet_af1f`.
fn decode_packet_af1f(body: &[u8], header: &PacketHeader) -> Result<(Vec<f64>, Array2<u8>)> {
    let PacketHeader {
        n_bins, n_radials, ..
    } = *header;
    // AF1F has no `first_bin` concept in the ICD the way packet 16 does —
    // xradar doesn't check it either — so it's read (in `read_packet_header`)
    // but not validated here.

    let mut off = 30;
    let mut azimuths = vec![0.0f64; n_radials as usize];
    let mut codes = Array2::<u8>::zeros((n_radials as usize, n_bins as usize));

    for radial in 0..n_radials {
        let (n_halfwords, start_angle, delta) =
            read_radial_prefix(body, &mut off, radial, n_radials)?;
        let rle_byte_len = n_halfwords as usize * 2;
        if off + rle_byte_len > body.len() {
            return Err(Level3DecodeError::RadialByteCountMismatch {
                radial,
                n_bytes: n_halfwords,
                n_bins,
            });
        }
        azimuths[radial as usize] = azimuth_centre_deg(start_angle, delta);

        let rle_bytes = &body[off..off + rle_byte_len];
        let mut gate = 0usize;
        let mut row = codes.row_mut(radial as usize);
        for &b in rle_bytes {
            let color = b & 0x0F;
            let run = (b >> 4) as usize;
            if run == 0 {
                continue;
            }
            let end = gate + run;
            if end > n_bins as usize {
                break; // caught by the total-mismatch check below
            }
            for cell in row.iter_mut().take(end).skip(gate) {
                *cell = color;
            }
            gate = end;
        }
        if gate != n_bins as usize {
            return Err(Level3DecodeError::RleExpansionMismatch {
                radial,
                expanded: gate,
                expected: n_bins,
            });
        }
        off += rle_byte_len;
    }
    Ok((azimuths, codes))
}

/// Packet 28: the generic data packet — XDR-encoded (RFC 1832), `u16` raw
/// levels, a completely different byte shape from packet 16/AF1F (not
/// fixed-width radial rows at all). Returns `(azimuths, codes,
/// gate_width_m, first_gate_m)` — unlike packet 16/AF1F, gate geometry is
/// declared directly in metres inside the XDR payload itself (no
/// elevation-dependent `cos(elevation) * 1000` packing to correct for;
/// verified `gate_width=250.0`, `first_gate=125.0` against a real `DPR`
/// fixture, matching packet 16's own 125 m convention — plan 0012 §3.1).
///
/// **Azimuth convention, resolved (plan 0012 §3.3 step 6) — the SAME
/// `+ width/2` centering packet 16/AF1F's `azimuth_centre_deg` applies,
/// not a different convention.** Confirmed against the actual NEXRAD ICD
/// text (2620001AC, Appendix E, Figure E-4, "Radial Information Data
/// Structure"): `Azimuth` is documented explicitly as "Azimuth of the
/// LEADING EDGE of the radial" — the ICD's own words, for this exact
/// packet-28 field, not inferred from packet 16/AF1F's unrelated
/// convention. An EARLIER version of this function got this wrong,
/// concluding no centering was needed: that conclusion came from checking
/// two independent Python readers' low-level XDR-PARSING code (xradar
/// #392's `_unpack_radial`, MetPy's `Level3XDRParser._unpack_radial`),
/// both of which store the field verbatim and take no position on
/// centering — without also checking either reader's HIGHER-LEVEL
/// azimuth-exposing method. xradar's `get_azimuth()` (docstring: "Return
/// ray start azimuth angles") DOES take a position, applying
/// `+ get_azimuth_delta() / 2.0` uniformly across every packet family
/// including 28 — which, per the ICD text above, was the correct
/// convention all along. A real `DPR` fixture's round-looking
/// `azimuth = 0.0, 1.0, 2.0, ...` values (at `width = 1.0`) are exactly as
/// consistent with "leading edge on a regular grid" as with "already a
/// centre" — that data alone couldn't distinguish the two conventions;
/// only the ICD text could, and does.
///
/// Header layout, right after the shared 16-byte symbology block header:
/// `GEN_DATA_PACK_HEADER` = `packet_code` (`i16`, already consumed by
/// [`peek_packet_code`]) + `reserved` (`i16`) + `num_bytes` (`i32`, the
/// XDR payload's own byte length) — 8 bytes total, XDR payload starts at
/// byte 24.
fn decode_packet28(body: &[u8]) -> Result<(Vec<f64>, Array2<u16>, f32, f32)> {
    const HEADER_LEN: usize = 24; // 16 (symbology block) + 8 (GEN_DATA_PACK_HEADER)
    if body.len() < HEADER_LEN {
        return Err(Level3DecodeError::SymbologyTruncated { len: body.len() });
    }
    let num_bytes = read_i32_be(body, 20).expect("checked len >= 24 above");
    let available = body.len() - HEADER_LEN;
    if num_bytes < 0 || num_bytes as usize > available {
        return Err(Level3DecodeError::XdrTruncated {
            context: "generic data packet body",
            have: available,
            need: num_bytes.max(0) as usize,
        });
    }
    let xdr_bytes = &body[HEADER_LEN..HEADER_LEN + num_bytes as usize];
    let mut cursor = XdrCursor::new(xdr_bytes);

    // Product description block — read and discard every field this
    // backend doesn't need (the PDB already supplies lat/lon/height/vcp/
    // elevation uniformly across every packet family); consumed strictly
    // in field order so the cursor stays synced with what follows.
    // Order cross-checked against two independent readers (xradar #392's
    // `_Level3XDRParser.__call__`, MetPy's `Level3XDRParser.
    // _unpack_prod_desc`) and verified byte-exact against a real `DPR`
    // fixture — parsing this exact shape plus the one radial component
    // below consumes the declared XDR payload to precisely zero bytes
    // remaining (checked at the end of this function).
    cursor.unpack_string()?; // name
    cursor.unpack_string()?; // description
    cursor.unpack_int()?; // code
    cursor.unpack_int()?; // type
    cursor.unpack_uint()?; // prod_time
    cursor.unpack_string()?; // radar_name
    cursor.unpack_float()?; // latitude
    cursor.unpack_float()?; // longitude
    cursor.unpack_float()?; // height
    cursor.unpack_uint()?; // vol_time
    cursor.unpack_uint()?; // el_time
    cursor.unpack_float()?; // el_angle
    cursor.unpack_int()?; // vol_num
    cursor.unpack_int()?; // op_mode
    cursor.unpack_int()?; // vcp_num
    cursor.unpack_int()?; // el_num
    cursor.unpack_int()?; // compression
    cursor.unpack_int()?; // uncompressed_size

    // Product-level parameters (name/value pairs) — real fixtures carry
    // zero; consumed and discarded either way.
    cursor.unpack_counted_list(|c, _i| {
        c.unpack_string()?;
        c.unpack_string()?;
        Ok(())
    })?;

    let components = cursor.unpack_counted_list(|c, _i| {
        let type_code = c.unpack_int()?;
        if type_code != RADIAL_COMPONENT_TYPE {
            return Err(Level3DecodeError::UnsupportedXdrComponent(type_code));
        }
        parse_radial_component(c)
    })?;
    if components.len() != 1 {
        return Err(Level3DecodeError::UnexpectedXdrComponentCount(
            components.len(),
        ));
    }
    let (azimuths, codes, gate_width_m, first_gate_m) = components
        .into_iter()
        .next()
        .expect("len checked == 1 above");

    let remaining = cursor.bytes_remaining();
    if remaining != 0 {
        return Err(Level3DecodeError::XdrTrailingBytes { remaining });
    }

    Ok((azimuths, codes, gate_width_m, first_gate_m))
}

/// One `RADIAL_COMPONENT_TYPE` component: `description` (unused,
/// consumed), `gate_width`/`first_gate` (metres, returned verbatim),
/// radial-level `parameters` (unused, consumed), then `num_rads` radials
/// each contributing one row of `u16` raw codes to the output grid.
///
/// Bounds `num_rads` against [`MAX_XDR_RADIALS`] before looping, and the
/// output grid's cell count against
/// [`MAX_GATE_COUNT`] before allocating it (once `num_bins` is known from
/// the first radial) — added by a 2026-08-09 review pass (plan 0012 §0):
/// the reference oracle this was cross-checked against does neither, and
/// doesn't check that every radial declares the SAME `num_bins`, or that
/// a radial's `data` array length agrees with its own `num_bins` field —
/// this backend's no-silent-assumptions discipline checks both explicitly
/// (see [`Level3DecodeError::XdrRadialBinCountMismatch`]/
/// [`Level3DecodeError::XdrRadialDataLengthMismatch`]).
fn parse_radial_component(cursor: &mut XdrCursor) -> Result<(Vec<f64>, Array2<u16>, f32, f32)> {
    cursor.unpack_string()?; // description, unused
    let gate_width_m = cursor.unpack_float()?;
    let first_gate_m = cursor.unpack_float()?;
    cursor.unpack_counted_list(|c, _i| {
        c.unpack_string()?;
        c.unpack_string()?;
        Ok(())
    })?; // radial-level parameters, unused

    let num_rads = cursor.unpack_int()?;
    if num_rads <= 0 {
        return Err(Level3DecodeError::NonPositiveXdrGeometry {
            n_radials: num_rads,
            n_bins: 0,
        });
    }
    if num_rads > MAX_XDR_RADIALS {
        return Err(Level3DecodeError::ImplausibleXdrRadialCount {
            n_radials: num_rads,
            max: MAX_XDR_RADIALS,
        });
    }
    let num_rads = num_rads as usize;

    // `num_rads` is already bounded by `MAX_XDR_RADIALS` above, so
    // reserving its exact capacity up front is a bounded, safe allocation
    // (unlike a raw `Vec::with_capacity(num_rads)` before that check —
    // see `MAX_XDR_RADIALS`'s own doc for why this ordering matters).
    let mut azimuths = Vec::with_capacity(num_rads);
    let mut expected_bins: Option<i32> = None;
    let mut codes: Option<Array2<u16>> = None;

    for r in 0..num_rads {
        let azimuth = cursor.unpack_float()?;
        let _elevation = cursor.unpack_float()?;
        let width = cursor.unpack_float()?;
        let num_bins = cursor.unpack_int()?;
        cursor.unpack_string()?; // attributes, unused
        let data = cursor.unpack_int_array()?;

        if num_bins <= 0 {
            return Err(Level3DecodeError::NonPositiveXdrGeometry {
                n_radials: num_rads as i32,
                n_bins: num_bins,
            });
        }
        let expected = *expected_bins.get_or_insert(num_bins);
        if num_bins != expected {
            return Err(Level3DecodeError::XdrRadialBinCountMismatch {
                radial: r,
                n_bins: num_bins,
                expected,
            });
        }
        if data.len() != num_bins as usize {
            return Err(Level3DecodeError::XdrRadialDataLengthMismatch {
                radial: r,
                declared: data.len(),
                num_bins,
            });
        }

        // First radial: `num_bins` is now known — bound and allocate the
        // output grid BEFORE writing anything into it, the packet-28
        // analogue of packet 16/AF1F's pre-allocation `GridTooLarge` check
        // in `read_packet_header`.
        if codes.is_none() {
            let cells = num_rads as u64 * num_bins as u64;
            if cells > MAX_GATE_COUNT {
                return Err(Level3DecodeError::XdrGridTooLarge {
                    n_radials: num_rads as i32,
                    n_bins: num_bins,
                    cells,
                    max: MAX_GATE_COUNT,
                });
            }
            codes = Some(Array2::<u16>::zeros((num_rads, num_bins as usize)));
        }
        let grid = codes.as_mut().expect("just initialized above");
        let mut row = grid.row_mut(r);
        for (gate, &value) in data.iter().enumerate() {
            let level =
                u16::try_from(value).map_err(|_| Level3DecodeError::XdrRawLevelOutOfRange {
                    radial: r,
                    gate,
                    value,
                })?;
            row[gate] = level;
        }
        // Ray CENTRE, not the stored leading edge — NEXRAD ICD 2620001AC
        // Appendix E, Figure E-4 ("Radial Information Data Structure"):
        // "Azimuth ... Azimuth of the LEADING EDGE of the radial" — the
        // exact same convention packet 16/AF1F's `azimuth_centre_deg`
        // already corrects for, confirmed by the ICD text itself (not
        // just a reference reader's behaviour) after an earlier pass
        // wrongly concluded packet 28 needed no correction (having
        // checked only two Python readers' low-level XDR-parsing code,
        // which stores the field verbatim and takes no position on
        // centering, without checking either reader's higher-level
        // azimuth-exposing method — xradar's `get_azimuth()` docstring
        // ["Return ray start azimuth angles"] plus its callers'
        // `+ get_azimuth_delta()/2.0` convention, applied uniformly across
        // every packet family including 28, turned out to be right all
        // along). See plan 0012 §3.3 step 6 for the full resolution.
        let centred = (azimuth as f64 + width as f64 / 2.0) % 360.0;
        azimuths.push(centred);
    }

    Ok((
        azimuths,
        codes.expect("num_rads > 0 checked above; loop runs at least once"),
        gate_width_m,
        first_gate_m,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal one-radial packet-16 symbology block: block header
    /// (10 B) + layer header (6 B) + packet header (14 B) + one radial (6 B
    /// header + `n_bins` code bytes).
    fn build_symbology(n_bins: u16, codes: &[u8], start_angle: u16, delta: u16) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes()); // divider
        body.extend_from_slice(&1i16.to_be_bytes()); // block_id
        body.extend_from_slice(&[0u8; 6]); // rest of block header
        body.extend_from_slice(&[0u8; 6]); // layer header
        body.extend_from_slice(&16u16.to_be_bytes()); // packet_code
        body.extend_from_slice(&0u16.to_be_bytes()); // first_bin
        body.extend_from_slice(&n_bins.to_be_bytes()); // n_bins
        body.extend_from_slice(&999i16.to_be_bytes()); // _i_c (unused)
        body.extend_from_slice(&998i16.to_be_bytes()); // _j_c (unused)
        body.extend_from_slice(&999u16.to_be_bytes()); // _scale (unused, cos(el)*1000)
        body.extend_from_slice(&1u16.to_be_bytes()); // n_radials
        body.extend_from_slice(&(n_bins).to_be_bytes()); // n_bytes for the radial
        body.extend_from_slice(&start_angle.to_be_bytes());
        body.extend_from_slice(&delta.to_be_bytes());
        body.extend_from_slice(codes);
        body
    }

    #[test]
    fn decode_symbology_computes_ray_centre_not_start_angle() {
        let body = build_symbology(4, &[2, 3, 4, 5], 100, 10); // 10.0 deg start, 1.0 deg wide
        let SymbologyResult::U8 {
            azimuths, codes, ..
        } = decode_symbology(&body).unwrap()
        else {
            panic!("packet 16 must produce SymbologyResult::U8");
        };
        assert_eq!(azimuths.len(), 1);
        // centre = (100 + 10/2.0) / 10.0 = 10.5, NOT the 10.0 start angle.
        assert!((azimuths[0] - 10.5).abs() < 1e-9);
        assert_eq!(codes.row(0).to_vec(), vec![2, 3, 4, 5]);
    }

    #[test]
    fn decode_symbology_rejects_unsupported_packet_code() {
        let mut body = build_symbology(2, &[2, 3], 0, 10);
        body[16..18].copy_from_slice(&12i16.to_be_bytes()); // some other packet code
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::UnsupportedPacketCode { code: 12 })
        ));
    }

    // -- Packet 28 (XDR, generic data packet) — codes 176/177's packet family --

    fn xdr_string(s: &str) -> Vec<u8> {
        let mut out = (s.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(s.as_bytes());
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        out
    }

    fn xdr_int(i: i32) -> Vec<u8> {
        i.to_be_bytes().to_vec()
    }

    fn xdr_uint(u: u32) -> Vec<u8> {
        u.to_be_bytes().to_vec()
    }

    fn xdr_float(f: f32) -> Vec<u8> {
        f.to_be_bytes().to_vec()
    }

    fn xdr_int_array(data: &[i32]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        for v in data {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    fn xdr_empty_list() -> Vec<u8> {
        // count=0, leading pointer=0, no elements.
        let mut out = xdr_int(0);
        out.extend(xdr_int(0));
        out
    }

    /// One radial: `(azimuth, width, num_bins, data)` — elevation is
    /// always 0.0 (unused by this backend).
    fn xdr_radial(azimuth: f32, width: f32, num_bins: i32, data: &[i32]) -> Vec<u8> {
        let mut out = xdr_float(azimuth);
        out.extend(xdr_float(0.0)); // elevation, unused
        out.extend(xdr_float(width));
        out.extend(xdr_int(num_bins));
        out.extend(xdr_string("")); // attributes, unused
        out.extend(xdr_int_array(data));
        out
    }

    /// Build a full, valid packet-28 (generic data packet) symbology
    /// block: the shared 16-byte block/layer header, the 8-byte
    /// `GEN_DATA_PACK_HEADER`, then a minimal product description + one
    /// radial component containing `radials` (pre-built via
    /// [`xdr_radial`]). Field order matches [`decode_packet28`]'s doc
    /// comment exactly.
    fn build_generic_symbology(gate_width: f32, first_gate: f32, radials: &[Vec<u8>]) -> Vec<u8> {
        let mut xdr = Vec::new();
        xdr.extend(xdr_string("RATE")); // name
        xdr.extend(xdr_string("Digital Instantaneous Precipitation Rate")); // description
        xdr.extend(xdr_int(176)); // code
        xdr.extend(xdr_int(1)); // type
        xdr.extend(xdr_uint(0)); // prod_time
        xdr.extend(xdr_string("KLOT")); // radar_name
        xdr.extend(xdr_float(41.604)); // latitude
        xdr.extend(xdr_float(-88.085)); // longitude
        xdr.extend(xdr_float(200.0)); // height
        xdr.extend(xdr_uint(0)); // vol_time
        xdr.extend(xdr_uint(0)); // el_time
        xdr.extend(xdr_float(0.0)); // el_angle
        xdr.extend(xdr_int(1)); // vol_num
        xdr.extend(xdr_int(3)); // op_mode
        xdr.extend(xdr_int(212)); // vcp_num
        xdr.extend(xdr_int(0)); // el_num
        xdr.extend(xdr_int(0)); // compression
        xdr.extend(xdr_int(0)); // uncompressed_size
        xdr.extend(xdr_empty_list()); // product-level parameters

        // components: count=1, leading pointer, type=1 (radial), radial component
        xdr.extend(xdr_int(1));
        xdr.extend(xdr_int(0)); // leading pointer
        xdr.extend(xdr_int(RADIAL_COMPONENT_TYPE));
        xdr.extend(xdr_string("Rate Data array product output")); // description
        xdr.extend(xdr_float(gate_width));
        xdr.extend(xdr_float(first_gate));
        xdr.extend(xdr_empty_list()); // radial-level parameters
        xdr.extend(xdr_int(radials.len() as i32)); // num_rads
        for r in radials {
            xdr.extend_from_slice(r);
        }

        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes()); // divider
        body.extend_from_slice(&1i16.to_be_bytes()); // block_id
        body.extend_from_slice(&[0u8; 6]); // rest of block header
        body.extend_from_slice(&[0u8; 6]); // layer header
        body.extend_from_slice(&GENERIC_PACKET_CODE.to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes()); // reserved
        body.extend_from_slice(&(xdr.len() as i32).to_be_bytes()); // num_bytes
        body.extend_from_slice(&xdr);
        body
    }

    #[test]
    fn decode_symbology_dispatches_packet_28_to_decode_packet28() {
        let radials = vec![
            xdr_radial(0.0, 1.0, 4, &[0, 10, 60, 150]),
            xdr_radial(1.0, 1.0, 4, &[0, 0, 0, 0]),
        ];
        let body = build_generic_symbology(250.0, 125.0, &radials);
        let result = decode_symbology(&body).unwrap();
        match result {
            SymbologyResult::U16 {
                azimuths,
                codes,
                gate_width_m,
                first_gate_m,
            } => {
                // Centred, not the stored leading edge: (0.0 + 1.0/2.0),
                // (1.0 + 1.0/2.0).
                assert_eq!(azimuths, vec![0.5, 1.5]);
                assert_eq!(codes.row(0).to_vec(), vec![0u16, 10, 60, 150]);
                assert_eq!(gate_width_m, 250.0);
                assert_eq!(first_gate_m, 125.0);
            }
            SymbologyResult::U8 { .. } => panic!("packet 28 must produce SymbologyResult::U16"),
        }
    }

    #[test]
    fn decode_packet28_azimuth_gets_the_same_leading_edge_correction_as_packet16() {
        // Plan 0012 §3.3 step 6's resolved azimuth convention, confirmed
        // against NEXRAD ICD 2620001AC Appendix E Figure E-4: packet 28's
        // `azimuth` field is the LEADING edge too, same as packet 16/AF1F
        // — `+ width/2` centering applies here as well.
        let radials = vec![xdr_radial(10.0, 1.0, 2, &[0, 0])];
        let body = build_generic_symbology(250.0, 125.0, &radials);
        match decode_symbology(&body).unwrap() {
            SymbologyResult::U16 { azimuths, .. } => {
                assert!((azimuths[0] - 10.5).abs() < 1e-6); // 10.0 + 1.0/2.0
            }
            SymbologyResult::U8 { .. } => unreachable!(),
        }
    }

    #[test]
    fn decode_packet28_rejects_unsupported_component_type() {
        // Hand-build a body whose one component declares type 2, not 1.
        let mut xdr = Vec::new();
        for _ in 0..18 {
            xdr.extend(xdr_int(0)); // 18 throwaway product-desc-shaped fields
        }
        xdr.extend(xdr_empty_list()); // parameters
        xdr.extend(xdr_int(1)); // components count
        xdr.extend(xdr_int(0)); // leading pointer
        xdr.extend(xdr_int(2)); // unsupported component type

        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&GENERIC_PACKET_CODE.to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&(xdr.len() as i32).to_be_bytes());
        body.extend_from_slice(&xdr);

        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::UnsupportedXdrComponent(2))
        ));
    }

    #[test]
    fn decode_packet28_rejects_a_radial_with_a_different_bin_count() {
        let radials = vec![
            xdr_radial(0.0, 1.0, 4, &[0, 0, 0, 0]),
            xdr_radial(1.0, 1.0, 3, &[0, 0, 0]), // disagrees with radial 0's 4 bins
        ];
        let body = build_generic_symbology(250.0, 125.0, &radials);
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::XdrRadialBinCountMismatch {
                radial: 1,
                n_bins: 3,
                expected: 4,
            })
        ));
    }

    #[test]
    fn decode_packet28_rejects_a_raw_level_outside_u16_range() {
        let radials = vec![xdr_radial(0.0, 1.0, 2, &[0, 70_000])]; // > u16::MAX
        let body = build_generic_symbology(250.0, 125.0, &radials);
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::XdrRawLevelOutOfRange {
                radial: 0,
                gate: 1,
                value: 70_000,
            })
        ));
    }

    #[test]
    fn decode_packet28_rejects_a_negative_raw_level() {
        let radials = vec![xdr_radial(0.0, 1.0, 2, &[0, -1])];
        let body = build_generic_symbology(250.0, 125.0, &radials);
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::XdrRawLevelOutOfRange {
                radial: 0,
                gate: 1,
                value: -1,
            })
        ));
    }

    #[test]
    fn decode_packet28_rejects_zero_radials() {
        let body = build_generic_symbology(250.0, 125.0, &[]);
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::NonPositiveXdrGeometry { n_radials: 0, .. })
        ));
    }

    #[test]
    fn decode_packet28_rejects_trailing_bytes_after_the_declared_payload() {
        let radials = vec![xdr_radial(0.0, 1.0, 2, &[0, 0])];
        let mut body = build_generic_symbology(250.0, 125.0, &radials);
        // Append 4 extra bytes to the num_bytes field's declared length —
        // the parse itself still succeeds (those bytes come after
        // everything it reads), so this must be caught by the
        // consumed-exactly-the-payload check, not a mid-parse failure.
        let num_bytes_off = 20;
        let old_len =
            i32::from_be_bytes(body[num_bytes_off..num_bytes_off + 4].try_into().unwrap());
        body[num_bytes_off..num_bytes_off + 4].copy_from_slice(&(old_len + 4).to_be_bytes());
        body.extend_from_slice(&[0u8; 4]);
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::XdrTrailingBytes { remaining: 4 })
        ));
    }

    #[test]
    fn decode_packet28_rejects_a_crafted_grid_before_allocating() {
        // `num_rads` at the (real, plausible) cap, times a small but real
        // `num_bins` (with real, matching `data` bytes behind it — a
        // single `unpack_int_array` call is separately capped at
        // `MAX_XDR_ARRAY_LEN` elements, so `num_bins` alone can never
        // reach the multi-billion range `MAX_GATE_COUNT` is meant to
        // catch; `num_rads * num_bins` overflowing it via many
        // modestly-sized radials is the realistic version of this attack)
        // — must be rejected by `XdrGridTooLarge` right after radial 0,
        // before `Array2::zeros` is ever attempted and before radial 1
        // (which this body doesn't even provide bytes for) is read.
        let num_bins = 100;
        let radial0 = xdr_radial(0.0, 1.0, num_bins, &vec![0i32; num_bins as usize]);

        let mut xdr = Vec::new();
        for _ in 0..18 {
            xdr.extend(xdr_int(0));
        }
        xdr.extend(xdr_empty_list());
        xdr.extend(xdr_int(1));
        xdr.extend(xdr_int(0));
        xdr.extend(xdr_int(RADIAL_COMPONENT_TYPE));
        xdr.extend(xdr_string(""));
        xdr.extend(xdr_float(250.0));
        xdr.extend(xdr_float(125.0));
        xdr.extend(xdr_empty_list());
        xdr.extend(xdr_int(100_000)); // num_rads: 100_000 * 100 = 10_000_000 > MAX_GATE_COUNT
        xdr.extend(radial0);

        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&GENERIC_PACKET_CODE.to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&(xdr.len() as i32).to_be_bytes());
        body.extend_from_slice(&xdr);

        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::XdrGridTooLarge { .. })
        ));
    }

    #[test]
    fn decode_packet28_rejects_num_rads_over_the_cap() {
        let mut xdr = Vec::new();
        for _ in 0..18 {
            xdr.extend(xdr_int(0));
        }
        xdr.extend(xdr_empty_list());
        xdr.extend(xdr_int(1));
        xdr.extend(xdr_int(0));
        xdr.extend(xdr_int(RADIAL_COMPONENT_TYPE));
        xdr.extend(xdr_string(""));
        xdr.extend(xdr_float(250.0));
        xdr.extend(xdr_float(125.0));
        xdr.extend(xdr_empty_list());
        xdr.extend(xdr_int(super::super::xdr::MAX_XDR_RADIALS + 1)); // num_rads over the cap

        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&GENERIC_PACKET_CODE.to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&(xdr.len() as i32).to_be_bytes());
        body.extend_from_slice(&xdr);

        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::ImplausibleXdrRadialCount { .. })
        ));
    }

    #[test]
    fn decode_packet28_rejects_truncated_body_without_panicking() {
        // A handful of malformed/truncated inputs, arbitrary byte
        // sequences after a valid-looking generic-packet header — must
        // all be clean errors, never a panic.
        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&GENERIC_PACKET_CODE.to_be_bytes());
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&1000i32.to_be_bytes()); // num_bytes, way more than available
                                                        // no XDR payload at all
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::XdrTruncated { .. })
        ));
    }

    #[test]
    fn decode_symbology_rejects_a_crafted_grid_before_allocating() {
        // Regression test for a real bug this backend shipped with: a
        // ~100-byte crafted body declaring n_radials = n_bins = 65535
        // (packet 16) requests a ~4 GiB `Array2::<u8>::zeros` allocation
        // with no body data behind it at all. Must be rejected by
        // `GridTooLarge` before any allocation is attempted — if this
        // regresses, the test process aborts or OOMs rather than failing
        // cleanly.
        let mut body = build_symbology(2, &[2, 3], 0, 10);
        body[20..22].copy_from_slice(&65535u16.to_be_bytes()); // n_bins
        body[28..30].copy_from_slice(&65535u16.to_be_bytes()); // n_radials
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::GridTooLarge {
                n_radials: 65535,
                n_bins: 65535,
                ..
            })
        ));
    }

    #[test]
    fn decode_packet_af1f_rejects_a_crafted_grid_before_allocating() {
        // Same regression, packet AF1F: RLE compression means a tiny body
        // can legitimately declare a huge n_radials/n_bins (unlike packet
        // 16, whose per-radial byte count alone would reject this) — the
        // `GridTooLarge` guard in `read_packet_header` is what catches it
        // here, not a body-bytes-available check.
        let rle = [(2u8 << 4) | 5, (2u8 << 4) | 9];
        let mut body = build_af1f_symbology(4, &rle, 100, 10);
        body[20..22].copy_from_slice(&65535u16.to_be_bytes()); // n_bins
        body[28..30].copy_from_slice(&65535u16.to_be_bytes()); // n_radials
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::GridTooLarge {
                n_radials: 65535,
                n_bins: 65535,
                ..
            })
        ));
    }

    #[test]
    fn decode_packet16_rejects_a_grid_under_the_cap_but_over_the_body() {
        // A grid small enough to pass `GridTooLarge` (well under
        // MAX_GATE_COUNT) but that still claims more radials than the
        // body actually has bytes for — `AllocationExceedsBody`, packet
        // 16's own per-radial-byte-count guard, not the grid-size one.
        let mut body = build_symbology(2, &[2, 3], 0, 10);
        body[28..30].copy_from_slice(&1000u16.to_be_bytes()); // n_radials, body has 1
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::AllocationExceedsBody {
                n_radials: 1000,
                ..
            })
        ));
    }

    #[test]
    fn decode_symbology_rejects_nonzero_first_bin() {
        let mut body = build_symbology(2, &[2, 3], 0, 10);
        body[18..20].copy_from_slice(&1u16.to_be_bytes());
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::NonZeroFirstBin(1))
        ));
    }

    #[test]
    fn decode_symbology_rejects_implausible_radial_width() {
        let body = build_symbology(2, &[2, 3], 0, 101); // > 10 deg
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::ImplausibleRadialWidth { .. })
        ));
        let body_zero = build_symbology(2, &[2, 3], 0, 0);
        assert!(matches!(
            decode_symbology(&body_zero),
            Err(Level3DecodeError::ImplausibleRadialWidth { .. })
        ));
    }

    #[test]
    fn decode_symbology_rejects_truncated_body() {
        assert!(matches!(
            decode_symbology(b"short"),
            Err(Level3DecodeError::SymbologyTruncated { .. })
        ));
    }

    #[test]
    fn decode_symbology_tolerates_pad_byte_beyond_n_bins() {
        // n_bytes (5) > n_bins (4): a real odd-bin-count radial's pad byte.
        // Only the first n_bins bytes should be read into codes.
        //
        // This is a documented ICD behavior, not an inferred convention —
        // NEXRAD ICD 2620001AC, Figure 3-11c ("Digital Radial Data Array
        // Packet - Packet Code 16"), Note 1: "The RPG clips radials to 70
        // kft. This could result in an odd number of bins in a radial.
        // However, the radial will always be on a halfword boundary, so
        // the number of bytes in a radial may be number of bins in a
        // radial + 1." I.e. `n_bins` (the packet header field) is always
        // the true, authoritative gate count; a `n_bytes == n_bins + 1`
        // radial carries one genuine pad byte, not a dropped 1688th gate.
        // Confirmed against 13 real fixtures this session (2 odd-`n_bins`
        // cases, both exhibiting exactly this +1 pattern with the extra
        // byte always 0 across every radial; 11 even-`n_bins` cases,
        // zero mismatches) — see `radish/tests/test_nexrad_level3_xradar_oracle.rs`
        // for why xradar's PR-392 branch's raw_data needs a matching trim
        // before comparison on these two fixtures specifically.
        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&16u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&4u16.to_be_bytes()); // n_bins = 4
        body.extend_from_slice(&999i16.to_be_bytes());
        body.extend_from_slice(&998i16.to_be_bytes());
        body.extend_from_slice(&999u16.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes()); // n_radials = 1
        body.extend_from_slice(&5u16.to_be_bytes()); // n_bytes = 5 (pad byte)
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&10u16.to_be_bytes());
        body.extend_from_slice(&[7, 8, 9, 10, 0xFF]); // 5 bytes, last is pad
        let SymbologyResult::U8 { codes, .. } = decode_symbology(&body).unwrap() else {
            panic!("packet 16 must produce SymbologyResult::U8");
        };
        assert_eq!(codes.row(0).to_vec(), vec![7, 8, 9, 10]);
    }

    #[test]
    fn decompress_payload_passes_through_uncompressed_bytes() {
        let payload = b"not bzip2 data";
        assert_eq!(decompress_payload(payload).unwrap(), payload.to_vec());
    }

    #[test]
    fn decompress_payload_rejects_oversized_uncompressed() {
        let payload = vec![0u8; MAX_DECOMPRESSED_BYTES + 1];
        assert!(matches!(
            decompress_payload(&payload),
            Err(Level3DecodeError::UncompressedPayloadTooLarge { .. })
        ));
    }

    #[test]
    fn decompress_bzip2_capped_round_trips_a_real_stream() {
        use bzip2::write::BzEncoder;
        use bzip2::Compression;
        use std::io::Write;

        let original = b"NEXRAD Level 3 test payload, repeated ".repeat(200);
        let mut encoder = BzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&original).unwrap();
        let compressed = encoder.finish().unwrap();

        let out = decompress_bzip2_capped(&compressed, MAX_DECOMPRESSED_BYTES).unwrap();
        assert_eq!(out, original);
    }

    #[test]
    fn decompress_bzip2_capped_rejects_truncated_stream() {
        use bzip2::write::BzEncoder;
        use bzip2::Compression;
        use std::io::Write;

        let original = b"NEXRAD Level 3 test payload, repeated ".repeat(200);
        let mut encoder = BzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&original).unwrap();
        let compressed = encoder.finish().unwrap();
        let truncated = &compressed[..compressed.len() / 2];

        assert!(matches!(
            decompress_bzip2_capped(truncated, MAX_DECOMPRESSED_BYTES),
            Err(Level3DecodeError::Bzip2Truncated)
        ));
    }

    #[test]
    fn decompress_bzip2_capped_enforces_the_cap() {
        use bzip2::write::BzEncoder;
        use bzip2::Compression;
        use std::io::Write;

        // Highly compressible input so the compressed stream is tiny but
        // the decompressed output is well over a small cap.
        let original = vec![b'A'; 1_000_000];
        let mut encoder = BzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(&original).unwrap();
        let compressed = encoder.finish().unwrap();

        assert!(matches!(
            decompress_bzip2_capped(&compressed, 1024),
            Err(Level3DecodeError::Bzip2ExceedsCap { cap: 1024 })
        ));
    }

    #[test]
    fn check_trailing_tolerates_wmo_terminator_and_bare_etx() {
        assert!(check_trailing(b"").is_ok());
        assert!(check_trailing(WMO_TERMINATOR).is_ok());
        assert!(check_trailing(b"\x03").is_ok());
        assert!(check_trailing(b"\r\r\n\x03\x00\x00").is_ok()); // NUL-padded
        assert!(check_trailing(b"garbage").is_err());
    }

    /// Build a minimal one-radial packet-AF1F symbology block: the same
    /// 30-byte shared header as `build_symbology`, but with `AF1F_PACKET_CODE`
    /// and an RLE-encoded radial (`rle` = concatenated (run<<4 | color)
    /// bytes) instead of raw per-gate bytes.
    fn build_af1f_symbology(n_bins: u16, rle: &[u8], start_angle: u16, delta: u16) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&AF1F_PACKET_CODE.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes()); // first_bin (unchecked for AF1F)
        body.extend_from_slice(&n_bins.to_be_bytes());
        body.extend_from_slice(&999i16.to_be_bytes());
        body.extend_from_slice(&998i16.to_be_bytes());
        body.extend_from_slice(&999u16.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes()); // n_radials
                                                     // n_bytes is a HALFWORD count for AF1F, so byte len / 2 (rle.len()
                                                     // must be even for a well-formed test fixture).
        assert_eq!(
            rle.len() % 2,
            0,
            "test rle payload must be halfword-aligned"
        );
        body.extend_from_slice(&((rle.len() / 2) as u16).to_be_bytes());
        body.extend_from_slice(&start_angle.to_be_bytes());
        body.extend_from_slice(&delta.to_be_bytes());
        body.extend_from_slice(rle);
        body
    }

    #[test]
    fn decode_packet_af1f_expands_runs_to_the_declared_bin_count() {
        // 4 bins: run=2 color=5, run=2 color=9 -> [5,5,9,9]
        let rle = [(2u8 << 4) | 5, (2u8 << 4) | 9];
        let body = build_af1f_symbology(4, &rle, 100, 10);
        let SymbologyResult::U8 {
            azimuths, codes, ..
        } = decode_symbology(&body).unwrap()
        else {
            panic!("packet AF1F must produce SymbologyResult::U8");
        };
        assert!((azimuths[0] - 10.5).abs() < 1e-9);
        assert_eq!(codes.row(0).to_vec(), vec![5, 5, 9, 9]);
    }

    #[test]
    fn decode_packet_af1f_rejects_runs_that_dont_sum_to_n_bins() {
        // Declares 4 bins but the RLE only expands to 3 (1 + 2). `n_bytes`
        // is a halfword count on the wire, so the RLE span itself must
        // stay even-length; the mismatch is in the *expansion*, not the
        // byte count.
        let rle = [(1u8 << 4) | 7, (2u8 << 4) | 3];
        let body = build_af1f_symbology(4, &rle, 0, 10);
        assert!(matches!(
            decode_symbology(&body),
            Err(Level3DecodeError::RleExpansionMismatch {
                expanded: 3,
                expected: 4,
                ..
            })
        ));
    }

    #[test]
    fn decode_packet_af1f_tolerates_zero_length_runs() {
        // A zero-run byte contributes nothing but isn't itself invalid.
        let rle = [3u8, (2u8 << 4) | 5];
        let body = build_af1f_symbology(2, &rle, 0, 10);
        let SymbologyResult::U8 { codes, .. } = decode_symbology(&body).unwrap() else {
            panic!("packet AF1F must produce SymbologyResult::U8");
        };
        assert_eq!(codes.row(0).to_vec(), vec![5, 5]);
    }

    // -- Fuzz: never panic on untrusted bytes ----------------------------
    //
    // Added by the code-review pass that closed plan 0012 out — matches
    // this crate's own established convention for untrusted-input parsers
    // (`nexrad::decode::messages::tests::
    // proptest_decode_messages_never_panics_on_random_input`,
    // `nexrad::demux::tests::arbitrary_bytes_never_panic`) that the
    // earlier hand-crafted malformed-input tests above didn't actually
    // fulfill: those pin specific, anticipated failure shapes; a property
    // test pins the INVARIANT (no panic, ever) against shapes nobody
    // anticipated. `decode_packet28`/`XdrCursor` are new,
    // untrusted-input-facing parsing code — exactly the category
    // `docs/NEXRAD_LEVEL3_WASM.md` §4.7 already treats as security-relevant
    // for this backend.

    #[test]
    fn proptest_decode_symbology_never_panics_on_random_input() {
        // Broad: fully arbitrary bytes, any packet code (or none) —
        // exercises `peek_packet_code`'s own bounds checks and the
        // packet16/AF1F/28 dispatch, matching the existing crate-wide
        // convention for this style of test.
        use proptest::prelude::*;
        proptest!(|(bytes in prop::collection::vec(any::<u8>(), 0..4096))| {
            let _ = decode_symbology(&bytes);
        });
    }

    #[test]
    fn proptest_decode_packet28_never_panics_on_random_xdr_payload() {
        // Targeted: a valid block/layer header + GEN_DATA_PACK_HEADER
        // declaring packet code 28, but a fully random XDR payload (and a
        // `num_bytes` that may or may not match its real length) — fully
        // random top-level bytes would almost never land on packet code
        // 28 by chance (`peek_packet_code` would reject nearly all of
        // them before `decode_packet28` is ever reached), so this fixes
        // the header shape to actually drive the new parser under
        // fuzzing, the way `proptest_decode_symbology_never_panics_on_random_input`
        // above cannot on its own.
        use proptest::prelude::*;
        proptest!(|(
            declared_num_bytes in any::<i32>(),
            payload in prop::collection::vec(any::<u8>(), 0..2048),
        )| {
            let mut body = Vec::new();
            body.extend_from_slice(&(-1i16).to_be_bytes()); // divider
            body.extend_from_slice(&1i16.to_be_bytes()); // block_id
            body.extend_from_slice(&[0u8; 6]); // rest of block header
            body.extend_from_slice(&[0u8; 6]); // layer header
            body.extend_from_slice(&GENERIC_PACKET_CODE.to_be_bytes());
            body.extend_from_slice(&0i16.to_be_bytes()); // reserved
            body.extend_from_slice(&declared_num_bytes.to_be_bytes()); // num_bytes, possibly a lie
            body.extend_from_slice(&payload);
            let _ = decode_symbology(&body);
        });
    }
}
