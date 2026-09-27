//! Mapping from NEXRAD `Product`s to radish/ODIM moment names and CF metadata.
//!
//! This module owns only the **format-specific** half — translating a
//! NEXRAD `Product` enum into a canonical ODIM short name. The CF strings
//! (`units`, `standard_name`, `long_name`) come from
//! [`crate::backends::common::metadata`], which is the single source of
//! truth shared with future backends so an `xr.DataTree` produced by any
//! backend has byte-identical per-DataArray attribute strings.

use radish_types::moments;

use crate::backends::common::{meta_for, OdimMomentMeta};
use crate::backends::nexrad::decode::products::Product;

/// `CCORH` isn't in `radish_types::moments` (which mirrors the CfRadial2 short
/// names, where this product is undefined). Keep it as a local constant for the
/// NEXRAD-specific `Product::ClutterFilterPower` mapping.
const CCORH: &str = "CCORH";

/// Map a NEXRAD `Product` to its ODIM moment name. The result feeds into
/// [`crate::backends::common::metadata::meta_for`] for the CF strings, so
/// adding a new product means adding one match arm here plus (if needed)
/// a row in the central metadata table.
pub(super) fn product_to_odim_name(product: Product) -> &'static str {
    match product {
        Product::Reflectivity => moments::DBZH,
        Product::Velocity => moments::VRADH,
        Product::SpectrumWidth => moments::WRADH,
        Product::DifferentialReflectivity => moments::ZDR,
        Product::DifferentialPhase => moments::PHIDP,
        Product::CorrelationCoefficient => moments::RHOHV,
        Product::ClutterFilterPower => CCORH,
    }
}

/// Map a NEXRAD `Product` to its full ODIM moment metadata (name + CF
/// strings). Adapter callers reach for this directly. Panics if the
/// metadata table doesn't have an entry for the resolved ODIM name —
/// that's a programming error, caught by the
/// `every_supported_product_has_metadata` test below.
pub(super) fn moment_meta(product: Product) -> &'static OdimMomentMeta {
    let odim = product_to_odim_name(product);
    meta_for(odim).unwrap_or_else(|| {
        panic!(
            "BUG: ODIM moment {odim:?} for NEXRAD product {product:?} \
             has no entry in backends::common::metadata::TABLE"
        )
    })
}

/// Products radish surfaces from a NEXRAD sweep, in a stable order so DataTree
/// variable order is deterministic across runs. The full set matches xradar's
/// NEXRAD reader exactly, including `ClutterFilterPower` (CCORH).
pub(super) const SUPPORTED_PRODUCTS: &[Product] = &[
    Product::Reflectivity,
    Product::Velocity,
    Product::SpectrumWidth,
    Product::DifferentialReflectivity,
    Product::DifferentialPhase,
    Product::CorrelationCoefficient,
    Product::ClutterFilterPower,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflectivity_maps_to_dbzh_with_xradar_strings() {
        let m = moment_meta(Product::Reflectivity);
        assert_eq!(m.odim_name, "DBZH");
        assert_eq!(m.units, "dBZ");
        assert_eq!(m.standard_name, "radar_equivalent_reflectivity_factor_h");
        assert_eq!(m.long_name, "Equivalent reflectivity factor H");
    }

    #[test]
    fn velocity_uses_xradar_units_string() {
        assert_eq!(moment_meta(Product::Velocity).units, "meters per seconds");
        assert_eq!(
            moment_meta(Product::SpectrumWidth).units,
            "meters per seconds"
        );
    }

    #[test]
    fn rho_uses_unitless_not_empty_string() {
        assert_eq!(
            moment_meta(Product::CorrelationCoefficient).units,
            "unitless"
        );
        assert_eq!(moment_meta(Product::ClutterFilterPower).units, "unitless");
    }

    #[test]
    fn ccorh_present_in_supported_products() {
        assert!(SUPPORTED_PRODUCTS.contains(&Product::ClutterFilterPower));
    }

    #[test]
    fn names_match_radish_types_constants() {
        assert_eq!(moment_meta(Product::Reflectivity).odim_name, moments::DBZH);
        assert_eq!(moment_meta(Product::Velocity).odim_name, moments::VRADH);
        assert_eq!(
            moment_meta(Product::CorrelationCoefficient).odim_name,
            moments::RHOHV
        );
    }

    /// Guardrail: `moment_meta` panics if the central metadata table is
    /// missing a row for any supported NEXRAD product. Catches the case
    /// where a future contributor adds a new `Product` arm to
    /// `product_to_odim_name` but forgets to extend
    /// `common::metadata::TABLE`.
    #[test]
    fn every_supported_product_has_metadata() {
        for &p in SUPPORTED_PRODUCTS {
            let m = moment_meta(p); // would panic on a missing entry
            assert!(!m.units.is_empty(), "{p:?} has empty units");
        }
    }
}
