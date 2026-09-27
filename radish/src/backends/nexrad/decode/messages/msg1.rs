//! MSG_1 (Digital Radar Data, legacy) parser — ICD §3.2.4.2 Table III.
//!
//! Pre-Build-12 (March 2012) NEXRAD files used MSG_1 instead of
//! MSG_31. Each radial is a 2416-byte logical message body =
//! 100-byte fixed header + up to three 1-byte-per-gate moment
//! arrays (reflectivity, velocity, spectrum width). Gate locations
//! are pointed at by HW19-21, measured in bytes from the start of
//! the message body.
//!
//! Halfword layout (all `i16` / `i32` / `f32` big-endian on wire;
//! field names mirror danielway/nexrad's
//! `digital_radar_data_legacy::raw::Header` so parity is trivial):
//!
//! | HW    | Bytes | Field                                  |
//! |------:|-------|----------------------------------------|
//! |  1-2  |  0-3  | collection_time (ms past midnight)     |
//! |  3    |  4-5  | modified_julian_date (days)            |
//! |  4    |  6-7  | unambiguous_range (× 100 m)            |
//! |  5    |  8-9  | azimuth_angle (binary fraction)        |
//! |  6    | 10-11 | azimuth_number                         |
//! |  7    | 12-13 | radial_status                          |
//! |  8    | 14-15 | elevation_angle (binary fraction)      |
//! |  9    | 16-17 | elevation_number                       |
//! | 10    | 18-19 | surveillance_first_gate_range (m)      |
//! | 11    | 20-21 | doppler_first_gate_range (m)           |
//! | 12    | 22-23 | surveillance_gate_interval (m)         |
//! | 13    | 24-25 | doppler_gate_interval (m)              |
//! | 14    | 26-27 | num_surveillance_gates                 |
//! | 15    | 28-29 | num_doppler_gates                      |
//! | 16    | 30-31 | sector_number                          |
//! | 17-18 | 32-35 | calibration_constant (dB, f32)         |
//! | 19    | 36-37 | reflectivity_pointer (bytes)           |
//! | 20    | 38-39 | velocity_pointer (bytes)               |
//! | 21    | 40-41 | spectrum_width_pointer (bytes)         |
//! | 22    | 42-43 | doppler_velocity_resolution            |
//! | 23    | 44-45 | vcp_number                             |
//! | 24-50 | 46-99 | spare (54 bytes)                       |
//!
//! Gate value encoding (same for all three moments):
//! `0` = below threshold, `1` = range folded, `2..=255` = scaled.
//! Reflectivity: `dBZ = (raw - 2)/2 - 32`.
//! Velocity: `m/s = (raw - 2)*0.5 - 63.5` (resolution 2) or
//! `(raw - 2)*1.0 - 127.0` (resolution 4).
//! Spectrum width: `m/s = (raw - 2)*0.5 - 63.5`.

use chrono::{DateTime, TimeZone, Utc};

use crate::backends::nexrad::decode::error::{NexradDecodeError, Result};
use crate::backends::nexrad::decode::reader::SliceReader;

/// Wire size of MSG_1's fixed header (100 bytes).
pub(crate) const MSG1_HEADER_BYTES: usize = 100;

