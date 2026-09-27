//! Low-level per-moment demultiplexer for NEXRAD Level 2.
//!
//! One LDM data record holds ~120 Message 31 radials with **every**
//! moment interleaved into the same byte range. The volume-level
//! readers ([`super::NexradBackend`]) always decode all of them into
//! physical-units `f32`. This module is the primitive underneath:
//! pull **one moment** out of **one record** (or one sweep-sized byte
//! span) as the raw integer words the RDA actually transmitted, so
//! chunked/lazy consumers — zarr codecs, virtual reference stores,
//! partial-volume reads — can decode exactly the bytes they need.
//!
//! # Cost model — read this before reaching for the sweep variant
//!
//! [`decode_record_moment`] takes an **already decompressed** record, so
//! it is pure byte-walking: ~0.06 ms for a 120-radial × 1832-gate
//! reflectivity block on a 2025 laptop. That is the shape this module
//! exists for — one moment, one chunk, called concurrently.
//!
//! [`decode_sweep_moment`] decompresses its span on **every call**.
//! Decoding N moments out of one span therefore costs N bzip2 passes,
//! which is decompression-bound: on a 5.8 MB KLOT volume one moment
//! takes ~134 ms while `NexradBackend::read_volume` decodes *all* of
//! them in ~181 ms. Use the sweep variant when you want one or two
//! moments out of a sweep-sized span; use the volume readers when you
//! want everything. The per-record decompression is rayon-parallel
//! (~5× on 8 cores) — see `CLAUDE.md`, that speedup is a regression
//! gate.
//!
//! # Output contract
//!
//! Arrays are row-major `(rays, gates)` raw words. The caller applies
//! the CF transform themselves, exactly as xradar does:
//!
//! ```text
//! physical = raw / scale - offset / scale
//!          = raw * scale_factor + add_offset
//! ```
//!
//! * One row per Message 31 radial, in record order. MSG_2 / MSG_5 and
//!   any other message type consume no row.
//! * Rows past the radial count, and rows where the moment block is
//!   absent from that radial, are set to `fill_value`.
//! * A present row writes `gate_count` decoded words then pads to the
//!   end of the row — **not** with `fill_value`. Without a remap the
//!   pad is raw `0`, matching xradar's
//!   `np.pad(..., constant_values=0)` byte for byte, which keeps radish
//!   bit-identical on short moments (ZDR/PHI/RHO routinely cover fewer
//!   gates than REF in the same sweep). With a remap the pad is the
//!   remap of raw `0`, so it still decodes to the physical value a
//!   source raw `0` would have.
//! * `gate_count > gates` is an error, never a silent truncation.
//!
//! # Masking (ICD §3.2.4.17.6 Table XVII-I)
//!
//! Raw words are masked to their significant bits before any remap:
//!
//! | Moment | Word size | Mask     |
//! |--------|----------:|----------|
//! | `PHI`  |        16 | `0x03FF` |
//! | `ZDR`  |        16 | `0x07FF` |
//! | any    |         8 | `0x00FF` |
//! | other  |        16 | `0xFFFF` |
//!
//! **Deviation from xradar, deliberate.** xradar's mask branch
//! (`nexrad_level2.py`, `NexradLevel2ArrayWrapper._getitem`) falls back
//! to `np.uint8(0xFF)` for anything that isn't PHI/16 or ZDR/16, and
//! numpy applies that to a `>u2` array by promoting to uint16 and
//! keeping the low 8 bits — so a 16-bit REF/VEL/SW/RHO would be
//! silently truncated. Those moments are 8-bit in every file in the
//! wild today, so radish stays bit-identical to xradar on real data;
//! we just don't reproduce the truncation if one ever ships 16-bit.
//!
//! # Encoding varies across RDA builds
//!
//! NEXRAD moment encodings are not constant. KVNX flipped ZDR from
//! `word_size=8, scale=16.0, offset=128.0` to
//! `word_size=16, scale=32.0, offset=418.0` across a 7.7 h RDA upgrade
//! outage on 2020-06-02. Any array-shaped target pins a single dtype
//! and a single `scale_factor`/`add_offset`, so this module reads each
//! block's own descriptor and either remaps onto the caller's grid or
//! refuses — it never approximates.
//!
//! With [`DemuxOptions::target`] set to `(scale_t, offset_t)`, and the
//! block declaring `(scale_s, offset_s)`:
//!
//! ```text
//! ratio = scale_t / scale_s
//! bias  = offset_t - offset_s * ratio
//! raw_t = raw_s * ratio + bias
//! ```
//!
//! The remap is accepted only when `ratio` and `bias` are both
//! non-negative integers and the widest masked source word still fits
//! the output width. For the KVNX ZDR case that is
//! `ratio = 2, bias = 162` → `raw16 = 2 * raw8 + 162`, exact in
//! physical units. Anything else raises
//! [`RadishError::MomentEncoding`].
//!
//! With `target` unset, every block in the call must already be
//! encoded at the requested output width; a mismatch raises rather
//! than passing values through on the wrong grid.
//!
//! Because gate padding goes through the same remap, a remapped array
//! is physically self-consistent end to end: pad gates and
//! below-threshold gates both land on the same physical floor, exactly
//! as they do on the source grid. Consumers that need to tell the two
//! apart should read `gate_count` from [`record_moment_encoding`] and
//! slice — the raw words alone cannot distinguish them, on either grid.
//!
//! # Public-type stability
//!
//! The structs ([`DemuxOptions`], [`TargetEncoding`], [`MomentEncoding`],
//! [`RecordInventory`]) are `#[non_exhaustive]` because their fields are
//! radish's editorial choice and may grow. The enums
//! ([`MomentSelector`], [`OutputWord`], [`RawMoment`]) are left
//! exhaustive on purpose: their variants enumerate closed domains fixed
//! by the wire format (the seven ICD moments, 8-/16-bit words), so a
//! caller matching all of them should get a compile error if the set
//! ever changes, not a silently-taken wildcard arm.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use crate::backends::common::par_map;
use crate::{RadishError, Result};

use super::decode::error::NexradDecodeError;
use super::decode::header::MessageType;
use super::decode::messages::msg31::moment::MomentDescriptor;
use super::decode::messages::msg31::Msg31;
use super::decode::messages::{decode_messages, Message, MessagePayload};
use super::decode::record::{decompress, is_raw_archive2, raw_archive2_body, split_ldm_records};

/// Wrap a decoder-layer error as a crate-level one. Used at every
/// boundary between `decode::` and this module; a named fn keeps the
/// seven call sites from drifting onto different variants.
fn decode_err(error: NexradDecodeError) -> RadishError {
    RadishError::Decode(error.to_string())
}

/// Which Message 31 moment to demultiplex.
///
/// Parses from the NEXRAD short names used on the wire (`REF`, `VEL`,
/// `SW`, `ZDR`, `PHI`, `RHO`, `CFP`) and, as a convenience, from the
/// ODIM names radish's volume readers emit (`DBZH`, `VRADH`, `WRADH`,
/// `ZDR`, `PHIDP`, `RHOHV`, `CCORH`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MomentSelector {
    /// Reflectivity — `DREF` on the wire, `DBZH` in ODIM.
    Ref,
    /// Radial velocity — `DVEL` / `VRADH`.
    Vel,
    /// Spectrum width — `DSW ` / `WRADH`.
    Sw,
    /// Differential reflectivity — `DZDR` / `ZDR`.
    Zdr,
    /// Differential phase — `DPHI` / `PHIDP`.
    Phi,
    /// Correlation coefficient — `DRHO` / `RHOHV`.
    Rho,
    /// Clutter filter power removed — `DCFP` / `CCORH`.
    Cfp,
}

impl MomentSelector {
    /// Every selector, in ICD Table XVII-A pointer order.
    pub const ALL: [MomentSelector; 7] = [
        Self::Ref,
        Self::Vel,
        Self::Sw,
        Self::Zdr,
        Self::Phi,
        Self::Rho,
        Self::Cfp,
    ];

    /// The NEXRAD short name (`"REF"`, `"VEL"`, …). Stable — it is the
    /// key used in [`RecordInventory::moments`] and in the Python
    /// bindings' dicts.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ref => "REF",
            Self::Vel => "VEL",
            Self::Sw => "SW",
            Self::Zdr => "ZDR",
            Self::Phi => "PHI",
            Self::Rho => "RHO",
            Self::Cfp => "CFP",
        }
    }
}

impl FromStr for MomentSelector {
    type Err = RadishError;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_uppercase().as_str() {
            "REF" | "DBZH" => Ok(Self::Ref),
            "VEL" | "VRADH" => Ok(Self::Vel),
            "SW" | "WRADH" => Ok(Self::Sw),
            "ZDR" => Ok(Self::Zdr),
            "PHI" | "PHIDP" => Ok(Self::Phi),
            "RHO" | "RHOHV" => Ok(Self::Rho),
            "CFP" | "CCORH" => Ok(Self::Cfp),
            other => Err(RadishError::Unsupported(format!(
                "unknown NEXRAD moment {other:?}; expected one of \
                 REF, VEL, SW, ZDR, PHI, RHO, CFP"
            ))),
        }
    }
}

/// Width of the raw words written to the output array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputWord {
    /// `uint8` output.
    U8,
    /// `uint16` output.
    U16,
}

impl OutputWord {
    /// Bit width — 8 or 16, comparable with
    /// `MomentDescriptor::data_word_size_bits`.
    pub fn bits(self) -> u8 {
        match self {
            Self::U8 => 8,
            Self::U16 => 16,
        }
    }

    /// Largest value representable in this width.
    fn max(self) -> u32 {
        match self {
            Self::U8 => u32::from(u8::MAX),
            Self::U16 => u32::from(u16::MAX),
        }
    }
}

/// The raw grid the caller wants values on, as declared by a NEXRAD
/// moment descriptor: `physical = (raw - offset) / scale`.
///
/// `#[non_exhaustive]`: build with [`TargetEncoding::new`] so a future
/// field is not a breaking change.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct TargetEncoding {
    /// Target `scale` (the descriptor field, not CF `scale_factor`).
    pub scale: f32,
    /// Target `offset` (the descriptor field, not CF `add_offset`).
    pub offset: f32,
}

impl TargetEncoding {
    /// A target raw grid where `physical = (raw - offset) / scale`.
    pub fn new(scale: f32, offset: f32) -> Self {
        Self { scale, offset }
    }
}

/// Everything a demux call needs beyond the bytes themselves.
///
/// `#[non_exhaustive]`: build with [`DemuxOptions::new`] and the
/// `with_*` setters rather than a struct literal, so adding a field
/// later is not a breaking change.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct DemuxOptions {
    /// Which moment to extract.
    pub moment: MomentSelector,
    /// Row count of the output array.
    pub rays: usize,
    /// Gate (column) count of the output array.
    pub gates: usize,
    /// Output word width.
    pub word: OutputWord,
    /// Value for rows past the radial count and rows where the moment
    /// is absent. Must fit `word`.
    pub fill_value: u16,
    /// Target raw grid. `None` requires every block to already match
    /// `word`; `Some` remaps exactly or errors.
    pub target: Option<TargetEncoding>,
}

