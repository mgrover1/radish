//! Moment (radar variable) data structures.

use ndarray::Array2;

/// A product's declared value scaling, verbatim from its source encoding —
/// e.g. a NEXRAD Level 3 (NIDS) product description block.
///
/// Exists because some consumers need the **codes**, not the physical
/// values: NEXRAD Level 3 defines its wire format directly in terms of a
/// product's own declared scale (see `docs/NEXRAD_LEVEL3_WASM.md` §4.1) —
/// re-deriving a scale from [`MomentData::data`] and re-quantizing would
/// both waste work and risk a rounding disagreement with the source file.
/// [`MomentData::raw_codes`] carries the verbatim `u8` codes; this struct is
/// what a caller needs to turn a code back into the same physical value the
/// source file declares, without assuming any particular quantization.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeclaredScale {
    /// Physical value at [`data_floor_code`](Self::data_floor_code).
    pub value_min: f32,
    /// Physical value change per code above the floor.
    pub value_increment: f32,
    /// Number of codes that carry data (from the floor code through 255,
    /// inclusive) — NEXRAD Level 3's digital radial products declare or
    /// imply 254.
    pub n_levels: u16,
    /// The lowest code that carries data. Codes below this are sentinels
    /// (e.g. NEXRAD Level 3: 0 = below threshold, 1 = range folded) —
    /// never `value_min + (code - data_floor_code) * value_increment`.
    pub data_floor_code: u8,
}

impl DeclaredScale {
    /// Physical value for one code, or `None` below [`data_floor_code`](Self::data_floor_code)
    /// (a sentinel, not data). For a [`MomentData::raw_codes_u16`] code,
    /// use [`decode_code`](Self::decode_code) instead — a `u16` doesn't
    /// fit this method's `u8` parameter.
    pub fn decode(&self, code: u8) -> Option<f32> {
        self.decode_code(code)
    }

    /// Generic counterpart to [`decode`](Self::decode) — same formula, any
    /// integer width `Into<u32>` (`u8` for [`MomentData::raw_codes`],
    /// `u16` for [`MomentData::raw_codes_u16`]). Added so a packet-28
    /// consumer (`RATE`/code 176 today) has a real method to call instead
    /// of hand-rolling the shifted-linear formula itself — this is the
    /// same generalization `nexrad_level3::decode`'s internal
    /// `precip_family_declared_scale` already applies to its OWN copy of
    /// this formula, now shared with the one external consumers actually
    /// call.
    pub fn decode_code<T>(&self, code: T) -> Option<f32>
    where
        T: Copy + Into<u32>,
    {
        let code: u32 = code.into();
        let floor = self.data_floor_code as u32;
        if code < floor {
            return None;
        }
        Some(self.value_min + (code - floor) as f32 * self.value_increment)
    }
}

/// Radar moment data (e.g., reflectivity, velocity)
#[derive(Debug, Clone)]
pub struct MomentData {
    /// Variable name (e.g., "DBZH", "VRADH")
    pub name: String,

    /// CF standard name
    pub standard_name: Option<String>,

    /// Long descriptive name
    pub long_name: Option<String>,

    /// Units
    pub units: String,

    /// 2D data array [rays × gates]
    pub data: Array2<f32>,

    /// Fill value (missing data indicator)
    pub fill_value: Option<f32>,

    /// Scale factor
    pub scale_factor: Option<f32>,

    /// Add offset
    pub add_offset: Option<f32>,

    /// Valid minimum
    pub valid_min: Option<f32>,

    /// Valid maximum
    pub valid_max: Option<f32>,

    /// Coordinates this variable depends on
    pub coordinates: Option<String>,

    /// Additional attributes
    pub attributes: std::collections::HashMap<String, String>,

    /// Verbatim on-wire codes, when the source format defines its wire
    /// format in terms of codes rather than physical values (currently:
    /// NEXRAD Level 3 / NIDS digital radial products). `None` for every
    /// other backend — this is deliberately *additional* to
    /// [`data`](Self::data), not a replacement: `data` stays the physical
    /// values every other radish consumer expects, and `raw_codes` +
    /// [`declared_scale`](Self::declared_scale) are there for a consumer
    /// that needs the source's own quantization untouched (see
    /// `docs/NEXRAD_LEVEL3_WASM.md` §4.1 — "the codes pass through
    /// untouched, that is the whole design"). Same shape as `data`
    /// (`[rays × gates]`) when present.
    pub raw_codes: Option<Array2<u8>>,

