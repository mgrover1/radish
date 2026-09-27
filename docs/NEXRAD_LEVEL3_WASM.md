# NEXRAD Level 3 (NIDS) in radish, and radish in the browser

**Status:** implemented. All phases below are done and verified — this
document now serves as the design record and the byte-format reference
for the decoder, not a forward-looking plan.

---

## 1. Why this exists

radish already reads CfRadial1, ODIM, IRIS/Sigmet, and NEXRAD Level 2 —
NEXRAD Level 3 (NIDS) is the obvious remaining gap in radar-format
coverage. Level 3 products are small (a real product is at most ~1.3 MB
decompressed) and self-contained, which also makes them the natural first
target for a `wasm32` build: a radar client that wants to decode and
display NIDS products with no server in the path — reading the bytes
directly from public object storage, entirely in the browser — needs a
decoder that can run there.

That's the motivating use case: a serverless, browser-only NEXRAD Level 3
viewer, where the browser fetches NIDS bytes from public storage
(`s3://unidata-nexrad-level3`, no auth required) and decodes/renders them
with no backend at all. **The one thing that architecture costs is a
decoder** — someone has to turn NIDS bytes into codes a browser can
render, and it has to run in the browser itself, not on a server that
doesn't exist in this deployment shape.

That decoder is what this document is about, and radish is where it
should live.

---

## 2. Why radish, and not a standalone port

The alternative to putting this in radish is a from-scratch decoder in
whatever language the browser client is written in (e.g. a TypeScript
port), kept in sync with a reference implementation by a diff test. That
has a structural problem: two independent implementations of one binary
format that must agree byte-for-byte, forever — and if a radar client
also displays NEXRAD Level 2 data through the same code-space convention,
a disagreement between the two decoders doesn't throw, it silently
renders the same storm differently depending on which tier served it.

radish removes the problem instead of policing it:

```
radish (pure Rust core: decode, model, normalize)
  |
  +-- radish-python   (pyo3 / maturin, cdylib "_radish")  -> servers, notebooks
  +-- radish-wasm     (wasm-bindgen)                       -> the browser
```

**One decoder, two bindings, nothing to diff.** The pyo3 binding already
existed (`python/Cargo.toml`, `crate-type = ["cdylib"]`, `pyo3` + `numpy`)
before this work started, which is the condition that makes this pay
off — without an existing native binding, a new Rust decoder would just
be a *different* second implementation to keep in sync; with one already
in place, adding a wasm binding makes it the *only* implementation,
reachable from both server and browser contexts.

Secondary benefits, in order of importance:

1. radish gains a Level 3 backend it wants regardless of any particular
   consumer — it's the obvious gap in its existing format coverage.
2. radish gains a `wasm32` target, which makes every backend it has
   browser-reachable, not just this one.
3. Any downstream browser client gets a decoder without owning a second
   codebase to keep byte-exact with a server-side reference.

---

## 3. Prior art consulted while designing the byte-level contract

Two independent readers of the NIDS format were used to cross-check the
byte-level contract below (§4) before writing any decode code:

- A production Python decoder (not part of this repository) that reads a
  narrow, deliberately restricted set of products — the six digital
  radial products carried by message codes 153/154/99/159/161/163 —
  packet 16 only, and refuses everything else. Byte parity with this
  decoder, on real fixtures pulled unmodified from
  `s3://unidata-nexrad-level3`, is this backend's acceptance gate for
  those six codes (`radish/tests/test_nexrad_level3_parity.rs`).
