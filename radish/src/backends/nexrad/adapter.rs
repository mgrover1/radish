//! Convert a `nexrad_model::data::Scan` into radish's `VolumeData`.
//!
//! The strategy:
//!
//! * Volume metadata is derived from the scan's site (lat/lon/altitude/ICAO),
//!   the VCP number, and the scan's overall time range.
//! * For each sweep, the radials are sorted by azimuth *once* and the
//!   permutation is reused for every coordinate axis and every moment. We do
//!   not call [`SweepField::from_radials`]: it would re-sort the radial slice
//!   per moment and allocate an intermediate `Vec<f32>` + `Vec<GateStatus>`
//!   that we'd then copy into the final `Array2<f32>`. Instead we walk
//!   `Product::moment_data(radial)` directly and decode each gate straight
//!   into the output buffer.
//! * The first moment seen establishes the per-sweep range axis (finest gate
//!   count + step). Other moments are NaN-padded out to that gate count so the
//!   sweep has a single coherent (rays × gates) shape — matching radish's
//!   `Coordinates::range`.
//! * `BelowThreshold` and `RangeFolded` gates become `f32::NAN`, as do gates
//!   beyond a moment's `gate_count` and rows for radials that don't carry
//!   the moment at all.

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use ndarray::Array2;
use radish_types::{PlatformType, SweepMode};

use crate::{
    Coordinates, MomentData, RadishError, Result, SweepData, SweepMetadata, VolumeData,
    VolumeMetadata,
};

use super::attrs::{sweep_attrs_from_cut, volume_attrs};
use super::decode::messages::msg5::ElevationCut;
use super::decode::model::{nominal_ray_count, Radial, Scan, Sweep};
use super::decode::products::{CfpMomentValue, DataMoment, MomentValue, Product};
use super::mapping::{moment_meta, SUPPORTED_PRODUCTS};
use super::sniff;
use crate::backends::common::{
    assemble_ppi_coordinates, build_range_axis, decode_into_array, par_map, sort_indices_by_key,
    MomentGeometry,
};

/// What to do with sweeps whose data is provably partial
/// (`Sweep.complete == false` — truncated volume, mid-rotation join,
/// or interior chunk gap). Plan 0009; mirrors xradar #332's
/// `incomplete_sweep` values plus an explicit `Keep`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum IncompleteSweepPolicy {
    /// Emit incomplete sweeps as-is, with fewer rays than a full
    /// rotation — flagged via `SweepMetadata::is_complete`. The
    /// low-level default (today's behavior).
    #[default]
    Keep,
    /// Omit incomplete sweeps from the volume. `VolumeMetadata` keeps
    /// the *original* accounting: `sweep_group_names` /
    /// `sweep_fixed_angles` list only the survivors under their
    /// original positional names (`sweep_0`, `sweep_2`, …— gaps stay
    /// gaps, matching xradar's drop mode), and
    /// `incomplete_sweep_indices` still records what was dropped.
    Drop,
    /// Reindex incomplete sweeps onto the full-rotation grid: missing
    /// rays NaN across every moment, azimuth = uniform grid centers,
    /// elevation = sweep median, missing times extrapolated along
    /// rotation order. xradar's `reindex_angle` + `ipol_time`
    /// semantics, computed exactly from MSG_31 fields.
    Pad,
}

/// Convert a fully-decoded NEXRAD `Scan` into a radish `VolumeData`.
///
/// Sweep conversion is data-parallel: each `convert_sweep` invocation reads
/// only its own `Sweep` and writes its own owned `SweepData`. We dispatch
/// across rayon's global pool — already warmed up by the in-tree
/// decoder's parallel bzip2 decompression that ran moments earlier.
///
/// Under [`IncompleteSweepPolicy::Drop`], incomplete sweeps are filtered
/// *before* conversion — a degenerate force-emitted trailing sweep (e.g.
/// one radial with no supported moments) can therefore never fail the
/// whole volume in drop mode.
pub(super) fn convert_scan(
    scan: Scan,
    source: &Path,
    policy: IncompleteSweepPolicy,
) -> Result<VolumeData> {
    let mut metadata = build_volume_metadata(&scan, source)?;
    let cuts = scan.coverage_pattern.elevation_cuts();

    let selected: Vec<(usize, &Sweep)> = scan
        .sweeps
        .iter()
        .enumerate()
        .filter(|(_, s)| policy != IncompleteSweepPolicy::Drop || s.complete)
        .collect();

    if policy == IncompleteSweepPolicy::Drop {
        let kept: Vec<usize> = selected.iter().map(|(i, _)| *i).collect();
        retain_sweeps_in_metadata(&mut metadata, &kept);
    }

    let sweeps: Vec<SweepData> = par_map(&selected, |(idx, sweep)| {
        let data = convert_sweep(sweep, *idx, cuts.get(*idx))?;
        Ok::<_, RadishError>(if policy == IncompleteSweepPolicy::Pad && !sweep.complete {
            pad_sweep_to_full_rotation(data, sweep)
        } else {
            data
        })
    })?;
    Ok(VolumeData::new(metadata, sweeps))
}

