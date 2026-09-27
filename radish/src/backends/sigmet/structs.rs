//! On-wire struct parsers for IRIS RAW.
//!
//! Field layouts ported verbatim from xradar's `iris.py` dictionaries
//! (see line 510+ for `STRUCTURE_HEADER`, 1308 for `INGEST_CONFIGURATION`,
//! 1606 for `INGEST_HEADER`, etc.). All fields are little-endian. We use
//! explicit `byteorder::ReadBytesExt::read_iN::<LittleEndian>()` calls
//! rather than `bytemuck`-style `repr(C, packed)` casts so each offset is
//! visible at the call site — debugging an offset mismatch on a real
//! fixture is far easier this way (the Phase 0 spike caught one such
//! mistake immediately).
//!
//! Conventions used here:
//! * `BIN2` (unsigned 16-bit angle): `degrees = raw * 360 / 65536`
//! * `BIN4` (signed 32-bit angle):   `degrees = raw * 180 / 2^31`
//! * `YMDS_TIME` (12 B): seconds + millis + status + year + month + day
//!
//! Sizes (from xradar dict sums):
//! * STRUCTURE_HEADER     : 12 B
//! * INGEST_CONFIGURATION : 480 B
//! * TASK_SCHED_INFO      : 120 B
//! * TASK_DSP_INFO        : 320 B
//! * TASK_CALIB_INFO      : 320 B
//! * TASK_RANGE_INFO      : 160 B
//! * TASK_SCAN_INFO       : 320 B
//! * TASK_MISC_INFO       : 320 B
//! * TASK_END_INFO        : 320 B
//! * TASK_CONFIGURATION   : sum of the above (1880 B core + padding to 2612 B)
//! * INGEST_HEADER        : STRUCTURE_HEADER + spare(12) + INGEST_CONFIGURATION + TASK_CONFIGURATION + spare = 4884 B

use byteorder::{LittleEndian, ReadBytesExt};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use std::io::{Cursor, Read};

use crate::{RadishError, Result};

/// Size of one IRIS record on disk.
pub(super) const RECORD_BYTES: usize = 6144;

/// Size of `STRUCTURE_HEADER` in bytes.
pub(super) const STRUCTURE_HEADER_BYTES: usize = 12;
/// Size of `INGEST_CONFIGURATION` in bytes.
pub(super) const INGEST_CONFIGURATION_BYTES: usize = 480;
/// Size of `INGEST_DATA_HEADER` in bytes (one per (sweep, moment) pair).
pub(super) const INGEST_DATA_HEADER_BYTES: usize = 76;
/// Size of `RAW_PROD_BHDR` in bytes (skipped at every record boundary).
pub(super) const RAW_PROD_BHDR_BYTES: usize = 12;

/// Minimum acceptable size of a `TASK_CONFIGURATION` block. Anything
/// shorter and we know the file is truncated before the SCAN_INFO sweep
/// fixed-angle table — `parse` returns `MalformedRecord`.
pub(super) const TASK_CONFIGURATION_MIN_BYTES: usize = 1892;
/// Size of a fully-padded `TASK_CONFIGURATION` block; some IRIS encoders
/// pad up to this length even though the core fields end at
/// [`TASK_CONFIGURATION_MIN_BYTES`]. We accept anything in between and
/// cap reads at the larger value to avoid stepping into the next record.
pub(super) const TASK_CONFIGURATION_MAX_BYTES: usize = 2612;

/// `structure_identifier` value indicating an `INGEST_HEADER`.
pub(super) const STRUCT_ID_INGEST_HEADER: i16 = 23;
/// `structure_identifier` value indicating a `PRODUCT_HDR`.
pub(super) const STRUCT_ID_PRODUCT_HDR: i16 = 27;

/// `STRUCTURE_HEADER` (12 B). Every IRIS sub-structure begins with one.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(super) struct StructureHeader {
    pub structure_identifier: i16,
    pub format_version: i16,
    pub bytes_in_structure: i32,
    pub reserved: i16,
    pub flag: i16,
}

impl StructureHeader {
    pub fn parse(mut buf: &[u8]) -> Result<Self> {
        if buf.len() < STRUCTURE_HEADER_BYTES {
            return Err(RadishError::MalformedRecord {
                offset: 0,
                msg: format!(
                    "STRUCTURE_HEADER needs {STRUCTURE_HEADER_BYTES} bytes, got {}",
                    buf.len()
                ),
            });
        }
        Ok(Self {
            structure_identifier: read_i16_le(&mut buf)?,
            format_version: read_i16_le(&mut buf)?,
            bytes_in_structure: read_i32_le(&mut buf)?,
            reserved: read_i16_le(&mut buf)?,
            flag: read_i16_le(&mut buf)?,
        })
    }
}

/// Decoded subset of `INGEST_CONFIGURATION` used by the adapter.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(super) struct IngestConfiguration {
    pub iris_version: String,
    pub site_name: String,
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    pub height_site_m: i16,
    pub height_radar_m: i16,
    pub altitude_radar_cm: i32,
    pub volume_scan_start_time: DateTime<Utc>,
    pub task_config_count: i16,
}

