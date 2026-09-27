//! MSG_2 (RDA Status Data) parser — ICD §3.2.4.6 Table IV.
//!
//! 60-halfword fixed-frame message (= 120 bytes after the 16-byte
//! Table II logical header). Mostly bit-packed status flags + a
//! few scalar fields.
//!
//! Halfword layout (all `u16` big-endian on the wire):
//!
//! | HW | Field                                                    |
//! |---:|----------------------------------------------------------|
//! |  1 | RDA Status (bit-packed: Start-Up / Standby / Restart...) |
//! |  2 | Operability Status                                       |
//! |  3 | Control Status                                           |
//! |  4 | Auxiliary Power Generator State                          |
//! |  5 | Average Transmitter Power (W)                            |
//! |  6 | Horizontal Reflectivity Calibration Correction           |
//! |  7 | Data Transmission Enabled                                |
//! |  8 | Volume Coverage Pattern Number (signed magnitude)        |
//! |  9 | RDA Control Authorization                                |
//! | 10 | RDA Build Number (scaled int, e.g. `1900` = build 19.0)  |
//! | 11 | Operational Mode                                         |
//! | 12 | Super Resolution Status                                  |
//! | 13 | Clutter Mitigation Decision Status                       |
//! | 14 | RDA Scan and Data Flags (AVSET, EBC, RDA Log, TimeSeries)|
//! | 15 | RDA Alarm Summary                                        |
//! | 16 | Command Acknowledgement                                  |
//! | 17 | Channel Control Status                                   |
//! | 18 | Spot Blanking Status                                     |
//! | 19 | Bypass Map Generation Date                               |
//! | 20 | Bypass Map Generation Time                               |
//! | 21 | Clutter Filter Map Generation Date                       |
//! | 22 | Clutter Filter Map Generation Time                       |
//! | 23 | Vertical Reflectivity Calibration Correction             |
//! | 24 | Transition Power Source Status                           |
//! | 25 | RMS Control Status                                       |
//! | 26 | Performance Check Status                                 |
//! |27-40| Alarm Codes (14 halfwords)                              |
//! | 41 | Signal Processing Options                                |
//! |42-58| Spares (17 halfwords)                                   |
//! | 59 | Downloaded VCP Pattern Number                            |
//! | 60 | Status Version                                           |

use crate::backends::nexrad::decode::error::Result;
use crate::backends::nexrad::decode::reader::SliceReader;

/// Decoded MSG_2 RDA Status. Field names mirror ICD Table IV
/// labels (lowered + snake_cased) so parity tests can map 1:1
/// against danielway's `nexrad-decode::messages::rda_status_data`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Msg2 {
    pub(crate) rda_status: u16,
    pub(crate) operability_status: u16,
    pub(crate) control_status: u16,
    pub(crate) auxiliary_power_generator_state: u16,
    pub(crate) average_transmitter_power_w: u16,
    pub(crate) horizontal_reflectivity_calibration_correction: i16,
    pub(crate) data_transmission_enabled: u16,
    pub(crate) volume_coverage_pattern_number: i16,
    pub(crate) rda_control_authorization: u16,
    /// RDA major.minor build encoded as a scaled integer
    /// (`1900` = 19.0, `2400` = 24.0, etc.).
    pub(crate) rda_build_number: u16,
    pub(crate) operational_mode: u16,
    pub(crate) super_resolution_status: u16,
    pub(crate) clutter_mitigation_decision_status: u16,
    /// AVSET / EBC / RDA Log / Time Series flag bits per ICD HW 14.
    pub(crate) rda_scan_and_data_flags: u16,
    pub(crate) rda_alarm_summary: u16,
    pub(crate) command_acknowledgement: u16,
    pub(crate) channel_control_status: u16,
    pub(crate) spot_blanking_status: u16,
    pub(crate) bypass_map_generation_date: u16,
    pub(crate) bypass_map_generation_time: u16,
    pub(crate) clutter_filter_map_generation_date: u16,
    pub(crate) clutter_filter_map_generation_time: u16,
    pub(crate) vertical_reflectivity_calibration_correction: i16,
    pub(crate) transition_power_source_status: u16,
    pub(crate) rms_control_status: u16,
    pub(crate) performance_check_status: u16,
    /// 14 halfwords starting at HW 27. MSB set = alarm cleared.
    pub(crate) alarm_codes: [u16; 14],
    pub(crate) signal_processing_options: u16,
    /// HW 42-58 reserved (17 halfwords). Preserved verbatim.
    pub(crate) spares: [u16; 17],
    pub(crate) downloaded_pattern_number: u16,
    pub(crate) status_version: u16,
}

/// Wire size of MSG_2's logical content: 60 halfwords × 2 = 120 bytes.
pub(crate) const MSG2_BYTES: usize = 120;

