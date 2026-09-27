//! MSG_31 per-radial data header (ICD §3.2.4.17.1 Table XVII-A).
//!
//! Layout: 32 bytes of fixed fields followed by **9 (Build-11) or
//! 10 (Build-12+) `u32` data block pointers**. The CFP (Clutter
//! Filter Power) block was added in Build 12 (March 2012), so older
//! files reserve only 9 pointer slots. `DataHeader::read` detects
//! the layout by inspecting the smallest non-zero pointer value —
//! `68` → 9-slot legacy, `72` → 10-slot modern.
//!
//! | Offset | Bytes | Field                                                  |
//! |-------:|------:|--------------------------------------------------------|
//! |      0 |     4 | Radar identifier (ICAO, 4 ASCII chars)                 |
//! |      4 |     4 | Collection time (ms past midnight GMT)                 |
//! |      8 |     2 | Modified Julian Date                                   |
//! |     10 |     2 | Azimuth Number (1..720 within elevation scan)          |
//! |     12 |     4 | Azimuth Angle (degrees, f32)                           |
//! |     16 |     1 | Compression Indicator (0=none, 1=BZIP2, 2=zlib, 3=fut) |
//! |     17 |     1 | Spare (halfword alignment)                             |
//! |     18 |     2 | Radial Length (uncompressed bytes incl. header)        |
//! |     20 |     1 | Azimuth Resolution (1=0.5°, 2=1.0°)                    |
//! |     21 |     1 | Radial Status (0=elev start, 2=elev end, ...)          |
//! |     22 |     1 | Elevation Number (1..32 within volume scan)            |
//! |     23 |     1 | Cut Sector Number (0..3)                               |
//! |     24 |     4 | Elevation Angle (degrees, f32)                         |
//! |     28 |     1 | Radial Spot Blanking Status                            |
//! |     29 |     1 | Azimuth Indexing Mode (0=none, 1..100 = 0.01..1.00°)   |
//! |     30 |     2 | Data Block Count (4..10)                               |
//! |  32-67 |    36 | Data Block Pointers ×9 (Build-11)                      |
//! |  32-71 |    40 | Data Block Pointers ×10 (Build-12+; adds CFP)          |
//!
//! Pointers are byte offsets relative to the start of the MSG_31
//! wire body (i.e. the position right after the 28-byte combined
//! TCM and Table II header). Equivalent to `danielway/nexrad`'s
//! `start_position` (`digital_radar_data::Message::parse`) and
//! xradar's `block_pointer + 12 + LEN_MSG_HEADER` (nexrad_level2.py:877).
//!
//! Layout of the pointer array (per ICD Table XVII-A, in order):
//!
//! | Index | Block                                |
//! |------:|--------------------------------------|
//! |     0 | Volume Data Constant (VOL)           |
//! |     1 | Elevation Data Constant (ELV)        |
//! |     2 | Radial Data Constant (RAD)           |
//! |     3 | Moment "REF"                         |
//! |     4 | Moment "VEL"                         |
//! |     5 | Moment "SW"                          |
//! |     6 | Moment "ZDR"                         |
//! |     7 | Moment "PHI"                         |
//! |     8 | Moment "RHO"                         |
//! |     9 | Moment "CFP"                         |

use crate::backends::nexrad::decode::error::Result;
use crate::backends::nexrad::decode::reader::SliceReader;

/// Width of the fixed (non-pointer) portion of the header.
pub(crate) const FIXED_HEADER_SIZE: usize = 32;

/// Maximum number of data block pointers (Build-12+ files).
pub(crate) const POINTER_COUNT: usize = 10;

/// Pointer-slot count for pre-Build-12 (≤2011) NEXRAD files.
pub(crate) const LEGACY_POINTER_COUNT: usize = 9;

/// Width of one pointer.
pub(crate) const POINTER_SIZE: usize = 4;

/// Wire width of the Build-12+ data header (32 fixed + 10×4 pointers).
pub(crate) const MODERN_HEADER_SIZE: usize = FIXED_HEADER_SIZE + POINTER_COUNT * POINTER_SIZE;

/// Wire width of the Build-11 data header (32 fixed + 9×4 pointers).
pub(crate) const LEGACY_HEADER_SIZE: usize =
    FIXED_HEADER_SIZE + LEGACY_POINTER_COUNT * POINTER_SIZE;

