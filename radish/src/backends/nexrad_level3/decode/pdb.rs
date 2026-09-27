//! Product Description Block (PDB) field reads: site position, VCP,
//! elevation, and the two-form value scaling. Mirrors
//! `nexrad_level3.py:367-466`'s halfword arithmetic and validation.
//!
//! Halfwords are numbered 1-based within the PDB, per the ICD — halfword
//! `index` starts at byte offset `pdb + 2 * (index - 1)`.

use super::bytes::{read_i16_be, read_i32_be, read_u16_be};
use super::error::{Level3DecodeError, Result};
use super::DATA_FLOOR_CODE;

fn hw_i16(raw: &[u8], pdb: usize, index: usize) -> Result<i16> {
    read_i16_be(raw, pdb + 2 * (index - 1)).ok_or(Level3DecodeError::PdbFieldOutOfBounds { index })
}

fn hw_u16(raw: &[u8], pdb: usize, index: usize) -> Result<u16> {
    read_u16_be(raw, pdb + 2 * (index - 1)).ok_or(Level3DecodeError::PdbFieldOutOfBounds { index })
}

fn hw_i32(raw: &[u8], pdb: usize, index: usize) -> Result<i32> {
    read_i32_be(raw, pdb + 2 * (index - 1)).ok_or(Level3DecodeError::PdbFieldOutOfBounds { index })
}

/// Read a big-endian `f32` scale/offset pair starting at halfword `index`
/// (4 bytes each, back to back — the float scale form's halfwords 22-25).
fn hw_f32_pair(raw: &[u8], pdb: usize, index: usize) -> Result<(f32, f32)> {
    let off = pdb + 2 * (index - 1);
    let bytes = raw
        .get(off..off + 8)
        .ok_or(Level3DecodeError::PdbFieldOutOfBounds { index })?;
    let scale = f32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let offset = f32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    Ok((scale, offset))
}

/// The PDB fields this decoder reads, before validation.
pub(crate) struct PdbFields {
    pub lat: f64,
    pub lon: f64,
    pub height_m: f64,
    pub product_code: i16,
    pub vcp: u16,
    pub scan_days: u16,
    pub scan_seconds: i32,
    pub elevation_deg: f64,
}

/// Read the fixed PDB fields this decoder needs. Caller must already have
/// verified `raw.len() >= pdb + 102` (`nexrad_level3.py:438-441`) — every
/// field read here (halfwords 2-21) sits within that span, so failures
/// here indicate a logic bug rather than a short read, but reads stay
/// checked regardless (see [`Level3DecodeError::PdbFieldOutOfBounds`]).
pub(crate) fn read_pdb_fields(raw: &[u8], pdb: usize) -> Result<PdbFields> {
    Ok(PdbFields {
        lat: hw_i32(raw, pdb, 2)? as f64 / 1000.0,
        lon: hw_i32(raw, pdb, 4)? as f64 / 1000.0,
        // The PDB carries height in feet.
        height_m: hw_i16(raw, pdb, 6)? as f64 * 0.3048,
        product_code: hw_i16(raw, pdb, 7)?,
        vcp: hw_u16(raw, pdb, 9)?,
        scan_days: hw_u16(raw, pdb, 12)?,
        scan_seconds: hw_i32(raw, pdb, 13)?,
        // f64, matching `nexrad_level3.py:454`'s `hw(21) / 10` — computing
        // this in f32 (as an earlier version did) loses precision the
        // oracle never had: e.g. 31 / 10.0f32 rounds to a value that
        // widens to 3.0999999046325684 in f64, not 3.1. Narrow to f32 only
        // at the one point that genuinely needs it — the per-radial
        // `Coordinates.elevation` array (`adapter.rs`).
        elevation_deg: hw_i16(raw, pdb, 21)? as f64 / 10.0,
    })
}

