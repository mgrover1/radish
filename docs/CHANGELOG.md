# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **macOS CI (`Test (macos-latest, stable)`, `Test Python 3.12/3.13 on
  macos-latest`) failing with `Unable to locate HDF5 root directory
  and/or headers`, then `Invalid H5_VERSION: "2.2.0"` once that was
  fixed.** Two stacked problems, both from Homebrew's `hdf5` formula
  moving to an unversioned HDF5 2.2.0 release:
  1. `hdf5-metno-sys`'s Homebrew autodetection only recognizes specific
     versioned formula names (`hdf5@1.14`, `hdf5@2.0`, ...) and never
     finds a plain `hdf5` keg — fixed by exporting
     `HDF5_DIR`/`NETCDF_DIR`/`PKG_CONFIG_PATH` via `brew --prefix` after
     `brew install` in `rust-ci.yml`/`python-ci.yml`, matching the
     pattern `release.yml` already used for macOS wheel builds.
  2. Once `HDF5_DIR` was found, `hdf5-metno-sys` 0.11.3's version parser
     didn't recognize HDF5 2.2.0 at all and panicked. `hdf5-metno`
     0.12.x pins `hdf5-metno-sys = "^0.11.3"`; HDF5 2.x support only
     landed in `hdf5-metno-sys` 0.12.x, which pairs with `hdf5-metno`
     0.15.0. Bumped the workspace's `hdf5` dependency (`package =
     "hdf5-metno"`) from `"0.12"` to `"0.15"` to pick it up. radish's
     only use of the crate is the `hdf5::Error` conversion in
     `error.rs`, so this is a `Cargo.lock`-only-shaped change with no
     source changes needed; verified with `cargo build`/`test`/`clippy`
     (460 tests pass) plus a runtime smoke test that creates, writes,
     and reads back a real `.h5` file through the new version. Also
     incidentally drops the unmaintained `paste` crate (replaced by
     `pastey`), clearing one of the four pre-existing `cargo audit`
     warnings.

  `README.md`, `CLAUDE.md`, and `docs/GETTING_STARTED.md` are updated
  to the `brew --prefix` form for #1 (previously hardcoded to
  `/opt/homebrew`, which is Apple Silicon-only).

## [0.4.0] - 2026-08-11

The "NEXRAD Level 3, fully decoded" release. All 7 previously-unsupported NIDS message codes (170/172-177 — digital precip accumulation, instantaneous rate, and hydrometeor classification) now decode, closing out the last gap in packet-16/AF1F/packet-28 coverage. Also extends `TILT_LETTER_TABLE` for SRMV/HCLASS/WRADH, adds real `wasm-bindgen-test` coverage for the `radish-wasm` crate (previously untested), and widens `MomentData` with an additive `raw_codes_u16` field for packet 28's wider codes. (#46, #47)

### Added

- **`radish-wasm` gains real test coverage** — the crate had none before
  this (`crate-type` was `cdylib`-only, which can't be linked against by
  an integration test binary at all; now `["cdylib", "rlib"]`).
  `wasm/tests/decode.rs` runs `wasm-bindgen-test` against the actual
  compiled `wasm32-unknown-unknown` binary under Node (synthetic,
  ICD-shaped bytes, no fixtures needed); `wasm/tests/real_fixtures.rs`
  complements it with real `DAA`/`DPR`/`HHC` objects, `include_bytes!`-
  embedded at compile time behind a `build.rs`-emitted `--cfg
  has_real_fixtures` (wasm32 has no runtime filesystem access, so this
  can't be a `#[cfg(test)]` env-var read the way every other gated test in
  this repo works; a Cargo feature was tried first and reverted since this
  repo's CI runs `--all-features` and would have swept it on unconditionally
  — see `radish/tests/fixtures/CORPUS.md`'s Test gating section for the
  full reasoning and exact commands). Its SHA-256 checks read the same
  committed `expected/*.json` oracle sidecars
  `test_nexrad_level3_xradar_oracle.rs` uses, so the two suites can't
  silently drift apart. Confirms `codes()`/`codesU16()`/`codesWidth` — the
  packet-28 additions below — actually work end to end through the real,
  browser-facing artifact, not just their Rust source.