/// Restrict `build_volume_metadata`'s per-sweep vectors to the sweeps
/// surviving [`IncompleteSweepPolicy::Drop`], identified by their
/// *original* positional indices. Names keep those original indices
/// (`sweep_2` stays `sweep_2` even when `sweep_1` was dropped);
/// `incomplete_sweep_indices` deliberately keeps recording what was
/// dropped. Lives next to [`build_volume_metadata`] because the two
/// are halves of one decision — any per-sweep vector added there must
/// be filtered here.
fn retain_sweeps_in_metadata(metadata: &mut VolumeMetadata, kept: &[usize]) {
    metadata.sweep_group_names = kept.iter().map(|i| format!("sweep_{i}")).collect();
    // Plain indexing: the angles were built one-entry-per-sweep from
    // the same scan a few lines earlier; a miss is a bug worth a panic.
    metadata.sweep_fixed_angles = kept
        .iter()
        .map(|i| metadata.sweep_fixed_angles[*i])
        .collect();
}

/// Build the `VolumeMetadata` from the scan, falling back to the file path for
/// the ICAO when the scan does not carry a `Site` (rare but possible for
/// truncated chunk files).
pub(super) fn build_volume_metadata(scan: &Scan, source: &Path) -> Result<VolumeMetadata> {
    let site = scan.site.as_ref();

    let icao = site
        .map(|s| s.icao_str().into_owned())
        .or_else(|| sniff::icao_from_filename(source).map(str::to_owned))
        .unwrap_or_else(|| "UNKN".to_string());

    // WSR-88D antenna height = base height + tower (feedhorn) height.
    let (latitude, longitude, altitude, altitude_agl) = match site {
        Some(s) => (
            f64::from(s.latitude_degrees),
            f64::from(s.longitude_degrees),
            f64::from(s.site_height_m) + f64::from(s.tower_height_m),
            Some(f64::from(s.tower_height_m)),
        ),
        None => (f64::NAN, f64::NAN, f64::NAN, None),
    };

    let (time_start, time_end) = scan
        .time_range()
        .unwrap_or((DateTime::<Utc>::UNIX_EPOCH, DateTime::<Utc>::UNIX_EPOCH));

    let num_sweeps = scan.sweeps.len();
    let mut metadata =
        VolumeMetadata::new(icao, latitude, longitude, altitude, time_start, time_end);
    metadata.altitude_agl = altitude_agl;
    metadata.institution = "NOAA/NWS".to_string();
    metadata.platform_type = Some(PlatformType::Fixed);
    metadata.generate_sweep_names(num_sweeps);
    // Prefer the MSG_5 commanded angle (`ElevationCut`) over the median
    // of per-ray MSG_31 angles — see [`fixed_angle_for`] for why. Keeps
    // the root-level `sweep_fixed_angle(sweep)` array byte-identical to
    // xradar's and aligned with the VCP reference.
    let cuts = scan.coverage_pattern.elevation_cuts();
    metadata.sweep_fixed_angles = scan
        .sweeps
        .iter()
        .enumerate()
        .map(|(idx, s)| fixed_angle_for(cuts.get(idx), s).unwrap_or(f64::NAN))
        .collect();

    // VCP attributes match xradar's `VCP-NNN` form (e.g. `VCP-212`) so
    // engine-swap users see the same scan_name string.
    let vcp_number = scan.coverage_pattern.number();
    metadata
        .attributes
        .insert("scan_name".to_string(), format!("VCP-{vcp_number}"));
    metadata
        .attributes
        .insert("vcp".to_string(), vcp_number.to_string());
    metadata.attributes.insert(
        "vcp_description".to_string(),
        scan.coverage_pattern.description().to_string(),
    );

    metadata.nexrad = Some(volume_attrs(
        &scan.coverage_pattern,
        scan.rda_status.as_ref(),
        &scan.sweeps,
        num_sweeps as u32,
    ));

    metadata.incomplete_sweep_indices = scan
        .sweeps
        .iter()
        .enumerate()
        .filter_map(|(idx, s)| (!s.complete).then_some(idx))
        .collect();

    Ok(metadata)
}

