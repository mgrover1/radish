//! NEXRAD Level 2 (Archive II / AR2V) backend.
//!
//! Adapts the upstream `nexrad-model` decoded `Scan` representation into
//! radish's `VolumeData` model. The upstream crate already returns
//! physical-units `f32` per gate, so the adapter does no scaling: it only
//! reshapes flat per-moment buffers into `Array2<f32>`, assembles the per-sweep
//! `Coordinates`, and surfaces site/VCP metadata.
//!
//! See `plans/0001-nexrad-level2-backend.md` for the design and the Phase 0
//! benchmark (≈25× faster decode than xradar on the user's KLOT fixture).

use std::path::Path;

use crate::{backends::RadarBackend, RadishError, Result, SweepData, VolumeData, VolumeMetadata};

mod adapter;
pub use adapter::IncompleteSweepPolicy;
mod attrs;
// Internal byte-level NEXRAD decoder. Phase 7 wires it into the
// runtime read path; some helpers (e.g. `read_i32_be`) remain
// unused inside the decoder's own scaffold but are kept for
// completeness — `#[allow(dead_code)]` keeps the module quiet
// without polluting individual files.
#[allow(dead_code)]
mod decode;
pub mod demux;
mod mapping;
pub(crate) mod sniff;

/// Time the in-tree decode pipeline only (no adapter to
/// `VolumeData`). Returns `Ok(())` so callers can `time` it
/// without holding the produced `Scan` open. Hidden from the public
/// docs — exposed only so `tests/test_decode_speed_comparison.rs`
/// can do head-to-head timing against `nexrad::data::volume::File::scan()`.
#[doc(hidden)]
pub fn time_decode_volume(bytes: &[u8]) -> Result<()> {
    let _scan = decode::decode_volume(bytes).map_err(|e| RadishError::Decode(e.to_string()))?;
    Ok(())
}

/// Phase-breakdown bench escape hatch. Exposed for
/// `tests/test_decode_phase_breakdown.rs` only — runs the decode
/// pipeline with an optional `eprintln!` per phase when
/// `RADISH_NEXRAD_PHASE_BREAKDOWN=1` is set. Not part of the
/// supported API; treats numbers as a one-shot profiling aid, not
/// a regression gate.
#[doc(hidden)]
pub fn bench_decode_phases(bytes: &[u8]) -> Result<()> {
    decode::decode_volume_with_phase_timing(bytes)
        .map_err(|e| RadishError::Decode(e.to_string()))?;
    Ok(())
}

/// Backend for NEXRAD Level 2 Archive II files (AR2V).
pub struct NexradBackend;

impl NexradBackend {
    /// Create a new backend instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for NexradBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RadarBackend for NexradBackend {
    fn name(&self) -> &str {
        "nexrad_level2"
    }

    fn description(&self) -> &str {
        "NOAA NEXRAD Level II (Archive II / AR2V)"
    }

    fn supported_extensions(&self) -> &[&str] {
        sniff::EXTENSIONS
    }

    /// NEXRAD files are commonly distributed without an extension, so the
    /// default extension match is supplemented with `AR2V` magic-byte and
    /// canonical filename-pattern checks.
    fn can_read(&self, path: &Path) -> bool {
        sniff::looks_like_nexrad(path)
    }

    /// Magic-byte sniff for in-memory buffers — recognises raw `AR2V` and
    /// gzip-wrapped (older `*.gz` archive volume) buffers.
    fn can_read_bytes(&self, head: &[u8]) -> bool {
        sniff::looks_like_ar2v_bytes(head)
    }

