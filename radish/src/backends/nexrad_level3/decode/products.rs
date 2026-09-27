//! The product table this backend can decode: message code -> moment,
//! decode scheme, and geometry. Plus a small, SEPARATE tilt-ordinal table
//! for the 6 products whose AWIPS letter -> tilt mapping was independently
//! verified against real files pulled from `s3://unidata-nexrad-level3`.
//!
//! **Two tables, on purpose, for two different questions:**
//!
//! - "What moment/decode scheme does message code N carry?" — [`PRODUCTS`],
//!   keyed by message code. Ported from xradar #392's `PRODUCT_TABLE`
//!   (openradar/xradar, commit `fcc31ae`), which is the breadth reference
//!   for the 20 codes beyond the original 6 — see `docs/NEXRAD_LEVEL3_WASM.md`
//!   §3. This is the ONLY correct way to resolve moment: a message
//!   code identifies the *format*, and unlike tilt, every file with that
//!   code carries the *same* moment regardless of which tilt it's at.
//! - "Which of the 6 canonical tilt ordinals (0-5) does AWIPS letter L at
//!   this file's site belong to?" — [`TILT_LETTER_TABLE`] /
//!   [`tilt_letter_lookup`]. It only covers the letters (`B`,`G`,`U`,`X`,
//!   `C`,`K`,`S`,`H`) independently confirmed present in the bucket.
//!   Fabricating AWIPS letters for the rest was deliberately NOT done here —
//!   this repo has no S3-verified source for them the way these have, and
//!   the standing rule for this decoder is "every added code needs a
//!   fixture or a verified source, or it is untested surface." Two letters
//!   are deliberately NOT here despite having verified real-world AWIPS ids
//!   (`W` for `WRADH`'s `NSW`, `P` for `ACCUM`'s `N1P`/`N3P`/`NTP`) — see
//!   [`TILT_LETTER_TABLE`]'s own doc comment for why each doesn't fit this
//!   table's shape. `DecodedProduct::tilt` is `Option<usize>`: `Some` only
//!   when this table resolves it, `None` otherwise — never guessed from
//!   elevation angle. The real elevation always comes from the PDB
//!   regardless.

/// `(AWIPS tilt prefix, ordinal)` — elevation order, matching the byte-level
/// oracle's own tilt-prefix table.
pub(crate) const TILT_PREFIXES: [&str; 6] = ["N0", "NA", "N1", "NB", "N2", "N3"];

/// How a product's raw levels become physical values. Six schemes, matching
/// xradar #392's `ProductSpec.decode` string values exactly
/// (`linear_hw`/`float_scale`/`precip`/`rate`/`legacy16`/`class_int`) — an
/// enum here, not a string, for the same reason `docs/NEXRAD_LEVEL3_WASM.md`
/// §4.3 gives for the original int/float split: the type system should make
/// an unrepresentable scheme unrepresentable, not merely undetected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecodeScheme {
    /// Halfwords 22/23 as `i16`: `scale = hw23/10 * post_scale`,
    /// `offset = (hw22/10 - 2*hw23/10) * post_scale`. Codes 94/99/153/154/155.
    LinearHw,
    /// Halfwords 22-25 as two big-endian `f32` (scale, offset); physical =
    /// `(raw - offset) / scale`. Codes 159/161/163.
    FloatScale,
    /// Same byte layout as `FloatScale`, but the post-decode factor is
    /// fixed at `0.01 in -> mm` regardless of `post_scale`, and the floor/
    /// ceiling flag-count field comes from further into the PDB
    /// (`pdb::precip_family_scale`) than `FloatScale` ever reads — surface
    /// accumulation products (170/172-175). Packet 16, confirmed against
    /// real `DAA`/`DTA`/`DU3`/`DU6` fixtures (plan 0012 §2.4).
    Precip,
    /// Same PDB layout and flag-count logic as `Precip`, but the factor is
    /// `1 in -> mm` (code 176, `DPR` rate) — arrives via packet 28 (XDR),
    /// so its raw levels are `u16`, not `u8` (see [`super::RawCodes`]).
    /// Confirmed against a real `DPR` fixture (plan 0012 §3).
    Rate,
    /// The 16-level flag-encoded legacy scheme: `threshold_data` is 16
    /// `(flag, value)` byte pairs, one entry per possible raw level 0-15.
    /// Codes 19/20/25/27/28/30/56/78/79/80, all via packet AF1F.
    Legacy16,
    /// Categorical — codes index a fixed label table, not a linear scale.
    /// Code 165 (`HCLASS`) and code 177 (`HHC`, best-tilt composite) BOTH
    /// arrive via packet 16 — code 177 was originally assumed (plan 0012's
    /// first draft) to need packet 28 like `Rate`/176, but three
    /// independently-fetched real `HHC` fixtures all declared packet code
    /// 16, not 28 (plan 0012 §3's implementation notes) — the assumption
    /// was wrong, not the file. `packet_family_implemented`'s ClassInt
    /// arm no longer needs to special-case 177 at all as a result.
    ClassInt,
}

