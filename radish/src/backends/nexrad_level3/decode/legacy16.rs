//! The 16-level flag-encoded legacy scaling scheme
//! ([`super::products::DecodeScheme::Legacy16`]) — packet AF1F products:
//! legacy reflectivity/velocity/spectrum-width/storm-relative-velocity
//! (19/20/25/27/28/30/56) and legacy accumulations (78/79/80).
//!
//! `threshold_data` (PDB halfwords 22-37, 32 bytes) holds 16 `(flag,
//! value)` byte pairs — one entry per possible raw code 0-15 (packet
//! AF1F's RLE "colour" nibble). Ported from xradar #392's
//! `_decode_legacy16` (openradar/xradar, commit `fcc31ae`).

/// Build the 16-entry physical-value lookup table for one product's
/// `threshold_data`. `levels[code]` is the physical value for raw code
/// `code` (0-15); `NaN` marks "no data" (flag bit `0x80`).
///
/// Flag bits, applied in this exact order (later bits win on conflict,
/// matching xradar's sequential NumPy boolean-mask assignment rather than
/// an if/elif chain — see the exact bit precedence below):
/// - `0x01`: value is negative.
/// - `0x20` -> ÷20, then `0x10` -> ÷10 (overwrites), then `0x40` -> ÷100
///   (overwrites again) — only one is expected set per the ICD, but the
///   precedence order matters for byte-exact parity on a malformed input
///   with more than one set.
/// - `0x80`: this level carries no data (`NaN`), checked first — it's the
///   only bit that skips the rest of the decode.
pub(crate) fn decode_legacy16(threshold_data: &[u8; 32], post_scale: f32) -> [f32; 16] {
    let mut levels = [0.0f32; 16];
    for (i, level) in levels.iter_mut().enumerate() {
        let flag = threshold_data[2 * i];
        if flag & 0x80 != 0 {
            *level = f32::NAN;
            continue;
        }
        let value = threshold_data[2 * i + 1] as f32;
        let sign = if flag & 0x01 != 0 { -1.0 } else { 1.0 };
        let mut scale = 1.0f32;
        if flag & 0x20 != 0 {
            scale = 1.0 / 20.0;
        }
        if flag & 0x10 != 0 {
            scale = 1.0 / 10.0;
        }
        if flag & 0x40 != 0 {
            scale = 1.0 / 100.0;
        }
        *level = value * sign * scale * post_scale;
    }
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_legacy16_applies_sign_and_tenths_scale() {
        let mut threshold = [0u8; 32];
        // level 3: flag = sign(0x01) | tenths(0x10) = 0x11, value = 55 -> -5.5
        threshold[6] = 0x11;
        threshold[7] = 55;
        let levels = decode_legacy16(&threshold, 1.0);
        assert!((levels[3] - (-5.5)).abs() < 1e-6);
    }

    #[test]
    fn decode_legacy16_no_data_flag_yields_nan() {
        let mut threshold = [0u8; 32];
        threshold[0] = 0x80;
        let levels = decode_legacy16(&threshold, 1.0);
        assert!(levels[0].is_nan());
    }

    #[test]
    fn decode_legacy16_applies_post_scale_for_unit_conversion() {
        let mut threshold = [0u8; 32];
        threshold[0] = 0x00; // no flags: positive, unscaled
        threshold[1] = 10; // raw value 10
        let kt_to_ms = 0.514444f32;
        let levels = decode_legacy16(&threshold, kt_to_ms);
        assert!((levels[0] - 10.0 * kt_to_ms).abs() < 1e-6);
    }

    #[test]
    fn decode_legacy16_hundredths_flag_overrides_tenths_and_twentieths() {
        // All three scale flags set: 0x40 (hundredths) must win per the
        // sequential-overwrite precedence.
        let mut threshold = [0u8; 32];
        threshold[0] = 0x20 | 0x10 | 0x40;
        threshold[1] = 100;
        let levels = decode_legacy16(&threshold, 1.0);
        assert!((levels[0] - 1.0).abs() < 1e-6); // 100 / 100 = 1.0
    }
}
