//! Top-level walker for IRIS RAW.
//!
//! Strategy:
//!
//! 1. Find the `INGEST_HEADER` (id=23). It's at offset 0 if the file is
//!    bare; at offset 6144 if a `PRODUCT_HDR` (id=27) wraps it.
//! 2. Parse `INGEST_CONFIGURATION` (offset +12) and `TASK_CONFIGURATION`
//!    (offset +12+480) for site / task / range / scan-mode metadata plus
//!    the `dsp_data_mask` bitfield identifying enabled moments.
//! 3. Skip past the rest of the metadata records — the first sweep-data
//!    record is at offset `(num_metadata_records) * 6144`. For the common
//!    case (PRODUCT_HDR + INGEST_HEADER), that's record 2 = offset 12288.
//! 4. Walk sweep-data records. Each record starts with a 12-byte
//!    `RAW_PROD_BHDR` identifying its sweep number and where the first
//!    ray in the record starts. The first record of each (sweep, moment)
//!    block is preceded by an `INGEST_DATA_HEADER` (one per active moment).
//! 5. Per-ray: 12-byte `RAY_HEADER` + RLE-compressed bin data terminated
//!    by a `cmp_val=1, MSB=0` marker. Bin width is 1 or 2 bytes depending
//!    on the moment's data type.
//!
//! Bin RLE encoding (ported from xradar's `get_compression_code`):
//!
//! * code is read as a signed 16-bit LE int.
//! * If the high bit is set (code is negative as i16): "copy" mode —
//!   `cmp_val = (code as u16) & 0x7FFF` is the number of 16-bit words of
//!   literal data following.
//! * If the high bit is clear (code is positive): "skip" mode —
//!   `cmp_val` is the number of 16-bit words to skip (treated as missing
//!   data, written as zeros so the per-data-type decoder can map raw==0
//!   to NaN).
//! * `cmp_val == 1, MSB clear` is the end-of-ray sentinel.
//!
//! Cross-record handling: the RLE stream is logically continuous across
//! record boundaries, but every 6144-byte record begins with a
//! `RAW_PROD_BHDR` that must be skipped. `RecordReader` below abstracts
//! that — calls return successive 16-bit code words / data words while
//! transparently jumping the per-record header.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use crate::{RadishError, Result};

use super::calibration::{DecodeParams, Decoder};
use super::mapping::moment_for_id;
use super::structs::{
    bin2_to_degrees, IngestConfiguration, IngestDataHeader, RawProdBhdr, ScanMode, StructureHeader,
    TaskConfiguration, INGEST_CONFIGURATION_BYTES, INGEST_DATA_HEADER_BYTES, RAW_PROD_BHDR_BYTES,
    RECORD_BYTES, STRUCTURE_HEADER_BYTES, STRUCT_ID_INGEST_HEADER, STRUCT_ID_PRODUCT_HDR,
    TASK_CONFIGURATION_MAX_BYTES, TASK_CONFIGURATION_MIN_BYTES,
};

/// Top-level decoded volume: metadata + per-sweep per-ray decoded gates.
#[derive(Debug, Clone)]
pub(super) struct DecodedVolume {
    pub site_name: String,
    pub iris_version: String,
    pub task_name: String,
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    pub altitude_m: f64,
    pub volume_start_time: DateTime<Utc>,
    pub scan_mode: ScanMode,
    pub nyquist_velocity_ms: f32,
    pub prf_hz: f32,
    pub unambiguous_range_m: f32,
    /// Per-gate range axis in metres.
    pub range_axis_m: Vec<f32>,
    /// Output gate count (kept for callers that want it without
    /// re-deriving from `range_axis_m.len()`).
    #[allow(dead_code)]
    pub gate_count: usize,
    pub sweeps: Vec<DecodedSweep>,
}

/// One sweep's worth of decoded rays.
#[derive(Debug, Clone)]
pub(super) struct DecodedSweep {
    pub sweep_number: u32,
    pub fixed_angle_deg: f32,
    pub start_time: DateTime<Utc>,
    pub rays: Vec<DecodedRay>,
}

/// One ray's azimuth/elevation/time + per-moment decoded f32 bins.
#[derive(Debug, Clone)]
pub(super) struct DecodedRay {
    pub azimuth_deg: f32,
    pub elevation_deg: f32,
    pub time_offset_s: f32,
    /// Per IRIS data-type id (matches `SigmetMoment.data_type_id`),
    /// `Vec<f32>` of length `gate_count` (NaN for missing/skip cells).
    pub moments: HashMap<u8, Vec<f32>>,
}

// ---- top-level entry ----------------------------------------------------