impl IngestConfiguration {
    /// Parse `INGEST_CONFIGURATION` from the 480-byte sub-buffer that
    /// follows `STRUCTURE_HEADER` inside `INGEST_HEADER`. Field offsets
    /// match xradar's dict layout (verified on a real CHI fixture in the
    /// Phase 0 spike).
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < INGEST_CONFIGURATION_BYTES {
            return Err(RadishError::MalformedRecord {
                offset: 0,
                msg: format!(
                    "INGEST_CONFIGURATION needs {INGEST_CONFIGURATION_BYTES} bytes, got {}",
                    buf.len()
                ),
            });
        }
        let mut c = Cursor::new(buf);

        // Layout (cumulative offsets in INGEST_CONFIGURATION):
        //   off 0   filename(80)
        //   off 80  number_files(2) + number_sweeps_completed(2) + total_size(4)
        //   off 88  volume_scan_start_time(12 = YMDS_TIME)
        //   off 100 spare_0(12)
        //   off 112 ray_header_bytes(2) + extended_ray_header_bytes(2)
        //   off 116 number_task_config_table(2) + playback_version(2)
        //   off 120 spare_1(4)
        //   off 124 iris_version(8)
        //   off 132 hardware_site(16)
        //   off 148 gmt_offset_minutes_local(2)
        //   off 150 site_name(16)
        //   off 166 gmt_offset_minutes_standard(2)
        //   off 168 latitude_radar(BIN4)
        //   off 172 longitude_radar(BIN4)
        //   off 176 height_site(2) + height_radar(2)
        //   off 180 (...)
        //   off 192 altitude_radar(SINT4)
        //   off 196 (...) — velocity, antenna offsets, fault status, etc.

        c.set_position(88);
        let volume_scan_start_time = read_ymds_time(&mut c)?;

        c.set_position(116);
        let task_config_count = c.read_i16::<LittleEndian>()?;

        c.set_position(124);
        let iris_version = read_fixed_string(&mut c, 8)?;

        c.set_position(150);
        let site_name = read_fixed_string(&mut c, 16)?;

        c.set_position(168);
        let lat_raw = c.read_i32::<LittleEndian>()?;
        let lon_raw = c.read_i32::<LittleEndian>()?;
        let height_site_m = c.read_i16::<LittleEndian>()?;
        let height_radar_m = c.read_i16::<LittleEndian>()?;

        c.set_position(192);
        let altitude_radar_cm = c.read_i32::<LittleEndian>()?;

        Ok(Self {
            iris_version,
            site_name,
            latitude_deg: bin4_to_degrees(lat_raw),
            longitude_deg: bin4_to_degrees(lon_raw),
            height_site_m,
            height_radar_m,
            altitude_radar_cm,
            volume_scan_start_time,
            task_config_count,
        })
    }
}

/// Decoded subset of `TASK_CONFIGURATION` used by the adapter.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(super) struct TaskConfiguration {
    pub task_name: String,
    pub scan_mode: ScanMode,
    pub sweeps_per_volume: u16,
    /// Bitmask of enabled IRIS data-type ids. xradar's `DSP_DATA_MASK`
    /// stores four UINT4 mask words (`mask_word_0..3`) covering bits
    /// 0..127, with `extended_header_type` interleaved between word_0
    /// and word_1. We pack them into a single `u128` so the adapter can
    /// iterate `0..128` and pick out IDs above 31 (DB_HCLASS=55,
    /// DB_DBTE8=71, DB_DBZE8=73, etc. — common in real CHI fixtures).
    pub dsp_data_mask: u128,
    pub nyquist_velocity_ms: f32,
    pub prf_hz: f32,
    /// Radar wavelength in centimetres (from `TASK_MISC_INFO.wavelength`,
    /// stored in 1/100 cm). Used by the KDP decoder.
    pub wavelength_cm: f32,
    pub unambiguous_range_m: f32,
    /// First gate distance in centimetres.
    pub range_first_bin_cm: i32,
    /// Last gate distance in centimetres.
    pub range_last_bin_cm: i32,
    /// Step between gates (cm) on the OUTPUT side after binning.
    pub step_output_bins_cm: i32,
    /// Number of OUTPUT bins per ray (after step / number_input_bins reduction).
    pub bins_output: u16,
    /// Per-sweep target elevation angles (degrees), one per sweep.
    pub sweep_fixed_angles_deg: Vec<f32>,
}

/// Distilled scan mode. The ICD has more values; we collapse them into
/// the shapes the adapter actually treats differently. `#[non_exhaustive]`
/// because the adapter and the Python wrapper match on this — adding a
/// future variant (e.g. `Sector` for sector-PPI) shouldn't break callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub(super) enum ScanMode {
    Ppi,
    Rhi,
    Other(u16),
}