    /// Decode a NEXRAD Level 2 volume from a single in-memory byte buffer.
    ///
    /// Convenience entry point for the common case of "fetch the whole file
    /// from S3 / HTTP / fsspec, then decode" — equivalent to xradar's
    /// `xradar.io.open_nexradlevel2_datatree(data)` when given one bytes
    /// object.
    ///
    /// **The buffer must be already-decompressed AR2V bytes.** radish does
    /// not handle gzip — for `.gz` archives use fsspec's transparent
    /// decompression filter (`fsspec.open(uri, "rb", compression="gzip")`)
    /// or call `gzip.decompress(raw)` manually before passing in. Keeps
    /// the radish dependency surface minimal (no `flate2` crate) and
    /// follows xradar's convention since their `open_nexradlevel2_datatree`
    /// also expects pre-decompressed bytes when given a `bytes` input.
    fn read_bytes_volume(&self, data: Vec<u8>) -> Result<VolumeData> {
        self.read_bytes_volume_with(data, IncompleteSweepPolicy::Keep)
    }

    fn scan_file(&self, path: &Path) -> Result<VolumeMetadata> {
        // Phase 1 does a full decode then drops sweeps. A truly cheap path
        // would read only the 24-byte volume header (ICAO + datetime) plus
        // MSG_5 (VCP) — but `radish::VolumeMetadata` also wants lat/lon and
        // per-sweep angles, which need at least one decompressed LDM chunk.
        // Re-evaluate when a user needs sub-100 ms scan.
        let scan = decode_path(path)?;
        adapter::build_volume_metadata(&scan, path)
    }

    fn read_sweep(&self, path: &Path, sweep_idx: usize) -> Result<SweepData> {
        let scan = decode_path(path)?;
        let sweep = scan
            .sweeps
            .get(sweep_idx)
            .ok_or(RadishError::InvalidSweepIndex(sweep_idx))?;
        let cut = scan.coverage_pattern.elevation_cuts().get(sweep_idx);
        adapter::convert_sweep(sweep, sweep_idx, cut)
    }

    fn read_volume(&self, path: &Path) -> Result<VolumeData> {
        self.read_volume_with(path, IncompleteSweepPolicy::Keep)
    }
}

impl NexradBackend {
    /// Decode a NEXRAD Level 2 volume from a sequence of chunk byte buffers.
    ///
    /// Each volume from NOAA's live `unidata-nexrad-level2-chunks` S3 bucket
    /// is split into many small files that arrive seconds after each scan
    /// (vs. minutes for the assembled `noaa-nexrad-level2` archive):
    ///
    /// * `S` — volume header + first metadata records (start)
    /// * `I00..In` — LDM-compressed sweep data, numbered in scan order
    /// * `E` — end-of-volume marker
    ///
    /// The chunks must arrive **in scan order** — the same contract as
    /// `xradar.io.open_nexradlevel2_datatree(list_of_bytes)`. Concatenating
    /// `[S, I00, I01, ..., E]` reconstitutes a complete Archive II file
    /// byte-for-byte (volume header from `S`, then the LDM record stream
    /// across all chunks), so we hand it to the same decoder. A truncated
    /// volume (no `E`, or only the first few `I` chunks) decodes whatever
    /// rays survive — incomplete trailing sweeps come through with fewer
    /// rays than the VCP's expected count.
    pub fn read_chunks_volume(&self, chunks: Vec<Vec<u8>>) -> Result<VolumeData> {
        self.read_chunks_volume_with(chunks, IncompleteSweepPolicy::Keep)
    }

    /// [`Self::read_volume`] with an explicit incomplete-sweep policy
    /// (plan 0009). `Keep` is byte-identical to `read_volume`.
    pub fn read_volume_with(
        &self,
        path: &Path,
        policy: IncompleteSweepPolicy,
    ) -> Result<VolumeData> {
        let scan = decode_path(path)?;
        adapter::convert_scan(scan, path, policy)
    }

    /// [`Self::read_bytes_volume`] with an explicit incomplete-sweep
    /// policy (plan 0009).
    pub fn read_bytes_volume_with(
        &self,
        data: Vec<u8>,
        policy: IncompleteSweepPolicy,
    ) -> Result<VolumeData> {
        let scan = decode_bytes(&data)?;
        adapter::convert_scan(scan, Path::new("<bytes>"), policy)
    }