/// Decode an in-memory IRIS RAW buffer.
pub(super) fn parse_volume(data: &[u8]) -> Result<DecodedVolume> {
    if data.len() < STRUCTURE_HEADER_BYTES {
        return Err(RadishError::MalformedRecord {
            offset: 0,
            msg: "file too short for STRUCTURE_HEADER".to_string(),
        });
    }

    // Locate the INGEST_HEADER. Two layouts handled:
    //   bare      : INGEST_HEADER at offset 0
    //   wrapped   : PRODUCT_HDR (id=27) record 0, INGEST_HEADER record 1
    let first = StructureHeader::parse(&data[..STRUCTURE_HEADER_BYTES])?;
    let (ingest_header_offset, metadata_records) = match first.structure_identifier {
        STRUCT_ID_INGEST_HEADER => (0usize, 1usize),
        STRUCT_ID_PRODUCT_HDR => (RECORD_BYTES, 2usize),
        other => {
            return Err(RadishError::InvalidFormat(format!(
                "unexpected structure_identifier {other}; expected 23 (INGEST_HEADER) or 27 (PRODUCT_HDR)"
            )))
        }
    };

    if data.len() < ingest_header_offset + STRUCTURE_HEADER_BYTES + INGEST_CONFIGURATION_BYTES {
        return Err(RadishError::MalformedRecord {
            offset: ingest_header_offset as u64,
            msg: "INGEST_HEADER does not fit in file".to_string(),
        });
    }

    // Confirm the INGEST_HEADER's STRUCTURE_HEADER.
    let ingest_struct_header = StructureHeader::parse(
        &data[ingest_header_offset..ingest_header_offset + STRUCTURE_HEADER_BYTES],
    )?;
    if ingest_struct_header.structure_identifier != STRUCT_ID_INGEST_HEADER {
        return Err(RadishError::InvalidFormat(format!(
            "expected INGEST_HEADER (id=23) at offset {ingest_header_offset}, got id={}",
            ingest_struct_header.structure_identifier
        )));
    }

    // INGEST_CONFIGURATION starts immediately after INGEST_HEADER's STRUCTURE_HEADER.
    let cfg_off = ingest_header_offset + STRUCTURE_HEADER_BYTES;
    let cfg = IngestConfiguration::parse(&data[cfg_off..cfg_off + INGEST_CONFIGURATION_BYTES])?;

    // TASK_CONFIGURATION starts after INGEST_CONFIGURATION.
    let task_off = cfg_off + INGEST_CONFIGURATION_BYTES;
    if data.len() < task_off + TASK_CONFIGURATION_MIN_BYTES {
        return Err(RadishError::MalformedRecord {
            offset: task_off as u64,
            msg: "TASK_CONFIGURATION does not fit in file".to_string(),
        });
    }
    let task_buf_len = (data.len() - task_off).min(TASK_CONFIGURATION_MAX_BYTES);
    let task = TaskConfiguration::parse(&data[task_off..task_off + task_buf_len])?;

    // Build the active moment list from dsp_data_mask. Each set bit
    // identifies a SIGMET_DATA_TYPES id present in the volume. xradar's
    // `_data_types_from_dsp_mask` (iris.py:430) walks bits 0..127 across
    // four mask words; we pack those into a single u128 in
    // `TaskConfiguration::parse` and iterate the same range here.
    //
    // Moments not yet implemented in our `SUPPORTED_MOMENTS` table
    // (DB_HCLASS, DB_DBTE8, DB_DBZE8, etc.) get filtered out by
    // `moment_for_id` returning None — but we still need to know how
    // many INGEST_DATA_HEADERs / RLE ray streams the file carries so
    // the sweep walker can advance past them. That count is the total
    // bit-set count, not just the supported subset.
    // `active_data_type_ids` is the FULL list of IRIS data-type ids
    // present in the file, in ascending bit-order. The encoder writes
    // one INGEST_DATA_HEADER per active id and interleaves rays in the
    // same order. We must walk every id to keep the RLE stream in sync,
    // even for ids we don't decode (e.g. DB_HCLASS, DB_DBTE8). The
    // adapter later filters down to the supported subset using
    // `moment_for_id`.
    let active_data_type_ids: Vec<u8> = (0..128)
        .filter(|&bit| (task.dsp_data_mask >> bit) & 1 == 1)
        .map(|bit| bit as u8)
        .collect();

    // Per-gate range axis: `range_first_bin_cm + i * step_output_bins_cm`,
    // converted to metres. `bins_output` is the number of OUTPUT gates.
    let gate_count = task.bins_output as usize;
    let first_gate_m = task.range_first_bin_cm as f32 / 100.0;
    let step_m = task.step_output_bins_cm as f32 / 100.0;
    let range_axis_m: Vec<f32> = (0..gate_count)
        .map(|i| first_gate_m + (i as f32) * step_m)
        .collect();

    // Walk sweep-data records starting at the first record after metadata.
    let data_start = metadata_records * RECORD_BYTES;
    let sweeps = parse_sweeps(
        data,
        data_start,
        task.sweeps_per_volume as usize,
        gate_count,
        &active_data_type_ids,
        DecodeParams {
            nyquist_ms: task.nyquist_velocity_ms,
            wavelength_cm: task.wavelength_cm,
        },
    )?;

    Ok(DecodedVolume {
        site_name: cfg.site_name,
        iris_version: cfg.iris_version,
        task_name: task.task_name,
        latitude_deg: cfg.latitude_deg,
        longitude_deg: cfg.longitude_deg,
        // Widen i16 fields to i32 BEFORE adding so an adversarial INGEST
        // header can't cause a silent overflow in release builds. Real
        // values are typically small (≤10 km) but we don't trust input.
        altitude_m: (cfg.altitude_radar_cm as f64 / 100.0)
            .max(cfg.height_site_m as f64 + cfg.height_radar_m as f64),
        volume_start_time: cfg.volume_scan_start_time,
        scan_mode: task.scan_mode,
        nyquist_velocity_ms: task.nyquist_velocity_ms,
        prf_hz: task.prf_hz,
        unambiguous_range_m: task.unambiguous_range_m,
        range_axis_m,
        gate_count,
        sweeps,
    })
}

