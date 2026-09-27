//! MSG_5 (Volume Coverage Pattern) parser — ICD §3.2.4.12 Table XI.
//!
//! Layout:
//!
//! * 11 halfwords header (HW 1-11): message size, pattern type,
//!   pattern number, elevation cut count, version, clutter map
//!   group, doppler velocity resolution + pulse width (packed in
//!   HW 6), 2 reserved halfwords, VCP sequencing, VCP supplemental
//!   data, 1 reserved halfword.
//! * Then `number_of_elevation_cuts × 23 halfwords` per ICD
//!   footnote 18: `EX = (12 + (X-1)) + ((Cut-1) * 23)`.
//!
//! Each elevation cut (46 bytes) carries the commanded angle, scan
//! parameters, SAILS/MRLE/MPDA/BASE-TILT classification flags, and
//! per-sector metadata.

use crate::backends::nexrad::decode::error::{NexradDecodeError, Result};
use crate::backends::nexrad::decode::reader::SliceReader;

/// Halfwords per elevation cut block (E1-E23 per ICD Note 18).
pub(crate) const ELEVATION_CUT_HALFWORDS: usize = 23;

/// One elevation cut from an MSG_5 VCP definition (ICD Table XI
/// E1-E23). Field types follow ICD; we preserve raw values for
/// fields that are bit-packed (channel_configuration,
/// waveform_type, super_resolution_control, supplemental_data) so
/// downstream consumers can apply their own interpretations.
///
/// `Default` is provided to support `synthetic_from_radials`'s
/// zero-filled cuts when no real MSG_5 is present in the file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct ElevationCut {
    /// E1 — commanded elevation angle as a Code*2 binary angle
    /// (Table III-A). Stored raw; convert via
    /// `binary_angle_degrees(self.elevation_angle_raw)`.
    pub(crate) elevation_angle_raw: u16,
    /// E2 upper byte — channel configuration code.
    pub(crate) channel_configuration: u8,
    /// E2 lower byte — waveform type code.
    pub(crate) waveform_type: u8,
    /// E3 upper byte — super-resolution control bits.
    pub(crate) super_resolution_control: u8,
    /// E3 lower byte — surveillance PRF number (1-8, 0 = N/A).
    pub(crate) surveillance_prf_number: u8,
    /// E4 — surveillance pulse count per radial.
    pub(crate) surveillance_pulse_count: u16,
    /// E5 — azimuth rate (Table XI-D BA velocity).
    pub(crate) azimuth_rate_raw: u16,
    /// E6-E11 — SNR thresholds (scaled signed integer, /8 → dB).
    pub(crate) reflectivity_threshold_raw: i16,
    pub(crate) velocity_threshold_raw: i16,
    pub(crate) spectrum_width_threshold_raw: i16,
    pub(crate) differential_reflectivity_threshold_raw: i16,
    pub(crate) differential_phase_threshold_raw: i16,
    pub(crate) correlation_coefficient_threshold_raw: i16,
    /// E12-E14 — Sector 1 (edge angle, doppler PRF #, pulse count).
    pub(crate) sector1_edge_angle_raw: u16,
    pub(crate) sector1_doppler_prf_number: u16,
    pub(crate) sector1_doppler_pulse_count: u16,
    /// E15 — supplemental data (SAILS / MRLE / MPDA / BASE-TILT bits).
    pub(crate) supplemental_data: u16,
    /// E16-E18 — Sector 2.
    pub(crate) sector2_edge_angle_raw: u16,
    pub(crate) sector2_doppler_prf_number: u16,
    pub(crate) sector2_doppler_pulse_count: u16,
    /// E19 — EBC angle correction (binary angle).
    pub(crate) ebc_angle_raw: u16,
    /// E20-E22 — Sector 3.
    pub(crate) sector3_edge_angle_raw: u16,
    pub(crate) sector3_doppler_prf_number: u16,
    pub(crate) sector3_doppler_pulse_count: u16,
    /// E23 — reserved halfword.
    pub(crate) reserved: u16,
}