/// Decoded MSG_1 message: 100-byte header + up to three owned gate
/// arrays. Owns its gate buffers because raw Archive II files have
/// no decompressed payload to borrow from — frames are read directly
/// into a worker-local `Vec` and dropped at end of scope.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Msg1 {
    pub(crate) collection_time_ms: i32,
    pub(crate) modified_julian_date: i16,
    pub(crate) unambiguous_range_x100m: i16,
    pub(crate) azimuth_angle_raw: i16,
    pub(crate) azimuth_number: i16,
    pub(crate) radial_status: i16,
    pub(crate) elevation_angle_raw: i16,
    pub(crate) elevation_number: i16,
    pub(crate) surveillance_first_gate_range_m: i16,
    pub(crate) doppler_first_gate_range_m: i16,
    pub(crate) surveillance_gate_interval_m: i16,
    pub(crate) doppler_gate_interval_m: i16,
    pub(crate) num_surveillance_gates: i16,
    pub(crate) num_doppler_gates: i16,
    pub(crate) sector_number: i16,
    pub(crate) calibration_constant_db: f32,
    pub(crate) reflectivity_pointer: i16,
    pub(crate) velocity_pointer: i16,
    pub(crate) spectrum_width_pointer: i16,
    pub(crate) doppler_velocity_resolution: i16,
    pub(crate) vcp_number: i16,
    pub(crate) reflectivity_gates: Option<Vec<u8>>,
    pub(crate) velocity_gates: Option<Vec<u8>>,
    pub(crate) spectrum_width_gates: Option<Vec<u8>>,
}

impl Msg1 {
    /// Parse a MSG_1 message body from `reader`. Caller must have
    /// already taken the 16-byte Table II header off the front, so
    /// `reader` is positioned at byte 0 of the message body.
    pub(crate) fn read(reader: &mut SliceReader<'_>) -> Result<Self> {
        let body_start = reader.position();
        let collection_time_ms = reader.read_i32_be()?;
        let modified_julian_date = reader.read_i16_be()?;
        let unambiguous_range_x100m = reader.read_i16_be()?;
        let azimuth_angle_raw = reader.read_i16_be()?;
        let azimuth_number = reader.read_i16_be()?;
        let radial_status = reader.read_i16_be()?;
        let elevation_angle_raw = reader.read_i16_be()?;
        let elevation_number = reader.read_i16_be()?;
        let surveillance_first_gate_range_m = reader.read_i16_be()?;
        let doppler_first_gate_range_m = reader.read_i16_be()?;
        let surveillance_gate_interval_m = reader.read_i16_be()?;
        let doppler_gate_interval_m = reader.read_i16_be()?;
        let num_surveillance_gates = reader.read_i16_be()?;
        let num_doppler_gates = reader.read_i16_be()?;
        let sector_number = reader.read_i16_be()?;
        let calibration_constant_db = reader.read_f32_be()?;
        let reflectivity_pointer = reader.read_i16_be()?;
        let velocity_pointer = reader.read_i16_be()?;
        let spectrum_width_pointer = reader.read_i16_be()?;
        let doppler_velocity_resolution = reader.read_i16_be()?;
        let vcp_number = reader.read_i16_be()?;
        // Skip 54 bytes of spare (HW24..50).
        reader.advance(54)?;

        // Pointers are byte offsets from the start of the message
        // body; `reader.position()` is the absolute cursor in the
        // input buffer. Translate by `body_start`.
        let take_gates =
            |reader: &mut SliceReader<'_>, ptr: i16, count: i16| -> Result<Option<Vec<u8>>> {
                if ptr <= 0 || count <= 0 {
                    return Ok(None);
                }
                let target = body_start.checked_add(ptr as usize).ok_or(
                    NexradDecodeError::MalformedHeader {
                        offset: body_start,
                        reason: "MSG_1 moment pointer overflow",
                    },
                )?;
                // The pointer must be at or past the current cursor (we
                // don't seek backward) and within the buffer.
                if target < reader.position() {
                    return Err(NexradDecodeError::MalformedHeader {
                        offset: body_start,
                        reason: "MSG_1 moment pointer points before current cursor",
                    });
                }
                reader.try_skip_to(target)?;
                let bytes = reader.take_bytes(count as usize)?;
                Ok(Some(bytes.to_vec()))
            };

        let reflectivity_gates = take_gates(reader, reflectivity_pointer, num_surveillance_gates)?;
        let velocity_gates = take_gates(reader, velocity_pointer, num_doppler_gates)?;
        let spectrum_width_gates = take_gates(reader, spectrum_width_pointer, num_doppler_gates)?;