impl Msg2 {
    /// Parse exactly `MSG2_BYTES` from `reader`. Caller must have
    /// already taken the 16-byte Table II header off the front.
    pub(crate) fn read(reader: &mut SliceReader<'_>) -> Result<Self> {
        let rda_status = reader.read_u16_be()?;
        let operability_status = reader.read_u16_be()?;
        let control_status = reader.read_u16_be()?;
        let auxiliary_power_generator_state = reader.read_u16_be()?;
        let average_transmitter_power_w = reader.read_u16_be()?;
        let horizontal_reflectivity_calibration_correction = reader.read_i16_be()?;
        let data_transmission_enabled = reader.read_u16_be()?;
        let volume_coverage_pattern_number = reader.read_i16_be()?;
        let rda_control_authorization = reader.read_u16_be()?;
        let rda_build_number = reader.read_u16_be()?;
        let operational_mode = reader.read_u16_be()?;
        let super_resolution_status = reader.read_u16_be()?;
        let clutter_mitigation_decision_status = reader.read_u16_be()?;
        let rda_scan_and_data_flags = reader.read_u16_be()?;
        let rda_alarm_summary = reader.read_u16_be()?;
        let command_acknowledgement = reader.read_u16_be()?;
        let channel_control_status = reader.read_u16_be()?;
        let spot_blanking_status = reader.read_u16_be()?;
        let bypass_map_generation_date = reader.read_u16_be()?;
        let bypass_map_generation_time = reader.read_u16_be()?;
        let clutter_filter_map_generation_date = reader.read_u16_be()?;
        let clutter_filter_map_generation_time = reader.read_u16_be()?;
        let vertical_reflectivity_calibration_correction = reader.read_i16_be()?;
        let transition_power_source_status = reader.read_u16_be()?;
        let rms_control_status = reader.read_u16_be()?;
        let performance_check_status = reader.read_u16_be()?;

        let mut alarm_codes = [0u16; 14];
        for slot in alarm_codes.iter_mut() {
            *slot = reader.read_u16_be()?;
        }

        let signal_processing_options = reader.read_u16_be()?;

        let mut spares = [0u16; 17];
        for slot in spares.iter_mut() {
            *slot = reader.read_u16_be()?;
        }

        let downloaded_pattern_number = reader.read_u16_be()?;
        let status_version = reader.read_u16_be()?;

        Ok(Self {
            rda_status,
            operability_status,
            control_status,
            auxiliary_power_generator_state,
            average_transmitter_power_w,
            horizontal_reflectivity_calibration_correction,
            data_transmission_enabled,
            volume_coverage_pattern_number,
            rda_control_authorization,
            rda_build_number,
            operational_mode,
            super_resolution_status,
            clutter_mitigation_decision_status,
            rda_scan_and_data_flags,
            rda_alarm_summary,
            command_acknowledgement,
            channel_control_status,
            spot_blanking_status,
            bypass_map_generation_date,
            bypass_map_generation_time,
            clutter_filter_map_generation_date,
            clutter_filter_map_generation_time,
            vertical_reflectivity_calibration_correction,
            transition_power_source_status,
            rms_control_status,
            performance_check_status,
            alarm_codes,
            signal_processing_options,
            spares,
            downloaded_pattern_number,
            status_version,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 120-byte synthetic MSG_2 with distinct values per field so a
    /// transposition error is caught.
    fn synth_msg2() -> Vec<u8> {
        let mut buf = Vec::with_capacity(MSG2_BYTES);
        // HW 1..26 — 26 halfwords × 2 = 52 bytes.
        let scalars: [u16; 26] = [
            16,     // 1 rda_status: Operate (bit 4)
            2,      // 2 operability_status: On-line
            8,      // 3 control_status: Either
            1,      // 4 aux_power_state
            500,    // 5 avg_tx_power_w
            150u16, // 6 horiz_refl_calib_corr (raw scaled integer)
            14,     // 7 data_tx_enabled (REF + VEL + SW = 2|4|8)
            212,    // 8 vcp_number (positive = remote)
            0,      // 9 control_auth: No Action
            1900,   // 10 rda_build_number (= 19.0)
            4,      // 11 operational_mode
            2,      // 12 super_resolution_status: Enabled
            1,      // 13 clutter_mitigation
            10,     // 14 rda_scan_and_data_flags: AVSET enabled (bit 1) | EBC (bit 3)
            0,      // 15 alarm_summary
            0,      // 16 cmd_ack
            0,      // 17 channel_ctrl_status: controlling
            0,      // 18 spot_blanking
            20_000, // 19 bypass_map_date
            500,    // 20 bypass_map_time
            20_001, // 21 cfm_date
            300,    // 22 cfm_time
            120u16, // 23 vert_refl_calib_corr
            3,      // 24 tps_status: OK
            2,      // 25 rms_control: RMS in control
            0,      // 26 perf_check: no command pending
        ];
        for v in scalars {
            buf.extend_from_slice(&v.to_be_bytes());
        }
        // HW 27..40 — 14 alarm codes, all zero.
        for _ in 0..14 {
            buf.extend_from_slice(&0u16.to_be_bytes());
        }
        // HW 41 — signal processing options.
        buf.extend_from_slice(&1u16.to_be_bytes()); // CMD rho-hv test enabled
                                                    // HW 42..58 — 17 spares, all zero.
        for _ in 0..17 {
            buf.extend_from_slice(&0u16.to_be_bytes());
        }
        // HW 59 — downloaded pattern.
        buf.extend_from_slice(&212u16.to_be_bytes());
        // HW 60 — status version.
        buf.extend_from_slice(&3u16.to_be_bytes());
        debug_assert_eq!(buf.len(), MSG2_BYTES);
        buf
    }

    #[test]
    fn read_consumes_exactly_120_bytes() {
        let bytes = synth_msg2();
        let mut r = SliceReader::new(&bytes);
        let _ = Msg2::read(&mut r).unwrap();
        assert_eq!(r.position(), MSG2_BYTES);
    }

    #[test]
    fn round_trip_preserves_every_field() {
        let bytes = synth_msg2();
        let mut r = SliceReader::new(&bytes);
        let m = Msg2::read(&mut r).unwrap();
        // Cover every field that comes off the wire — a swap
        // between two adjacent halfwords gets caught immediately.
        assert_eq!(m.rda_status, 16);
        assert_eq!(m.operability_status, 2);
        assert_eq!(m.control_status, 8);
        assert_eq!(m.auxiliary_power_generator_state, 1);
        assert_eq!(m.average_transmitter_power_w, 500);
        assert_eq!(m.horizontal_reflectivity_calibration_correction, 150);
        assert_eq!(m.data_transmission_enabled, 14);
        assert_eq!(m.volume_coverage_pattern_number, 212);
        assert_eq!(m.rda_control_authorization, 0);
        assert_eq!(m.rda_build_number, 1900);
        assert_eq!(m.operational_mode, 4);
        assert_eq!(m.super_resolution_status, 2);
        assert_eq!(m.clutter_mitigation_decision_status, 1);
        assert_eq!(m.rda_scan_and_data_flags, 10);
        assert_eq!(m.rda_alarm_summary, 0);
        assert_eq!(m.command_acknowledgement, 0);
        assert_eq!(m.channel_control_status, 0);
        assert_eq!(m.spot_blanking_status, 0);
        assert_eq!(m.bypass_map_generation_date, 20_000);
        assert_eq!(m.bypass_map_generation_time, 500);
        assert_eq!(m.clutter_filter_map_generation_date, 20_001);
        assert_eq!(m.clutter_filter_map_generation_time, 300);
        assert_eq!(m.vertical_reflectivity_calibration_correction, 120);
        assert_eq!(m.transition_power_source_status, 3);
        assert_eq!(m.rms_control_status, 2);
        assert_eq!(m.performance_check_status, 0);
        assert!(m.alarm_codes.iter().all(|&c| c == 0));
        assert_eq!(m.signal_processing_options, 1);
        assert!(m.spares.iter().all(|&s| s == 0));
        assert_eq!(m.downloaded_pattern_number, 212);
        assert_eq!(m.status_version, 3);
    }

    #[test]
    fn read_errors_on_short_input() {
        let bytes = synth_msg2();
        let mut r = SliceReader::new(&bytes[..60]);
        assert!(Msg2::read(&mut r).is_err());
    }

    #[test]
    fn negative_signed_calibration_corrections_round_trip() {
        // ICD: ±198.00 dB range, raw is signed scaled integer
        // (LSB = 0.01 dB). -100 raw → -1.0 dB.
        let mut bytes = synth_msg2();
        // HW 6 (horiz_refl_calib_corr) → bytes 10..12.
        bytes[10..12].copy_from_slice(&(-100_i16).to_be_bytes());
        // HW 23 (vert_refl_calib_corr) → bytes 44..46.
        bytes[44..46].copy_from_slice(&(-50_i16).to_be_bytes());
        let mut r = SliceReader::new(&bytes);
        let m = Msg2::read(&mut r).unwrap();
        assert_eq!(m.horizontal_reflectivity_calibration_correction, -100);
        assert_eq!(m.vertical_reflectivity_calibration_correction, -50);
    }

    #[test]
    fn negative_vcp_number_signals_local_pattern() {
        // ICD HW 8 sign convention: negative = RDA Local Pattern
        // Selected; positive = RDA Remote Pattern Selected.
        let mut bytes = synth_msg2();
        // HW 8 → bytes 14..16.
        bytes[14..16].copy_from_slice(&(-32_i16).to_be_bytes());
        let mut r = SliceReader::new(&bytes);
        let m = Msg2::read(&mut r).unwrap();
        assert_eq!(m.volume_coverage_pattern_number, -32);
    }
}
