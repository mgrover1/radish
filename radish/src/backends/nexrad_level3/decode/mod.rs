//! NEXRAD Level 3 (NIDS) byte-level decoder — top-level orchestration.
//! Mirrors `nexrad_level3.py:422-557`'s `decode()` for message
//! header/PDB/bzip2/packet-16, extended in Phase 3 with packet AF1F and
//! the message-code-driven product table ported from xradar #392.
//!
//! Layout (a halfword is 2 bytes, big-endian; the ICD numbers them 1-based
//! within each block):
//!
//! ```text
//! text        WMO header + AWIPS id, CR CR LF separated
//! MHB   18 B  message code, date/time, length, block count
//! PDB  102 B  site lat/lon/height, product code, VCP, elevation, data scale
//! payload     usually bzip2 -> symbology block -> layer -> radial packet
//! ```

mod bytes;
pub(crate) mod error;
mod header;
mod legacy16;
mod pdb;
pub(crate) mod products;
mod symbology;
mod xdr;

pub(crate) use error::{Level3DecodeError, Result};
pub(crate) use header::{find_awips_token, find_message_header};

use chrono::{DateTime, Utc};
use ndarray::Array2;

use self::bytes::read_i16_be;
use self::products::DecodeScheme;

/// Codes below this are sentinels (0 = below threshold, 1 = range folded)
/// for the packet-16/AF1F linear schemes — never data. The single source
/// of truth for the ICD's floor-code convention: [`linear_declared_scale`]
/// below, [`pdb::value_scaling_float`]'s `value_min` formula, and
/// [`DeclaredLinearScale::data_floor_code`] (which carries it on to
/// [`crate::model::DeclaredScale`] at the model boundary) all read this one
/// constant rather than each redeclaring the literal `2`.
pub(crate) const DATA_FLOOR_CODE: u8 = 2;

/// 1 inch in millimetres — shared by `Precip`/`Rate`'s unit-conversion
/// factors below. Matches xradar #392's `IN_TO_MM` exactly.
const IN_TO_MM: f32 = 25.4;

/// `Precip` (170/172-175): raw levels are hundredths of an inch.
const PRECIP_FACTOR: f32 = 0.01 * IN_TO_MM;

/// `Rate` (176): raw levels are inches (per hour).
const RATE_FACTOR: f32 = IN_TO_MM;

/// The scale a packet-16/AF1F linear-coded moment declares, verbatim —
/// what becomes [`crate::model::DeclaredScale`] at the model boundary.
/// `None` for schemes with no simple linear declaration (`Legacy16`,
/// `ClassInt`) — those still populate `codes` and `physical`, just not
/// this.
pub(crate) struct DeclaredLinearScale {
    pub value_min: f32,
    pub value_increment: f32,
    pub n_levels: u16,
    /// Always [`DATA_FLOOR_CODE`] today — carried per-instance (rather than
    /// a caller re-reading the constant) so [`crate::model::DeclaredScale`]
    /// stays a plain field-for-field copy at the model boundary.
    pub data_floor_code: u8,
}

/// Verbatim on-wire raw codes — `u8` for packet 16/AF1F (never exceeds 8
/// bits/gate), `u16` for packet 28/XDR (`RATE`/code 176 — the only
/// packet-28 product this backend decodes as of plan 0012; `HCLASS`/code
/// 177 was independently confirmed via real fixtures to actually arrive
/// via packet 16 instead, contrary to this plan's original assumption —
/// see `products.rs`'s module doc). An enum, not two `Option` fields on
/// [`DecodedProduct`]: every decode path produces raw codes of exactly one
/// width, so "neither populated" and "both populated" should both be
/// unrepresentable, not just untested — the same "make the third state a
/// compile error" reasoning [`super::products::DecodeScheme`]'s own doc
/// comment gives. `pub(crate)`-internal only; [`crate::model::MomentData`]
/// (the external-consumer-facing model) keeps its two separate
/// `Option<Array2<_>>` fields instead, since that boundary DOES have real
/// external consumers (wasm, pyo3) with an additive-only constraint an
/// enum would force every existing call site to touch — see plan 0012 §3.2.
pub(crate) enum RawCodes {
    U8(Array2<u8>),
    U16(Array2<u16>),
}

impl RawCodes {
    pub(crate) fn dim(&self) -> (usize, usize) {
        match self {
            RawCodes::U8(c) => c.dim(),
            RawCodes::U16(c) => c.dim(),
        }
    }
}

/// One decoded NIDS product — the Rust analogue of the Python oracle's
/// `Level3Sweep` (`nexrad_level3.py:229-282`), extended for the broader
/// product set.
pub(crate) struct DecodedProduct {
    /// 3-letter Level 3 site id, e.g. `"LOT"` (NOT the 4-letter ICAO id) —
    /// always available: any well-formed 6-char text-header token carries
    /// it, independent of whether the product letter is recognised.
    pub site: String,
    /// The 3-character AWIPS product token, e.g. `"N0B"`, when the text
    /// header carried a well-formed one (it always should for a real NIDS
    /// file — see [`header::find_awips_token`]).
    pub awips_id: String,
    pub moment: &'static str,
    /// 0..5 tilt ordinal, populated only for the subset of products with
    /// an S3-verified AWIPS-letter table (`products::tilt_letter_lookup`)
    /// — see `decode::products`'s module doc for why this isn't guessed
    /// for the rest. The real elevation (`elevation_deg`) is always
    /// populated regardless of whether `tilt` resolves.
    pub tilt: Option<usize>,
    pub message_code: i16,
    pub vcp: u16,
    pub elevation_deg: f64,
    pub scan_time: DateTime<Utc>,
    pub lat: f64,
    pub lon: f64,
    pub height_m: f64,
    /// Ray CENTRES, degrees, in scan order.
    pub azimuths: Vec<f64>,
    /// `(n_radials, n_bins)` verbatim codes — see [`RawCodes`]'s doc for
    /// which packet families produce which width.
    pub codes: RawCodes,
    pub first_gate_m: f32,
    pub gate_spacing_m: f32,
    /// The product's own linear scale, when it has one (`LinearHw`/
    /// `FloatScale` — the original 6 byte-verified products plus 94/155).
    pub declared_scale: Option<DeclaredLinearScale>,
    /// Physical values, always populated regardless of decode scheme —
    /// every backend consumer expects `MomentData::data` to be usable.
    pub physical: Array2<f32>,
}