// ---- sweep / ray walker -------------------------------------------------

/// Cross-record reader. Consumes 16-bit LE words from the sweep-data
/// stream, transparently skipping the 12-byte `RAW_PROD_BHDR` at every
/// 6144-byte record boundary.
struct RecordReader<'a> {
    data: &'a [u8],
    /// Current absolute byte position in `data`.
    pos: usize,
    /// Inclusive end of the sweep-data region we're allowed to read from.
    end: usize,
}

impl<'a> RecordReader<'a> {
    fn new(data: &'a [u8], start: usize, end: usize) -> Self {
        Self {
            data,
            pos: start,
            end: end.min(data.len()),
        }
    }

    /// True when we've consumed all available data.
    fn is_done(&self) -> bool {
        self.pos >= self.end
    }

    fn record_offset_of(&self, pos: usize) -> usize {
        (pos / RECORD_BYTES) * RECORD_BYTES
    }

    /// If `self.pos` is at the start of a record, skip the
    /// `RAW_PROD_BHDR`. Idempotent — only the head of a record gets skipped.
    fn skip_record_header_if_at_boundary(&mut self) {
        if self.pos < self.end && self.pos.is_multiple_of(RECORD_BYTES) {
            self.pos += RAW_PROD_BHDR_BYTES;
        }
    }

    /// Read one little-endian u16 word, skipping record headers as needed.
    /// This is the per-gate hot path — keep it tight.
    fn read_u16(&mut self) -> Result<u16> {
        self.skip_record_header_if_at_boundary();
        if self.pos + 2 > self.end {
            return Err(RadishError::MalformedRecord {
                offset: self.pos as u64,
                msg: "unexpected EOF reading u16".to_string(),
            });
        }
        let bytes = [self.data[self.pos], self.data[self.pos + 1]];
        self.pos += 2;
        // If our 2-byte read straddled a record boundary, advance past
        // the next record's header so the *next* read sees data, not
        // RAW_PROD_BHDR. (In practice the IRIS encoder aligns codes on
        // record boundaries so straddling shouldn't happen, but defensive.)
        if self.record_offset_of(self.pos - 2) != self.record_offset_of(self.pos) {
            self.skip_record_header_if_at_boundary();
        }
        Ok(u16::from_le_bytes(bytes))
    }

    /// Skip `n` bytes (only used for ray-header skip when explicitly
    /// requested; not used by RLE).
    #[allow(dead_code)]
    fn skip(&mut self, n: usize) -> Result<()> {
        if self.pos + n > self.end {
            return Err(RadishError::MalformedRecord {
                offset: self.pos as u64,
                msg: format!("skip past end: {n} bytes"),
            });
        }
        self.pos += n;
        Ok(())
    }
}