/// One message code's decode parameters. Mirrors xradar's `ProductSpec`
/// (`nexrad_level3.py` in xradar, i.e. `xradar/io/backends/nexrad_level3.py`)
/// field for field.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProductSpec {
    pub moment: &'static str,
    pub decode: DecodeScheme,
    /// Range bin size in metres, when the ICD resolution is fixed and
    /// known. `None` means: use the packet header's own range-scale field
    /// directly — correct ONLY for surface products, where (unlike
    /// elevation-bearing ones) that field is not
    /// `floor(1000 * cos(elevation))`. See `docs/NEXRAD_LEVEL3_WASM.md` §4.4.
    pub bin_size: Option<f32>,
    /// Whether this product carries a real elevation angle (PDB
    /// `halfword_30`-equivalent) vs. `0.0` for surface/volume products.
    pub has_elevation: bool,
    /// Unit-conversion factor applied after the raw scale/offset — e.g.
    /// knots -> m/s for legacy velocity products.
    pub post_scale: f32,
}

/// 1 international knot in m/s.
const KT_TO_MS: f32 = 0.514444;

/// Message code -> product spec. Ported from xradar #392's `PRODUCT_TABLE`.
/// Every entry decodes today (plan 0012 closed the last 7: 170/172-175 via
/// `Precip`, 176 via `Rate`/packet 28 (`u16` raw levels — see
/// [`super::RawCodes`]), 177 via `ClassInt`/packet 16).
pub(crate) const PRODUCTS: &[(i16, ProductSpec)] = &[
    // --- packet AF1F (RLE), legacy 16-level scheme ---
    (
        19,
        ProductSpec {
            moment: "DBZH",
            decode: DecodeScheme::Legacy16,
            bin_size: Some(1000.0),
            has_elevation: true,
            post_scale: 1.0,
        },
    ),
    (
        20,
        ProductSpec {
            moment: "DBZH",
            decode: DecodeScheme::Legacy16,
            bin_size: Some(2000.0),
            has_elevation: true,
            post_scale: 1.0,
        },
    ),
    (
        25,
        ProductSpec {
            moment: "VRADH",
            decode: DecodeScheme::Legacy16,
            bin_size: Some(250.0),
            has_elevation: true,
            post_scale: KT_TO_MS,
        },
    ),
    (
        27,
        ProductSpec {
            moment: "VRADH",
            decode: DecodeScheme::Legacy16,
            bin_size: Some(1000.0),
            has_elevation: true,
            post_scale: KT_TO_MS,
        },
    ),
    (
        28,
        ProductSpec {
            moment: "WRADH",
            decode: DecodeScheme::Legacy16,
            bin_size: Some(250.0),
            has_elevation: true,
            post_scale: KT_TO_MS,
        },
    ),
    (
        30,
        ProductSpec {
            moment: "WRADH",
            decode: DecodeScheme::Legacy16,
            bin_size: Some(1000.0),
            has_elevation: true,
            post_scale: KT_TO_MS,
        },
    ),
    (
        56,
        ProductSpec {
            moment: "SRMV",
            decode: DecodeScheme::Legacy16,
            bin_size: Some(1000.0),
            has_elevation: true,
            post_scale: KT_TO_MS,
        },
    ),
    (
        78,
        ProductSpec {
            moment: "ACCUM",
            decode: DecodeScheme::Legacy16,
            bin_size: None,
            has_elevation: false,
            post_scale: 25.4, // 1 in -> mm
        },
    ),
    (
        79,
        ProductSpec {
            moment: "ACCUM",
            decode: DecodeScheme::Legacy16,
            bin_size: None,
            has_elevation: false,
            post_scale: 25.4,
        },
    ),
    (
        80,
        ProductSpec {
            moment: "ACCUM",
            decode: DecodeScheme::Legacy16,
            bin_size: None,
            has_elevation: false,
            post_scale: 25.4,
        },
    ),
    // --- packet 16, linear halfword scale ---
    (
        94,
        ProductSpec {
            moment: "DBZH",
            decode: DecodeScheme::LinearHw,
            bin_size: Some(1000.0),
            has_elevation: true,
            post_scale: 1.0,
        },
    ),
    (
        99,
        ProductSpec {
            moment: "VRADH",
            decode: DecodeScheme::LinearHw,
            bin_size: Some(250.0),
            has_elevation: true,
            post_scale: 1.0,
        },
    ),
    (
        153,
        ProductSpec {
            moment: "DBZH",
            decode: DecodeScheme::LinearHw,
            bin_size: Some(250.0),
            has_elevation: true,
            post_scale: 1.0,
        },
    ),
    (
        154,
        ProductSpec {
            moment: "VRADH",
            decode: DecodeScheme::LinearHw,
            bin_size: Some(250.0),
            has_elevation: true,
            post_scale: 1.0,
        },
    ),
    (
        155,
        ProductSpec {
            moment: "WRADH",
            decode: DecodeScheme::LinearHw,
            bin_size: Some(250.0),
            has_elevation: true,
            post_scale: 1.0,
        },
    ),
    // --- packet 16, float scale/offset (dual-pol) ---
    (
        159,
        ProductSpec {
            moment: "ZDR",
            decode: DecodeScheme::FloatScale,
            bin_size: Some(250.0),
            has_elevation: true,
            post_scale: 1.0,
        },
    ),
    (
        161,
        ProductSpec {
            moment: "RHOHV",
            decode: DecodeScheme::FloatScale,
            bin_size: Some(250.0),
            has_elevation: true,
            post_scale: 1.0,
        },
    ),
    (
        163,
        ProductSpec {
            moment: "KDP",
            decode: DecodeScheme::FloatScale,
            bin_size: Some(250.0),
            has_elevation: true,
            post_scale: 1.0,
        },
    ),
    // --- packet 16, categorical ---
    (
        165,
        ProductSpec {
            moment: "HCLASS",
            decode: DecodeScheme::ClassInt,
            bin_size: Some(250.0),
            has_elevation: true,
            post_scale: 1.0,
        },
    ),
    // --- packet 16 (`Precip`, 170/172-175) / packet 28 XDR (`Rate`, 176) /
    //     packet 16 (`ClassInt`, 177) — closed by plan 0012 ---
    (
        170,
        ProductSpec {
            moment: "ACCUM",
            decode: DecodeScheme::Precip,
            bin_size: None,
            has_elevation: false,
            post_scale: 1.0,
        },
    ),
    (
        172,
        ProductSpec {
            moment: "ACCUM",
            decode: DecodeScheme::Precip,
            bin_size: None,
            has_elevation: false,
            post_scale: 1.0,
        },
    ),
    (
        173,
        ProductSpec {
            moment: "ACCUM",
            decode: DecodeScheme::Precip,
            bin_size: None,
            has_elevation: false,
            post_scale: 1.0,
        },
    ),
    (
        174,
        ProductSpec {
            moment: "ACCUM",
            decode: DecodeScheme::Precip,
            bin_size: None,
            has_elevation: false,
            post_scale: 1.0,
        },
    ),
    (
        175,
        ProductSpec {
            moment: "ACCUM",
            decode: DecodeScheme::Precip,
            bin_size: None,
            has_elevation: false,
            post_scale: 1.0,
        },
    ),
    (
        176,
        ProductSpec {
            moment: "RATE",
            decode: DecodeScheme::Rate,
            bin_size: None,
            has_elevation: false,
            post_scale: 1.0,
        },
    ),
    (
        177,
        ProductSpec {
            moment: "HCLASS",
            decode: DecodeScheme::ClassInt,
            bin_size: Some(250.0),
            has_elevation: false,
            post_scale: 1.0,
        },
    ),
];