/// Decoded MSG_5 VCP definition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Msg5 {
    pub(crate) message_size_halfwords: u16,
    pub(crate) pattern_type: u16,
    pub(crate) pattern_number: u16,
    pub(crate) number_of_elevation_cuts: u16,
    pub(crate) vcp_version: u8,
    pub(crate) clutter_map_group_number: u8,
    /// HW 6 upper byte — doppler velocity resolution code
    /// (2 = 0.5 m/s, 4 = 1.0 m/s).
    pub(crate) doppler_velocity_resolution: u8,
    /// HW 6 lower byte — pulse width code (2 = short, 4 = long).
    pub(crate) pulse_width: u8,
    /// HW 9 — VCP sequencing flags (per ICD Note 15).
    pub(crate) vcp_sequencing: u16,
    /// HW 10 — VCP supplemental data (SAILS/MRLE/MPDA/BASE-TILT
    /// flags, per ICD Note 16).
    pub(crate) vcp_supplemental: u16,
    pub(crate) elevation_cuts: Vec<ElevationCut>,
}

impl Msg5 {
    /// VCP pattern number (e.g. 212 for VCP-212). Mirrors upstream's
    /// `VolumeCoveragePattern::number()` accessor — wraps the
    /// `pattern_number` field.
    pub(crate) fn number(&self) -> u16 {
        self.pattern_number
    }