/// `(value_min, value_increment, n_levels)` from the PDB's integer
/// (`LinearHw`) scale form — halfwords 22/23/24: min x10, increment x10,
/// level count. Mirrors `nexrad_level3.py:396-401`.
///
/// Split from the float form into its own function (rather than one
/// `value_scaling(scheme)` with a 4-armed `unreachable!()` for the
/// schemes it can't handle) so calling it with the wrong scheme is a type
/// error caught by `decode/mod.rs`'s match arms, not a runtime panic on
/// real product input — see `decode/products.rs`'s `DecodeScheme` doc
/// comment for why this crate treats that distinction as worth the extra
/// function.
pub(crate) fn value_scaling_linear_hw(raw: &[u8], pdb: usize) -> Result<(f32, f32, u16)> {
    let value_min = hw_i16(raw, pdb, 22)? as f32 / 10.0;
    let value_increment = hw_i16(raw, pdb, 23)? as f32 / 10.0;
    // Not otherwise validated (matching the oracle — only
    // `value_increment` is checked, in `decode()` after this returns). A
    // negative reading here would indicate a malformed file already
    // headed for a different error upstream (or, for a well-formed one,
    // never happens).
    let n_levels = hw_i16(raw, pdb, 24)? as u16;
    Ok((value_min, value_increment, n_levels))
}

/// `(value_min, value_increment, n_levels)` from the PDB's float
/// (`FloatScale`) scale form — halfwords 22-25: a big-endian `f32` scale
/// then offset. See `docs/NEXRAD_LEVEL3_WASM.md` §4.3 for why this and
/// [`value_scaling_linear_hw`] are dispatched on an enum rather than a
/// heuristic on the values. Mirrors `nexrad_level3.py:403-419`.
///
/// Verified against real products from `s3://unidata-nexrad-level3`
/// (the oracle's own docstring, `nexrad_level3.py:386-390`):
///
/// ```text
/// N0X (159)  scale 16   offset 128    -> -7.875   .. 7.9375
/// N0C (161)  scale 300  offset -60.5  ->  0.20833 .. 1.05167
/// N0K (163)  scale 20   offset 43     -> -2.05    .. 10.6
/// ```
pub(crate) fn value_scaling_float(raw: &[u8], pdb: usize) -> Result<(f32, f32, u16)> {
    const DATA_LEVEL_COUNT: u16 = 254;

    let (scale, offset) = hw_f32_pair(raw, pdb, 22)?;
    if !scale.is_finite() || scale == 0.0 {
        return Err(Level3DecodeError::NonFiniteScale(scale));
    }
    if !offset.is_finite() {
        return Err(Level3DecodeError::NonFiniteOffset(offset));
    }
    // The level count is ASSERTED, not read: the float form spends
    // halfwords 22-25 on the scale/offset pair, so there is no
    // level-count slot where the integer form keeps one —
    // `DATA_FLOOR_CODE`..255 is what the encoding means
    // (`nexrad_level3.py:408-419`).
    Ok((
        (DATA_FLOOR_CODE as f32 - offset) / scale,
        1.0 / scale,
        DATA_LEVEL_COUNT,
    ))
}