/// Whether `code`'s symbology packet type is implemented — as of plan
/// 0012, every code in [`PRODUCTS`] is (`None` for an unknown code is the
/// only `false` this function returns anymore).
///
/// Exhaustively matched on `Option<DecodeScheme>`, not a simplified
/// `Some(_) => true`, on purpose: a future `DecodeScheme` variant forces a
/// compile error here instead of silently being treated as implemented —
/// the same "make the unrepresentable state a compile error" discipline
/// [`DecodeScheme`]'s own doc comment asks for. Kept as a named function
/// (not inlined at its one call site in `decode/mod.rs`) so that
/// discipline has one place to live as the product table keeps growing.
pub(crate) fn packet_family_implemented(code: i16) -> bool {
    match decode_scheme_for(code) {
        None => false,
        Some(
            DecodeScheme::LinearHw
            | DecodeScheme::FloatScale
            | DecodeScheme::Legacy16
            | DecodeScheme::ClassInt
            | DecodeScheme::Precip
            | DecodeScheme::Rate,
        ) => true,
    }
}

/// `(AWIPS letter, tilts)` for the products with an S3-verified AWIPS-id
/// table. `letter` is the AWIPS id's third character (`N0B` -> `B`);
/// `tilts` are indices into [`TILT_PREFIXES`].
const ALL_TILTS: &[usize] = &[0, 1, 2, 3, 4, 5];
const LOWER_TILTS: &[usize] = &[0, 1, 2];
const UPPER_TILTS: &[usize] = &[3, 4, 5];
/// `N0`, `N1`, `N2`, `N3` — every split-cut tilt EXCEPT `NA` (0.9°) and
/// `NB` (1.8°). Confirmed by listing the live bucket (`LOT`/`TLX`), not
/// assumed from `LOWER_TILTS`/`UPPER_TILTS`'s pattern: unlike `DBZH`/
/// `VRADH`, Storm Relative Mean Radial Velocity genuinely skips two of the
/// six elevations rather than splitting cleanly in half.
const SRMV_TILTS: &[usize] = &[0, 2, 4, 5];

