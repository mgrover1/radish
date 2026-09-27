//! Per-IRIS-data-type calibration helpers.
//!
//! Each IRIS data type has its own encoding rule. Most are linear:
//! `value = (raw + offset) / scale`. Velocity is Nyquist-scaled, 8-bit
//! phase maps to ~[0°, 180°] and 2-byte phase to [0°, 360°], KDP is an
//! exponential of the wavelength, and a couple of types mask a no-data
//! sentinel. Helpers here mirror xradar's `iris.py` `decode_*` family
//! (`decode_array`, `decode_vel`, `decode_width`, `decode_phidp`,
//! `decode_kdp`, `decode_sqi`, …); per-gate values match xradar's output
//! to floating-point precision.
//!
//! **Known divergence:** 8-bit `DB_VEL` on dual-/batch-PRF tasks. xradar
//! multiplies the Nyquist by `multi_prf_mode_flag + 1` for `decode_vel`
//! only; we don't parse that flag from `TASK_DSP_INFO` yet, so `VRADH`
//! is off by that integer factor on multi-PRF volumes (exact on
//! single-PRF, which is the common case). Tracked as a follow-up.
//!
//! **No-data policy (matches xradar).** xradar only masks where a data
//! type's `func`/`fkw` says so: `DB_VEL` (8-bit) carries `mask: 0.0`;
//! `decode_sqi` (RHOHV/SQI) yields `NaN` *naturally* from `sqrt` of a
//! negative; `decode_kdp` masks its `0`/`-1` sentinels. Every `decode_array`
//! type — reflectivity, ZDR, width, PHIDP, and all 2-byte variants including
//! `DB_VEL2` — is **never** masked: a raw `0` decodes to a finite (possibly
//! very low) value. Earlier revisions blanket-masked `raw == 0 → NaN` for all
//! types, which over-masked power/phase moments relative to xradar (issue
//! #28). We now mask only where xradar does.
//!
//! Each helper takes a `raw: u16` (we widen 8-bit raws on read) plus a
//! [`DecodeParams`] carrying the per-volume `nyquist_ms` (velocity-style
//! decoders) and `wavelength_cm` (KDP). Non-dependent helpers ignore them.

/// Per-volume scalars threaded into every decoder. `Copy` so the per-gate
/// hot loop passes it by value without borrowing.
#[derive(Clone, Copy, Debug)]
pub(super) struct DecodeParams {
    /// Nyquist velocity (m/s), used by the velocity/width decoders.
    pub nyquist_ms: f32,
    /// Radar wavelength in centimetres, used by the KDP decoder.
    pub wavelength_cm: f32,
}

/// Function pointer type for per-gate decoders. The signature carries the
/// per-volume [`DecodeParams`] so velocity- and KDP-style helpers can use
/// `nyquist`/`wavelength`; other helpers ignore it.
pub(super) type Decoder = fn(raw: u16, params: DecodeParams) -> f32;

/// Pass-through (no decode applied) — used only for `DB_HCLASS`
/// (hydrometeor classification), an 8-bit integer/categorical field that
/// xradar lists with `func: None`. We keep the `raw == 0 → NaN` sentinel
/// because `0` means "no class"; categorical fields are out of scope for
/// the xradar power-moment parity work. No measurement moment routes
/// here — the 2-byte `DB_SQI2` / `DB_SNR16` that previously did now have
/// real linear decoders.
pub(super) const DECODE_NONE: Decoder = |raw, _p| {
    if raw == 0 {
        f32::NAN
    } else {
        raw as f32
    }
};

/// Linear 16-bit decoder, `(raw - 32768) / 100`. xradar decodes the modern
/// 2-byte reflectivity, velocity, ZDR and KDP types identically — plain
/// `decode_array(scale=100, offset=-32768)` with no mask — so the per-moment
/// `DECODE_*_2BYTE` aliases below all point here rather than repeat the
/// formula.
pub(super) const DECODE_LINEAR_2BYTE: Decoder = |raw, _p| (raw as f32 - 32768.0) / 100.0;

/// 8-bit DBZ-family decoder: scale=2.0, offset=-64.0. Used for DB_DBT,
/// DB_DBZ, DB_DBZC. No masking (xradar `decode_array`, no `mask`).
pub(super) const DECODE_DBZ_8BIT: Decoder = |raw, _p| (raw as f32 - 64.0) / 2.0;

/// 16-bit DBZ-family decoder (DB_DBT2, DB_DBZ2). See [`DECODE_LINEAR_2BYTE`].
pub(super) const DECODE_DBZ_2BYTE: Decoder = DECODE_LINEAR_2BYTE;

