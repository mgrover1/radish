//! Build `NexradVolumeAttrs` and `NexradSweepAttrs` from upstream MSG_2 / MSG_5
//! decoded structs, matching xradar's emission verbatim.
//!
//! Mapping decisions here are deliberately conservative and mirror what
//! xradar's `open_nexradlevel2_datatree` does in
//! `xradar/io/backends/nexrad_level2.py`. If a value diverges, the user-facing
//! `xr.DataTree` `.attrs` will diverge — that's the contract.

use crate::backends::nexrad::decode::messages::msg2::Msg2;
use crate::backends::nexrad::decode::messages::msg5::{ElevationCut, Msg5};
use crate::backends::nexrad::decode::model::Sweep;
use crate::{NexradSweepAttrs, NexradVolumeAttrs};

/// xradar's `_WAVEFORM_TYPES` lookup. We match xradar **by raw byte**, not by
/// ICD semantics: xradar's table is off-by-one vs the ICD (raw=3 ⇒ "batch"
/// in xradar, but "Contiguous Doppler without Ambiguity Resolution" per ICD
/// 2620002AA), and raw=5 falls through to `str(code)`. The whole point of
/// this code path is *xradar parity*, so we emit what xradar would emit
/// regardless of whether xradar matches the spec.
///
/// Cached strings for raw=5/6/... avoid allocating per call.
pub(super) fn waveform_type_str_from_raw(raw: u8) -> String {
    match raw {
        0 => "not_applicable".to_string(),
        1 => "contiguous_surveillance".to_string(),
        2 => "contiguous_doppler".to_string(),
        3 => "batch".to_string(),
        4 => "staggered_pulse_pair".to_string(),
        // xradar falls through to `_WAVEFORM_TYPES.get(wf, str(wf))`.
        other => other.to_string(),
    }
}

/// xradar's `_CHANNEL_CONFIGS` lookup. Same raw-byte rule as above.
pub(super) fn channel_config_str_from_raw(raw: u8) -> String {
    match raw {
        0 => "constant_phase".to_string(),
        1 => "random_phase".to_string(),
        2 => "sz2_phase_coding".to_string(),
        other => other.to_string(),
    }
}

/// xradar's `_get_dynamic_scan_type`. SAILS and MRLE are mutually exclusive
/// per ICD 2620002AA Note 16.
pub(super) fn dynamic_scan_type(vcp: &Msg5) -> String {
    if vcp.sails_enabled() {
        let n = vcp.sails_cuts();
        if n == 0 {
            "SAILS".to_string()
        } else {
            format!("SAILS x {n}")
        }
    } else if vcp.mrle_enabled() {
        let n = vcp.mrle_cuts();
        if n == 0 {
            "MRLE".to_string()
        } else {
            format!("MRLE x {n}")
        }
    } else {
        "standard".to_string()
    }
}

/// Pack the four super-resolution control bits back into the 4-bit code that
/// xradar publishes as `super_resolution`. Bit layout per ICD MSG_5_ELEV E4:
/// bit 0 = 0.5° azimuth, bit 1 = 1/4 km reflectivity, bit 2 = Doppler to 300
/// km, bit 3 = dual-pol to 300 km.
fn pack_super_resolution(cut: &ElevationCut) -> u8 {
    let mut bits = 0u8;
    if cut.super_resolution_half_degree_azimuth() {
        bits |= 0b0001;
    }
    if cut.super_resolution_quarter_km_reflectivity() {
        bits |= 0b0010;
    }
    if cut.super_resolution_doppler_to_300km() {
        bits |= 0b0100;
    }
    if cut.super_resolution_dual_pol_to_300km() {
        bits |= 0b1000;
    }
    bits
}

/// Build the per-sweep attrs from a single MSG_5 elevation cut.
///
/// xradar emits waveform / channel-config strings keyed off the raw
/// E3 / E2 bytes. Our internal decoder keeps those raw bytes
/// alongside their bit-packed siblings — no round-trip through a
/// typed enum needed.
pub(super) fn sweep_attrs_from_cut(cut: &ElevationCut) -> NexradSweepAttrs {
    NexradSweepAttrs {
        waveform_type: waveform_type_str_from_raw(cut.waveform_type),
        channel_config: channel_config_str_from_raw(cut.channel_configuration),
        super_resolution: pack_super_resolution(cut),
        sails_cut: cut.is_sails_cut(),
        sails_sequence_number: cut.sails_sequence_number(),
        mrle_cut: cut.is_mrle_cut(),
        mrle_sequence_number: cut.mrle_sequence_number(),
        mpda_cut: cut.is_mpda_cut(),
        base_tilt_cut: cut.is_base_tilt_cut(),
    }
}

