//! Errors raised by the NEXRAD Level 3 (NIDS) byte-level decoder.
//!
//! One variant per raise site in the independent Python decode oracle
//! this backend must match byte-for-byte on — see
//! `docs/NEXRAD_LEVEL3_WASM.md`. Kept 1:1 with the oracle deliberately:
//! the parity test suite (`radish/tests/test_nexrad_level3_parity.rs`)
//! needs to assert "this input fails to decode" as precisely as "this
//! input decodes to these bytes", and a merged catch-all variant would
//! blur which check actually fired.

use thiserror::Error;

/// Errors that can surface while decoding a NEXRAD Level 3 (NIDS) product.
/// `pub(crate)`, not `pub`: never referenced outside the `decode` module
/// tree — the `backends::nexrad_level3` boundary wraps it into
/// `RadishError::Decode(String)` via `Display`, matching how the `nexrad`
/// backend wraps its own internal `NexradDecodeError`.
#[derive(Debug, Error)]
pub(crate) enum Level3DecodeError {
    /// The text header didn't contain any 6-character alphanumeric token
    /// at all. Since Phase 3 this is no longer about product recognition
    /// (moved to the message code, `products::spec_for`) — a malformed or
    /// missing token means no site string is available at all.
    #[error("no AWIPS product id in the text header")]
    NoAwipsToken,

    /// No offset in the first 128 bytes both matched a known message code
    /// and had the PDB's `-1` divider 18 bytes later.
    #[error("no digital radial message header found (known message codes {known:?})")]
    NoMessageHeader { known: Vec<u16> },

    /// Fewer than `need` bytes before the product description block ends
    /// — reading past this would hit `struct.unpack`-equivalent UB.
    #[error("truncated before the product description block ({have} bytes, need {need})")]
    TruncatedBeforePdb { have: usize, need: usize },

    /// PDB halfword 7 (product code) disagreed with the message header's
    /// code — the file is internally inconsistent.
    #[error("product code {product_code} disagrees with message code {message_code}")]
    ProductCodeMismatch {
        product_code: i16,
        message_code: i16,
    },

    /// Decoded site lat/lon isn't a point on Earth.
    #[error("site position ({lat}, {lon}) is not on Earth")]
    ImplausibleSitePosition { lat: f64, lon: f64 },

    /// The float32 scale form's `value_increment` came out non-positive.
    #[error("non-positive value increment {0}")]
    NonPositiveIncrement(f32),

    /// The float32 scale form's declared `scale` is zero or non-finite.
    #[error("non-finite or zero value scale {0}")]
    NonFiniteScale(f32),

    /// The float32 scale form's declared `offset` is non-finite.
    #[error("non-finite value offset {0}")]
    NonFiniteOffset(f32),

    /// PDB `days`/`seconds` don't form a plausible date/time.
    #[error("implausible scan date/time (days={days}, seconds={seconds})")]
    ImplausibleScanTime { days: u16, seconds: i32 },

    /// Decoded scan time is further in the future than
    /// `MAX_SCAN_SKEW_SECONDS` allows — corruption, not clock skew.
    #[error("scan time is {days_ahead:.0} days in the future")]
    ScanTimeTooFarAhead { days_ahead: f64 },

    /// An uncompressed payload (no `BZ` magic) exceeded the decompression
    /// cap on its own — nothing else bounds a raw payload's size.
    #[error("uncompressed payload of {len} bytes exceeds the {cap}-byte decompression cap")]
    UncompressedPayloadTooLarge { len: usize, cap: usize },

    /// The bzip2 stream ended without reaching its own end-of-stream
    /// marker — a short/partial read, not a size violation.
    #[error("bzip2 stream ends mid-payload")]
    Bzip2Truncated,

    /// Incremental bzip2 decompression hit the hard byte cap before the
    /// stream finished — the decompression-bomb guard firing.
    #[error("payload exceeds the {cap}-byte decompression cap")]
    Bzip2ExceedsCap { cap: usize },

    /// bzip2 decompression failed for a reason other than truncation or
    /// exceeding the cap (corrupt stream).
    #[error("corrupt bzip2 payload: {0}")]
    Bzip2Corrupt(String),