const TILT_LETTER_TABLE: &[(u8, &[usize])] = &[
    (b'B', ALL_TILTS),   // DBZH, 153
    (b'G', LOWER_TILTS), // VRADH, 154
    (b'U', UPPER_TILTS), // VRADH, 99
    (b'X', ALL_TILTS),   // ZDR, 159
    (b'C', ALL_TILTS),   // RHOHV, 161
    (b'K', ALL_TILTS),   // KDP, 163
    (b'S', SRMV_TILTS),  // SRMV, 56 — NOT ALL_TILTS, see SRMV_TILTS's doc
    (b'H', ALL_TILTS),   // HCLASS, 165
                         // Deliberately NOT here: `P` (ACCUM, codes 78/79/80). `N1P`/`N3P` share
                         // a prefix with real tilt ordinals (`N1`=1.3°, `N3`=3.1°) that mean
                         // something unrelated for this product family — the letter encodes
                         // accumulation PERIOD (1-hour/3-hour/storm-total), not elevation, and
                         // `NTP`'s prefix (`NT`) isn't a tilt prefix at all. Resolving `None` for
                         // all three is the correct answer, not a gap — adding `P` here would
                         // silently mislabel `N1P` as "1.3° tilt". Also NOT here: `W` (WRADH,
                         // legacy `NSW`), which has no tilt-shaped AWIPS id in the first place
                         // (single object, no per-tilt suffix at all).
];