impl ScanMode {
    pub fn label(&self) -> &'static str {
        match self {
            ScanMode::Ppi => "PPI",
            ScanMode::Rhi => "RHI",
            ScanMode::Other(_) => "OTHER",
        }
    }
}

impl TaskConfiguration {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        // TASK_CONFIGURATION layout (cumulative offsets, including the
        // leading STRUCTURE_HEADER which IS part of the block):
        //   off 0     STRUCTURE_HEADER (12)
        //   off 12    TASK_SCHED_INFO  (120)
        //   off 132   TASK_DSP_INFO    (320)
        //   off 452   TASK_CALIB_INFO  (320)
        //   off 772   TASK_RANGE_INFO  (160)
        //   off 932   TASK_SCAN_INFO   (320)
        //   off 1252  TASK_MISC_INFO   (320)
        //   off 1572  TASK_END_INFO    (320)
        //   off 1892  comments(720)
        if buf.len() < TASK_CONFIGURATION_MIN_BYTES {
            return Err(RadishError::MalformedRecord {
                offset: 0,
                msg: format!(
                    "TASK_CONFIGURATION too short: {} < {TASK_CONFIGURATION_MIN_BYTES}",
                    buf.len()
                ),
            });
        }
        const TASK_DSP_INFO_OFF: u64 = 132;
        const TASK_RANGE_INFO_OFF: u64 = 772;
        const TASK_SCAN_INFO_OFF: u64 = 932;
        const TASK_MISC_INFO_OFF: u64 = 1252;
        const TASK_END_INFO_OFF: u64 = 1572;

        let mut c = Cursor::new(buf);

        // ----- TASK_DSP_INFO -----
        // Inside TASK_DSP_INFO (xradar `DSP_DATA_MASK` lives in
        // TASK_DSP_INFO at offset 4):
        //   off 0   iris_task_id (4)
        //   off 4   mask_word_0          (UINT4)  ← bits 0..31
        //   off 8   extended_header_type (UINT4)
        //   off 12  mask_word_1          (UINT4)  ← bits 32..63
        //   off 16  mask_word_2          (UINT4)  ← bits 64..95
        //   off 20  mask_word_3          (UINT4)  ← bits 96..127
        //   off 24  mask_word_4          (UINT4)  (unused for data-type lookup)
        //   off 28  ... DSP options
        // `_data_types_from_dsp_mask` (iris.py:430) ignores word_4 and
        // `extended_header_type`, so we mirror that.
        c.set_position(TASK_DSP_INFO_OFF + 4);
        let mask_word_0 = c.read_u32::<LittleEndian>()? as u128;
        let _extended_header_type = c.read_u32::<LittleEndian>()?;
        let mask_word_1 = c.read_u32::<LittleEndian>()? as u128;
        let mask_word_2 = c.read_u32::<LittleEndian>()? as u128;
        let mask_word_3 = c.read_u32::<LittleEndian>()? as u128;
        let dsp_data_mask =
            mask_word_0 | (mask_word_1 << 32) | (mask_word_2 << 64) | (mask_word_3 << 96);

        // PRF (UINT4 Hz) lives somewhere in TASK_DSP_INFO. Exact offset is
        // version-dependent; xradar's iris.py reads it via
        // `task_dsp_info`'s dict, which puts `prf` at offset 136 in the
        // typical Build 8.x layout. We accept that as best-effort; if
        // unset, fall back to deriving unambiguous_range from
        // range_last_bin_cm.
        c.set_position(TASK_DSP_INFO_OFF + 136);
        let prf_hz = c.read_i32::<LittleEndian>()?.max(0) as f32;

        // ----- TASK_MISC_INFO -----
        // `wavelength` (SINT4, units of 1/100 cm) is the first field of
        // TASK_MISC_INFO. We need it for the KDP decoder (xradar passes
        // `wavelength / 100` cm) and to derive the Nyquist velocity.
        c.set_position(TASK_MISC_INFO_OFF);
        let wavelength_raw = c.read_i32::<LittleEndian>()?.max(0) as f32;
        let wavelength_cm = wavelength_raw / 100.0;

        // ----- nyquist velocity -----
        // xradar: `nyquist = wavelength * prf / (10000 * 4)` (m/s), where
        // `wavelength` is in 1/100 cm. For dual-PRF tasks xradar scales this
        // by `multi_prf_mode_flag + 1`; we don't parse that flag yet, so this
        // is the single-PRF Nyquist. Only the 8-bit VEL/WIDTH decoders use
        // it (the 16-bit variants are self-scaled).
        let nyquist_velocity_ms = if prf_hz > 0.0 {
            wavelength_raw * prf_hz / 40000.0
        } else {
            0.0
        };