/// `(value_min, value_increment, floor_code, valid_max)` for the
/// `Precip`/`Rate` PDB layout — the same 8-byte float32 scale/offset pair
/// as [`value_scaling_float`] (halfwords 22-25), reinterpreted in the
/// shifted-linear form `value_min + (code - floor_code) * value_increment`
/// (mathematically identical to the raw `physical = code * scale + offset`
/// form the reference oracle uses — see the derivation in plan 0012 §2.1 —
/// just reframed so this crate's existing `DeclaredLinearScale`/
/// `DeclaredScale` model, built for a per-instance floor code already,
/// covers this scheme too without a new model type), PLUS a
/// product-family leading/trailing flag-count field further into the PDB
/// (halfwords 27-29) that gives the true per-FILE floor/ceiling — NOT
/// [`super::DATA_FLOOR_CODE`]'s universal 2, which is packet 16/AF1F's
/// OTHER schemes' convention, confirmed to not apply here (see this
/// module's own doc and plan 0012 §2.2's now-confirmed hypothesis: the
/// precip/rate family's real floor is 1, read from real `DAA`/`DTA`/`DU3`/
/// `DU6` fixtures, not a hardcoded 2).
///
/// `factor` bakes in the physical-unit conversion — `0.01 * IN_TO_MM` for
/// `Precip` (raw is hundredths of an inch), `IN_TO_MM` for `Rate` (raw is
/// inches per hour) — mirrors xradar #392's `get_scale_offset`/
/// `get_flag_counts`, cross-checked against real `DAA`/`DPR` fixture bytes
/// before trusting the port (plan 0012 §2.3/§2.4).
pub(crate) fn precip_family_scale(
    raw: &[u8],
    pdb: usize,
    factor: f32,
) -> Result<(f32, f32, u8, Option<u32>)> {
    let (file_scale, file_offset) = hw_f32_pair(raw, pdb, 22)?;
    if !file_scale.is_finite() || file_scale == 0.0 {
        return Err(Level3DecodeError::NonFiniteScale(file_scale));
    }
    if !file_offset.is_finite() {
        return Err(Level3DecodeError::NonFiniteOffset(file_offset));
    }
    let out_scale = factor / file_scale;
    let out_offset = -file_offset * factor / file_scale;

    // Halfwords 27 (max data value, u16), 28/29 (leading/trailing flag
    // byte counts, i16) — xradar #392's `get_flag_counts`. Validated
    // exactly like the oracle: implausible readings fall back to the ICD
    // default rather than propagating garbage.
    let max_val = hw_u16(raw, pdb, 27)?;
    let leading = hw_i16(raw, pdb, 28)?;
    let trailing = hw_i16(raw, pdb, 29)?;
    let (floor_code, valid_max) =
        if (0..=8).contains(&leading) && (0..=8).contains(&trailing) && max_val != 0 {
            (
                leading as u8,
                Some((max_val as i32 - trailing as i32).max(0) as u32),
            )
        } else {
            // Implausible flag bytes: fall back to the fixed ICD default
            // for THIS product family specifically — `leading = 1`, one
            // sentinel code ("below threshold"), not packet 16/AF1F's
            // OTHER schemes' universal floor-of-2. No ceiling in the
            // fallback case, matching the oracle.
            (1u8, None)
        };

    // Reframe `physical = code * out_scale + out_offset` as
    // `value_min + (code - floor_code) * value_increment`:
    // value_min := physical(floor_code) = floor_code * out_scale + out_offset.
    // Exact, not an approximation — substituting back gives
    // value_min + (code - floor_code) * out_scale
    //   = floor_code*out_scale + out_offset + code*out_scale - floor_code*out_scale
    //   = code * out_scale + out_offset, i.e. the original formula.
    let value_min = floor_code as f32 * out_scale + out_offset;
    Ok((value_min, out_scale, floor_code, valid_max))
}

