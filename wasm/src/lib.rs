//! wasm-bindgen bindings for `radish`'s NEXRAD Level 3 (NIDS) decoder and
//! region-based velocity dealiasing — the browser-reachable surface for a
//! serverless, browser-only NEXRAD Level 3 client. See
//! `docs/NEXRAD_LEVEL3_WASM.md` for the full design this crate
//! implements.
//!
//! **Library only.** No `fetch`, no S3, no worker logic — bytes in,
//! decoded/dealiased sweep out. The calling JavaScript/TypeScript owns
//! everything upstream (fetching NIDS bytes over the network) and
//! downstream (rendering, GPU geometry — see `docs/NEXRAD_LEVEL3_WASM.md`
//! §9's "no `to_vertices` API").
//!
//! **Boundary clarification, not in the design doc explicitly**: a caller
//! wanting a fitted `az_start_deg`/`az_step_deg` slope pair (rather than a
//! per-radial array) has to derive it itself. This crate exports the
//! **general per-radial azimuth array** ([`DecodedProduct::azimuths`]) and
//! leaves slope-fitting to the caller — that's application-layer display
//! policy, not decode-side knowledge.

use ndarray::Array2;
use wasm_bindgen::prelude::*;

use js_sys::{Float32Array, Int32Array, Uint16Array, Uint8Array};
use radish::backends::{NexradLevel3Backend, RadarBackend};
use radish::transforms::{dealias_region_based as rs_dealias_region_based, DealiasOptions};
use radish::RadishError;

/// Convert a [`RadishError`] into a JS `Error` with `.name` set to the
/// variant — mirrors `python/src/lib.rs`'s `dealias_err`/`demux_err`
/// pattern (match on the variant, not just stringify it), so a JS caller
/// can branch on `err.name` (e.g. `"Dealias"` = fix your call site,
/// `"Decode"` = the input bytes are bad) instead of pattern-matching
/// English text out of `err.message`. Found lacking via adversarial
/// review this session — this function used to flatten every variant
/// into an opaque string, discarding information the Python binding
/// already preserves for the identical underlying error type.
///
/// Lists every variant this crate's own `decode_nexrad_level3`/
/// `dealias_region_based` can actually produce, plus a fallback `_` arm
/// for `RadishError::Hdf5`/`RadishError::NetCdf` specifically — NOT
/// because this crate ever enables radish's `native` feature (it never
/// does; see `wasm/Cargo.toml`), but because Cargo's feature unification
/// means the two variants can still exist in `RadishError`'s type when
/// this crate is compiled as part of a larger cargo invocation that ALSO
/// builds `radish` (or `radish-python`) with `native` enabled — e.g.
/// `cargo clippy --workspace --all-features` — even though those two
/// variants are never actually *constructible* from any code path this
/// crate calls. Confirmed by that exact command failing to compile
/// without this arm, despite `cargo build -p radish-wasm --target
/// wasm32-unknown-unknown` (the real, isolated CI check) never having
/// needed it.
fn radish_err(e: RadishError) -> JsValue {
    let name = match &e {
        RadishError::Io(_) => "Io",
        RadishError::InvalidFormat(_) => "InvalidFormat",
        RadishError::MissingAttribute(_) => "MissingAttribute",
        RadishError::MissingVariable(_) => "MissingVariable",
        RadishError::InvalidSweepIndex(_) => "InvalidSweepIndex",
        RadishError::Conversion(_) => "Conversion",
        RadishError::MalformedRecord { .. } => "MalformedRecord",
        RadishError::Decode(_) => "Decode",
        RadishError::MomentEncoding(_) => "MomentEncoding",
        RadishError::Unsupported(_) => "Unsupported",
        RadishError::Dealias(_) => "Dealias",
        RadishError::General(_) => "General",
        #[allow(unreachable_patterns)]
        _ => "Native",
    };
    let err = js_sys::Error::new(&e.to_string());
    err.set_name(name);
    err.into()
}

/// A local (non-`RadishError`) failure — this module's own precondition
/// checks (missing sweeps/moments in a decoded product, a shape mismatch
/// building an `Array2` from a flat JS array) that don't have a
/// `RadishError` variant of their own. Same `.name`-tagging convention as
/// [`radish_err`], with the name passed explicitly since there's no
/// variant to derive it from.
fn local_err(name: &str, msg: impl std::fmt::Display) -> JsValue {
    let err = js_sys::Error::new(&msg.to_string());
    err.set_name(name);
    err.into()
}