    /// Best-effort human-readable VCP description (e.g. "Convective
    /// precipitation"). Lookup table mirrors xradar's
    /// `_VCP_DESCRIPTIONS`. Returns `"Unknown VCP"` for unknown
    /// pattern numbers so the adapter's `vcp_description` attr is
    /// always populated.
    pub(crate) fn description(&self) -> &'static str {
        match self.pattern_number {
            12 | 212 => "Convective precipitation, dual-pol",
            21 | 121 => "Convective precipitation, mid-range",
            31 | 32 => "Clear-air mode, light or no precipitation",
            34 => "Clear-air mode, dual-pol",
            35 => "Clear-air mode, dual-pol (high res)",
            80 => "Convective precipitation (deprecated)",
            90 => "Convective precipitation, special",
            112 => "MPDA convective precipitation",
            215 => "Convective precipitation, dual-pol (high alt)",
            _ => "Unknown VCP",
        }
    }

    /// Number of elevation cuts in this VCP, as a `u8` for parity
    /// with upstream's accessor (HW 4 of MSG_5 is `u16` but values
    /// are constrained to 1..32 by the ICD).
    pub(crate) fn number_of_elevation_cuts_u8(&self) -> u8 {
        self.number_of_elevation_cuts as u8
    }

    /// Slice of elevation cuts. Mirrors upstream's accessor.
    pub(crate) fn elevation_cuts(&self) -> &[ElevationCut] {
        &self.elevation_cuts
    }

    /// SAILS enabled — bit 0 of `vcp_supplemental` (HW 10) per ICD
    /// Note 16. Bits 1-3 give the SAILS cut count (max 3).
    pub(crate) fn sails_enabled(&self) -> bool {
        self.vcp_supplemental & 0b0001 != 0
    }

    /// Number of SAILS cuts (bits 1-3 of `vcp_supplemental`).
    pub(crate) fn sails_cuts(&self) -> u8 {
        ((self.vcp_supplemental >> 1) & 0b0111) as u8
    }

    /// MRLE enabled — bit 4 of `vcp_supplemental` per ICD Note 16.
    pub(crate) fn mrle_enabled(&self) -> bool {
        self.vcp_supplemental & 0b0001_0000 != 0
    }

    /// Number of MRLE cuts (bits 5-7 of `vcp_supplemental`, max 4).
    pub(crate) fn mrle_cuts(&self) -> u8 {
        ((self.vcp_supplemental >> 5) & 0b0111) as u8
    }

    /// MPDA VCP — bit 11 of `vcp_supplemental` per ICD Note 16.
    pub(crate) fn mpda_enabled(&self) -> bool {
        self.vcp_supplemental & 0b1000_0000_0000 != 0
    }

    /// VCP contains at least one BASE TILT — bit 12 of
    /// `vcp_supplemental` per ICD Note 16.
    pub(crate) fn base_tilt_enabled(&self) -> bool {
        self.vcp_supplemental & 0b0001_0000_0000_0000 != 0
    }

    /// Number of BASE TILTs (bits 13-15 of `vcp_supplemental`).
    pub(crate) fn base_tilt_count(&self) -> u8 {
        ((self.vcp_supplemental >> 13) & 0b0111) as u8
    }

    /// VCP truncated in number of elevation cuts — bit 14 of
    /// `vcp_sequencing` (HW 9) per ICD Note 15. Set when this VCP is
    /// part of an active VCP Sequence and truncated.
    pub(crate) fn truncated(&self) -> bool {
        self.vcp_sequencing & 0b0100_0000_0000_0000 != 0
    }

    /// VCP is part of an active VCP Sequence — bit 13 of
    /// `vcp_sequencing` per ICD Note 15.
    pub(crate) fn sequence_active(&self) -> bool {
        self.vcp_sequencing & 0b0010_0000_0000_0000 != 0
    }

    /// Doppler velocity resolution in m/s. ICD HW 6 upper byte
    /// encodes 2 → 0.5 m/s, 4 → 1.0 m/s.
    pub(crate) fn doppler_velocity_resolution_m_per_s(&self) -> f32 {
        match self.doppler_velocity_resolution {
            2 => 0.5,
            4 => 1.0,
            _ => f32::NAN,
        }
    }

    /// xradar-parity pulse-width string ("short" / "long" / `""`
    /// for unknown). ICD HW 6 lower byte: 2 → short, 4 → long.
    pub(crate) fn pulse_width_str(&self) -> &'static str {
        match self.pulse_width {
            2 => "short",
            4 => "long",
            _ => "",
        }
    }

    /// Build a synthetic MSG_5 from a list of radials when the source
    /// file lacks one (common with pre-Build-12 raw Archive II — many
    /// 1990s/2000s files simply don't carry a VCP record). Pattern
    /// number falls back to `0`; doppler velocity resolution comes
    /// from the elevation cut count alone. Per-cut elevation angles
    /// use the median measured angle for that elevation_number.
    ///
    /// This is a **placeholder** to keep the rest of the pipeline's
    /// hard MSG_5 dependency satisfied — downstream consumers that
    /// rely on SAILS / MRLE classification flags will see all-zeros
    /// (i.e. legacy files report no SAILS/MRLE).
    pub(crate) fn synthetic_from_radials(
        radials: &[crate::backends::nexrad::decode::model::Radial],
    ) -> Self {
        use std::collections::BTreeMap;

        let mut by_elev: BTreeMap<u8, Vec<f32>> = BTreeMap::new();
        for r in radials {
            by_elev
                .entry(r.elevation_number)
                .or_default()
                .push(r.elevation_angle_degrees);
        }
        let mut elevation_cuts: Vec<ElevationCut> = Vec::with_capacity(by_elev.len());
        for (_, mut angles) in by_elev {
            if angles.is_empty() {
                continue;
            }
            // `f32::total_cmp` is the IEEE-754-aware total order — avoids
            // the `partial_cmp(...).unwrap_or(Equal)` NaN dance.
            angles.sort_by(|a, b| a.total_cmp(b));
            let median = angles[angles.len() / 2];
            // Inverse of binary_angle_degrees: raw = degrees * 32768 / 180.
            let raw = (median * 32768.0 / 180.0).round().clamp(0.0, 65535.0) as u16;
            elevation_cuts.push(ElevationCut {
                elevation_angle_raw: raw,
                ..ElevationCut::default()
            });
        }
        let number_of_elevation_cuts = elevation_cuts.len() as u16;
        Self {
            number_of_elevation_cuts,
            doppler_velocity_resolution: 2,
            elevation_cuts,
            ..Self::default()
        }
    }

    pub(crate) fn read(reader: &mut SliceReader<'_>) -> Result<Self> {
        let message_size_halfwords = reader.read_u16_be()?;
        let pattern_type = reader.read_u16_be()?;
        let pattern_number = reader.read_u16_be()?;
        let number_of_elevation_cuts = reader.read_u16_be()?;
        let vcp_version = reader.read_u8()?;
        let clutter_map_group_number = reader.read_u8()?;
        let doppler_velocity_resolution = reader.read_u8()?;
        let pulse_width = reader.read_u8()?;
        let _reserved_hw7 = reader.read_u16_be()?;
        let _reserved_hw8 = reader.read_u16_be()?;
        let vcp_sequencing = reader.read_u16_be()?;
        let vcp_supplemental = reader.read_u16_be()?;
        let _reserved_hw11 = reader.read_u16_be()?;

        // Sanity-check elevation count to keep `Vec::with_capacity`
        // honest. ICD Table XI: 1..32.
        if number_of_elevation_cuts == 0 || number_of_elevation_cuts > 32 {
            return Err(NexradDecodeError::MalformedHeader {
                offset: 0,
                reason: "MSG_5 number_of_elevation_cuts outside ICD 1..32",
            });
        }

        let mut elevation_cuts = Vec::with_capacity(number_of_elevation_cuts as usize);
        for _ in 0..number_of_elevation_cuts {
            elevation_cuts.push(ElevationCut::read(reader)?);
        }

        Ok(Self {
            message_size_halfwords,
            pattern_type,
            pattern_number,
            number_of_elevation_cuts,
            vcp_version,
            clutter_map_group_number,
            doppler_velocity_resolution,
            pulse_width,
            vcp_sequencing,
            vcp_supplemental,
            elevation_cuts,
        })
    }
}