/// xradar's `decode_rda_scan_data_flags`. Decodes the raw HW 14 u16, NOT the
/// upstream `ScanDataFlags::avset_enabled()` accessor — that one checks
/// bit 0 instead of bit 1, which is a bug vs xradar's (correct) ICD-Note-13
/// bit positions. We need xradar parity, so we redo the bit math.
fn decode_rda_scan_data_flags(raw: u16) -> (bool, bool) {
    // bit 1 (value 0x0002) = AVSET enabled per ICD; bit 3 (0x0008) = EBC enabled.
    let avset_enabled = raw & 0x0002 != 0;
    let ebc_enabled = raw & 0x0008 != 0;
    (avset_enabled, ebc_enabled)
}

/// Build the volume-level attrs from MSG_5 (always present; we already use it
/// for moments) plus the optional MSG_2. Missing MSG_2 yields zero/false
/// values for the 5 RDA-status fields — xradar's
/// `dict.get(name, default)` does the same.
///
/// `sweeps` provides the per-sweep data needed to populate
/// [`NexradVolumeAttrs::sweep_attrs`] (one [`NexradSweepAttrs`] per sweep,
/// from the corresponding `ElevationCut`) and
/// [`NexradVolumeAttrs::sweep_time_ranges`] (each sweep's
/// `(time_start, time_end)` from its first/last ray's
/// `collection_time`). Both are populated whether the caller is doing a
/// metadata-only `scan_file` or a full `read_volume` — neither needs
/// per-ray moment decode.
pub(super) fn volume_attrs(
    vcp: &Msg5,
    msg2: Option<&Msg2>,
    sweeps: &[Sweep],
    actual_elevation_cuts: u32,
) -> NexradVolumeAttrs {
    let (avset_enabled, ebc_enabled, super_res_status, rda_build_number, operational_mode) =
        match msg2 {
            Some(m) => {
                let (avset, ebc) = decode_rda_scan_data_flags(m.rda_scan_and_data_flags);
                (
                    avset,
                    ebc,
                    m.super_resolution_status,
                    m.rda_build_number,
                    m.operational_mode,
                )
            }
            None => (false, false, 0, 0, 0),
        };

    // Per-sweep MSG_5 attrs. Index-aligned with the volume's sweep list:
    // entry `i` corresponds to `sweeps[i]`. When the cut table is shorter
    // than the sweep count (e.g. truncated VCP), pad with default
    // `NexradSweepAttrs` so callers can still index by sweep number — see
    // [`NexradVolumeAttrs::sweep_attrs`] for the consumer-side contract on
    // distinguishing "default-padded" from "definitively false."
    let cuts = vcp.elevation_cuts();
    let sweep_attrs: Vec<NexradSweepAttrs> = (0..sweeps.len())
        .map(|idx| cuts.get(idx).map(sweep_attrs_from_cut).unwrap_or_default())
        .collect();

    // Per-sweep `(time_start, time_end)` ranges as Unix seconds since
    // 1970-01-01 UTC, matching the `Coordinates::time` axis convention.
    // `Sweep::time_range()` is a min/max walk over already-decoded
    // radials — sub-millisecond for a typical volume.
    let sweep_time_ranges: Vec<Option<(f64, f64)>> = sweeps
        .iter()
        .map(|s| {
            s.time_range().map(|(start, end)| {
                (
                    start.timestamp_micros() as f64 / 1.0e6,
                    end.timestamp_micros() as f64 / 1.0e6,
                )
            })
        })
        .collect();

    NexradVolumeAttrs {
        dynamic_scan_type: dynamic_scan_type(vcp),
        mpda_vcp: vcp.mpda_enabled(),
        base_tilt_vcp: vcp.base_tilt_enabled(),
        num_base_tilts: vcp.base_tilt_count(),
        vcp_truncated: vcp.truncated(),
        vcp_sequence_active: vcp.sequence_active(),
        number_elevation_cuts: u32::from(vcp.number_of_elevation_cuts_u8()),
        doppler_velocity_resolution: vcp.doppler_velocity_resolution_m_per_s(),
        vcp_pulse_width: vcp.pulse_width_str().to_string(),
        avset_enabled,
        ebc_enabled,
        super_res_status,
        rda_build_number,
        operational_mode,
        actual_elevation_cuts,
        sweep_attrs,
        sweep_time_ranges,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;
    use crate::backends::nexrad::decode::messages::msg31::moment::MomentDescriptor;
    use crate::backends::nexrad::decode::model::{OwnedMoment, Radial};

    #[test]
    fn waveform_type_from_raw_matches_xradar_table() {
        // Pinning xradar's `_WAVEFORM_TYPES` map verbatim. xradar's table is
        // off-by-one vs the ICD starting at code 3 — that's xradar's bug,
        // not ours, and we match it on purpose for drop-in attr parity.
        assert_eq!(waveform_type_str_from_raw(0), "not_applicable");
        assert_eq!(waveform_type_str_from_raw(1), "contiguous_surveillance");
        assert_eq!(waveform_type_str_from_raw(2), "contiguous_doppler");
        assert_eq!(waveform_type_str_from_raw(3), "batch");
        assert_eq!(waveform_type_str_from_raw(4), "staggered_pulse_pair");
        // xradar falls through: `_WAVEFORM_TYPES.get(5, str(5)) == "5"`.
        assert_eq!(waveform_type_str_from_raw(5), "5");
        assert_eq!(waveform_type_str_from_raw(99), "99");
    }

    #[test]
    fn channel_config_from_raw_matches_xradar_table() {
        assert_eq!(channel_config_str_from_raw(0), "constant_phase");
        assert_eq!(channel_config_str_from_raw(1), "random_phase");
        assert_eq!(channel_config_str_from_raw(2), "sz2_phase_coding");
        // xradar falls through to `str(3) == "3"` for unknown codes.
        assert_eq!(channel_config_str_from_raw(3), "3");
    }

    #[test]
    fn sweep_attrs_emit_xradar_strings_for_batch_and_staggered() {
        // The bug motivating xradar parity: KLOT volumes have a sweep
        // with raw waveform_type=4 (ICD "Batch") for which xradar emits
        // `"staggered_pulse_pair"` — not `"batch"`. We match xradar.
        let cut_b = zero_cut_with(|c| {
            c.waveform_type = 4;
            c.channel_configuration = 0;
        });
        let attrs = sweep_attrs_from_cut(&cut_b);
        assert_eq!(attrs.waveform_type, "staggered_pulse_pair");

        // raw=3 ("Contiguous Doppler without ambiguity resolution" per
        // ICD) → xradar emits "batch". Channel config 2 = SZ2Phase.
        let cut_cdwo = zero_cut_with(|c| {
            c.waveform_type = 3;
            c.channel_configuration = 2;
        });
        let attrs = sweep_attrs_from_cut(&cut_cdwo);
        assert_eq!(attrs.waveform_type, "batch");
        assert_eq!(attrs.channel_config, "sz2_phase_coding");
    }

    #[test]
    fn dynamic_scan_type_sails_with_count() {
        // SAILS+1 → "SAILS x 1"; SAILS+0 → bare "SAILS"; not-SAILS → standard.
        let cuts = vec![sample_cut()];
        let vcp = vcp_with_supplemental(supplemental_bits(true, 1, false, 0), cuts.clone());
        assert_eq!(dynamic_scan_type(&vcp), "SAILS x 1");

        let vcp = vcp_with_supplemental(supplemental_bits(true, 0, false, 0), cuts.clone());
        assert_eq!(dynamic_scan_type(&vcp), "SAILS");

        let vcp = vcp_with_supplemental(supplemental_bits(false, 0, true, 2), cuts.clone());
        assert_eq!(dynamic_scan_type(&vcp), "MRLE x 2");

        let vcp = vcp_with_supplemental(supplemental_bits(false, 0, false, 0), cuts);
        assert_eq!(dynamic_scan_type(&vcp), "standard");
    }

    #[test]
    fn pack_super_resolution_round_trip() {
        // Half-deg azimuth (bit 0) + dual-pol 300 km (bit 3) → 0b1001.
        let cut = zero_cut_with(|c| c.super_resolution_control = 0b1001);
        assert_eq!(pack_super_resolution(&cut), 0b1001);
    }

    #[test]
    fn rda_scan_data_flags_xradar_bits() {
        // Pinning xradar's `decode_rda_scan_data_flags` verbatim: AVSET at bit
        // 1 (0x0002), EBC at bit 3 (0x0008). The upstream Rust accessor
        // misnumbers these, so we don't use it.
        assert_eq!(decode_rda_scan_data_flags(0x0000), (false, false));
        assert_eq!(decode_rda_scan_data_flags(0x0002), (true, false));
        assert_eq!(decode_rda_scan_data_flags(0x0008), (false, true));
        assert_eq!(decode_rda_scan_data_flags(0x000A), (true, true));
        // Bit 0 (0x0001) is spare per ICD; should NOT toggle AVSET.
        assert_eq!(decode_rda_scan_data_flags(0x0001), (false, false));
    }

    #[test]
    fn volume_attrs_without_msg2_zeroes_rda_fields() {
        let vcp = vcp_with_supplemental(0, vec![sample_cut()]);
        let attrs = volume_attrs(&vcp, None, &[], 5);
        assert_eq!(attrs.dynamic_scan_type, "standard");
        assert!(!attrs.avset_enabled);
        assert!(!attrs.ebc_enabled);
        assert_eq!(attrs.super_res_status, 0);
        assert_eq!(attrs.rda_build_number, 0);
        assert_eq!(attrs.operational_mode, 0);
        assert_eq!(attrs.actual_elevation_cuts, 5);
        assert_eq!(attrs.number_elevation_cuts, 1);
        // Empty `sweeps` slice → empty per-sweep arrays even when the
        // VCP carries cuts. Pins "we size by sweeps, not by cuts."
        assert!(attrs.sweep_attrs.is_empty());
        assert!(attrs.sweep_time_ranges.is_empty());
    }

    /// HIGH-priority: pin that `volume_attrs` populates one
    /// `NexradSweepAttrs` per sweep, sourced from the corresponding
    /// `ElevationCut`. A regression that drops the sweeps loop
    /// (or sources from `sweeps[0]` for every entry) is caught here.
    #[test]
    fn volume_attrs_populates_one_sweep_attr_per_sweep_from_matching_cut() {
        // Three cuts with distinct, observable signatures via the
        // SAILS bit so each entry is uniquely identifiable.
        let cut_a = cut_with_sails(false);
        let cut_b = cut_with_sails(true);
        let cut_c = cut_with_sails(false);
        let vcp = vcp_with_supplemental(
            supplemental_bits(true, 1, false, 0),
            vec![cut_a, cut_b, cut_c],
        );
        let sweeps = vec![empty_sweep(1), empty_sweep(2), empty_sweep(3)];

        let attrs = volume_attrs(&vcp, None, &sweeps, 3);

        assert_eq!(attrs.sweep_attrs.len(), 3, "one entry per sweep");
        assert!(!attrs.sweep_attrs[0].sails_cut);
        assert!(attrs.sweep_attrs[1].sails_cut, "cut_b has sails_cut=true");
        assert!(!attrs.sweep_attrs[2].sails_cut);
    }

    /// HIGH-priority: pin the truncated-VCP padding contract. When
    /// `cuts.len() < sweeps.len()`, trailing entries fall back to
    /// `NexradSweepAttrs::default()` rather than panicking or
    /// truncating the output.
    #[test]
    fn volume_attrs_pads_with_default_when_cuts_shorter_than_sweeps() {
        let vcp = vcp_with_supplemental(0, vec![sample_cut()]);
        let sweeps = vec![
            empty_sweep(1),
            empty_sweep(2),
            empty_sweep(3),
            empty_sweep(4),
        ];

        let attrs = volume_attrs(&vcp, None, &sweeps, 4);

        assert_eq!(attrs.sweep_attrs.len(), 4);
        // Entry 0 is sourced from the real cut (waveform_type non-empty).
        assert_eq!(
            attrs.sweep_attrs[0].waveform_type,
            "contiguous_surveillance"
        );
        // Entries 1..4 are defaults — empty waveform_type is the
        // sentinel that tells consumers "we don't know."
        for sa in &attrs.sweep_attrs[1..] {
            assert_eq!(sa, &NexradSweepAttrs::default());
        }
    }

    /// HIGH-priority: pin both branches of `Sweep::time_range`.
    /// `None` for empty sweeps (no radials), `Some((start, end))` with
    /// `start <= end` for sweeps with timestamped radials.
    #[test]
    fn volume_attrs_sweep_time_ranges_some_and_none_branches() {
        let vcp = vcp_with_supplemental(0, vec![sample_cut(), sample_cut()]);
        let sweeps = vec![
            empty_sweep(1), // → None
            sweep_with_timestamps(
                2,
                &[
                    1_700_000_000_000_i64, // 2023-11-14T22:13:20Z
                    1_700_000_005_500_i64, // +5.5 s
                    1_700_000_002_000_i64,
                ],
            ),
        ];

        let attrs = volume_attrs(&vcp, None, &sweeps, 2);

        assert_eq!(attrs.sweep_time_ranges.len(), 2);
        assert!(attrs.sweep_time_ranges[0].is_none(), "no radials → None");

        let (start, end) = attrs.sweep_time_ranges[1].expect("Some");
        assert!((start - 1_700_000_000.0).abs() < 1e-3, "start = {start}");
        assert!((end - 1_700_000_005.5).abs() < 1e-3, "end = {end}");
        assert!(start <= end, "time_range invariant: start <= end");
    }

    /// Pack the four MSG_5 supplemental-data bits into the HW 10
    /// layout per ICD Note 16: bit 0 = SAILS enabled, bits 1-3 =
    /// SAILS cut count, bit 4 = MRLE enabled, bits 5-7 = MRLE cut count.
    fn supplemental_bits(sails: bool, sails_cuts: u8, mrle: bool, mrle_cuts: u8) -> u16 {
        let mut bits = 0u16;
        if sails {
            bits |= 0b0001;
        }
        bits |= u16::from(sails_cuts & 0b0111) << 1;
        if mrle {
            bits |= 0b0001_0000;
        }
        bits |= u16::from(mrle_cuts & 0b0111) << 5;
        bits
    }

    /// Construct an all-zero `ElevationCut` with the given mutator
    /// applied. Keeps test fixtures focused on the field the test
    /// actually exercises.
    fn zero_cut_with(mutate: impl FnOnce(&mut ElevationCut)) -> ElevationCut {
        let mut cut = ElevationCut {
            elevation_angle_raw: 0,
            channel_configuration: 0,
            waveform_type: 1,                 // CS
            super_resolution_control: 0b0011, // half-deg azimuth + 1/4-km reflectivity
            surveillance_prf_number: 1,
            surveillance_pulse_count: 17,
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
        mutate(&mut cut);
        cut
    }

    fn sample_cut() -> ElevationCut {
        zero_cut_with(|_| {})
    }

    /// `sails=true` → SAILS cut bit set + sequence number 1
    /// (matches the original test fixture's signal).
    fn cut_with_sails(sails: bool) -> ElevationCut {
        zero_cut_with(|c| {
            if sails {
                c.supplemental_data = 0b0001 | (1 << 1); // SAILS + sequence=1
            }
        })
    }

    fn vcp_with_supplemental(vcp_supplemental: u16, cuts: Vec<ElevationCut>) -> Msg5 {
        let n_cuts = cuts.len() as u16;
        Msg5 {
            message_size_halfwords: 0,
            pattern_type: 1,
            pattern_number: 212,
            number_of_elevation_cuts: n_cuts,
            vcp_version: 1,
            clutter_map_group_number: 1,
            doppler_velocity_resolution: 2, // 0.5 m/s
            pulse_width: 2,                 // short
            vcp_sequencing: 0,
            vcp_supplemental,
            elevation_cuts: cuts,
        }
    }

    fn empty_sweep(elevation_number: u8) -> Sweep {
        Sweep {
            elevation_number,
            radials: Vec::new(),
            complete: true,
        }
    }

    /// Build a sweep whose radials carry the given collection
    /// timestamps (milliseconds since Unix epoch). Other radial
    /// fields are arbitrary defaults — only timestamps drive
    /// [`Sweep::time_range`].
    fn sweep_with_timestamps(elevation_number: u8, timestamps_ms: &[i64]) -> Sweep {
        let radials: Vec<Radial> = timestamps_ms
            .iter()
            .enumerate()
            .map(|(i, ts)| Radial {
                azimuth_number: i as u16,
                azimuth_angle_degrees: i as f32,
                azimuth_resolution_spacing: 0,
                elevation_number,
                elevation_angle_degrees: 0.5,
                radial_status: 0,
                collection_time: DateTime::<Utc>::from_timestamp_millis(*ts),
                reflectivity: Some(OwnedMoment {
                    descriptor: MomentDescriptor {
                        gate_count: 1,
                        range_to_first_gate_km: 2.0,
                        gate_interval_km: 0.25,
                        tover_db: 0.0,
                        snr_threshold_db: 0.0,
                        control_flags: 0,
                        data_word_size_bits: 8,
                        scale: 2.0,
                        offset: 66.0,
                    },
                    gate_bytes: vec![10],
                }),
                velocity: None,
                spectrum_width: None,
                differential_reflectivity: None,
                differential_phase: None,
                correlation_coefficient: None,
                clutter_filter_power: None,
            })
            .collect();
        Sweep {
            elevation_number,
            radials,
            complete: true,
        }
    }
}