        Ok(Self {
            collection_time_ms,
            modified_julian_date,
            unambiguous_range_x100m,
            azimuth_angle_raw,
            azimuth_number,
            radial_status,
            elevation_angle_raw,
            elevation_number,
            surveillance_first_gate_range_m,
            doppler_first_gate_range_m,
            surveillance_gate_interval_m,
            doppler_gate_interval_m,
            num_surveillance_gates,
            num_doppler_gates,
            sector_number,
            calibration_constant_db,
            reflectivity_pointer,
            velocity_pointer,
            spectrum_width_pointer,
            doppler_velocity_resolution,
            vcp_number,
            reflectivity_gates,
            velocity_gates,
            spectrum_width_gates,
        })
    }

    /// Azimuth angle in degrees, decoded from the binary-fraction
    /// encoding (ICD Table III-A: `degrees = raw * 180/32768`).
    pub(crate) fn azimuth_angle_degrees(&self) -> f32 {
        f32::from(self.azimuth_angle_raw) * 180.0 / 32768.0
    }

    /// Elevation angle in degrees (same encoding as azimuth).
    pub(crate) fn elevation_angle_degrees(&self) -> f32 {
        f32::from(self.elevation_angle_raw) * 180.0 / 32768.0
    }

    /// Collection time as a `DateTime<Utc>`. Returns `None` if the
    /// values fall outside the chrono-representable range.
    ///
    /// Per ICD 2620002R Table III §3.2.4.2, `modified_julian_date`
    /// is **1-indexed days since 1970-01-01**: day 1 = 1970-01-01,
    /// day 2 = 1970-01-02, … `collection_time_ms` is milliseconds
    /// since midnight of that day. The `-1` below is the difference
    /// between "1-indexed day-of-epoch" (ICD convention) and
    /// "0-indexed seconds-since-epoch" (Unix convention). Matches
    /// xradar's `(date - 1) * 86400e3 + ms` and `danielway/nexrad`'s
    /// `volume/record.rs` byte-for-byte.
    pub(crate) fn collection_time(&self) -> Option<DateTime<Utc>> {
        let days = i64::from(self.modified_julian_date);
        let secs = i64::from(self.collection_time_ms / 1_000);
        let nanos = u32::try_from((self.collection_time_ms % 1_000).abs()).ok()? * 1_000_000;
        let total_secs = days
            .checked_sub(1)?
            .checked_mul(86_400)?
            .checked_add(secs)?;
        Utc.timestamp_opt(total_secs, nanos).single()
    }

    /// Doppler velocity resolution as m/s per LSB (ICD §3.2.4.5).
    /// Code `4` → 1.0 m/s; anything else → 0.5 m/s.
    pub(crate) fn doppler_velocity_resolution_mps(&self) -> f32 {
        if self.doppler_velocity_resolution == 4 {
            1.0
        } else {
            0.5
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a 100-byte MSG_1 header with distinct values per field.
    /// Pointers default to no gate data (set explicitly per test).
    fn synth_header(
        refl_ptr: i16,
        vel_ptr: i16,
        sw_ptr: i16,
        num_surv: i16,
        num_dopp: i16,
    ) -> Vec<u8> {
        let mut buf = Vec::with_capacity(MSG1_HEADER_BYTES);
        buf.extend_from_slice(&12_345_678_i32.to_be_bytes()); // collection_time_ms
        buf.extend_from_slice(&15_000_i16.to_be_bytes()); // julian date
        buf.extend_from_slice(&4_660_i16.to_be_bytes()); // unambiguous range × 100m
        buf.extend_from_slice(&8_192_i16.to_be_bytes()); // azimuth = 45°
        buf.extend_from_slice(&73_i16.to_be_bytes()); // azimuth_number
        buf.extend_from_slice(&1_i16.to_be_bytes()); // radial_status (intermediate)
        buf.extend_from_slice(&182_i16.to_be_bytes()); // elevation ≈ 1.0°
        buf.extend_from_slice(&3_i16.to_be_bytes()); // elevation_number
        buf.extend_from_slice(&(-2_000_i16).to_be_bytes()); // surv first gate range
        buf.extend_from_slice(&125_i16.to_be_bytes()); // dopp first gate range
        buf.extend_from_slice(&1_000_i16.to_be_bytes()); // surv interval (m)
        buf.extend_from_slice(&250_i16.to_be_bytes()); // dopp interval
        buf.extend_from_slice(&num_surv.to_be_bytes());
        buf.extend_from_slice(&num_dopp.to_be_bytes());
        buf.extend_from_slice(&1_i16.to_be_bytes()); // sector
        buf.extend_from_slice(&(-67.5_f32).to_be_bytes()); // calibration constant
        buf.extend_from_slice(&refl_ptr.to_be_bytes());
        buf.extend_from_slice(&vel_ptr.to_be_bytes());
        buf.extend_from_slice(&sw_ptr.to_be_bytes());
        buf.extend_from_slice(&2_i16.to_be_bytes()); // vel res = 0.5 m/s
        buf.extend_from_slice(&11_i16.to_be_bytes()); // VCP 11
        buf.extend(vec![0u8; 54]); // spare
        debug_assert_eq!(buf.len(), MSG1_HEADER_BYTES);
        buf
    }

    #[test]
    fn read_consumes_exactly_100_bytes_with_no_moment_pointers() {
        let bytes = synth_header(0, 0, 0, 0, 0);
        let mut r = SliceReader::new(&bytes);
        let m = Msg1::read(&mut r).unwrap();
        assert_eq!(r.position(), MSG1_HEADER_BYTES);
        assert!(m.reflectivity_gates.is_none());
        assert!(m.velocity_gates.is_none());
        assert!(m.spectrum_width_gates.is_none());
    }

    #[test]
    fn round_trip_preserves_every_header_field() {
        let bytes = synth_header(0, 0, 0, 0, 0);
        let mut r = SliceReader::new(&bytes);
        let m = Msg1::read(&mut r).unwrap();
        assert_eq!(m.collection_time_ms, 12_345_678);
        assert_eq!(m.modified_julian_date, 15_000);
        assert_eq!(m.unambiguous_range_x100m, 4_660);
        assert_eq!(m.azimuth_angle_raw, 8_192);
        assert_eq!(m.azimuth_number, 73);
        assert_eq!(m.radial_status, 1);
        assert_eq!(m.elevation_angle_raw, 182);
        assert_eq!(m.elevation_number, 3);
        assert_eq!(m.surveillance_first_gate_range_m, -2_000);
        assert_eq!(m.doppler_first_gate_range_m, 125);
        assert_eq!(m.surveillance_gate_interval_m, 1_000);
        assert_eq!(m.doppler_gate_interval_m, 250);
        assert_eq!(m.sector_number, 1);
        assert!((m.calibration_constant_db - (-67.5)).abs() < 1e-6);
        assert_eq!(m.doppler_velocity_resolution, 2);
        assert_eq!(m.vcp_number, 11);
    }

    #[test]
    fn azimuth_and_elevation_decode_to_degrees() {
        let bytes = synth_header(0, 0, 0, 0, 0);
        let mut r = SliceReader::new(&bytes);
        let m = Msg1::read(&mut r).unwrap();
        // 8192 / 32768 * 180 = 45.0
        assert!((m.azimuth_angle_degrees() - 45.0).abs() < 1e-3);
        // 182 / 32768 * 180 ≈ 0.99975 ≈ 1.0
        assert!((m.elevation_angle_degrees() - 1.0).abs() < 1e-2);
    }

    #[test]
    fn read_decodes_gate_arrays_when_pointers_set() {
        // Body layout: [100-byte header][460 refl bytes][920 vel][920 sw]
        //   = 2400 bytes total.
        let refl_ptr = MSG1_HEADER_BYTES as i16; // 100
        let num_surv: i16 = 460;
        let vel_ptr = (MSG1_HEADER_BYTES + num_surv as usize) as i16; // 560
        let num_dopp: i16 = 920;
        let sw_ptr = vel_ptr + num_dopp; // 1480

        let mut bytes = synth_header(refl_ptr, vel_ptr, sw_ptr, num_surv, num_dopp);
        bytes.extend(vec![0xAAu8; num_surv as usize]);
        bytes.extend(vec![0xBBu8; num_dopp as usize]);
        bytes.extend(vec![0xCCu8; num_dopp as usize]);

        let mut r = SliceReader::new(&bytes);
        let m = Msg1::read(&mut r).unwrap();

        let refl = m.reflectivity_gates.expect("refl gates");
        assert_eq!(refl.len(), num_surv as usize);
        assert!(refl.iter().all(|&b| b == 0xAA));

        let vel = m.velocity_gates.expect("vel gates");
        assert_eq!(vel.len(), num_dopp as usize);
        assert!(vel.iter().all(|&b| b == 0xBB));

        let sw = m.spectrum_width_gates.expect("sw gates");
        assert_eq!(sw.len(), num_dopp as usize);
        assert!(sw.iter().all(|&b| b == 0xCC));
    }

    #[test]
    fn missing_moment_pointer_yields_none() {
        // Only reflectivity present; velocity + sw pointers zero.
        let num_surv: i16 = 100;
        let mut bytes = synth_header(MSG1_HEADER_BYTES as i16, 0, 0, num_surv, 0);
        bytes.extend(vec![0xAAu8; num_surv as usize]);
        let mut r = SliceReader::new(&bytes);
        let m = Msg1::read(&mut r).unwrap();
        assert!(m.reflectivity_gates.is_some());
        assert!(m.velocity_gates.is_none());
        assert!(m.spectrum_width_gates.is_none());
    }

    #[test]
    fn read_errors_on_short_input() {
        let bytes = synth_header(0, 0, 0, 0, 0);
        let mut r = SliceReader::new(&bytes[..50]);
        assert!(matches!(
            Msg1::read(&mut r).unwrap_err(),
            NexradDecodeError::UnexpectedEof { .. }
        ));
    }

    #[test]
    fn doppler_velocity_resolution_decodes_codes_2_and_4() {
        let bytes_05 = synth_header(0, 0, 0, 0, 0);
        let m05 = Msg1::read(&mut SliceReader::new(&bytes_05)).unwrap();
        assert!((m05.doppler_velocity_resolution_mps() - 0.5).abs() < 1e-6);

        // Patch HW22 (vel res) to 4 → 1.0 m/s.
        let mut bytes_10 = synth_header(0, 0, 0, 0, 0);
        bytes_10[42..44].copy_from_slice(&4_i16.to_be_bytes());
        let m10 = Msg1::read(&mut SliceReader::new(&bytes_10)).unwrap();
        assert!((m10.doppler_velocity_resolution_mps() - 1.0).abs() < 1e-6);
    }

    /// ICD §3.2.4.2: MSG_1's `modified_julian_date` is **1-indexed
    /// days since 1970-01-01**, so day 1 with `collection_time_ms=0`
    /// must decode to exactly the Unix epoch. Companion regression
    /// test to `msg31_collection_time_day_1_decodes_to_unix_epoch`
    /// for the legacy decode path.
    #[test]
    fn collection_time_day_1_decodes_to_unix_epoch() {
        // Patch the synthetic header's collection_time (bytes 0..4)
        // and modified_julian_date (bytes 4..6) to the day-1 / ms=0
        // boundary case.
        let mut bytes = synth_header(0, 0, 0, 0, 0);
        bytes[0..4].copy_from_slice(&0_i32.to_be_bytes());
        bytes[4..6].copy_from_slice(&1_i16.to_be_bytes());
        let m = Msg1::read(&mut SliceReader::new(&bytes)).unwrap();
        let dt = m.collection_time().expect("decoded");
        assert_eq!(dt.timestamp(), 0, "day 1 + ms 0 must equal Unix epoch");
        assert_eq!(dt.timestamp_subsec_nanos(), 0);
    }
}