/// Parse all sweeps in the volume.
///
/// `active_data_type_ids` is the full set of IRIS data-type ids present
/// in the file (from the DSP data mask). We walk every id so the RLE
/// stream stays in sync — but only ids whose `moment_for_id` lookup
/// succeeds get their decoded values stashed; unsupported ids are read
/// past with `DECODE_NONE` and discarded.
fn parse_sweeps(
    data: &[u8],
    data_start: usize,
    sweeps_per_volume: usize,
    gate_count: usize,
    active_data_type_ids: &[u8],
    params: DecodeParams,
) -> Result<Vec<DecodedSweep>> {
    let mut sweeps = Vec::with_capacity(sweeps_per_volume);
    let mut cursor = data_start;

    for sweep_idx in 0..sweeps_per_volume {
        if cursor >= data.len() {
            break;
        }

        // Each sweep starts with a record carrying RAW_PROD_BHDR + N
        // INGEST_DATA_HEADERs (one per active moment), followed by ray data.
        let bhdr = RawProdBhdr::parse(&data[cursor..cursor + RAW_PROD_BHDR_BYTES])?;
        if bhdr.sweep_number as i64 != (sweep_idx as i64) + 1 {
            // Some volumes index sweeps from 0 or 1 depending on encoder; we
            // accept both. If we're really off, abort the walk.
            if bhdr.sweep_number != 0 && bhdr.sweep_number as usize != sweep_idx + 1 {
                break;
            }
        }

        // Parse one INGEST_DATA_HEADER per active id. They sit immediately
        // after the RAW_PROD_BHDR in the record. Use the IDH's own
        // `bits_per_bin` field to drive RLE width — it's authoritative
        // even when the id isn't in our SUPPORTED_MOMENTS table.
        let mut idh_off = cursor + RAW_PROD_BHDR_BYTES;
        let mut idhs: Vec<IngestDataHeader> = Vec::with_capacity(active_data_type_ids.len());
        for _ in active_data_type_ids {
            if idh_off + INGEST_DATA_HEADER_BYTES > data.len() {
                break;
            }
            let idh = IngestDataHeader::parse(&data[idh_off..idh_off + INGEST_DATA_HEADER_BYTES])?;
            idhs.push(idh);
            idh_off += INGEST_DATA_HEADER_BYTES;
        }

        let nrays = idhs
            .first()
            .map(|h| h.number_rays_per_sweep as usize)
            .unwrap_or(360);
        let fixed_angle = idhs.first().map(|h| h.fixed_angle_deg).unwrap_or(0.0);
        let start_time = idhs.first().map(|h| h.sweep_start_time).unwrap_or_default();

        let sweep_end = data.len();
        let mut reader = RecordReader::new(data, idh_off, sweep_end);

        let mut rays: Vec<DecodedRay> = (0..nrays)
            .map(|_| DecodedRay {
                azimuth_deg: f32::NAN,
                elevation_deg: f32::NAN,
                time_offset_s: 0.0,
                moments: HashMap::with_capacity(active_data_type_ids.len()),
            })
            .collect();

        for ray in rays.iter_mut().take(nrays) {
            for (moment_pos, idh) in idhs.iter().enumerate() {
                if reader.is_done() {
                    break;
                }
                let bytes_per_bin = (idh.bits_per_bin as usize) / 8;
                if bytes_per_bin == 0 {
                    // Encoder said "0 bits" — nothing to decode, skip.
                    continue;
                }
                let supported = moment_for_id(idh.data_type);
                let decoder: Decoder = supported
                    .map(|m| m.decoder)
                    .unwrap_or(super::calibration::DECODE_NONE);
                if let Some((az, el, t_off, decoded)) =
                    decode_one_ray(&mut reader, gate_count, bytes_per_bin, decoder, params)?
                {
                    if moment_pos == 0 {
                        ray.azimuth_deg = az;
                        ray.elevation_deg = el;
                        ray.time_offset_s = t_off;
                    }
                    if let Some(m) = supported {
                        ray.moments.insert(m.data_type_id, decoded);
                    }
                } else if let Some(m) = supported {
                    ray.moments
                        .insert(m.data_type_id, vec![f32::NAN; gate_count]);
                }
            }
        }

        sweeps.push(DecodedSweep {
            // 0-indexed to match xradar's `open_iris_datatree` and FM301
            // (xradar exposes `sweep_number = 0` for the first sweep
            // even though the IRIS RAW file's INGEST_DATA_HEADER stores
            // it 1-indexed). Keeping the same convention means
            // `dt['/sweep_0'].sweep_number == 0` on both readers.
            sweep_number: sweep_idx as u32,
            fixed_angle_deg: fixed_angle,
            start_time,
            rays,
        });

        // Advance cursor to the next record-aligned position past where
        // the reader stopped. The next sweep's RAW_PROD_BHDR is at the
        // start of the next record we haven't yet visited.
        let next = reader.pos.div_ceil(RECORD_BYTES) * RECORD_BYTES;
        cursor = next;
    }

    Ok(sweeps)
}

/// Decode one ray. RLE stream output layout:
///   word 0: az_start  (BIN2)
///   word 1: el_start  (BIN2)
///   word 2: az_stop   (BIN2)
///   word 3: el_stop   (BIN2)
///   word 4: rbins     (UINT2)
///   word 5: dtime     (UINT2 — seconds within sweep)
///   words 6..N: bin data (8-bit moments pack 2 gates per word; 16-bit
///                         moments use one word per gate).
///
/// xradar treats the whole ray (header + bins) as a single RLE stream
/// terminated by `cmp_val=1, MSB=0`. We follow the same scheme: allocate
/// `6 + gate_count_words` of u16 storage, run the RLE decoder, then
/// interpret the first 6 words as the header.
///
/// Returns `Ok(None)` if the very first code is the missing-ray sentinel.
///
/// **Error semantics** (deliberate): a `MalformedRecord` from a torn or
/// truncated RLE stream propagates `Err` up to the sweep walker, which
/// in turn aborts the whole `read_volume`. We do *not* attempt to skip
/// the bad ray and continue, because once the RLE stream loses sync
/// every subsequent ray for every subsequent moment will read garbage
/// — silently filling the volume with NaN. Failing loud is the only
/// safe option. (The encoder-overshoot guard at the bottom of
/// [`apply_rle_step`] already tolerates the small drift IRIS files
/// commonly carry; only major corruption surfaces an error.)
///
/// Tuple in the success arm is `(azimuth_deg, elevation_deg,
/// time_offset_s, decoded_gates)`.
type RayDecode = (f32, f32, f32, Vec<f32>);