impl DemuxOptions {
    /// Options for demultiplexing `moment` into an `out_shape` = `(rays,
    /// gates)` array of `word`-width raw words. `fill_value` defaults to
    /// `0` and there is no target grid; the fields are `pub`, so set them
    /// directly (e.g. `opts.fill_value = 255`).
    ///
    /// `out_shape` is a `(rays, gates)` pair rather than two `usize`
    /// arguments so the two dimensions can't be silently transposed at a
    /// call site — it matches the `(usize, usize)` shape the Python layer
    /// threads through end to end.
    pub fn new(moment: MomentSelector, out_shape: (usize, usize), word: OutputWord) -> Self {
        let (rays, gates) = out_shape;
        Self {
            moment,
            rays,
            gates,
            word,
            fill_value: 0,
            target: None,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.gates == 0 {
            return Err(RadishError::MomentEncoding(
                "out_shape gate dimension must be non-zero".to_string(),
            ));
        }
        if u32::from(self.fill_value) > self.word.max() {
            return Err(RadishError::MomentEncoding(format!(
                "fill_value {} does not fit uint{}",
                self.fill_value,
                self.word.bits()
            )));
        }
        // The ICD caps a moment block at 1840 gates and the on-wire
        // `gate_count` field is a `u16`, so a wider request cannot
        // describe real data.
        if self.gates > usize::from(u16::MAX) {
            return Err(RadishError::MomentEncoding(format!(
                "out_shape gate dimension {} exceeds the u16 the wire format uses for \
                 gate counts ({}) — NEXRAD moments top out at 1840 gates",
                self.gates,
                u16::MAX
            )));
        }
        // `rays * gates` is allocated up front. Reject anything that
        // overflows *or* that is merely implausible: `vec![_; n]` on an
        // absurd `n` calls `handle_alloc_error`, which aborts the
        // process — and an abort cannot be turned back into a Python
        // exception, so a caller typo would take the interpreter with
        // it. The widest real sweep is ~800 x 1840; the cap below
        // leaves three orders of magnitude of headroom.
        let elements = self.rays.checked_mul(self.gates).ok_or_else(|| {
            RadishError::MomentEncoding(format!(
                "out_shape ({}, {}) overflows usize",
                self.rays, self.gates
            ))
        })?;
        if elements > MAX_OUTPUT_ELEMENTS {
            return Err(RadishError::MomentEncoding(format!(
                "out_shape ({}, {}) requests {elements} words, above the {MAX_OUTPUT_ELEMENTS} \
                 limit — radish refuses rather than risking an allocation abort",
                self.rays, self.gates
            )));
        }
        Ok(())
    }

    /// Reject a row count that the requested `rays` cannot hold, before
    /// anything is allocated against it.
    fn check_rows_fit(&self, rows: usize) -> Result<()> {
        if rows > self.rays {
            return Err(RadishError::MomentEncoding(format!(
                "input holds {rows} MSG_31 radials but the requested out_shape only has {} rays \
                 — widen the ray dimension (radish never drops radials silently)",
                self.rays
            )));
        }
        Ok(())
    }
}

/// Ceiling on `rays * gates` for one demux call. Not a format limit —
/// a guard so an implausible `out_shape` returns an error instead of
/// aborting the process inside the allocator. 1 Gi words is ~700x the
/// widest real NEXRAD sweep.
const MAX_OUTPUT_ELEMENTS: usize = 1 << 30;

/// A demultiplexed moment: row-major `rays * gates` raw words at the
/// width the caller requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawMoment {
    /// `uint8` words.
    U8(Vec<u8>),
    /// `uint16` words.
    U16(Vec<u16>),
}

impl RawMoment {
    /// Total element count (`rays * gates`).
    pub fn len(&self) -> usize {
        match self {
            Self::U8(v) => v.len(),
            Self::U16(v) => v.len(),
        }
    }

    /// Whether the array holds no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Allocate `len` cells pre-filled with `fill`, **fallibly**.
    ///
    /// `vec![fill; len]` routes allocation failure to `handle_alloc_error`,
    /// which aborts the process — and an abort cannot be turned back into
    /// a Python exception. `DemuxOptions::validate` caps the element
    /// count, but a cap in elements is not a cap in bytes and says
    /// nothing about the memory a cgroup-limited worker actually has, so
    /// reserve fallibly and report instead.
    fn filled(word: OutputWord, len: usize, fill: u16) -> Result<Self> {
        fn try_filled<T: Copy>(len: usize, fill: T) -> Result<Vec<T>> {
            let mut v: Vec<T> = Vec::new();
            v.try_reserve_exact(len).map_err(|_| {
                RadishError::MomentEncoding(format!(
                    "could not allocate {len} x {} bytes for the output array",
                    std::mem::size_of::<T>()
                ))
            })?;
            v.resize(len, fill);
            Ok(v)
        }
        Ok(match word {
            OutputWord::U8 => Self::U8(try_filled(len, fill as u8)?),
            OutputWord::U16 => Self::U16(try_filled(len, fill)?),
        })
    }

    /// Copy `src` (a contiguous run of whole rows) into `self` starting
    /// at row `start_row`. Both sides use the same row stride.
    fn blit_rows(&mut self, src: &Self, start_row: usize, gates: usize) {
        let start = start_row * gates;
        match (self, src) {
            (Self::U8(dst), Self::U8(s)) => dst[start..start + s.len()].copy_from_slice(s),
            (Self::U16(dst), Self::U16(s)) => dst[start..start + s.len()].copy_from_slice(s),
            // Both sides are built from the same `OutputWord` in every
            // call path, so a width mismatch is unreachable.
            _ => unreachable!("blit_rows across mismatched output widths"),
        }
    }

    /// Reorder the first `order.len()` rows according to `order`,
    /// leaving any trailing fill rows untouched.
    fn permute_rows(&mut self, order: &[usize], gates: usize) {
        fn permute<T: Copy>(v: &mut [T], order: &[usize], gates: usize) {
            let mut sorted = Vec::with_capacity(order.len() * gates);
            for &row in order {
                sorted.extend_from_slice(&v[row * gates..(row + 1) * gates]);
            }
            v[..sorted.len()].copy_from_slice(&sorted);
        }
        match self {
            Self::U8(v) => permute(v, order, gates),
            Self::U16(v) => permute(v, order, gates),
        }
    }
}

/// Source encoding of one moment, as declared on the wire.
///
/// Everything a caller needs to size an array and build its CF
/// attributes before decoding: `scale_factor = 1 / scale`,
/// `add_offset = -offset / scale`.
///
/// `#[non_exhaustive]`: radish returns these; the marker reserves room
/// to report more per-moment facts without a breaking change.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct MomentEncoding {
    /// `8` or `16` — the on-wire word size of the first radial that
    /// carried this moment.
    pub word_size: u8,
    /// Descriptor `scale` from the first radial carrying this moment.
    pub scale: f32,
    /// Descriptor `offset` from the first radial carrying this moment.
    pub offset: f32,
    /// Gate count from the first radial carrying this moment.
    pub gate_count: u16,
    /// Largest gate count seen across all radials — the safe column
    /// count for an array that must hold every row.
    pub max_gate_count: u16,
    /// Range to the centre of the first gate, km.
    pub first_gate_km: f32,
    /// Gate spacing, km.
    pub gate_interval_km: f32,
    /// How many radials actually carried this moment.
    pub radials_present: usize,
    /// `false` if any radial disagreed with the first-seen
    /// `(word_size, scale, offset)` — the signal that a
    /// [`DemuxOptions::target`] is mandatory.
    pub uniform: bool,
}

impl MomentEncoding {
    fn from_descriptor(d: &MomentDescriptor) -> Self {
        Self {
            word_size: d.data_word_size_bits,
            scale: d.scale,
            offset: d.offset,
            gate_count: d.gate_count,
            max_gate_count: d.gate_count,
            first_gate_km: d.range_to_first_gate_km,
            gate_interval_km: d.gate_interval_km,
            radials_present: 1,
            uniform: true,
        }
    }

    /// Merge another inventory's view of the same moment into this one.
    fn merge(&mut self, other: &Self) {
        self.radials_present += other.radials_present;
        self.max_gate_count = self.max_gate_count.max(other.max_gate_count);
        if !other.uniform
            || other.word_size != self.word_size
            || other.scale != self.scale
            || other.offset != self.offset
        {
            self.uniform = false;
        }
    }
}

/// Per-radial headers plus per-moment encodings for a record or a
/// sweep-sized span. One pass; cheap enough to call before every
/// decode.
///
/// `#[non_exhaustive]`: radish returns these; the marker reserves room
/// to report more per-radial fields without a breaking change.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct RecordInventory {
    /// Number of Message 31 radials — the row count a demux of this
    /// input will produce.
    pub radial_count: usize,
    /// Per-radial azimuth angle, degrees, in record order.
    pub azimuth: Vec<f32>,
    /// Per-radial elevation angle, degrees, in record order.
    pub elevation: Vec<f32>,
    /// Per-radial azimuth number within the elevation scan.
    pub azimuth_number: Vec<u16>,
    /// Per-radial elevation number within the volume scan.
    pub elevation_number: Vec<u8>,
    /// Per-radial collection time, milliseconds past midnight GMT.
    pub collection_time_ms: Vec<u32>,
    /// Per-radial modified Julian date.
    pub modified_julian_date: Vec<u16>,
    /// Encodings for every moment present in the input.
    pub moments: BTreeMap<MomentSelector, MomentEncoding>,
}

impl RecordInventory {
    fn push_radial(&mut self, msg: &Msg31<'_>) {
        let h = &msg.header;
        self.radial_count += 1;
        self.azimuth.push(h.azimuth_angle_degrees);
        self.elevation.push(h.elevation_angle_degrees);
        self.azimuth_number.push(h.azimuth_number);
        self.elevation_number.push(h.elevation_number);
        self.collection_time_ms.push(h.collection_time_ms);
        self.modified_julian_date.push(h.modified_julian_date);
        for selector in MomentSelector::ALL {
            if let Some((descriptor, _)) = select_block(msg, selector) {
                // Same shape as `append` below — one definition of what
                // makes an encoding non-uniform, rather than a separate
                // `fold` that had to be kept in lockstep with `merge`.
                let seen = MomentEncoding::from_descriptor(descriptor);
                self.moments
                    .entry(selector)
                    .and_modify(|e| e.merge(&seen))
                    .or_insert(seen);
            }
        }
    }

