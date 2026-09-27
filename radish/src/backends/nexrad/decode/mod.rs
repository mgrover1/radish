//! In-tree NEXRAD Level 2 byte-level decoder. **Not yet wired to the
//! production read path** — see plan `0003-internal-nexrad-decoder`.
//!
//! Phase 1-4 already merged: byte-cursor primitives, LDM record
//! splitter + bzip2 decompression, Volume Header parser,
//! MessageHeader iteration loop with the boundary-resync fix, and
//! typed parsers for MSG_2 (RDA Status), MSG_5 (VCP), MSG_31
//! (radial data with VOL/ELV/RAD/REF/VEL/SW/ZDR/PHI/RHO/CFP).
//!
//! Phase 5 (this PR) ties it all together: `decode_volume(bytes) ->
//! Scan` produces a radish-internal `Scan` with sweeps grouped by
//! ICD §3.2.4.17 radial_status markers. Phase 6 adds parity tests
//! against `danielway/nexrad`. Phase 7 swaps the runtime call site
//! in `adapter.rs::convert_scan` to use this entry point.

pub(super) mod error;
pub(super) mod header;
pub(super) mod messages;
pub(super) mod model;
pub(super) mod products;
pub(super) mod reader;
pub(super) mod record;
pub(super) mod volume;

#[cfg(test)]
mod integration_test;

use crate::backends::common::par_map;
use error::Result;
use messages::msg2::Msg2;
use messages::msg5::Msg5;
use messages::{decode_messages, MessagePayload};
use model::{group_radials_into_sweeps, Radial, Scan, Site};
use record::{decompress, is_raw_archive2, raw_archive2_body, split_ldm_records};
use volume::parse as parse_volume_header;

/// End-to-end decode of an in-memory NEXRAD Level 2 buffer:
///
/// 1. Optional 24-byte AR2V volume header (skipped if absent).
/// 2. LDM record split → bzip2 decompress → message stream.
/// 3. Per-message typed dispatch via `decode_messages` (Phase 2-4).
/// 4. Aggregate radials, MSG_2 (RDA status), MSG_5 (VCP), site.
/// 5. Group radials into sweeps via ICD radial_status markers.
///
/// Errors with `MissingCoveragePattern` if the file's MSG_5 isn't
/// present — without it we can't emit per-elevation classifier
/// flags downstream.
/// Phase-instrumented variant of [`decode_volume`]. Prints
/// per-phase wall-clock to stderr. Bench-only; the production
/// path stays uninstrumented. Mirrors the fused par_iter shape so
/// the breakdown reflects what `decode_volume` actually does.
pub(crate) fn decode_volume_with_phase_timing(bytes: &[u8]) -> Result<Scan> {
    use std::time::Instant;
    let t0 = Instant::now();
    let _ = parse_volume_header(bytes);
    let per_record: Vec<PerRecordDecode> = if is_raw_archive2(bytes) {
        let body = raw_archive2_body(bytes)?;
        vec![decode_one_message_stream(body)?]
    } else {
        let records = split_ldm_records(bytes)?;
        par_map(&records, decode_one_record)?
    };
    let t1 = Instant::now();
    let t2 = Instant::now();

    let total_radials: usize = per_record.iter().map(|r| r.radials.len()).sum();
    let mut radials: Vec<Radial> = Vec::with_capacity(total_radials);
    let mut coverage_pattern: Option<Msg5> = None;
    let mut rda_status: Option<Msg2> = None;
    let mut site: Option<Site> = None;
    for mut chunk in per_record {
        if site.is_none() {
            site = chunk.site.take();
        }
        if rda_status.is_none() {
            rda_status = chunk.msg2.take();
        }
        if coverage_pattern.is_none() {
            coverage_pattern = chunk.msg5.take();
        }
        radials.append(&mut chunk.radials);
    }
    let t3 = Instant::now();

    let coverage_pattern = match coverage_pattern {
        Some(m) => m,
        None if !radials.is_empty() => Msg5::synthetic_from_radials(&radials),
        None => return Err(error::NexradDecodeError::MissingCoveragePattern),
    };
    let sweeps = group_radials_into_sweeps(radials);
    let t4 = Instant::now();

    let to_ms = |a: Instant, b: Instant| (b - a).as_secs_f64() * 1000.0;
    eprintln!(
        "  phase  split_ldm:                  {:>6.2} ms",
        to_ms(t0, t1)
    );
    eprintln!(
        "  phase  par_iter(decompress+decode):{:>6.2} ms  (fused: bzip2 + typed parse + Radial::from_msg31)",
        to_ms(t1, t2)
    );
    eprintln!(
        "  phase  stitch_per_record:          {:>6.2} ms",
        to_ms(t2, t3)
    );
    eprintln!(
        "  phase  group_into_sweeps:          {:>6.2} ms",
        to_ms(t3, t4)
    );
    eprintln!(
        "  phase  TOTAL:                      {:>6.2} ms",
        to_ms(t0, t4)
    );

    Ok(Scan {
        coverage_pattern,
        sweeps,
        site,
        rda_status,
    })
}

