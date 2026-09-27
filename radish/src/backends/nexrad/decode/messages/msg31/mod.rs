//! MSG_31 (Digital Radar Data Generic Format) parser — ICD §3.2.4.17.
//!
//! Parsing strategy:
//!
//! 1. Read the data header — 32 fixed bytes plus 9 (Build-11) or 10
//!    (Build-12+) `u32` pointers; see `header::DataHeader::read` and
//!    `header::PointerLayout`.
//! 2. For each non-zero pointer (capped to `data_block_count` and the
//!    detected slot count), seek the reader to `body_start + ptr`,
//!    read the 4-byte `DataBlockId`, and route by **name** to the
//!    matching block parser. Pointer offsets are byte offsets from
//!    the start of the MSG_31 wire body (i.e. immediately after the
//!    28-byte combined TCM and Table II header). Slot indices in
//!    ICD Table XVII-A are advisory only — real files pack the
//!    valid blocks contiguously into the leading slots regardless
//!    of which ICD type each slot would conventionally hold.
//! 3. Pointer order in the file is not guaranteed; we sort and walk
//!    in order so the forward-only reader always advances.

pub(crate) mod cfp;
pub(crate) mod header;
pub(crate) mod info_blocks;
pub(crate) mod moment;

use crate::backends::nexrad::decode::error::{NexradDecodeError, Result};
use crate::backends::nexrad::decode::reader::SliceReader;

use cfp::CfpBlock;
use header::DataHeader;
#[cfg(test)]
use header::MODERN_HEADER_SIZE;
use info_blocks::{DataBlockId, ElevationBlock, RadialBlock, VolumeBlock};
use moment::MomentBlock;

/// Decoded MSG_31 message (one radial). Borrowed gate slices live
/// in the `MomentBlock` / `CfpBlock` payloads so the whole struct
/// references the underlying record buffer.
#[derive(Debug)]
pub(crate) struct Msg31<'a> {
    pub(crate) header: DataHeader,
    pub(crate) volume: Option<VolumeBlock>,
    pub(crate) elevation: Option<ElevationBlock>,
    pub(crate) radial: Option<RadialBlock>,
    pub(crate) reflectivity: Option<MomentBlock<'a>>,
    pub(crate) velocity: Option<MomentBlock<'a>>,
    pub(crate) spectrum_width: Option<MomentBlock<'a>>,
    pub(crate) zdr: Option<MomentBlock<'a>>,
    pub(crate) phi: Option<MomentBlock<'a>>,
    pub(crate) rho: Option<MomentBlock<'a>>,
    pub(crate) cfp: Option<CfpBlock<'a>>,
}