    /// Bytes remained after the bzip2 stream ended that weren't a
    /// tolerated WMO/NOAAPORT terminator — a sign of a body cut in half
    /// and concatenated with something else.
    #[error("{trailing_len} unexpected bytes after the bzip2 stream")]
    UnexpectedTrailingBytes { trailing_len: usize },

    /// The decompressed symbology block is shorter than its own fixed
    /// header.
    #[error("symbology block truncated ({len} bytes)")]
    SymbologyTruncated { len: usize },

    /// The symbology block's divider/block-id pair didn't match the ICD's
    /// fixed values.
    #[error("bad symbology header (divider={divider}, block_id={block_id})")]
    BadSymbologyHeader { divider: i16, block_id: i16 },

    /// The packet declared zero radials or zero bins.
    #[error("empty sweep ({n_radials} radials x {n_bins} bins)")]
    EmptySweep { n_radials: u16, n_bins: u16 },

    /// The packet's first-bin offset wasn't 0 — never observed in
    /// practice, and honouring a nonzero value would shift every gate.
    #[error("first range bin {0} is not 0; unsupported")]
    NonZeroFirstBin(u16),

    /// `n_radials * (6 + n_bins)` exceeds the bytes actually remaining in
    /// the body — the pre-allocation guard against a multi-GiB allocation
    /// from a tiny crafted input.
    #[error("{n_radials}x{n_bins} needs {needed} bytes, only {available} remain")]
    AllocationExceedsBody {
        n_radials: u16,
        n_bins: u16,
        needed: usize,
        available: usize,
    },

    /// The body ended before every declared radial's 6-byte header could
    /// be read.
    #[error("truncated at radial {radial}/{n_radials}")]
    TruncatedAtRadial { radial: u16, n_radials: u16 },

    /// A radial's declared angular width is 0 or wider than 10 degrees.
    #[error("radial {radial} has an implausible width {width_deg} deg")]
    ImplausibleRadialWidth { radial: u16, width_deg: f32 },

    /// A radial declared fewer bytes than the packet's bin count, or its
    /// declared byte count runs past the end of the body.
    #[error("radial {radial} declares {n_bytes} bytes for {n_bins} bins")]
    RadialByteCountMismatch {
        radial: u16,
        n_bytes: u16,
        n_bins: u16,
    },

    /// A PDB halfword read fell outside the buffer. Should be unreachable
    /// in practice — callers check `raw.len() >= pdb + 102` before reading
    /// any PDB field, and every field this decoder reads sits within that
    /// span — but reads stay checked rather than trusting that invariant
    /// silently, matching this crate's no-panics-on-untrusted-input
    /// discipline (`docs/NEXRAD_LEVEL3_WASM.md` §4.7).
    #[error("PDB halfword {index} is out of bounds")]
    PdbFieldOutOfBounds { index: usize },

    /// The symbology layer's packet code is neither the digital radial
    /// array (16), RLE radials (`AF1F`), nor the generic data packet (28,
    /// XDR — `RATE`/code 176's packet family) — some OTHER packet code
    /// this backend has no product wired to at all. Distinct from
    /// [`Self::UnsupportedProduct`]: that fires before symbology decode is
    /// even attempted, for a message code whose packet family isn't
    /// implemented; this fires mid-symbology-decode, for a packet code
    /// this backend has never seen paired with any message code it knows.
    #[error("unsupported symbology packet code {code} (0x{code:04x})")]
    UnsupportedPacketCode { code: i16 },

    /// The message code is recognised (`products::is_known_message_code`)
    /// but `products::packet_family_implemented` reports its packet
    /// family isn't implemented — as of plan 0012, this is unreachable for
    /// every code actually in `products::PRODUCTS` (all 34 are
    /// implemented); kept as a real, reachable error path for whenever a
    /// future product is added to the table before its packet family is,
    /// the same "known but deferred" state the original 7 codes (170/
    /// 172-175/176/177) occupied before this pass closed them.
    #[error("NEXRAD Level 3 message code {code} is a known product but its packet family isn't implemented yet")]
    UnsupportedProduct { code: i16 },

    /// A packet-AF1F (RLE) radial's runs didn't sum to exactly `expected`
    /// bins — either a malformed input or (rarer) a genuinely different
    /// encoding this decoder doesn't handle.
    #[error("radial {radial} RLE runs expand to {expanded} bins, expected {expected}")]
    RleExpansionMismatch {
        radial: u16,
        expanded: usize,
        expected: u16,
    },