impl ElevationCut {
    fn read(reader: &mut SliceReader<'_>) -> Result<Self> {
        let elevation_angle_raw = reader.read_u16_be()?;
        let channel_configuration = reader.read_u8()?;
        let waveform_type = reader.read_u8()?;
        let super_resolution_control = reader.read_u8()?;
        let surveillance_prf_number = reader.read_u8()?;
        let surveillance_pulse_count = reader.read_u16_be()?;
        let azimuth_rate_raw = reader.read_u16_be()?;
        let reflectivity_threshold_raw = reader.read_i16_be()?;
        let velocity_threshold_raw = reader.read_i16_be()?;
        let spectrum_width_threshold_raw = reader.read_i16_be()?;
        let differential_reflectivity_threshold_raw = reader.read_i16_be()?;
        let differential_phase_threshold_raw = reader.read_i16_be()?;
        let correlation_coefficient_threshold_raw = reader.read_i16_be()?;
        let sector1_edge_angle_raw = reader.read_u16_be()?;
        let sector1_doppler_prf_number = reader.read_u16_be()?;
        let sector1_doppler_pulse_count = reader.read_u16_be()?;
        let supplemental_data = reader.read_u16_be()?;
        let sector2_edge_angle_raw = reader.read_u16_be()?;
        let sector2_doppler_prf_number = reader.read_u16_be()?;
        let sector2_doppler_pulse_count = reader.read_u16_be()?;
        let ebc_angle_raw = reader.read_u16_be()?;
        let sector3_edge_angle_raw = reader.read_u16_be()?;
        let sector3_doppler_prf_number = reader.read_u16_be()?;
        let sector3_doppler_pulse_count = reader.read_u16_be()?;
        let reserved = reader.read_u16_be()?;
        Ok(Self {
            elevation_angle_raw,
            channel_configuration,
            waveform_type,
            super_resolution_control,
            surveillance_prf_number,
            surveillance_pulse_count,
            azimuth_rate_raw,
            reflectivity_threshold_raw,
            velocity_threshold_raw,
            spectrum_width_threshold_raw,
            differential_reflectivity_threshold_raw,
            differential_phase_threshold_raw,
            correlation_coefficient_threshold_raw,
            sector1_edge_angle_raw,
            sector1_doppler_prf_number,
            sector1_doppler_pulse_count,
            supplemental_data,
            sector2_edge_angle_raw,
            sector2_doppler_prf_number,
            sector2_doppler_pulse_count,
            ebc_angle_raw,
            sector3_edge_angle_raw,
            sector3_doppler_prf_number,
            sector3_doppler_pulse_count,
            reserved,
        })
    }

