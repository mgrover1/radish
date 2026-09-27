//! MSG_31 information blocks: VOL, ELV, RAD.
//!
//! Each starts with a 4-byte `DataBlockId` ("D" + 3-byte ASCII
//! name like "VOL", "ELV", "RAD"). The byte after the id is the
//! block-size halfword `lrtup`. The remainder is the block-specific
//! payload per ICD Table XVII-E (VOL), XVII-F (ELV), XVII-H (RAD).
//!
//! Build 12.0 expanded RAD (16 → 24 bytes) for dual-polarization;
//! Build 20.0 expanded VOL (40 → 48 bytes) for the
//! `zdr_bias_estimate_weighted_mean` field. We branch on the
//! declared `lrtup` so both legacy and modern files round-trip.

use crate::backends::nexrad::decode::error::Result;
use crate::backends::nexrad::decode::reader::SliceReader;

/// 4-byte ASCII data block identifier (e.g. "DVOL", "DREF", "DCFP")
/// — the "D" prefix is constant per ICD Table XVII-B; the trailing
/// 3 bytes are the block name.
pub(crate) const DATA_BLOCK_ID_SIZE: usize = 4;

/// Decoded `DataBlockId`. Parsed once at the start of every block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DataBlockId {
    /// First byte should be `b'D'` per ICD; we just preserve it.
    pub(crate) marker: u8,
    /// 3-byte ASCII block name (e.g. `b"VOL"`, `b"REF"`).
    pub(crate) name: [u8; 3],
}

impl DataBlockId {
    pub(crate) fn read(reader: &mut SliceReader<'_>) -> Result<Self> {
        let bytes = reader.take_bytes(DATA_BLOCK_ID_SIZE)?;
        let mut name = [0u8; 3];
        name.copy_from_slice(&bytes[1..4]);
        Ok(Self {
            marker: bytes[0],
            name,
        })
    }

    pub(crate) fn name_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.name)
    }
}

/// Volume Data Constant block (ICD §3.2.4.17.5 Table XVII-E).
///
/// Two on-wire variants: the legacy 40-byte struct (Build 19.0 and
/// earlier) and the modern 48-byte struct (Build 20.0+) which adds
/// `zdr_bias_estimate_weighted_mean` plus 6 spare bytes. We detect
/// the variant from the `lrtup` field:
///
/// * `lrtup == 44` → legacy (40 + 4 DataBlockId)
/// * `lrtup == 52` → modern (48 + 4 DataBlockId)
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct VolumeBlock {
    pub(crate) lrtup: u16,
    pub(crate) major_version: u8,
    pub(crate) minor_version: u8,
    pub(crate) latitude_degrees: f32,
    pub(crate) longitude_degrees: f32,
    pub(crate) site_height_m: i16,
    pub(crate) tower_height_m: u16,
    pub(crate) calibration_constant_db: f32,
    pub(crate) horizontal_shv_tx_power_kw: f32,
    pub(crate) vertical_shv_tx_power_kw: f32,
    pub(crate) system_differential_reflectivity_db: f32,
    pub(crate) initial_system_differential_phase_deg: f32,
    pub(crate) volume_coverage_pattern_number: u16,
    pub(crate) processing_status: u16,
    /// Build-20+ only — `None` on legacy files.
    pub(crate) zdr_bias_estimate_weighted_mean: Option<i16>,
}

impl VolumeBlock {
    pub(crate) fn read(reader: &mut SliceReader<'_>) -> Result<Self> {
        let lrtup = reader.read_u16_be()?;
        let major_version = reader.read_u8()?;
        let minor_version = reader.read_u8()?;
        let latitude_degrees = reader.read_f32_be()?;
        let longitude_degrees = reader.read_f32_be()?;
        let site_height_m = reader.read_i16_be()?;
        let tower_height_m = reader.read_u16_be()?;
        let calibration_constant_db = reader.read_f32_be()?;
        let horizontal_shv_tx_power_kw = reader.read_f32_be()?;
        let vertical_shv_tx_power_kw = reader.read_f32_be()?;
        let system_differential_reflectivity_db = reader.read_f32_be()?;
        let initial_system_differential_phase_deg = reader.read_f32_be()?;
        let volume_coverage_pattern_number = reader.read_u16_be()?;
        let processing_status = reader.read_u16_be()?;
        let zdr_bias_estimate_weighted_mean = if lrtup >= 52 {
            // Modern (Build 20+) — 2-byte ZDR bias estimate plus 6 spare.
            let v = reader.read_i16_be()?;
            reader.advance(6)?;
            Some(v)
        } else {
            None
        };
        Ok(Self {
            lrtup,
            major_version,
            minor_version,
            latitude_degrees,
            longitude_degrees,
            site_height_m,
            tower_height_m,
            calibration_constant_db,
            horizontal_shv_tx_power_kw,
            vertical_shv_tx_power_kw,
            system_differential_reflectivity_db,
            initial_system_differential_phase_deg,
            volume_coverage_pattern_number,
            processing_status,
            zdr_bias_estimate_weighted_mean,
        })
    }
}