    /// `n_radials * n_bins` (the codes array this file would allocate)
    /// exceeds [`MAX_GATE_COUNT`](super::symbology::MAX_GATE_COUNT) —
    /// checked BEFORE allocating, in `u64`, and independent of how many
    /// body bytes are actually present. `AllocationExceedsBody` alone
    /// isn't enough for packet `AF1F`: RLE compression means a tiny body
    /// can legitimately declare a huge `n_radials`/`n_bins`, so bounding
    /// only against "bytes available" (as packet 16's per-radial check
    /// does) can't catch it — this bounds the allocation itself instead.
    #[error("{n_radials}x{n_bins} = {cells} gates exceeds the {max}-gate cap")]
    GridTooLarge {
        n_radials: u16,
        n_bins: u16,
        cells: u64,
        max: u64,
    },

    // -- Packet 28 (XDR, generic data packet) — added for codes 176/177 --
    /// XDR parsing ran out of bytes before finishing a fixed-size read.
    /// `context` names what was being read (`"int"`, `"float"`, `"string
    /// body"`, ...) — a truncated or corrupt packet, not a panic.
    #[error("truncated XDR payload while reading {context} ({have} bytes, need {need})")]
    XdrTruncated {
        context: &'static str,
        have: usize,
        need: usize,
    },

    /// An XDR string's declared length prefix exceeds
    /// [`MAX_XDR_STRING_LEN`](super::xdr::MAX_XDR_STRING_LEN) — rejected
    /// BEFORE the byte read, the packet-28 analogue of `GridTooLarge`/
    /// `AllocationExceedsBody`'s pre-allocation discipline (`docs/
    /// NEXRAD_LEVEL3_WASM.md` §4.7: an untrusted length prefix must never
    /// drive an allocation before it's bounds-checked).
    #[error("XDR string length {len} exceeds the {max}-byte cap")]
    XdrStringTooLong { len: u32, max: u32 },

    /// An XDR int array's declared element count exceeds
    /// [`MAX_XDR_ARRAY_LEN`](super::xdr::MAX_XDR_ARRAY_LEN) — same
    /// pre-allocation discipline as [`Self::XdrStringTooLong`].
    #[error("XDR int array length {len} exceeds the {max}-element cap")]
    XdrArrayTooLong { len: u32, max: u32 },

    /// A "counted list" (`parameters`/`components`) declared a count
    /// exceeding [`MAX_XDR_LIST_LEN`](super::xdr::MAX_XDR_LIST_LEN) — a
    /// fast, explicit refusal rather than relying only on the loop running
    /// out of real bytes eventually (see that constant's own doc).
    #[error("XDR list length {len} exceeds the {max}-element cap")]
    XdrListTooLong { len: i32, max: i32 },

    /// A generic-data-packet (28) component's type code wasn't `1`
    /// ("radial") — the only component type this backend decodes. A real,
    /// named refusal (matching the reference oracle's own
    /// `NotImplementedError` for unknown component types), not a guess at
    /// what an unknown type means.
    #[error("unsupported generic-data-packet component type {0} (only radial/1 is decoded)")]
    UnsupportedXdrComponent(i32),

    /// A generic-data-packet declared zero components, or more than one —
    /// this backend (like both reference readers it was cross-checked
    /// against) only decodes exactly one radial component per product.
    #[error("generic data packet declares {0} components, expected exactly 1")]
    UnexpectedXdrComponentCount(usize),