/// Convert a single `Sweep` into `SweepData`. Returns
/// [`RadishError::MalformedRecord`] when the sweep has no radials or no
/// supported moments.
pub(super) fn convert_sweep(
    sweep: &Sweep,
    sweep_idx: usize,
    cut: Option<&ElevationCut>,
) -> Result<SweepData> {
    let radials = sweep.radials.as_slice();
    if radials.is_empty() {
        return Err(RadishError::MalformedRecord {
            offset: 0,
            msg: format!("sweep {sweep_idx} has no radials"),
        });
    }

    // 1. Probe each supported product to see whether it appears in this sweep
    //    and capture its (first_gate, gate_interval, gate_count). Cheap: we
    //    only need to find the first radial with each product, no decoding.
    let geometries: Vec<(Product, MomentGeometry)> = SUPPORTED_PRODUCTS
        .iter()
        .filter_map(|&p| probe_geometry(radials, p).map(|g| (p, g)))
        .collect();
    let (_, canonical) = geometries
        .iter()
        .max_by_key(|(_, g)| g.gate_count)
        .ok_or_else(|| RadishError::MalformedRecord {
            offset: 0,
            msg: format!("sweep {sweep_idx} has no supported moments"),
        })?;
    let max_gates = canonical.gate_count;

    // 2. Sort radials by azimuth *once* and reuse for every axis + moment.
    let order = sort_indices_by_key(radials, |r| r.azimuth_angle_degrees);

    // 3. Build coordinates from the sorted permutation.
    let coordinates = assemble_ppi_coordinates(
        radials,
        &order,
        build_range_axis(canonical),
        |r| r.azimuth_angle_degrees,
        |r| r.elevation_angle_degrees,
        |r| {
            r.collection_time
                .map(|dt| dt.timestamp_micros() as f64 / 1.0e6)
                .unwrap_or(f64::NAN)
        },
    );

    let fixed_angle =
        fixed_angle_for(cut, sweep).unwrap_or_else(|| median_f32(&coordinates.elevation));
    let sweep_number = u32::from(sweep.elevation_number);
    let mut sweep_meta = SweepMetadata::new(sweep_number, SweepMode::Azimuth, fixed_angle);
    sweep_meta.nexrad = cut.map(sweep_attrs_from_cut);
    sweep_meta.is_complete = sweep.complete;

    // 4. Build moments by walking radials in sorted order and decoding gates
    //    directly into the (nrays × max_gates) Array2 — no intermediate Vec,
    //    no second sort.
    let nrays = radials.len();
    let mut moments: HashMap<String, MomentData> = HashMap::with_capacity(geometries.len());
    for (product, geometry) in &geometries {
        let arr = decode_product(radials, &order, *product, geometry, nrays, max_gates)?;
        let meta = moment_meta(*product);
        let mut moment = MomentData::new(meta.odim_name.to_string(), meta.units.to_string(), arr);
        moment.standard_name = Some(meta.standard_name.to_string());
        moment.long_name = Some(meta.long_name.to_string());
        moment.fill_value = Some(f32::NAN);
        moment.scale_factor = Some(1.0);
        moment.add_offset = Some(0.0);
        moments.insert(meta.odim_name.to_string(), moment);
    }

    Ok(SweepData::new(sweep_meta, moments, coordinates))
}

/// Reindex an incomplete sweep's dense (ascending-azimuth) arrays onto
/// the full-rotation grid — the [`IncompleteSweepPolicy::Pad`] transform.
///
/// Grid: `n` rays of `res = 360/n` degrees, 0°-aligned with centers at
/// `(slot + 0.5) * res` — the same grid xradar's `reindex_angle`
/// produces, and the same ascending-azimuth convention complete sweeps
/// already follow. `n` comes from MSG_31 `azimuth_resolution_spacing`
/// (exact), falling back to the median azimuth step of the observed
/// rays (MSG_1 legacy).
///
/// Each observed ray lands at `floor(az / res)`; on a collision the
/// first (lowest-azimuth) ray wins. Missing slots get NaN in every
/// moment, the sweep's median elevation, and a timestamp extrapolated
/// along *rotation* order from the earliest observed ray — linear
/// interpolation in slot order would go wrong when the observed arc
/// wraps 0° (cut start azimuths are arbitrary).
fn pad_sweep_to_full_rotation(mut data: SweepData, sweep: &Sweep) -> SweepData {
    let nrays = data.coordinates.azimuth.len();
    let Some(n) = full_rotation_size(sweep, &data.coordinates.azimuth) else {
        return data;
    };
    if n <= nrays {
        return data;
    }
    let res = 360.0_f32 / n as f32;

    // Dense row → grid slot; first ray wins a contested slot.
    let mut row_for_slot: Vec<Option<usize>> = vec![None; n];
    for k in 0..nrays {
        let az = data.coordinates.azimuth[k].rem_euclid(360.0);
        let slot = ((az / res) as usize).min(n - 1);
        if row_for_slot[slot].is_none() {
            row_for_slot[slot] = Some(k);
        }
    }

    let old = std::mem::take(&mut data.coordinates);
    let azimuth: Vec<f32> = (0..n).map(|i| (i as f32 + 0.5) * res).collect();
    let el_fill = median_f32(&old.elevation) as f32;
    let elevation: Vec<f32> = row_for_slot
        .iter()
        .map(|row| row.map_or(el_fill, |k| old.elevation[k]))
        .collect();
    let time = fill_missing_times(&row_for_slot, &old.time);

    for moment in data.moments.values_mut() {
        let ngates = moment.data.ncols();
        let mut arr = Array2::from_elem((n, ngates), f32::NAN);
        for (slot, row) in row_for_slot.iter().enumerate() {
            if let Some(k) = row {
                arr.row_mut(slot).assign(&moment.data.row(*k));
            }
        }
        moment.data = arr;
    }

    data.coordinates = Coordinates::new(time, old.range, azimuth, elevation);
    data
}

/// Rays in one full rotation for `sweep`: MSG_31's
/// `azimuth_resolution_spacing` when known, else inferred from the
/// median step between the dense sorted azimuths. `None` when there
/// aren't even two rays to infer from.
fn full_rotation_size(sweep: &Sweep, sorted_azimuths: &[f32]) -> Option<usize> {
    if let Some(n) = nominal_ray_count(&sweep.radials) {
        return Some(n);
    }
    if sorted_azimuths.len() < 2 {
        return None;
    }
    let steps: Vec<f32> = sorted_azimuths
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|d| *d > 0.0)
        .collect();
    if steps.is_empty() {
        return None;
    }
    let n = (360.0 / median_f32(&steps)).round() as usize;
    // Sanity band: legacy 1° (360) through split-cut 0.5° (720), with
    // headroom for exotic spacings. Outside it the inference is noise.
    (90..=1440).contains(&n).then_some(n)
}