/// On-wire pointer-table layout. Build-11 (pre-March-2012) reserves
/// 9 slots; Build-12+ adds the CFP (Clutter Filter Power) block as
/// slot 10. Detection in `PointerLayout::detect` uses the smallest
/// non-zero pointer value, which equals the header's wire size by
/// construction (the first data block immediately follows the
/// header).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PointerLayout {
    /// Build-11.x and earlier — 9 pointer slots, 68-byte header.
    Legacy,
    /// Build-12+ — 10 pointer slots, 72-byte header.
    Modern,
}

impl PointerLayout {
    /// Number of pointer slots in the on-wire header.
    pub(crate) fn pointer_count(self) -> usize {
        match self {
            Self::Legacy => LEGACY_POINTER_COUNT,
            Self::Modern => POINTER_COUNT,
        }
    }

    /// Wire size of the entire data header (fixed fields + pointers).
    pub(crate) fn wire_size(self) -> usize {
        FIXED_HEADER_SIZE + self.pointer_count() * POINTER_SIZE
    }

    /// Detect from the first 9 pointer values. `Legacy` iff the
    /// smallest non-zero pointer equals `LEGACY_HEADER_SIZE` (= 68).
    /// Anything else — including all-zero, or a smallest non-zero of
    /// 72, or any other plausible/garbage value — defaults to
    /// `Modern`. Justification: Build-12+ is the dominant format
    /// since March 2012 (well over 99% of public NEXRAD volumes), so
    /// when the signal is ambiguous, `Modern` is the lower-risk
    /// guess. The lenient `parse_fixed_frame_payload` handler will
    /// fall back to `Raw` if the resulting parse fails.
    fn detect(first_nine: &[u32]) -> Self {
        let smallest = first_nine.iter().copied().filter(|p| *p != 0).min();
        match smallest {
            Some(v) if v as usize == LEGACY_HEADER_SIZE => Self::Legacy,
            _ => Self::Modern,
        }
    }
}

/// Pointer-array index for each block type. Values are stable per
/// ICD Table XVII-A and used by the dispatcher to route each
/// non-zero pointer to the right parser.
pub(crate) const PTR_VOL: usize = 0;
pub(crate) const PTR_ELV: usize = 1;
pub(crate) const PTR_RAD: usize = 2;
pub(crate) const PTR_REF: usize = 3;
pub(crate) const PTR_VEL: usize = 4;
pub(crate) const PTR_SW: usize = 5;
pub(crate) const PTR_ZDR: usize = 6;
pub(crate) const PTR_PHI: usize = 7;
pub(crate) const PTR_RHO: usize = 8;
pub(crate) const PTR_CFP: usize = 9;

/// One MSG_31 radial's fixed header (no pointers — those live in
/// `pointers`). Field types follow ICD Table XVII-A.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DataHeader {
    pub(crate) radar_identifier: [u8; 4],
    pub(crate) collection_time_ms: u32,
    pub(crate) modified_julian_date: u16,
    pub(crate) azimuth_number: u16,
    pub(crate) azimuth_angle_degrees: f32,
    pub(crate) compression_indicator: u8,
    pub(crate) radial_length: u16,
    pub(crate) azimuth_resolution_spacing: u8,
    pub(crate) radial_status: u8,
    pub(crate) elevation_number: u8,
    pub(crate) cut_sector_number: u8,
    pub(crate) elevation_angle_degrees: f32,
    pub(crate) radial_spot_blanking_status: u8,
    pub(crate) azimuth_indexing_mode: u8,
    pub(crate) data_block_count: u16,
    /// Byte offsets to data blocks, **relative to the start of the
    /// MSG_31 wire body** (= the position right after the
    /// 28-byte combined TCM + Table II header, which is also where
    /// `DataHeader::read` begins consuming bytes — matches
    /// `danielway/nexrad`'s `start_position` semantics in
    /// `digital_radar_data::Message::parse`). Zero means "block
    /// absent for this radial."
    pub(crate) pointers: [u32; POINTER_COUNT],
    /// On-wire pointer-table layout (Build-11 9-slot or Build-12+
    /// 10-slot). Only `pointers[..layout.pointer_count()]` are
    /// meaningful — slots beyond are zero-initialized.
    pub(crate) layout: PointerLayout,
}