/// Per-LDM-record decode results, gathered in `decode_volume`'s
/// `par_iter` step. Each rayon worker decompresses one record,
/// walks its messages, and accumulates radials + the optional
/// MSG_2 / MSG_5 / Site. The final stitch step preserves
/// LDM-record arrival order so radials stay sweep-grouped.
#[derive(Default)]
struct PerRecordDecode {
    radials: Vec<Radial>,
    msg2: Option<Msg2>,
    msg5: Option<Msg5>,
    site: Option<Site>,
}

/// Decompress one LDM record, then walk its typed messages,
/// extracting per-record radials + the first MSG_2 / MSG_5 / Site
/// seen inside this record. Caller stitches per-record results
/// back together preserving record order.
fn decode_one_record(record: &record::LdmRecord<'_>) -> Result<PerRecordDecode> {
    let payload = decompress(record)?;
    decode_one_message_stream(&payload)
}

/// Walk one already-decompressed message stream, extracting per-frame
/// radials + the first MSG_1 / MSG_2 / MSG_5 / MSG_31-derived Site
/// seen inside this stream. Shared between the LDM path (one stream
/// per LDM record after bzip2) and the raw-CTM path (one stream per
/// 2432-byte frame, no decompression).
fn decode_one_message_stream(payload: &[u8]) -> Result<PerRecordDecode> {
    let messages = decode_messages(payload)?;
    let mut out = PerRecordDecode::default();
    for msg in messages {
        match msg.payload {
            MessagePayload::Msg31(boxed) => {
                let mut m = *boxed;
                if out.site.is_none() {
                    if let Some(vol) = m.volume.take() {
                        out.site = Some(Site::from_vol(m.header.radar_identifier, &vol));
                    }
                }
                out.radials.push(Radial::from_msg31(m));
            }
            MessagePayload::Msg1(boxed) => {
                out.radials.push(Radial::from_msg1(*boxed));
            }
            MessagePayload::Msg2(boxed) if out.msg2.is_none() => {
                out.msg2 = Some(*boxed);
            }
            MessagePayload::Msg5(boxed) if out.msg5.is_none() => {
                out.msg5 = Some(*boxed);
            }
            _ => {}
        }
    }
    Ok(out)
}

pub(crate) fn decode_volume(bytes: &[u8]) -> Result<Scan> {
    // Volume header is optional but useful for the ICAO fallback
    // when no MSG_31 has been seen yet. We discard it for now;
    // the per-radial DataHeader carries the same identifier.
    let _ = parse_volume_header(bytes);

    // Pre-Build-12 (March 2012) raw Archive II files have no LDM
    // wrapper — the entire body after the 24-byte volume header is
    // one continuous variable-length / fixed-segment message
    // stream (mirroring `danielway/nexrad`'s `split_ctm_frames`,
    // which returns one record for the whole body). Detection is
    // by zero-valued u32_be at offset 24 (where an LDM size prefix
    // would otherwise live). No per-record parallelism — there's
    // only one record by construction.
    let per_record: Vec<PerRecordDecode> = if is_raw_archive2(bytes) {
        let body = raw_archive2_body(bytes)?;
        vec![decode_one_message_stream(body)?]
    } else {
        let records = split_ldm_records(bytes)?;
        // Fused per-record pipeline: each rayon worker decompresses
        // one LDM record AND walks its typed messages in the same
        // task, so the typed-parse + Radial::from_msg31 gate copies
        // run in parallel with bzip2 decompression instead of
        // sequentially after `decompress_all` finishes. Mirrors
        // `nexrad-data 1.0.0-rc.7`'s `File::scan` shape
        // (`src/volume/file.rs:143-157`); on this machine takes ~17 ms
        // of sequential post-decompress work to ~3 ms parallelised.
        //
        // `par_iter().collect::<Result<Vec<_>>>()` short-circuits on
        // first error and preserves record order in the output Vec —
        // important because radial order within a sweep matters for
        // the ICD radial_status grouping pass below.
        par_map(&records, decode_one_record)?
    };

    // Stitch: walk per-record results in arrival order, taking
    // the first non-None for site/msg2/msg5 and concatenating
    // radials. With known total radial count we can presize the
    // Vec to avoid intermediate reallocs (typical NEXRAD volume
    // is 6_000-7_500 radials).
    let total_radials: usize = per_record.iter().map(|r| r.radials.len()).sum();
    let mut radials: Vec<Radial> = Vec::with_capacity(total_radials);
    let mut coverage_pattern: Option<Msg5> = None;
    let mut rda_status: Option<Msg2> = None;
    let mut site: Option<Site> = None;
    for mut chunk in per_record {
        if site.is_none() {
            site = chunk.site.take();
        }
        if rda_status.is_none() {
            rda_status = chunk.msg2.take();
        }
        if coverage_pattern.is_none() {
            coverage_pattern = chunk.msg5.take();
        }
        radials.append(&mut chunk.radials);
    }

    // Pre-Build-12 raw files don't always carry an MSG_5. Synthesize
    // a minimal placeholder VCP from the radials' own elevation list
    // so the rest of the pipeline can keep its hard MSG_5 dependency.
    let coverage_pattern = match coverage_pattern {
        Some(m) => m,
        None if !radials.is_empty() => Msg5::synthetic_from_radials(&radials),
        None => return Err(error::NexradDecodeError::MissingCoveragePattern),
    };
    let sweeps = group_radials_into_sweeps(radials);

    Ok(Scan {
        coverage_pattern,
        sweeps,
        site,
        rda_status,
    })
}