/// Fill the padded grid's time axis. Slots holding an observed ray keep
/// its measured timestamp; missing slots get
/// `t0 + rotation_distance(slot0 → slot) * dt`, where `slot0`/`t0` are
/// the earliest observed ray and `dt` the per-slot scan rate from the
/// observed arc. Monotonic in rotation order even when the arc wraps 0°.
fn fill_missing_times(row_for_slot: &[Option<usize>], dense_time: &[f64]) -> Vec<f64> {
    let n = row_for_slot.len();
    let anchors: Vec<(usize, f64)> = row_for_slot
        .iter()
        .enumerate()
        .filter_map(|(slot, row)| row.map(|k| (slot, dense_time[k])))
        .filter(|(_, t)| t.is_finite())
        .collect();

    let Some(&(slot0, t0)) = anchors
        .iter()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
    else {
        // No usable timestamps at all — propagate what's there (NaN).
        return row_for_slot
            .iter()
            .map(|row| row.map_or(f64::NAN, |k| dense_time[k]))
            .collect();
    };
    // Total: `anchors` is non-empty (the min above proved it), and the
    // `(slot0, t0)` fallback is exactly the single-anchor semantic
    // (arc 0 → dt 0 → flood t0).
    let &(slot1, t1) = anchors
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or(&(slot0, t0));

    let arc = (slot1 + n - slot0) % n;
    let dt = if arc > 0 { (t1 - t0) / arc as f64 } else { 0.0 };

    (0..n)
        .map(|slot| match row_for_slot[slot] {
            Some(k) if dense_time[k].is_finite() => dense_time[k],
            _ => t0 + ((slot + n - slot0) % n) as f64 * dt,
        })
        .collect()
}

/// Find the first radial that carries `product` and return its geometry.
/// Returns `None` if the product is absent from every radial in the sweep.
///
/// Handles both the regular six moments (`Product::moment_data` →
/// `&OwnedMoment`) and the special-cased clutter filter power
/// (`Product::cfp_moment_data` → `&OwnedCfp`). Both impl the
/// `DataMoment` trait so the geometry call is uniform.
fn probe_geometry(radials: &[Radial], product: Product) -> Option<MomentGeometry> {
    radials.iter().find_map(|r| {
        if let Some(m) = product.moment_data(r) {
            return Some(MomentGeometry {
                first_gate_km: m.first_gate_range_km(),
                gate_interval_km: m.gate_interval_km(),
                gate_count: m.gate_count() as usize,
            });
        }
        if let Some(c) = product.cfp_moment_data(r) {
            return Some(MomentGeometry {
                first_gate_km: c.first_gate_range_km(),
                gate_interval_km: c.gate_interval_km(),
                gate_count: c.gate_count() as usize,
            });
        }
        None
    })
}

/// Decode every radial's `product` directly into a freshly-allocated
/// (nrays × max_gates) `Array2<f32>`. The decode walks the canonical sort
/// order and skips the upstream `SweepField` allocation+sort pair.
///
/// Padding rules (encoded in the buffer's pre-fill + closure contract):
/// * If a radial doesn't carry the product, its row becomes all-NaN.
/// * If a radial's `gate_count` < the sweep's `max_gates`, the trailing cells
///   become NaN.
/// * `MomentValue::BelowThreshold` / `RangeFolded` and any non-`Value` CFP
///   status code map to NaN, matching xradar's masked-array convention.
///
/// Dispatches between the two upstream accessor flavours: regular `Product`s
/// expose `Product::moment_data(radial) -> Option<&MomentData>`, while
/// `ClutterFilterPower` lives on `Radial::clutter_filter_power() ->
/// Option<&CFPMomentData>` with a different value enum. Both are funneled
/// through `decode_into_array` via a `fill_row` closure so the buffer-
/// management scaffold is shared.
fn decode_product(
    radials: &[Radial],
    order: &[usize],
    product: Product,
    geometry: &MomentGeometry,
    nrays: usize,
    max_gates: usize,
) -> Result<Array2<f32>> {
    let ngates = geometry.gate_count;
    if product == Product::ClutterFilterPower {
        decode_into_array(radials, order, nrays, ngates, max_gates, |r, dst| {
            if let Some(cfp) = r.clutter_filter_power.as_ref() {
                let n = dst.len();
                for (slot, v) in dst.iter_mut().zip(cfp.iter_moment_value().take(n)) {
                    *slot = scaled_cfp(v);
                }
            }
            // Untouched cells (radial without product, or md shorter than
            // dst) keep their pre-NaN value.
        })
    } else {
        decode_into_array(radials, order, nrays, ngates, max_gates, |r, dst| {
            if let Some(md) = product.moment_data(r) {
                let n = dst.len();
                for (slot, mv) in dst.iter_mut().zip(md.iter().take(n)) {
                    *slot = scaled_moment(mv);
                }
            }
        })
    }
}

#[inline]
fn scaled_moment(value: MomentValue) -> f32 {
    match value {
        MomentValue::Value(v) => v,
        MomentValue::BelowThreshold | MomentValue::RangeFolded => f32::NAN,
    }
}

#[inline]
fn scaled_cfp(value: CfpMomentValue) -> f32 {
    match value {
        CfpMomentValue::Value(v) => v,
        // Status variants represent metadata about the clutter filter,
        // not a measurement; xradar emits NaN for these.
        CfpMomentValue::Status(_) => f32::NAN,
    }
}