/// Tilt ordinal (0-5) for a 3-character AWIPS product id (`"N0B"` -> `0`),
/// or `None` if it isn't one of [`TILT_LETTER_TABLE`]'s verified letters —
/// deliberately narrower than [`PRODUCTS`]; see the module doc.
pub(crate) fn tilt_letter_lookup(awips_product: &[u8]) -> Option<usize> {
    if awips_product.len() != 3 {
        return None;
    }
    let (prefix, letter) = awips_product.split_at(2);
    let &(_, tilts) = TILT_LETTER_TABLE.iter().find(|(l, _)| [*l] == letter)?;
    tilts
        .iter()
        .find(|&&tilt| TILT_PREFIXES[tilt].as_bytes() == prefix)
        .copied()
}

/// Fixed `(AWIPS id, tilt)` pairs for products whose id isn't
/// `{tilt prefix}{letter}`-shaped, so [`TILT_LETTER_TABLE`] cannot express
/// them at all — not "one more letter to add", a genuinely different id
/// shape. Today: `WRADH`'s legacy `NSW` (code 30), a single object with no
/// per-tilt AWIPS suffix. Its real elevation is 0.5° — confirmed by
/// decoding a live object, not assumed from the tilt-0 pattern other
/// legacy single-tilt products (`N0R`/`N0Z`/`N0V`) happen to share.
const SPECIAL_AWIPS_IDS: &[(&str, usize)] = &[("NSW", 0)];

/// [`tilt_letter_lookup`]'s counterpart for [`SPECIAL_AWIPS_IDS`] — checked
/// as a fallback in [`super::decode`] so `DecodedProduct::tilt` resolves
/// for `NSW` too, the same as any other verified product, rather than
/// silently staying `None` because its shape doesn't fit the generic table.
pub(crate) fn special_awips_id_lookup(awips_product: &[u8]) -> Option<usize> {
    let id = std::str::from_utf8(awips_product).ok()?;
    SPECIAL_AWIPS_IDS
        .iter()
        .find(|(known, _)| *known == id)
        .map(|(_, tilt)| *tilt)
}

/// Whether `message_code` is one this backend recognises at all (decodable
/// now, or deferred with a clear error) — derived from [`PRODUCTS`].
pub(crate) fn is_known_message_code(code: i16) -> bool {
    PRODUCTS.iter().any(|(c, _)| *c == code)
}

/// All known message codes, for error messages.
pub(crate) fn known_message_codes() -> Vec<u16> {
    let mut codes: Vec<u16> = PRODUCTS.iter().map(|(c, _)| *c as u16).collect();
    codes.sort_unstable();
    codes.dedup();
    codes
}

/// The full spec for a known message code.
pub(crate) fn spec_for(code: i16) -> Option<&'static ProductSpec> {
    PRODUCTS.iter().find(|(c, _)| *c == code).map(|(_, s)| s)
}