/// Parse a single MSG_31 message. `body_start` is the offset of
/// the MSG_31 wire body in the input buffer (= `reader.position()`
/// at the call site, equivalently `tcm_start + 28` since the caller
/// has already consumed the 12-byte TCM prefix + 16-byte Table II
/// header). The argument is kept rather than reassigned from
/// `reader.position()` inside so the caller stays in control of
/// which position is canonical.
///
/// Block pointers in the on-wire header are byte offsets relative
/// to `body_start`, matching `danielway/nexrad`'s `start_position`
/// (`digital_radar_data::Message::parse`) and xradar's
/// `block_pointer + 12 + LEN_MSG_HEADER` arithmetic
/// (`nexrad_level2.py:877`).
pub(crate) fn parse<'a>(reader: &mut SliceReader<'a>, body_start: usize) -> Result<Msg31<'a>> {
    debug_assert_eq!(
        reader.position(),
        body_start,
        "reader must be positioned at the MSG_31 wire body start \
         (= post TCM + Table II header) — see parse() docstring",
    );
    let header = DataHeader::read(reader)?;

    // Resolve pointers and route blocks **by data_name**, not by
    // slot index. Per empirical observation (matched by both
    // `danielway/nexrad` and xradar): NEXRAD files pack the
    // `data_block_count` valid blocks contiguously into the
    // pointer slots in arrival order, and the slot index in ICD
    // §3.2.4.17.1 Table XVII-A doesn't determine the block's
    // type — the 4-byte `DataBlockId.name` does. KLOT surveillance
    // sweeps demonstrate this: a `data_block_count = 8` radial
    // with no VEL/SW packs {VOL, ELV, RAD, REF, ZDR, PHI, RHO, CFP}
    // into the 8 valid pointer slots. Routing by index would
    // mislabel ZDR's gate bytes as VEL.
    //
    // Cap to the on-wire pointer-slot count (9 for Build-11, 10
    // for Build-12+ — `header.layout.pointer_count()`), then to
    // `data_block_count`, drop zero slots, and sort by ptr so the
    // forward-only reader walks forward.
    let valid_len = (header.data_block_count as usize).min(header.layout.pointer_count());
    let mut sorted_pointers: Vec<u32> = header
        .pointers
        .iter()
        .copied()
        .take(valid_len)
        .filter(|ptr| *ptr != 0)
        .collect();
    sorted_pointers.sort();

    let mut volume: Option<VolumeBlock> = None;
    let mut elevation: Option<ElevationBlock> = None;
    let mut radial: Option<RadialBlock> = None;
    let mut reflectivity: Option<MomentBlock<'a>> = None;
    let mut velocity: Option<MomentBlock<'a>> = None;
    let mut spectrum_width: Option<MomentBlock<'a>> = None;
    let mut zdr: Option<MomentBlock<'a>> = None;
    let mut phi: Option<MomentBlock<'a>> = None;
    let mut rho: Option<MomentBlock<'a>> = None;
    let mut cfp: Option<CfpBlock<'a>> = None;

    for ptr in sorted_pointers {
        let target = body_start.checked_add(ptr as usize).ok_or(
            NexradDecodeError::InvalidPointerOffset {
                block: "<unknown>",
                offset: ptr,
                message_size: 0,
            },
        )?;
        reader.try_skip_to(target)?;

        // Read the 4-byte DataBlockId, then route by `name`.
        // Spectrum width is `b"SW "` (2-char name padded with
        // space, per ICD Table XVII-B). Unknown names skip the
        // block (forward compat with future ICD revisions).
        let id = DataBlockId::read(reader)?;
        match &id.name {
            b"VOL" => volume = Some(VolumeBlock::read(reader)?),
            b"ELV" => elevation = Some(ElevationBlock::read(reader)?),
            b"RAD" => radial = Some(RadialBlock::read(reader)?),
            b"REF" => reflectivity = Some(MomentBlock::read(reader)?),
            b"VEL" => velocity = Some(MomentBlock::read(reader)?),
            b"SW " => spectrum_width = Some(MomentBlock::read(reader)?),
            b"ZDR" => zdr = Some(MomentBlock::read(reader)?),
            b"PHI" => phi = Some(MomentBlock::read(reader)?),
            b"RHO" => rho = Some(MomentBlock::read(reader)?),
            b"CFP" => cfp = Some(CfpBlock::read(reader)?),
            _ => {
                // Unknown block type — skip silently so future ICD
                // revisions don't break us. Reader stays at the
                // position right after the DataBlockId; the next
                // pointer's `try_skip_to` will advance over the
                // unknown payload.
            }
        }
    }

    Ok(Msg31 {
        header,
        volume,
        elevation,
        radial,
        reflectivity,
        velocity,
        spectrum_width,
        zdr,
        phi,
        rho,
        cfp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use header::{POINTER_COUNT, PTR_ELV, PTR_REF};

    /// Build a minimal MSG_31 payload (header + REF block only).
    /// `message_start_offset` here is 0 since the synthetic input
    /// has no TCM/Table-II header in front.
    fn synth_msg31_with_ref_only(num_gates: u16) -> Vec<u8> {
        let header_offset = 0usize;
        let ref_offset = MODERN_HEADER_SIZE;
        // REF block: 4-byte DataBlockId + 24-byte descriptor + N gates
        let mut buf = Vec::new();

        // Build header (with only REF pointer set).
        buf.extend_from_slice(b"KLOT");
        buf.extend_from_slice(&0u32.to_be_bytes()); // collection_time_ms
        buf.extend_from_slice(&20_405u16.to_be_bytes()); // date
        buf.extend_from_slice(&1u16.to_be_bytes()); // azimuth_number
        buf.extend_from_slice(&0.5_f32.to_be_bytes()); // azimuth_angle
        buf.push(0); // compression
        buf.push(0); // spare
        buf.extend_from_slice(
            &((MODERN_HEADER_SIZE + 4 + 24 + num_gates as usize) as u16).to_be_bytes(),
        ); // radial_length
        buf.push(2); // az_resolution
        buf.push(1); // radial_status
        buf.push(1); // elevation_number
        buf.push(0); // cut_sector
        buf.extend_from_slice(&0.5_f32.to_be_bytes()); // elevation_angle
        buf.push(0); // spot_blanking
        buf.push(0); // azimuth_indexing
        buf.extend_from_slice(&4u16.to_be_bytes()); // data_block_count

        // 10 pointers — only REF (index 3) is non-zero.
        for idx in 0..POINTER_COUNT {
            let ptr = if idx == PTR_REF { ref_offset as u32 } else { 0 };
            buf.extend_from_slice(&ptr.to_be_bytes());
        }
        debug_assert_eq!(buf.len(), MODERN_HEADER_SIZE);

        // REF block at offset MODERN_HEADER_SIZE.
        buf.extend_from_slice(b"DREF"); // DataBlockId
                                        // Descriptor (24 bytes)
        buf.extend_from_slice(&0u32.to_be_bytes()); // reserved
        buf.extend_from_slice(&num_gates.to_be_bytes());
        buf.extend_from_slice(&2_000u16.to_be_bytes()); // first gate 2.000 km
        buf.extend_from_slice(&250u16.to_be_bytes()); // interval 0.250 km
        buf.extend_from_slice(&0u16.to_be_bytes()); // tover
        buf.extend_from_slice(&0_i16.to_be_bytes()); // snr
        buf.push(0); // ctrl flags
        buf.push(8); // 8-bit gates
        buf.extend_from_slice(&2.0_f32.to_be_bytes()); // scale
        buf.extend_from_slice(&66.0_f32.to_be_bytes()); // offset
                                                        // Gate bytes — sequential 2..2+num_gates so we can verify decode.
        for n in 0..num_gates {
            buf.push((2 + n) as u8);
        }
        debug_assert_eq!(buf.len(), MODERN_HEADER_SIZE + 4 + 24 + num_gates as usize);
        let _ = header_offset;
        buf
    }

    #[test]
    fn parse_msg31_with_ref_only_yields_no_other_blocks() {
        let bytes = synth_msg31_with_ref_only(8);
        let mut r = SliceReader::new(&bytes);
        let msg = parse(&mut r, 0).expect("parse");
        assert_eq!(&msg.header.radar_identifier, b"KLOT");
        assert!(msg.reflectivity.is_some());
        assert!(msg.velocity.is_none());
        assert!(msg.volume.is_none());
        assert!(msg.cfp.is_none());

        // 8 gates: raws 2..9 → values (raw - 66) / 2.
        let ref_block = msg.reflectivity.unwrap();
        let values: Vec<_> = ref_block.iter().collect();
        assert_eq!(values.len(), 8);
        for (i, v) in values.iter().enumerate() {
            match v {
                moment::MomentValue::Value(x) => {
                    let expected = (2.0 + i as f32 - 66.0) / 2.0;
                    assert!((x - expected).abs() < 1e-6, "gate {i}: {x} vs {expected}");
                }
                other => panic!("gate {i}: expected Value, got {other:?}"),
            }
        }
    }

    #[test]
    fn parse_handles_pointers_in_arbitrary_order() {
        // Build a header with REF at offset A, ELV at offset B,
        // where B < A. The dispatcher must sort by offset, walk
        // forward only, and still populate both fields correctly.
        let header_offset = 0usize;

        // ELV block (8 bytes payload + 4 byte id) at the lower offset.
        let elv_offset = MODERN_HEADER_SIZE;
        // REF block (28-byte descriptor + 4 byte id + 4 gates) at the higher offset.
        let ref_offset = elv_offset + 4 + 8;

        let mut buf = Vec::new();
        // Header
        buf.extend_from_slice(b"KLOT");
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&1u16.to_be_bytes());
        buf.extend_from_slice(&0_f32.to_be_bytes());
        buf.push(0);
        buf.push(0);
        buf.extend_from_slice(&500u16.to_be_bytes());
        buf.push(2);
        buf.push(1);
        buf.push(1);
        buf.push(0);
        buf.extend_from_slice(&0_f32.to_be_bytes());
        buf.push(0);
        buf.push(0);
        buf.extend_from_slice(&5u16.to_be_bytes());
        // Pointers: ELV gets the lower offset, REF the higher.
        // The header serializes them in pointer-index order
        // (VOL=0, ELV=1, RAD=2, REF=3, ...) so ELV at index 1 and
        // REF at index 3. Even though REF appears later in the
        // pointer array, its byte offset can be later or earlier;
        // here we make sure ELV's offset < REF's offset so the
        // sort-by-offset walk visits ELV first.
        for idx in 0..POINTER_COUNT {
            let ptr = match idx {
                PTR_ELV => elv_offset as u32,
                PTR_REF => ref_offset as u32,
                _ => 0,
            };
            buf.extend_from_slice(&ptr.to_be_bytes());
        }

        // ELV: 4-byte id + 8-byte payload (lrtup, atmos, calib).
        buf.extend_from_slice(b"DELV");
        buf.extend_from_slice(&12u16.to_be_bytes()); // lrtup
        buf.extend_from_slice(&(-15_i16).to_be_bytes());
        buf.extend_from_slice(&36.0_f32.to_be_bytes());

        // REF: 4-byte id + 24-byte descriptor + 4 gates.
        buf.extend_from_slice(b"DREF");
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&4u16.to_be_bytes()); // gate count
        buf.extend_from_slice(&2_000u16.to_be_bytes());
        buf.extend_from_slice(&250u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0_i16.to_be_bytes());
        buf.push(0);
        buf.push(8);
        buf.extend_from_slice(&2.0_f32.to_be_bytes());
        buf.extend_from_slice(&66.0_f32.to_be_bytes());
        buf.extend_from_slice(&[2, 3, 4, 5]);

        let mut r = SliceReader::new(&buf);
        let msg = parse(&mut r, header_offset).expect("parse");
        assert!(msg.elevation.is_some(), "ELV pointer was non-zero");
        assert!(msg.reflectivity.is_some(), "REF pointer was non-zero");
        let e = msg.elevation.unwrap();
        assert_eq!(e.lrtup, 12);
        assert!((e.atmospheric_attenuation_db_per_km - (-0.015)).abs() < 1e-6);
    }

    /// **Audit regression test (HIGH).** Pin the route-by-name
    /// behavior. ICD §3.2.4.17.1 Table XVII-A lists pointer slots
    /// in a fixed order (VOL=0, ELV=1, RAD=2, REF=3, VEL=4, SW=5,
    /// ZDR=6, PHI=7, RHO=8, CFP=9), but real files pack
    /// `data_block_count` valid blocks contiguously in arrival
    /// order — so a radial with no VEL/SW (e.g. KLOT surveillance
    /// sweeps) puts ZDR data into pointer slot 4, which ICD says is
    /// VEL. The parser must route by `DataBlockId.name`, not by
    /// slot index, matching xradar and `danielway/nexrad`.
    ///
    /// This test puts a `DZDR` block at the slot that ICD
    /// assigns to VEL and verifies the parser populates
    /// `differential_reflectivity` (not `velocity`).
    #[test]
    fn parse_routes_by_block_name_not_slot_index() {
        let header_offset = 0usize;
        let zdr_offset = MODERN_HEADER_SIZE; // first block right after the 72-byte data header

        let mut buf = Vec::new();
        // Header — minimal valid radial.
        buf.extend_from_slice(b"KLOT");
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&1u16.to_be_bytes());
        buf.extend_from_slice(&0_f32.to_be_bytes());
        buf.push(0);
        buf.push(0);
        buf.extend_from_slice(&500u16.to_be_bytes());
        buf.push(2);
        buf.push(1);
        buf.push(1);
        buf.push(0);
        buf.extend_from_slice(&0_f32.to_be_bytes());
        buf.push(0);
        buf.push(0);
        // data_block_count = 5 — the loop iterates slots 0..5 and
        // skips zero-valued ones, leaving slot 4 (ICD's PTR_VEL
        // position) as the only one that points at real data.
        buf.extend_from_slice(&5u16.to_be_bytes());

        // Pointers: put the (non-zero) pointer at slot 4 — ICD's
        // PTR_VEL position. The block at the offset is `DZDR`, so a
        // route-by-index parser would mislabel it as velocity; a
        // route-by-name parser routes correctly.
        for idx in 0..POINTER_COUNT {
            let ptr = if idx == 4 { zdr_offset as u32 } else { 0 };
            buf.extend_from_slice(&ptr.to_be_bytes());
        }
        debug_assert_eq!(buf.len(), MODERN_HEADER_SIZE);

        // ZDR block — `DZDR` id + 24-byte descriptor + 2 gates.
        buf.extend_from_slice(b"DZDR");
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&2u16.to_be_bytes());
        buf.extend_from_slice(&2_000u16.to_be_bytes());
        buf.extend_from_slice(&250u16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0_i16.to_be_bytes());
        buf.push(0);
        buf.push(8);
        buf.extend_from_slice(&10.0_f32.to_be_bytes());
        buf.extend_from_slice(&100.0_f32.to_be_bytes());
        buf.extend_from_slice(&[42, 43]);

        let mut r = SliceReader::new(&buf);
        let msg = parse(&mut r, header_offset).expect("parse");

        // Routing was by name, so the ZDR slot is populated and
        // VEL stays None — even though the pointer sat in ICD's
        // PTR_VEL slot.
        assert!(
            msg.zdr.is_some(),
            "ZDR block must be routed by name, not by slot index"
        );
        assert!(
            msg.velocity.is_none(),
            "VEL must be None — slot 4 carried a DZDR block, not DVEL"
        );
    }

    /// Forward-compat: an unknown 3-letter block name (future ICD
    /// revision adds a new moment) should be skipped silently
    /// rather than failing the parse.
    #[test]
    fn parse_skips_unknown_block_names_for_forward_compat() {
        let header_offset = 0usize;
        let unk_offset = MODERN_HEADER_SIZE;

        let mut buf = Vec::new();
        buf.extend_from_slice(b"KLOT");
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&1u16.to_be_bytes());
        buf.extend_from_slice(&0_f32.to_be_bytes());
        buf.push(0);
        buf.push(0);
        buf.extend_from_slice(&500u16.to_be_bytes());
        buf.push(2);
        buf.push(1);
        buf.push(1);
        buf.push(0);
        buf.extend_from_slice(&0_f32.to_be_bytes());
        buf.push(0);
        buf.push(0);
        buf.extend_from_slice(&1u16.to_be_bytes());
        for idx in 0..POINTER_COUNT {
            let ptr = if idx == 0 { unk_offset as u32 } else { 0 };
            buf.extend_from_slice(&ptr.to_be_bytes());
        }
        // Unknown block name — `DXYZ`. Not a real ICD block.
        buf.extend_from_slice(b"DXYZ");

        let mut r = SliceReader::new(&buf);
        let msg = parse(&mut r, header_offset).expect("parse must not fail on unknown block");
        // Every block field stays None; the parser advanced past
        // the unknown DataBlockId without exploding.
        assert!(msg.volume.is_none());
        assert!(msg.elevation.is_none());
        assert!(msg.reflectivity.is_none());
        assert!(msg.velocity.is_none());
    }
}