/// Median of a slice of f32 elevations using `select_nth_unstable_by` so we
/// don't allocate a fully-sorted copy of the slice for every fallback.
fn median_f32(elevations: &[f32]) -> f64 {
    if elevations.is_empty() {
        return f64::NAN;
    }
    let mut buf: Vec<f32> = elevations.to_vec();
    let mid = buf.len() / 2;
    let (_, m, _) = buf.select_nth_unstable_by(mid, |a, b| {
        a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
    });
    *m as f64
}

/// Pick the right elevation angle for a sweep, preferring the
/// MSG_5 *commanded* angle over the MSG_31 *achieved* (median-of-radials)
/// angle.
///
/// Two values both return "elevation in degrees" but mean different
/// things:
///
/// * [`ElevationCut::elevation_angle_degrees_f64`] — the commanded angle
///   from MSG_5 (the VCP definition; what the radar was *told* to point
///   at). xradar's `open_nexradlevel2_datatree` reads this. Matches the
///   VCP reference table users compare against (e.g. "VCP-32 sweep 1
///   = 0.5°").
/// * [`Sweep::elevation_angle_degrees`] — the median of MSG_31 per-ray
///   elevation angles (the *achieved* beam angle, averaged over the
///   sweep). The antenna's servo doesn't track the commanded angle
///   exactly, so this differs from the commanded angle by up to
///   ~0.18° on a typical scan.
///
/// Both readings are spec-compliant; we ship the commanded one for
/// xradar parity. Fallback chain: commanded → median-of-radials → None
/// (caller decides what to do — `f64::NAN` for the volume-level array,
/// `median_f32(coordinates.elevation)` for the per-sweep value).
fn fixed_angle_for(cut: Option<&ElevationCut>, sweep: &Sweep) -> Option<f64> {
    cut.map(|c| c.elevation_angle_degrees_f64())
        .or_else(|| sweep.elevation_angle_degrees().map(f64::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::nexrad::decode::messages::msg31::moment::MomentDescriptor;
    use crate::backends::nexrad::decode::messages::msg5::Msg5;
    use crate::backends::nexrad::decode::model::OwnedMoment;
    use crate::backends::nexrad::decode::products::CfpStatus;

    /// Plan 0009 pad: an incomplete 1.0°-spacing sweep reindexes onto
    /// the 360-slot grid — observed rays keep their values at
    /// `floor(az/res)`, everything else is NaN, and the azimuth
    /// coordinate becomes the uniform grid centers.
    #[test]
    fn pad_reindexes_incomplete_sweep_to_full_grid() {
        let mut radials = vec![
            ref_only_radial(0.5, 1, vec![10, 20]),
            ref_only_radial(1.5, 1, vec![30, 40]),
            ref_only_radial(2.5, 1, vec![50, 60]),
        ];
        for r in &mut radials {
            r.azimuth_resolution_spacing = 2; // 1.0° → 360 slots
        }
        let sweep = Sweep {
            elevation_number: 1,
            radials,
            complete: false,
        };
        let dense = convert_sweep(&sweep, 0, None).expect("dense convert");
        let padded = pad_sweep_to_full_rotation(dense, &sweep);

        assert_eq!(padded.coordinates.azimuth.len(), 360);
        assert_eq!(padded.coordinates.azimuth[0], 0.5);
        assert_eq!(padded.coordinates.azimuth[359], 359.5);
        let dbzh = &padded.moments["DBZH"];
        assert_eq!(dbzh.data.nrows(), 360);
        // Observed rays: az 0.5 → slot 0, az 1.5 → slot 1, az 2.5 → slot 2.
        // raw 10 → (10-66)/2 = -28.0 etc.
        assert_eq!(dbzh.data[[0, 0]], -28.0);
        assert_eq!(dbzh.data[[1, 0]], -18.0);
        assert_eq!(dbzh.data[[2, 0]], -8.0);
        // Everything else NaN.
        assert!(dbzh.data[[3, 0]].is_nan());
        assert!(dbzh.data[[359, 1]].is_nan());
        let nan_rows = (0..360)
            .filter(|&i| padded.moments["DBZH"].data[[i, 0]].is_nan())
            .count();
        assert_eq!(nan_rows, 357);
        // Elevation fill = sweep median (all rays at 0.5°).
        assert_eq!(padded.coordinates.elevation[100], 0.5);
    }

    /// Pad is a no-op for sweeps already at (or above) the rotation
    /// size, and for sweeps too small to infer a grid from.
    #[test]
    fn pad_leaves_full_or_uninferrable_sweeps_alone() {
        let sweep = Sweep {
            elevation_number: 1,
            radials: vec![ref_only_radial(10.0, 1, vec![2, 3])],
            complete: false,
        };
        let dense = convert_sweep(&sweep, 0, None).expect("dense convert");
        let nrays_before = dense.coordinates.azimuth.len();
        let padded = pad_sweep_to_full_rotation(dense, &sweep);
        assert_eq!(padded.coordinates.azimuth.len(), nrays_before);
    }

    /// The grid size falls back to median-azimuth-step inference when
    /// no radial carries `azimuth_resolution_spacing` (MSG_1 legacy).
    #[test]
    fn full_rotation_size_infers_from_azimuth_steps() {
        let sweep = Sweep {
            elevation_number: 1,
            radials: vec![ref_only_radial(0.0, 1, vec![2])], // spacing 0
            complete: false,
        };
        let azimuths: Vec<f32> = (0..10).map(|i| i as f32 * 1.0).collect();
        assert_eq!(full_rotation_size(&sweep, &azimuths), Some(360));
        let azimuths_half: Vec<f32> = (0..10).map(|i| i as f32 * 0.5).collect();
        assert_eq!(full_rotation_size(&sweep, &azimuths_half), Some(720));
        assert_eq!(full_rotation_size(&sweep, &[5.0]), None);
    }

    /// Degenerate time axes: no finite anchors propagates NaN; a
    /// single anchor floods its value (no rate to extrapolate with).
    #[test]
    fn fill_missing_times_handles_no_and_single_anchor() {
        let all_nan = fill_missing_times(&[Some(0), None, Some(1)], &[f64::NAN, f64::NAN]);
        assert!(all_nan.iter().all(|t| t.is_nan()));

        let single = fill_missing_times(&[None, Some(0), None], &[42.0]);
        assert_eq!(single, vec![42.0, 42.0, 42.0]);
    }

    /// Missing-slot times extrapolate along rotation order from the
    /// earliest observed ray — correct even when the observed arc
    /// wraps 0° (slots 4,5,0 observed here).
    #[test]
    fn fill_missing_times_is_rotation_aware_across_wrap() {
        let dense_time = vec![100.0, 101.0, 102.0];
        // Dense rows 0,1,2 sit at slots 4,5,0 of a 6-slot grid.
        let row_for_slot = vec![Some(2), None, None, None, Some(0), Some(1)];
        let t = fill_missing_times(&row_for_slot, &dense_time);
        assert_eq!(t[4], 100.0);
        assert_eq!(t[5], 101.0);
        assert_eq!(t[0], 102.0);
        // slot0=4, dt=1.0; rotation distance 4→1 is 3, 4→2 is 4, 4→3 is 5.
        assert_eq!(t[1], 103.0);
        assert_eq!(t[2], 104.0);
        assert_eq!(t[3], 105.0);
    }

    /// Plan 0009 drop: incomplete sweeps are filtered before conversion
    /// (a degenerate moment-less trailing sweep can't error the volume),
    /// survivors keep their original positional names, and the dropped
    /// indices stay on record.
    #[test]
    fn drop_policy_filters_incomplete_sweeps_and_keeps_original_names() {
        let make_sweep = |elev: u8, complete: bool| Sweep {
            elevation_number: elev,
            radials: vec![ref_only_radial(0.0, elev, vec![2, 3])],
            complete,
        };
        // Middle sweep incomplete; trailing sweep incomplete AND
        // moment-less (would error convert_sweep if not filtered).
        let mut degenerate = ref_only_radial(0.0, 3, vec![2]);
        degenerate.reflectivity = None;
        let sweeps = vec![
            make_sweep(1, true),
            make_sweep(2, false),
            Sweep {
                elevation_number: 3,
                radials: vec![degenerate],
                complete: false,
            },
        ];
        let all_radials: Vec<Radial> = sweeps.iter().flat_map(|s| s.radials.clone()).collect();
        let scan = Scan {
            coverage_pattern: Msg5::synthetic_from_radials(&all_radials),
            sweeps,
            site: None,
            rda_status: None,
        };

        let volume = convert_scan(scan, Path::new("unit-test"), IncompleteSweepPolicy::Drop)
            .expect("drop mode must not error on degenerate incomplete sweeps");
        assert_eq!(volume.sweeps.len(), 1);
        assert!(volume.sweeps[0].metadata.is_complete);
        assert_eq!(volume.metadata.sweep_group_names, vec!["sweep_0"]);
        assert_eq!(volume.metadata.incomplete_sweep_indices, vec![1, 2]);
    }

    /// Plan 0009: `Sweep.complete` must survive into both metadata
    /// surfaces — `SweepMetadata.is_complete` (full-decode path) and
    /// `VolumeMetadata.incomplete_sweep_indices` (metadata-only scan
    /// path, which never materializes `SweepData`).
    #[test]
    fn completeness_propagates_to_sweep_and_volume_metadata() {
        let sweeps = vec![
            Sweep {
                elevation_number: 1,
                radials: vec![ref_only_radial(0.0, 1, vec![2, 3])],
                complete: true,
            },
            Sweep {
                elevation_number: 2,
                radials: vec![ref_only_radial(0.0, 2, vec![2, 3])],
                complete: false,
            },
        ];
        let all_radials: Vec<Radial> = sweeps.iter().flat_map(|s| s.radials.clone()).collect();
        let scan = Scan {
            coverage_pattern: Msg5::synthetic_from_radials(&all_radials),
            sweeps,
            site: None,
            rda_status: None,
        };

        let meta = build_volume_metadata(&scan, Path::new("unit-test")).expect("metadata");
        assert_eq!(meta.incomplete_sweep_indices, vec![1]);

        let volume = convert_scan(scan, Path::new("unit-test"), IncompleteSweepPolicy::Keep)
            .expect("convert");
        assert!(volume.sweeps[0].metadata.is_complete);
        assert!(!volume.sweeps[1].metadata.is_complete);
        assert_eq!(volume.metadata.incomplete_sweep_indices, vec![1]);
    }

    fn empty_sweep(elevation_number: u8) -> Sweep {
        Sweep {
            elevation_number,
            radials: Vec::new(),
            complete: true,
        }
    }

    #[test]
    fn convert_sweep_errors_on_empty_radials() {
        let sweep = empty_sweep(1);
        match convert_sweep(&sweep, 0, None) {
            Err(RadishError::MalformedRecord { msg, .. }) => assert!(msg.contains("no radials")),
            other => panic!("expected MalformedRecord, got {other:?}"),
        }
    }

    #[test]
    fn scaled_moment_maps_non_valid_to_nan() {
        assert_eq!(scaled_moment(MomentValue::Value(42.0)), 42.0);
        assert!(scaled_moment(MomentValue::BelowThreshold).is_nan());
        assert!(scaled_moment(MomentValue::RangeFolded).is_nan());
    }

    #[test]
    fn scaled_cfp_maps_status_codes_to_nan() {
        assert_eq!(scaled_cfp(CfpMomentValue::Value(0.5)), 0.5);
        assert!(scaled_cfp(CfpMomentValue::Status(CfpStatus::FilterNotApplied)).is_nan());
        assert!(scaled_cfp(CfpMomentValue::Status(CfpStatus::PointClutterFilterApplied)).is_nan());
    }

    #[test]
    fn median_f32_handles_empty() {
        assert!(median_f32(&[]).is_nan());
    }

    #[test]
    fn median_f32_returns_middle_value() {
        // Median of [0.5, 1.5, 2.5] = 1.5
        assert!((median_f32(&[2.5, 0.5, 1.5]) - 1.5).abs() < 1e-6);
    }

    #[test]
    fn probe_geometry_returns_none_when_product_absent() {
        // Empty radial slice → no geometry for any product.
        let empty: &[Radial] = &[];
        assert!(probe_geometry(empty, Product::Reflectivity).is_none());
    }

    /// Build a single radial with reflectivity-only data. NEXRAD raw byte
    /// semantics: 0 → BelowThreshold, 1 → RangeFolded, ≥2 → physical via
    /// `(raw - offset) / scale`. We use scale=2.0, offset=66.0 (the legacy
    /// MSG_1 reflectivity defaults) so the math is hand-checkable.
    fn ref_only_radial(azimuth_deg: f32, elevation_number: u8, raw_gates: Vec<u8>) -> Radial {
        Radial {
            azimuth_number: 0,
            azimuth_angle_degrees: azimuth_deg,
            azimuth_resolution_spacing: 0,
            elevation_number,
            elevation_angle_degrees: 0.5,
            radial_status: 0,
            collection_time: None,
            reflectivity: Some(OwnedMoment {
                descriptor: MomentDescriptor {
                    gate_count: raw_gates.len() as u16,
                    range_to_first_gate_km: 2.0,
                    gate_interval_km: 0.25,
                    tover_db: 0.0,
                    snr_threshold_db: 0.0,
                    control_flags: 0,
                    data_word_size_bits: 8,
                    scale: 2.0,
                    offset: 66.0,
                },
                gate_bytes: raw_gates,
            }),
            velocity: None,
            spectrum_width: None,
            differential_reflectivity: None,
            differential_phase: None,
            correlation_coefficient: None,
            clutter_filter_power: None,
        }
    }

    #[test]
    fn convert_sweep_sorts_rays_and_decodes_reflectivity_correctly() {
        // Two rays in REVERSE azimuth order: the adapter must sort them so
        // row 0 has azimuth=10° and row 1 has azimuth=20°.
        let sweep = Sweep {
            elevation_number: 1,
            complete: true,
            radials: vec![
                ref_only_radial(20.0, 1, vec![130, 0, 130]),
                ref_only_radial(10.0, 1, vec![2, 1, 2]),
            ],
        };
        let sd = convert_sweep(&sweep, 0, None).expect("convert_sweep");

        // Coords reflect the sorted order.
        assert_eq!(sd.coordinates.azimuth, vec![10.0, 20.0]);
        let dbzh = sd.moments.get("DBZH").expect("DBZH present");
        assert_eq!(dbzh.shape(), (2, 3));

        // Row 0 (azimuth 10°): raws [2, 1, 2] → [-32.0, NaN, -32.0]
        assert!((dbzh.data[(0, 0)] - (-32.0)).abs() < 1e-6);
        assert!(dbzh.data[(0, 1)].is_nan());
        assert!((dbzh.data[(0, 2)] - (-32.0)).abs() < 1e-6);

        // Row 1 (azimuth 20°): raws [130, 0, 130] → [32.0, NaN, 32.0]
        assert!((dbzh.data[(1, 0)] - 32.0).abs() < 1e-6);
        assert!(dbzh.data[(1, 1)].is_nan());
        assert!((dbzh.data[(1, 2)] - 32.0).abs() < 1e-6);

        // Range axis matches the moment's geometry: first gate 2 km, step 0.25 km.
        assert_eq!(sd.coordinates.range, vec![2000.0, 2250.0, 2500.0]);
    }

    #[test]
    fn convert_sweep_pads_short_moments_with_nan() {
        let sweep = Sweep {
            elevation_number: 1,
            complete: true,
            radials: vec![
                ref_only_radial(0.0, 1, vec![10, 20]),
                ref_only_radial(180.0, 1, vec![30, 40]),
            ],
        };
        let sd = convert_sweep(&sweep, 0, None).expect("convert_sweep");
        let dbzh = &sd.moments["DBZH"];
        assert_eq!(dbzh.shape(), (2, 2));
        // raw=10 → (10-66)/2 = -28; raw=20 → (20-66)/2 = -23
        assert!((dbzh.data[(0, 0)] - (-28.0)).abs() < 1e-6);
        assert!((dbzh.data[(0, 1)] - (-23.0)).abs() < 1e-6);
    }

    /// Build an `ElevationCut` with a given commanded elevation
    /// angle in degrees. The angle is encoded back into ICD
    /// Table III-A binary form so the decoded
    /// `elevation_angle_degrees()` round-trips.
    fn cut_with_angle(angle_deg: f64) -> ElevationCut {
        // Inverse of `binary_angle_degrees`: angle * 65536 / 360,
        // mask off bits 0-2 (per Table III-A bits 3-15 carry the
        // angle).
        let raw = ((angle_deg * 65536.0 / 360.0).round() as u16) & !0b0000_0111;
        ElevationCut {
            elevation_angle_raw: raw,
            channel_configuration: 0,
            waveform_type: 1,
            super_resolution_control: 0,
            surveillance_prf_number: 1,
            surveillance_pulse_count: 17,
            azimuth_rate_raw: 0,
            reflectivity_threshold_raw: 0,
            velocity_threshold_raw: 0,
            spectrum_width_threshold_raw: 0,
            differential_reflectivity_threshold_raw: 0,
            differential_phase_threshold_raw: 0,
            correlation_coefficient_threshold_raw: 0,
            sector1_edge_angle_raw: 0,
            sector1_doppler_prf_number: 0,
            sector1_doppler_pulse_count: 0,
            supplemental_data: 0,
            sector2_edge_angle_raw: 0,
            sector2_doppler_prf_number: 0,
            sector2_doppler_pulse_count: 0,
            ebc_angle_raw: 0,
            sector3_edge_angle_raw: 0,
            sector3_doppler_prf_number: 0,
            sector3_doppler_pulse_count: 0,
            reserved: 0,
        }
    }

    /// Build a single MSG_31 radial whose elevation field is `elev_deg`.
    fn radial_at(azimuth_deg: f32, elev_deg: f32) -> Radial {
        Radial {
            azimuth_number: 0,
            azimuth_angle_degrees: azimuth_deg,
            azimuth_resolution_spacing: 0,
            elevation_number: 1,
            elevation_angle_degrees: elev_deg,
            radial_status: 0,
            collection_time: None,
            reflectivity: Some(OwnedMoment {
                descriptor: MomentDescriptor {
                    gate_count: 1,
                    range_to_first_gate_km: 2.0,
                    gate_interval_km: 0.25,
                    tover_db: 0.0,
                    snr_threshold_db: 0.0,
                    control_flags: 0,
                    data_word_size_bits: 8,
                    scale: 2.0,
                    offset: 66.0,
                },
                gate_bytes: vec![10],
            }),
            velocity: None,
            spectrum_width: None,
            differential_reflectivity: None,
            differential_phase: None,
            correlation_coefficient: None,
            clutter_filter_power: None,
        }
    }

    /// HIGH-priority regression: pin "we use MSG_5 commanded, not
    /// MSG_31 median." A future contributor swapping back to
    /// `sweep.elevation_angle_degrees()` would break this test.
    #[test]
    fn fixed_angle_for_prefers_msg5_commanded_over_msg31_median() {
        // Cut commands ~0.5° (binary-angle quantised). Three radials
        // whose elevations average to ~0.44°.
        let cut = cut_with_angle(0.5);
        let commanded = cut.elevation_angle_degrees_f64();
        let sweep = Sweep {
            elevation_number: 1,
            complete: true,
            radials: vec![
                radial_at(0.0, 0.4395),
                radial_at(120.0, 0.4395),
                radial_at(240.0, 0.4395),
            ],
        };

        let got = fixed_angle_for(Some(&cut), &sweep).expect("Some");
        assert!(
            (got - commanded).abs() < 1e-6,
            "expected commanded {commanded}, got {got}"
        );
        // Sanity: commanded is *not* the median (0.4395), so we'd
        // notice a regression that swapped back to it.
        assert!((got - 0.4395).abs() > 0.01, "got = {got} matches median");
    }

    /// HIGH-priority fallback: when the cut is missing (truncated VCP,
    /// malformed file), drop to the median-of-radials path.
    #[test]
    fn fixed_angle_for_falls_back_to_sweep_median_when_cut_is_none() {
        let sweep = Sweep {
            elevation_number: 1,
            complete: true,
            radials: vec![
                radial_at(0.0, 1.5),
                radial_at(120.0, 1.5),
                radial_at(240.0, 1.5),
            ],
        };
        let got = fixed_angle_for(None, &sweep).expect("Some");
        assert!(
            (got - 1.5).abs() < 1e-6,
            "expected 1.5° (median), got {got}"
        );
    }

    /// HIGH-priority: when both the cut and the sweep are unusable
    /// (no radials, no cut), `fixed_angle_for` must return None so the
    /// caller can route to its own fallback.
    #[test]
    fn fixed_angle_for_returns_none_for_empty_sweep_and_no_cut() {
        assert!(fixed_angle_for(None, &empty_sweep(1)).is_none());
    }

    /// MEDIUM: the lossless `f64::from(f32)` path in the sweep-only
    /// fallback must not introduce float drift.
    #[test]
    fn fixed_angle_for_promotes_f32_to_f64_losslessly() {
        let sweep = Sweep {
            elevation_number: 1,
            complete: true,
            radials: vec![radial_at(0.0, 1.5_f32)],
        };
        let got = fixed_angle_for(None, &sweep).expect("Some");
        assert_eq!(got, f64::from(1.5_f32));
    }
}