/// 8-bit velocity decoder: `value = nyquist * (raw - 128) / 127`.
///
/// xradar's `decode_vel` carries `mask: 0.0`, but that mask does **not**
/// surface as `NaN` in xradar's output: `np.ma.masked_equal(raw, 0)` leaves
/// the underlying datum untouched, and the masked arithmetic skips it, so the
/// final value at `raw == 0` collapses to `0.0` m/s (verified byte-for-byte
/// against `open_iris_datatree`, issue #28). We mirror that exactly — mapping
/// `raw == 0 → 0.0`, not `NaN` and not the formula's `-nyquist·128/127`.
pub(super) const DECODE_VEL_8BIT: Decoder = |raw, p| {
    if raw == 0 {
        0.0
    } else {
        p.nyquist_ms * (raw as f32 - 128.0) / 127.0
    }
};

/// 16-bit velocity decoder (DB_VEL2, m/s). See [`DECODE_LINEAR_2BYTE`].
pub(super) const DECODE_VEL_2BYTE: Decoder = DECODE_LINEAR_2BYTE;

/// 8-bit spectrum-width decoder: width = nyquist * raw / 256. No masking.
pub(super) const DECODE_WIDTH_8BIT: Decoder = |raw, p| p.nyquist_ms * (raw as f32) / 256.0;

/// 16-bit spectrum-width decoder: scale=100.0 (no offset, no mask).
pub(super) const DECODE_WIDTH_2BYTE: Decoder = |raw, _p| (raw as f32) / 100.0;

/// 8-bit ZDR decoder: scale=16.0, offset=-128.0. No masking.
pub(super) const DECODE_ZDR_8BIT: Decoder = |raw, _p| (raw as f32 - 128.0) / 16.0;

/// 16-bit ZDR decoder (DB_ZDR2). See [`DECODE_LINEAR_2BYTE`].
pub(super) const DECODE_ZDR_2BYTE: Decoder = DECODE_LINEAR_2BYTE;

/// 8-bit PHIDP decoder: 180.0 * (raw - 1) / 254.0 (degrees). No masking
/// (xradar `decode_phidp`, no `mask`).
pub(super) const DECODE_PHIDP_8BIT: Decoder = |raw, _p| 180.0 * (raw as f32 - 1.0) / 254.0;

/// 16-bit PHIDP decoder: 360.0 * (raw - 1) / 65534.0 (degrees, [0, 360]).
/// No masking. Mirrors xradar `decode_phidp2` (`DB_PHIDP2`, id 24,
/// `fkw {scale: 65534, offset: -1}`): `360 * decode_array(raw)`. Verified
/// against xradar 0.12.0 — raw 1/32768/65535 -> 0/180/360°. (The prior
/// `360*raw/65535 - 180` produced [-180, 180], ~180° off.)
pub(super) const DECODE_PHIDP_2BYTE: Decoder = |raw, _p| 360.0 * (raw as f32 - 1.0) / 65534.0;

/// 8-bit RHOHV/SQI decoder: sqrt((raw - 1) / 253.0). xradar's `decode_sqi`
/// masks nothing explicitly — `raw == 0` falls out as `sqrt(-1/253) = NaN`
/// naturally, so we just apply the formula.
pub(super) const DECODE_RHOHV_8BIT: Decoder = |raw, _p| ((raw as f32 - 1.0) / 253.0).sqrt();

/// Linear 16-bit decoder over the full 2^16 span, `(raw - 1) / 65536`.
/// xradar's `decode_array(scale=65536, offset=-1)` — shared verbatim by
/// `DB_RHOHV2` (id 20) and `DB_SQI2` (id 23), so both alias this rather
/// than repeat the constants (mirrors the [`DECODE_LINEAR_2BYTE`] idiom).
pub(super) const DECODE_ARRAY_65536: Decoder = |raw, _p| (raw as f32 - 1.0) / 65536.0;

/// 16-bit RHOHV decoder (`DB_RHOHV2`, id 20). See [`DECODE_ARRAY_65536`].
/// The full 2^16 span keeps ρHV ≤ 1.0; the prior /65533 let raw 65535
/// decode to 1.000015 vs xradar's 0.999969.
pub(super) const DECODE_RHOHV_2BYTE: Decoder = DECODE_ARRAY_65536;