/// Decode one NIDS product. Loud rather than lenient throughout — a
/// mis-parsed product would yield a plausible-looking sweep pointed at the
/// wrong place, far worse than a decode error a caller can skip past.
pub(crate) fn decode(raw: &[u8]) -> Result<DecodedProduct> {
    // Message header first — it's what tells us which product this is,
    // and (since Phase 3) that no longer depends on the AWIPS token at
    // all. `find_message_header` only accepts a KNOWN code
    // (`products::is_known_message_code`), so `products::spec_for` below
    // is always `Some`.
    let mhb = find_message_header(raw)?;
    let message_code =
        read_i16_be(raw, mhb).expect("find_message_header validated this offset is readable");
    let spec = products::spec_for(message_code)
        .expect("message_code passed find_message_header's known-code check");

    if !products::packet_family_implemented(message_code) {
        return Err(Level3DecodeError::UnsupportedProduct { code: message_code });
    }

    // The text-header token supplies the site string (always) and,
    // for the verified subset, a tilt ordinal — never the moment, which
    // is `spec.moment` above regardless of what letter shows up.
    let (awips_id, site) = find_awips_token(raw)?;
    let tilt = products::tilt_letter_lookup(awips_id.as_bytes())
        .or_else(|| products::special_awips_id_lookup(awips_id.as_bytes()));

    let pdb = mhb + 18;
    // Without this, every PDB read below would need its own truncation
    // check. A short/partial read (51-95 bytes was the fuzzer-found case
    // in the oracle) must be a clean error here, not a panic later.
    if raw.len() < pdb + 102 {
        return Err(Level3DecodeError::TruncatedBeforePdb {
            have: raw.len(),
            need: pdb + 102,
        });
    }

    let fields = pdb::read_pdb_fields(raw, pdb)?;
    let scan_time = header::scan_time(fields.scan_days, fields.scan_seconds)?;

    if fields.product_code != message_code {
        return Err(Level3DecodeError::ProductCodeMismatch {
            product_code: fields.product_code,
            message_code,
        });
    }
    if !(-90.0..=90.0).contains(&fields.lat) || !(-180.0..=180.0).contains(&fields.lon) {
        return Err(Level3DecodeError::ImplausibleSitePosition {
            lat: fields.lat,
            lon: fields.lon,
        });
    }

    let payload = &raw[pdb + 102..];
    let body = symbology::decompress_payload(payload)?;

    // Elevation: 0.0 for surface/volume products, matching xradar's
    // `get_elevation` (`has_elevation: false` products carry no angle).
    let elevation_deg = if spec.has_elevation {
        fields.elevation_deg
    } else {
        0.0
    };

    // Gate geometry and raw codes both come out of `decode_symbology`
    // together, keyed on which packet family the file actually declared —
    // see `RawCodes`'s doc for why this is an enum, not two parallel
    // `Option`s. Packet 16/AF1F: a fixed ICD bin size for elevation-bearing
    // products (the packet's own range-scale field is
    // `cos(elevation) * 1000` for those, not metres — `docs/
    // NEXRAD_LEVEL3_WASM.md` §4.4), or the packet's own field directly for
    // surface products, where it genuinely is the range scale in metres.
    // Packet 28: the XDR radial component declares real metres directly,
    // unconditionally — see `symbology::decode_packet28`'s doc.
    let (azimuths, codes, gate_spacing_m, first_gate_m) = match symbology::decode_symbology(&body)?
    {
        symbology::SymbologyResult::U8 {
            azimuths,
            codes,
            range_scale_raw,
        } => {
            let gate_spacing_m = spec.bin_size.unwrap_or(range_scale_raw as f32);
            (
                azimuths,
                RawCodes::U8(codes),
                gate_spacing_m,
                gate_spacing_m / 2.0,
            )
        }
        symbology::SymbologyResult::U16 {
            azimuths,
            codes,
            gate_width_m,
            first_gate_m,
        } => (azimuths, RawCodes::U16(codes), gate_width_m, first_gate_m),
    };

    let (declared_scale, physical) = match (spec.decode, &codes) {
        (DecodeScheme::LinearHw, RawCodes::U8(codes)) => {
            let (value_min, value_increment, n_levels) = pdb::value_scaling_linear_hw(raw, pdb)?;
            linear_declared_scale(codes, value_min, value_increment, n_levels)?
        }
        (DecodeScheme::FloatScale, RawCodes::U8(codes)) => {
            let (value_min, value_increment, n_levels) = pdb::value_scaling_float(raw, pdb)?;
            linear_declared_scale(codes, value_min, value_increment, n_levels)?
        }
        (DecodeScheme::Legacy16, RawCodes::U8(codes)) => {
            let threshold = pdb::threshold_data(raw, pdb)?;
            let levels = legacy16::decode_legacy16(&threshold, spec.post_scale);
            // AF1F codes are always 0-15 (a 4-bit RLE nibble) — see
            // `symbology::decode_packet_af1f`.
            let physical = codes.mapv(|code| levels[(code & 0x0F) as usize]);
            (None, physical)
        }
        // Categorical: code 0 is "no data", every other code indexes a
        // fixed NWS category table this backend doesn't own (see
        // `docs/NEXRAD_LEVEL3_WASM.md`'s categorical-products note).
        // Shared by code 165 (packet 16, `u8`) and code 177 (also packet
        // 16 — see `products.rs`'s module doc for why 177 was independently
        // confirmed NOT to need the `u16` path this plan originally
        // assumed it would). `class_int_physical` is generic over the raw
        // width so this identical formula isn't written out twice.
        (DecodeScheme::ClassInt, RawCodes::U8(codes)) => (None, class_int_physical(codes)),
        (DecodeScheme::ClassInt, RawCodes::U16(codes)) => (None, class_int_physical(codes)),
        // `Precip` (170/172-175, packet 16) and `Rate` (176, packet 28) —
        // same PDB-declared float32 scale/offset as `FloatScale`, plus a
        // product-family flag-count field and a fixed unit-conversion
        // factor `Rate` shares with `Precip` (`IN_TO_MM` vs
        // `0.01 * IN_TO_MM`) — see `pdb::precip_family_scale`'s doc for the
        // full derivation.
        (DecodeScheme::Precip, RawCodes::U8(codes)) => {
            let (value_min, value_increment, floor_code, valid_max) =
                pdb::precip_family_scale(raw, pdb, PRECIP_FACTOR)?;
            precip_family_declared_scale(codes, value_min, value_increment, floor_code, valid_max)
        }
        (DecodeScheme::Rate, RawCodes::U16(codes)) => {
            let (value_min, value_increment, floor_code, valid_max) =
                pdb::precip_family_scale(raw, pdb, RATE_FACTOR)?;
            precip_family_declared_scale(codes, value_min, value_increment, floor_code, valid_max)
        }
        // Every combination below is a `(DecodeScheme, width)` pairing NO
        // packet family produces — `packet_family_implemented`/
        // `products.rs` and `symbology.rs`'s own dispatch agree on exactly
        // one width per scheme, so reaching any of these arms means those
        // two disagree: a radish bug, not malformed input. Enumerated
        // explicitly per scheme, NOT collapsed into `(_, RawCodes::U8(_))`/
        // `(_, RawCodes::U16(_))` catch-alls — a future `DecodeScheme`
        // variant must force a compile error here, the same "make the
        // unrepresentable state a compile error, not merely undetected"
        // discipline `DecodeScheme`'s own doc comment and
        // `packet_family_implemented`'s exhaustive match already hold
        // themselves to; a wildcard would silently swallow a new variant
        // into this generic error path instead.
        (DecodeScheme::LinearHw, RawCodes::U16(_))
        | (DecodeScheme::FloatScale, RawCodes::U16(_))
        | (DecodeScheme::Legacy16, RawCodes::U16(_))
        | (DecodeScheme::Precip, RawCodes::U16(_)) => {
            return Err(Level3DecodeError::RawCodeWidthMismatch {
                expected: "u8",
                actual: "u16",
            })
        }
        (DecodeScheme::Rate, RawCodes::U8(_)) => {
            return Err(Level3DecodeError::RawCodeWidthMismatch {
                expected: "u16",
                actual: "u8",
            })
        }
    };

    Ok(DecodedProduct {
        site,
        awips_id,
        moment: spec.moment,
        tilt,
        message_code,
        vcp: fields.vcp,
        elevation_deg,
        scan_time,
        lat: fields.lat,
        lon: fields.lon,
        height_m: fields.height_m,
        azimuths,
        codes,
        first_gate_m,
        gate_spacing_m,
        declared_scale,
        physical,
    })
}