    /// A generic-data-packet radial declared `num_rads` or a first
    /// radial's `num_bins` outside `1..=i32::MAX` in a way that makes the
    /// geometry unusable (non-positive) — rejected before allocating the
    /// `(nradials, nbins)` output array. XDR reads these as signed `i32`
    /// (unlike packet 16/AF1F's unsigned `u16` header fields), so zero and
    /// negative both need an explicit check here that packet 16/AF1F don't
    /// need.
    #[error(
        "generic data packet geometry {n_radials}x{n_bins} is implausible (both must be positive)"
    )]
    NonPositiveXdrGeometry { n_radials: i32, n_bins: i32 },

    /// A radial component's `num_rads` exceeds
    /// [`MAX_XDR_RADIALS`](super::xdr::MAX_XDR_RADIALS) — rejected before
    /// starting the per-radial parse loop at all, a fast/clear refusal
    /// rather than relying only on the loop eventually running out of real
    /// (already-capped) decompressed bytes.
    #[error("generic data packet declares {n_radials} radials, exceeding the {max}-radial cap")]
    ImplausibleXdrRadialCount { n_radials: i32, max: i32 },

    /// A generic-data-packet's `(n_radials, n_bins)` cell count exceeds
    /// [`MAX_GATE_COUNT`](super::symbology::MAX_GATE_COUNT) — the same
    /// pre-allocation cap packet 16/AF1F's `GridTooLarge` enforces, shared
    /// here rather than duplicated with a second constant.
    #[error("{n_radials}x{n_bins} = {cells} gates exceeds the {max}-gate cap")]
    XdrGridTooLarge {
        n_radials: i32,
        n_bins: i32,
        cells: u64,
        max: u64,
    },

    /// A generic-data-packet radial's `num_bins` disagreed with the first
    /// radial's — this backend, unlike the reference oracle (see plan 0012
    /// §3.1's note that it doesn't check this), requires every radial in a
    /// product to declare the same gate count, matching packet 16/AF1F's
    /// existing uniform-grid assumption (`Array2` has one shape for the
    /// whole product; there's nowhere to put a per-radial-varying count).
    #[error("radial {radial} declares {n_bins} bins, expected {expected} (from radial 0)")]
    XdrRadialBinCountMismatch {
        radial: usize,
        n_bins: i32,
        expected: i32,
    },

    /// A generic-data-packet radial's `data` array length (its OWN XDR
    /// count prefix) disagreed with that same radial's `num_bins` field —
    /// two numbers that should always agree for a well-formed product;
    /// this backend checks rather than assumes (the reference oracle
    /// doesn't check this either).
    #[error("radial {radial} data array has {declared} elements, expected {num_bins} (its own num_bins)")]
    XdrRadialDataLengthMismatch {
        radial: usize,
        declared: usize,
        num_bins: i32,
    },

    /// A generic-data-packet radial's raw level fell outside `0..=u16::MAX`
    /// — XDR encodes it as a signed `i32`, but this backend's own model
    /// (`MomentData::raw_codes_u16: Array2<u16>`) and the ICD both say
    /// packet 28's raw levels are `u16`. Refused rather than silently
    /// truncated/wrapped into range — this crate's standing "refuse, never
    /// silently rescale" rule (`docs/NEXRAD_LEVEL3_WASM.md` §4.2) applied
    /// to raw-code fidelity, not just physical-value scaling.
    #[error("radial {radial} gate {gate} raw level {value} is outside u16 range")]
    XdrRawLevelOutOfRange {
        radial: usize,
        gate: usize,
        value: i32,
    },

    /// The XDR parser finished the product description + one radial
    /// component without consuming the entire declared payload (or
    /// consumed past it) — a sign the field-order this backend assumes
    /// diverged from what the file actually contains, rather than a
    /// merely-unused trailer. Confirmed on real fixtures that a correct
    /// parse consumes the payload to EXACTLY zero bytes remaining (plan
    /// 0012 §3's implementation notes) — so any nonzero remainder here is
    /// treated as a parse-shape bug, not benign padding.
    #[error("XDR payload has {remaining} bytes left over after parsing (expected 0)")]
    XdrTrailingBytes { remaining: usize },

    // -- Internal invariant defense, not reachable from crafted input --
    /// A `(DecodeScheme, raw-code width)` combination that should never
    /// occur — every scheme this backend implements is wired to exactly
    /// one packet family (packet 16/AF1F -> `u8`, packet 28 -> `u16`), so
    /// reaching this means `products.rs`'s table and `symbology.rs`'s
    /// dispatch disagree about a scheme's packet family. A radish bug, not
    /// malformed input — still a named error rather than `unreachable!()`,
    /// matching this crate's loud-rather-than-panicking discipline even
    /// for its own internal invariants.
    #[error(
        "decode scheme produced unexpected raw-code width (expected {expected}, got {actual})"
    )]
    RawCodeWidthMismatch {
        expected: &'static str,
        actual: &'static str,
    },
}

/// Convenience alias used throughout the decode module.
pub(crate) type Result<T> = std::result::Result<T, Level3DecodeError>;