    /// [`Self::read_chunks_volume`] with an explicit incomplete-sweep
    /// policy (plan 0009) — the natural entry point for partial
    /// real-time chunk lists, where the trailing sweep is usually
    /// still in progress.
    pub fn read_chunks_volume_with(
        &self,
        chunks: Vec<Vec<u8>>,
        policy: IncompleteSweepPolicy,
    ) -> Result<VolumeData> {
        let combined = concat_chunks(chunks)?;
        let scan = decode_bytes(&combined)?;
        adapter::convert_scan(scan, Path::new("<chunks>"), policy)
    }

    /// Scan a NEXRAD Level 2 volume from a single in-memory byte buffer
    /// for **metadata only** — the bytes-input twin of [`Self::scan_file`].
    ///
    /// Equivalent to `read_bytes_volume(data)` then dropping the per-ray
    /// moment data, but skips the `convert_scan` adapter pass that
    /// materializes the per-sweep `Array2<f32>` moments. ~3× faster
    /// than `read_bytes_volume` on a typical fixture, matching the
    /// speedup `scan_file` has over `read_volume`.
    ///
    /// **Compression-agnostic**: the buffer must be already-decompressed
    /// AR2V bytes. radish does not handle gzip — use fsspec's transparent
    /// decompression filter (`fsspec.open(uri, "rb",
    /// compression="gzip")`) or `gzip.decompress(raw)` for `.gz`
    /// archives. obstore users who want the same ergonomics can
    /// register obstore as an fsspec backend
    /// (`from obstore.fsspec import register`) — that gives
    /// `fsspec.open(..., compression="gzip")` access to obstore's
    /// faster S3 I/O without sacrificing transparent decompression.
    pub fn scan_bytes_volume(&self, data: Vec<u8>) -> Result<VolumeMetadata> {
        let scan = decode_bytes(&data)?;
        adapter::build_volume_metadata(&scan, Path::new("<bytes>"))
    }

    /// Scan metadata from a NEXRAD Level 2 chunk-stream volume — the
    /// chunked-input twin of [`Self::scan_file`]. Same chunk-order
    /// contract as [`Self::read_chunks_volume`] (`S` first, then
    /// `I00..In`, then `E`), but stops after metadata extraction.
    ///
    /// Useful for the
    /// [`unidata-nexrad-level2-chunks`](https://registry.opendata.aws/noaa-nexrad/)
    /// S3 stream where you want to know the volume's VCP / instrument
    /// / time coverage without paying for per-ray decode.
    ///
    /// Same compression-agnostic contract as
    /// [`Self::scan_bytes_volume`]: chunks must be raw, already-
    /// decompressed bytes from each S3 object.
    pub fn scan_chunks_volume(&self, chunks: Vec<Vec<u8>>) -> Result<VolumeMetadata> {
        let combined = concat_chunks(chunks)?;
        let scan = decode_bytes(&combined)?;
        adapter::build_volume_metadata(&scan, Path::new("<chunks>"))
    }
}