/// Elevation Data Constant block (ICD §3.2.4.17.6 Table XVII-F).
/// Single 8-byte payload — atmospheric attenuation + reflectivity
/// calibration at this elevation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ElevationBlock {
    pub(crate) lrtup: u16,
    /// Atmospheric attenuation in dB/km. ICD: ScaledSInteger2 with
    /// LSB = 0.001, so reported as `raw / 1000.0`.
    pub(crate) atmospheric_attenuation_db_per_km: f32,
    pub(crate) calibration_constant_db: f32,
}

impl ElevationBlock {
    pub(crate) fn read(reader: &mut SliceReader<'_>) -> Result<Self> {
        let lrtup = reader.read_u16_be()?;
        let atmos_raw = reader.read_i16_be()?;
        let calibration_constant_db = reader.read_f32_be()?;
        Ok(Self {
            lrtup,
            atmospheric_attenuation_db_per_km: f32::from(atmos_raw) / 1000.0,
            calibration_constant_db,
        })
    }
}

/// Radial Data Constant block (ICD §3.2.4.17.7 Table XVII-H).
///
/// Build 12.0+ added per-channel calibration constants for dual
/// polarisation. We branch on `lrtup`:
///
/// * `lrtup == 20` → legacy (16 + 4 DataBlockId)
/// * `lrtup == 28` → modern (24 + 4 DataBlockId)
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RadialBlock {
    pub(crate) lrtup: u16,
    pub(crate) unambiguous_range_km: f32,
    pub(crate) horizontal_channel_noise_level_dbm: f32,
    pub(crate) vertical_channel_noise_level_dbm: f32,
    pub(crate) nyquist_velocity_m_per_s: f32,
    pub(crate) radial_flags: u16,
    /// Build-12+ only — `None` on legacy files.
    pub(crate) horizontal_calibration_constant_db: Option<f32>,
    /// Build-12+ only — `None` on legacy files.
    pub(crate) vertical_calibration_constant_db: Option<f32>,
}