- **NEXRAD Level 3: all 7 previously-unsupported message codes now decode**
  — `packet_family_implemented` returns `true` for every code in
  `PRODUCTS`; there is no longer a known-but-unimplemented product.
  - **170/172/173/174/175** (`DAA`/`DTA`/`DU3`+`DU6`/`DOD`/`DSD`, the
    digital precip-accumulation family, `DecodeScheme::Precip`) — packet
    16, confirmed on 4 real objects (`DAA`/`DTA`/`DU3`/`DU6`). Same
    8-byte float32 PDB scale/offset pair `FloatScale` reads, but with a
    product-family-specific floor code read from further into the PDB
    (`leading = 1` on real fixtures, not the universal `DATA_FLOOR_CODE =
    2` every OTHER packet-16 scheme uses) and a fixed `0.01 in -> mm`
    conversion factor. `172`/`DTA` decodes through the identical,
    code-agnostic path as the other four but has no byte-exact
    real-fixture oracle confirmation of its own (xradar's own reader
    warns its handling of `DTA`'s product version 3 is unverified) — a
    known, stated gap, not a silent one.
  - **176** (`DPR`, Digital Instantaneous Precipitation Rate,
    `DecodeScheme::Rate`) — packet 28 (XDR, RFC 1832), confirmed on a
    real object. New: a from-scratch XDR unpacker
    (`nexrad_level3::decode::xdr`, every length-prefixed read capped
    against an untrusted length prefix BEFORE allocating), and
    `MomentData::raw_codes_u16: Option<Array2<u16>>` — additive, alongside
    `raw_codes`, for packet 28's `u16` raw levels. Same PDB scale/flag
    logic as `Precip`, factor `1 in -> mm`.
  - **177** (`HHC`, Hybrid Hydrometeor Classification,
    `DecodeScheme::ClassInt` — the same scheme as `HCLASS`/165) — **packet
    16, not packet 28.** An earlier assumption (this project's own design
    doc) had it backwards; 3 independently-fetched real `HHC` objects all
    declared packet 16 on direct byte inspection. Needed almost no new
    code: decodes through the exact same path 165 already used, with
    `has_elevation: false` the only real difference.
  - **wasm**: `DecodedProduct.codesU16()`/`.codesWidth` — additive, zero-copy
    `Uint16Array` accessor for packet-28 products, mirroring `.codes()`'s
    existing contract; `.codes()` itself is unchanged for every existing
    (packet 16/AF1F) caller.
  - **pyo3**: `MomentData.raw_codes()`/`.raw_codes_u16()`/`.value_min`/
    `.value_increment`/`.n_levels`/`.data_floor_code` — new Python
    surface; neither `raw_codes` nor `declared_scale` reached Python at
    all before this (confirmed by grep, not assumed).
- **NEXRAD Level 3 azimuth convention corrected for packet 28**: the
  `Azimuth` field is the ray's LEADING edge, per NEXRAD ICD 2620001AC
  Appendix E Figure E-4 — the same `+ width/2` correction packet 16/AF1F
  already apply, not a different convention. Verified byte-exact
  (codes AND azimuths) against a real `DPR` object and an independent
  reader.
- **NEXRAD Level 3 tilt-letter table extended**: `TILT_LETTER_TABLE` now
  resolves `S` (Storm Relative Mean Radial Velocity, code 56 — 4 of 6 tilt
  ordinals, `N0`/`N1`/`N2`/`N3`, a decode-scheme fact that holds for any date
  the archive tier reads) and `H` (Hydrometeor Classification, code 165, all
  6 tilts). This table records what the AWIPS-id SCHEME resolves to, not
  what is currently broadcasting — for `S` specifically, only `N0S` has live
  2026 data; `N1S`/`N2S`/`N3S` stopped broadcasting 2023-05-22 (confirmed on
  two live sites). The consumer (`radar-animation`'s free-tier capability
  table) is what decides which of a scheme's tilts to actually advertise as
  selectable; this crate answers "what does this AWIPS id decode to",
  unconditionally. A new `SPECIAL_AWIPS_IDS` table (`NSW` -> tilt 0) covers
  `WRADH`'s legacy spectrum width, whose AWIPS id isn't
  `{tilt prefix}{letter}`-shaped and so can't go in the generic table at
  all — `decode()` now checks it as a fallback after `tilt_letter_lookup`.
  `P` (`ACCUM`, codes 78/79/80) deliberately stays unresolved: its letter
  encodes accumulation *period*, not elevation, and `N1P`/`N3P` share a
  prefix with real, unrelated tilt ordinals — adding it would silently
  mislabel a 1-hour accumulation as a 1.3° tilt.
- **`export_product_catalogue_json`**, an `#[ignore]`d test in
  `nexrad_level3::decode::products` that writes `generated/
  nexrad_level3_products.json` — `PRODUCTS`, `TILT_LETTER_TABLE`, and
  `SPECIAL_AWIPS_IDS`, machine-readable, for non-Rust consumers (a sibling
  repo's build step) that need this table but cannot link the crate. Run
  explicitly: `RADISH_WRITE_PRODUCT_CATALOGUE=1 cargo test --release -p
  radish export_product_catalogue_json -- --ignored` — a second, in-body env
  var gate beyond `#[ignore]`, matching the fixture-gated tests in
  `backends/nexrad/decode/integration_test.rs`, so a `cargo test --
  --ignored` sweep cannot overwrite `generated/` as a side effect nobody
  asked for.
- Regression tests for `special_awips_id_lookup`/`tilt_letter_lookup` against
  non-UTF-8 input (both must return `None`, never panic — the AWIPS-id bytes
  they receive come straight off the wire, unvalidated), and a test that
  `SPECIAL_AWIPS_IDS` and `TILT_LETTER_TABLE` cannot structurally collide.

## [0.3.0] - 2026-08-07

The "browser-reachable NEXRAD Level 3 + region-based velocity dealiasing" release. Adds a NEXRAD Level 3 (NIDS) decode backend and a Rust port of Py-ART's region-based velocity dealiasing, plus a new `wasm32-unknown-unknown` target and `radish-wasm` crate so both can run entirely client-side, no server in the loop. Also folds in the NEXRAD real-time-chunks work (incomplete-sweep detection, chunk-list validation, single-chunk auto-detection), the low-level per-moment NEXRAD decoders for chunked/lazy consumers, a full pass of Sigmet/IRIS correctness fixes verified against xradar, and a security-audit cleanup that brings `cargo audit` back to green. (#41, #43, #44)

### Added

- **NEXRAD real-time chunks: incomplete-sweep detection + `incomplete_sweep`
  policy** (plan 0009, following xradar #332). Sweeps whose data is provably
  partial — a volume truncated mid-sweep (partial chunk list from
  `unidata-nexrad-level2-chunks`), a stream joined mid-rotation, or an
  interior chunk gap (start/end markers present but rays missing — a case
  xradar's marker-only flag cannot catch) — are now detected in the decoder
  and surfaced as `SweepData.is_complete` plus
  `VolumeData.incomplete_sweeps` / `VolumeMetadata.incomplete_sweeps`.
  `read_nexrad`, `read_nexrad_bytes`, and `read_nexrad_chunks` accept
  `incomplete_sweep="keep"|"drop"|"pad"` (default `"keep"`, today's
  behavior). `"pad"` reindexes incomplete sweeps onto the full 360/720-ray
  azimuth grid — grid size comes from MSG31's `azimuth_resolution_spacing`
  (exact, with median-step inference for MSG_1 legacy), missing rays are
  NaN across every moment, elevation fills with the sweep median, and
  missing times extrapolate along rotation order (correct across the 0°
  wrap, where linear-in-slot interpolation would not be).
- **Chunk-list validation**: `read_nexrad_chunks` / `scan_nexrad_chunks` now
  reject an empty list and an `AR2V` volume header in any chunk after the
  first (out-of-order chunks / mixed volumes previously concatenated into
  silently corrupt output). Headerless `I`/`E`-only lists remain allowed.
- **Single-chunk auto-detection**: format sniffing now recognises a lone
  real-time chunk object by bytes (LDM control word + `BZh` + level digit +
  plausible record size) and by object name (`YYYYMMDD-HHMMSS-NNN-[SIE]`),
  so `radish.open_datatree(one_chunk_bytes)` and chunk paths route to the
  NEXRAD backend without `backend="nexrad"`.
- **NEXRAD Level 3 (NIDS) backend**, a new `NexradLevel3Backend`
  decoding single-tilt, single-moment NIDS products — reflectivity,
  velocity, spectrum width, ZDR/RHOHV/KDP, hydrometeor classification, and
  the legacy 8/16-level `AF1F`-encoded family (26 message codes total, 19
  implemented past the PDB stage; the remaining 7 — packet-28/XDR and 5
  surface precip codes — refuse cleanly with a named `UnsupportedProduct`
  rather than guessing). `MomentData` gains `raw_codes: Option<Array2<u8>>`
  and `declared_scale: Option<DeclaredScale>` so a caller can recover the
  verbatim on-wire codes, not just the decoded physical values;
  `SweepMetadata` gains `nids: Option<NidsSweepAttrs>` (AWIPS id, message
  code, VCP, tilt ordinal). Byte-exact against a local Python oracle on 7
  real S3 fixtures (`radish/tests/test_nexrad_level3_parity.rs`,
  `#[ignore]`-gated); value-parity against xradar #392's independently
  authored synthetic test vectors for the remaining implemented codes
  (`radish/tests/test_nexrad_level3_xradar_vectors.rs`); and value-parity
  against xradar #392's own decoder on 4 more real fixtures covering
  packet `AF1F` and categorical products, plus 3 real fixtures of
  currently-unimplemented product families confirming a clean rejection
  (`radish/tests/test_nexrad_level3_xradar_oracle.rs`). That cross-check
  surfaced and resolved a real decode question against the NEXRAD ICD
  directly (2620001AC, Figure 3-11c, Note 1): an odd `n_bins` radial
  carries one halfword-alignment pad byte, not a 1688th gate — radish's
  original decode was already ICD-correct; the divergence was in
  xradar's still-unmerged branch.
- **`radish` core now compiles for `wasm32-unknown-unknown`** behind a new
  `native` Cargo feature (default-on) gating `hdf5`/`netcdf`/`rayon` out of
  the dependency graph entirely under `--no-default-features` — verified in
  CI (`.github/workflows/rust-ci.yml`'s `wasm` job asserts
  `netcdf`/`hdf5-metno-sys`/`libz-sys`/`rayon` never enter the wasm32
  dependency graph, for both `radish` core and the new `radish-wasm` crate
  below).
- **Region-based velocity dealiasing**: `radish::transforms::dealias_region_based`,
  a Rust port of Py-ART's `pyart.correct.dealias_region_based`, bit-exact
  with Py-ART on every unmasked gate — verified against a real Py-ART
  install on 2 real NEXRAD Level 2 velocity sweeps
  (`radish/tests/test_dealias_parity.rs`, `#[ignore]`-gated;
  `radish/benches/dealias.rs` records ~8x speedup over Py-ART's own
  Python-object-heavy implementation on the same sweep). Reachable from
  Python as `radish.dealias_region_based(velocity, valid_mask, nyquist,
  rays_wrap_around, ...)` and from the new `radish-wasm` crate as
  `dealiasRegionBased(...)`. `valid_mask` uses the opposite polarity from
  Py-ART's own `gfilter` (`true` = valid/usable, matching Rust convention)
  — documented prominently at every layer since getting this backwards
  silently inverts every result.
- **New `radish-wasm` crate** (`wasm/`), a `wasm-bindgen` binding exposing
  `decodeNexradLevel3(bytes) -> DecodedProduct` (zero-copy `codes()` via
  `Uint8Array::view`, general per-radial `azimuths()` — no fitted
  `az_start_deg`/`az_step_deg` slope; that's application-layer display
  policy) and `dealiasRegionBased(...)`. Library only — no fetch, no S3, no
  worker logic. Measured (not assumed): ~64 KB gzipped for decode+dealias
  combined; ~22 ms to decode a real 224 KB NIDS product, ~570 ms to dealias
  a real 720x1192 velocity sweep (Node.js `wasm-bindgen --target nodejs`
  harness on real fixtures) — see `docs/NEXRAD_LEVEL3_WASM.md` §8 for the
  full measurement table.

### Changed

- **Behavior change — `radish.open_datatree` / `open_dataset` (and the
  `engine="radish"` xarray entrypoints) now default to
  `incomplete_sweep="drop"` for NEXRAD input**: provably-partial sweeps are
  omitted with a `UserWarning` listing the dropped indices (xradar #332
  parity). Previously they were passed through silently with a short
  azimuth dimension. Surviving sweeps keep their *original* `sweep_N`
  names — dropping `sweep_1` leaves a gap rather than renaming `sweep_2`.
  Pass `incomplete_sweep="keep"` for the old shape, or `"pad"` for
  NaN-padded full rotations. Low-level `read_*` defaults are unchanged
  (`"keep"`).

- **Low-level NEXRAD per-moment decoders** —
  `radish.decode_nexrad_record_moment`,
  `radish.decode_nexrad_sweep_moment`,
  `radish.nexrad_record_moment_encoding`,
  `radish.nexrad_sweep_moment_encoding`, and the
  `radish.MomentEncodingError` exception. These pull **one moment** out of
  **one LDM record** (or one sweep-sized byte span) as the raw NEXRAD words,
  so chunked/lazy consumers — zarr codecs, virtual/byte-range reference
  stores, partial-volume reads — can decode exactly the bytes they need
  instead of a whole volume. A 120-radial × 1832-gate reflectivity block
  decodes in ~0.06 ms; the sweep variant decompresses records in parallel
  via rayon (~5× on 8 cores). Verified bit-identical to
  `xradar.io.open_nexradlevel2_datatree` on the first cut of every fixture
  in the corpus. (#32)

  The names are format-qualified (matching `read_nexrad` / `scan_nexrad`)
  so a future Sigmet/ODIM equivalent has room to exist. The unqualified
  spellings issue #32 introduced — `decode_record_moment`,
  `decode_sweep_moment`, `record_moment_encoding`, `sweep_moment_encoding`
  — are kept as first-class aliases referring to the same objects, so that
  issue's `hasattr(radish, "decode_record_moment")` check and any early
  code keep working.

  Output arrays are native-endian; a non-native dtype (`">u2"`) is
  **rejected** rather than silently satisfied, because an array that
  compares equal element-wise but whose `.tobytes()` is byte-swapped is
  exactly the corruption a zarr/reference-store caller would not notice.
  An implausible `out_shape` is rejected too — the allocator would
  otherwise `abort()`, which cannot be turned back into a Python
  exception and would take a long-lived worker down with it.

  The decoders read each Message 31 data block's own
  `word_size`/`scale`/`offset` rather than assuming a fixed encoding —
  NEXRAD moment encodings change across RDA builds (KVNX flipped ZDR from
  `8-bit, scale=16, offset=128` to `16-bit, scale=32, offset=418` on
  2020-06-02, so a decoder that assumes one encoding returns physically
  wrong values for the other era). Pass `scale=`/`offset=` to remap onto a
  common target grid; the remap is applied only when exactly representable
  and `MomentEncodingError` is raised otherwise. An undersized `out_shape`
  is likewise an error — radish never silently truncates gates or drops
  radials.

  Because one output array carries exactly one
  `scale_factor`/`add_offset`, blocks that disagree on
  `(word_size, scale, offset)` are refused unless a target grid is given
  — including blocks of the same width whose `scale`/`offset` differ, and
  including disagreements between separate LDM records in one sweep span.
  `sort_by_azimuth=True` reproduces `np.argsort(azimuth, kind="stable")`
  exactly, signed zero and NaN included, so callers can reorder their
  coordinate arrays to match.

- **KVNX cross-RDA-build fixtures** added to the test corpus
  (`radish/tests/fixtures/CORPUS.md`): `KVNX20200602_123502_V06` and
  `KVNX20200602_201830_V06`, the 8-bit and 16-bit ZDR eras either side of
  the 2020-06-02 upgrade outage. The earlier volume also pins a divergence
  where xradar's first cut has 719 rays with a 1.0° azimuth hole at
  ~90.75°, while radish returns all 720 at uniform 0.5° spacing — confirmed
  against a hand-rolled `bz2`/`struct` walk of the Message 31 headers and
  against radish's own independent volume reader. (#32)

- **Rust API (`radish::backends::nexrad::demux`): the public structs are
  `#[non_exhaustive]` with constructors**, so radish can add fields later
  without a breaking change. Build `DemuxOptions` with
  `DemuxOptions::new(moment, out_shape, word)` — `out_shape` is a `(rays,
  gates)` pair so the two dimensions can't be silently transposed — then
  set the `pub` `fill_value` / `target` fields directly; build
  `TargetEncoding` with `TargetEncoding::new(scale, offset)`. The returned
  `MomentEncoding` and `RecordInventory` are `#[non_exhaustive]` too. The
  enums (`MomentSelector`, `OutputWord`, `RawMoment`) stay exhaustive on
  purpose — their variants are closed domains fixed by the wire format.
  The Python API is unaffected. (#32)

### Fixed

- **Sigmet/IRIS: north-crossing ray azimuth was 180° off.** The ray that
  straddles the 0°/360° seam has begin ≈ 359.5° and end ≈ 0.5°, and the
  per-ray azimuth was computed as the naive mean `(begin + end) / 2`,
  which lands at ~180° — a full half-turn from the true bearing. That ray
  then sat next to a legitimate ~180° ray as a near-duplicate while the
  true ~0° ray went missing from the sweep. `decode_one_ray` now takes the
  **circular** midpoint (unwrapping across the seam before averaging, then
  folding back onto `[0, 360)`), so the IRIS `azimuth` coordinate matches
  `xradar.io.open_iris_datatree` on every ray, including the north
  crossing. Non-seam rays are bit-identical to before. (#40)

- **KILX corpus documentation was inverted.** `CORPUS.md` and
  `python/tests/conftest.py` described `sweep_10` of
  `KILX20230629_154426_V06` as carrying 358 MSG_31 records with 360 being
  an upstream bug. The file carries **360** — a full 1° circle — and 358
  is what xradar reports. `radish/tests/test_nexrad_internal_parity.rs`
  has always asserted the correct 360; only the prose was wrong.
  Confirmed against radish's own reader, a hand-rolled `bz2`/`struct`
  walk, and Py-ART. (#32)

- **Sigmet/IRIS: `time` coordinate was a 1970 epoch offset** — every
  decoded `.RAW` volume placed its rays at `1970-01-01T00:00:08`
  instead of the absolute acquisition time (e.g.
  `2022-06-01T00:02:48.818`). The adapter passed only the per-ray
  `RAY_HEADER` offset (seconds within the sweep) into the coordinate
  builder, never adding the sweep's absolute `YMDS_TIME` start time;
  `read_ymds_time` also discarded the milliseconds field. Both are
  fixed, so the IRIS `time` axis now matches xradar's
  `open_iris_datatree` to the millisecond. Reported via raw2zarr's
  IRIS backend, which could not key its `vcp_time` axis on radish
  until this was resolved. (#28)

- **Sigmet/IRIS: power/phase moments were over-masked vs xradar** —
  radish masked the IRIS `raw == 0` sentinel for *every* moment,
  dropping ~75–94% of `DBZH`/`DBTH`/`ZDR`/`PHIDP` cells where xradar
  keeps the below-threshold values (100% finite). The per-gate decoders
  now mirror xradar's per-type policy exactly: `decode_array` types
  (reflectivity, ZDR, width, PHIDP, and all 2-byte variants) are never
  masked, and `RHOHV`/`SQI` fall to `NaN` only where `sqrt` of a
  negative naturally does. (#28)

- **Sigmet/IRIS: velocity (`VRADH`) dropped its no-data gates** —
  radish returned `NaN` at the ~84% of velocity gates with `raw == 0`,
  while xradar reports `0.0` m/s there. xradar's `DB_VEL` `mask: 0.0`
  does not surface as `NaN`: `np.ma.masked_equal` leaves the underlying
  datum, so the masked value collapses to `0.0`. radish's `DB_VEL`
  8-bit decoder now matches (`raw == 0 → 0.0`), verified byte-for-byte
  against `open_iris_datatree`. (#28)

- **Sigmet/IRIS: KDP was a raw passthrough; nyquist was hardcoded 0** —
  `DB_KDP`/`DB_KDP2` emitted raw byte values instead of decoded KDP, and
  the 8-bit VEL/WIDTH Nyquist scale was `0.0` because the radar
  wavelength was never parsed. `TASK_MISC_INFO.wavelength` is now read,
  `DB_KDP` decodes via xradar's exponential transform
  (`-0.25·sign·600^((127-|d|)/126)/λ`, verified against xradar 0.12.0 on
  both the positive and negative-`d` branches), and the Nyquist velocity
  is derived as `wavelength·prf/40000`. (#28)

- **Sigmet/IRIS: 2-byte moment formulas corrected against xradar** —
  three 2-byte data types diverged from `xradar.io.open_iris_datatree`
  and are now fixed, each pinned by an oracle-value test computed from
  xradar 0.12.0's own decode functions:
  - `DB_PHIDP2` was `360·raw/65535 − 180` (range −180…180°), ~180° off
    the whole scan; now `360·(raw−1)/65534` (0…360°) per xradar
    `decode_phidp2`.
  - `DB_RHOHV2` used divisor `65533`, letting ρHV exceed 1.0; now the
    full `65536` span (`(raw−1)/65536`) so it stays ≤ 1.
  - `DB_SQI2` (→`SQIH`) and `DB_SNR16` (→`SNRH`) passed through as raw
    counts; now decode linearly (`(raw−1)/65536` and `(raw−63)/2`).

  Known remaining divergence: 8-bit `DB_VEL` on dual-/batch-PRF tasks is
  off by the `multi_prf_mode_flag + 1` factor xradar applies (exact on
  single-PRF); tracked as a follow-up. The `time` epoch, over-masking,
  and velocity-zero fixes above are additionally covered by fixture-free
  unit tests so CI verifies them without the `.RAW` fixture. (#28)

### Security

- **All outstanding `cargo audit` advisories resolved; the Security Audit
  CI job is green again with no `--ignore` entries.** It had been failing
  on every branch — `main`'s last green run predates the advisories.

  | Advisory | Crate | Resolution |
  | --- | --- | --- |
  | RUSTSEC-2026-0177 | pyo3 0.22.6 | pyo3 0.22 → 0.29 |
  | RUSTSEC-2025-0020 | pyo3 0.22.6 | pyo3 0.22 → 0.29 (was previously ignored in CI) |
  | RUSTSEC-2026-0204 | crossbeam-epoch 0.9.18 | `cargo update` → 0.9.20 |
  | RUSTSEC-2026-0185 | quinn-proto 0.11.14 | `cargo update` → 0.11.16 (high, 7.5) |

  The pyo3 bump also required `numpy` 0.22 → 0.29. The migration was
  small: `PyArray2::from_owned_array_bound` → `from_owned_array`,
  `PyArray1::from_slice_bound` → `from_slice`, and an explicit
  `from_py_object` opt-in on the five `#[pyclass]` types that derive
  `Clone` (pyo3 0.29 makes that derive opt-in; opting in preserves
  today's behaviour exactly). No API or behaviour change for Python
  callers.

  The old ignore was justified on the grounds that "the upstream
  `nexrad` crate ecosystem hasn't moved yet". That was stale —
  `cargo tree -i pyo3` shows pyo3 is pulled only by `numpy` and by
  radish itself, and `nexrad` is a dev-dependency that doesn't depend on
  pyo3 at all.

## [0.2.5] - 2026-05-05

The "every NEXRAD timestamp was +1 day" fix-only release. ICD 2620002R Table III §3.2.4.17 specifies the per-radial `modified_julian_date` field as 1-indexed days since 1970-01-01, but radish 0.2.2 through 0.2.4 computed `days * 86_400 + secs` (no `-1`), shifting every emitted timestamp by exactly +86,400,000 ms — every sweep, every ray, every file. xradar's `nexrad_level2.py:open_sweeps_as_dict` and danielway/nexrad's `volume/record.rs` both subtract 1; only radish disagreed. Filed by the raw2zarr maintainer, who currently mitigates with an in-process `-86400` shim that 0.2.5 lets them remove. (#26) Plus CI maintenance: GitHub Actions bumped to Node 24-compatible versions before the deprecation deadline (#25).

### Fixed

- **NEXRAD: every ray timestamp was off by exactly +1 day** — every
  sweep, every ray, every NEXRAD Level 2 file across radish-rs
  versions **0.2.2, 0.2.3, and 0.2.4**. ICD 2620002R Table III
  §3.2.4.17 specifies the MSG_31 / MSG_1 ray-header
  `modified_julian_date` field as **1-indexed days since
  1970-01-01** (day 1 = 1970-01-01), but radish computed
  `days * 86_400 + secs` (no `-1`), placing day 1 of the epoch at
  1970-01-02 instead of 1970-01-01. Every decoded timestamp was
  exactly +86_400_000 ms ahead of truth. Moment data
  (DBZH/VRADH/ZDR/PHIDP/RHOHV) was unaffected — only the time axis.

  **Affected APIs:** `radish.scan_nexrad`, `radish.scan`,
  `radish.scan_nexrad_chunks`, `radish.open_datatree`,
  `radish.open_dataset`, `radish.read_nexrad`,
  `radish.read_nexrad_chunks` — all share the same date decoder, so
  all of them were wrong. The bug surfaced in
  `metadata.time_coverage_start` / `time_coverage_end`,
  `nexrad_attrs.sweep_time_ranges[i]`, and per-sweep `time` xarray
  coordinates.

  **Severity:** critical for any consumer using the time axis.
  Time-series Zarr / icechunk stores keyed on `vcp_time` filed
  every record under the wrong date; joins with NWP / RAP / HRRR
  model output landed in the wrong analysis cycle; nowcasting
  pipelines mistimed adjacent sites.

  **Fix:** insert `-1` in both date-conversion call sites
  (`decode/model.rs::msg31_collection_time` and
  `decode/messages/msg1.rs::Msg1::collection_time`) so
  `unix_secs = (days - 1) * 86_400 + collection_time_ms / 1000`,
  matching xradar's `nexrad_level2.py:open_sweeps_as_dict` and
  danielway/nexrad's `volume/record.rs` byte-for-byte.

  **Verified:** the bug-report's reproducer
  (`s3://unidata-nexrad-level2/2025/12/13/KLOT/KLOT20251213_180112_V06`)
  now decodes to `time_coverage_start = 2025-12-13T18:01:12Z`
  matching the V06 filename truth. Plus 4 new regression tests:
  three unit tests pinning the day-1 boundary + the KLOT
  filename-truth fixture value, plus tightened integration tests
  on the KLOT 2025-12-10 + KVNX 2011 fixtures asserting the
  decoded date matches the filename-encoded one. Filed by the
  raw2zarr maintainer at
  `https://github.com/aladinor/raw2zarr` — they currently
  mitigate with an in-process `-86400` shim that this fix lets
  them remove. Thanks to the filer for the precise reproducer
  + cross-implementation comparison against xradar and
  danielway/nexrad.

## [0.2.4] - 2026-05-04

The "metadata-fast-path on bytes / streams" release. Closes the input-shape asymmetry between `read_nexrad` (path/bytes/file-like/chunks) and `scan_nexrad` (path-only) that 0.2.3 left in place. After 0.2.3, `radish.open_datatree(blob)` worked on pre-Build-12 NEXRAD via raw Archive II + Build-11 MSG_31 support, but the **metadata-only** fast path still required a temp-file workaround for S3 / fsspec / obstore inputs. 0.2.4 closes that gap with a new format-agnostic `radish.scan(filename_or_obj, backend=None)` dispatcher and the underlying `scan_nexrad_bytes` / `scan_nexrad_chunks` PyO3 functions. End-to-end on a modern KLOT V06 (5.8 MB): `radish.scan(blob)` ≈ 80 ms, vs `radish.open_datatree(blob)` ≈ 200 ms — the 2.5× speedup is now reachable on bytes input, matching what was already available on path input. (#21)

### Added

- **NEXRAD: `radish.scan` accepts bytes / file-like / chunk streams**,
  closing the input-shape asymmetry between `read_nexrad`
  (path/bytes/file-like/chunks) and `scan_nexrad` (path-only). New
  format-agnostic `radish.scan(filename_or_obj, backend=None)`
  dispatcher mirrors `radish.open_datatree` — same input-shape
  detection, returns `VolumeMetadata` instead of `xr.DataTree`.
  Two new PyO3 friend functions exposed: `scan_nexrad_bytes(data)`
  and `scan_nexrad_chunks(chunks)`. **Compression-agnostic**: caller
  passes already-decompressed AR2V bytes; for `.gz` archives use
  fsspec's `compression="gzip"` filter, `gzip.decompress(raw)`, or
  obstore registered as an fsspec backend
  (`from obstore.fsspec import register`). Closes the
  fail-fallback-to-xradar pattern in raw2zarr v0.18.0 PR #244 — the
  ~10× metadata-extraction speedup is now reachable on S3 input
  through a single `fsspec.open(uri, 'rb').read() →
  radish.scan(blob)` hop.

### Changed

- **`PyNexradVolumeAttrs` and `PyNexradSweepAttrs` now implement
  `__eq__`** (via PyO3's `pyclass(eq)` derived from the underlying
  Rust `PartialEq`). Lets users compare metadata across input
  shapes (e.g. `radish.scan(path).nexrad_attrs ==
  radish.scan(bytes).nexrad_attrs`) without walking every field —
  useful for the parity checks bulk-ingest workflows do per file.

## [0.2.3] - 2026-05-04

The "pre-Build-12 NEXRAD" release. Adds full support for NEXRAD Level 2 / Archive II files predating Build 12 (March 2012) — the format used by the entire 1991-2012 public archive on AWS / Unidata. `radish.scan(blob)` and `radish.open_datatree(blob)` now succeed on these files where they previously raised `unexpected EOF at offset 36`. Modern Build-12+ LDM files are unchanged (verified against KLOT, KILX). End-to-end smoke on `KVNX20110520_000442_V06.gz` (45.6 MB raw AR2): 17 sweeps × 720 az × 1832 range with full dual-pol moments (DBZH/ZDR/PHIDP/RHOHV) in ~780 ms. (#22)

### Added

- **NEXRAD: pre-Build-12 raw Archive II support, including
  Build-11.x MSG_31 layout.** Before this change, files predating
  Build 12 (March 2012) — e.g.
  `s3://unidata-nexrad-level2/2011/05/20/KVNX/...` — raised
  `unexpected EOF at offset 36` because the decoder assumed every
  file was wrapped in LDM-bzip2 records. radish now:

  1. Detects raw Archive II via the zero-valued `u32_be` at byte
     offset 24 (matches xradar's `nexrad_level2.py:309-319` and
     `danielway/nexrad`'s `volume/record.rs:139-156`) and walks
     the message stream directly without bzip2 decompression.
  2. Includes a new `messages::msg1` parser for the legacy MSG_1
     (Digital Radar Data, ICD §3.2.4.2 Table III) format used by
     1991-2008 files.
  3. **Detects the Build-11 MSG_31 layout (9 pointer slots, 68-byte
     header) vs Build-12+ (10 pointer slots, 72-byte header).**
     The CFP block was added in Build 12, so older MSG_31 messages
     reserve only 9 pointer slots in their data header. Detection
     uses the smallest non-zero pointer value (which always equals
     the on-wire header size by construction). Pointer arithmetic
     in `msg31::parse` was also corrected from
     `message_start_offset + ptr` to the canonical
     `header_offset + ptr` (= `start_position + ptr`,
     matching `danielway/nexrad`'s `digital_radar_data::Message::parse`
     and xradar's `block_pointer + 12 + LEN_MSG_HEADER`).
  4. Synthesizes a minimal MSG_5 (VCP) fallback when the source
     file lacks one — common on legacy raw files.

  **Verified:** `KVNX20110520_000442_V06.gz` (45.6 MB raw AR2)
  decodes through `radish.open_datatree` to 17 sweeps × 720 az ×
  1832 range with full dual-pol moments (DBZH/ZDR/PHIDP/RHOHV) in
  ~1.2 s end-to-end. Modern KLOT/KILX LDM files unchanged.

## [0.2.2] - 2026-05-04

The "internal NEXRAD decoder" release. radish now ships a from-scratch ICD-2620002AA-compliant Level 2 / Archive II decoder at `radish::backends::nexrad::decode`, replacing the runtime dependency on `danielway/nexrad`. The public Python and Rust surfaces are unchanged; output values are byte-identical to 0.2.0 except where 0.2.0 had bugs (KLOT VCP-32 surveillance sweeps now omit spurious `VRADH`/`WRADH` moments). Decode performance matches `danielway/nexrad` (1.01× ratio on KLOT and KILX) and is **7.78× faster than xradar** end-to-end through the xarray engine.

### Performance

- **NEXRAD: fused decompress + typed decode into one rayon
  par_iter step** in `decode_volume`. Each rayon worker now
  decompresses one LDM record AND walks its typed messages in
  the same task, so the typed parse + gate-byte copies run in
  parallel with bzip2 decompression instead of sequentially
  after it. Mirrors `nexrad-data-1.0.0-rc.7`'s `File::scan` shape.
  KLOT (5.8 MB): radish::decode_volume 143 → 125.5 ms (-12%),
  matching `danielway/nexrad`'s 127 ms (1.01× ratio).
  KILX (10.4 MB): 147 → 140.8 ms, danielway 142.3 ms.
  Python end-to-end vs xradar: KLOT 6.7× → 7.78×.

### Fixed

- **NEXRAD: MSG_31 data-block routing now goes by
  `DataBlockId.name`, not by ICD-slot index.** Real files pack
  the `data_block_count` valid blocks contiguously into the
  pointer slots in arrival order; the slot index doesn't
  determine the block type. Pre-fix, KLOT VCP-32 surveillance
  sweeps surfaced spurious `VRADH` / `WRADH` moments because
  pointer slot 4 (ICD's PTR_VEL) actually carried a `DZDR`
  block — its gate bytes got mislabeled as velocity.
  Post-fix matches xradar and `danielway/nexrad`'s name-based
  routing. Two new regression tests pin the behavior. (#17)

### Changed

- **NEXRAD: replaced `nexrad` / `nexrad-decode` / `nexrad-data` /
  `nexrad-model` runtime dependencies with the in-tree decoder
  at `radish::backends::nexrad::decode`** — Phase 7 of plan 0003.
  `NexradBackend::{read_volume, scan_file, read_sweep,
  read_bytes_volume, read_chunks_volume}` now route through
  `decode::decode_volume`. The upstream `nexrad` crate stays as
  a `[dev-dependencies]` reference for
  `tests/test_nexrad_internal_parity.rs` only; `cargo tree -p
  radish --edges normal` shows zero `nexrad-*` runtime deps.
  No public API change. The bundled bug-fix benefit is on
  `KILX20230629_154426_V06` where xradar reports 358 rays in
  sweep_10 vs the on-wire-correct 360 — the in-tree decoder
  matches `danielway/nexrad`'s 360 (xradar's stride bug
  documented at
  `xradar/io/backends/nexrad_level2.py:397`).

### Added

- **NEXRAD: end-to-end `decode_volume(bytes) -> Scan` + parity
  harness against `danielway/nexrad`** — Phase 5+6 of plan 0003.
  New `decode/model.rs` lands the radish-internal `Scan` / `Sweep`
  / `Radial` / `Site` types with owned gate-byte buffers
  (`OwnedMoment` / `OwnedCfp`) so the returned tree is
  self-contained — matches the existing
  `nexrad_model::data::Radial` ownership shape that radish's
  adapter consumes today. `decode_volume` ties LDM split + bzip2
  + typed message decode + sweep grouping in one call. Sweep
  grouping uses the **ICD §3.2.4.17 radial_status start/end
  markers** (audit-required: SAILS / MRLE supplemental cuts that
  re-use a previous `elevation_number` form their own short
  sweep instead of merging into the parent — the divergence the
  earlier `danielway/nexrad` audit flagged).
  `radish/tests/test_nexrad_internal_parity.rs` adds two gated
  tests: KLOT and KILX structural parity vs `danielway/nexrad`.
  ICD §3.2.4.17 field-by-field analysis of the previously suspect
  `KILX20230629_154426_V06` confirmed all 6840 MSG_31 records are
  on-wire valid (monotonic timestamps, sequential azimuth_numbers,
  `radial_status=1`, `spot_blank=0`); both our decoder and
  danielway correctly read all 6840. The retracted xradar issue
  #376 stands retracted — the off-by-2 was xradar's, traced to
  its `(recnum - 134) // 120` stride in
  `xradar/io/backends/nexrad_level2.py:397` hard-coding 120
  messages per LDM record (LDM 49 of KILX has 122 = 120 MSG_31 +
  2 MSG_2). Live KLOT fixture: 12 sweeps, KLOT lat/lon ≈
  41.6°N / -88.1°W, every sweep has REF moment. Not yet wired
  into the runtime path — Phase 7 swaps the call site. (#16)
- **NEXRAD: typed MSG_2 (RDA Status) + MSG_5 (Volume Coverage
  Pattern) parsers** at
  `radish/src/backends/nexrad/decode/messages/{msg2,msg5}.rs` —
  Phase 4 of plan 0003. MSG_2 is a flat 60-halfword
  fixed-frame parser (ICD §3.2.4.6 Table IV) covering all 30+
  status/calibration fields including the bit-packed
  `rda_scan_and_data_flags` (HW 14) that radish's existing
  `attrs.rs` consumes for the AVSET/EBC parity attrs. MSG_5
  decodes the 11-halfword header + N×23-halfword elevation cuts
  (ICD §3.2.4.12 Table XI), including ICD Table III-A binary-
  angle decoding for commanded elevation angles. The fixed-frame
  branch in `decode_messages` now dispatches MSG_2/MSG_5 to typed
  parsers in both single-segment and multi-segment (reassembled)
  paths via new `parse_fixed_frame_payload` /
  `parse_reassembled_payload` helpers; everything else stays
  `Raw` / `Reassembled`. Live KLOT fixture validation: typed MSG_2
  decodes plausible bounds (rda_build_number 19xx-24xx,
  vcp_magnitude in ICD range 1..767), typed MSG_5 advertises the
  same VCP as MSG_2 with first cut elevation ≈ 0.5°. (#15)
- **NEXRAD: typed MSG_31 (Digital Radar Data Generic Format)
  parser** at `radish/src/backends/nexrad/decode/messages/msg31/`
  — Phase 3 of plan 0003. Decodes the 72-byte per-radial data
  header (ICD §3.2.4.17.1 Table XVII-A: ICAO + collection time +
  azimuth/elevation + 10 data block pointers), the VOL/ELV/RAD
  info blocks (Tables XVII-E/F/H, with legacy 16-byte and modern
  24-byte RAD layouts auto-detected via `lrtup`; legacy 40-byte
  and modern 48-byte VOL likewise), the generic moment block
  shared by REF/VEL/SW/ZDR/PHI/RHO (Table XVII-B descriptor with
  ICD Table XVII-I gate decoding: `raw=0 → BelowThreshold`,
  `raw=1 → RangeFolded`, else `(raw - offset) / scale`), and the
  CFP block (Table XVII-Q clutter-status / power overlay). The
  message-iteration loop now dispatches MSG_31 to the typed
  parser via `MessagePayload::Msg31(Box<msg31::Msg31<'a>>)`;
  Skip / fixed-frame messages keep their `Raw` payload until
  Phase 4. Live KLOT fixture validates: 7200 typed MSG_31s
  parsed, first radial's VOL block carries KLOT's published
  lat/lon (~41.6°N, -88.1°W), modified Julian date matches
  2025-12-10 (20433). (#14)
- **NEXRAD: internal byte-level decoder infrastructure** at
  `radish/src/backends/nexrad/decode/` — first installment toward
  replacing the runtime dependency on `danielway/nexrad`.
  Lands typed `NexradDecodeError`, `SliceReader` with a
  `try_skip_to(target)` boundary-resync helper for defensive
  recovery from any future under-read, LDM record splitter + bzip2
  (parallel via
  rayon), optional 24-byte Volume Header parser, `MessageHeader`
  per ICD §3.1.3 + §3.2.4.1 (28-byte: 12 TCM + 16 Table II logical),
  `MessageType` enum with explicit `Skip(u8)` for forward-compat,
  and a `decode_messages` iteration loop with the boundary fix from
  day one. Handles ICD Note 7's 0xFFFF variable-length sentinel and
  walks past LDM bzip2 trailing zero-padded frames silently. Not yet
  wired to the production read path — `read_nexrad` / `scan_nexrad`
  still go through the upstream `nexrad-data` decoder. Phase 7 of
  plan 0003 will swap the call site once Phase 3-6 fill in the
  per-message parsers and side-by-side parity tests. (#13)
- **NEXRAD test-corpus infrastructure** — new
  `RADISH_NEXRAD_FIXTURE_DIR` env-var convention (legacy single-file
  `RADISH_NEXRAD_FIXTURE` still honoured); both Rust
  (`radish/tests/test_nexrad.rs`) and Python
  (`python/tests/conftest.py`) test harnesses resolve fixtures
  from the directory with consistent fallback ordering. New
  `radish/tests/fixtures/CORPUS.md` documents the canonical KLOT +
  KILX corpus with SHA-256 sums, S3 URLs, `curl` / `fsspec`
  download recipes, and a deferred-fixture roster. New
  `corpus_sha256s_match_documentation` test pins file contents
  against documented sums so a maintainer who replaces a fixture
  with a slightly different S3 version gets a loud failure pointing
  at CORPUS.md before any parity test runs against drift-data. New
  `nexrad_kilx_fixture` Python fixture + `kilx_fixture()` Rust
  helper queued for the upcoming Phase 2 regression test. (#12)

### Changed

- **CI: Python matrix trimmed to 3.12 + 3.13** (was 3.9, 3.10, 3.11,
  3.12). Drivers: Python 3.9 reached EOL on 2025-10-31; the new
  internal decoder uses PEP 604 (`Path | None`) syntax requiring
  3.10+; consolidating now avoids re-trimming as the decoder grows.
  `python/pyproject.toml` `requires-python = ">=3.12"`. Wheel matrix
  drops from 12 to 6 (3 targets × 2 versions). Lint and type-check
  jobs bumped 3.11 → 3.13. (#12)
- **`docs/RELEASING.md`** — release matrix now exports
  `RADISH_NEXRAD_FIXTURE_DIR=$HOME/.cache/radish/fixtures/nexrad`
  and runs `cargo test -- --ignored` for the parity suite that
  lands in a later phase. Wheel-count claim updated to 6. (#12, #13)

## [0.2.0] - 2026-05-03

### Added

- **NEXRAD: per-sweep MSG_5 attrs and time ranges on `scan_nexrad`** —
  adds `sweep_attrs: Vec<NexradSweepAttrs>` and
  `sweep_time_ranges: Vec<Option<(f64, f64)>>` to `NexradVolumeAttrs`,
  populated by both `scan_nexrad` (metadata-only) and `read_nexrad`
  (full decode). Lets downstream bulk-ingest callers classify
  SAILS / MRLE / MPDA / base-tilt slices and find sweep boundaries
  without paying for per-ray decode. PyO3 surface adds two matching
  getters on `PyNexradVolumeAttrs`. Time ranges are Unix seconds for
  `pandas.to_datetime(t, unit="s")` round-trips. (#7)
- **`docs/` folder** consolidating long-form documentation —
  `ARCHITECTURE.md`, `GETTING_STARTED.md`, `PROJECT_SUMMARY.md`, plus
  the new `CHANGELOG.md` and `README.md` index. The repo root stays
  scoped to operational entry points. (#10)
- **`docs/CHANGELOG.md`** kicks off formal release-note tracking
  (Keep a Changelog 1.1.0 / SemVer 2.0). (#10)
- **`docs/RELEASING.md`** — release walkthrough lives next to the
  rest of the long-form docs. (#9)
- **`scripts/bump-version.sh`** — keeps `Cargo.toml` workspace version
  and `python/pyproject.toml` in lockstep during version bumps. (#9)
- **Crate-level rustdoc** for `radish` significantly expanded;
  intra-doc links now resolve cleanly. (#10)
- **PyO3 docstrings** on every public class/function so
  `help(radish.read_nexrad)` and IPython `?` work as expected. (#10)
- **`releases/tag/...` and `compare/...` ignore patterns** in
  `.github/markdown-link-check-config.json` so Keep-a-Changelog
  footers don't 404 in the window between a CHANGELOG bump and the
  GitHub release/tag actually existing. (#10)

### Changed

- **PyPI distribution renamed to `radish-rs`** (atmoscale account,
  `alfonso@atmoscale.ai`). The Python import path stays `radish`. (#9)
- **Release pipeline modernized** — `release.yml` now uses OIDC
  trusted publishing on PyPI, manylinux_2_28 wheels, gated
  `create-release` job (only fires on tag push, not
  `workflow_dispatch` from a feature branch), and
  `generate-import-lib` for cross-platform builds. (#9)
- **Wheel matrix trimmed** to Linux x86_64 + macOS x86_64 + macOS
  arm64. Linux aarch64 (cross-compile linker can't find aarch64
  hdf5/netcdf) and Windows (`hdf5-metno-sys` vcpkg static-md issues)
  are deferred; sdist is the fallback for those platforms. (#9)
- **Long-form docs moved into `docs/`** — cross-references in
  `CLAUDE.md`, `docs/GETTING_STARTED.md`, and `docs/PROJECT_SUMMARY.md`
  updated to the new paths. (#10)

### Fixed

- **NEXRAD `sweep_fixed_angle` parity** — was returning the
  achieved median (`Sweep::elevation_angle_degrees()`) instead of
  xradar's commanded MSG_5 (`ElevationCut::elevation_angle_degrees()`),
  diverging by up to ~0.18°. New `fixed_angle_for(cut, sweep)` helper
  prefers the commanded angle and falls back to the median-of-radials.
  Result: byte-identical to xradar on the KLOT fixture. (#8)

### Removed

- **`plans/` directory** removed from version control and added to
  `.gitignore` along with `.claude/` — these were author-private
  working notes that didn't belong in the repo. (#10)

## [0.1.0] - 2026-05-03

First public release on PyPI as
[`radish-rs`](https://pypi.org/project/radish-rs/0.1.0/).

### Added

- **Core data model**: `VolumeData`, `SweepData`, `MomentData`,
  `Coordinates`, `VolumeMetadata`, `SweepMetadata` — normalized to
  CfRadial2 / FM301.
- **Backend trait system**: `RadarBackend` trait with `scan_file`,
  `read_sweep`, `read_volume`, plus `auto_backend()` /
  `auto_backend_for_bytes()` dispatchers and a `can_read_bytes` /
  `read_bytes_volume` extension for in-memory inputs.
- **CfRadial1 backend** (NetCDF input). Migrated from `netcdf` 0.9
  to 0.12 and from the `hdf5` crate to `hdf5-metno`.
- **NEXRAD Level 2 backend** (Archive II input):
  - MSG_2 + MSG_5 + MSG_31 decoding.
  - Parallel LDM bzip2 decompression via the upstream `nexrad`
    crate's `parallel` feature (mandatory; see `CLAUDE.md` for the
    ~6× throughput gotcha if disabled).
  - `read_nexrad_bytes` for in-memory single buffers.
  - **Chunk-stream reader** for `unidata-nexrad-level2-chunks` —
    decode a volume from a list of byte chunks without first
    re-assembling the file.
  - MSG_2/MSG_5 root and per-sweep attrs surfaced for engine-swap
    parity with xradar (`Dataset.attrs` keys match).
  - Structural shape matches xradar's `open_nexradlevel2_datatree`
    for drop-in compatibility.
- **IRIS / Sigmet RAW backend** (PPI + RHI):
  - Per-sweep + volume attrs (`SigmetVolumeAttrs`,
    `SigmetSweepAttrs`).
  - Bytes input.
  - Criterion + Python wall-clock benchmarks vs xradar.
  - Integration tests (Rust + Python).
- **FM301 scalar variables** — `sweep_mode`, `sweep_number`,
  `sweep_fixed_angle`, `prt_mode`, `follow_mode` emitted as scalar
  data variables on each sweep group.
- **Python bindings** (PyO3 0.22): `read_cfradial1`, `scan_cfradial1`,
  `read_nexrad`, `scan_nexrad`, `read_sigmet`, `scan_sigmet`,
  `open_datatree`. Move-on-read ownership model so a typical
  xarray-driven open avoids cloning per-moment payloads.
- **Unified entry point**: `radish.open_datatree(input, backend=...)`
  dispatches across paths, bytes, and lists of either.
- **xarray plugin**: `xarray.open_datatree(path, engine="radish")`
  routes to the right backend automatically.
- **Structural parity tests vs xradar** — pin the data-tree shape
  for sigmet today, NEXRAD-ready scaffolding in place.
- **Long-form documentation** — `ARCHITECTURE.md`,
  `GETTING_STARTED.md`, `PROJECT_SUMMARY.md` (covering the Phase 0/1/2
  plan), all later moved under `docs/` in `[Unreleased]`.

### Known limitations

- Linux aarch64 + Windows wheels are not yet shipped (cross-compile
  and vcpkg static-lib hdf5 issues respectively); the sdist is the
  fallback for those platforms.
- CfRadial2 native reader and ODIM H5 backend are planned for
  Phase 2 (see `docs/PROJECT_SUMMARY.md`).

[Unreleased]: https://github.com/aladinor/radish/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/aladinor/radish/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aladinor/radish/compare/v0.2.5...v0.3.0
[0.2.5]: https://github.com/aladinor/radish/compare/v0.2.4...v0.2.5
[0.2.4]: https://github.com/aladinor/radish/compare/v0.2.3...v0.2.4
[0.2.3]: https://github.com/aladinor/radish/compare/v0.2.2...v0.2.3
[0.2.2]: https://github.com/aladinor/radish/compare/v0.2.0...v0.2.2
[0.2.0]: https://github.com/aladinor/radish/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/aladinor/radish/releases/tag/v0.1.0