impl DataHeader {
    /// Wire size of this on-wire header — 68 bytes for Build-11,
    /// 72 bytes for Build-12+. Used by `msg31::parse` to compute
    /// absolute block-target byte offsets.
    pub(crate) fn wire_size(&self) -> usize {
        self.layout.wire_size()
    }

    /// Parse the on-wire data header.
    pub(crate) fn read(reader: &mut SliceReader<'_>) -> Result<Self> {
        let radar_id_bytes = reader.take_bytes(4)?;
        let mut radar_identifier = [0u8; 4];
        radar_identifier.copy_from_slice(radar_id_bytes);

        let collection_time_ms = reader.read_u32_be()?;
        let modified_julian_date = reader.read_u16_be()?;
        let azimuth_number = reader.read_u16_be()?;
        let azimuth_angle_degrees = reader.read_f32_be()?;
        let compression_indicator = reader.read_u8()?;
        let _spare = reader.read_u8()?;
        let radial_length = reader.read_u16_be()?;
        let azimuth_resolution_spacing = reader.read_u8()?;
        let radial_status = reader.read_u8()?;
        let elevation_number = reader.read_u8()?;
        let cut_sector_number = reader.read_u8()?;
        let elevation_angle_degrees = reader.read_f32_be()?;
        let radial_spot_blanking_status = reader.read_u8()?;
        let azimuth_indexing_mode = reader.read_u8()?;
        let data_block_count = reader.read_u16_be()?;

        // Read the first 9 pointer slots (always present), detect
        // the layout from them, then conditionally read the 10th.
        // Using the smallest non-zero value (rather than
        // `pointers[0]` directly) handles intermediate radials where
        // the VOL block is absent (`pointers[0] == 0`).
        let mut pointers = [0u32; POINTER_COUNT];
        for slot in pointers.iter_mut().take(LEGACY_POINTER_COUNT) {
            *slot = reader.read_u32_be()?;
        }
        let layout = PointerLayout::detect(&pointers[..LEGACY_POINTER_COUNT]);
        if layout == PointerLayout::Modern {
            pointers[LEGACY_POINTER_COUNT] = reader.read_u32_be()?;
        }

        Ok(Self {
            radar_identifier,
            collection_time_ms,
            modified_julian_date,
            azimuth_number,
            azimuth_angle_degrees,
            compression_indicator,
            radial_length,
            azimuth_resolution_spacing,
            radial_status,
            elevation_number,
            cut_sector_number,
            elevation_angle_degrees,
            radial_spot_blanking_status,
            azimuth_indexing_mode,
            data_block_count,
            pointers,
            layout,
        })
    }

    /// Site identifier as a UTF-8 string. NEXRAD ICAOs are pure
    /// ASCII so the lossy conversion is safe in practice.
    pub(crate) fn icao_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.radar_identifier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 72-byte header buffer with KLOT's typical Build-19
    /// values. Used by every header test below.
    fn klot_sample() -> Vec<u8> {
        let mut buf = Vec::with_capacity(MODERN_HEADER_SIZE);
        buf.extend_from_slice(b"KLOT"); // radar_identifier
        buf.extend_from_slice(&12_345_678u32.to_be_bytes()); // collection_time_ms
        buf.extend_from_slice(&20_405u16.to_be_bytes()); // modified_julian_date
        buf.extend_from_slice(&123u16.to_be_bytes()); // azimuth_number
        buf.extend_from_slice(&147.5_f32.to_be_bytes()); // azimuth_angle
        buf.push(0); // compression_indicator (uncompressed)
        buf.push(0); // spare
        buf.extend_from_slice(&9944u16.to_be_bytes()); // radial_length
        buf.push(2); // azimuth_resolution (1.0°)
        buf.push(1); // radial_status (intermediate)
        buf.push(11); // elevation_number
        buf.push(0); // cut_sector_number
        buf.extend_from_slice(&5.098_f32.to_be_bytes()); // elevation_angle
        buf.push(0); // spot_blanking
        buf.push(0); // azimuth_indexing_mode
        buf.extend_from_slice(&10u16.to_be_bytes()); // data_block_count
                                                     // 10 pointers, all populated. pointers[0] = 72
                                                     // signals Build-12+ layout per `DataHeader::read`.
        let modern_first_ptr = MODERN_HEADER_SIZE as u32; // 72
        for idx in 0..POINTER_COUNT as u32 {
            buf.extend_from_slice(&(modern_first_ptr + idx * 1000).to_be_bytes());
        }
        debug_assert_eq!(buf.len(), MODERN_HEADER_SIZE);
        buf
    }