    fn append(&mut self, mut other: Self) {
        self.radial_count += other.radial_count;
        self.azimuth.append(&mut other.azimuth);
        self.elevation.append(&mut other.elevation);
        self.azimuth_number.append(&mut other.azimuth_number);
        self.elevation_number.append(&mut other.elevation_number);
        self.collection_time_ms
            .append(&mut other.collection_time_ms);
        self.modified_julian_date
            .append(&mut other.modified_julian_date);
        for (selector, encoding) in &other.moments {
            self.moments
                .entry(*selector)
                .and_modify(|e| e.merge(encoding))
                .or_insert(*encoding);
        }
    }
}

/// Resolved per-block transform: mask the raw word, then
/// `raw * ratio + bias`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Remap {
    mask: u32,
    ratio: u32,
    bias: u32,
}

/// Identity of a block's on-wire encoding: `(word_size_bits, scale bits,
/// offset bits)`. Compared bitwise so `f32` equality quirks can't make
/// two different grids look the same.
type EncodingKey = (u8, u32, u32);

fn encoding_key(descriptor: &MomentDescriptor) -> EncodingKey {
    (
        descriptor.data_word_size_bits,
        descriptor.scale.to_bits(),
        descriptor.offset.to_bits(),
    )
}

/// Resolves each block's transform while enforcing the single most
/// important invariant of the whole module: **every value in one output
/// array must land on one physical grid.**
///
/// With a `target`, blocks may differ — each is remapped onto the common
/// grid, which is the entire point. Without one, the caller has been
/// handed a single `scale_factor`/`add_offset` pair by the inspector, so
/// two blocks with different `scale`/`offset` would silently decode half
/// the array onto the wrong grid. Checking only `word_size` (as an
/// earlier version did) is not enough: same width, different scale is
/// exactly the case that slips through.
///
/// Doubles as a memo — one record's radials essentially always share an
/// encoding, so `Remap::resolve`'s float work runs once per record
/// rather than once per radial.
#[derive(Debug, Default)]
struct RemapPin {
    pinned: Option<(EncodingKey, Remap)>,
}

impl RemapPin {
    fn resolve(
        &mut self,
        descriptor: &MomentDescriptor,
        selector: MomentSelector,
        opts: &DemuxOptions,
    ) -> Result<Remap> {
        let key = encoding_key(descriptor);
        match self.pinned {
            Some((pinned_key, remap)) if pinned_key == key => Ok(remap),
            Some((pinned_key, _)) if opts.target.is_none() => {
                Err(mixed_encoding_error(selector, pinned_key, key))
            }
            _ => {
                let remap = Remap::resolve(descriptor, selector, opts)?;
                self.pinned = Some((key, remap));
                Ok(remap)
            }
        }
    }

    /// The encoding this call settled on, for the cross-record check.
    fn key(&self) -> Option<EncodingKey> {
        self.pinned.map(|(key, _)| key)
    }
}

fn mixed_encoding_error(
    selector: MomentSelector,
    (first_bits, first_scale, first_offset): EncodingKey,
    (next_bits, next_scale, next_offset): EncodingKey,
) -> RadishError {
    RadishError::MomentEncoding(format!(
        "{} mixes on-wire encodings within one call — (word_size={first_bits}, \
         scale={}, offset={}) and (word_size={next_bits}, scale={}, offset={}). \
         A single output array carries one scale_factor/add_offset, so these cannot \
         share it. Pass scale=/offset= to remap both onto a common grid.",
        selector.name(),
        f32::from_bits(first_scale),
        f32::from_bits(first_offset),
        f32::from_bits(next_scale),
        f32::from_bits(next_offset),
    ))
}

impl Remap {
    /// Pure pass-through — mask only, no affine step. Gates the memcpy
    /// fast path in `write_row`; see the comment there for why it earns
    /// its keep.
    fn is_identity(&self) -> bool {
        self.ratio == 1 && self.bias == 0
    }

    #[inline]
    fn apply(&self, raw: u32) -> u32 {
        (raw & self.mask) * self.ratio + self.bias
    }

    fn resolve(
        descriptor: &MomentDescriptor,
        selector: MomentSelector,
        opts: &DemuxOptions,
    ) -> Result<Self> {
        let mask = mask_for(selector, descriptor.data_word_size_bits);
        let Some(target) = opts.target else {
            if descriptor.data_word_size_bits != opts.word.bits() {
                return Err(RadishError::MomentEncoding(format!(
                    "{} is encoded at word_size={} (scale={}, offset={}) but uint{} output was \
                     requested. Pass scale=/offset= to remap onto the target grid, or request \
                     uint{} to take the source encoding as-is.",
                    selector.name(),
                    descriptor.data_word_size_bits,
                    descriptor.scale,
                    descriptor.offset,
                    opts.word.bits(),
                    descriptor.data_word_size_bits,
                )));
            }
            return Ok(Self {
                mask,
                ratio: 1,
                bias: 0,
            });
        };

        let source_scale = f64::from(descriptor.scale);
        let source_offset = f64::from(descriptor.offset);
        let target_scale = f64::from(target.scale);
        let target_offset = f64::from(target.offset);
        if !source_scale.is_normal() || !target_scale.is_normal() {
            return Err(RadishError::MomentEncoding(format!(
                "{} scale must be finite and non-zero (source={}, target={})",
                selector.name(),
                descriptor.scale,
                target.scale,
            )));
        }

        let ratio = target_scale / source_scale;
        let bias = target_offset - source_offset * ratio;
        let describe = || {
            format!(
                "{} source (word_size={}, scale={}, offset={}) -> target (uint{}, scale={}, \
                 offset={}) needs raw_t = raw_s * {ratio} + {bias}",
                selector.name(),
                descriptor.data_word_size_bits,
                descriptor.scale,
                descriptor.offset,
                opts.word.bits(),
                target.scale,
                target.offset,
            )
        };
        if !exact_non_negative_integer(ratio) || ratio < 1.0 {
            return Err(RadishError::MomentEncoding(format!(
                "{}, which is not an exact integer >= 1 — the remap would be lossy",
                describe()
            )));
        }
        if !exact_non_negative_integer(bias) {
            return Err(RadishError::MomentEncoding(format!(
                "{}, which is not an exact non-negative integer — the remap would be lossy",
                describe()
            )));
        }
        let (ratio, bias) = (ratio as u32, bias as u32);
        // Bound against the *output moment's* significant-bit mask, not
        // just the output width. 16-bit ZDR carries 11 significant bits
        // (0x07FF) and PHI 10 (0x03FF), so a remap landing above that is
        // off the grid the caller asked for — and any consumer applying
        // the ICD mask (including this module) would truncate it.
        // Checked once per block against the widest masked source word
        // rather than per gate.
        let out_mask = mask_for(selector, opts.word.bits());
        let widest = u64::from(mask) * u64::from(ratio) + u64::from(bias);
        if widest > u64::from(out_mask) {
            return Err(RadishError::MomentEncoding(format!(
                "{}, whose widest value {widest} overflows the uint{} {} grid (max {out_mask})",
                describe(),
                opts.word.bits(),
                selector.name(),
            )));
        }
        Ok(Self { mask, ratio, bias })
    }
}

/// True for finite values that are whole numbers in `[0, u32::MAX]`.
fn exact_non_negative_integer(x: f64) -> bool {
    x.is_finite() && x >= 0.0 && x <= f64::from(u32::MAX) && x.fract() == 0.0
}

/// Significant-bit mask per ICD §3.2.4.17.6 Table XVII-I. See the
/// module docs for the deviation from xradar.
fn mask_for(selector: MomentSelector, word_size_bits: u8) -> u32 {
    match (selector, word_size_bits) {
        (MomentSelector::Phi, 16) => 0x03FF,
        (MomentSelector::Zdr, 16) => 0x07FF,
        (_, 16) => 0xFFFF,
        _ => 0x00FF,
    }
}

/// Borrow the requested moment's descriptor + gate bytes from a
/// parsed radial. `None` when that radial didn't carry the moment.
fn select_block<'a>(
    msg: &'a Msg31<'a>,
    selector: MomentSelector,
) -> Option<(&'a MomentDescriptor, &'a [u8])> {
    match selector {
        MomentSelector::Ref => msg
            .reflectivity
            .as_ref()
            .map(|b| (&b.descriptor, b.gate_bytes)),
        MomentSelector::Vel => msg.velocity.as_ref().map(|b| (&b.descriptor, b.gate_bytes)),
        MomentSelector::Sw => msg
            .spectrum_width
            .as_ref()
            .map(|b| (&b.descriptor, b.gate_bytes)),
        MomentSelector::Zdr => msg.zdr.as_ref().map(|b| (&b.descriptor, b.gate_bytes)),
        MomentSelector::Phi => msg.phi.as_ref().map(|b| (&b.descriptor, b.gate_bytes)),
        MomentSelector::Rho => msg.rho.as_ref().map(|b| (&b.descriptor, b.gate_bytes)),
        MomentSelector::Cfp => msg.cfp.as_ref().map(|b| (&b.descriptor, b.gate_bytes)),
    }
}

/// One record's worth of demultiplexed rows plus the azimuths that go
/// with them, both in record order.
struct RecordRows {
    data: RawMoment,
    /// Per-radial azimuths, or empty when the caller isn't sorting —
    /// `rows` is the authority on the row count either way.
    azimuth: Vec<f32>,
    rows: usize,
    /// The on-wire encoding this record's blocks used, if it carried the
    /// moment at all. `stitch` compares these across records so a
    /// per-record check can't miss a disagreement *between* records.
    encoding: Option<EncodingKey>,
}

impl RecordRows {
    fn rows(&self) -> usize {
        self.rows
    }
}