/// Decode scheme for a known message code. `None` if `code` isn't one of
/// [`PRODUCTS`]' codes.
pub(crate) fn decode_scheme_for(code: i16) -> Option<DecodeScheme> {
    spec_for(code).map(|s| s.decode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilt_letter_lookup_resolves_reflectivity_tilts() {
        assert_eq!(tilt_letter_lookup(b"N0B"), Some(0));
        assert_eq!(tilt_letter_lookup(b"NAB"), Some(1));
        assert_eq!(tilt_letter_lookup(b"N3B"), Some(5));
    }

    #[test]
    fn tilt_letter_lookup_splits_velocity_by_tilt() {
        assert_eq!(tilt_letter_lookup(b"N0G"), Some(0));
        assert_eq!(tilt_letter_lookup(b"N1G"), Some(2));
        // N0U / NBG don't exist — the split is (0,1,2)->G, (3,4,5)->U.
        assert_eq!(tilt_letter_lookup(b"N0U"), None);
        assert_eq!(tilt_letter_lookup(b"NBG"), None);
        assert_eq!(tilt_letter_lookup(b"NBU"), Some(3));
        assert_eq!(tilt_letter_lookup(b"N3U"), Some(5));
    }

    #[test]
    fn tilt_letter_lookup_resolves_dual_pol() {
        assert_eq!(tilt_letter_lookup(b"N0X"), Some(0));
        assert_eq!(tilt_letter_lookup(b"N0C"), Some(0));
        assert_eq!(tilt_letter_lookup(b"N0K"), Some(0));
    }

    #[test]
    fn tilt_letter_lookup_resolves_hydrometeor_classification() {
        // 165, all 6 tilts — confirmed by listing LOT/TLX.
        assert_eq!(tilt_letter_lookup(b"N0H"), Some(0));
        assert_eq!(tilt_letter_lookup(b"NAH"), Some(1));
        assert_eq!(tilt_letter_lookup(b"N1H"), Some(2));
        assert_eq!(tilt_letter_lookup(b"NBH"), Some(3));
        assert_eq!(tilt_letter_lookup(b"N2H"), Some(4));
        assert_eq!(tilt_letter_lookup(b"N3H"), Some(5));
    }

    #[test]
    fn tilt_letter_lookup_skips_two_tilts_for_storm_relative_velocity() {
        // 56 (SRMV) — confirmed present at N0/N1/N2/N3 only; NA (0.9°) and
        // NB (1.8°) genuinely do not exist in the bucket for this product,
        // unlike every other multi-tilt letter here.
        assert_eq!(tilt_letter_lookup(b"N0S"), Some(0));
        assert_eq!(tilt_letter_lookup(b"NAS"), None);
        assert_eq!(tilt_letter_lookup(b"N1S"), Some(2));
        assert_eq!(tilt_letter_lookup(b"NBS"), None);
        assert_eq!(tilt_letter_lookup(b"N2S"), Some(4));
        assert_eq!(tilt_letter_lookup(b"N3S"), Some(5));
    }

    #[test]
    fn tilt_letter_lookup_does_not_misresolve_accumulation_periods_as_tilts() {
        // P (ACCUM, 78/79/80) is deliberately absent from
        // TILT_LETTER_TABLE — see its own doc comment. Locking in the
        // negative result: N1P/N3P must NOT silently resolve to the tilt
        // ordinals their prefixes would otherwise mean (1.3deg/3.1deg).
        assert_eq!(tilt_letter_lookup(b"N1P"), None);
        assert_eq!(tilt_letter_lookup(b"N3P"), None);
        assert_eq!(tilt_letter_lookup(b"NTP"), None);
    }

    #[test]
    fn special_awips_id_lookup_resolves_spectrum_width() {
        assert_eq!(special_awips_id_lookup(b"NSW"), Some(0));
        assert_eq!(special_awips_id_lookup(b"N0B"), None); // not a special case
        assert_eq!(special_awips_id_lookup(b"XYZ"), None);
    }

    #[test]
    fn special_awips_id_lookup_does_not_panic_on_non_utf8_bytes() {
        // `[super::decode]` calls this with the raw AWIPS-id bytes straight
        // off the wire, unvalidated — a corrupt or truncated product header
        // is exactly the input this needs to survive without panicking.
        // `from_utf8().ok()?` is the whole guard; this pins it against a
        // regression (e.g. an `.unwrap()` creeping back in) rather than
        // trusting the `?` reads correctly by inspection.
        assert_eq!(special_awips_id_lookup(&[0xFF, 0xFE, 0xFD]), None);
        assert_eq!(special_awips_id_lookup(&[]), None);
    }

    #[test]
    fn tilt_letter_lookup_rejects_unverified_letters() {
        assert_eq!(tilt_letter_lookup(b"NSW"), None); // WRADH: not tilt-shaped at all — see special_awips_id_lookup instead
        assert_eq!(tilt_letter_lookup(b"XYZ"), None);
        assert_eq!(tilt_letter_lookup(b"N0"), None); // wrong length
    }

    #[test]
    fn tilt_letter_lookup_does_not_panic_on_non_utf8_bytes() {
        assert_eq!(tilt_letter_lookup(&[0xFF, 0xFE, 0xFD]), None);
    }

    #[test]
    fn special_awips_ids_and_tilt_letters_do_not_overlap() {
        // The two tables answer the same question (`awips id -> tilt`) for
        // disjoint SHAPES of id — [`SPECIAL_AWIPS_IDS`]'s own doc comment is
        // explicit that it exists only for ids `TILT_LETTER_TABLE` cannot
        // express. If a future entry's `awips_id` ever collided with one of
        // `TILT_LETTER_TABLE`'s single-letter suffixes, `mod.rs`'s
        // `tilt_letter_lookup(..).or_else(special_awips_id_lookup)` would
        // still resolve it — `tilt_letter_lookup` only matches a 3-byte
        // `{prefix}{letter}` shape, so a distinct multi-character id like
        // `NSW` cannot collide with it structurally. This test locks that
        // structural distinction in, rather than trusting the two lists
        // happen to stay disjoint by convention as more entries are added.
        for (id, _tilt) in SPECIAL_AWIPS_IDS {
            assert_eq!(
                tilt_letter_lookup(id.as_bytes()),
                None,
                "{id} is in both SPECIAL_AWIPS_IDS and resolves via \
                 TILT_LETTER_TABLE — pick one table per id"
            );
        }
    }

    #[test]
    fn known_message_codes_covers_all_26_products() {
        let codes = known_message_codes();
        assert_eq!(codes.len(), 26);
        assert!(codes.contains(&153));
        assert!(codes.contains(&19));
        assert!(codes.contains(&177));
    }

    #[test]
    fn decode_scheme_matches_xradar_table_for_a_sample() {
        assert_eq!(decode_scheme_for(153), Some(DecodeScheme::LinearHw));
        assert_eq!(decode_scheme_for(159), Some(DecodeScheme::FloatScale));
        assert_eq!(decode_scheme_for(19), Some(DecodeScheme::Legacy16));
        assert_eq!(decode_scheme_for(165), Some(DecodeScheme::ClassInt));
        assert_eq!(decode_scheme_for(170), Some(DecodeScheme::Precip));
        assert_eq!(decode_scheme_for(176), Some(DecodeScheme::Rate));
        assert_eq!(decode_scheme_for(9999), None);
    }

    #[test]
    fn packet_family_implemented_covers_every_product_table_entry() {
        // packet 16
        assert!(packet_family_implemented(153));
        assert!(packet_family_implemented(165));
        // packet AF1F
        assert!(packet_family_implemented(19));
        assert!(packet_family_implemented(78));
        // formerly deferred, closed by plan 0012: precip (packet 16),
        // rate (packet 28/XDR), HCLASS-177 (packet 16, not 28 — see
        // `DecodeScheme::ClassInt`'s doc)
        assert!(packet_family_implemented(170));
        assert!(packet_family_implemented(176));
        assert!(packet_family_implemented(177));
        // still the only `false` case: a code not in PRODUCTS at all
        assert!(!packet_family_implemented(9999));
    }

    // -- JSON export, for consumers outside this crate ----------------------
    //
    // radar-animation's free tier needs this table but cannot link Rust —
    // `scripts/generate_serverless_tables.py` reads the JSON this test
    // writes instead of a hand-maintained copy, which is exactly the drift
    // this crate's own module docs warn about elsewhere ("a hand-copy would
    // be correct on the day it was written and silently wrong afterwards").
    //
    // A `#[test]`, not a `pub` function or a `bin` target: every item this
    // needs (`PRODUCTS`, `TILT_LETTER_TABLE`, `SPECIAL_AWIPS_IDS`) is
    // `pub(crate)`, deliberately not part of the public API surface a wasm
    // or pyo3 caller could depend on — a test compiles inside the crate and
    // sees them without widening that surface. `#[ignore]` because writing
    // a file is a side effect a normal `cargo test` run should not have;
    // run explicitly with `cargo test --release -p radish
    // export_product_catalogue_json -- --ignored`.

    use std::io::Write;

    /// Mirrors [`DecodeScheme`], spelled with xradar's own string names
    /// (this module's own doc comment: "xradar #392's `ProductSpec.decode`
    /// string values exactly") — so a consumer never invents its own
    /// vocabulary for the same six schemes.
    fn decode_scheme_name(scheme: DecodeScheme) -> &'static str {
        match scheme {
            DecodeScheme::LinearHw => "linear_hw",
            DecodeScheme::FloatScale => "float_scale",
            DecodeScheme::Precip => "precip",
            DecodeScheme::Rate => "rate",
            DecodeScheme::Legacy16 => "legacy16",
            DecodeScheme::ClassInt => "class_int",
        }
    }

    #[test]
    #[ignore = "writes a file; run explicitly to regenerate the export"]
    fn export_product_catalogue_json() {
        // A SECOND gate beyond `#[ignore]`, matching the fixture-gated tests
        // in `backends/nexrad/decode/integration_test.rs`
        // (`RADISH_NEXRAD_FIXTURE_DIR` et al.): `#[ignore]` alone still runs
        // under `cargo test -- --ignored`, which some workflows use to sweep
        // every ignored test in one pass, and that sweep should not have the
        // side effect of overwriting `generated/` on a machine that never
        // asked for a regeneration. The env var makes the intent explicit
        // the same way the fixture-dir tests do.
        if std::env::var_os("RADISH_WRITE_PRODUCT_CATALOGUE").is_none() {
            eprintln!(
                "skipping: set RADISH_WRITE_PRODUCT_CATALOGUE=1 to regenerate \
                 generated/nexrad_level3_products.json"
            );
            return;
        }

        let products: Vec<serde_json::Value> = PRODUCTS
            .iter()
            .map(|(code, spec)| {
                serde_json::json!({
                    "message_code": code,
                    "moment": spec.moment,
                    "decode_scheme": decode_scheme_name(spec.decode),
                    "bin_size_m": spec.bin_size,
                    "has_elevation": spec.has_elevation,
                    "post_scale": spec.post_scale,
                    "packet_implemented": packet_family_implemented(*code),
                })
            })
            .collect();

        let tilt_letter_table: Vec<serde_json::Value> = TILT_LETTER_TABLE
            .iter()
            .map(|(letter, tilts)| {
                serde_json::json!({
                    "letter": (*letter as char).to_string(),
                    "tilts": tilts,
                })
            })
            .collect();

        let special_awips_ids: Vec<serde_json::Value> = SPECIAL_AWIPS_IDS
            .iter()
            .map(|(id, tilt)| serde_json::json!({ "awips_id": id, "tilt": tilt }))
            .collect();

        let out = serde_json::json!({
            "schema_version": 1,
            "generator": "radish products.rs export_product_catalogue_json",
            "package_version": env!("CARGO_PKG_VERSION"),
            "tilt_prefixes": TILT_PREFIXES,
            "products": products,
            "tilt_letter_table": tilt_letter_table,
            "special_awips_ids": special_awips_ids,
        });

        // Repo-relative, not CARGO_MANIFEST_DIR-relative: this crate lives
        // at `radish/radish`, and the export belongs at the repo root
        // alongside `docs/`, where a sibling-repo consumer expects to find
        // generated artifacts, not one crate-directory level down.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("generated")
            .join("nexrad_level3_products.json");
        std::fs::create_dir_all(path.parent().unwrap()).expect("create generated/ dir");
        let mut file = std::fs::File::create(&path).expect("create export file");
        file.write_all(serde_json::to_string_pretty(&out).unwrap().as_bytes())
            .expect("write export file");

        // Round-trip check in the same test, so a malformed write fails
        // here rather than surfacing as a confusing parse error in the
        // Python consumer.
        let reread = std::fs::read_to_string(&path).expect("re-read export file");
        let parsed: serde_json::Value =
            serde_json::from_str(&reread).expect("export file is valid JSON");
        assert_eq!(parsed["products"].as_array().unwrap().len(), PRODUCTS.len());
    }
}