        // ----- TASK_RANGE_INFO -----
        //   off 0  range_first_bin (SINT4)
        //   off 4  range_last_bin  (SINT4)
        //   off 8  number_input_bins  (SINT2)
        //   off 10 number_output_bins (SINT2)
        //   off 12 step_input_bins    (SINT4)
        //   off 16 step_output_bins   (SINT4)
        c.set_position(TASK_RANGE_INFO_OFF);
        let range_first_bin_cm = c.read_i32::<LittleEndian>()?;
        let range_last_bin_cm = c.read_i32::<LittleEndian>()?;
        let _number_input_bins = c.read_i16::<LittleEndian>()?;
        let bins_output = c.read_u16::<LittleEndian>()?;
        let _step_input_bins = c.read_i32::<LittleEndian>()?;
        let step_output_bins_cm = c.read_i32::<LittleEndian>()?;

        let unambiguous_range_m = if prf_hz > 0.0 {
            299_792_458.0_f32 / (2.0 * prf_hz)
        } else {
            range_last_bin_cm as f32 / 100.0
        };

        // ----- TASK_SCAN_INFO -----
        // Layout (xradar lines 1773+):
        //   off 0   antenna_scan_mode  (UINT2)
        //   off 2   desired_angular_resolution (SINT2)
        //   off 4   spare_0 (2)
        //   off 6   sweep_number (SINT2)  ← total sweeps in volume
        //   off 8   scan_info (200 B, mode-dependent)
        c.set_position(TASK_SCAN_INFO_OFF);
        let scan_mode_raw = c.read_u16::<LittleEndian>()?;
        let scan_mode = match scan_mode_raw {
            1 | 4 => ScanMode::Ppi,
            2 => ScanMode::Rhi,
            other => ScanMode::Other(other),
        };

        c.set_position(TASK_SCAN_INFO_OFF + 6);
        let sweep_number_total = c.read_i16::<LittleEndian>()?;
        let sweeps_per_volume = sweep_number_total.max(0) as u16;

        c.set_position(TASK_SCAN_INFO_OFF + 8);
        let max_sweeps = sweeps_per_volume.min(40) as usize;
        let mut sweep_fixed_angles_deg = Vec::with_capacity(max_sweeps);
        for _ in 0..max_sweeps {
            let raw = c.read_u16::<LittleEndian>()?;
            sweep_fixed_angles_deg.push(bin2_to_degrees(raw) as f32);
        }

        // ----- TASK_END_INFO -----
        //   off 0   task_major_number (SINT2)
        //   off 2   task_minor_number (SINT2)
        //   off 4   task_configuration_file_name (string_dict(12))
        //   off 16  task_description (string_dict(80))
        //   …
        let task_name = read_fixed_string_at(buf, TASK_END_INFO_OFF as usize + 4, 12)?;

        Ok(Self {
            task_name,
            scan_mode,
            sweeps_per_volume,
            dsp_data_mask,
            nyquist_velocity_ms,
            prf_hz,
            wavelength_cm,
            unambiguous_range_m,
            range_first_bin_cm,
            range_last_bin_cm,
            step_output_bins_cm,
            bins_output,
            sweep_fixed_angles_deg,
        })
    }
}

/// `RAW_PROD_BHDR` (12 B) — appears at the start of every record carrying
/// sweep data, identifying which sweep / which ray-byte-offset within
/// the record.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(super) struct RawProdBhdr {
    pub record_number: i16,
    pub sweep_number: i16,
    pub first_ray_byte_offset: i16,
    pub sweep_ray_number: i16,
    pub flags: i16,
    pub spare: i16,
}

impl RawProdBhdr {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < 12 {
            return Err(RadishError::MalformedRecord {
                offset: 0,
                msg: "RAW_PROD_BHDR needs 12 bytes".to_string(),
            });
        }
        let mut c = Cursor::new(buf);
        Ok(Self {
            record_number: c.read_i16::<LittleEndian>()?,
            sweep_number: c.read_i16::<LittleEndian>()?,
            first_ray_byte_offset: c.read_i16::<LittleEndian>()?,
            sweep_ray_number: c.read_i16::<LittleEndian>()?,
            flags: c.read_i16::<LittleEndian>()?,
            spare: c.read_i16::<LittleEndian>()?,
        })
    }
}

/// `INGEST_DATA_HEADER` (76 B) — one per (sweep, moment) pair, recorded
/// at the start of the first record carrying that pair. Layout matches
/// xradar's `INGEST_DATA_HEADER` dict (`iris.py:1634`).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(super) struct IngestDataHeader {
    pub structure_header: StructureHeader,
    pub sweep_start_time: DateTime<Utc>,
    pub sweep_number: i16,
    pub number_rays_per_sweep: i16,
    pub first_ray_index: i16,
    pub number_rays_file_expected: i16,
    pub number_rays_file_written: i16,
    pub fixed_angle_deg: f32,
    pub bits_per_bin: i16,
    pub data_type: u8,
}