/// Shared by [`DecodeScheme::LinearHw`] and [`DecodeScheme::FloatScale`]:
/// validate `value_increment`, derive `physical` from `codes`
/// (`DATA_FLOOR_CODE` and above is real data; below is a sentinel -> NaN),
/// and package the declared scale for the model boundary.
fn linear_declared_scale(
    codes: &Array2<u8>,
    value_min: f32,
    value_increment: f32,
    n_levels: u16,
) -> Result<(Option<DeclaredLinearScale>, Array2<f32>)> {
    if value_increment <= 0.0 {
        return Err(Level3DecodeError::NonPositiveIncrement(value_increment));
    }
    let physical = codes.mapv(|code| {
        if code >= DATA_FLOOR_CODE {
            value_min + (code - DATA_FLOOR_CODE) as f32 * value_increment
        } else {
            f32::NAN
        }
    });
    Ok((
        Some(DeclaredLinearScale {
            value_min,
            value_increment,
            n_levels,
            data_floor_code: DATA_FLOOR_CODE,
        }),
        physical,
    ))
}

/// [`ClassInt`](DecodeScheme::ClassInt)'s physical mapping — code 0 is "no
/// data", every other code IS the category index (no scale/offset, no
/// floor beyond 0). Generic over the raw-code width via `Into<u32>` so the
/// `(ClassInt, U8)` and `(ClassInt, U16)` match arms in `decode()` share
/// one formula instead of writing the same `mapv` twice — both codes 165
/// and 177 are `u8` today (see `products.rs`'s module doc for why 177
/// isn't the `u16`/packet-28 product this plan originally assumed), but
/// the match itself must stay exhaustive over both widths regardless (see
/// `RawCodes`'s doc), so this stays generic rather than `u8`-only.
fn class_int_physical<T>(codes: &Array2<T>) -> Array2<f32>
where
    T: Copy + Into<u32>,
{
    codes.mapv(|code| {
        let c: u32 = code.into();
        if c == 0 {
            f32::NAN
        } else {
            c as f32
        }
    })
}

/// [`Precip`](DecodeScheme::Precip)/[`Rate`](DecodeScheme::Rate)'s
/// counterpart to [`linear_declared_scale`] — same shifted-linear formula
/// shape, but with a per-instance floor code (`floor_code`, read from the
/// file — see `pdb::precip_family_scale`'s doc for why this is NOT
/// [`DATA_FLOOR_CODE`]'s fixed 2) and an OPTIONAL upper ceiling
/// (`valid_max`, also per-instance) instead of `linear_declared_scale`'s
/// unconditional "floor and above is data" — the reference oracle's
/// `get_data` NaNs BOTH `raw < leading` and (when a ceiling was read)
/// `raw > valid_max`; `linear_declared_scale`'s callers never had a
/// ceiling to enforce, so that check doesn't exist there.
///
/// Generic over the raw-code width (`u8` for `Precip`, `u16` for `Rate`)
/// via `Into<u32>` — the same formula, just evaluated at whichever
/// integer width the packet family that scheme uses actually produced.
fn precip_family_declared_scale<T>(
    codes: &Array2<T>,
    value_min: f32,
    value_increment: f32,
    floor_code: u8,
    valid_max: Option<u32>,
) -> (Option<DeclaredLinearScale>, Array2<f32>)
where
    T: Copy + Into<u32>,
{
    let physical = codes.mapv(|code| {
        let c: u32 = code.into();
        if c < floor_code as u32 || valid_max.is_some_and(|m| c > m) {
            f32::NAN
        } else {
            value_min + (c as f32 - floor_code as f32) * value_increment
        }
    });
    // Metadata only (see `linear_declared_scale`'s callers: `n_levels` is
    // never used to bound `physical` above, just carried on to
    // `DeclaredScale` for a caller that wants it) — floor through the
    // ceiling inclusive when one was read, otherwise as wide as the raw
    // code type can represent.
    let n_levels = valid_max
        .map(|m| (m.saturating_sub(floor_code as u32) + 1).min(u16::MAX as u32) as u16)
        .unwrap_or(u16::MAX - floor_code as u16);
    (
        Some(DeclaredLinearScale {
            value_min,
            value_increment,
            n_levels,
            data_floor_code: floor_code,
        }),
        physical,
    )
}