/// 32-byte `threshold_data` field, PDB halfwords 22-37 — the raw bytes
/// [`crate::backends::nexrad_level3::decode::legacy16::decode_legacy16`]
/// needs. Caller must already have verified `raw.len() >= pdb + 102`.
pub(crate) fn threshold_data(raw: &[u8], pdb: usize) -> Result<[u8; 32]> {
    let off = pdb + 2 * (22 - 1);
    raw.get(off..off + 32)
        .map(|b| b.try_into().expect("slice of len 32"))
        .ok_or(Level3DecodeError::PdbFieldOutOfBounds { index: 22 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdb_bytes_for_linear_float(scale: f32, offset: f32) -> Vec<u8> {
        // halfwords 1..21 as zero padding, then the f32 pair at 22-25.
        let mut buf = vec![0u8; 2 * 21];
        buf.extend_from_slice(&scale.to_be_bytes());
        buf.extend_from_slice(&offset.to_be_bytes());
        buf
    }

    #[test]
    fn value_scaling_float_form_matches_oracle_docstring_n0x() {
        let raw = pdb_bytes_for_linear_float(16.0, 128.0);
        let (min, inc, n) = value_scaling_float(&raw, 0).unwrap();
        assert!((min - (-7.875)).abs() < 1e-4);
        assert!((inc - 0.0625).abs() < 1e-6);
        assert_eq!(n, 254);
    }

    #[test]
    fn value_scaling_float_form_matches_oracle_docstring_n0c() {
        let raw = pdb_bytes_for_linear_float(300.0, -60.5);
        let (min, inc, _n) = value_scaling_float(&raw, 0).unwrap();
        assert!((min - 0.20833).abs() < 1e-4);
        assert!((inc - (1.0 / 300.0)).abs() < 1e-6);
    }

    #[test]
    fn value_scaling_float_form_matches_oracle_docstring_n0k() {
        let raw = pdb_bytes_for_linear_float(20.0, 43.0);
        let (min, inc, _n) = value_scaling_float(&raw, 0).unwrap();
        assert!((min - (-2.05)).abs() < 1e-4);
        assert!((inc - 0.05).abs() < 1e-6);
    }

    #[test]
    fn value_scaling_rejects_zero_scale() {
        let raw = pdb_bytes_for_linear_float(0.0, 128.0);
        assert!(matches!(
            value_scaling_float(&raw, 0),
            Err(Level3DecodeError::NonFiniteScale(_))
        ));
    }

    #[test]
    fn value_scaling_rejects_non_finite_scale() {
        // Distinct from the zero-scale case above: NaN/Infinity satisfy
        // `scale == 0.0`'s negation but must still be rejected by the
        // `!scale.is_finite()` half of the same check.
        let raw = pdb_bytes_for_linear_float(f32::NAN, 128.0);
        assert!(matches!(
            value_scaling_float(&raw, 0),
            Err(Level3DecodeError::NonFiniteScale(_))
        ));
        let raw_inf = pdb_bytes_for_linear_float(f32::INFINITY, 128.0);
        assert!(matches!(
            value_scaling_float(&raw_inf, 0),
            Err(Level3DecodeError::NonFiniteScale(_))
        ));
    }

    #[test]
    fn value_scaling_rejects_non_finite_offset() {
        let raw = pdb_bytes_for_linear_float(16.0, f32::NAN);
        assert!(matches!(
            value_scaling_float(&raw, 0),
            Err(Level3DecodeError::NonFiniteOffset(_))
        ));
    }

    #[test]
    fn value_scaling_integer_form_reads_min_increment_levels() {
        let mut raw = vec![0u8; 2 * 24];
        // halfword 22 = -320 (min x10 -> -32.0), 23 = 5 (increment x10 -> 0.5), 24 = 254.
        raw[2 * 21..2 * 22].copy_from_slice(&(-320i16).to_be_bytes());
        raw[2 * 22..2 * 23].copy_from_slice(&5i16.to_be_bytes());
        raw[2 * 23..2 * 24].copy_from_slice(&254i16.to_be_bytes());
        let (min, inc, n) = value_scaling_linear_hw(&raw, 0).unwrap();
        assert_eq!(min, -32.0);
        assert_eq!(inc, 0.5);
        assert_eq!(n, 254);
    }

    #[test]
    fn read_pdb_fields_decodes_lat_lon_height_vcp() {
        let mut raw = vec![0u8; 2 * 21];
        raw[2..6].copy_from_slice(&41_881i32.to_be_bytes()); // hw2 lat x1000
        raw[6..10].copy_from_slice(&(-88_084i32).to_be_bytes()); // hw4 lon x1000
        raw[2 * 5..2 * 6].copy_from_slice(&650i16.to_be_bytes()); // hw6 height (ft)
        raw[2 * 6..2 * 7].copy_from_slice(&153i16.to_be_bytes()); // hw7 product code
        raw[2 * 8..2 * 9].copy_from_slice(&215i16.to_be_bytes()); // hw9 vcp
        let fields = read_pdb_fields(&raw, 0).unwrap();
        assert!((fields.lat - 41.881).abs() < 1e-6);
        assert!((fields.lon - (-88.084)).abs() < 1e-6);
        assert!((fields.height_m - 650.0 * 0.3048).abs() < 1e-6);
        assert_eq!(fields.product_code, 153);
        assert_eq!(fields.vcp, 215);
    }

    // -- `precip_family_scale`: flag-count fallback logic -------------------
    //
    // This is the part plan 0012 §2.5 calls out as "the part most likely
    // to have a real edge case" — the boundary between "PDB flag bytes are
    // plausible, read them directly" and "fall back to the ICD default."

    fn pdb_bytes_for_precip_family(
        scale: f32,
        offset: f32,
        max_val: u16,
        leading: i16,
        trailing: i16,
    ) -> Vec<u8> {
        // halfwords 1..21 zero padding, f32 pair at 22-25, then max_val
        // (hw27), leading (hw28), trailing (hw29) — hw26 stays zero.
        let mut buf = vec![0u8; 2 * 21];
        buf.extend_from_slice(&scale.to_be_bytes());
        buf.extend_from_slice(&offset.to_be_bytes());
        buf.extend_from_slice(&[0u8; 2]); // hw26, unused
        buf.extend_from_slice(&max_val.to_be_bytes());
        buf.extend_from_slice(&leading.to_be_bytes());
        buf.extend_from_slice(&trailing.to_be_bytes());
        buf
    }

    #[test]
    fn precip_family_scale_reads_plausible_flag_bytes_directly() {
        // Real values read off a live `LOT_DAA` fixture (plan 0012 §2.4
        // step 2): scale=1.2555609941482544, offset=0.8744438886642456,
        // max_val=255, leading=1, trailing=0 — all within the oracle's own
        // validity window, so this must NOT fall back to the default.
        let raw = pdb_bytes_for_precip_family(1.255_561, 0.874_444, 255, 1, 0);
        const PRECIP_FACTOR: f32 = 0.01 * 25.4; // IN_TO_MM
        let (value_min, value_increment, floor_code, valid_max) =
            precip_family_scale(&raw, 0, PRECIP_FACTOR).unwrap();
        assert_eq!(
            floor_code, 1,
            "leading=1 must be read directly, not defaulted"
        );
        assert_eq!(valid_max, Some(255));
        // value_min is physical(floor_code=1): code=1 -> 1*out_scale+out_offset.
        let out_scale = PRECIP_FACTOR / 1.255_561;
        let out_offset = -0.874_444 * PRECIP_FACTOR / 1.255_561;
        assert!((value_increment - out_scale).abs() < 1e-6);
        assert!((value_min - (1.0 * out_scale + out_offset)).abs() < 1e-4);
    }

    #[test]
    fn precip_family_scale_falls_back_to_leading_one_on_implausible_flag_bytes() {
        // leading=99 is outside the oracle's `0..=8` validity window ->
        // must fall back to the ICD default (leading=1, no ceiling) rather
        // than propagate the garbage value.
        let raw = pdb_bytes_for_precip_family(16.0, 128.0, 255, 99, 0);
        let (_, _, floor_code, valid_max) = precip_family_scale(&raw, 0, 1.0).unwrap();
        assert_eq!(floor_code, 1);
        assert_eq!(valid_max, None);
    }

    #[test]
    fn precip_family_scale_falls_back_when_trailing_is_implausible() {
        let raw = pdb_bytes_for_precip_family(16.0, 128.0, 255, 1, -1);
        let (_, _, floor_code, valid_max) = precip_family_scale(&raw, 0, 1.0).unwrap();
        assert_eq!(floor_code, 1);
        assert_eq!(valid_max, None);
    }

    #[test]
    fn precip_family_scale_falls_back_when_max_val_is_zero() {
        // `max_val == 0` is explicitly part of the oracle's own validity
        // check, independent of leading/trailing being in-range.
        let raw = pdb_bytes_for_precip_family(16.0, 128.0, 0, 1, 0);
        let (_, _, floor_code, valid_max) = precip_family_scale(&raw, 0, 1.0).unwrap();
        assert_eq!(floor_code, 1);
        assert_eq!(valid_max, None);
    }

    #[test]
    fn precip_family_scale_accepts_the_boundary_values_zero_and_eight() {
        // The oracle's validity check is `0 <= leading <= 8` — 0 and 8
        // must both be treated as plausible, not off-by-one excluded.
        let raw = pdb_bytes_for_precip_family(16.0, 128.0, 255, 0, 8);
        let (_, _, floor_code, valid_max) = precip_family_scale(&raw, 0, 1.0).unwrap();
        assert_eq!(floor_code, 0);
        assert_eq!(valid_max, Some(255 - 8));
    }

    #[test]
    fn precip_family_scale_rejects_zero_file_scale() {
        let raw = pdb_bytes_for_precip_family(0.0, 128.0, 255, 1, 0);
        assert!(matches!(
            precip_family_scale(&raw, 0, 1.0),
            Err(Level3DecodeError::NonFiniteScale(_))
        ));
    }

    #[test]
    fn precip_family_scale_rejects_non_finite_file_offset() {
        let raw = pdb_bytes_for_precip_family(16.0, f32::NAN, 255, 1, 0);
        assert!(matches!(
            precip_family_scale(&raw, 0, 1.0),
            Err(Level3DecodeError::NonFiniteOffset(_))
        ));
    }
}