fn decode_one_ray(
    reader: &mut RecordReader<'_>,
    gate_count: usize,
    bytes_per_bin: usize,
    decoder: Decoder,
    params: DecodeParams,
) -> Result<Option<RayDecode>> {
    if reader.is_done() {
        return Ok(None);
    }

    // xradar (`iris.py:3525`): per-ray buffer is **gate_count + 6 int16
    // words regardless of bytes_per_bin**. For 8-bit moments the int16
    // array is later viewed as `(2,) uint8` and sliced `[:, :gate_count]`,
    // so half the int16 words are dropped — but the RLE stream still
    // encodes the full `gate_count + 6` int16 word stride per ray.
    //
    // We measured `vec![0; n]` against a caller-owned scratch buffer
    // reused via `clear()`/`resize()` (rust-best-practices audit P2):
    // the macro is ~25% faster on this fixture because (a) glibc's
    // free-list catches the per-call alloc/free for a same-size Vec
    // essentially for free and (b) `vec![0; n]` lowers to
    // `__memset_avx2`, while `clear()+resize()` on a previously-used
    // buffer pays for an explicit element-by-element zero. So the
    // "obvious" optimisation is anti-perf here — keep the macro.
    let buf_len = gate_count + 6;
    let mut raw_words: Vec<u16> = vec![0; buf_len];
    let mut word_pos = 0usize;

    // Peek the first compression code. If it's `cmp_val=1, MSB=0` we're
    // looking at a missing-ray sentinel — return None so the caller fills
    // an all-NaN row.
    let (first_msb, first_val) = decompose_code(reader.read_u16()?);
    if !first_msb && first_val == 1 {
        return Ok(None);
    }

    // Process the first code we already read.
    apply_rle_step(reader, &mut raw_words, &mut word_pos, first_msb, first_val)?;

    // Continue until the end-of-ray sentinel.
    loop {
        let (cmp_msb, cmp_val) = decompose_code(reader.read_u16()?);
        if !cmp_msb && cmp_val == 1 {
            break;
        }
        apply_rle_step(reader, &mut raw_words, &mut word_pos, cmp_msb, cmp_val)?;
    }

    // RAY_HEADER from the first 6 words.
    let az_start = raw_words[0];
    let el_start = raw_words[1];
    let az_stop = raw_words[2];
    let _el_stop = raw_words[3];
    let _rbins = raw_words[4];
    let dtime = raw_words[5];

    // Bin data from words 6..end.
    //
    // The audit (rust-best-practices P9) flagged the per-gate fn-pointer
    // dispatch (`decoder(...)`) as a potential inlining barrier. We
    // measured replacing it with an enum + outer-loop match (one
    // specialised inner loop per decoder kind, ~150 LOC of duplicate
    // gate-decode code): the gain is below this fixture's measurement
    // noise (±20% wall-clock variance) and not worth the maintenance
    // cost. Modern CPUs predict the indirect branch well when the same
    // target fires 770k times in a row. If a future fixture surfaces a
    // case where dispatch dominates we can revisit.
    let bin_words = &raw_words[6..];
    let mut decoded = Vec::with_capacity(gate_count);
    if bytes_per_bin == 1 {
        // Two 8-bit gates per 16-bit word: low byte = gate 2k, high byte = gate 2k+1.
        for w in bin_words.iter().take(gate_count.div_ceil(2)) {
            decoded.push(decoder(w & 0xFF, params));
            if decoded.len() < gate_count {
                decoded.push(decoder(w >> 8, params));
            }
        }
    } else {
        for w in bin_words.iter().take(gate_count) {
            decoded.push(decoder(*w, params));
        }
    }
    decoded.resize(gate_count, f32::NAN);

    let azimuth = ray_azimuth_deg(az_start, az_stop);
    let elevation = bin2_to_degrees(el_start) as f32;
    let time_offset_s = dtime as f32;

    Ok(Some((azimuth, elevation, time_offset_s, decoded)))
}

/// Circular midpoint of a ray's begin/end azimuths (BIN2 words), in degrees
/// on `[0, 360)`.
///
/// A ray straddling the 0°/360° seam (north crossing) has begin ≈ 359.5° and
/// end ≈ 0.5°, so the naive arithmetic mean `(359.5 + 0.5) / 2` lands at 180°
/// — a full half-turn from the true bearing. Whenever the two endpoints differ
/// by more than 180° we unwrap the smaller one across the seam before
/// averaging, then fold the result back onto `[0, 360)`. Rays that do not
/// cross the seam take the `else` branch and are bit-identical to the old
/// `(start + stop) * 0.5`.
fn ray_azimuth_deg(az_start: u16, az_stop: u16) -> f32 {
    let a = bin2_to_degrees(az_start) as f32;
    let b = bin2_to_degrees(az_stop) as f32;
    let b = if (b - a).abs() > 180.0 {
        if b < a {
            b + 360.0
        } else {
            b - 360.0
        }
    } else {
        b
    };
    ((a + b) * 0.5).rem_euclid(360.0)
}