/// One decoded NEXRAD Level 3 (NIDS) product — a single sweep, single
/// moment (see `docs/NEXRAD_LEVEL3_WASM.md` §4.1: a NIDS file is its own
/// single-sweep, single-moment volume, not one cut of a larger VCP).
/// Returned by [`decode_nexrad_level3`].
#[wasm_bindgen]
pub struct DecodedProduct {
    site: String,
    awips_id: String,
    moment: String,
    message_code: u16,
    vcp: u16,
    tilt: Option<u8>,
    elevation_deg: f64,
    lat: f64,
    lon: f64,
    height_m: f64,
    first_gate_m: f32,
    gate_spacing_m: f32,
    n_radials: usize,
    n_bins: usize,
    azimuths: Vec<f32>,
    /// Exactly one of `codes`/`codes_u16` is `Some`, matching
    /// `radish::MomentData`'s own additive-field contract (see
    /// [`Self::codes_width`]'s doc) — kept as two `Option` fields here,
    /// not an internal `RawCodes`-style enum, because wasm-bindgen structs
    /// can't export enum-typed fields directly to JS the way plain getters
    /// can.
    codes: Option<Array2<u8>>,
    codes_u16: Option<Array2<u16>>,
    value_min: Option<f32>,
    value_increment: Option<f32>,
    n_levels: Option<u16>,
}

#[wasm_bindgen]
impl DecodedProduct {
    /// The verbatim on-wire codes, `[n_radials * n_bins]` bytes, row-major
    /// (radial-major, matching [`Self::n_radials`]/[`Self::n_bins`]) — for
    /// a packet 16/AF1F product (`codesWidth() === 8`). **Empty**
    /// (zero-length, not `undefined`) for a packet-28 product
    /// (`codesWidth() === 16`, e.g. `RATE`) — check `codesWidth()` first;
    /// this is deliberately NOT `Option<Uint8Array>`/an exception, to keep
    /// this method's signature identical to every version of this crate
    /// before packet 28 existed (plan 0012 §0's corrected, additive-only
    /// scope for this touchpoint — no existing caller, which only ever
    /// decoded packet 16/AF1F products, observes any change here).
    ///
    /// **Zero-copy.** This is a live view (`js_sys::Uint8Array::view`)
    /// directly into this `DecodedProduct` instance's own WebAssembly
    /// linear memory — not a copy. It stays valid only as long as BOTH of
    /// these hold: (1) this `DecodedProduct` instance itself hasn't been
    /// freed (don't call `.free()` — or let it be garbage-collected via
    /// `FinalizationRegistry`, if using that pattern — before you're done
    /// reading), and (2) no wasm memory growth has happened since this
    /// call (calling back into ANY wasm export, including another decode,
    /// can grow memory and detach every previously-returned view). Copy
    /// the bytes out (e.g. `Uint8Array.slice()` or `new Uint8Array(view)`)
    /// before doing anything else if you need them to outlive that
    /// window.
    ///
    /// # Safety
    /// The `unsafe` block is `js_sys::Uint8Array::view`'s own contract:
    /// the returned typed array aliases `self.codes`'s backing buffer,
    /// which stays valid exactly as long as `self` (this struct's Rust
    /// allocation) isn't dropped and the wasm heap isn't grown — both
    /// documented above for the JS caller.
    #[wasm_bindgen(js_name = codes)]
    pub fn codes(&self) -> Uint8Array {
        match self.codes.as_ref() {
            Some(codes) => {
                let flat = codes
                    .as_slice()
                    .expect("freshly-decoded Array2<u8> is always contiguous, row-major");
                // SAFETY: see the doc comment above — valid as long as
                // `self` isn't dropped and wasm memory isn't grown before
                // the caller reads or copies the view.
                unsafe { Uint8Array::view(flat) }
            }
            None => Uint8Array::new_with_length(0),
        }
    }

    /// The `u16` counterpart to [`Self::codes`], for a packet-28 product
    /// (`codesWidth() === 16`, e.g. `RATE`) — empty for a packet 16/AF1F
    /// product. Same zero-copy contract as `codes()`.
    ///
    /// # Safety
    /// See `codes()`'s `# Safety` section — identical contract, aliasing
    /// `self.codes_u16` instead of `self.codes`.
    #[wasm_bindgen(js_name = codesU16)]
    pub fn codes_u16(&self) -> Uint16Array {
        match self.codes_u16.as_ref() {
            Some(codes) => {
                let flat = codes
                    .as_slice()
                    .expect("freshly-decoded Array2<u16> is always contiguous, row-major");
                // SAFETY: see `codes()`'s doc comment.
                unsafe { Uint16Array::view(flat) }
            }
            None => Uint16Array::new_with_length(0),
        }
    }