    /// Verbatim on-wire codes for a source format whose raw levels don't
    /// fit in a `u8` — currently: NEXRAD Level 3's symbology packet 28
    /// (XDR, generic data packet — `RATE`/code 176, and any future
    /// packet-28 product). Added *alongside* [`raw_codes`](Self::raw_codes)
    /// rather than widening it, for the same additive reasoning that
    /// field's own doc comment gives: exactly one of `raw_codes` /
    /// `raw_codes_u16` is `Some` for a given moment (never both, never
    /// neither, for any backend that populates either) — see plan 0012 §0/
    /// §3.2 for why a wasm/pyo3 consumer that only ever decoded packet
    /// 16/AF1F products keeps working unmodified against this field's
    /// addition. `None` for every backend/product that doesn't need the
    /// wider width. Same shape as `data` (`[rays × gates]`) when present.
    pub raw_codes_u16: Option<Array2<u16>>,

    /// The scale [`raw_codes`](Self::raw_codes)/[`raw_codes_u16`](Self::raw_codes_u16)
    /// is declared on, verbatim from the source file. `None` whenever both
    /// raw-code fields are `None`, and also `None` for a categorical
    /// product (e.g. hydrometeor classification) whose codes index a fixed
    /// label table rather than a linear physical scale.
    ///
    /// [`DeclaredScale::decode`] takes a `u8` code — for a
    /// [`raw_codes_u16`](Self::raw_codes_u16) product, call
    /// [`DeclaredScale::decode_code`] instead (a `u16` code can exceed
    /// what `decode`'s `u8` parameter can even represent).
    pub declared_scale: Option<DeclaredScale>,
}

impl MomentData {
    /// Create a new MomentData
    pub fn new(name: String, units: String, data: Array2<f32>) -> Self {
        Self {
            name,
            standard_name: None,
            long_name: None,
            units,
            data,
            fill_value: None,
            scale_factor: None,
            add_offset: None,
            valid_min: None,
            valid_max: None,
            coordinates: None,
            attributes: std::collections::HashMap::new(),
            raw_codes: None,
            raw_codes_u16: None,
            declared_scale: None,
        }
    }

    /// Get the shape of the data array
    pub fn shape(&self) -> (usize, usize) {
        let shape = self.data.shape();
        (shape[0], shape[1])
    }

    /// Apply scale and offset to get physical values
    pub fn apply_scale_offset(&mut self) {
        if let (Some(scale), Some(offset)) = (self.scale_factor, self.add_offset) {
            self.data.mapv_inplace(|v| {
                if let Some(fill) = self.fill_value {
                    if v == fill {
                        return v;
                    }
                }
                v * scale + offset
            });
            self.scale_factor = None;
            self.add_offset = None;
        }
    }

    /// Mask invalid values
    pub fn mask_invalid(&mut self, mask_value: f32) {
        if let Some(fill) = self.fill_value {
            self.data
                .mapv_inplace(|v| if v == fill { mask_value } else { v });
        }

        if let (Some(min), Some(max)) = (self.valid_min, self.valid_max) {
            self.data
                .mapv_inplace(|v| if v < min || v > max { mask_value } else { v });
        }
    }
}

// `MomentMetadata` and its `from_name` table used to live here. They were
// never wired into any backend and are now superseded by the per-backend
// metadata sources of truth (e.g. `radish::backends::nexrad::mapping`, which
// uses the `radish_types::moments` constants directly). Re-introduce only
// when a generalised name→metadata lookup actually has callers.

#[cfg(test)]
mod tests {
    use super::*;

    fn scale() -> DeclaredScale {
        DeclaredScale {
            value_min: -32.0,
            value_increment: 0.5,
            n_levels: 254,
            data_floor_code: 2,
        }
    }

    #[test]
    fn decode_returns_none_below_the_floor() {
        assert_eq!(scale().decode(0), None);
        assert_eq!(scale().decode(1), None);
    }

    #[test]
    fn decode_returns_the_physical_value_at_and_above_the_floor() {
        assert_eq!(scale().decode(2), Some(-32.0));
        assert_eq!(scale().decode(3), Some(-31.5));
    }

    #[test]
    fn decode_code_agrees_with_decode_for_u8() {
        // decode() now just delegates to decode_code() — pin that they
        // never silently diverge (e.g. a future edit to one formula
        // without the other).
        for code in 0u8..=255 {
            assert_eq!(scale().decode(code), scale().decode_code(code));
        }
    }

    #[test]
    fn decode_code_works_for_u16_beyond_u8_range() {
        // The whole reason decode_code exists: a `raw_codes_u16` code can
        // exceed 255, which decode()'s `u8` parameter can't even represent.
        let s = scale();
        assert_eq!(s.decode_code(2u16), Some(-32.0));
        assert_eq!(s.decode_code(1000u16), Some(-32.0 + 998.0 * 0.5));
    }

    #[test]
    fn decode_code_returns_none_below_the_floor_for_u16() {
        assert_eq!(scale().decode_code(0u16), None);
        assert_eq!(scale().decode_code(1u16), None);
    }
}
