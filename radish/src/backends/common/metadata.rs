//! Single source of truth for per-ODIM-moment CF metadata strings.
//!
//! Backends differ in how they identify a moment in the source format
//! (NEXRAD's `Product::Reflectivity`, IRIS's `DB_DBZ` byte, ODIM's
//! `/dataset1/data1` group, etc.), but the **target** is the same ODIM
//! short name and the same CF metadata: `units`, `standard_name`,
//! `long_name`. Putting the metadata table here lets each backend declare
//! only the source→ODIM mapping; the per-DataArray xarray attrs come from
//! a single canonical lookup so engine-swap users see byte-identical
//! values regardless of which backend produced the tree.
//!
//! Strings are chosen to match xradar's per-format readers verbatim. Keep
//! this in sync with:
//!
//! * `xradar/io/backends/nexrad_level2.py::nexrad_mapping`
//! * `xradar/io/backends/iris.py::iris_mapping`
//! * `xradar/io/backends/odim_h5.py::odim_mapping` (when we add it)
//!
//! When backends disagree on the canonical string for a given moment, the
//! one that's closest to CfRadial2 wins — we accept divergence with a
//! given xradar reader rather than divergence with the standard.

/// Canonical CF metadata for one ODIM moment. The `odim_name` field is the
/// key (radish-public short name), the others are pure attribute strings
/// emitted on the resulting `xarray.DataArray`.
pub(crate) struct OdimMomentMeta {
    /// ODIM short name — the key consumed by callers and the variable name
    /// the moment surfaces as in the resulting xarray DataTree (e.g.
    /// `"DBZH"`, `"VRADH"`).
    pub(crate) odim_name: &'static str,
    /// Unit string emitted as the `units` attribute on the moment's
    /// `xarray.DataArray`. Mirrors xradar's per-format reader exactly,
    /// including idiosyncrasies like the trailing-`s` `"meters per seconds"`
    /// for velocity moments — engine-swap parity depends on byte-for-byte
    /// equality here.
    pub(crate) units: &'static str,
    /// CF `standard_name` attribute. Conventionally lower-snake-case and
    /// matched against the CF standard-name table where applicable.
    pub(crate) standard_name: &'static str,
    /// Human-readable description emitted as the `long_name` attribute.
    /// Mixed-case prose; not a CF-controlled vocabulary.
    pub(crate) long_name: &'static str,
}

/// All ODIM moments radish backends currently emit. Extend this table when
/// adding a new moment that any backend needs.
const TABLE: &[OdimMomentMeta] = &[
    OdimMomentMeta {
        odim_name: "DBZH",
        units: "dBZ",
        standard_name: "radar_equivalent_reflectivity_factor_h",
        long_name: "Equivalent reflectivity factor H",
    },
    OdimMomentMeta {
        odim_name: "DBTH",
        units: "dBZ",
        standard_name: "radar_equivalent_reflectivity_factor_h",
        long_name: "Total power H",
    },
    OdimMomentMeta {
        odim_name: "VRADH",
        // xradar uses the trailing-s "meters per seconds"; we mirror verbatim.
        units: "meters per seconds",
        standard_name: "radial_velocity_of_scatterers_away_from_instrument_h",
        long_name: "Radial velocity of scatterers away from instrument H",
    },
    OdimMomentMeta {
        odim_name: "WRADH",
        units: "meters per seconds",
        standard_name: "radar_doppler_spectrum_width_h",
        long_name: "Doppler spectrum width H",
    },
    OdimMomentMeta {
        odim_name: "ZDR",
        units: "dB",
        standard_name: "radar_differential_reflectivity_hv",
        long_name: "Log differential reflectivity H/V",
    },
    OdimMomentMeta {
        odim_name: "PHIDP",
        units: "degrees",
        standard_name: "radar_differential_phase_hv",
        long_name: "Differential phase HV",
    },
    OdimMomentMeta {
        odim_name: "RHOHV",
        units: "unitless",
        standard_name: "radar_correlation_coefficient_hv",
        long_name: "Correlation coefficient HV",
    },
    OdimMomentMeta {
        odim_name: "CCORH",
        // CCORH (clutter filter power) isn't in CfRadial2; xradar surfaces
        // it from NEXRAD with these strings, so we pin them here.
        units: "unitless",
        standard_name: "clutter_correction_h",
        long_name: "Clutter Correction H",
    },
    OdimMomentMeta {
        odim_name: "KDP",
        units: "degrees per kilometer",
        standard_name: "radar_specific_differential_phase_hv",
        long_name: "Specific differential phase HV",
    },
    OdimMomentMeta {
        odim_name: "SQIH",
        units: "unitless",
        standard_name: "radar_signal_quality_index_h",
        long_name: "Signal Quality Index H",
    },
    OdimMomentMeta {
        odim_name: "SNRH",
        units: "dB",
        standard_name: "radar_signal_to_noise_ratio_h",
        long_name: "Signal to noise ratio H",
    },
];

/// Look up CF metadata for an ODIM short name. Returns `None` if the moment
/// is unknown — callers can fall back to a synthesised default but this is
/// a sign the table needs an entry.
pub(crate) fn meta_for(odim_name: &str) -> Option<&'static OdimMomentMeta> {
    TABLE.iter().find(|m| m.odim_name == odim_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_for_dbzh_returns_xradar_strings() {
        let m = meta_for("DBZH").expect("DBZH is in the table");
        assert_eq!(m.units, "dBZ");
        assert_eq!(m.standard_name, "radar_equivalent_reflectivity_factor_h");
        assert_eq!(m.long_name, "Equivalent reflectivity factor H");
    }

    #[test]
    fn velocity_units_mirror_xradar_trailing_s() {
        // xradar emits the literal "meters per seconds" (with trailing s);
        // any drift here is a parity failure.
        assert_eq!(meta_for("VRADH").unwrap().units, "meters per seconds");
        assert_eq!(meta_for("WRADH").unwrap().units, "meters per seconds");
    }

    #[test]
    fn rho_and_ccorh_emit_literal_unitless() {
        // Unit-free moments use the literal "unitless", not the empty string.
        assert_eq!(meta_for("RHOHV").unwrap().units, "unitless");
        assert_eq!(meta_for("CCORH").unwrap().units, "unitless");
    }

    #[test]
    fn unknown_odim_returns_none() {
        assert!(meta_for("DEFINITELY_NOT_AN_ODIM_NAME").is_none());
    }

    #[test]
    fn ccorh_uses_clutter_correction_strings() {
        let m = meta_for("CCORH").unwrap();
        assert_eq!(m.standard_name, "clutter_correction_h");
        assert_eq!(m.long_name, "Clutter Correction H");
    }
}