    /// Convert the `elevation_angle_raw` (Code*2 binary angle per
    /// Table III-A) to degrees. Bits 3-15 carry the angle, weighted
    /// `180 * 2^(bit_index - 15)`. The upstream `nexrad-decode`
    /// uses this same formula.
    pub(crate) fn elevation_angle_degrees(&self) -> f32 {
        binary_angle_degrees(self.elevation_angle_raw)
    }

    /// `f64` flavour for the adapter's `metadata.sweep_fixed_angles`
    /// path which stores fixed angles as f64.
    pub(crate) fn elevation_angle_degrees_f64(&self) -> f64 {
        f64::from(self.elevation_angle_degrees())
    }

    /// Per ICD Note 17, bit 0 of `supplemental_data` (E15): SAILS
    /// cut. Bits 1-3 carry the SAILS sequence number.
    pub(crate) fn is_sails_cut(&self) -> bool {
        self.supplemental_data & 0b0001 != 0
    }

    pub(crate) fn sails_sequence_number(&self) -> u8 {
        ((self.supplemental_data >> 1) & 0b0111) as u8
    }

    /// Bit 4 of `supplemental_data`: MRLE cut. Bits 5-7 carry the
    /// MRLE sequence number.
    pub(crate) fn is_mrle_cut(&self) -> bool {
        self.supplemental_data & 0b0001_0000 != 0
    }

    pub(crate) fn mrle_sequence_number(&self) -> u8 {
        ((self.supplemental_data >> 5) & 0b0111) as u8
    }

    /// Bit 9 of `supplemental_data`: MPDA cut.
    pub(crate) fn is_mpda_cut(&self) -> bool {
        self.supplemental_data & 0b0010_0000_0000 != 0
    }

    /// Bit 10 of `supplemental_data`: BASE TILT cut.
    pub(crate) fn is_base_tilt_cut(&self) -> bool {
        self.supplemental_data & 0b0100_0000_0000 != 0
    }

    /// E3 super-resolution control bits (ICD Table XI E3 upper
    /// byte). Bits per xradar's `pack_super_resolution`:
    /// 0 = half-degree azimuth, 1 = quarter-km reflectivity,
    /// 2 = doppler to 300 km, 3 = dual-pol to 300 km.
    pub(crate) fn super_resolution_half_degree_azimuth(&self) -> bool {
        self.super_resolution_control & 0b0001 != 0
    }

    pub(crate) fn super_resolution_quarter_km_reflectivity(&self) -> bool {
        self.super_resolution_control & 0b0010 != 0
    }

    pub(crate) fn super_resolution_doppler_to_300km(&self) -> bool {
        self.super_resolution_control & 0b0100 != 0
    }

    pub(crate) fn super_resolution_dual_pol_to_300km(&self) -> bool {
        self.super_resolution_control & 0b1000 != 0
    }
}

