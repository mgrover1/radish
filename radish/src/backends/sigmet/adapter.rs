//! Convert a [`super::decode::DecodedVolume`] into radish's `VolumeData`.
//!
//! The shape mirrors the NEXRAD adapter: parallel sweep conversion via
//! `rayon`, sort rays by azimuth (PPI) or elevation (RHI) once, reuse the
//! permutation for every coord axis and every moment buffer. All the
//! buffer-management logic lives in `backends::common::*` so this file
//! is mostly format-specific glue.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use ndarray::Array2;
use radish_types::SweepMode;

use crate::backends::common::{
    assemble_ppi_coordinates, decode_into_array, par_enumerate_map, sort_indices_by_key,
};
use crate::{
    MomentData, RadishError, Result, SigmetSweepAttrs, SigmetVolumeAttrs, SweepData, SweepMetadata,
    VolumeData, VolumeMetadata,
};

use super::decode::{DecodedRay, DecodedSweep, DecodedVolume};
use super::mapping::{cf_metadata_for, moment_for_id, MomentMapping, SUPPORTED_MOMENTS};
use super::structs::ScanMode;

pub(super) fn convert_volume(decoded: DecodedVolume, source: &Path) -> Result<VolumeData> {
    let metadata = build_volume_metadata(&decoded, source)?;
    // One pass over every ray collects the set of data_type_ids actually
    // present in the volume; then filter SUPPORTED_MOMENTS by that set
    // to preserve the table's stable emission order. The previous
    // approach was triple-nested (`for each known id { for each ray
    // { contains_key }}`), O(N_ids × N_sweeps × N_rays); this is
    // O(N_rays + N_ids).
    let mut seen: std::collections::HashSet<u8> = std::collections::HashSet::new();
    for sweep in &decoded.sweeps {
        for ray in &sweep.rays {
            for &id in ray.moments.keys() {
                seen.insert(id);
            }
        }
    }
    let active_ids: Vec<u8> = SUPPORTED_MOMENTS
        .iter()
        .map(|m| m.data_type_id)
        .filter(|id| seen.contains(id))
        .collect();

    let sweeps: Vec<SweepData> = par_enumerate_map(&decoded.sweeps, |idx, sweep| {
        convert_sweep(
            sweep,
            idx,
            decoded.scan_mode,
            &decoded.range_axis_m,
            &active_ids,
        )
    })?;

    Ok(VolumeData::new(metadata, sweeps))
}

pub(super) fn build_volume_metadata(
    decoded: &DecodedVolume,
    source: &Path,
) -> Result<VolumeMetadata> {
    let instrument_name = if decoded.site_name.is_empty() {
        source
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("UNKN")
            .to_string()
    } else {
        decoded.site_name.clone()
    };

    let mut metadata = VolumeMetadata::new(
        instrument_name,
        decoded.latitude_deg,
        decoded.longitude_deg,
        decoded.altitude_m,
        decoded.volume_start_time,
        decoded
            .sweeps
            .last()
            .map(|s| s.start_time)
            .unwrap_or(decoded.volume_start_time),
    );
    metadata.institution = "Vaisala / SIGMET".to_string();
    metadata.platform_type = Some(radish_types::PlatformType::Fixed);
    metadata.generate_sweep_names(decoded.sweeps.len());
    // Read the per-sweep INGEST_DATA_HEADER.fixed_angle (BIN2-encoded
    // achieved beam angle). Differs from xradar's value by sub-
    // milli-degree quantization noise (~0.002°): xradar reads the
    // commanded angle from a mode-dependent block in TASK_SCAN_INFO
    // (TASK_PPI_SCAN_INFO for PPI vs TASK_RHI_SCAN_INFO for RHI),
    // which we don't fully wire through yet. The diff is below the
    // antenna's pointing accuracy and well below any meaningful VCP
    // step — tracked as a follow-up.
    metadata.sweep_fixed_angles = decoded
        .sweeps
        .iter()
        .map(|s| s.fixed_angle_deg as f64)
        .collect();
    metadata
        .attributes
        .insert("scan_name".to_string(), decoded.task_name.clone());
    metadata.attributes.insert(
        "scan_mode".to_string(),
        decoded.scan_mode.label().to_string(),
    );
    metadata
        .attributes
        .insert("iris_version".to_string(), decoded.iris_version.clone());
    metadata.sigmet = Some(SigmetVolumeAttrs {
        task_name: decoded.task_name.clone(),
        iris_version: decoded.iris_version.clone(),
        prf_hz: decoded.prf_hz,
        prf_low_hz: 0.0,
        nyquist_velocity_ms: decoded.nyquist_velocity_ms,
        unambiguous_range_m: decoded.unambiguous_range_m,
        scan_mode: decoded.scan_mode.label().to_string(),
    });
    Ok(metadata)
}