/// 16-bit SQI decoder (`DB_SQI2`, id 23). See [`DECODE_ARRAY_65536`].
/// Previously routed to [`DECODE_NONE`], which left it as raw counts.
pub(super) const DECODE_SQI_2BYTE: Decoder = DECODE_ARRAY_65536;

/// 16-bit SNR decoder (DB_SNR16): (raw - 63) / 2.0, dB. Mirrors xradar
/// `decode_array` for id 66, whose `fkw` is `{scale: 2, offset: -63}`
/// (`(raw + offset) / scale`) — verified against xradar 0.12.0, raw
/// 0/32768 -> -31.5/16352.5. Previously routed to [`DECODE_NONE`], which
/// left it as raw counts.
pub(super) const DECODE_SNR_2BYTE: Decoder = |raw, _p| (raw as f32 - 63.0) / 2.0;

/// 8-bit KDP decoder, mirroring xradar's `decode_kdp` (4.4.20). The raw byte
/// is a *signed* int8: `0` and `-1` are the no-data sentinels (→ NaN),
/// `-128` maps to exactly 0, and everything else takes the exponential
/// transform divided by the radar wavelength (cm):
///   `-0.25 * sign(d) * 600 ** ((127 - |d|) / 126) / wavelength`.
pub(super) const DECODE_KDP_8BIT: Decoder = |raw, p| {
    // `raw` is a widened 8-bit byte; `as u8 as i8` reinterprets it as
    // the signed byte the ICD (4.4.20) defines.
    let d = (raw as u8) as i8;
    if d == 0 || d == -1 || p.wavelength_cm <= 0.0 {
        // raw 0/-1 are the IRIS no-data sentinels; a non-positive wavelength
        // means the radar's calibration wasn't decoded, so KDP is undefined
        // (guards the divide below from producing ±inf).
        f32::NAN
    } else if d == -128 {
        0.0
    } else {
        let df = d as f32;
        let v = -0.25 * df.signum() * 600f32.powf((127.0 - df.abs()) / 126.0);
        v / p.wavelength_cm
    }
};