/// Convert an ICD Table III-A binary angle (Code*2) to degrees.
/// Bits 0-2 are unused; bits 3-15 carry the angle, with weight
/// `180 * 2^(bit_index - 15)`.
pub(crate) fn binary_angle_degrees(raw: u16) -> f32 {
    let mut angle = 0.0_f32;
    for i in 3i32..16 {
        if raw & (1u16 << i) != 0 {
            angle += 180.0_f32 * (2.0_f32).powi(i - 15);
        }
    }
    angle
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an N-elevation-cut MSG_5 with `n_cuts` zeroed cuts +
    /// distinct pattern_number / vcp_version values to verify the
    /// header round-trip.
    fn synth_msg5(n_cuts: u16) -> Vec<u8> {
        let mut buf = Vec::new();
        // HW 1..11 — header.
        let total_halfwords = 11 + n_cuts * ELEVATION_CUT_HALFWORDS as u16;
        buf.extend_from_slice(&total_halfwords.to_be_bytes()); // HW 1
        buf.extend_from_slice(&2u16.to_be_bytes()); // HW 2 pattern_type
        buf.extend_from_slice(&212u16.to_be_bytes()); // HW 3 pattern_number
        buf.extend_from_slice(&n_cuts.to_be_bytes()); // HW 4 n_cuts
        buf.push(1); // HW 5 upper: vcp_version
        buf.push(1); // HW 5 lower: clutter_map_group
        buf.push(2); // HW 6 upper: dop_vel_res = 0.5 m/s
        buf.push(2); // HW 6 lower: pulse_width = short
        buf.extend_from_slice(&0u16.to_be_bytes()); // HW 7 reserved
        buf.extend_from_slice(&0u16.to_be_bytes()); // HW 8 reserved
        buf.extend_from_slice(&(n_cuts | (3u16 << 5)).to_be_bytes()); // HW 9 vcp_sequencing
        buf.extend_from_slice(&(1u16 | (1u16 << 1)).to_be_bytes()); // HW 10 supplemental: SAILS + 1 SAILS cut
        buf.extend_from_slice(&0u16.to_be_bytes()); // HW 11 reserved
                                                    // Elevation cuts — distinct per-cut elevation_angle_raw so
                                                    // we can verify ordering.
        for i in 0..n_cuts {
            // E1 — angle; use a non-trivial bit pattern per cut.
            buf.extend_from_slice(&((i + 1) << 8).to_be_bytes()); // E1
            buf.push(0); // E2 channel_configuration
            buf.push(1); // E2 waveform_type (= CS)
            buf.push(0); // E3 super_resolution_control
            buf.push(1); // E3 surveillance_prf_number
            buf.extend_from_slice(&17u16.to_be_bytes()); // E4 pulse_count
            buf.extend_from_slice(&0u16.to_be_bytes()); // E5 azimuth_rate
                                                        // E6..E11 — six SNR thresholds, all 0
            for _ in 0..6 {
                buf.extend_from_slice(&0_i16.to_be_bytes());
            }
            // E12..E14 — sector 1
            buf.extend_from_slice(&0u16.to_be_bytes()); // edge_angle
            buf.extend_from_slice(&0u16.to_be_bytes()); // prf_number
            buf.extend_from_slice(&0u16.to_be_bytes()); // pulse_count
                                                        // E15 supplemental_data — set sails_cut bit on first
                                                        // cut to verify per-cut decoding.
            let supp = if i == 0 { 1 } else { 0 };
            buf.extend_from_slice(&(supp as u16).to_be_bytes());
            // E16..E18 — sector 2
            buf.extend_from_slice(&0u16.to_be_bytes());
            buf.extend_from_slice(&0u16.to_be_bytes());
            buf.extend_from_slice(&0u16.to_be_bytes());
            // E19 — EBC angle
            buf.extend_from_slice(&0u16.to_be_bytes());
            // E20..E22 — sector 3
            buf.extend_from_slice(&0u16.to_be_bytes());
            buf.extend_from_slice(&0u16.to_be_bytes());
            buf.extend_from_slice(&0u16.to_be_bytes());
            // E23 — reserved
            buf.extend_from_slice(&0u16.to_be_bytes());
        }
        debug_assert_eq!(
            buf.len(),
            (11 + (n_cuts as usize) * ELEVATION_CUT_HALFWORDS) * 2
        );
        buf
    }

    #[test]
    fn read_consumes_full_header_and_cuts() {
        let bytes = synth_msg5(13);
        let mut r = SliceReader::new(&bytes);
        let msg = Msg5::read(&mut r).unwrap();
        assert_eq!(msg.pattern_number, 212);
        assert_eq!(msg.number_of_elevation_cuts, 13);
        assert_eq!(msg.vcp_version, 1);
        assert_eq!(msg.doppler_velocity_resolution, 2);
        assert_eq!(msg.pulse_width, 2);
        assert_eq!(msg.elevation_cuts.len(), 13);
        // First cut has supplemental_data = 1 (SAILS) per the synth.
        assert_eq!(msg.elevation_cuts[0].supplemental_data, 1);
        // Subsequent cuts have supplemental_data = 0.
        assert!(msg.elevation_cuts[1..]
            .iter()
            .all(|c| c.supplemental_data == 0));
    }

    #[test]
    fn read_rejects_zero_or_oversized_elevation_count() {
        let mut bytes = synth_msg5(3);
        // HW 4 (number_of_elevation_cuts) → bytes 6..8.
        bytes[6..8].copy_from_slice(&0u16.to_be_bytes());
        let mut r = SliceReader::new(&bytes);
        assert!(Msg5::read(&mut r).is_err());

        let mut bytes = synth_msg5(3);
        bytes[6..8].copy_from_slice(&33u16.to_be_bytes());
        let mut r = SliceReader::new(&bytes);
        assert!(Msg5::read(&mut r).is_err());
    }

    #[test]
    fn read_errors_on_truncated_cut() {
        // Header says 5 cuts but only 2 cuts of bytes follow.
        let mut bytes = synth_msg5(5);
        bytes.truncate(11 * 2 + 2 * ELEVATION_CUT_HALFWORDS * 2);
        let mut r = SliceReader::new(&bytes);
        assert!(Msg5::read(&mut r).is_err());
    }

    #[test]
    fn vcp_supplemental_bit_extractors_match_icd_note_16() {
        let cuts = vec![ElevationCut {
            elevation_angle_raw: 0,
            channel_configuration: 0,
            waveform_type: 1,
            super_resolution_control: 0,
            surveillance_prf_number: 1,
            surveillance_pulse_count: 0,
            azimuth_rate_raw: 0,
            reflectivity_threshold_raw: 0,
            velocity_threshold_raw: 0,
            spectrum_width_threshold_raw: 0,
            differential_reflectivity_threshold_raw: 0,
            differential_phase_threshold_raw: 0,
            correlation_coefficient_threshold_raw: 0,
            sector1_edge_angle_raw: 0,
            sector1_doppler_prf_number: 0,
            sector1_doppler_pulse_count: 0,
            supplemental_data: 0,
            sector2_edge_angle_raw: 0,
            sector2_doppler_prf_number: 0,
            sector2_doppler_pulse_count: 0,
            ebc_angle_raw: 0,
            sector3_edge_angle_raw: 0,
            sector3_doppler_prf_number: 0,
            sector3_doppler_pulse_count: 0,
            reserved: 0,
        }];
        let mut msg = Msg5 {
            message_size_halfwords: 0,
            pattern_type: 0,
            pattern_number: 212,
            number_of_elevation_cuts: 1,
            vcp_version: 0,
            clutter_map_group_number: 0,
            doppler_velocity_resolution: 2,
            pulse_width: 2,
            vcp_sequencing: 0,
            vcp_supplemental: 0b0001 | (0b010 << 1), // SAILS + 2 SAILS cuts
            elevation_cuts: cuts.clone(),
        };
        assert!(msg.sails_enabled());
        assert_eq!(msg.sails_cuts(), 2);
        assert!(!msg.mrle_enabled());

        msg.vcp_supplemental = 0b0001_0000 | (0b011 << 5); // MRLE + 3 MRLE cuts
        assert!(!msg.sails_enabled());
        assert!(msg.mrle_enabled());
        assert_eq!(msg.mrle_cuts(), 3);

        msg.vcp_supplemental = 0b1000_0000_0000; // bit 11: MPDA
        assert!(msg.mpda_enabled());
        msg.vcp_supplemental = 0b0001_0000_0000_0000 | (0b001 << 13); // bit 12: BASE TILT, bits 13-15: 1
        assert!(msg.base_tilt_enabled());
        assert_eq!(msg.base_tilt_count(), 1);
    }

    #[test]
    fn vcp_sequencing_truncated_and_active_bits() {
        let mut msg = Msg5 {
            message_size_halfwords: 0,
            pattern_type: 0,
            pattern_number: 212,
            number_of_elevation_cuts: 1,
            vcp_version: 0,
            clutter_map_group_number: 0,
            doppler_velocity_resolution: 2,
            pulse_width: 2,
            vcp_sequencing: 0,
            vcp_supplemental: 0,
            elevation_cuts: vec![],
        };
        msg.vcp_sequencing = 0b0010_0000_0000_0000; // bit 13
        assert!(msg.sequence_active());
        assert!(!msg.truncated());
        msg.vcp_sequencing = 0b0100_0000_0000_0000; // bit 14
        assert!(msg.truncated());
        assert!(!msg.sequence_active());
    }

    #[test]
    fn elevation_cut_supplemental_bit_extractors_match_icd_note_17() {
        let mut cut = ElevationCut {
            elevation_angle_raw: 0,
            channel_configuration: 0,
            waveform_type: 1,
            super_resolution_control: 0,
            surveillance_prf_number: 1,
            surveillance_pulse_count: 0,
            azimuth_rate_raw: 0,
            reflectivity_threshold_raw: 0,
            velocity_threshold_raw: 0,
            spectrum_width_threshold_raw: 0,
            differential_reflectivity_threshold_raw: 0,
            differential_phase_threshold_raw: 0,
            correlation_coefficient_threshold_raw: 0,
            sector1_edge_angle_raw: 0,
            sector1_doppler_prf_number: 0,
            sector1_doppler_pulse_count: 0,
            supplemental_data: 0,
            sector2_edge_angle_raw: 0,
            sector2_doppler_prf_number: 0,
            sector2_doppler_pulse_count: 0,
            ebc_angle_raw: 0,
            sector3_edge_angle_raw: 0,
            sector3_doppler_prf_number: 0,
            sector3_doppler_pulse_count: 0,
            reserved: 0,
        };
        cut.supplemental_data = 0b0001 | (0b010 << 1); // SAILS cut, sequence 2
        assert!(cut.is_sails_cut());
        assert_eq!(cut.sails_sequence_number(), 2);
        assert!(!cut.is_mrle_cut());

        cut.supplemental_data = 0b0001_0000 | (0b011 << 5); // MRLE cut, sequence 3
        assert!(!cut.is_sails_cut());
        assert!(cut.is_mrle_cut());
        assert_eq!(cut.mrle_sequence_number(), 3);

        cut.supplemental_data = 0b0010_0000_0000; // bit 9: MPDA
        assert!(cut.is_mpda_cut());

        cut.supplemental_data = 0b0100_0000_0000; // bit 10: BASE TILT
        assert!(cut.is_base_tilt_cut());
    }

    #[test]
    fn super_resolution_control_bits() {
        let mut cut = ElevationCut {
            elevation_angle_raw: 0,
            channel_configuration: 0,
            waveform_type: 1,
            super_resolution_control: 0,
            surveillance_prf_number: 1,
            surveillance_pulse_count: 0,
            azimuth_rate_raw: 0,
            reflectivity_threshold_raw: 0,
            velocity_threshold_raw: 0,
            spectrum_width_threshold_raw: 0,
            differential_reflectivity_threshold_raw: 0,
            differential_phase_threshold_raw: 0,
            correlation_coefficient_threshold_raw: 0,
            sector1_edge_angle_raw: 0,
            sector1_doppler_prf_number: 0,
            sector1_doppler_pulse_count: 0,
            supplemental_data: 0,
            sector2_edge_angle_raw: 0,
            sector2_doppler_prf_number: 0,
            sector2_doppler_pulse_count: 0,
            ebc_angle_raw: 0,
            sector3_edge_angle_raw: 0,
            sector3_doppler_prf_number: 0,
            sector3_doppler_pulse_count: 0,
            reserved: 0,
        };
        cut.super_resolution_control = 0b1001; // half-deg azimuth + dual-pol 300km
        assert!(cut.super_resolution_half_degree_azimuth());
        assert!(!cut.super_resolution_quarter_km_reflectivity());
        assert!(!cut.super_resolution_doppler_to_300km());
        assert!(cut.super_resolution_dual_pol_to_300km());
    }

    #[test]
    fn binary_angle_decodes_known_values() {
        // Bit 15 = 180 * 2^0 = 180°.
        assert!((binary_angle_degrees(0x8000) - 180.0).abs() < 1e-3);
        // Bit 14 = 180 * 2^-1 = 90°.
        assert!((binary_angle_degrees(0x4000) - 90.0).abs() < 1e-3);
        // 0x0100 = bit 8 = 180 * 2^-7 ≈ 1.40625°. Used by the
        // synth_msg5 helper so the cuts have distinguishable angles.
        let v = binary_angle_degrees(0x0100);
        assert!((v - 1.40625).abs() < 1e-3, "got {v}");
    }
}