pub(super) fn convert_sweep_at(decoded: &DecodedVolume, idx: usize) -> Option<Result<SweepData>> {
    let sweep = decoded.sweeps.get(idx)?;
    let active_ids: Vec<u8> = SUPPORTED_MOMENTS
        .iter()
        .map(|m| m.data_type_id)
        .filter(|id| sweep.rays.iter().any(|r| r.moments.contains_key(id)))
        .collect();
    Some(convert_sweep(
        sweep,
        idx,
        decoded.scan_mode,
        &decoded.range_axis_m,
        &active_ids,
    ))
}

fn convert_sweep(
    sweep: &DecodedSweep,
    sweep_idx: usize,
    scan_mode: ScanMode,
    range_axis_m: &[f32],
    active_ids: &[u8],
) -> Result<SweepData> {
    let rays = &sweep.rays;
    if rays.is_empty() {
        return Err(RadishError::MalformedRecord {
            offset: 0,
            msg: format!("sweep {sweep_idx} has no rays"),
        });
    }

    // Sort rays by primary angle: azimuth for PPI (and Other/unknown,
    // safe fallback), elevation for RHI.
    let order = match scan_mode {
        ScanMode::Rhi => sort_indices_by_key(rays, |r| r.elevation_deg),
        _ => sort_indices_by_key(rays, |r| r.azimuth_deg),
    };

    // `assemble_ppi_coordinates` expects absolute Unix seconds, but the
    // RAY_HEADER only carries `time_offset_s` (seconds within the sweep).
    // Anchor it to the sweep's absolute start time so the `time` coordinate
    // is the real acquisition instant rather than a 1970-epoch offset.
    let base_secs = sweep.start_time.timestamp() as f64
        + sweep.start_time.timestamp_subsec_micros() as f64 / 1e6;

    let coordinates = assemble_ppi_coordinates(
        rays,
        &order,
        range_axis_m.to_vec(),
        |r: &DecodedRay| r.azimuth_deg,
        |r: &DecodedRay| r.elevation_deg,
        |r: &DecodedRay| base_secs + r.time_offset_s as f64,
    );

    let nrays = rays.len();
    let max_gates = range_axis_m.len();
    let mut moments: HashMap<String, MomentData> = HashMap::with_capacity(active_ids.len());
    // Track which output variable names we've already emitted so 8-bit
    // and 16-bit variants of the same moment (e.g. DB_DBZ + DB_DBZ2 →
    // DBZH) don't both produce a DataArray. `&'static str` keys avoid
    // the per-insert `String` allocation the previous `HashMap<String,
    // ()>` paid.
    let mut emitted: HashSet<&'static str> = HashSet::with_capacity(active_ids.len());
    for &data_type_id in active_ids {
        let Some(m) = moment_for_id(data_type_id) else {
            continue;
        };
        if !emitted.insert(m.output_name) {
            continue;
        }

        let arr = build_moment_array(rays, &order, data_type_id, nrays, max_gates)?;

        // Two paths: `MomentMapping::Odim` types pull full CF metadata
        // from `common::metadata::TABLE`; `MomentMapping::Iris` types
        // (DB_HCLASS, DB_DBTE8, DB_DBZE8) emit only the inline `units`
        // string. The exhaustive match keeps the two paths
        // type-distinct so a typo in `output_name` can't silently
        // route an ODIM moment through the no-metadata path.
        let key = m.output_name.to_string();
        let mut moment = match m.mapping {
            MomentMapping::Odim => {
                let meta = cf_metadata_for(m).ok_or_else(|| {
                    RadishError::Conversion(format!(
                        "no CF metadata for ODIM moment {:?} (IRIS {})",
                        m.output_name, m.iris_name
                    ))
                })?;
                let mut x = MomentData::new(key.clone(), meta.units.to_string(), arr);
                x.standard_name = Some(meta.standard_name.to_string());
                x.long_name = Some(meta.long_name.to_string());
                x
            }
            MomentMapping::Iris { units } => MomentData::new(key.clone(), units.to_string(), arr),
        };
        moment.fill_value = Some(f32::NAN);
        moment.scale_factor = Some(1.0);
        moment.add_offset = Some(0.0);
        moments.insert(key, moment);
    }

    let mut meta = SweepMetadata::new(
        sweep.sweep_number,
        match scan_mode {
            ScanMode::Rhi => SweepMode::Elevation,
            _ => SweepMode::Azimuth,
        },
        sweep.fixed_angle_deg as f64,
    );
    meta.follow_mode = None;
    meta.prt_mode = None;
    meta.sigmet = Some(SigmetSweepAttrs {
        sweep_mode: match scan_mode {
            ScanMode::Rhi => "rhi".to_string(),
            _ => "azimuth_surveillance".to_string(),
        },
        fixed_angle_deg: sweep.fixed_angle_deg,
    });
    Ok(SweepData::new(meta, moments, coordinates))
}