impl RadialBlock {
    pub(crate) fn read(reader: &mut SliceReader<'_>) -> Result<Self> {
        let lrtup = reader.read_u16_be()?;
        let unambig_raw = reader.read_i16_be()?;
        let horizontal_channel_noise_level_dbm = reader.read_f32_be()?;
        let vertical_channel_noise_level_dbm = reader.read_f32_be()?;
        let nyquist_raw = reader.read_i16_be()?;
        let radial_flags = reader.read_u16_be()?;
        let (horizontal_calibration_constant_db, vertical_calibration_constant_db) = if lrtup >= 28
        {
            let h = reader.read_f32_be()?;
            let v = reader.read_f32_be()?;
            (Some(h), Some(v))
        } else {
            (None, None)
        };
        // ICD Table XVII-H: unambiguous_range and nyquist_velocity
        // are ScaledInteger2 with LSB = 0.1 → divide raw by 10.0.
        Ok(Self {
            lrtup,
            unambiguous_range_km: f32::from(unambig_raw) / 10.0,
            horizontal_channel_noise_level_dbm,
            vertical_channel_noise_level_dbm,
            nyquist_velocity_m_per_s: f32::from(nyquist_raw) / 100.0,
            radial_flags,
            horizontal_calibration_constant_db,
            vertical_calibration_constant_db,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_block_id_round_trips() {
        let bytes = b"DVOL";
        let mut r = SliceReader::new(bytes);
        let id = DataBlockId::read(&mut r).unwrap();
        assert_eq!(id.marker, b'D');
        assert_eq!(&id.name, b"VOL");
        assert_eq!(id.name_str(), "VOL");
    }

    /// Build a 40-byte legacy VOL payload (Build 19 and earlier).
    fn legacy_vol_payload() -> Vec<u8> {
        let mut buf = Vec::with_capacity(40);
        buf.extend_from_slice(&44u16.to_be_bytes()); // lrtup = 44
        buf.push(2); // major
        buf.push(0); // minor
        buf.extend_from_slice(&41.604_f32.to_be_bytes()); // KLOT lat
        buf.extend_from_slice(&(-88.084_f32).to_be_bytes()); // KLOT lon
        buf.extend_from_slice(&202_i16.to_be_bytes()); // site height
        buf.extend_from_slice(&29u16.to_be_bytes()); // tower
        buf.extend_from_slice(&36.0_f32.to_be_bytes()); // calib const
        buf.extend_from_slice(&750.0_f32.to_be_bytes()); // h tx pwr
        buf.extend_from_slice(&750.0_f32.to_be_bytes()); // v tx pwr
        buf.extend_from_slice(&0.5_f32.to_be_bytes()); // sys ZDR
        buf.extend_from_slice(&90.0_f32.to_be_bytes()); // initial DP
        buf.extend_from_slice(&212u16.to_be_bytes()); // VCP number
        buf.extend_from_slice(&0u16.to_be_bytes()); // proc status
        debug_assert_eq!(buf.len(), 40);
        buf
    }

    #[test]
    fn volume_block_legacy_round_trips() {
        let bytes = legacy_vol_payload();
        let mut r = SliceReader::new(&bytes);
        let v = VolumeBlock::read(&mut r).unwrap();
        // Cover every field that comes in from the wire so a future
        // typo in `read()` (e.g. swapping H tx_power and V tx_power
        // since they're both Real4) is caught.
        assert_eq!(v.lrtup, 44);
        assert_eq!(v.major_version, 2);
        assert_eq!(v.minor_version, 0);
        assert!((v.latitude_degrees - 41.604).abs() < 1e-3);
        assert!((v.longitude_degrees - (-88.084)).abs() < 1e-3);
        assert_eq!(v.site_height_m, 202);
        assert_eq!(v.tower_height_m, 29);
        assert!((v.calibration_constant_db - 36.0).abs() < 1e-6);
        assert!((v.horizontal_shv_tx_power_kw - 750.0).abs() < 1e-6);
        assert!((v.vertical_shv_tx_power_kw - 750.0).abs() < 1e-6);
        assert!((v.system_differential_reflectivity_db - 0.5).abs() < 1e-6);
        assert!((v.initial_system_differential_phase_deg - 90.0).abs() < 1e-6);
        assert_eq!(v.volume_coverage_pattern_number, 212);
        assert_eq!(v.processing_status, 0);
        assert!(
            v.zdr_bias_estimate_weighted_mean.is_none(),
            "legacy block doesn't carry zdr_bias"
        );
    }

    #[test]
    fn volume_block_modern_carries_zdr_bias() {
        // Modern: lrtup = 52, plus 8 trailing bytes (zdr_bias_int16
        // + 6 spare).
        let mut bytes = legacy_vol_payload();
        bytes[..2].copy_from_slice(&52u16.to_be_bytes());
        bytes.extend_from_slice(&(-100_i16).to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        let mut r = SliceReader::new(&bytes);
        let v = VolumeBlock::read(&mut r).unwrap();
        assert_eq!(v.lrtup, 52);
        assert_eq!(v.zdr_bias_estimate_weighted_mean, Some(-100));
    }

    #[test]
    fn elevation_block_round_trips() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&12u16.to_be_bytes()); // lrtup
        buf.extend_from_slice(&(-15_i16).to_be_bytes()); // atmos: -0.015 dB/km
        buf.extend_from_slice(&36.0_f32.to_be_bytes()); // calib const
        let mut r = SliceReader::new(&buf);
        let e = ElevationBlock::read(&mut r).unwrap();
        assert_eq!(e.lrtup, 12);
        assert!((e.atmospheric_attenuation_db_per_km - (-0.015)).abs() < 1e-6);
        assert!((e.calibration_constant_db - 36.0).abs() < 1e-6);
    }

    #[test]
    fn radial_block_legacy_round_trips() {
        // Legacy: 16 bytes (lrtup=20).
        let mut buf = Vec::new();
        buf.extend_from_slice(&20u16.to_be_bytes()); // lrtup
        buf.extend_from_slice(&4660_i16.to_be_bytes()); // unambig_range raw → 466.0 km
        buf.extend_from_slice(&(-90.0_f32).to_be_bytes()); // h noise
        buf.extend_from_slice(&(-90.0_f32).to_be_bytes()); // v noise
        buf.extend_from_slice(&3500_i16.to_be_bytes()); // nyquist raw → 35.0 m/s
        buf.extend_from_slice(&0u16.to_be_bytes()); // radial flags
        let mut r = SliceReader::new(&buf);
        let rad = RadialBlock::read(&mut r).unwrap();
        assert_eq!(rad.lrtup, 20);
        assert!((rad.unambiguous_range_km - 466.0).abs() < 1e-6);
        assert!((rad.nyquist_velocity_m_per_s - 35.0).abs() < 1e-6);
        assert!(rad.horizontal_calibration_constant_db.is_none());
    }

    #[test]
    fn radial_block_modern_carries_calib_constants() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&28u16.to_be_bytes()); // lrtup = modern
        buf.extend_from_slice(&4660_i16.to_be_bytes());
        buf.extend_from_slice(&(-90.0_f32).to_be_bytes());
        buf.extend_from_slice(&(-90.0_f32).to_be_bytes());
        buf.extend_from_slice(&3500_i16.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0.5_f32.to_be_bytes()); // h calib
        buf.extend_from_slice(&0.6_f32.to_be_bytes()); // v calib
        let mut r = SliceReader::new(&buf);
        let rad = RadialBlock::read(&mut r).unwrap();
        assert_eq!(rad.horizontal_calibration_constant_db, Some(0.5));
        assert_eq!(rad.vertical_calibration_constant_db, Some(0.6));
    }
}