/// Decode `gate_bytes` into row `row` of `dst`: mask, remap, then pad
/// the rest of the row with raw `0`.
fn write_row(
    dst: &mut RawMoment,
    row: usize,
    gates: usize,
    gate_bytes: &[u8],
    word_bytes: usize,
    remap: Remap,
) -> Result<()> {
    let present = gate_bytes.len() / word_bytes;
    if present > gates {
        return Err(RadishError::MomentEncoding(format!(
            "moment has {present} gates but the requested out_shape only has {gates} \
             — widen the gate dimension (radish never truncates gates silently)"
        )));
    }
    let base = row * gates;

    // Two source widths x two output widths.
    //
    // The 8-bit identity branch is load-bearing, not redundant: with no
    // remap the mask is `0xFF` (a no-op on a widened `u8`) and the
    // multiply-add is 1/0, so the loop reduces to a widening copy that
    // LLVM lowers to a memcpy for `u8` output. Routing it through
    // `remap.apply` instead blocks that and measured a **2.1x
    // regression on the whole call** for REF (0.071 -> 0.150 ms), which
    // is why it survives looking like dead code.
    macro_rules! fill {
        ($out:expr, $ty:ty) => {{
            let dst_row = &mut $out[base..base + gates];
            if word_bytes == 1 {
                if remap.is_identity() {
                    for (slot, &b) in dst_row.iter_mut().zip(gate_bytes) {
                        *slot = b as $ty;
                    }
                } else {
                    for (slot, &b) in dst_row.iter_mut().zip(gate_bytes) {
                        *slot = remap.apply(u32::from(b)) as $ty;
                    }
                }
            } else {
                // Load with `u16::from_be_bytes` and stay in u16 lanes.
                // Building the word as `(u32::from(hi) << 8) | u32::from(lo)`
                // blocks LLVM's vectorised byteswap and measured ~4x
                // slower on this loop — a 2x regression on the whole
                // call for the 16-bit moments (ZDR/PHI). Safe because
                // `Remap::resolve` proves `mask * ratio + bias` fits the
                // output mask, hence u16, before we ever get here.
                let (mask, ratio, bias) =
                    (remap.mask as u16, remap.ratio as u16, remap.bias as u16);
                for (slot, pair) in dst_row.iter_mut().zip(gate_bytes.chunks_exact(2)) {
                    let raw = u16::from_be_bytes([pair[0], pair[1]]);
                    *slot = ((raw & mask).wrapping_mul(ratio).wrapping_add(bias)) as $ty;
                }
            }
            // Right-pad with the remap of raw 0, not `fill_value`.
            //
            // With no remap that is literally raw 0 — byte-for-byte
            // xradar parity (`np.pad(..., constant_values=0)`). With a
            // remap active it is `bias`, which decodes to the *same
            // physical value* as a source raw 0 would have. Padding with
            // a literal 0 there would silently put the pad region on a
            // different physical floor than the below-threshold gates
            // it sits next to (on the 2020-06-02 KVNX ZDR remap:
            // -13.06 dB of pad against a -8.0 dB detection floor).
            dst_row[present..].fill(remap.apply(0) as $ty);
        }};
    }
    match dst {
        RawMoment::U8(v) => fill!(v, u8),
        RawMoment::U16(v) => fill!(v, u16),
    }
    Ok(())
}

/// Parse one already-decompressed message stream and return just its
/// Message 31 radials, in record order.
///
/// Every entry point funnels through here so the legacy-MSG_1 refusal is
/// stated once — in particular the inspectors need it too, since the
/// documented workflow calls them *first* and an empty inventory would
/// be a confusing way to learn the file is pre-Build-12.
fn parse_radials<'a>(messages: &'a [Message<'a>]) -> Result<Vec<&'a Msg31<'a>>> {
    let radials: Vec<&Msg31<'_>> = messages
        .iter()
        .filter_map(|m| match &m.payload {
            MessagePayload::Msg31(boxed) => Some(&**boxed),
            _ => None,
        })
        .collect();

    if radials.is_empty()
        && messages
            .iter()
            .any(|m| m.header.message_type == MessageType::DigitalRadarDataLegacy)
    {
        return Err(RadishError::Unsupported(
            "this input carries legacy MSG_1 radials (pre-Build-12, before March 2012); the \
             per-moment demultiplexer only supports MSG_31. Use radish.open_datatree() for \
             these files."
                .to_string(),
        ));
    }
    Ok(radials)
}

