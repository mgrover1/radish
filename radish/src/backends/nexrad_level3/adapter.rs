//! Convert a [`super::decode::DecodedProduct`] into radish's `VolumeData`.
//!
//! NIDS products are single-tilt, single-moment: the output volume has
//! exactly one sweep, and that sweep has exactly one moment. Populates
//! BOTH the normal physical-value pipeline (`MomentData::data`, always)
//! and the raw-code pass-through radish's browser tier needs
//! (`raw_codes` + `declared_scale`, only for the linear-coded schemes
//! that have a simple declared scale) — see `docs/NEXRAD_LEVEL3_WASM.md`
//! §4.1: "the codes pass through untouched, that is the whole design."

use std::collections::HashMap;

use radish_types::{PlatformType, SweepMode};

use crate::backends::common::meta_for;
use crate::model::{DeclaredScale, NidsSweepAttrs};
use crate::{Coordinates, MomentData, SweepData, SweepMetadata, VolumeData, VolumeMetadata};

use super::decode::{DecodedProduct, RawCodes};

/// Build the single-sweep, single-moment [`VolumeData`] a decoded NIDS
/// product normalizes to.
pub(super) fn convert(product: DecodedProduct) -> VolumeData {
    let (n_radials, n_bins) = product.codes.dim();

    let meta = meta_for(product.moment);
    let mut moment = MomentData::new(
        product.moment.to_string(),
        meta.map(|m| m.units).unwrap_or_default().to_string(),
        product.physical,
    );
    moment.standard_name = meta.map(|m| m.standard_name.to_string());
    moment.long_name = meta.map(|m| m.long_name.to_string());
    moment.declared_scale = product.declared_scale.map(|s| DeclaredScale {
        value_min: s.value_min,
        value_increment: s.value_increment,
        n_levels: s.n_levels,
        data_floor_code: s.data_floor_code,
    });
    // Exactly one of these is populated, per `RawCodes`'s own doc — packet
    // 16/AF1F keeps using `raw_codes` exactly as every consumer already
    // expects; only the packet-28 path (`RATE`/176) populates the new
    // `raw_codes_u16` field instead. See plan 0012 §3.2.
    match product.codes {
        RawCodes::U8(codes) => moment.raw_codes = Some(codes),
        RawCodes::U16(codes) => moment.raw_codes_u16 = Some(codes),
    }

    let mut moments = HashMap::with_capacity(1);
    moments.insert(product.moment.to_string(), moment);

    let range: Vec<f32> = (0..n_bins)
        .map(|i| product.first_gate_m + i as f32 * product.gate_spacing_m)
        .collect();
    let azimuth: Vec<f32> = product.azimuths.iter().map(|&a| a as f32).collect();
    let elevation: Vec<f32> = vec![product.elevation_deg as f32; n_radials];
    let time: Vec<f64> = vec![product.scan_time.timestamp() as f64; n_radials];
    let coordinates = Coordinates::new(time, range, azimuth, elevation);

    // Sweep number is always 0 — a NIDS file is its own single-sweep
    // volume, not one cut of a larger VCP radish reconstructs. The
    // canonical tilt ordinal (when resolvable) lives on `NidsSweepAttrs`
    // instead; conflating the two was a Phase 2 shortcut this corrects.
    let mut sweep_metadata = SweepMetadata::new(0, SweepMode::Azimuth, product.elevation_deg);
    sweep_metadata.nids = Some(NidsSweepAttrs {
        site: product.site.clone(),
        awips_id: product.awips_id,
        message_code: product.message_code as u16,
        vcp: product.vcp,
        tilt: product.tilt.map(|t| t as u8),
    });
    let sweep = SweepData::new(sweep_metadata, moments, coordinates);

    let mut volume_metadata = VolumeMetadata::new(
        // NIDS carries only the 3-letter Level 3 site id (e.g. "LOT"), not
        // the 4-letter ICAO ("KLOT") — reconstructing the ICAO prefix isn't
        // uniform across regions (K/P/T vary by location) and isn't in the
        // file, so this deliberately doesn't guess.
        product.site.clone(),
        product.lat,
        product.lon,
        product.height_m,
        product.scan_time,
        product.scan_time,
    );
    volume_metadata.platform_type = Some(PlatformType::Fixed);
    volume_metadata.site_name = Some(product.site);
    volume_metadata.sweep_fixed_angles = vec![product.elevation_deg];
    volume_metadata.generate_sweep_names(1);

    VolumeData::new(volume_metadata, vec![sweep])
}