/// Build a complete, minimal, valid NIDS product byte-for-byte: text
/// header with an AWIPS id, MHB, PDB (integer-scaled reflectivity), and an
/// uncompressed one-radial packet-16 symbology block. Used to smoke-test
/// the full `decode()` pipeline without a real fixture, and re-exported
/// (`pub(crate)`, test-only) for `nexrad_level3::mod`'s and `adapter`'s own
/// test modules to reuse rather than duplicate — Phase 4 adds byte-for-byte
/// parity against the Python oracle on real fixtures.
#[cfg(test)]
pub(crate) fn build_minimal_n0b() -> Vec<u8> {
    let mut raw = Vec::new();
    // Text header: AWIPS id token, then padding to reach the MHB.
    raw.extend_from_slice(b"N0BLOT\r\r\n");
    while raw.len() < 30 {
        raw.push(b' ');
    }
    let mhb = raw.len();
    raw.extend_from_slice(&153i16.to_be_bytes()); // message code
    raw.extend_from_slice(&[0u8; 16]); // rest of the 18-byte MHB, unread
    assert_eq!(raw.len(), mhb + 18);

    let pdb = raw.len();
    let mut pdb_bytes = vec![0u8; 102];
    // hw1: the PDB's opening `-1` divider — `find_message_header`
    // validates against this at `mhb + 18` (== `pdb + 0`).
    pdb_bytes[0..2].copy_from_slice(&(-1i16).to_be_bytes());
    // hw2 lat x1000 (i32 at halfword offset 2 -> byte 2).
    pdb_bytes[2..6].copy_from_slice(&41_881i32.to_be_bytes());
    // hw4 lon x1000.
    pdb_bytes[6..10].copy_from_slice(&(-88_084i32).to_be_bytes());
    // hw6 height, feet.
    pdb_bytes[10..12].copy_from_slice(&650i16.to_be_bytes());
    // hw7 product code (must equal the message code).
    pdb_bytes[12..14].copy_from_slice(&153i16.to_be_bytes());
    // hw9 vcp.
    pdb_bytes[16..18].copy_from_slice(&215i16.to_be_bytes());
    // hw12 scan days (u16, >=1).
    pdb_bytes[22..24].copy_from_slice(&20000u16.to_be_bytes());
    // hw13 scan seconds (i32).
    pdb_bytes[24..28].copy_from_slice(&3600i32.to_be_bytes());
    // hw21 elevation x10.
    pdb_bytes[40..42].copy_from_slice(&5i16.to_be_bytes());
    // hw22 value_min x10 = -32.0 dBZ.
    pdb_bytes[42..44].copy_from_slice(&(-320i16).to_be_bytes());
    // hw23 value_increment x10 = 0.5 dB.
    pdb_bytes[44..46].copy_from_slice(&5i16.to_be_bytes());
    // hw24 n_levels.
    pdb_bytes[46..48].copy_from_slice(&254i16.to_be_bytes());
    raw.extend_from_slice(&pdb_bytes);
    assert_eq!(raw.len(), pdb + 102);

    // Uncompressed symbology block (no "BZ" magic -> passthrough).
    let mut body = Vec::new();
    body.extend_from_slice(&(-1i16).to_be_bytes());
    body.extend_from_slice(&1i16.to_be_bytes());
    body.extend_from_slice(&[0u8; 6]);
    body.extend_from_slice(&[0u8; 6]);
    body.extend_from_slice(&16u16.to_be_bytes()); // packet_code
    body.extend_from_slice(&0u16.to_be_bytes()); // first_bin
    body.extend_from_slice(&4u16.to_be_bytes()); // n_bins
    body.extend_from_slice(&999i16.to_be_bytes());
    body.extend_from_slice(&998i16.to_be_bytes());
    body.extend_from_slice(&999u16.to_be_bytes());
    body.extend_from_slice(&1u16.to_be_bytes()); // n_radials
    body.extend_from_slice(&4u16.to_be_bytes()); // n_bytes for the one radial
    body.extend_from_slice(&0u16.to_be_bytes()); // start_angle
    body.extend_from_slice(&10u16.to_be_bytes()); // delta (1.0 deg)
    body.extend_from_slice(&[2, 3, 4, 5]); // codes
    raw.extend_from_slice(&body);

    raw
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every synthetic fixture in this test module is packet 16/AF1F
    /// (`u8` raw codes) — this unwraps `RawCodes::U8` with a clear panic
    /// message rather than every call site pattern-matching inline.
    fn expect_u8(codes: &RawCodes) -> &Array2<u8> {
        match codes {
            RawCodes::U8(c) => c,
            RawCodes::U16(_) => panic!("expected RawCodes::U8, got RawCodes::U16"),
        }
    }

    #[test]
    fn decode_a_minimal_synthetic_n0b_end_to_end() {
        let raw = build_minimal_n0b();
        let product = decode(&raw).unwrap();
        assert_eq!(product.awips_id, "N0B");
        assert_eq!(product.site, "LOT");
        assert_eq!(product.moment, "DBZH");
        assert_eq!(product.tilt, Some(0));
        assert_eq!(product.message_code, 153);
        assert_eq!(product.vcp, 215);
        assert!((product.elevation_deg - 0.5).abs() < 1e-6);
        assert!((product.lat - 41.881).abs() < 1e-6);
        assert!((product.lon - (-88.084)).abs() < 1e-6);
        let scale = product.declared_scale.as_ref().unwrap();
        assert_eq!(scale.value_min, -32.0);
        assert_eq!(scale.value_increment, 0.5);
        assert_eq!(scale.n_levels, 254);
        assert_eq!(product.gate_spacing_m, 250.0);
        assert_eq!(product.first_gate_m, 125.0);
        assert_eq!(product.codes.dim(), (1, 4));
        assert_eq!(expect_u8(&product.codes).row(0).to_vec(), vec![2, 3, 4, 5]);
        assert!((product.azimuths[0] - 0.5).abs() < 1e-9); // (0 + 10/2.0) / 10.0
        assert_eq!(
            product.physical.row(0).to_vec(),
            vec![-32.0, -31.5, -31.0, -30.5]
        );
    }

    #[test]
    fn decode_rejects_product_code_message_code_mismatch() {
        let mut raw = build_minimal_n0b();
        // Corrupt the PDB's product-code halfword (hw7, at mhb+18+12).
        let mhb = raw
            .windows(2)
            .position(|w| w == 153i16.to_be_bytes())
            .unwrap();
        let pdb = mhb + 18;
        raw[pdb + 12..pdb + 14].copy_from_slice(&99i16.to_be_bytes());
        assert!(matches!(
            decode(&raw),
            Err(Level3DecodeError::ProductCodeMismatch { .. })
        ));
    }

    #[test]
    fn decode_rejects_truncated_input() {
        let raw = build_minimal_n0b();
        assert!(matches!(
            decode(&raw[..raw.len() / 2]),
            Err(Level3DecodeError::TruncatedBeforePdb { .. })
        ));
    }

    #[test]
    fn decode_resolves_tilt_for_special_awips_ids_via_the_fallback() {
        // NSW isn't `{prefix}{letter}`-shaped, so `tilt_letter_lookup` alone
        // can't resolve it — this proves `decode()` actually falls through
        // to `special_awips_id_lookup` rather than the fallback existing
        // only as dead code nothing calls.
        let mut raw = build_minimal_n0b();
        raw[0..6].copy_from_slice(b"NSWLOT");
        let product = decode(&raw).unwrap();
        assert_eq!(product.awips_id, "NSW");
        assert_eq!(product.tilt, Some(0));
    }

    #[test]
    fn decode_resolves_tilt_none_for_unverified_letters_but_still_decodes() {
        // Swap the AWIPS letter to a code with no verified tilt table
        // (still message code 153, so decode succeeds; tilt is just
        // unresolved rather than guessed). `J` deliberately: not a real
        // AWIPS letter anywhere in `PRODUCTS`/`TILT_LETTER_TABLE` today,
        // so this test doesn't go stale the next time a real letter
        // (like `H` before it) gets verified and added to the table.
        let mut raw = build_minimal_n0b();
        raw[0..6].copy_from_slice(b"N0JLOT");
        let product = decode(&raw).unwrap();
        assert_eq!(product.awips_id, "N0J");
        assert_eq!(product.tilt, None);
        assert_eq!(product.moment, "DBZH"); // still resolved via message code
    }

    /// Just enough to reach `find_message_header` — no PDB or symbology
    /// block needed, since an unrecognised message code errors out before
    /// either is read.
    fn build_header_only(message_code: i16) -> Vec<u8> {
        let mut raw = b"N0ZLOT\r\r\n".to_vec();
        while raw.len() < 30 {
            raw.push(b' ');
        }
        raw.extend_from_slice(&message_code.to_be_bytes());
        raw.extend_from_slice(&[0u8; 16]);
        raw.extend_from_slice(&(-1i16).to_be_bytes()); // PDB divider
                                                       // Trailing slack: `find_message_header`'s scan window is
                                                       // `0..min(len,128)-20` (exclusive), so a header with nothing after
                                                       // its own divider check sits just outside the window — see
                                                       // `sniff.rs`'s `with_message_header` helper for the same fix.
        raw.extend_from_slice(&[0u8; 8]);
        raw
    }

    #[test]
    fn decode_rejects_an_unrecognised_message_code_end_to_end() {
        // Plan 0012 closed all 7 previously-deferred codes (170/172-175 via
        // `Precip`, 176 via `Rate`, 177 via `ClassInt` — the latter
        // independently confirmed to arrive via packet 16, not the
        // packet-28 this plan originally assumed; see `products.rs`'s
        // module doc), so `PRODUCTS` no longer has any code
        // `packet_family_implemented` reports `false` for — this test
        // used to cover THAT rejection path with the deferred codes
        // themselves; it now covers the one rejection path that still
        // exists at this stage: a message code `PRODUCTS` doesn't list at
        // all. Replaced rather than left asserting a scenario that no
        // longer occurs (plan 0012 §3.5's own words, about a sibling test
        // file: "a stale 'this rejects' test that's actually testing dead
        // code... is worse than no test").
        let raw = build_header_only(9999);
        assert!(matches!(
            decode(&raw),
            Err(Level3DecodeError::NoMessageHeader { .. })
        ));
    }

    /// A full, valid packet-AF1F / `Legacy16` product (message code 19,
    /// legacy reflectivity) — text header, MHB, PDB with a `threshold_data`
    /// flag/value table, and an RLE-encoded symbology block. Exercises the
    /// integration `decode_legacy16_af1f_product_end_to_end` needs: PDB
    /// `threshold_data` read -> `legacy16::decode_legacy16` -> AF1F RLE
    /// expand -> `codes.mapv` with the `& 0x0F` mask, all wired together
    /// exactly as `decode()` does it (each piece already has isolated unit
    /// tests elsewhere; this is the only place they run as one pipeline).
    fn build_minimal_legacy19() -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"N0RLOT\r\r\n");
        while raw.len() < 30 {
            raw.push(b' ');
        }
        let mhb = raw.len();
        raw.extend_from_slice(&19i16.to_be_bytes());
        raw.extend_from_slice(&[0u8; 16]);
        assert_eq!(raw.len(), mhb + 18);

        let pdb = raw.len();
        let mut pdb_bytes = vec![0u8; 102];
        pdb_bytes[0..2].copy_from_slice(&(-1i16).to_be_bytes()); // divider
        pdb_bytes[2..6].copy_from_slice(&41_881i32.to_be_bytes()); // lat
        pdb_bytes[6..10].copy_from_slice(&(-88_084i32).to_be_bytes()); // lon
        pdb_bytes[10..12].copy_from_slice(&650i16.to_be_bytes()); // height, ft
        pdb_bytes[12..14].copy_from_slice(&19i16.to_be_bytes()); // product code
        pdb_bytes[16..18].copy_from_slice(&215i16.to_be_bytes()); // vcp
        pdb_bytes[22..24].copy_from_slice(&20000u16.to_be_bytes()); // scan days
        pdb_bytes[24..28].copy_from_slice(&3600i32.to_be_bytes()); // scan seconds
        pdb_bytes[40..42].copy_from_slice(&5i16.to_be_bytes()); // elevation x10
                                                                // threshold_data (halfwords 22-37, bytes 42..74): 16 (flag, value)
                                                                // pairs. flag=0x00 for every level -> positive, unscaled, so
                                                                // levels[i] == i (given post_scale = 1.0 for message code 19).
        for i in 0..16u8 {
            pdb_bytes[42 + 2 * i as usize] = 0x00; // flag
            pdb_bytes[42 + 2 * i as usize + 1] = i; // value
        }
        raw.extend_from_slice(&pdb_bytes);
        assert_eq!(raw.len(), pdb + 102);

        // Packet AF1F, one radial, 4 bins: RLE (run=2,color=3),(run=2,color=7)
        // -> codes [3,3,7,7].
        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&(-20705i16).to_be_bytes()); // AF1F packet code
        body.extend_from_slice(&0u16.to_be_bytes()); // first_bin (unchecked)
        body.extend_from_slice(&4u16.to_be_bytes()); // n_bins
        body.extend_from_slice(&999i16.to_be_bytes());
        body.extend_from_slice(&998i16.to_be_bytes());
        body.extend_from_slice(&999u16.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes()); // n_radials
        body.extend_from_slice(&1u16.to_be_bytes()); // n_bytes: 1 halfword = 2 RLE bytes
        body.extend_from_slice(&0u16.to_be_bytes()); // start_angle
        body.extend_from_slice(&10u16.to_be_bytes()); // delta
        body.push((2u8 << 4) | 3);
        body.push((2u8 << 4) | 7);
        raw.extend_from_slice(&body);

        raw
    }

    #[test]
    fn decode_legacy16_af1f_product_end_to_end() {
        let raw = build_minimal_legacy19();
        let product = decode(&raw).unwrap();
        assert_eq!(product.moment, "DBZH");
        assert_eq!(product.message_code, 19);
        // Legacy16 has no simple linear declared scale.
        assert!(product.declared_scale.is_none());
        assert_eq!(expect_u8(&product.codes).row(0).to_vec(), vec![3, 3, 7, 7]);
        // levels[i] == i for the all-zero-flag threshold table built above.
        assert_eq!(product.physical.row(0).to_vec(), vec![3.0, 3.0, 7.0, 7.0]);
        // message code 19's ICD bin size (`ProductSpec.bin_size`), not the
        // packet-16-only 250 m constant.
        assert_eq!(product.gate_spacing_m, 1000.0);
    }

    /// A full, valid packet-16 / `ClassInt` (categorical) product (message
    /// code 165, hydrometeor classification).
    fn build_minimal_hclass165() -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"N0HLOT\r\r\n");
        while raw.len() < 30 {
            raw.push(b' ');
        }
        let mhb = raw.len();
        raw.extend_from_slice(&165i16.to_be_bytes());
        raw.extend_from_slice(&[0u8; 16]);
        assert_eq!(raw.len(), mhb + 18);

        let pdb = raw.len();
        let mut pdb_bytes = vec![0u8; 102];
        pdb_bytes[0..2].copy_from_slice(&(-1i16).to_be_bytes());
        pdb_bytes[2..6].copy_from_slice(&41_881i32.to_be_bytes());
        pdb_bytes[6..10].copy_from_slice(&(-88_084i32).to_be_bytes());
        pdb_bytes[10..12].copy_from_slice(&650i16.to_be_bytes());
        pdb_bytes[12..14].copy_from_slice(&165i16.to_be_bytes());
        pdb_bytes[16..18].copy_from_slice(&215i16.to_be_bytes());
        pdb_bytes[22..24].copy_from_slice(&20000u16.to_be_bytes());
        pdb_bytes[24..28].copy_from_slice(&3600i32.to_be_bytes());
        pdb_bytes[40..42].copy_from_slice(&5i16.to_be_bytes());
        raw.extend_from_slice(&pdb_bytes);
        assert_eq!(raw.len(), pdb + 102);

        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&16u16.to_be_bytes()); // packet 16
        body.extend_from_slice(&0u16.to_be_bytes()); // first_bin
        body.extend_from_slice(&4u16.to_be_bytes()); // n_bins
        body.extend_from_slice(&999i16.to_be_bytes());
        body.extend_from_slice(&998i16.to_be_bytes());
        body.extend_from_slice(&999u16.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes()); // n_radials
        body.extend_from_slice(&4u16.to_be_bytes()); // n_bytes
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&10u16.to_be_bytes());
        body.extend_from_slice(&[0, 5, 10, 0]); // codes: no-data, 5, 10, no-data
        raw.extend_from_slice(&body);

        raw
    }

    #[test]
    fn decode_class_int_product_end_to_end() {
        let raw = build_minimal_hclass165();
        let product = decode(&raw).unwrap();
        assert_eq!(product.moment, "HCLASS");
        assert!(product.declared_scale.is_none());
        assert_eq!(expect_u8(&product.codes).row(0).to_vec(), vec![0, 5, 10, 0]);
        let physical = product.physical.row(0).to_vec();
        assert!(physical[0].is_nan()); // code 0 = no data
        assert_eq!(physical[1], 5.0); // code IS the category index
        assert_eq!(physical[2], 10.0);
        assert!(physical[3].is_nan());
    }

    /// A full, valid packet-16 / `Precip` product (message code 170,
    /// `DAA` — digital accumulation array). PDB `threshold_data` carries a
    /// plausible flag-count field (`leading=1, trailing=0, max_val=255`),
    /// matching what real `DAA`/`DTA`/`DU3`/`DU6` fixtures were confirmed
    /// to declare (plan 0012 §2.4 step 2) — this exercises the PLAUSIBLE
    /// branch of `precip_family_scale`'s fallback logic; `pdb.rs`'s own
    /// unit tests cover the fallback branch directly.
    fn build_minimal_precip170() -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"DAALOT\r\r\n");
        while raw.len() < 30 {
            raw.push(b' ');
        }
        let mhb = raw.len();
        raw.extend_from_slice(&170i16.to_be_bytes());
        raw.extend_from_slice(&[0u8; 16]);
        assert_eq!(raw.len(), mhb + 18);

        let pdb = raw.len();
        let mut pdb_bytes = vec![0u8; 102];
        pdb_bytes[0..2].copy_from_slice(&(-1i16).to_be_bytes());
        pdb_bytes[2..6].copy_from_slice(&41_881i32.to_be_bytes());
        pdb_bytes[6..10].copy_from_slice(&(-88_084i32).to_be_bytes());
        pdb_bytes[10..12].copy_from_slice(&650i16.to_be_bytes());
        pdb_bytes[12..14].copy_from_slice(&170i16.to_be_bytes());
        pdb_bytes[16..18].copy_from_slice(&215i16.to_be_bytes());
        pdb_bytes[22..24].copy_from_slice(&20000u16.to_be_bytes());
        pdb_bytes[24..28].copy_from_slice(&3600i32.to_be_bytes());
        // threshold_data (halfwords 22-37, PDB bytes 42..74): scale/offset
        // f32 pair (hw22-25), then hw27 max_val / hw28 leading / hw29
        // trailing at PDB bytes 52/54/56.
        pdb_bytes[42..46].copy_from_slice(&2.0f32.to_be_bytes()); // scale
        pdb_bytes[46..50].copy_from_slice(&0.0f32.to_be_bytes()); // offset
        pdb_bytes[52..54].copy_from_slice(&255u16.to_be_bytes()); // max_val
        pdb_bytes[54..56].copy_from_slice(&1i16.to_be_bytes()); // leading
        pdb_bytes[56..58].copy_from_slice(&0i16.to_be_bytes()); // trailing
        raw.extend_from_slice(&pdb_bytes);
        assert_eq!(raw.len(), pdb + 102);

        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&16u16.to_be_bytes()); // packet 16
        body.extend_from_slice(&0u16.to_be_bytes()); // first_bin
        body.extend_from_slice(&4u16.to_be_bytes()); // n_bins
        body.extend_from_slice(&999i16.to_be_bytes());
        body.extend_from_slice(&998i16.to_be_bytes());
        body.extend_from_slice(&999u16.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes()); // n_radials
        body.extend_from_slice(&4u16.to_be_bytes()); // n_bytes
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&10u16.to_be_bytes());
        body.extend_from_slice(&[0, 1, 2, 255]); // codes: below-floor, floor, floor+1, max
        raw.extend_from_slice(&body);

        raw
    }

    #[test]
    fn decode_precip_product_end_to_end() {
        let raw = build_minimal_precip170();
        let product = decode(&raw).unwrap();
        assert_eq!(product.moment, "ACCUM");
        let scale = product.declared_scale.as_ref().unwrap();
        // factor = PRECIP_FACTOR = 0.01 * 25.4 = 0.254; file scale=2.0,
        // offset=0.0 -> out_scale = 0.254/2.0 = 0.127, out_offset = 0.0.
        assert!((scale.value_increment - 0.127).abs() < 1e-6);
        assert_eq!(scale.data_floor_code, 1); // read from the PDB, not DATA_FLOOR_CODE=2
        let physical = product.physical.row(0).to_vec();
        assert!(
            physical[0].is_nan(),
            "code 0 is below the floor (leading=1)"
        );
        assert!((physical[1] - (1.0 * 0.127)).abs() < 1e-5); // code == floor_code
        assert!((physical[2] - (2.0 * 0.127)).abs() < 1e-5);
        assert!((physical[3] - (255.0 * 0.127)).abs() < 1e-4);
    }

    /// A full, valid packet-28 (XDR) / `Rate` product (message code 176,
    /// `DPR`). Gate geometry (`gate_width=250.0`, `first_gate=125.0`) and
    /// field order verified against a real `DPR` fixture (plan 0012 §3's
    /// implementation notes).
    fn build_minimal_rate176() -> Vec<u8> {
        fn xdr_string(s: &str) -> Vec<u8> {
            let mut out = (s.len() as u32).to_be_bytes().to_vec();
            out.extend_from_slice(s.as_bytes());
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
            out
        }
        fn xdr_empty_list() -> Vec<u8> {
            let mut out = 0i32.to_be_bytes().to_vec();
            out.extend(0i32.to_be_bytes());
            out
        }

        let mut raw = Vec::new();
        raw.extend_from_slice(b"DPRLOT\r\r\n");
        while raw.len() < 30 {
            raw.push(b' ');
        }
        let mhb = raw.len();
        raw.extend_from_slice(&176i16.to_be_bytes());
        raw.extend_from_slice(&[0u8; 16]);
        assert_eq!(raw.len(), mhb + 18);

        let pdb = raw.len();
        let mut pdb_bytes = vec![0u8; 102];
        pdb_bytes[0..2].copy_from_slice(&(-1i16).to_be_bytes());
        pdb_bytes[2..6].copy_from_slice(&41_881i32.to_be_bytes());
        pdb_bytes[6..10].copy_from_slice(&(-88_084i32).to_be_bytes());
        pdb_bytes[10..12].copy_from_slice(&650i16.to_be_bytes());
        pdb_bytes[12..14].copy_from_slice(&176i16.to_be_bytes());
        pdb_bytes[16..18].copy_from_slice(&215i16.to_be_bytes());
        pdb_bytes[22..24].copy_from_slice(&20000u16.to_be_bytes());
        pdb_bytes[24..28].copy_from_slice(&3600i32.to_be_bytes());
        pdb_bytes[42..46].copy_from_slice(&1000.0f32.to_be_bytes()); // scale, real DPR value
        pdb_bytes[46..50].copy_from_slice(&0.0f32.to_be_bytes()); // offset
        pdb_bytes[52..54].copy_from_slice(&65535u16.to_be_bytes()); // max_val
        pdb_bytes[54..56].copy_from_slice(&0i16.to_be_bytes()); // leading
        pdb_bytes[56..58].copy_from_slice(&0i16.to_be_bytes()); // trailing
        raw.extend_from_slice(&pdb_bytes);
        assert_eq!(raw.len(), pdb + 102);

        let mut xdr = Vec::new();
        for _ in 0..18 {
            xdr.extend(0i32.to_be_bytes()); // 18 throwaway product-desc fields
        }
        xdr.extend(xdr_empty_list()); // product-level parameters
        xdr.extend(1i32.to_be_bytes()); // components count
        xdr.extend(0i32.to_be_bytes()); // leading pointer
        xdr.extend(1i32.to_be_bytes()); // component type = radial
        xdr.extend(xdr_string("")); // description
        xdr.extend(250.0f32.to_be_bytes()); // gate_width
        xdr.extend(125.0f32.to_be_bytes()); // first_gate
        xdr.extend(xdr_empty_list()); // radial-level parameters
        xdr.extend(2i32.to_be_bytes()); // num_rads
        for (az, data) in [(0.0f32, [0i32, 100, 401, 0]), (1.0, [16, 0, 0, 0])] {
            xdr.extend(az.to_be_bytes());
            xdr.extend(0.0f32.to_be_bytes()); // elevation
            xdr.extend(1.0f32.to_be_bytes()); // width
            xdr.extend(4i32.to_be_bytes()); // num_bins
            xdr.extend(xdr_string("")); // attributes
            xdr.extend(4u32.to_be_bytes()); // data array count
            for v in data {
                xdr.extend(v.to_be_bytes());
            }
        }

        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&28i16.to_be_bytes()); // GENERIC_PACKET_CODE
        body.extend_from_slice(&0i16.to_be_bytes()); // reserved
        body.extend_from_slice(&(xdr.len() as i32).to_be_bytes()); // num_bytes
        body.extend_from_slice(&xdr);
        raw.extend_from_slice(&body);

        raw
    }

    #[test]
    fn decode_rate_product_end_to_end() {
        let raw = build_minimal_rate176();
        let product = decode(&raw).unwrap();
        assert_eq!(product.moment, "RATE");
        assert_eq!(product.gate_spacing_m, 250.0);
        assert_eq!(product.first_gate_m, 125.0);
        // Centred, not the stored leading edge (radials built with
        // az=0.0/1.0, width=1.0 -> ICD 2620001AC Figure E-4's
        // leading-edge convention applies here too, same as packet 16).
        assert_eq!(product.azimuths, vec![0.5, 1.5]);
        let codes = match &product.codes {
            RawCodes::U16(c) => c,
            RawCodes::U8(_) => panic!("Rate (packet 28) must produce RawCodes::U16"),
        };
        assert_eq!(codes.row(0).to_vec(), vec![0u16, 100, 401, 0]);
        // factor = RATE_FACTOR = 25.4; file scale=1000.0, offset=0.0 ->
        // out_scale = 25.4/1000.0 = 0.0254.
        let physical = product.physical.row(0).to_vec();
        assert!((physical[0] - 0.0).abs() < 1e-6); // leading=0, code 0 is valid data
        assert!((physical[2] - (401.0 * 0.0254)).abs() < 1e-4);
    }

    /// The plan 0012 correction that changed this pass's scope most:
    /// `HCLASS`/code 177 was assumed to need packet 28 + the `u16` model
    /// widening, same as `RATE`/176 — a real fixture-based re-check found
    /// it actually arrives via packet 16, the SAME wire shape code 165
    /// already decodes, with `has_elevation: false` the only real
    /// difference. This locks that in: 177 must decode through the exact
    /// same `(DecodeScheme::ClassInt, RawCodes::U8)` arm 165 uses, not the
    /// `u16` arm — a regression here would silently misdecode every real
    /// `HHC` file the moment `packet_family_implemented` ever changed to
    /// assume packet 28 for it.
    fn build_minimal_hclass177() -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(b"HHCLOT\r\r\n");
        while raw.len() < 30 {
            raw.push(b' ');
        }
        let mhb = raw.len();
        raw.extend_from_slice(&177i16.to_be_bytes());
        raw.extend_from_slice(&[0u8; 16]);
        assert_eq!(raw.len(), mhb + 18);

        let pdb = raw.len();
        let mut pdb_bytes = vec![0u8; 102];
        pdb_bytes[0..2].copy_from_slice(&(-1i16).to_be_bytes());
        pdb_bytes[2..6].copy_from_slice(&41_881i32.to_be_bytes());
        pdb_bytes[6..10].copy_from_slice(&(-88_084i32).to_be_bytes());
        pdb_bytes[10..12].copy_from_slice(&650i16.to_be_bytes());
        pdb_bytes[12..14].copy_from_slice(&177i16.to_be_bytes());
        pdb_bytes[16..18].copy_from_slice(&215i16.to_be_bytes());
        pdb_bytes[22..24].copy_from_slice(&20000u16.to_be_bytes());
        pdb_bytes[24..28].copy_from_slice(&3600i32.to_be_bytes());
        // hw21 elevation deliberately left 0 — has_elevation:false for 177
        // means `decode()` forces 0.0 regardless of what the PDB carries.
        raw.extend_from_slice(&pdb_bytes);
        assert_eq!(raw.len(), pdb + 102);

        let mut body = Vec::new();
        body.extend_from_slice(&(-1i16).to_be_bytes());
        body.extend_from_slice(&1i16.to_be_bytes());
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&16u16.to_be_bytes()); // packet 16 — NOT 28
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&4u16.to_be_bytes());
        body.extend_from_slice(&999i16.to_be_bytes());
        body.extend_from_slice(&998i16.to_be_bytes());
        body.extend_from_slice(&999u16.to_be_bytes());
        body.extend_from_slice(&1u16.to_be_bytes());
        body.extend_from_slice(&4u16.to_be_bytes());
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&10u16.to_be_bytes());
        body.extend_from_slice(&[0, 5, 220, 0]);
        raw.extend_from_slice(&body);

        raw
    }

    #[test]
    fn decode_hhc177_uses_packet16_class_int_not_the_u16_path() {
        let raw = build_minimal_hclass177();
        let product = decode(&raw).unwrap();
        assert_eq!(product.moment, "HCLASS");
        assert_eq!(product.elevation_deg, 0.0);
        assert_eq!(product.gate_spacing_m, 250.0); // ProductSpec::bin_size for 177
        assert!(matches!(product.codes, RawCodes::U8(_)));
        let physical = product.physical.row(0).to_vec();
        assert!(physical[0].is_nan());
        assert_eq!(physical[1], 5.0);
        assert_eq!(physical[2], 220.0);
        assert!(physical[3].is_nan());
    }
}