    /// `8` if [`Self::codes`] is the accessor to call for this product's
    /// raw codes, `16` if [`Self::codes_u16`] is — a decoded product's raw
    /// codes are always exactly one width; this is how a caller tells
    /// which of the two zero-copy accessors actually has data before
    /// calling either, rather than probing both and checking for an empty
    /// array.
    #[wasm_bindgen(js_name = codesWidth, getter)]
    pub fn codes_width(&self) -> u8 {
        if self.codes.is_some() {
            8
        } else {
            16
        }
    }

    /// Per-radial azimuth, ray center, degrees — NOT a fitted
    /// `az_start_deg`/`az_step_deg` slope; see this module's doc for why.
    #[wasm_bindgen(js_name = azimuths)]
    pub fn azimuths(&self) -> Float32Array {
        Float32Array::from(self.azimuths.as_slice())
    }

    #[wasm_bindgen(getter)]
    pub fn site(&self) -> String {
        self.site.clone()
    }

    #[wasm_bindgen(js_name = awipsId, getter)]
    pub fn awips_id(&self) -> String {
        self.awips_id.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn moment(&self) -> String {
        self.moment.clone()
    }

    #[wasm_bindgen(js_name = messageCode, getter)]
    pub fn message_code(&self) -> u16 {
        self.message_code
    }

    #[wasm_bindgen(getter)]
    pub fn vcp(&self) -> u16 {
        self.vcp
    }

    /// `0..=5` tilt ordinal, only for the AWIPS letters radish has an
    /// independently-verified table for — `undefined` otherwise (never
    /// guessed from elevation angle). [`Self::elevation_deg`] is always
    /// populated regardless.
    #[wasm_bindgen(getter)]
    pub fn tilt(&self) -> Option<u8> {
        self.tilt
    }

    #[wasm_bindgen(js_name = elevationDeg, getter)]
    pub fn elevation_deg(&self) -> f64 {
        self.elevation_deg
    }

    #[wasm_bindgen(getter)]
    pub fn lat(&self) -> f64 {
        self.lat
    }

    #[wasm_bindgen(getter)]
    pub fn lon(&self) -> f64 {
        self.lon
    }

    #[wasm_bindgen(js_name = heightM, getter)]
    pub fn height_m(&self) -> f64 {
        self.height_m
    }

    #[wasm_bindgen(js_name = firstGateM, getter)]
    pub fn first_gate_m(&self) -> f32 {
        self.first_gate_m
    }

    #[wasm_bindgen(js_name = gateSpacingM, getter)]
    pub fn gate_spacing_m(&self) -> f32 {
        self.gate_spacing_m
    }

    #[wasm_bindgen(js_name = nRadials, getter)]
    pub fn n_radials(&self) -> usize {
        self.n_radials
    }

    #[wasm_bindgen(js_name = nBins, getter)]
    pub fn n_bins(&self) -> usize {
        self.n_bins
    }

    /// Physical value at the data floor code, or `undefined` for a
    /// categorical product (its codes index a fixed label table, not a
    /// linear scale — see `radish::DeclaredScale`'s doc).
    #[wasm_bindgen(js_name = valueMin, getter)]
    pub fn value_min(&self) -> Option<f32> {
        self.value_min
    }

    #[wasm_bindgen(js_name = valueIncrement, getter)]
    pub fn value_increment(&self) -> Option<f32> {
        self.value_increment
    }

    #[wasm_bindgen(js_name = nLevels, getter)]
    pub fn n_levels(&self) -> Option<u16> {
        self.n_levels
    }
}

/// Decode a raw NEXRAD Level 3 (NIDS) product's bytes into a
/// [`DecodedProduct`]. Bytes-in, decoded-sweep-out — the same contract
/// `radish::backends::NexradLevel3Backend::read_bytes_volume` has
/// natively; this is a thin wasm-bindgen wrapper around it.
#[wasm_bindgen(js_name = decodeNexradLevel3)]
pub fn decode_nexrad_level3(bytes: &[u8]) -> Result<DecodedProduct, JsValue> {
    let volume = NexradLevel3Backend::new()
        .read_bytes_volume(bytes.to_vec())
        .map_err(radish_err)?;

    let sweep = volume
        .sweeps
        .into_iter()
        .next()
        .ok_or_else(|| local_err("Decode", "decoded volume has no sweeps"))?;
    let nids = sweep
        .metadata
        .nids
        .ok_or_else(|| local_err("Decode", "decoded sweep has no NIDS attrs"))?;
    let (moment_name, moment) = sweep
        .moments
        .into_iter()
        .next()
        .ok_or_else(|| local_err("Decode", "decoded sweep has no moments"))?;
    // `MomentData::shape()` already gives `(num_rays, num_gates)` from
    // `data` — always populated, and always the same dims as whichever
    // `raw_codes`/`raw_codes_u16` is set (`decode/mod.rs` derives
    // `physical` via `codes.mapv(...)` on that same array), so there's no
    // need to hand-inspect the raw-code fields to recover this.
    let (n_radials, n_bins) = moment.shape();

    let first_gate_m = sweep.coordinates.range.first().copied().unwrap_or(0.0);
    let gate_spacing_m = if sweep.coordinates.range.len() >= 2 {
        sweep.coordinates.range[1] - sweep.coordinates.range[0]
    } else {
        0.0
    };

    Ok(DecodedProduct {
        site: nids.site,
        awips_id: nids.awips_id,
        moment: moment_name,
        message_code: nids.message_code,
        vcp: nids.vcp,
        tilt: nids.tilt,
        elevation_deg: sweep.metadata.fixed_angle,
        lat: volume.metadata.latitude,
        lon: volume.metadata.longitude,
        height_m: volume.metadata.altitude,
        first_gate_m,
        gate_spacing_m,
        n_radials,
        n_bins,
        azimuths: sweep.coordinates.azimuth,
        codes: moment.raw_codes,
        codes_u16: moment.raw_codes_u16,
        value_min: moment.declared_scale.map(|s| s.value_min),
        value_increment: moment.declared_scale.map(|s| s.value_increment),
        n_levels: moment.declared_scale.map(|s| s.n_levels),
    })
}

/// Dealias one sweep's Doppler velocity using Py-ART's region-based
/// algorithm — bit-exact with `pyart.correct.dealias_region_based` on
/// every unmasked gate (see `radish/tests/test_dealias_parity.rs` for the
/// parity gate this is checked against). Reachable from wasm precisely
/// because a serverless, browser-only deployment has no server to run
/// Py-ART on — see `docs/NEXRAD_LEVEL3_WASM.md`.
///
/// `velocity` and `valid_mask` are flat, row-major, `n_rays * n_gates`
/// long. `valid_mask[i] != 0` means gate `i` is valid and should be
/// dealiased (the opposite polarity of Py-ART's own `gfilter`, which
/// excludes on nonzero — see `radish::transforms::dealias_region_based`'s
/// Rust doc for why this binding keeps that convention).
///
/// Returns per-gate fold counts (`Int32Array`, same flat row-major
/// layout), not corrected velocities — `corrected = velocity + folds *
/// 2.0 * nyquist` is a cheap multiply-add the caller does itself. Copied
/// (not zero-copy) into a new JS-owned array: fold arrays are far smaller
/// than the codes array `DecodedProduct::codes` exists to avoid copying,
/// so the extra copy here isn't worth the same lifetime-hazard tradeoff.
#[wasm_bindgen(js_name = dealiasRegionBased)]
#[allow(clippy::too_many_arguments)]
pub fn dealias_region_based(
    velocity: &[f32],
    valid_mask: &[u8],
    n_rays: usize,
    n_gates: usize,
    nyquist: f32,
    rays_wrap_around: bool,
    interval_splits: usize,
    skip_between_rays: i32,
    skip_along_ray: i32,
    centered: bool,
) -> Result<Int32Array, JsValue> {
    let expected_len = n_rays
        .checked_mul(n_gates)
        .ok_or_else(|| local_err("Dealias", "n_rays * n_gates overflowed"))?;
    if velocity.len() != expected_len || valid_mask.len() != expected_len {
        return Err(local_err(
            "Dealias",
            format!(
                "velocity ({} elements) and valid_mask ({} elements) must both be \
                 n_rays * n_gates = {expected_len} elements",
                velocity.len(),
                valid_mask.len(),
            ),
        ));
    }

    let velocity = Array2::from_shape_vec((n_rays, n_gates), velocity.to_vec())
        .map_err(|e| local_err("Dealias", e))?;
    let valid = Array2::from_shape_vec(
        (n_rays, n_gates),
        valid_mask.iter().map(|&b| b != 0).collect(),
    )
    .map_err(|e| local_err("Dealias", e))?;

    let opts = DealiasOptions {
        interval_splits,
        skip_between_rays,
        skip_along_ray,
        centered,
    };
    let folds = rs_dealias_region_based(&velocity, &valid, nyquist, rays_wrap_around, opts)
        .map_err(radish_err)?;

    let flat = folds
        .as_slice()
        .expect("freshly-computed Array2<i32> is always contiguous, row-major");
    Ok(Int32Array::from(flat))
}