- [openradar/xradar PR #392][xr392] — a general Level 3 reader covering
  packets 16, `AF1F`, and 28 across ~27 message codes. Used as the
  breadth reference for the codes beyond the original six, and as a
  second independent reading of the ICD's more ambiguous fields
  (`radish/tests/test_nexrad_level3_xradar_vectors.rs` ports its
  synthetic, byte-built test vectors).

Two independent implementations of the same specification agreeing is
real evidence the ICD reading is correct; where they disagreed during
development, that disagreement was itself the signal to go re-read the
ICD rather than pick one arbitrarily.

`radish/src/backends/nexrad/` (the existing NEXRAD Level 2 backend) was
the scaffolding this backend followed for module shape, error handling,
and how a backend plugs into `auto_backend()` — see
`docs/ARCHITECTURE.md`.

---

## 4. The byte-level contract — read this before touching decode code

This section exists because most of it is counter-intuitive, and every
item was learned by getting it wrong first while cross-checking against
the two reference readers in §3.

### 4.1 The codes pass through untouched

`MomentData` exposes the decoded product **both** ways: normalized
physical values (radish's usual model, for consumers that want physical
units) **and** the verbatim on-wire codes plus the declared scale
(`raw_codes: Option<Array2<u8>>`, `declared_scale: Option<DeclaredScale>`
on `MomentData`). This exists because a display pipeline that maps codes
through a fixed color lookup table directly — never converting to a
physical float at all — needs the raw byte, not a re-derived and
possibly-rounded-differently physical value:

```rust
moment.data            // f32, physical, NaN below the data floor — radish's normal model
moment.raw_codes       // Array2<u8>, verbatim, exactly as the file stored them
moment.declared_scale  // (value_min, value_increment, n_levels) as DECLARED in the PDB
```

**Widened, additively, for packet 28 (plan 0012, §11).** Packet 28's raw
levels are `u16`, not `u8` — `raw_codes_u16: Option<Array2<u16>>` carries
those (`RATE`/176 today), alongside `raw_codes`, never both populated for
the same moment. Every packet 16/AF1F product keeps using `raw_codes`
exactly as above, unmodified.

### 4.2 Refuse, never silently rescale

If a product's on-wire declared scale doesn't match what a caller
expects, the correct behavior is a hard error, not a silent remap —
radish exposes the declared scale faithfully rather than normalizing it
away, so a caller that needs to enforce a specific scale can detect a
mismatch itself. Silently rescaling would mis-color every pixel by a
constant, invisibly.

### 4.3 Two scaling forms, and the type that keeps them apart

The PDB (Product Description Block) encodes value scaling two different
ways, selected by message code:

| form | codes | where | meaning |
|---|---|---|---|
| **integer** | 153, 154, 99 (+ more, see `decode::products::PRODUCTS`) | halfwords 22 / 23 / 24 | min x10, increment x10, level count |
| **float32** | 159, 161, 163 | halfwords 22–25 | a float32 scale and offset |

This is an enum (`DecodeScheme`) in radish's decoder, not a boolean or a
free-text field — the type system makes a third, invalid state
unrepresentable, closing off exactly the kind of typo-based bug a
loosely-typed equivalent (e.g. a string compared case-sensitively) is
prone to.

### 4.4 Facts that are NOT in the file

Three values look like they're in the file and aren't:

- **Gate spacing for the original digital radial products is 250 m, and
  it does not come from the packet header.** The halfword that looks
  like a range-scale field is `cos(elevation) × 1000` — it reads 999 on
  a 0.5° tilt and 998 on a 3.1° one. Dividing it by 1000 and calling it
  kilometres is meaningless. Verified by cross-checking against a real
  NEXRAD Level 2 volume for the same site and time, and against the
  packet's own declared bin count × spacing. Other product families use
  a per-family fixed bin size instead (`decode::products::ProductSpec::bin_size`)
  — never derived from this field for elevation-bearing products.
- **First gate is 125 m** (half the gate spacing), the *center* of bin 0.
  Verified against an independent Level 2 archive for the same radar.
- **The tilt and the moment come from the AWIPS id in the text header**
  for the six verified products, not from the message code alone — a
  message code identifies the *format*, not the tilt (all six
  reflectivity tilts report message code 153). For the wider product set
  beyond those six, radish resolves the moment from the message code
  directly (deterministic and always correct) and leaves tilt
  unresolved rather than guessing from an unverified AWIPS-letter table.

### 4.5 Azimuths are ray CENTRES, not the stored start angles

The file stores each radial's **start** angle plus its angular width. A
correctly georeferenced sweep needs the **center** — `start + delta/2`.
Publishing raw start angles rotates every sweep by half a beamwidth
counter-clockwise — hundreds of meters of displacement at typical
ranges, growing toward the sweep edge. Verified byte-exact against a
real byte-level oracle on 7 real fixtures.

**Applies to packet 28 too — confirmed against the ICD text itself, not
assumed from packet 16/AF1F (plan 0012, §11).** NEXRAD ICD 2620001AC,
Appendix E, Figure E-4 ("Radial Information Data Structure") documents
packet 28's `Azimuth` field as "Azimuth of the LEADING EDGE of the
radial" — the identical convention, for a structurally unrelated packet
format. An earlier pass concluded no correction was needed there, from
reading two independent Python readers' low-level XDR-parsing code
(neither takes a position on centering at that layer) without checking
either reader's higher-level azimuth-exposing method — worth remembering
before trusting a reference implementation's silence as an answer.

### 4.6 Velocity is two products, split by tilt

The NWS splits velocity across two message codes — a one-letter-per-moment
map can't express this:

| letter | code | tilts | elevations |
|---|---|---|---|
| `G` | 154 | 0, 1, 2 | 0.5, 0.9, 1.3 |
| `U` | 99 | 3, 4, 5 | 1.8, 2.4, 3.1 |

Both declare min -63.5, increment 0.5, 254 levels.

### 4.7 The decompression bomb guard is not optional

`MAX_DECOMPRESSED_BYTES = 8 << 20`. Decompression is incremental with a
hard cap, because a small crafted input can expand without bound
otherwise, and the largest real product is only ~1.3 MB decompressed.

**This matters more in a browser than on a server.** A one-shot
decompress-to-completion call in wasm reopens a hole that's already
closed elsewhere, in a context where the input can come from any object
key a page can be pointed at, and the "victim" of a resource-exhaustion
bug is the user's own browser tab.

### 4.8 Code 0 and code 1 are not data

`DATA_FLOOR_CODE = 2` for `LinearHw`/`FloatScale`. In every digital
radial product using one of those TWO schemes, code 0 is "below
threshold" and code 1 is "range folded". Physical value is
`value_min + (code - 2) * value_increment`; codes below 2 are NaN.
`raw_codes` still carries the verbatim code either way, so a consumer
that wants to distinguish "below threshold" from "range folded" can —
that distinction isn't collapsed away, just not exposed as a separate
field radish would have to own and keep in sync.

**NOT universal beyond those two schemes (plan 0012, §11) — `Precip`/
`Rate` have their own, per-FILE floor**, read from a product-family flag-
count field further into the PDB (`pdb::precip_family_scale`), most
commonly `1` rather than `2` — confirmed against real `DAA`/`DTA`/`DU3`/
`DU6`/`DPR` fixtures. Applying this section's floor-of-2 to that family
would silently shift every physical value by one code, which reads
exactly like "implausible numbers" rather than an obviously-wrong crash.

### 4.9 An odd `n_bins` gets one halfword-alignment pad byte, not a dropped gate

Packet 16's own header field (`n_bins`, "Number of Range Bins") is
always the true, authoritative gate count — a per-radial byte count one
larger than that is a documented, expected pad byte, not evidence the
header is wrong. NEXRAD ICD 2620001AC, Figure 3-11c ("Digital Radial
Data Array Packet - Packet Code 16"), Note 1: *"The RPG clips radials
to 70 kft. This could result in an odd number of bins in a radial.
However, the radial will always be on a halfword boundary, so the
number of bytes in a radial may be number of bins in a radial + 1."*
radish reads only the first `n_bins` bytes of each radial and discards
the rest (`decode_symbology_tolerates_pad_byte_beyond_n_bins` in
`decode/symbology.rs`), matching Note 1 directly. Verified empirically,
not just cited: cross-checked against real fixtures with an odd
`n_bins` (N1B, N3B — both KLOT super-res reflectivity tilts) — every
radial in both files declares exactly one extra byte, and that byte is
0 on all of them, while 11 other real fixtures with an even `n_bins`
show zero header/byte-count disagreement at all. This is also a
documented divergence from xradar's PR #392 branch (commit `9c8826c`
at the time this was checked): its `_read_packet16` does not implement
Note 1 and silently widens to the padded byte count instead — see
`radish/tests/fixtures/CORPUS.md`'s xradar cross-check section and
`generate_expected_xradar.py` for how the golden sidecars correct for
that before comparison.

---

## 5. Phase A — `radish` compiles to `wasm32-unknown-unknown`

**Status: done.**

`radish/Cargo.toml` gained a `[features]` table: `default = ["native"]`,
`native = ["dep:hdf5", "dep:netcdf", "dep:rayon"]`. `hdf5`/`netcdf` (C
libraries, no wasm story at all) and `rayon` (needs
`wasm-bindgen-rayon` + cross-origin-isolation headers, a real deployment
constraint not worth imposing on a static-hosted client) are now
`optional = true` and gated behind `native`. `bzip2` was already
pure-Rust (`libbz2-rs-sys`, the crate's default backend since 0.5) — no
change needed there.

CI (`.github/workflows/rust-ci.yml`'s `wasm` job) runs
`cargo check -p radish --no-default-features --target wasm32-unknown-unknown`
and separately asserts `netcdf`/`hdf5-metno-sys`/`libz-sys`/`rayon` never
appear in the dependency graph for either `radish` core or the
`radish-wasm` crate — so a future PR reintroducing an unconditional
`hdf5`/`netcdf` import, or a workspace-dependency change that silently
re-enables `native` for the wasm crate (a real bug found and fixed during
this work — see §8), fails loudly rather than silently regressing.

---

## 6. Phase B — the Level 3 backend

**Status: done.** `radish/src/backends/nexrad_level3/`, following the
existing backend pattern (`docs/ARCHITECTURE.md`, "Backend Implementation
Pattern"), reusing `backends/common/` rather than new byte-reading
helpers.

- Content-based sniffing: NIDS files begin with a WMO/AWIPS text header,
  separator `\r\r\n`, and a 6-char alphanumeric AWIPS id token — the
  sniff keeps scanning rather than committing to the first 6-char token
  found, since a NOAAPORT-style prefix line can put an unrelated one
  ahead of the real AWIPS id.
- Message header + PDB located by *validating*, not pattern-searching:
  the message code at the candidate offset must be a known one AND the
  product description block must be internally self-consistent.
- Both scaling forms (§4.3), dispatched on `DecodeScheme`, an exhaustive
  enum.
- Packet 16 (digital radial data array), packet `AF1F` (RLE radials, the
  legacy 8/16-level product family), packet 28 (XDR generic data packet,
  `u16` raw levels — `decode::nexrad_level3::xdr`), and incremental
  capped bzip2 decompression (§4.7). See §11 for how packet 28 and the
  `Precip`/`Rate` schemes closed.
- Product table: 26 message codes from xradar #392's table, ALL
  implemented past the PDB stage as of plan 0012 (§11) — an unrecognised
  message code still returns a named error rather than guessing.
- Model output: radish's normal CfRadial2/FM301 physical-value model,
  plus the raw-code accessors from §4.1 — both, not either.

---

## 7. Phase C — the parity gate

**Status: done.** Two tiers:

**Tier 1 — byte-exact against a real byte-level oracle (CI-blocking).**
7 real, unmodified NIDS fixtures pulled from `s3://unidata-nexrad-level3`
(documented in `radish/tests/fixtures/CORPUS.md`, not committed —
env-var-resolved with SHA-256 pinning, matching the existing Level 2
corpus convention), covering both scaling forms, both velocity message
codes, and two different azimuth resolutions. Expected output is
generated from the oracle decoder directly (never hand-derived), and
compared byte-for-byte against radish's decode via a committed SHA-256
sidecar (`radish/tests/fixtures/nexrad_level3/expected/`,
`radish/tests/test_nexrad_level3_parity.rs`). A dedicated sabotage-verify
test perturbs a known-good value and confirms the comparison actually
goes red — a parity test that has never failed has not been shown to
work.

**Tier 2 — value parity against xradar #392's independent, synthetic,
byte-built test vectors (tracked, advisory)** for the codes beyond the
original six (`radish/tests/test_nexrad_level3_xradar_vectors.rs`).

**Exit, verified**: CI cannot go green with a decoder that disagrees with
the byte-level oracle on the original six products.

---

## 8. Phase D — the wasm binding

**Status: done.**

New crate `wasm/` (sibling of `python/`), mirroring how the pyo3 binding
is structured.

- `crate-type = ["cdylib"]`, `wasm-bindgen`, and `radish` with
  `default-features = false` — as a **direct `path` dependency**, not
  `{ workspace = true, default-features = false }`: Cargo only honours a
  member's `default-features = false` override on a workspace-inherited
  dependency if the *workspace-level* entry also declares
  `default-features = false`, which this workspace's doesn't (every
  other member needs the default `native` feature). The
  workspace-inherited form silently no-ops — caught via
  `cargo tree -p radish-wasm`, which still pulled in `hdf5`/`netcdf`/
  `rayon` before this was fixed. CI now asserts this directly.
- Exports `codes()` as **zero-copy**: `js_sys::Uint8Array::view` over the
  wasm linear memory backing the decoded product's raw codes array.
  Lifetime documented on the method itself (invalidated if the
  `DecodedProduct` instance is freed, or wasm memory grows, before the
  caller reads/copies it).
- Also exports `dealiasRegionBased` (velocity + mask in, fold-count
  `Int32Array` out) — see §"Region-based velocity dealiasing" below for
  why a corrected-velocity display needs both decode and dealiasing with
  no server in the path.
- **Library, not an application.** No fetch, no S3, no worker logic —
  bytes (or typed arrays) in, decoded/dealiased sweep out.
- **Measured, not assumed** (Node.js harness, `wasm-bindgen --target
  nodejs` + `performance.now()`, real fixtures):

  | Metric | Value |
  |---|---|
  | Raw `.wasm`, rustc release (opt-level 3, LTO, strip) | 170,470 B |
  | After `wasm-opt -O3 --all-features` | 159,237 B |
  | After `wasm-opt -Oz --all-features` | 158,038 B |
  | Gzip -9, raw | 63,935 B |
  | Gzip -9, after `wasm-opt -O3` | 64,017 B |
  | Gzip -9, after `wasm-opt -Oz` | 64,184 B |
  | `decodeNexradLevel3`, real 224,559 B NIDS product (720×1840) | 21.9 ms median (n=50) |
  | `dealiasRegionBased`, real velocity sweep (720×1192, 225,842 nonzero folds) | 578.3 ms median (n=10) |

  **The gzip-size finding is worth stating plainly, since it contradicts
  the intuitive assumption**: `wasm-opt`'s size optimizations shrink the
  *raw* binary (170,470 → 158,038 B, ~7% smaller at `-Oz`) but very
  slightly *increase* the *gzipped* size (63,935 → 64,184 B) — code
  restructured for compactness can reduce the repetition gzip's LZ77
  window would otherwise compress well. Since browsers transfer the
  gzipped (or brotli'd) bytes, not the raw ones, `wasm-opt` here is close
  to a wash on transfer size for this module and buys nothing worth
  citing as a win — recorded honestly rather than assuming `-O3`/`-Oz`
  "obviously" helps. All figures are single-machine measurements, not a
  cross-platform benchmark claim.

  The dealiasing figure (fold count 225,842) matches the native Rust
  criterion benchmark and the Python-binding smoke test exactly — real
  end-to-end confirmation the wasm build's output is bit-identical to
  native, not just "runs without crashing."

**Exit, verified**: a browser (Node.js standing in for one) decodes and
dealiases a real NIDS product and a real velocity sweep with no server
in the path, with real size/latency numbers.

---

## Region-based velocity dealiasing

**Status: done.** `radish::transforms::dealias_region_based` is a Rust
port of Py-ART's `pyart.correct.dealias_region_based` (region growing +
4-connected-component labeling + a dynamic edge-network reduction),
bit-exact with Py-ART on every unmasked gate — verified against a real
Py-ART install on real NEXRAD Level 2 velocity sweeps, not only synthetic
cases (`radish/tests/test_dealias_parity.rs`).

This is in scope alongside decode for the same reason both target wasm:
a raw NEXRAD Level 3 velocity product is genuinely folded (NOAA's RPG
doesn't ship a pre-dealiased Level 3 product), and an unfolded velocity
display needs region-based dealiasing to be usable — masking only
below-threshold gates and not range-folded ones leaves every folded gate
painting as a maximum-velocity artifact that reads as a false rotation
signature. A server-backed deployment can run Py-ART directly; a
serverless, browser-only one needs the same algorithm reachable from
wasm, which is what this module provides.

`dealias_region_based` operates on already-decoded velocity — it doesn't
touch the decode path, and dealiasing is a transform a caller runs on
demand rather than a decode-time output (no new field on any decode-side
model type). `valid_mask` uses the opposite polarity from Py-ART's own
`gfilter` (`true` = valid/usable) to match Rust convention — documented
prominently at every layer it crosses (Rust core, PyO3, wasm), since
getting this backwards silently inverts every result.

Deliberately not ported: the sounding-anchored `ref_vel_field` path
(L-BFGS-B reference-velocity fitting) — rarely used, and has no obvious
wasm-friendly pure-Rust story.

Reachable from Python as `radish.dealias_region_based(...)` and from the
wasm crate as `dealiasRegionBased(...)`. ~8x faster than Py-ART's own
implementation on the same real sweep (`radish/benches/dealias.rs`).

---

## 9. What this deliberately does NOT do

- **No networking, no S3 listing, no caching.** A consuming application
  owns all of that. radish takes bytes (or typed arrays) in, and returns
  decoded/dealiased data out.
- **No rendering, no geometry-for-GPU, no `to_vertices` API.** How a
  consumer renders the decoded codes — texture upload, shader-based
  color mapping, vertex geometry, whatever fits its rendering pipeline —
  is display-layer policy this library doesn't own.
- **No fitted azimuth slope.** The wasm binding exports the general
  per-radial azimuth array, not a fitted `az_start_deg`/`az_step_deg`
  pair a specific renderer might want — that fit is application-layer
  policy, not decode-side knowledge.
- **No opinion on how a consumer re-quantizes or serves dealiased
  velocity codes.** The raw-codes-passthrough contract (§4.1) doesn't
  extend past ±Nyquist once a gate has been unfolded — how a caller
  re-quantizes and serves that is its own wire-format decision.

---

## 10. Reference map

| path | what |
|---|---|
| `docs/ARCHITECTURE.md` | backend trait, data model, the pattern a new backend follows |
| `radish/src/backends/nexrad/` | the Level 2 decoder — closest existing analogue |
| `radish/src/backends/nexrad_level3/` | this backend |
| `radish/src/transforms/dealias/` | the dealiasing port |
| `radish/src/backends/common/` | shared byte-reading, coords, geometry, sniffing |
| `radish/Cargo.toml` | the `[features]` table from Phase A |
| `python/Cargo.toml`, `wasm/Cargo.toml` | the two bindings |
| `radish/tests/fixtures/CORPUS.md` | fixture corpus documentation (NEXRAD L2, NIDS, and dealiasing) |
| `plans/0001-nexrad-level2-backend.md` … | how previous backends were planned in this repo |

### External

- [openradar/xradar PR #392][xr392] — the Python NIDS reader used as a
  breadth/value-parity reference.
- NEXRAD Level 3 ICD (NOAA 2620001) — the format specification. §4 above
  lists several places where the ICD's field names mislead.
- Py-ART's `pyart.correct.dealias_region_based` — the dealiasing oracle.

---

## 11. The 7 formerly-deferred codes — closed (2026-08-09, plan 0012)

Raised while scoping `radar-animation`'s free-tier product expansion
(`plans/level3-product-expansion.md` in that repo — this backend's
consumer, not this one), tracked here since §6 named the 7 deferred codes
without saying what closing each one needed, then closed by
`plans/0012-nexrad-level3-deferred-codes.md` (this repo). All 7 decode
today; `packet_family_implemented` returns `true` for every code in
`PRODUCTS`. What follows is what was actually true, not the assumptions
this section originally recorded — two of those assumptions turned out to
be wrong, corrected only by reading real bytes.

- **170/172/173/174/175** (`DAA`/`DTA`/`DU3`+`DU6`/`DOD`/`DSD` — the
  digital precip-accumulation family, `DecodeScheme::Precip`). §6's
  "packet type unconfirmed" is resolved: **packet 16**, confirmed by
  reading the raw symbology-block packet-code halfword directly on 4 real
  objects (`DAA`, `DTA`, `DU3`, `DU6`). The PDB scale field IS the same
  8-byte float32 pair `FloatScale` reads (halfwords 22-25) — what earlier
  attempts got wrong was the FLOOR code, not the scale field: this family
  does not use the universal `DATA_FLOOR_CODE = 2` §4.8 documents for
  every OTHER packet-16 scheme. It has its own per-file leading/trailing
  flag-count field further into the PDB (halfwords 27-29), read by
  `pdb::precip_family_scale` — real fixtures show `leading = 1` (not 2),
  so an earlier attempt computing `(raw - 2) * scale + offset` would have
  been systematically wrong by exactly one code, which reads exactly like
  "implausible numbers" while the scale floats themselves parse fine.
  `172`/`DTA` has a caveat: xradar's own PR-392 reader (used as a
  cross-check oracle for the other four) warns that its handling of
  `DTA`'s product version 3 is unverified, so 172 has no byte-exact
  real-fixture oracle confirmation of its own — see
  `generate_expected_xradar.py`'s module doc. It shares the identical,
  code-agnostic decode path the other four ARE confirmed on, so this is a
  residual gap in evidence, not a known or suspected bug.
- **176** (`DPR`, Digital Instantaneous Precipitation Rate —
  `DecodeScheme::Rate`). Packet 28 (XDR), `u16` raw levels, confirmed
  against a real object. This needed the real, additive model change §6
  anticipated: `MomentData::raw_codes_u16: Option<Array2<u16>>`,
  alongside (not replacing) `raw_codes` — see `model/moment.rs`. Also
  needed a from-scratch XDR (RFC 1832) unpacker
  (`decode::nexrad_level3::xdr`) with every length-prefixed read bounded
  against a cap BEFORE allocating, the same decompression-bomb discipline
  §4.7 already required of `decompress_bzip2_capped`, extended to XDR's
  own untrusted length prefixes. The `azimuth` field's convention took
  TWO passes to get right: the first concluded it needed no
  `+ width/2` correction (packet 16/AF1F's, from `azimuth_centre_deg`),
  based on reading two independent Python readers' low-level XDR-parsing
  code, both of which store the field verbatim and take no position on
  centering — without checking either reader's HIGHER-level
  azimuth-exposing method. The actual NEXRAD ICD (2620001AC, Appendix E,
  Figure E-4) settles it: `Azimuth` is documented as "Azimuth of the
  LEADING EDGE of the radial" for this exact packet-28 field — the SAME
  correction packet 16/AF1F need, not a different convention. Verified
  byte-exact (codes AND azimuths) against a real `DPR` object and xradar's
  independent reader after the fix
  (`test_nexrad_level3_xradar_oracle.rs::decode_matches_xradar_raw_data_u16`).
- **177** (`HHC`, Hybrid Hydrometeor Classification —
  `DecodeScheme::ClassInt`, the SAME scheme as `HCLASS`/165). **Packet
  16, not packet 28.** This section previously recorded the opposite,
  and explicitly warned a future reader to "trust
  `packet_family_implemented`/the real decode result over a one-off
  manual byte read" — that warning had it backwards. `packet_family_implemented`
  returning `false` for 177 was never evidence of packet 28: it's gated
  purely on the MESSAGE code, before symbology is ever parsed, so
  `UnsupportedProduct` fires identically regardless of what packet type
  the file actually uses — decoding a real object never happened before
  this pass. Once actually checked — 3 independently-fetched real `HHC`
  objects, byte-level packet-code read — all 3 declared packet 16.
  Consequence: 177 needed almost no new code at all, just removing the
  `code != 177` exclusion in `packet_family_implemented`'s `ClassInt` arm
  — it decodes through the exact same path 165 already used, with
  `has_elevation: false` (already correctly set in `PRODUCTS`) the only
  real difference. The lesson, stated plainly for whoever reads this
  next: a deferred/unsupported result is evidence about what this
  backend HASN'T checked, never evidence about what the file actually
  contains.

---

[xr392]: https://github.com/openradar/xradar/pull/392