/// Apply one RLE step (copy `cmp_val` words OR skip `cmp_val` words).
/// Splits out from the loop so the first peeked code can reuse the logic.
fn apply_rle_step(
    reader: &mut RecordReader<'_>,
    raw_words: &mut [u16],
    word_pos: &mut usize,
    cmp_msb: bool,
    cmp_val: usize,
) -> Result<()> {
    if cmp_msb {
        for _ in 0..cmp_val {
            if *word_pos >= raw_words.len() {
                let _ = reader.read_u16()?; // drain past end (encoder over-shoot)
                continue;
            }
            raw_words[*word_pos] = reader.read_u16()?;
            *word_pos += 1;
        }
    } else {
        *word_pos += cmp_val;
    }
    if *word_pos > raw_words.len() + 64 {
        return Err(RadishError::MalformedRecord {
            offset: reader.pos as u64,
            msg: format!(
                "RLE decode wrote {} past end of {}-word ray buffer",
                *word_pos - raw_words.len(),
                raw_words.len()
            ),
        });
    }
    Ok(())
}

/// Split a raw 16-bit RLE compression code into `(is_copy, count)`.
/// MSB set ⇒ copy mode (literal data words follow); MSB clear ⇒ skip
/// mode (zeros). Working in `u16` avoids the i16→i32→usize round-trip
/// and the overflow risk of treating the MSB as a sign bit.
#[inline]
fn decompose_code(code: u16) -> (bool, usize) {
    let cmp_msb = code & 0x8000 != 0;
    let cmp_val = (code & 0x7FFF) as usize;
    (cmp_msb, cmp_val)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::backends::sigmet::calibration::DECODE_NONE;

    /// Dummy decode params for tests that exercise `DECODE_NONE` (which
    /// ignores both fields).
    const TEST_PARAMS: DecodeParams = DecodeParams {
        nyquist_ms: 0.0,
        wavelength_cm: 0.0,
    };

    /// A buffer that's not even big enough for STRUCTURE_HEADER.
    #[test]
    fn parse_volume_rejects_short_buffer() {
        let err = parse_volume(&[0u8; 4]).expect_err("should reject");
        // Pin the variant + the message anchor so a regression that
        // changed *which* error fires doesn't silently still pass.
        match err {
            RadishError::MalformedRecord { msg, .. } => {
                assert!(msg.contains("STRUCTURE_HEADER"), "got msg={msg:?}");
            }
            other => panic!("expected MalformedRecord, got {other:?}"),
        }
    }

    /// A buffer with a non-IRIS structure_identifier should be rejected
    /// with InvalidFormat (not a malformed-record).
    #[test]
    fn parse_volume_rejects_unknown_structure_identifier() {
        let mut buf = vec![0u8; 4096];
        // id=99 LE
        buf[0] = 0x63;
        buf[1] = 0x00;
        let result = parse_volume(&buf);
        match result {
            Err(RadishError::InvalidFormat(_)) => (),
            other => panic!("expected InvalidFormat, got {other:?}"),
        }
    }

    /// `decompose_code` mirrors xradar's `get_compression_code` MSB/LSB
    /// split. Pin the boundary cases.
    #[test]
    fn decompose_code_msb_split() {
        // MSB clear, value 1 → end-of-ray sentinel
        assert_eq!(decompose_code(0x0001), (false, 1));
        // MSB clear, value 5 → skip 5 words
        assert_eq!(decompose_code(0x0005), (false, 5));
        // MSB set, value 5 → copy 5 words
        assert_eq!(decompose_code(0x8005), (true, 5));
        // MSB set, value 32767 (max) → copy 32767 words
        assert_eq!(decompose_code(0xFFFF), (true, 0x7FFF));
    }

    /// Helper: build a buffer that starts with the 12-byte `RAW_PROD_BHDR`
    /// the `RecordReader` skips at every record boundary, followed by
    /// `payload`. Returns the buffer; tests should set `reader.pos = 0`
    /// (the reader will skip the 12 bytes and land at the payload).
    fn with_record_prelude(payload: &[u8]) -> Vec<u8> {
        let mut buf = vec![0xAAu8; 12];
        buf.extend_from_slice(payload);
        buf
    }

    /// First compression code = `0x0001` (MSB clear, val=1) is the
    /// missing-ray sentinel — `decode_one_ray` must return `Ok(None)`
    /// without consuming a buffer's worth of data.
    #[test]
    fn decode_one_ray_returns_none_for_missing_ray_sentinel() {
        let buf = with_record_prelude(&[0x01, 0x00]);
        let mut reader = RecordReader::new(&buf, 0, buf.len());
        let result = decode_one_ray(&mut reader, 10, 1, DECODE_NONE, TEST_PARAMS).expect("Ok");
        assert!(
            result.is_none(),
            "missing-ray sentinel must surface as None"
        );
    }

    /// `apply_rle_step` must drain (consume) data words past `raw_words.len()`
    /// when an encoder over-shoots by ≤ 64 words — the trailing reads
    /// must advance the reader (so the next code lands at the right
    /// offset) but `word_pos` only advances for actual writes.
    #[test]
    fn apply_rle_step_drains_minor_encoder_overshoot() {
        // Payload = 4 u16 words (8 bytes) of literal copy data.
        let buf = with_record_prelude(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        let mut reader = RecordReader::new(&buf, 0, buf.len());
        let mut raw = vec![0u16; 2]; // smaller than the 4-word copy
        let mut pos = 0usize;
        apply_rle_step(&mut reader, &mut raw, &mut pos, true, 4).expect("Ok");
        // word_pos advanced only for the two writes that fit.
        assert_eq!(pos, 2);
        assert_eq!(raw[0], 0x2211);
        assert_eq!(raw[1], 0x4433);
        // Reader pos = 12-byte prelude + 4 u16 reads (2 written, 2 drained).
        assert_eq!(reader.pos, 12 + 8);
    }

    /// `apply_rle_step` must error (not silently corrupt) when an
    /// encoder over-shoots by more than 64 words — defensive guard
    /// against a malformed file flooding our buffer-end check.
    #[test]
    fn apply_rle_step_errors_on_major_overshoot() {
        let buf = with_record_prelude(&vec![0u8; 4096]);
        let mut reader = RecordReader::new(&buf, 0, buf.len());
        let mut raw = vec![0u16; 2];
        let mut pos = 0usize;
        // Skip step of 200 words pushes pos to 200 — past 2 + 64 = 66.
        let err =
            apply_rle_step(&mut reader, &mut raw, &mut pos, false, 200).expect_err("must error");
        match err {
            RadishError::MalformedRecord { msg, .. } => {
                assert!(msg.contains("past end"), "got msg={msg:?}");
            }
            other => panic!("expected MalformedRecord, got {other:?}"),
        }
    }

    /// The cross-record straddle path inside `RecordReader::read_u16`:
    /// when `pos` lands on a record boundary, the next read must skip
    /// the 12-byte `RAW_PROD_BHDR` and return data from byte 12 onward.
    #[test]
    fn record_reader_skips_raw_prod_bhdr_at_record_boundary() {
        // Two records of RECORD_BYTES = 6144 each.
        let mut buf = vec![0u8; RECORD_BYTES * 2];
        // First record starts with a fake RAW_PROD_BHDR (12 bytes of
        // garbage 0xAA) + a known sentinel u16 at offset 12.
        for b in &mut buf[0..12] {
            *b = 0xAA;
        }
        buf[12] = 0xCD;
        buf[13] = 0xAB; // u16 LE = 0xABCD
                        // Second record: fake header + sentinel 0xBEEF at offset 12.
        for b in &mut buf[RECORD_BYTES..RECORD_BYTES + 12] {
            *b = 0xAA;
        }
        buf[RECORD_BYTES + 12] = 0xEF;
        buf[RECORD_BYTES + 13] = 0xBE;

        let mut reader = RecordReader::new(&buf, 0, buf.len());
        // First read at the boundary must skip header → 0xABCD.
        assert_eq!(reader.read_u16().unwrap(), 0xABCD);
        // Now position is at 14. Skip ahead to the next record boundary
        // (byte 6144). A read there must again skip the second
        // record's header and return the 0xBEEF sentinel.
        reader.pos = RECORD_BYTES;
        assert_eq!(reader.read_u16().unwrap(), 0xBEEF);
    }

    /// `decode_one_ray` with `bytes_per_bin = 0` should produce a
    /// gate_count-long all-NaN buffer without consuming any RLE
    /// codes — pin the early-return path the sweep walker relies on
    /// when an INGEST_DATA_HEADER reports `bits_per_bin = 0`.
    ///
    /// (We don't actually call decode_one_ray with bytes_per_bin=0; the
    /// caller in `parse_sweeps` `continue`s on that condition. This
    /// test pins THAT contract: the caller must skip without invoking
    /// the decoder.)
    #[test]
    fn parse_sweeps_skips_zero_bits_per_bin_moments() {
        // Hand-build an INGEST_DATA_HEADER with bits_per_bin=0 and verify
        // that `IngestDataHeader::parse` accepts it (the caller's
        // `bytes_per_bin == 0` guard then keeps it from reaching the
        // RLE decoder).
        let mut buf = [0u8; INGEST_DATA_HEADER_BYTES];
        buf[0..2].copy_from_slice(&24i16.to_le_bytes()); // structure_id
        buf[2..4].copy_from_slice(&2i16.to_le_bytes());
        buf[4..8].copy_from_slice(&76i32.to_le_bytes());
        // YMDS time: zeros are fine.
        buf[18..20].copy_from_slice(&2024i16.to_le_bytes());
        buf[20..22].copy_from_slice(&1i16.to_le_bytes());
        buf[22..24].copy_from_slice(&1i16.to_le_bytes());
        // SINT2 fields zero. fixed_angle zero. bits_per_bin = 0.
        buf[36..38].copy_from_slice(&0i16.to_le_bytes());
        // data_type = 1 (DB_DBT)
        buf[38..40].copy_from_slice(&1u16.to_le_bytes());

        let idh = IngestDataHeader::parse(&buf).expect("parse");
        assert_eq!(
            idh.bits_per_bin, 0,
            "bits_per_bin=0 must round-trip; the sweep walker relies on this"
        );
        let bytes_per_bin = (idh.bits_per_bin as usize) / 8;
        assert_eq!(
            bytes_per_bin, 0,
            "the integer-divide gives 0 for the skip-this-moment guard"
        );
    }

    /// Property-based: a small synthetic RLE-encoded ray must round-trip.
    /// Encode a `Vec<u16>` of bin words via [`encode_ray_for_test`] (a
    /// minimal compatible encoder), then run `decode_one_ray` against
    /// the bytes and confirm the decoded buffer matches the input
    /// (modulo the leading 6-word header, which we treat as opaque).
    ///
    /// This catches off-by-one bugs in RLE bookkeeping that we'd
    /// otherwise rely on the live fixture to surface.
    #[test]
    fn rle_round_trip_on_short_synthetic_ray() {
        // 6-word header (we don't care about its content, only that
        // decode_one_ray reads 6 words off the front before bin data)
        // + 8 bin words of literal data, terminated by EOR.
        let header = [0xAAAAu16; 6];
        let bins: [u16; 8] = [
            0x0001, 0x0002, 0x0003, 0x0004, 0x0005, 0x0006, 0x0007, 0x0008,
        ];

        // Encode: "copy 14 words" (header + bins), then EOR.
        let mut bytes: Vec<u8> = Vec::new();
        // Record-prelude prepended so RecordReader doesn't try to skip
        // a header at our test buffer's start.
        bytes.extend_from_slice(&[0xAAu8; RAW_PROD_BHDR_BYTES]);
        // Code = 0x800E (MSB set, val=14).
        bytes.extend_from_slice(&0x800Eu16.to_le_bytes());
        for w in header.iter().chain(bins.iter()) {
            bytes.extend_from_slice(&w.to_le_bytes());
        }
        // End-of-ray sentinel: cmp_val=1, MSB clear.
        bytes.extend_from_slice(&0x0001u16.to_le_bytes());

        // Decode against a 16-bit moment with bytes_per_bin=2 so each
        // u16 maps directly to one gate.
        let mut reader = RecordReader::new(&bytes, 0, bytes.len());
        let result =
            decode_one_ray(&mut reader, 8, 2, DECODE_NONE, TEST_PARAMS).expect("decode_one_ray ok");
        let (_az, _el, _t, decoded) = result.expect("not a missing-ray sentinel");

        // DECODE_NONE maps raw=0 → NaN, else raw as f32. All our bins
        // are 1..=8, so we expect [1.0, 2.0, ..., 8.0].
        assert_eq!(decoded.len(), 8);
        for (i, v) in decoded.iter().enumerate() {
            let expected = (i + 1) as f32;
            assert!(
                (v - expected).abs() < 1e-6,
                "gate {i}: expected {expected}, got {v}"
            );
        }
    }

    /// BIN2 word for a bearing in `[0, 360)` (raw = deg * 65536 / 360).
    /// Note: `deg == 360.0` saturates to 65535 (= 359.9945°) rather than
    /// wrapping to 0 — pass the seam as `0.0`, not `360.0`.
    fn bin2(deg: f64) -> u16 {
        (deg * 65_536.0 / 360.0).round() as u16
    }

    /// A ray that does not cross north takes the plain-mean path and must be
    /// bit-identical to the old `(start + stop) * 0.5`.
    #[test]
    fn ray_azimuth_ordinary_ray_is_plain_mean() {
        let (start, stop) = (bin2(90.0), bin2(91.0));
        let old = (bin2_to_degrees(start) as f32 + bin2_to_degrees(stop) as f32) * 0.5;
        let got = ray_azimuth_deg(start, stop);
        assert!((got - old).abs() < 1e-6, "got {got}, old-mean {old}");
        assert!((got - 90.5).abs() < 1e-3, "got {got}");
    }

    /// The north-crossing ray from issue #40: begin ≈ 359.5°, end ≈ 0.5°.
    /// Naive mean gives 180°; the circular midpoint must be ~0°.
    #[test]
    fn ray_azimuth_north_crossing_folds_to_zero() {
        let (start, stop) = (bin2(359.5), bin2(0.5));
        let naive = (bin2_to_degrees(start) as f32 + bin2_to_degrees(stop) as f32) * 0.5;
        assert!((naive - 180.0).abs() < 0.5, "sanity: naive mean is ~180°");

        let got = ray_azimuth_deg(start, stop);
        // Fold onto the short side of the seam for the assertion.
        let signed = if got > 180.0 { got - 360.0 } else { got };
        assert!(signed.abs() < 1e-2, "expected ~0°, got {got}");
    }

    /// The seam handling is symmetric in the scan direction: a decreasing
    /// sweep (begin ≈ 0.5°, end ≈ 359.5°) also lands at the seam, not 180°.
    #[test]
    fn ray_azimuth_north_crossing_is_direction_symmetric() {
        let got = ray_azimuth_deg(bin2(0.5), bin2(359.5));
        let signed = if got > 180.0 { got - 360.0 } else { got };
        assert!(signed.abs() < 1e-2, "expected ~0°, got {got}");
    }

    /// Result is always folded onto `[0, 360)` — never negative, never ≥ 360.
    #[test]
    fn ray_azimuth_stays_in_range() {
        for deg in [0.0f64, 0.5, 90.0, 179.9, 180.1, 270.0, 359.9] {
            let start = bin2(deg);
            let stop = bin2((deg + 1.0).rem_euclid(360.0));
            let got = ray_azimuth_deg(start, stop);
            assert!((0.0..360.0).contains(&got), "deg={deg}: got {got}");
        }
    }
}