    #[test]
    fn read_consumes_exactly_72_bytes() {
        let bytes = klot_sample();
        let mut r = SliceReader::new(&bytes);
        let _ = DataHeader::read(&mut r).unwrap();
        assert_eq!(r.position(), MODERN_HEADER_SIZE);
    }

    #[test]
    fn round_trip_decodes_klot_fields() {
        let bytes = klot_sample();
        let mut r = SliceReader::new(&bytes);
        let h = DataHeader::read(&mut r).unwrap();
        assert_eq!(&h.radar_identifier, b"KLOT");
        assert_eq!(h.icao_str(), "KLOT");
        assert_eq!(h.collection_time_ms, 12_345_678);
        assert_eq!(h.modified_julian_date, 20_405);
        assert_eq!(h.azimuth_number, 123);
        assert!((h.azimuth_angle_degrees - 147.5).abs() < 1e-6);
        assert_eq!(h.compression_indicator, 0);
        assert_eq!(h.radial_length, 9944);
        assert_eq!(h.azimuth_resolution_spacing, 2);
        assert_eq!(h.radial_status, 1);
        assert_eq!(h.elevation_number, 11);
        assert_eq!(h.cut_sector_number, 0);
        assert!((h.elevation_angle_degrees - 5.098).abs() < 1e-6);
        assert_eq!(h.radial_spot_blanking_status, 0);
        assert_eq!(h.azimuth_indexing_mode, 0);
        assert_eq!(h.data_block_count, 10);
        // Every pointer populated to its expected value (Build-12+
        // layout: pointers[0] = 72, then +1000 per slot).
        let modern_first_ptr = MODERN_HEADER_SIZE as u32;
        for idx in 0..POINTER_COUNT {
            assert_eq!(h.pointers[idx], modern_first_ptr + idx as u32 * 1000);
        }
        assert_eq!(
            h.layout,
            PointerLayout::Modern,
            "Build-12+ layout (pointers[0] == 72) → Modern (10 slots)"
        );
        assert_eq!(h.wire_size(), MODERN_HEADER_SIZE);
    }

    /// Build a 68-byte pre-Build-12 (Build-11.x) data header. Mirrors
    /// the wire layout actually observed in
    /// `KVNX20110520_000442_V06`: 32-byte fixed header, 9 pointer
    /// slots, no CFP. The first pointer's value is exactly the
    /// header's wire size (68), which is the detection signal.
    fn build11_kvnx_sample() -> Vec<u8> {
        let mut buf = Vec::with_capacity(LEGACY_HEADER_SIZE);
        buf.extend_from_slice(b"KVNX");
        buf.extend_from_slice(&282u32.to_be_bytes());
        buf.extend_from_slice(&15_480u16.to_be_bytes());
        buf.extend_from_slice(&73u16.to_be_bytes());
        buf.extend_from_slice(&237.27_f32.to_be_bytes());
        buf.push(0);
        buf.push(0);
        buf.extend_from_slice(&5_624u16.to_be_bytes());
        buf.push(2);
        buf.push(3); // radial_status = ScanStart
        buf.push(1);
        buf.push(0);
        buf.extend_from_slice(&0.6757_f32.to_be_bytes());
        buf.push(0);
        buf.push(0);
        buf.extend_from_slice(&7u16.to_be_bytes()); // 7 valid blocks
        let legacy_ptrs: [u32; LEGACY_POINTER_COUNT] = [68, 112, 124, 144, 2004, 3224, 5636, 0, 0];
        for ptr in legacy_ptrs {
            buf.extend_from_slice(&ptr.to_be_bytes());
        }
        debug_assert_eq!(buf.len(), LEGACY_HEADER_SIZE);
        buf
    }