/// Demultiplex `radials` into rows `start_row..start_row + radials.len()`
/// of `dst`. `dst` must already be sized and pre-filled with
/// `fill_value` — rows whose radial lacks the moment are simply left
/// alone.
/// Returns the encoding these radials settled on, so the sweep path can
/// check that separate records agree with each other too.
fn write_radials(
    dst: &mut RawMoment,
    start_row: usize,
    radials: &[&Msg31<'_>],
    opts: &DemuxOptions,
) -> Result<Option<EncodingKey>> {
    let mut pin = RemapPin::default();
    for (offset, msg) in radials.iter().enumerate() {
        if let Some((descriptor, gate_bytes)) = select_block(msg, opts.moment) {
            let remap = pin.resolve(descriptor, opts.moment, opts)?;
            write_row(
                dst,
                start_row + offset,
                opts.gates,
                gate_bytes,
                descriptor.word_size_bytes(),
                remap,
            )?;
        }
    }
    Ok(pin.key())
}

/// Walk one already-decompressed message stream and demultiplex the
/// requested moment out of every Message 31 radial it contains.
///
/// Used by the sweep path, where each rayon worker produces its own
/// row block because it cannot know its start row until every record
/// has been counted. The record path writes straight into the output
/// instead — see [`decode_record_moment`].
fn demux_message_stream(
    stream: &[u8],
    opts: &DemuxOptions,
    want_azimuth: bool,
    running_rows: &AtomicUsize,
) -> Result<RecordRows> {
    let messages = decode_messages(stream).map_err(decode_err)?;
    let radials = parse_radials(&messages)?;

    // Guard the allocation on the *radial* count, which is independent
    // of `opts.rays` — `validate()` only bounded `rays * gates`, so a
    // record with more radials than `rays` could otherwise wrap here and
    // index out of bounds further down.
    opts.check_rows_fit(radials.len())?;
    // Then guard the *running* total across every record decoded so far.
    // Each worker holds its own buffer until `stitch` runs, so without
    // this the peak is `records x rays x gates` rather than the
    // `rays x gates` the caller declared — enough to abort the process
    // from a couple of megabytes of input. Checking here means we bail
    // before this worker allocates, so the sum of live buffers can never
    // exceed the declared output size.
    let prior = running_rows.fetch_add(radials.len(), AtomicOrdering::Relaxed);
    opts.check_rows_fit(prior + radials.len())?;
    let len = radials.len() * opts.gates; // bounded by check_rows_fit + validate

    let mut data = RawMoment::filled(opts.word, len, opts.fill_value)?;
    let encoding = write_radials(&mut data, 0, &radials, opts)?;
    // Only materialise azimuths when a sort will actually consume them.
    let azimuth = if want_azimuth {
        radials
            .iter()
            .map(|m| m.header.azimuth_angle_degrees)
            .collect()
    } else {
        Vec::new()
    };
    Ok(RecordRows {
        data,
        azimuth,
        rows: radials.len(),
        encoding,
    })
}

/// Collect the per-radial headers and per-moment encodings of one
/// already-decompressed message stream.
fn inventory_message_stream(stream: &[u8]) -> Result<RecordInventory> {
    let messages = decode_messages(stream).map_err(decode_err)?;
    let mut inventory = RecordInventory::default();
    for msg in parse_radials(&messages)? {
        inventory.push_radial(msg);
    }
    Ok(inventory)
}

/// Assemble per-record row blocks into one `(rays, gates)` array,
/// preserving record order, optionally sorted by azimuth.
fn stitch(
    per_record: Vec<RecordRows>,
    opts: &DemuxOptions,
    sort_by_azimuth: bool,
) -> Result<RawMoment> {
    let total: usize = per_record.iter().map(RecordRows::rows).sum();
    opts.check_rows_fit(total)?;

    // Each record vetted its own blocks; this catches records that are
    // each internally consistent but disagree with one another. Only
    // matters with no target grid — with one, differing encodings are
    // the case the remap exists to handle.
    if opts.target.is_none() {
        let mut seen: Option<EncodingKey> = None;
        for record in &per_record {
            match (seen, record.encoding) {
                (Some(first), Some(next)) if first != next => {
                    return Err(mixed_encoding_error(opts.moment, first, next));
                }
                (None, Some(next)) => seen = Some(next),
                _ => {}
            }
        }
    }

    let mut out = RawMoment::filled(opts.word, opts.rays * opts.gates, opts.fill_value)?;
    let mut azimuth = Vec::with_capacity(if sort_by_azimuth { total } else { 0 });
    let mut row = 0usize;
    for record in &per_record {
        out.blit_rows(&record.data, row, opts.gates);
        azimuth.extend_from_slice(&record.azimuth);
        row += record.rows();
    }

    if sort_by_azimuth && total > 1 {
        // Stable sort matching `np.argsort(azimuth, kind="stable")`
        // exactly, which callers are told to use to reorder their
        // coordinate arrays. `f32::total_cmp` would *not* match: it
        // orders -0.0 before +0.0 and puts negative NaN first, where
        // numpy treats +/-0.0 as equal (so stability keeps record
        // order) and sorts every NaN last.
        let mut order: Vec<usize> = (0..total).collect();
        order.sort_by(|a, b| {
            let (x, y) = (azimuth[*a], azimuth[*b]);
            match (x.is_nan(), y.is_nan()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            }
        });
        out.permute_rows(&order, opts.gates);
    }
    Ok(out)
}

/// Demultiplex one moment out of a single **decompressed** LDM record.
///
/// `record` is one record's message stream — i.e. the bytes past the
/// 4-byte LDM control word, already bzip2-decompressed. A record that
/// holds no Message 31 radials at all (the `S` chunk of a chunked
/// volume, which carries only MSG_2 / MSG_5) yields an all-`fill_value`
/// array rather than an error.
///
/// See the module docs for the full output contract.
pub fn decode_record_moment(record: &[u8], opts: &DemuxOptions) -> Result<RawMoment> {
    opts.validate()?;
    let messages = decode_messages(record).map_err(decode_err)?;
    let radials = parse_radials(&messages)?;
    opts.check_rows_fit(radials.len())?;

    // Single pass: allocate the output once and write rows straight
    // into it. Going through `stitch` would allocate and memset a
    // second buffer and then memcpy — ~3x the memory traffic on the
    // path the module docs advertise at ~0.06 ms per record.
    let mut out = RawMoment::filled(opts.word, opts.rays * opts.gates, opts.fill_value)?;
    write_radials(&mut out, 0, &radials, opts)?;
    Ok(out)
}

/// Walk a compressed span's records in parallel, applying `f` to each
/// decompressed message stream and preserving record order.
///
/// The single home for the rayon record pipeline that `CLAUDE.md` names
/// as a regression gate — previously spelled out once per sweep entry
/// point, so a change could silently de-parallelise one of them. Also
/// routes pre-Build-12 raw Archive II files (no LDM wrapper) the way
/// `decode_volume` does, which the sweep entry points used to skip:
/// without it a legacy file died in `split_ldm_records` with a generic
/// decode error instead of reaching the MSG_1 refusal.
fn par_map_streams<T, F>(span: &[u8], f: F) -> Result<Vec<T>>
where
    T: Send,
    F: Fn(&[u8]) -> Result<T> + Sync + Send,
{
    if is_raw_archive2(span) {
        let body = raw_archive2_body(span).map_err(decode_err)?;
        return Ok(vec![f(body)?]);
    }
    let records = split_ldm_records(span).map_err(decode_err)?;
    par_map(&records, |record| {
        f(&decompress(record).map_err(decode_err)?)
    })
}

/// Demultiplex one moment out of a **compressed** sweep-sized byte
/// span: `[i32 control word][bzip2 payload]` repeated, exactly as it
/// appears in an Archive II file. A leading 24-byte `AR2V` volume
/// header is skipped if present, so a whole-volume buffer works too.
///
/// Records are decompressed and demultiplexed in parallel via rayon,
/// then stitched back in record order.
///
/// With `sort_by_azimuth`, rows are stably sorted by the radial's
/// azimuth angle before being returned; trailing `fill_value` rows stay
/// at the end. Pair it with `np.argsort(azimuth, kind="stable")` over
/// [`sweep_moment_encoding`]'s `azimuth` to reorder coordinates the
/// same way.
pub fn decode_sweep_moment(
    span: &[u8],
    opts: &DemuxOptions,
    sort_by_azimuth: bool,
) -> Result<RawMoment> {
    opts.validate()?;
    // Shared across workers so a span whose records sum to more rows
    // than `rays` errors *before* N buffers exist. See
    // `demux_message_stream`.
    let running_rows = AtomicUsize::new(0);
    let per_record = par_map_streams(span, |stream| {
        demux_message_stream(stream, opts, sort_by_azimuth, &running_rows)
    })?;
    stitch(per_record, opts, sort_by_azimuth)
}

/// Inspect a single **decompressed** LDM record: per-radial headers and
/// the source encoding of every moment it carries.
///
/// Call this before [`decode_record_moment`] to size the output array
/// and to learn whether a `target` remap is required (`uniform ==
/// false` means the record mixes encodings).
pub fn record_moment_encoding(record: &[u8]) -> Result<RecordInventory> {
    inventory_message_stream(record)
}

/// Inspect a **compressed** sweep-sized byte span. The
/// [`decode_sweep_moment`] counterpart of [`record_moment_encoding`];
/// same input framing, same rayon-parallel record walk.
pub fn sweep_moment_encoding(span: &[u8]) -> Result<RecordInventory> {
    let per_record = par_map_streams(span, inventory_message_stream)?;
    let mut merged = RecordInventory::default();
    for inventory in per_record {
        merged.append(inventory);
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrap `body` (a MSG_31 wire body) in the 28-byte
    /// TCM + Table II message header the stream walker expects.
    fn frame_msg31(body: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; 12]; // TCM prefix — zero-filled in Archive II
        let logical = 16 + body.len();
        assert_eq!(logical % 2, 0, "message size is counted in halfwords");
        out.extend_from_slice(&((logical / 2) as u16).to_be_bytes());
        out.push(0); // channel
        out.push(31); // message type
        out.extend_from_slice(&0u16.to_be_bytes()); // sequence number
        out.extend_from_slice(&0u16.to_be_bytes()); // julian date
        out.extend_from_slice(&0u32.to_be_bytes()); // milliseconds
        out.extend_from_slice(&1u16.to_be_bytes()); // segment count
        out.extend_from_slice(&1u16.to_be_bytes()); // segment number
        out.extend_from_slice(body);
        out
    }

    /// One moment block: 4-byte `DataBlockId` + 24-byte descriptor +
    /// gate words.
    struct Block {
        name: &'static [u8; 4],
        word_size: u8,
        scale: f32,
        offset: f32,
        gates: Vec<u32>,
    }

    impl Block {
        fn encode(&self) -> Vec<u8> {
            let mut buf = Vec::new();
            buf.extend_from_slice(self.name);
            buf.extend_from_slice(&0u32.to_be_bytes()); // reserved
            buf.extend_from_slice(&(self.gates.len() as u16).to_be_bytes());
            buf.extend_from_slice(&2_125u16.to_be_bytes()); // first gate 2.125 km
            buf.extend_from_slice(&250u16.to_be_bytes()); // 0.250 km spacing
            buf.extend_from_slice(&0u16.to_be_bytes()); // tover
            buf.extend_from_slice(&0i16.to_be_bytes()); // snr threshold
            buf.push(0); // control flags
            buf.push(self.word_size);
            buf.extend_from_slice(&self.scale.to_be_bytes());
            buf.extend_from_slice(&self.offset.to_be_bytes());
            for &g in &self.gates {
                if self.word_size == 8 {
                    buf.push(g as u8);
                } else {
                    buf.extend_from_slice(&(g as u16).to_be_bytes());
                }
            }
            buf
        }
    }

    /// Build one framed MSG_31 radial carrying `blocks`.
    fn radial(azimuth: f32, blocks: &[Block]) -> Vec<u8> {
        const HEADER: usize = 72; // Build-12+ data header (32 fixed + 10 pointers)
        let mut header = Vec::new();
        header.extend_from_slice(b"KVNX");
        header.extend_from_slice(&0u32.to_be_bytes()); // collection time ms
        header.extend_from_slice(&20_405u16.to_be_bytes()); // modified julian date
        header.extend_from_slice(&1u16.to_be_bytes()); // azimuth number
        header.extend_from_slice(&azimuth.to_be_bytes());
        header.push(0); // compression
        header.push(0); // spare
        header.extend_from_slice(&0u16.to_be_bytes()); // radial length (unused here)
        header.push(2); // azimuth resolution
        header.push(1); // radial status
        header.push(1); // elevation number
        header.push(0); // cut sector
        header.extend_from_slice(&0.5f32.to_be_bytes()); // elevation angle
        header.push(0); // spot blanking
        header.push(0); // azimuth indexing
        header.extend_from_slice(&(blocks.len() as u16).to_be_bytes()); // data block count

        // Pointers are byte offsets from the start of the wire body.
        // Blocks are packed contiguously into the leading slots, which
        // is what real files do (the parser routes by name, not slot).
        let mut encoded = Vec::new();
        let mut pointers = [0u32; 10];
        let mut cursor = HEADER;
        for (i, block) in blocks.iter().enumerate() {
            pointers[i] = cursor as u32;
            let bytes = block.encode();
            cursor += bytes.len();
            encoded.extend_from_slice(&bytes);
        }
        for pointer in pointers {
            header.extend_from_slice(&pointer.to_be_bytes());
        }
        assert_eq!(header.len(), HEADER);
        header.extend_from_slice(&encoded);
        // Message size is counted in halfwords, so pad odd bodies.
        if header.len() % 2 == 1 {
            header.push(0);
        }
        frame_msg31(&header)
    }

    /// Frame a non-MSG_31 message as a fixed 2432-byte segment frame —
    /// what MSG_2 / MSG_5 actually look like between radials.
    fn frame_other(message_type: u8) -> Vec<u8> {
        const SEGMENT_FRAME_SIZE: usize = 2432;
        let mut out = vec![0u8; 12]; // TCM prefix
        out.extend_from_slice(&8u16.to_be_bytes()); // size: 8 halfwords == the 16-byte header
        out.push(0); // channel
        out.push(message_type);
        out.extend_from_slice(&0u16.to_be_bytes()); // sequence number
        out.extend_from_slice(&0u16.to_be_bytes()); // julian date
        out.extend_from_slice(&0u32.to_be_bytes()); // milliseconds
        out.extend_from_slice(&1u16.to_be_bytes()); // segment count
        out.extend_from_slice(&1u16.to_be_bytes()); // segment number
        out.resize(SEGMENT_FRAME_SIZE, 0); // pad out the frame
        out
    }

    fn ref_block(gates: Vec<u32>) -> Block {
        named_block(b"DREF", gates)
    }

    fn named_block(name: &'static [u8; 4], gates: Vec<u32>) -> Block {
        Block {
            name,
            word_size: 8,
            scale: 2.0,
            offset: 66.0,
            gates,
        }
    }

    fn zdr8(gates: Vec<u32>) -> Block {
        Block {
            name: b"DZDR",
            word_size: 8,
            scale: 16.0,
            offset: 128.0,
            gates,
        }
    }

    fn zdr16(gates: Vec<u32>) -> Block {
        Block {
            name: b"DZDR",
            word_size: 16,
            scale: 32.0,
            offset: 418.0,
            gates,
        }
    }

    fn opts(moment: MomentSelector, rays: usize, gates: usize, word: OutputWord) -> DemuxOptions {
        DemuxOptions::new(moment, (rays, gates), word)
    }

    fn u8s(m: &RawMoment) -> &[u8] {
        match m {
            RawMoment::U8(v) => v,
            RawMoment::U16(_) => panic!("expected uint8 output"),
        }
    }

    fn u16s(m: &RawMoment) -> &[u16] {
        match m {
            RawMoment::U16(v) => v,
            RawMoment::U8(_) => panic!("expected uint16 output"),
        }
    }

    #[test]
    fn selector_parses_nexrad_and_odim_names() {
        assert_eq!(
            "REF".parse::<MomentSelector>().unwrap(),
            MomentSelector::Ref
        );
        assert_eq!(
            "dbzh".parse::<MomentSelector>().unwrap(),
            MomentSelector::Ref
        );
        assert_eq!("SW ".parse::<MomentSelector>().unwrap(), MomentSelector::Sw);
        assert_eq!(
            "PHIDP".parse::<MomentSelector>().unwrap(),
            MomentSelector::Phi
        );
        assert!("DBZ".parse::<MomentSelector>().is_err());
    }

    #[test]
    fn decodes_one_radial_into_row_zero() {
        let record = radial(30.0, &[ref_block(vec![0, 1, 130, 2])]);
        let out = decode_record_moment(&record, &opts(MomentSelector::Ref, 1, 4, OutputWord::U8))
            .unwrap();
        assert_eq!(u8s(&out), &[0, 1, 130, 2]);
    }

    #[test]
    fn rows_are_in_record_order() {
        let mut record = radial(90.0, &[ref_block(vec![10, 11])]);
        record.extend_from_slice(&radial(10.0, &[ref_block(vec![20, 21])]));
        let out = decode_record_moment(&record, &opts(MomentSelector::Ref, 2, 2, OutputWord::U8))
            .unwrap();
        assert_eq!(u8s(&out), &[10, 11, 20, 21]);
    }

    #[test]
    fn short_moments_pad_with_raw_zero_not_fill_value() {
        // fill_value = 255 so pad (0) and fill (255) are distinguishable.
        let record = radial(30.0, &[ref_block(vec![7, 8])]);
        let mut o = opts(MomentSelector::Ref, 2, 4, OutputWord::U8);
        o.fill_value = 255;
        let out = decode_record_moment(&record, &o).unwrap();
        // Row 0: two decoded gates then raw-0 padding.
        // Row 1: no radial, so the whole row is fill_value.
        assert_eq!(u8s(&out), &[7, 8, 0, 0, 255, 255, 255, 255]);
    }

    #[test]
    fn absent_moment_leaves_the_row_at_fill_value() {
        // Radial 0 has REF+ZDR, radial 1 only REF.
        let mut record = radial(1.0, &[ref_block(vec![1, 2]), zdr8(vec![3, 4])]);
        record.extend_from_slice(&radial(2.0, &[ref_block(vec![5, 6])]));
        let mut o = opts(MomentSelector::Zdr, 2, 2, OutputWord::U8);
        o.fill_value = 99;
        let out = decode_record_moment(&record, &o).unwrap();
        assert_eq!(u8s(&out), &[3, 4, 99, 99]);
    }

    #[test]
    fn record_with_no_radials_is_all_fill_not_an_error() {
        let mut o = opts(MomentSelector::Ref, 2, 3, OutputWord::U8);
        o.fill_value = 7;
        let out = decode_record_moment(&[], &o).unwrap();
        assert_eq!(u8s(&out), &[7; 6]);
    }

    #[test]
    fn too_many_gates_is_an_error_never_a_truncation() {
        let record = radial(30.0, &[ref_block(vec![1, 2, 3, 4])]);
        let err = decode_record_moment(&record, &opts(MomentSelector::Ref, 1, 2, OutputWord::U8))
            .unwrap_err();
        assert!(
            matches!(err, RadishError::MomentEncoding(ref m) if m.contains("4 gates")),
            "got {err}"
        );
    }

    #[test]
    fn too_many_radials_is_an_error_never_a_drop() {
        let mut record = radial(1.0, &[ref_block(vec![1])]);
        record.extend_from_slice(&radial(2.0, &[ref_block(vec![2])]));
        let err = decode_record_moment(&record, &opts(MomentSelector::Ref, 1, 1, OutputWord::U8))
            .unwrap_err();
        assert!(
            matches!(err, RadishError::MomentEncoding(ref m) if m.contains("2 MSG_31 radials")),
            "got {err}"
        );
    }

    #[test]
    fn word_size_mismatch_without_a_target_is_rejected() {
        let record = radial(30.0, &[zdr8(vec![10, 20])]);
        let err = decode_record_moment(&record, &opts(MomentSelector::Zdr, 1, 2, OutputWord::U16))
            .unwrap_err();
        assert!(
            matches!(err, RadishError::MomentEncoding(ref m) if m.contains("scale=/offset=")),
            "got {err}"
        );
    }

    #[test]
    fn kvnx_zdr_8bit_remaps_exactly_onto_the_16bit_grid() {
        // The issue's case: raw16 = 2 * raw8 + 162.
        let record = radial(30.0, &[zdr8(vec![0, 1, 128, 255])]);
        let mut o = opts(MomentSelector::Zdr, 1, 4, OutputWord::U16);
        o.target = Some(TargetEncoding::new(32.0, 418.0));
        let out = decode_record_moment(&record, &o).unwrap();
        assert_eq!(u16s(&out), &[162, 164, 418, 672]);

        // Physical values must be identical on both grids.
        for (raw8, raw16) in [0u32, 1, 128, 255].into_iter().zip(u16s(&out)) {
            let source = (raw8 as f64 - 128.0) / 16.0;
            let target = (f64::from(*raw16) - 418.0) / 32.0;
            assert!((source - target).abs() < 1e-12, "{source} vs {target}");
        }
    }

    /// Every selector must read *its own* block. Without this, swapping
    /// two arms of `select_block` (e.g. `Sw` returning `msg.velocity`)
    /// is invisible: the fixture-free tests only ever built REF/ZDR/PHI,
    /// and the fixture test asserts array *sizes*, not values.
    #[test]
    fn each_selector_reads_its_own_block() {
        // One radial carrying all seven blocks, each with a distinct
        // gate value so a mix-up cannot go unnoticed. CFP is included
        // because it parses through `CfpBlock`, a separate type.
        let record = radial(
            30.0,
            &[
                named_block(b"DREF", vec![11]),
                named_block(b"DVEL", vec![22]),
                named_block(b"DSW ", vec![33]),
                named_block(b"DZDR", vec![44]),
                named_block(b"DPHI", vec![55]),
                named_block(b"DRHO", vec![66]),
                named_block(b"DCFP", vec![77]),
            ],
        );
        for (selector, expected) in [
            (MomentSelector::Ref, 11),
            (MomentSelector::Vel, 22),
            (MomentSelector::Sw, 33),
            (MomentSelector::Zdr, 44),
            (MomentSelector::Phi, 55),
            (MomentSelector::Rho, 66),
            (MomentSelector::Cfp, 77),
        ] {
            let out = decode_record_moment(&record, &opts(selector, 1, 1, OutputWord::U8))
                .unwrap_or_else(|e| panic!("{} failed: {e}", selector.name()));
            assert_eq!(
                u8s(&out),
                &[expected],
                "{} read the wrong block",
                selector.name()
            );
        }

        // The inventory must see all seven too.
        let inventory = record_moment_encoding(&record).unwrap();
        assert_eq!(inventory.moments.len(), 7);
        for selector in MomentSelector::ALL {
            assert!(
                inventory.moments.contains_key(&selector),
                "{} missing from the inventory",
                selector.name()
            );
        }
    }

    /// MSG_2 / MSG_5 sit between radials in a real record. They must
    /// consume no row, or every downstream coordinate is off by one.
    #[test]
    fn non_radial_messages_consume_no_row() {
        let mut record = frame_other(2); // MSG_2, RDA status
        record.extend_from_slice(&radial(10.0, &[ref_block(vec![1, 2])]));
        record.extend_from_slice(&frame_other(5)); // MSG_5, VCP
        record.extend_from_slice(&radial(20.0, &[ref_block(vec![3, 4])]));

        let out = decode_record_moment(&record, &opts(MomentSelector::Ref, 2, 2, OutputWord::U8))
            .unwrap();
        assert_eq!(u8s(&out), &[1, 2, 3, 4]);

        let inventory = record_moment_encoding(&record).unwrap();
        assert_eq!(inventory.radial_count, 2);
        assert_eq!(inventory.azimuth, vec![10.0, 20.0]);
    }

    /// A metadata-only record — the `S` chunk of a chunked volume —
    /// yields an all-fill array, not an error.
    #[test]
    fn metadata_only_record_is_all_fill() {
        let mut record = frame_other(2);
        record.extend_from_slice(&frame_other(5));
        let mut o = opts(MomentSelector::Ref, 2, 3, OutputWord::U8);
        o.fill_value = 9;
        let out = decode_record_moment(&record, &o).unwrap();
        assert_eq!(u8s(&out), &[9; 6]);
        assert_eq!(record_moment_encoding(&record).unwrap().radial_count, 0);
    }

    /// Legacy MSG_1 files are out of scope; both the decoder and the
    /// inspector must say so rather than returning an empty result. The
    /// inspector matters most — the documented workflow calls it first.
    #[test]
    fn legacy_msg1_input_is_refused_by_decoder_and_inspector() {
        let record = frame_other(1); // MSG_1, legacy digital radar data
        for error in [
            decode_record_moment(&record, &opts(MomentSelector::Ref, 1, 1, OutputWord::U8))
                .unwrap_err(),
            record_moment_encoding(&record).unwrap_err(),
        ] {
            assert!(
                matches!(error, RadishError::Unsupported(ref m) if m.contains("open_datatree")),
                "expected a legacy-MSG_1 refusal, got {error}"
            );
        }
    }

    /// The `0xFFFF` arm of `mask_for` — the deliberate deviation from
    /// xradar's `np.uint8(0xFF)` fallback, which would truncate a
    /// 16-bit non-PHI/ZDR moment to its low 8 bits.
    #[test]
    fn sixteen_bit_reflectivity_is_not_truncated_to_eight_bits() {
        let block = Block {
            name: b"DREF",
            word_size: 16,
            scale: 2.0,
            offset: 66.0,
            gates: vec![0xF7FF],
        };
        let record = radial(30.0, &[block]);
        let out = decode_record_moment(&record, &opts(MomentSelector::Ref, 1, 1, OutputWord::U16))
            .unwrap();
        assert_eq!(
            u16s(&out),
            &[0xF7FF],
            "16-bit REF keeps all 16 bits; xradar would return 0xFF"
        );
    }

    /// `max_gate_count` is what callers size their arrays with. Put the
    /// *shortest* radial first so an implementation that just mirrored
    /// first-seen would fail.
    #[test]
    fn max_gate_count_tracks_the_longest_radial_not_the_first() {
        let mut record = radial(1.0, &[ref_block(vec![7])]);
        record.extend_from_slice(&radial(2.0, &[ref_block(vec![1, 2, 3])]));

        let encoding = record_moment_encoding(&record).unwrap().moments[&MomentSelector::Ref];
        assert_eq!(encoding.gate_count, 1, "first-seen");
        assert_eq!(encoding.max_gate_count, 3, "widest across radials");

        // Sizing by max_gate_count must succeed and pad the short row.
        let out = decode_record_moment(
            &record,
            &opts(
                MomentSelector::Ref,
                2,
                usize::from(encoding.max_gate_count),
                OutputWord::U8,
            ),
        )
        .unwrap();
        assert_eq!(u8s(&out), &[7, 0, 0, 1, 2, 3]);
    }

    /// Unit conversions on the geometry fields (0.001 km -> km) are
    /// otherwise unasserted.
    #[test]
    fn inventory_reports_gate_geometry_in_kilometres() {
        let record = radial(1.0, &[ref_block(vec![1])]);
        let inventory = record_moment_encoding(&record).unwrap();
        let encoding = inventory.moments[&MomentSelector::Ref];
        assert!((encoding.first_gate_km - 2.125).abs() < 1e-6);
        assert!((encoding.gate_interval_km - 0.250).abs() < 1e-6);
        assert_eq!(inventory.azimuth_number, vec![1]);
        assert_eq!(inventory.elevation_number, vec![1]);
        assert_eq!(inventory.collection_time_ms, vec![0]);
    }

    /// Two *records* disagreeing is the realistic cross-RDA case in a
    /// chunked volume; only the within-record path was covered.
    #[test]
    fn sweep_inventory_flags_encodings_that_differ_across_records() {
        let mut span = ldm_record(&radial(1.0, &[zdr8(vec![1])]));
        span.extend_from_slice(&ldm_record(&radial(2.0, &[zdr16(vec![2])])));
        let encoding = sweep_moment_encoding(&span).unwrap().moments[&MomentSelector::Zdr];
        assert_eq!(encoding.radials_present, 2);
        assert!(!encoding.uniform, "the records use different word sizes");
    }

    #[test]
    fn implausible_or_malformed_output_shapes_are_rejected() {
        // Zero gates.
        let mut o = opts(MomentSelector::Ref, 1, 0, OutputWord::U8);
        assert!(decode_record_moment(&[], &o).is_err());

        // Wider than the u16 the wire uses for gate counts.
        o = opts(
            MomentSelector::Ref,
            1,
            usize::from(u16::MAX) + 1,
            OutputWord::U8,
        );
        assert!(decode_record_moment(&[], &o).is_err());

        // Overflows usize.
        o = opts(MomentSelector::Ref, usize::MAX, 2, OutputWord::U8);
        assert!(decode_record_moment(&[], &o).is_err());

        // Fits usize but would abort inside the allocator.
        o = opts(MomentSelector::Ref, 1 << 40, 1000, OutputWord::U8);
        assert!(decode_record_moment(&[], &o).is_err());

        // fill_value boundary for uint8: 255 fits, 256 does not.
        o = opts(MomentSelector::Ref, 1, 1, OutputWord::U8);
        o.fill_value = 255;
        assert!(decode_record_moment(&[], &o).is_ok());
        o.fill_value = 256;
        assert!(decode_record_moment(&[], &o).is_err());
    }

    #[test]
    fn remap_rejects_negative_bias_sub_unit_ratio_and_degenerate_scale() {
        let record = radial(30.0, &[zdr8(vec![10])]);
        let mut o = opts(MomentSelector::Zdr, 1, 1, OutputWord::U16);

        // bias = 0 - 128*1 = -128: raw 0 would map below zero.
        o.target = Some(TargetEncoding::new(16.0, 0.0));
        assert!(decode_record_moment(&record, &o).is_err(), "negative bias");

        // ratio = 8/16 = 0.5.
        o.target = Some(TargetEncoding::new(8.0, 64.0));
        assert!(decode_record_moment(&record, &o).is_err(), "ratio below 1");

        // Degenerate target scales are reachable straight from Python.
        for scale in [0.0, f32::NAN, f32::INFINITY] {
            o.target = Some(TargetEncoding::new(scale, 418.0));
            assert!(
                decode_record_moment(&record, &o).is_err(),
                "scale {scale} must be refused"
            );
        }
    }

    /// The bound is the output moment's significant-bit mask, not just
    /// the output width: 16-bit ZDR only has 11 usable bits.
    #[test]
    fn remap_is_bounded_by_the_moments_mask_not_just_the_word_width() {
        // ratio 4 => widest 255*4 + 0 = 1020, under u16::MAX but well
        // within ZDR's 0x7FF, so this one is fine.
        let record = radial(30.0, &[zdr8(vec![255])]);
        let mut o = opts(MomentSelector::Zdr, 1, 1, OutputWord::U16);
        o.target = Some(TargetEncoding::new(64.0, 512.0));
        assert_eq!(u16s(&decode_record_moment(&record, &o).unwrap()), &[1020]);

        // ratio 16 => widest 4080, past 0x7FF: off the ZDR grid even
        // though it fits a uint16.
        o.target = Some(TargetEncoding::new(256.0, 2048.0));
        let error = decode_record_moment(&record, &o).unwrap_err();
        assert!(
            matches!(error, RadishError::MomentEncoding(ref m) if m.contains("ZDR grid")),
            "got {error}"
        );
    }

    #[test]
    fn sorting_handles_zero_and_one_radial_spans() {
        let o = opts(MomentSelector::Ref, 2, 1, OutputWord::U8);
        // One radial.
        let span = ldm_record(&radial(42.0, &[ref_block(vec![5])]));
        assert_eq!(u8s(&decode_sweep_moment(&span, &o, true).unwrap()), &[5, 0]);
        // No radials.
        let span = ldm_record(&frame_other(2));
        assert_eq!(u8s(&decode_sweep_moment(&span, &o, true).unwrap()), &[0, 0]);
    }

    #[test]
    fn malformed_input_errors_rather_than_panicking() {
        let o = opts(MomentSelector::Ref, 4, 8, OutputWord::U8);

        // A MSG_31 whose declared size runs past the buffer.
        let mut truncated = radial(1.0, &[ref_block(vec![1, 2, 3, 4])]);
        truncated.truncate(truncated.len() / 2);
        let _ = decode_record_moment(&truncated, &o); // must not panic

        // Garbage bytes.
        let _ = decode_record_moment(&[0xAB; 64], &o);

        // An LDM control word pointing past the end of the span.
        let mut span = 9_999_999i32.to_be_bytes().to_vec();
        span.extend_from_slice(&[0u8; 16]);
        assert!(decode_sweep_moment(&span, &o, false).is_err());

        // A well-framed record whose payload isn't valid bzip2.
        let mut span = 32i32.to_be_bytes().to_vec();
        span.extend_from_slice(&[0xFF; 32]);
        assert!(decode_sweep_moment(&span, &o, false).is_err());
    }

    proptest::proptest! {
        /// No caller-supplied byte string may panic any entry point.
        /// These take arbitrary bytes off S3 / out of a zarr store, and
        /// a panic crossing PyO3 surfaces as `PanicException` rather
        /// than the documented error type.
        #[test]
        fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..512)) {
            let o = opts(MomentSelector::Ref, 8, 16, OutputWord::U8);
            let _ = decode_record_moment(&bytes, &o);
            let _ = decode_sweep_moment(&bytes, &o, true);
            let _ = record_moment_encoding(&bytes);
            let _ = sweep_moment_encoding(&bytes);
        }
    }

    /// **Adversarial regression.** Two radials with the *same* word
    /// size but different `scale`/`offset` used to stack into one array
    /// with no error, so the single `scale_factor`/`add_offset` the
    /// caller is handed decoded half of it onto the wrong physical
    /// grid — silent wrong data, the exact failure this module exists
    /// to prevent. Checking only `word_size` was not enough.
    #[test]
    fn same_width_different_scale_is_refused_without_a_target() {
        // Both blocks encode 0.0/1.0/2.0 dB, on different grids.
        let coarse = Block {
            name: b"DZDR",
            word_size: 8,
            scale: 16.0,
            offset: 128.0,
            gates: vec![128, 144, 160],
        };
        let fine = Block {
            name: b"DZDR",
            word_size: 8,
            scale: 8.0,
            offset: 64.0,
            gates: vec![64, 72, 80],
        };
        let mut record = radial(1.0, &[coarse]);
        record.extend_from_slice(&radial(2.0, &[fine]));

        let error = decode_record_moment(&record, &opts(MomentSelector::Zdr, 2, 3, OutputWord::U8))
            .unwrap_err();
        assert!(
            matches!(error, RadishError::MomentEncoding(ref m) if m.contains("mixes on-wire")),
            "got {error}"
        );

        // The same bytes with an explicit target grid must still work —
        // that is precisely what the remap is for.
        let mut o = opts(MomentSelector::Zdr, 2, 3, OutputWord::U16);
        o.target = Some(TargetEncoding::new(16.0, 128.0));
        let out = decode_record_moment(&record, &o).unwrap();
        assert_eq!(u16s(&out), &[128, 144, 160, 128, 144, 160]);
    }

    /// The same disagreement, but split across two LDM records so each
    /// record is internally consistent. Caught by the cross-record
    /// check in `stitch`, not the per-record pin.
    #[test]
    fn same_width_different_scale_is_refused_across_records() {
        let coarse = Block {
            name: b"DZDR",
            word_size: 8,
            scale: 16.0,
            offset: 128.0,
            gates: vec![128],
        };
        let fine = Block {
            name: b"DZDR",
            word_size: 8,
            scale: 8.0,
            offset: 64.0,
            gates: vec![64],
        };
        let mut span = ldm_record(&radial(1.0, &[coarse]));
        span.extend_from_slice(&ldm_record(&radial(2.0, &[fine])));

        let error = decode_sweep_moment(
            &span,
            &opts(MomentSelector::Zdr, 2, 1, OutputWord::U8),
            false,
        )
        .unwrap_err();
        assert!(
            matches!(error, RadishError::MomentEncoding(ref m) if m.contains("mixes on-wire")),
            "got {error}"
        );
    }

    /// **Adversarial regression.** Every record's rows used to be
    /// allocated before the *total* was checked, so peak memory was
    /// `records x rays x gates` rather than the `rays x gates` the
    /// caller declared — ~1.5 GiB from 2 MiB of input, enough to abort
    /// the process. The running total now trips before the buffers
    /// exist.
    #[test]
    fn sweep_rejects_an_oversized_span_before_allocating_every_record() {
        // 8 records x 4 radials, into an out_shape with room for 4.
        let stream: Vec<u8> = (0..4)
            .flat_map(|i| radial(i as f32, &[ref_block(vec![1])]))
            .collect();
        let mut span = Vec::new();
        for _ in 0..8 {
            span.extend_from_slice(&ldm_record(&stream));
        }
        let error = decode_sweep_moment(
            &span,
            &opts(MomentSelector::Ref, 4, 1, OutputWord::U8),
            false,
        )
        .unwrap_err();
        assert!(
            matches!(error, RadishError::MomentEncoding(ref m) if m.contains("MSG_31 radials")),
            "got {error}"
        );
    }

    /// **Adversarial regression.** `f32::total_cmp` orders `-0.0`
    /// before `+0.0` and puts negative NaN first; `np.argsort(kind=
    /// "stable")` — which the docs tell callers to use for their
    /// coordinate arrays — treats the zeros as equal and sorts NaN
    /// last. Misaligned rows against coordinates either way.
    #[test]
    fn azimuth_sort_matches_numpy_on_signed_zero_and_nan() {
        let cases: [(&str, [f32; 3], [u8; 3]); 3] = [
            ("+0.0 then -0.0", [0.0, -0.0, 5.0], [1, 2, 3]),
            ("negative NaN", [1.0, f32::NAN, 0.5], [3, 1, 2]),
            ("positive NaN", [1.0, -f32::NAN, 0.5], [3, 1, 2]),
        ];
        for (label, azimuths, expected) in cases {
            let stream: Vec<u8> = azimuths
                .iter()
                .enumerate()
                .flat_map(|(i, &az)| radial(az, &[ref_block(vec![i as u32 + 1])]))
                .collect();
            let span = ldm_record(&stream);
            let out = decode_sweep_moment(
                &span,
                &opts(MomentSelector::Ref, 3, 1, OutputWord::U8),
                true,
            )
            .unwrap();
            assert_eq!(u8s(&out), &expected, "{label}");
        }
    }

    /// **Adversarial regression.** An `out_shape` that passes the
    /// element cap can still exceed available memory; `vec![_; n]`
    /// would abort the process, which PyO3 cannot convert into an
    /// exception.
    #[test]
    fn an_unsatisfiable_allocation_errors_rather_than_aborting() {
        let mut o = opts(MomentSelector::Ref, 1 << 30, 1, OutputWord::U16);
        o.gates = 1;
        // Just under MAX_OUTPUT_ELEMENTS, so `validate` lets it through
        // and the allocator is the only thing left to say no.
        assert!(o.validate().is_ok());
        match decode_record_moment(&[], &o) {
            Ok(_) => { /* machine had 2 GiB spare; nothing to assert */ }
            Err(e) => assert!(
                matches!(e, RadishError::MomentEncoding(ref m) if m.contains("could not allocate")),
                "expected a graceful allocation error, got {e}"
            ),
        }
    }

    /// Pre-Build-12 raw Archive II files have no LDM wrapper. The sweep
    /// entry points used to call `split_ldm_records` directly, so such a
    /// file died with a generic decode error and never reached the
    /// MSG_1 refusal — the message was unreachable on that path.
    #[test]
    fn sweep_path_routes_raw_archive2_to_the_legacy_refusal() {
        let mut span = b"AR2V0006.001-XYZWXYZWXYZW"[..24].to_vec();
        // No LDM size prefix: the next 4 bytes are the first message's
        // zero-filled TCM header, which is exactly the `u32_be == 0`
        // marker `is_raw_archive2` looks for.
        span.extend_from_slice(&frame_other(1)); // a legacy MSG_1 frame
        debug_assert_eq!(&span[24..28], &[0u8; 4]);
        let error = decode_sweep_moment(
            &span,
            &opts(MomentSelector::Ref, 1, 1, OutputWord::U8),
            false,
        )
        .unwrap_err();
        assert!(
            matches!(error, RadishError::Unsupported(ref m) if m.contains("open_datatree")),
            "got {error}"
        );
    }

    #[test]
    fn remapped_padding_lands_on_the_same_physical_floor_as_raw_zero() {
        // Two gates of ZDR on the 8-bit grid, padded out to four on the
        // 16-bit grid. The pad must be 162 (the remap of raw 0), not a
        // literal 0 — otherwise the pad region would read as -13.06 dB
        // against a -8.0 dB detection floor.
        let record = radial(30.0, &[zdr8(vec![0, 200])]);
        let mut o = opts(MomentSelector::Zdr, 1, 4, OutputWord::U16);
        o.target = Some(TargetEncoding::new(32.0, 418.0));
        let out = decode_record_moment(&record, &o).unwrap();
        assert_eq!(u16s(&out), &[162, 562, 162, 162]);
        for raw in u16s(&out) {
            let physical = (f64::from(*raw) - 418.0) / 32.0;
            assert!(physical >= -8.0, "pad must not fall below the source floor");
        }
    }

    #[test]
    fn unremapped_padding_stays_raw_zero_for_xradar_parity() {
        let record = radial(30.0, &[ref_block(vec![5, 6])]);
        let out = decode_record_moment(&record, &opts(MomentSelector::Ref, 1, 4, OutputWord::U8))
            .unwrap();
        assert_eq!(u8s(&out), &[5, 6, 0, 0]);
    }

    #[test]
    fn native_16bit_zdr_passes_through_when_the_target_matches() {
        let record = radial(30.0, &[zdr16(vec![0, 500, 2047])]);
        let mut o = opts(MomentSelector::Zdr, 1, 3, OutputWord::U16);
        o.target = Some(TargetEncoding::new(32.0, 418.0));
        let out = decode_record_moment(&record, &o).unwrap();
        assert_eq!(u16s(&out), &[0, 500, 2047]);
    }

    #[test]
    fn zdr16_is_masked_to_11_bits() {
        // 0xF7FF has junk in the top 5 bits; the mask must drop them.
        let record = radial(30.0, &[zdr16(vec![0xF7FF])]);
        let out = decode_record_moment(&record, &opts(MomentSelector::Zdr, 1, 1, OutputWord::U16))
            .unwrap();
        assert_eq!(u16s(&out), &[0x07FF]);
    }

    #[test]
    fn phi16_is_masked_to_10_bits() {
        let block = Block {
            name: b"DPHI",
            word_size: 16,
            scale: 2.8361,
            offset: 2.0,
            gates: vec![0xFFFF],
        };
        let record = radial(30.0, &[block]);
        let out = decode_record_moment(&record, &opts(MomentSelector::Phi, 1, 1, OutputWord::U16))
            .unwrap();
        assert_eq!(u16s(&out), &[0x03FF]);
    }

    #[test]
    fn inexact_remap_is_refused() {
        // scale 16 -> 24 gives ratio 1.5: not an integer, so the map
        // would be lossy. Refuse rather than approximate.
        let record = radial(30.0, &[zdr8(vec![10])]);
        let mut o = opts(MomentSelector::Zdr, 1, 1, OutputWord::U16);
        o.target = Some(TargetEncoding::new(24.0, 418.0));
        let err = decode_record_moment(&record, &o).unwrap_err();
        assert!(
            matches!(err, RadishError::MomentEncoding(ref m) if m.contains("not an exact integer")),
            "got {err}"
        );
    }

    #[test]
    fn remap_that_overflows_the_output_width_is_refused() {
        // 8-bit source, ratio 2, into a uint8 output: 255*2 = 510.
        let record = radial(30.0, &[zdr8(vec![10])]);
        let mut o = opts(MomentSelector::Zdr, 1, 1, OutputWord::U8);
        o.target = Some(TargetEncoding::new(32.0, 256.0));
        let err = decode_record_moment(&record, &o).unwrap_err();
        assert!(
            matches!(err, RadishError::MomentEncoding(ref m) if m.contains("overflows the uint8 ZDR grid")),
            "got {err}"
        );
    }

    #[test]
    fn fill_value_must_fit_the_output_width() {
        let mut o = opts(MomentSelector::Ref, 1, 1, OutputWord::U8);
        o.fill_value = 300;
        assert!(decode_record_moment(&[], &o).is_err());
    }

    #[test]
    fn inventory_reports_per_radial_headers_and_encodings() {
        let mut record = radial(10.5, &[ref_block(vec![1, 2, 3]), zdr8(vec![4, 5])]);
        record.extend_from_slice(&radial(11.5, &[ref_block(vec![6, 7])]));

        let inventory = record_moment_encoding(&record).unwrap();
        assert_eq!(inventory.radial_count, 2);
        assert_eq!(inventory.azimuth, vec![10.5, 11.5]);
        assert_eq!(inventory.elevation, vec![0.5, 0.5]);
        assert_eq!(inventory.modified_julian_date, vec![20_405, 20_405]);

        let reflectivity = &inventory.moments[&MomentSelector::Ref];
        assert_eq!(reflectivity.word_size, 8);
        assert_eq!(reflectivity.scale, 2.0);
        assert_eq!(reflectivity.offset, 66.0);
        assert_eq!(reflectivity.gate_count, 3);
        assert_eq!(reflectivity.max_gate_count, 3);
        assert_eq!(reflectivity.radials_present, 2);
        assert!(reflectivity.uniform);

        // ZDR only appears on the first radial.
        let zdr = &inventory.moments[&MomentSelector::Zdr];
        assert_eq!(zdr.radials_present, 1);
        assert!(!inventory.moments.contains_key(&MomentSelector::Vel));
    }

    #[test]
    fn inventory_flags_mixed_encodings_as_non_uniform() {
        let mut record = radial(1.0, &[zdr8(vec![1, 2])]);
        record.extend_from_slice(&radial(2.0, &[zdr16(vec![3, 4])]));
        let inventory = record_moment_encoding(&record).unwrap();
        let zdr = &inventory.moments[&MomentSelector::Zdr];
        assert_eq!(zdr.word_size, 8, "first-seen wins");
        assert!(!zdr.uniform, "the second radial switched to 16-bit");
    }

    /// Wrap one decompressed message stream as an LDM record:
    /// `[i32 size][bzip2 payload]`.
    fn ldm_record(payload: &[u8]) -> Vec<u8> {
        use bzip2::write::BzEncoder;
        use bzip2::Compression;
        use std::io::Write;

        let mut compressed = Vec::new();
        let mut encoder = BzEncoder::new(&mut compressed, Compression::default());
        encoder.write_all(payload).unwrap();
        encoder.finish().unwrap();
        let mut frame = (compressed.len() as i32).to_be_bytes().to_vec();
        frame.extend_from_slice(&compressed);
        frame
    }

    #[test]
    fn sweep_span_stitches_records_in_order() {
        let mut span = ldm_record(&radial(90.0, &[ref_block(vec![1, 2])]));
        let mut second = radial(10.0, &[ref_block(vec![3, 4])]);
        second.extend_from_slice(&radial(20.0, &[ref_block(vec![5, 6])]));
        span.extend_from_slice(&ldm_record(&second));

        let out = decode_sweep_moment(
            &span,
            &opts(MomentSelector::Ref, 3, 2, OutputWord::U8),
            false,
        )
        .unwrap();
        assert_eq!(u8s(&out), &[1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn sweep_span_skips_a_leading_ar2v_volume_header() {
        let mut span = b"AR2V0006.001-XYZWXYZWXYZW"[..24].to_vec();
        span.extend_from_slice(&ldm_record(&radial(5.0, &[ref_block(vec![9, 8])])));
        let out = decode_sweep_moment(
            &span,
            &opts(MomentSelector::Ref, 1, 2, OutputWord::U8),
            false,
        )
        .unwrap();
        assert_eq!(u8s(&out), &[9, 8]);
    }

    #[test]
    fn sort_by_azimuth_is_stable_and_leaves_fill_rows_at_the_end() {
        // Record order 270, 10, 10, 90 -> sorted 10, 10, 90, 270 with
        // the two 10s keeping their relative order (rows 1 then 2).
        let mut stream = radial(270.0, &[ref_block(vec![1])]);
        stream.extend_from_slice(&radial(10.0, &[ref_block(vec![2])]));
        stream.extend_from_slice(&radial(10.0, &[ref_block(vec![3])]));
        stream.extend_from_slice(&radial(90.0, &[ref_block(vec![4])]));
        let span = ldm_record(&stream);

        let mut o = opts(MomentSelector::Ref, 6, 1, OutputWord::U8);
        o.fill_value = 200;
        let out = decode_sweep_moment(&span, &o, true).unwrap();
        assert_eq!(u8s(&out), &[2, 3, 4, 1, 200, 200]);
    }

    #[test]
    fn sweep_inventory_merges_across_records() {
        let mut span = ldm_record(&radial(1.0, &[ref_block(vec![1, 2, 3])]));
        span.extend_from_slice(&ldm_record(&radial(2.0, &[ref_block(vec![4])])));

        let inventory = sweep_moment_encoding(&span).unwrap();
        assert_eq!(inventory.radial_count, 2);
        assert_eq!(inventory.azimuth, vec![1.0, 2.0]);
        let reflectivity = &inventory.moments[&MomentSelector::Ref];
        assert_eq!(reflectivity.gate_count, 3, "first-seen wins");
        assert_eq!(reflectivity.max_gate_count, 3);
        assert_eq!(reflectivity.radials_present, 2);
        // Same scale/offset in both records, only the gate count moved.
        assert!(reflectivity.uniform);
    }

    #[test]
    fn mixed_encodings_still_decode_onto_a_common_target_grid() {
        // Same physical value (-8.0 dB) encoded both ways: raw8 = 0 on
        // the 8-bit grid, raw16 = 162 on the 16-bit grid.
        let mut record = radial(1.0, &[zdr8(vec![0])]);
        record.extend_from_slice(&radial(2.0, &[zdr16(vec![162])]));
        let mut o = opts(MomentSelector::Zdr, 2, 1, OutputWord::U16);
        o.target = Some(TargetEncoding::new(32.0, 418.0));
        let out = decode_record_moment(&record, &o).unwrap();
        assert_eq!(u16s(&out), &[162, 162]);
    }
}