impl IngestDataHeader {
    pub fn parse(buf: &[u8]) -> Result<Self> {
        if buf.len() < 76 {
            return Err(RadishError::MalformedRecord {
                offset: 0,
                msg: format!("INGEST_DATA_HEADER too short: {} bytes", buf.len()),
            });
        }
        let structure_header = StructureHeader::parse(&buf[..STRUCTURE_HEADER_BYTES])?;

        // INGEST_DATA_HEADER contents after STRUCTURE_HEADER (cumulative
        // offsets within the 76-byte block):
        //   off 12  sweep_start_time (YMDS_TIME, 12 B)
        //   off 24  sweep_number             (SINT2)
        //   off 26  number_rays_per_sweep    (SINT2)
        //   off 28  first_ray_index          (SINT2)
        //   off 30  number_rays_file_expected(SINT2)
        //   off 32  number_rays_file_written (SINT2)
        //   off 34  fixed_angle              (BIN2)
        //   off 36  bits_per_bin             (SINT2)
        //   off 38  data_type                (UINT2 — low byte = id)
        //   off 40  spare_0                  (36 B)
        let mut c = Cursor::new(&buf[STRUCTURE_HEADER_BYTES..]);
        let sweep_start_time = read_ymds_time(&mut c)?;
        let sweep_number = c.read_i16::<LittleEndian>()?;
        let number_rays_per_sweep = c.read_i16::<LittleEndian>()?;
        let first_ray_index = c.read_i16::<LittleEndian>()?;
        let number_rays_file_expected = c.read_i16::<LittleEndian>()?;
        let number_rays_file_written = c.read_i16::<LittleEndian>()?;
        let fixed_angle_raw = c.read_u16::<LittleEndian>()?;
        let bits_per_bin = c.read_i16::<LittleEndian>()?;
        let data_type_u16 = c.read_u16::<LittleEndian>()?;

        Ok(Self {
            structure_header,
            sweep_start_time,
            sweep_number,
            number_rays_per_sweep,
            first_ray_index,
            number_rays_file_expected,
            number_rays_file_written,
            fixed_angle_deg: bin2_to_degrees(fixed_angle_raw) as f32,
            bits_per_bin,
            data_type: (data_type_u16 & 0xFF) as u8,
        })
    }
}

// ---- low-level helpers --------------------------------------------------

/// IRIS BIN4 angle: signed 4-byte value, scaled by 2^31 / 180°.
pub(super) fn bin4_to_degrees(raw: i32) -> f64 {
    raw as f64 * 180.0 / 2_147_483_648.0
}

/// IRIS BIN2 angle: unsigned 2-byte value, scaled by 2^16 / 360°.
pub(super) fn bin2_to_degrees(raw: u16) -> f64 {
    raw as f64 * 360.0 / 65_536.0
}

fn read_i16_le(buf: &mut &[u8]) -> Result<i16> {
    Ok(buf.read_i16::<LittleEndian>()?)
}

fn read_i32_le(buf: &mut &[u8]) -> Result<i32> {
    Ok(buf.read_i32::<LittleEndian>()?)
}

fn read_fixed_string<R: Read>(r: &mut R, len: usize) -> Result<String> {
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(decode_fixed_string(&buf))
}

fn read_fixed_string_at(buf: &[u8], offset: usize, len: usize) -> Result<String> {
    if offset + len > buf.len() {
        return Err(RadishError::MalformedRecord {
            offset: offset as u64,
            msg: format!("string at offset {offset} runs past buffer"),
        });
    }
    Ok(decode_fixed_string(&buf[offset..offset + len]))
}

fn decode_fixed_string(buf: &[u8]) -> String {
    let trimmed: Vec<u8> = buf.iter().take_while(|&&b| b != 0).copied().collect();
    String::from_utf8_lossy(&trimmed).trim_end().to_string()
}