/// Build the `Array2<f32>` for one moment by walking rays in `order` and
/// copying their pre-decoded gates into the per-row slice.
fn build_moment_array(
    rays: &[DecodedRay],
    order: &[usize],
    data_type_id: u8,
    nrays: usize,
    max_gates: usize,
) -> Result<Array2<f32>> {
    decode_into_array(rays, order, nrays, max_gates, max_gates, |ray, dst| {
        if let Some(gates) = ray.moments.get(&data_type_id) {
            let n = dst.len().min(gates.len());
            dst[..n].copy_from_slice(&gates[..n]);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashMap;

    /// Build a synthetic single-ray sweep that carries one `data_type_id`
    /// of `gate_count` gates filled with `value`. Lets us exercise
    /// `convert_sweep` without going through the full IRIS decode path.
    fn one_ray_sweep(data_type_id: u8, gate_count: usize, value: f32) -> DecodedSweep {
        one_ray_sweep_at(data_type_id, gate_count, value, Utc::now(), 0.0)
    }

    /// Like [`one_ray_sweep`] but with an explicit sweep `start_time` and
    /// per-ray `time_offset_s`, for exercising time-coordinate anchoring.
    fn one_ray_sweep_at(
        data_type_id: u8,
        gate_count: usize,
        value: f32,
        start_time: chrono::DateTime<Utc>,
        time_offset_s: f32,
    ) -> DecodedSweep {
        let mut moments = HashMap::new();
        moments.insert(data_type_id, vec![value; gate_count]);
        let ray = DecodedRay {
            azimuth_deg: 0.0,
            elevation_deg: 0.5,
            time_offset_s,
            moments,
        };
        DecodedSweep {
            sweep_number: 1,
            fixed_angle_deg: 0.5,
            start_time,
            rays: vec![ray],
        }
    }

    /// Pin the IRIS-passthrough adapter path: `MomentMapping::Iris` rows
    /// produce a `MomentData` keyed by the IRIS short name, with the
    /// inline `units` string and **no** `standard_name`/`long_name`.
    /// A regression that routed Iris rows through the ODIM branch
    /// would either panic on the missing CF metadata lookup or emit
    /// the moment under the wrong name; this catches both.
    #[test]
    fn iris_passthrough_moment_emits_under_iris_short_name() {
        // DB_DBTE8 = id 71, MomentMapping::Iris { units: "dBZ" }.
        let sweep = one_ray_sweep(71, 4, 42.0);
        let range_axis: Vec<f32> = (0..4).map(|i| 1000.0 + i as f32 * 100.0).collect();

        let result =
            convert_sweep(&sweep, 0, ScanMode::Ppi, &range_axis, &[71]).expect("convert_sweep ok");

        // Variable name is the IRIS short name, not an ODIM translation.
        assert!(
            result.moments.contains_key("DB_DBTE8"),
            "expected DB_DBTE8 in moments, got {:?}",
            result.moments.keys().collect::<Vec<_>>()
        );
        let m = result.moments.get("DB_DBTE8").unwrap();
        // Units string is empty for IRIS-passthrough types — xradar
        // doesn't set a units attr for moments outside its
        // `iris_mapping` table, so we don't either.
        assert_eq!(m.units, "");
        // Iris-passthrough types skip the central CF metadata lookup,
        // so standard_name and long_name must remain unset.
        assert_eq!(m.standard_name, None);
        assert_eq!(m.long_name, None);
        // Sentinel attrs the adapter sets for every moment. NaN ≠ NaN
        // by IEEE-754 rules so we can't assert_eq directly.
        assert!(m.fill_value.expect("fill_value set").is_nan());
        assert_eq!(m.scale_factor, Some(1.0));
        assert_eq!(m.add_offset, Some(0.0));
    }

    /// Mirror test for the ODIM-mapped path: the variable lands under
    /// the ODIM short name with full CF metadata (units +
    /// standard_name + long_name) sourced from the central table.
    #[test]
    fn odim_mapped_moment_emits_under_odim_name_with_full_cf_metadata() {
        // DB_DBT = id 1, MomentMapping::Odim → output_name "DBTH".
        let sweep = one_ray_sweep(1, 4, 25.5);
        let range_axis: Vec<f32> = (0..4).map(|i| 1000.0 + i as f32 * 100.0).collect();

        let result =
            convert_sweep(&sweep, 0, ScanMode::Ppi, &range_axis, &[1]).expect("convert_sweep ok");

        let m = result
            .moments
            .get("DBTH")
            .unwrap_or_else(|| panic!("expected DBTH; got {:?}", result.moments.keys()));
        assert!(!m.units.is_empty(), "ODIM moment should carry units");
        assert!(
            m.standard_name.is_some(),
            "ODIM moment should carry standard_name from the CF metadata table"
        );
        assert!(
            m.long_name.is_some(),
            "ODIM moment should carry long_name from the CF metadata table"
        );
    }

    /// `ScanMode::Rhi` must produce `sweep_mode = "rhi"` in the
    /// per-sweep SigmetSweepAttrs and `SweepMode::Elevation` in the
    /// FM301 metadata. Other modes default to PPI / azimuth.
    #[test]
    fn rhi_scan_mode_propagates_to_sweep_attrs() {
        let sweep = one_ray_sweep(1, 4, 0.0);
        let range_axis = vec![100.0_f32; 4];

        let rhi = convert_sweep(&sweep, 0, ScanMode::Rhi, &range_axis, &[1]).unwrap();
        let rhi_attrs = rhi.metadata.sigmet.as_ref().unwrap();
        assert_eq!(rhi_attrs.sweep_mode, "rhi");
        assert_eq!(rhi.metadata.sweep_mode, radish_types::SweepMode::Elevation);

        let ppi = convert_sweep(&sweep, 0, ScanMode::Ppi, &range_axis, &[1]).unwrap();
        let ppi_attrs = ppi.metadata.sigmet.as_ref().unwrap();
        assert_eq!(ppi_attrs.sweep_mode, "azimuth_surveillance");
        assert_eq!(ppi.metadata.sweep_mode, radish_types::SweepMode::Azimuth);
    }

    /// **Regression (issue #28 time epoch).** The `time` coordinate must
    /// be the absolute acquisition instant — the sweep's `start_time`
    /// plus each ray's `time_offset_s` — not the bare 1970-epoch offset
    /// the old adapter produced by passing only `time_offset_s` into the
    /// coordinate builder. This is the fixture-free CI guard for the
    /// anchoring in `convert_sweep`; the prior tests all left
    /// `time_offset_s = 0` and never asserted on `coordinates.time`.
    #[test]
    fn convert_sweep_anchors_ray_time_to_sweep_start() {
        use chrono::TimeZone;

        let start = Utc.with_ymd_and_hms(2022, 6, 1, 0, 2, 48).unwrap();
        // DB_DBT (id 1) → DBTH, one ray at +5 s into the sweep.
        let sweep = one_ray_sweep_at(1, 4, 10.0, start, 5.0);
        let range_axis: Vec<f32> = (0..4).map(|i| 1000.0 + i as f32 * 100.0).collect();

        let out = convert_sweep(&sweep, 0, ScanMode::Ppi, &range_axis, &[1]).unwrap();
        let t0 = out.coordinates.time[0];
        let expected = start.timestamp() as f64 + 5.0;
        assert!(
            (t0 - expected).abs() < 1e-6,
            "time not anchored to sweep start: got {t0}, want {expected}"
        );
        // The old bug yielded ~5.0 (a 1970 offset); a real 2022 instant
        // is well past 1.6e9 Unix seconds.
        assert!(
            t0 > 1_600_000_000.0,
            "time must be absolute Unix seconds, got {t0}"
        );
    }
}