/// 16-bit KDP decoder (DB_KDP2). See [`DECODE_LINEAR_2BYTE`].
pub(super) const DECODE_KDP_2BYTE: Decoder = DECODE_LINEAR_2BYTE;

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience params with the given nyquist and a fixed S-band-ish
    /// wavelength (10 cm) for KDP tests.
    fn params(nyquist_ms: f32) -> DecodeParams {
        DecodeParams {
            nyquist_ms,
            wavelength_cm: 10.0,
        }
    }

    /// DBZ 8-bit is unmasked now: `raw=0 → (0-64)/2 = -32 dBZ` (finite),
    /// `raw=128 → 32 dBZ`, `raw=64 → 0 dBZ`.
    #[test]
    fn decode_dbz_8bit_pin_known_values() {
        assert_eq!(DECODE_DBZ_8BIT(0, params(0.0)), -32.0);
        assert_eq!(DECODE_DBZ_8BIT(128, params(0.0)), 32.0);
        assert_eq!(DECODE_DBZ_8BIT(64, params(0.0)), 0.0);
    }

    /// Velocity: `raw=0 → 0.0 m/s` (xradar's `mask:0.0` collapses to the
    /// underlying 0, not NaN), `raw=128 → 0 m/s`, `raw=255 → +nyquist`.
    #[test]
    fn decode_vel_8bit_with_nyquist_25() {
        let p = params(25.0);
        assert_eq!(DECODE_VEL_8BIT(0, p), 0.0);
        assert!((DECODE_VEL_8BIT(128, p) - 0.0).abs() < 1e-6);
        assert!((DECODE_VEL_8BIT(255, p) - 25.0).abs() < 1e-6);
    }

    /// PHIDP 8-bit is unmasked: `raw=0 → 180*(0-1)/254 ≈ -0.71°` (finite),
    /// `raw=128 → ~90°`.
    #[test]
    fn decode_phidp_8bit_values() {
        let v0 = DECODE_PHIDP_8BIT(0, params(0.0));
        assert!(
            (v0 - (-0.7086614)).abs() < 1e-4,
            "expected ~-0.71°, got {v0}"
        );
        let v = DECODE_PHIDP_8BIT(128, params(0.0));
        assert!((v - 90.0).abs() < 0.1, "expected ~90°, got {v}");
    }

    /// 16-bit DBZ is unmasked: `raw=0 → -327.68`, `raw=65535 → 327.67`,
    /// `raw=32768 → 0 dBZ`.
    #[test]
    fn decode_dbz_2byte_unmasked() {
        assert_eq!(DECODE_DBZ_2BYTE(0, params(0.0)), -327.68);
        assert!((DECODE_DBZ_2BYTE(65535, params(0.0)) - 327.67).abs() < 1e-2);
        assert_eq!(DECODE_DBZ_2BYTE(32768, params(0.0)), 0.0);
    }

    /// RHOHV 8-bit: `raw=0 → sqrt(-1/253) = NaN` (natural, matches
    /// `decode_sqi`); `raw=254 → sqrt(253/253) = 1.0`.
    #[test]
    fn decode_rhohv_8bit_max_is_one() {
        assert!(DECODE_RHOHV_8BIT(0, params(0.0)).is_nan());
        let v = DECODE_RHOHV_8BIT(254, params(0.0));
        assert!((v - 1.0).abs() < 1e-6, "expected 1.0, got {v}");
    }

    /// KDP 8-bit: `0`/`255(-1)` → NaN, `128(-128)` → exactly 0, and a known
    /// signed value matches xradar's exponential transform at λ=10 cm.
    #[test]
    fn decode_kdp_8bit_pin_known_values() {
        let p = params(10.0);
        assert!(DECODE_KDP_8BIT(0, p).is_nan());
        assert!(DECODE_KDP_8BIT(255, p).is_nan()); // -1
        assert_eq!(DECODE_KDP_8BIT(128, p), 0.0); // -128

        // d = 100 (positive): -0.25 * 1 * 600^((127-100)/126) / 10
        let expected = -0.25 * 600f32.powf((127.0 - 100.0) / 126.0) / 10.0;
        let got = DECODE_KDP_8BIT(100, p);
        assert!(
            (got - expected).abs() < 1e-6,
            "expected {expected}, got {got}"
        );
    }

    /// A missing/zero wavelength must not produce ±inf — KDP is undefined.
    #[test]
    fn decode_kdp_8bit_zero_wavelength_is_nan() {
        let p = DecodeParams {
            nyquist_ms: 0.0,
            wavelength_cm: 0.0,
        };
        assert!(DECODE_KDP_8BIT(100, p).is_nan());
    }

    /// When PRF is unavailable nyquist falls back to 0, zeroing velocity and
    /// width rather than producing garbage.
    #[test]
    fn decode_vel_width_8bit_zero_nyquist() {
        let p = params(0.0);
        assert_eq!(DECODE_VEL_8BIT(200, p), 0.0);
        assert_eq!(DECODE_WIDTH_8BIT(200, p), 0.0);
    }

    // ── Oracle-value tests ──────────────────────────────────────────
    // The expected numbers below were computed by running xradar
    // 0.12.0's own `SIGMET_DATA_TYPES` decode functions (see the review
    // notes), NOT by re-deriving them from these same formulas — so a
    // wrong constant here fails, unlike a self-referential check.

    /// **Regression (H1).** `DB_PHIDP2` must be `360*(raw-1)/65534`,
    /// range [0, 360°], matching xradar `decode_phidp2`. The old
    /// `360*raw/65535 - 180` was ~180° off.
    #[test]
    fn decode_phidp_2byte_matches_xradar() {
        let p = params(0.0);
        assert!((DECODE_PHIDP_2BYTE(1, p) - 0.0).abs() < 1e-4);
        assert!((DECODE_PHIDP_2BYTE(32768, p) - 180.0).abs() < 1e-3);
        assert!((DECODE_PHIDP_2BYTE(65535, p) - 360.0).abs() < 1e-3);
        // Unmasked: raw 0 is finite, not NaN.
        assert!(DECODE_PHIDP_2BYTE(0, p).is_finite());
    }

    /// **Regression (M1).** `DB_RHOHV2` uses the full 2^16 span, so ρHV
    /// never exceeds 1.0. xradar: raw 32768 -> 0.4999847, 65535 ->
    /// 0.9999695.
    #[test]
    fn decode_rhohv_2byte_matches_xradar_and_stays_le_one() {
        let p = params(0.0);
        assert!((DECODE_RHOHV_2BYTE(32768, p) - 0.499_984_74).abs() < 1e-6);
        let top = DECODE_RHOHV_2BYTE(65535, p);
        assert!((top - 0.999_969_5).abs() < 1e-6);
        assert!(top <= 1.0, "ρHV must not exceed 1.0, got {top}");
    }

    /// **Regression (L3).** `DB_SQI2` / `DB_SNR16` used to pass through
    /// as raw counts via `DECODE_NONE`; they now decode linearly.
    /// xradar: SQI2 raw 32768 -> 0.4999847; SNR16 raw 32768 -> 16352.5,
    /// raw 0 -> -31.5.
    #[test]
    fn decode_sqi_snr_2byte_match_xradar() {
        let p = params(0.0);
        assert!((DECODE_SQI_2BYTE(32768, p) - 0.499_984_74).abs() < 1e-6);
        assert!((DECODE_SQI_2BYTE(65535, p) - 0.999_969_5).abs() < 1e-6);
        assert_eq!(DECODE_SNR_2BYTE(32768, p), 16352.5);
        assert_eq!(DECODE_SNR_2BYTE(0, p), -31.5);
    }

    /// **Regression (KDP oracle).** Independently computed xradar
    /// `decode_kdp` values at λ=10 cm — a wrong 0.25/600/126/127 constant
    /// fails here where the sibling self-referential test can't catch it.
    #[test]
    fn decode_kdp_matches_xradar_oracle_values() {
        let p = params(10.0);
        // xradar `decode_kdp` on SIGNED int8 input (ICD 4.4.20), λ=10 cm.
        // d = 100 (raw 100, positive branch)
        assert!((DECODE_KDP_8BIT(100, p) - (-0.098_459_62)).abs() < 1e-6);
        // d = -56 (raw 200, negative branch): sign flips the result
        // positive — the case that distinguishes signed-int8 handling
        // from a naive unsigned read.
        assert!((DECODE_KDP_8BIT(200, p) - 0.919_191_9).abs() < 1e-5);
    }

    /// RHOHV 8-bit at the sentinel boundary: `raw==1 → sqrt(0) = 0.0`.
    #[test]
    fn decode_rhohv_8bit_at_one_is_zero() {
        assert_eq!(DECODE_RHOHV_8BIT(1, params(0.0)), 0.0);
    }

    /// The 2-byte aliases really are the same linear decoder; a
    /// mis-repoint (e.g. back to a masked one) would break this.
    #[test]
    fn two_byte_linear_aliases_agree() {
        let p = params(0.0);
        for raw in [0u16, 1, 12345, 32768, 65535] {
            let base = DECODE_LINEAR_2BYTE(raw, p);
            assert_eq!(DECODE_DBZ_2BYTE(raw, p), base);
            assert_eq!(DECODE_VEL_2BYTE(raw, p), base);
            assert_eq!(DECODE_ZDR_2BYTE(raw, p), base);
            assert_eq!(DECODE_KDP_2BYTE(raw, p), base);
        }
    }

    /// **Decoder-level mask-policy pin.** Issue #28's point is that
    /// measurement moments decode to a *finite* value at `raw==0` (the
    /// below-threshold floor), not `NaN`. This pins that property of the
    /// decoder formulas themselves. It does **not** guard the wiring —
    /// that a live moment is actually routed to one of these decoders and
    /// not to `DECODE_NONE` — which is the mapping-table invariant in
    /// `mapping.rs` (`no_odim_measurement_moment_routes_to_passthrough`);
    /// the SQI2/SNR16 bug was a wiring defect, so both guards are needed.
    #[test]
    fn measurement_decoders_are_finite_at_raw_zero_and_65535() {
        let p = params(25.0);
        // (decoder, name) — the label is needed for the failure message
        // since fn pointers carry no name.
        let unmasked = [
            (DECODE_DBZ_8BIT, "DBZ8"),
            (DECODE_DBZ_2BYTE, "DBZ2"),
            (DECODE_ZDR_8BIT, "ZDR8"),
            (DECODE_ZDR_2BYTE, "ZDR2"),
            (DECODE_WIDTH_8BIT, "WIDTH8"),
            (DECODE_WIDTH_2BYTE, "WIDTH2"),
            (DECODE_PHIDP_8BIT, "PHIDP8"),
            (DECODE_PHIDP_2BYTE, "PHIDP2"),
            (DECODE_RHOHV_2BYTE, "RHOHV2"),
            (DECODE_SQI_2BYTE, "SQI2"),
            (DECODE_SNR_2BYTE, "SNR16"),
            (DECODE_VEL_8BIT, "VEL8"),
            (DECODE_VEL_2BYTE, "VEL2"),
            (DECODE_KDP_2BYTE, "KDP2"),
        ];
        for (dec, name) in unmasked {
            for raw in [0u16, 65535] {
                assert!(
                    dec(raw, p).is_finite(),
                    "{name} must be finite at raw={raw} (no re-masking)"
                );
            }
        }
    }
}