/// Concatenate chunk buffers into a single owned `Vec<u8>` with one
/// allocation. The implementation is straightforward but worth keeping in
/// one place: chunk count can be in the dozens for a typical KXXX volume,
/// so pre-sizing the destination matters.
///
/// Rejects what is *provably* wrong before the decoder sees it:
///
/// * an empty list (nothing to decode — the decoder's
///   `UnsupportedRecordFormat` for zero bytes is a worse message);
/// * an `AR2V` volume header in any chunk after the first — the `S`
///   chunk must lead, so a later header means the list is out of order
///   or mixes volumes, and concatenating would silently corrupt.
///
/// A list with *no* volume header anywhere stays allowed: headerless
/// `I`/`E` streams decode (site and VCP recovered from MSG_31 / a
/// synthetic MSG_5), just with degraded volume metadata.
fn concat_chunks(chunks: Vec<Vec<u8>>) -> Result<Vec<u8>> {
    if chunks.is_empty() {
        return Err(RadishError::InvalidFormat(
            "empty chunk list — pass the volume's chunks in scan order (S, I01.., E)".to_string(),
        ));
    }
    for (idx, chunk) in chunks.iter().enumerate().skip(1) {
        if chunk.starts_with(sniff::AR2V_MAGIC) {
            return Err(RadishError::InvalidFormat(format!(
                "chunk {idx} starts with an AR2V volume header — the `S` chunk must be \
                 first and all chunks must come from a single volume in scan order"
            )));
        }
    }
    let total: usize = chunks.iter().map(|c| c.len()).sum();
    let mut out = Vec::with_capacity(total);
    for chunk in chunks {
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Decode an Archive II buffer through the in-tree decoder. The
/// returned `Scan` already carries the optional `rda_status` (first
/// MSG_2 in the file) — the adapter doesn't need a separate
/// MSG_2 walk anymore.
fn decode_bytes(data: &[u8]) -> Result<decode::model::Scan> {
    decode::decode_volume(data).map_err(|e| RadishError::Decode(e.to_string()))
}

/// Read the file from disk and decode through `decode_bytes`.
fn decode_path(path: &Path) -> Result<decode::model::Scan> {
    decode_bytes(&std::fs::read(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_and_description_are_static() {
        let b = NexradBackend::new();
        assert_eq!(b.name(), "nexrad_level2");
        assert!(b.description().contains("NEXRAD"));
    }

    #[test]
    fn extensions_include_ar2v() {
        let b = NexradBackend::new();
        assert!(b.supported_extensions().contains(&"ar2v"));
    }

    #[test]
    fn can_read_extensionless_canonical_filename() {
        let b = NexradBackend::new();
        // Filename-pattern branch — no I/O.
        assert!(b.can_read(Path::new("/data/KLOT20260310_231412_V06")));
    }

    #[test]
    fn cannot_read_random_extension() {
        let b = NexradBackend::new();
        assert!(!b.can_read(Path::new("/data/foo.txt")));
    }

    #[test]
    fn concat_chunks_preserves_bytes() {
        // Pinning the assembly contract: concat must be a flat byte-for-byte
        // join, no header rewriting. If a future "smart" implementation
        // tries to strip per-chunk headers, this test catches it.
        let a = vec![0x41, 0x52, 0x32, 0x56]; // "AR2V" magic
        let b = vec![4, 5, 6, 7, 8, 9];
        let c = vec![10, 11];
        let out = concat_chunks(vec![a.clone(), b.clone(), c.clone()]).expect("valid order");
        let expected: Vec<u8> = a.into_iter().chain(b).chain(c).collect();
        assert_eq!(out, expected);
    }

    #[test]
    fn concat_chunks_empty_list_errors() {
        let err = concat_chunks(Vec::<Vec<u8>>::new()).expect_err("empty must error");
        assert!(err.to_string().contains("empty chunk list"), "got: {err}");
    }

    #[test]
    fn concat_chunks_rejects_volume_header_after_first_chunk() {
        // An S chunk in second position = out-of-order list or two
        // volumes mixed — either way concatenation would corrupt.
        let i_chunk = vec![0x00, 0x01, 0x5f, 0x05, b'B', b'Z', b'h', b'9'];
        let s_chunk = b"AR2V0006.901xxxxxxxxxxxx".to_vec();
        let err = concat_chunks(vec![i_chunk.clone(), s_chunk.clone()])
            .expect_err("late header must error");
        assert!(err.to_string().contains("chunk 1"), "got: {err}");

        // Headerless list (I/E only) and header-first list stay fine.
        assert!(concat_chunks(vec![i_chunk.clone(), i_chunk.clone()]).is_ok());
        assert!(concat_chunks(vec![s_chunk, i_chunk]).is_ok());
    }

    /// Real-fixture round-trip test: read the entire fixture as bytes, split
    /// at arbitrary byte boundaries, hand the splits to `read_chunks_volume`,
    /// and verify the resulting volume metadata matches the path-based read.
    /// Gated on `RADISH_NEXRAD_FIXTURE` like the other integration tests.
    #[test]
    fn read_chunks_volume_round_trips_full_file() {
        let path = match std::env::var("RADISH_NEXRAD_FIXTURE") {
            Ok(p) => p,
            Err(_) => return,
        };
        let bytes = std::fs::read(&path).expect("read fixture");
        // Three-way split at thirds — any in-buffer split should round-trip
        // because we just concat them back.
        let n = bytes.len();
        let chunks = vec![
            bytes[..n / 3].to_vec(),
            bytes[n / 3..2 * n / 3].to_vec(),
            bytes[2 * n / 3..].to_vec(),
        ];

        let backend = NexradBackend::new();
        let from_path = backend.read_volume(Path::new(&path)).expect("path decode");
        let from_chunks = backend.read_chunks_volume(chunks).expect("chunks decode");

        assert_eq!(
            from_path.metadata.instrument_name,
            from_chunks.metadata.instrument_name
        );
        assert_eq!(from_path.num_sweeps(), from_chunks.num_sweeps());
        assert_eq!(
            from_path.metadata.sweep_fixed_angles,
            from_chunks.metadata.sweep_fixed_angles
        );
        // Pin the MSG_2 / MSG_5 attrs round-trip as well — this is the surface
        // most likely to silently degrade if the chunked path drops a record.
        let na = from_path
            .metadata
            .nexrad
            .expect("path: nexrad attrs present");
        let nb = from_chunks
            .metadata
            .nexrad
            .expect("chunks: nexrad attrs present");
        assert_eq!(na, nb);
    }

    /// Bytes-input metadata fast path: full bytes buffer through
    /// `scan_bytes_volume` must produce metadata identical to the
    /// path-based `scan_file` (modulo the internal source-path
    /// placeholder). The acceptance criterion the downstream chain
    /// (raw2zarr#244) needs to drop the xradar fallback.
    #[test]
    fn scan_bytes_volume_round_trips_match_scan_file() {
        let path = match std::env::var("RADISH_NEXRAD_FIXTURE") {
            Ok(p) => p,
            Err(_) => return,
        };
        let bytes = std::fs::read(&path).expect("read fixture");

        let backend = NexradBackend::new();
        let from_path = backend.scan_file(Path::new(&path)).expect("scan_file");
        let from_bytes = backend.scan_bytes_volume(bytes).expect("scan_bytes_volume");

        assert_eq!(from_path.instrument_name, from_bytes.instrument_name);
        assert_eq!(from_path.sweep_fixed_angles, from_bytes.sweep_fixed_angles);
        // MSG_2 / MSG_5 attrs must match bit-identically — this is the
        // surface raw2zarr#244 inspects per file.
        assert_eq!(from_path.nexrad, from_bytes.nexrad);
    }

    /// Chunked-input metadata fast path. Three-way byte split mirrors
    /// the existing `read_chunks_volume_round_trips_full_file` shape.
    #[test]
    fn scan_chunks_volume_round_trips_match_scan_file() {
        let path = match std::env::var("RADISH_NEXRAD_FIXTURE") {
            Ok(p) => p,
            Err(_) => return,
        };
        let bytes = std::fs::read(&path).expect("read fixture");
        let n = bytes.len();
        let chunks = vec![
            bytes[..n / 3].to_vec(),
            bytes[n / 3..2 * n / 3].to_vec(),
            bytes[2 * n / 3..].to_vec(),
        ];

        let backend = NexradBackend::new();
        let from_path = backend.scan_file(Path::new(&path)).expect("scan_file");
        let from_chunks = backend
            .scan_chunks_volume(chunks)
            .expect("scan_chunks_volume");

        assert_eq!(from_path.instrument_name, from_chunks.instrument_name);
        assert_eq!(from_path.sweep_fixed_angles, from_chunks.sweep_fixed_angles);
        assert_eq!(from_path.nexrad, from_chunks.nexrad);
    }
}