/// Read a 12-byte `YMDS_TIME` and return UTC. Layout:
///   seconds (SINT4)
///   millis  (UINT2 — low 10 bits = milliseconds, top 6 bits are
///            DST/UTC/local flags we ignore, mirroring xradar)
///   year    (SINT2)
///   month   (SINT2)
///   day     (SINT2)
fn read_ymds_time<R: Read>(r: &mut R) -> Result<DateTime<Utc>> {
    let secs = r.read_i32::<LittleEndian>()?;
    let millis_and_flags = r.read_u16::<LittleEndian>()?;
    // Only the low 10 bits carry milliseconds (0..=999); the upper bits are
    // time-zone flags. Keeping the sub-second component matches xradar, which
    // otherwise reports e.g. `…:48.818` where we would land on whole seconds.
    let millis = (millis_and_flags & 0x3FF) as i64;
    let year = r.read_i16::<LittleEndian>()?;
    let month = r.read_i16::<LittleEndian>()?;
    let day = r.read_i16::<LittleEndian>()?;

    if year <= 0 || month <= 0 || day <= 0 {
        return Ok(DateTime::<Utc>::UNIX_EPOCH);
    }
    let date = NaiveDate::from_ymd_opt(year as i32, month as u32, day as u32).ok_or_else(|| {
        RadishError::MalformedRecord {
            offset: 0,
            msg: format!("invalid YMDS date: {year}-{month}-{day}"),
        }
    })?;
    let dt = date
        .and_hms_opt(0, 0, 0)
        .and_then(|d| d.checked_add_signed(chrono::Duration::seconds(secs as i64)))
        .and_then(|d| d.checked_add_signed(chrono::Duration::milliseconds(millis)));
    let dt = dt.ok_or_else(|| RadishError::MalformedRecord {
        offset: 0,
        msg: format!("invalid YMDS time: secs={secs}"),
    })?;
    Ok(Utc.from_utc_datetime(&dt))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structure_header_round_trip() {
        // Hand-craft an INGEST_HEADER STRUCTURE_HEADER:
        // id=23 (LE), version=4, bytes_in_structure=4884, reserved=0, flag=0
        let buf = [
            0x17, 0x00, 0x04, 0x00, 0x14, 0x13, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let sh = StructureHeader::parse(&buf).expect("parse");
        assert_eq!(sh.structure_identifier, 23);
        assert_eq!(sh.format_version, 4);
        assert_eq!(sh.bytes_in_structure, 4884);
    }

    #[test]
    fn structure_header_too_short_returns_error() {
        let buf = [0x17, 0x00, 0x04, 0x00];
        assert!(StructureHeader::parse(&buf).is_err());
    }

    #[test]
    fn read_ymds_time_keeps_milliseconds() {
        // YMDS_TIME: secs=168 (SINT4), millis_and_flags=818 (UINT2, low 10
        // bits = ms), year=2022, month=6, day=1 → 2022-06-01T00:02:48.818.
        let buf: [u8; 12] = [
            0xA8, 0x00, 0x00, 0x00, // secs = 168
            0x32, 0x03, // 818 in the low 10 bits, flags clear
            0xE6, 0x07, // year = 2022
            0x06, 0x00, // month = 6
            0x01, 0x00, // day = 1
        ];
        let mut c = Cursor::new(&buf[..]);
        let dt = read_ymds_time(&mut c).expect("parse YMDS");
        assert_eq!(dt.timestamp(), 1654041768); // 2022-06-01T00:02:48Z
        assert_eq!(dt.timestamp_subsec_millis(), 818);
    }

    #[test]
    fn read_ymds_time_masks_flag_bits() {
        // Same instant but with high flag bits set in millis_and_flags; only
        // the low 10 bits (818) should survive as milliseconds.
        let buf: [u8; 12] = [
            0xA8, 0x00, 0x00, 0x00, // secs = 168
            0x32, 0xC3, // 0xC332: flags in top bits, 818 in low 10 bits
            0xE6, 0x07, 0x06, 0x00, 0x01, 0x00,
        ];
        let mut c = Cursor::new(&buf[..]);
        let dt = read_ymds_time(&mut c).expect("parse YMDS");
        assert_eq!(dt.timestamp_subsec_millis(), 818);
    }

    #[test]
    fn bin4_to_degrees_pinned_values() {
        // BIN4 maps a signed i32 to degrees: full int32 range covers
        // ±180°. So raw=2^30 → 90°, raw=2^29 → 45°.
        assert_eq!(bin4_to_degrees(0), 0.0);
        assert!((bin4_to_degrees(1 << 30) - 90.0).abs() < 1e-9);
        assert!((bin4_to_degrees(1 << 29) - 45.0).abs() < 1e-9);
        assert!((bin4_to_degrees(-(1 << 29)) - (-45.0)).abs() < 1e-9);
    }

    #[test]
    fn bin2_to_degrees_pinned_values() {
        // raw=0 → 0°, raw=32768 → 180°, raw=49152 → 270°
        assert_eq!(bin2_to_degrees(0), 0.0);
        assert!((bin2_to_degrees(32768) - 180.0).abs() < 1e-6);
        assert!((bin2_to_degrees(49152) - 270.0).abs() < 1e-6);
    }

    #[test]
    fn decode_fixed_string_strips_nulls_and_trailing_whitespace() {
        assert_eq!(decode_fixed_string(b"hello\0\0\0"), "hello");
        assert_eq!(decode_fixed_string(b"hi   "), "hi");
        assert_eq!(decode_fixed_string(b""), "");
    }

    /// Parametrised mask-word coverage helper. Sets one bit in one
    /// mask word and asserts only that bit is set in the packed u128.
    /// The four mask words live at TASK_DSP_INFO + {4, 12, 16, 20}
    /// (with `extended_header_type` between word_0 and word_1); a
    /// regression that swaps any of those offsets — or zeroes one
    /// word out — fails this test.
    fn assert_mask_word_bit_round_trips(word_offset: usize, bit_in_word: u32, packed_bit: u32) {
        let mut buf = vec![0u8; TASK_CONFIGURATION_MIN_BYTES];
        // Place a single set bit in the chosen mask word.
        buf[word_offset..word_offset + 4].copy_from_slice(&(1u32 << bit_in_word).to_le_bytes());
        // Required minimum field setup for parse() to succeed.
        buf[772 + 10..772 + 12].copy_from_slice(&4u16.to_le_bytes()); // bins_output
        buf[932..934].copy_from_slice(&1u16.to_le_bytes()); // scan_mode = PPI
        buf[932 + 6..932 + 8].copy_from_slice(&1i16.to_le_bytes()); // sweep_number_total

        let task = TaskConfiguration::parse(&buf).expect("parse");
        let expected = 1u128 << packed_bit;
        assert_eq!(
            task.dsp_data_mask, expected,
            "word offset {word_offset}, bit {bit_in_word}: \
             expected only bit {packed_bit} set, got {:#x}",
            task.dsp_data_mask
        );
    }

    /// `TaskConfiguration::parse` must read all four DSP_DATA_MASK words
    /// and pack them into the u128 covering bits 0..127 — without this
    /// the walker silently misses moments with ID > 31 (DB_HCLASS=55,
    /// DB_DBTE8=71, DB_DBZE8=73 — common in real CHI fixtures).
    #[test]
    fn task_configuration_parses_dsp_mask_above_bit_31() {
        // Build a minimal 1892-byte TASK_CONFIGURATION buffer with:
        //   - mask_word_0 (offset 132+4)  = 0
        //   - extended_header_type        = 0
        //   - mask_word_1 (offset 132+12) = 0
        //   - mask_word_2 (offset 132+16) = 1<<7    (id 64+7 = 71, DB_DBTE8)
        //   - mask_word_3 (offset 132+20) = 0
        //   - bins_output (offset 772+10) = 4       (small value to avoid range_axis explosion)
        //   - antenna_scan_mode (off 932) = 1       (PPI)
        //   - sweep_number_total (off 932+6) = 1
        let mut buf = vec![0u8; 1892];
        let task_dsp_off = 132usize;
        // mask_word_2 at off TASK_DSP_INFO+16 → byte slice [..16+4]
        let mw2_off = task_dsp_off + 16;
        buf[mw2_off..mw2_off + 4].copy_from_slice(&(1u32 << 7).to_le_bytes());
        // bins_output as a non-zero u16 (RANGE_INFO offset 10).
        let bins_off = 772 + 10;
        buf[bins_off..bins_off + 2].copy_from_slice(&4u16.to_le_bytes());
        // scan_mode = 1 (PPI), sweep_number_total = 1
        buf[932..934].copy_from_slice(&1u16.to_le_bytes());
        buf[932 + 6..932 + 8].copy_from_slice(&1i16.to_le_bytes());

        let task = TaskConfiguration::parse(&buf).expect("parse");
        assert!(
            task.dsp_data_mask & (1u128 << 71) != 0,
            "bit 71 (DB_DBTE8) must be set in dsp_data_mask, got {:#x}",
            task.dsp_data_mask
        );
        // Bits in mask_word_0/1/3 should still be zero.
        assert_eq!(task.dsp_data_mask & ((1u128 << 64) - 1), 0);
        assert_eq!(task.dsp_data_mask & (((1u128 << 32) - 1) << 96), 0);
    }

    /// Each of the four mask words must round-trip correctly through
    /// `TaskConfiguration::parse`. This catches off-by-one bugs in the
    /// per-word offset constants (the original draft had only `mask_word_0`
    /// wired up, dropping bits 32..127 silently). Covers mask_word_0,
    /// mask_word_1, mask_word_2, and mask_word_3 with one set bit each.
    #[test]
    fn task_configuration_dsp_mask_covers_all_four_words() {
        // (word offset within TASK_DSP_INFO, bit within word, packed bit)
        // word_0 lives at TASK_DSP_INFO + 4; the +4 / +12 / +16 / +20
        // offsets match the `DSP_DATA_MASK` layout in xradar's
        // `iris.py:1370` (mask_word_0, extended_header_type,
        // mask_word_1, mask_word_2, mask_word_3).
        let cases: &[(usize, u32, u32)] = &[
            (132 + 4, 5, 5),    // mask_word_0  bit  5 → packed bit  5
            (132 + 12, 3, 35),  // mask_word_1  bit  3 → packed bit 35
            (132 + 16, 7, 71),  // mask_word_2  bit  7 → packed bit 71 (DB_DBTE8)
            (132 + 20, 4, 100), // mask_word_3  bit  4 → packed bit 100
        ];
        for &(word_off, bit, packed) in cases {
            assert_mask_word_bit_round_trips(word_off, bit, packed);
        }
    }

    /// `TaskConfiguration::parse` reads `wavelength` (SINT4, 1/100 cm) from
    /// the start of TASK_MISC_INFO (offset 1252) and derives the Nyquist
    /// velocity as `wavelength_raw * prf / 40000` (xradar's formula). A
    /// regression in the TASK_MISC_INFO offset or the formula fails here.
    #[test]
    fn task_configuration_reads_wavelength_and_derives_nyquist() {
        let mut buf = vec![0u8; TASK_CONFIGURATION_MIN_BYTES];
        // wavelength at TASK_MISC_INFO offset 1252 (533 → 5.33 cm).
        buf[1252..1256].copy_from_slice(&533i32.to_le_bytes());
        // prf at TASK_DSP_INFO + 136 = 268 (850 Hz).
        buf[268..272].copy_from_slice(&850i32.to_le_bytes());
        // Minimum field setup so parse() succeeds.
        buf[772 + 10..772 + 12].copy_from_slice(&4u16.to_le_bytes()); // bins_output
        buf[932..934].copy_from_slice(&1u16.to_le_bytes()); // scan_mode = PPI
        buf[932 + 6..932 + 8].copy_from_slice(&1i16.to_le_bytes()); // sweep_number_total

        let task = TaskConfiguration::parse(&buf).expect("parse");
        assert!((task.wavelength_cm - 5.33).abs() < 1e-4);
        // 533 * 850 / 40000 = 11.32625 m/s
        assert!((task.nyquist_velocity_ms - 11.326_25).abs() < 1e-3);
    }

    /// With no PRF the Nyquist velocity degrades to 0 rather than dividing by
    /// an unset field — the 8-bit VEL/WIDTH decoders then zero out cleanly.
    #[test]
    fn task_configuration_zero_prf_yields_zero_nyquist() {
        let mut buf = vec![0u8; TASK_CONFIGURATION_MIN_BYTES];
        buf[1252..1256].copy_from_slice(&533i32.to_le_bytes()); // wavelength
                                                                // prf left at 0.
        buf[772 + 10..772 + 12].copy_from_slice(&4u16.to_le_bytes());
        buf[932..934].copy_from_slice(&1u16.to_le_bytes());
        buf[932 + 6..932 + 8].copy_from_slice(&1i16.to_le_bytes());

        let task = TaskConfiguration::parse(&buf).expect("parse");
        assert_eq!(task.nyquist_velocity_ms, 0.0);
    }

    /// `IngestDataHeader::parse` must read all five SINT2 fields between
    /// `sweep_start_time` and `fixed_angle`. A previous version was off
    /// by one (missing `number_rays_file_written`) and would land
    /// `fixed_angle` two bytes too early.
    #[test]
    fn ingest_data_header_parses_all_sint2_fields() {
        // Hand-roll a 76-byte INGEST_DATA_HEADER:
        //   off 0:  STRUCTURE_HEADER (id=24 = INGEST_DATA_HEADER), 12 B
        //   off 12: YMDS_TIME = secs(0), millis(0), 2024, 6, 15
        //   off 24: sweep_number = 3
        //   off 26: number_rays_per_sweep = 360
        //   off 28: first_ray_index = 0
        //   off 30: number_rays_file_expected = 360
        //   off 32: number_rays_file_written = 359   ← this field is the one
        //                                              that was missing in v0
        //   off 34: fixed_angle (BIN2) = 8192        (= 45°)
        //   off 36: bits_per_bin = 8
        //   off 38: data_type = 2 (DB_DBZ)
        let mut buf = [0u8; 76];
        // STRUCTURE_HEADER: id=24, ver=2, len=76
        buf[0..2].copy_from_slice(&24i16.to_le_bytes());
        buf[2..4].copy_from_slice(&2i16.to_le_bytes());
        buf[4..8].copy_from_slice(&76i32.to_le_bytes());
        // YMDS_TIME at off 12
        buf[12..16].copy_from_slice(&0i32.to_le_bytes());
        buf[16..18].copy_from_slice(&0u16.to_le_bytes());
        buf[18..20].copy_from_slice(&2024i16.to_le_bytes());
        buf[20..22].copy_from_slice(&6i16.to_le_bytes());
        buf[22..24].copy_from_slice(&15i16.to_le_bytes());
        // SINT2 fields
        buf[24..26].copy_from_slice(&3i16.to_le_bytes());
        buf[26..28].copy_from_slice(&360i16.to_le_bytes());
        buf[28..30].copy_from_slice(&0i16.to_le_bytes());
        buf[30..32].copy_from_slice(&360i16.to_le_bytes());
        buf[32..34].copy_from_slice(&359i16.to_le_bytes());
        buf[34..36].copy_from_slice(&8192u16.to_le_bytes());
        buf[36..38].copy_from_slice(&8i16.to_le_bytes());
        buf[38..40].copy_from_slice(&2u16.to_le_bytes()); // data_type UINT2

        let idh = IngestDataHeader::parse(&buf).expect("parse");
        assert_eq!(idh.sweep_number, 3);
        assert_eq!(idh.number_rays_per_sweep, 360);
        assert_eq!(idh.first_ray_index, 0);
        assert_eq!(idh.number_rays_file_expected, 360);
        assert_eq!(idh.number_rays_file_written, 359);
        // 8192 BIN2 → 8192 * 360 / 65536 = 45.0
        assert!((idh.fixed_angle_deg - 45.0).abs() < 1e-3);
        assert_eq!(idh.bits_per_bin, 8);
        assert_eq!(idh.data_type, 2);
    }
}