    #[test]
    fn build11_layout_detected_when_first_pointer_equals_legacy_size() {
        let bytes = build11_kvnx_sample();
        let mut r = SliceReader::new(&bytes);
        let h = DataHeader::read(&mut r).unwrap();
        assert_eq!(r.position(), LEGACY_HEADER_SIZE);
        assert_eq!(h.layout, PointerLayout::Legacy);
        assert_eq!(h.wire_size(), LEGACY_HEADER_SIZE);
        assert_eq!(&h.radar_identifier, b"KVNX");
        assert_eq!(h.data_block_count, 7);
        assert_eq!(h.pointers[0], 68, "Build-11 first pointer == header size");
        assert_eq!(h.pointers[6], 5636);
        assert_eq!(h.pointers[7], 0);
        assert_eq!(h.pointers[8], 0);
        assert_eq!(h.pointers[9], 0, "Build-11 has no slot 10");
    }

    /// Critical: when a Build-11 header is followed by data-block
    /// bytes, `DataHeader::read` must NOT consume the first 4 bytes
    /// of the next block as a phantom 10th pointer.
    #[test]
    fn build11_layout_does_not_consume_following_data_block_bytes() {
        let mut bytes = build11_kvnx_sample();
        // Append what would be the first data block's first 4 bytes
        // (e.g. `b"DVOL"`). Read must leave these bytes alone.
        bytes.extend_from_slice(b"DVOL");
        let mut r = SliceReader::new(&bytes);
        let _ = DataHeader::read(&mut r).unwrap();
        assert_eq!(
            r.position(),
            LEGACY_HEADER_SIZE,
            "must stop at byte 68 in Build-11 mode, not at 72"
        );
        // The DVOL bytes should still be there for the block parser.
        assert_eq!(r.remaining(), b"DVOL");
    }

    #[test]
    fn read_errors_on_short_input() {
        let bytes = klot_sample();
        let mut r = SliceReader::new(&bytes[..32]);
        assert!(DataHeader::read(&mut r).is_err());
    }

    /// Verify the load-bearing property `pointers[0] == wire_size()`
    /// for both layouts — this is the construction-time invariant
    /// that `PointerLayout::detect` relies on.
    #[test]
    fn first_pointer_equals_header_wire_size_in_both_layouts() {
        let modern = klot_sample();
        let mh = DataHeader::read(&mut SliceReader::new(&modern)).unwrap();
        assert_eq!(
            mh.pointers[0] as usize,
            mh.wire_size(),
            "Build-12+: pointers[0] must equal 72-byte header size"
        );

        let legacy = build11_kvnx_sample();
        let lh = DataHeader::read(&mut SliceReader::new(&legacy)).unwrap();
        assert_eq!(
            lh.pointers[0] as usize,
            lh.wire_size(),
            "Build-11: pointers[0] must equal 68-byte header size"
        );
    }

    /// All-zero pointer table → defaults to Modern (10 slots, 72-byte
    /// header). Justified by the dominance of Build-12+ files post-
    /// 2012; the lenient `parse_fixed_frame_payload` will fall back
    /// to Raw if the resulting block walk fails on a corrupt radial.
    #[test]
    fn all_zero_pointers_defaults_to_modern_layout() {
        let mut bytes = klot_sample();
        // Zero out every pointer slot.
        for idx in 0..POINTER_COUNT {
            let off = FIXED_HEADER_SIZE + idx * POINTER_SIZE;
            bytes[off..off + POINTER_SIZE].copy_from_slice(&0u32.to_be_bytes());
        }
        let mut r = SliceReader::new(&bytes);
        let h = DataHeader::read(&mut r).unwrap();
        assert_eq!(h.layout, PointerLayout::Modern);
        assert_eq!(r.position(), MODERN_HEADER_SIZE);
    }

    #[test]
    fn unused_pointers_are_preserved_as_zero() {
        let mut bytes = klot_sample();
        // Zero out the CFP pointer (index 9, byte offset 32+36=68).
        let cfp_ptr_offset = FIXED_HEADER_SIZE + PTR_CFP * POINTER_SIZE;
        bytes[cfp_ptr_offset..cfp_ptr_offset + 4].copy_from_slice(&0u32.to_be_bytes());
        let mut r = SliceReader::new(&bytes);
        let h = DataHeader::read(&mut r).unwrap();
        assert_eq!(h.pointers[PTR_CFP], 0, "CFP absent → pointer is 0");
        assert_ne!(h.pointers[PTR_REF], 0, "other pointers untouched");
    }
}
