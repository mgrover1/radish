//! Python bindings for radish.
//!
//! ## Ownership model
//!
//! Moment arrays and per-sweep state are *moved* out of the wrapper on first
//! access rather than cloned. A typical xarray-driven open touches every
//! sweep and every moment exactly once, and each moment array is on the
//! order of megabytes — cloning them all (the previous behaviour) was the
//! single largest cost in the wall-clock benchmark.
//!
//! Practical impact:
//!
//! * `VolumeData.get_sweep(i)` consumes sweep `i`. Calling it twice on the
//!   same index raises `RuntimeError`.
//! * `SweepData.get_moment(name)` consumes the moment. A second call returns
//!   `None`.
//! * `MomentData.data()` consumes the underlying numpy buffer (handed off
//!   via `PyArray2::from_owned_array`). A second call raises.
//! * Read-only metadata (`name`, `units`, `shape`, `num_rays`, `num_sweeps`,
//!   coordinates, etc.) is cached side-by-side and remains accessible after
//!   consumption.

use std::collections::HashMap;
use std::path::Path;

use ndarray::Array2;
use numpy::{PyArray1, PyArray2};
use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::pybacked::PyBackedBytes;
use pyo3::types::PyDict;

use radish::{
    backends::{
        auto_backend, auto_backend_for_bytes,
        nexrad::demux::{
            self, DemuxOptions, MomentSelector, OutputWord, RawMoment, RecordInventory,
            TargetEncoding,
        },
        CfRadial1Backend, IncompleteSweepPolicy, NexradBackend, RadarBackend, SigmetBackend,
    },
    Coordinates, DeclaredScale as RustDeclaredScale, MomentData as RustMomentData,
    NexradSweepAttrs as RustNexradSweepAttrs, NexradVolumeAttrs as RustNexradVolumeAttrs,
    RadishError, SigmetSweepAttrs as RustSigmetSweepAttrs,
    SigmetVolumeAttrs as RustSigmetVolumeAttrs, SweepData as RustSweepData, SweepMetadata,
    VolumeData as RustVolumeData, VolumeMetadata as RustVolumeMetadata,
};

/// Volume-level metadata: site coordinates, time coverage, sweep
/// inventory, and any backend-specific typed attribute objects
/// (NEXRAD `nexrad_attrs`, Sigmet `sigmet_attrs`).
///
/// Returned by `VolumeData.metadata` (a getter on
/// [`PyVolumeData`]) and by the format-specific scan helpers
/// (`radish.scan_cfradial1`, `radish.scan_nexrad`,
/// `radish.scan_sigmet`). Cheap to clone — the inner data is shared
/// per-volume and never grows with sweep count.
///
/// All angle and altitude fields use FM301 units (degrees, metres).
#[pyclass(name = "VolumeMetadata", from_py_object)]
#[derive(Clone)]
pub struct PyVolumeMetadata {
    inner: RustVolumeMetadata,
}

#[pymethods]
impl PyVolumeMetadata {
    /// Radar identifier from the source format. NEXRAD uses the 4-character
    /// ICAO code (e.g. `"KLOT"`); IRIS uses the configured site name (e.g.
    /// `"chiriqui-radar"`); CfRadial1 uses the file's `instrument_name`
    /// global attribute.
    #[getter]
    fn instrument_name(&self) -> &str {
        &self.inner.instrument_name
    }

    /// Radar latitude in degrees north (range [-90, 90]).
    #[getter]
    fn latitude(&self) -> f64 {
        self.inner.latitude
    }

    /// Radar longitude in degrees east (range [-180, 180]).
    #[getter]
    fn longitude(&self) -> f64 {
        self.inner.longitude
    }

    /// Radar antenna altitude in metres above mean sea level.
    #[getter]
    fn altitude(&self) -> f64 {
        self.inner.altitude
    }

    /// Per-sweep target elevation angles (degrees), one entry per sweep,
    /// in the same order as `sweep_group_names`.
    #[getter]
    fn sweep_fixed_angles(&self) -> Vec<f64> {
        self.inner.sweep_fixed_angles.clone()
    }

    /// Per-sweep group names (`["sweep_0", "sweep_1", ...]`). Used by
    /// the xarray backend to emit the root-level `sweep_group_name(sweep)`
    /// data variable that xradar's IRIS reader exposes.
    #[getter]
    fn sweep_group_names(&self) -> Vec<String> {
        self.inner.sweep_group_names.clone()
    }

    /// Number of sweeps (elevation cuts) in the volume.
    #[getter]
    fn num_sweeps(&self) -> usize {
        self.inner.sweep_group_names.len()
    }

    /// Positional indices of provably-partial sweeps (0-based, in
    /// original scan order — the `N` of each `sweep_N` name). Mirrors
    /// xradar's `NEXRADLevel2File.incomplete_sweeps`. Empty for
    /// complete volumes and for formats without partial-volume
    /// semantics. Under `incomplete_sweep="drop"` these indices name
    /// the sweeps that were dropped.
    #[getter]
    fn incomplete_sweeps(&self) -> Vec<usize> {
        self.inner.incomplete_sweep_indices.clone()
    }

    /// Institution operating the radar. Often empty for NEXRAD / IRIS;
    /// CfRadial1 files usually populate it.
    #[getter]
    fn institution(&self) -> &str {
        &self.inner.institution
    }

    /// Free-form attribute bag populated by the backend (e.g. NEXRAD writes
    /// `scan_name`, `vcp`, `vcp_description` here). Cloned into a fresh
    /// `dict` so Python callers can mutate without affecting the volume.
    #[getter]
    fn attributes(&self) -> std::collections::HashMap<String, String> {
        self.inner.attributes.clone()
    }

    /// Volume sequence number from the source file (CfRadial1
    /// `volume_number`, NEXRAD VCP volume index, …). 0 when not set.
    #[getter]
    fn volume_number(&self) -> u32 {
        self.inner.volume_number
    }

    /// ISO 8601 timestamp with `Z` suffix (e.g. `"2026-03-10T23:14:12Z"`),
    /// matching xradar's `time_coverage_start` root variable format.
    #[getter]
    fn time_coverage_start(&self) -> String {
        self.inner
            .time_coverage_start
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    /// ISO 8601 timestamp marking the end of volume coverage (last ray's
    /// time). Same format as [`time_coverage_start`][Self::time_coverage_start].
    #[getter]
    fn time_coverage_end(&self) -> String {
        self.inner
            .time_coverage_end
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    /// FM301 platform-type string (`"fixed"`, `"vehicle"`, ...). Returns
    /// `"fixed"` when not specified — that's the WSR-88D / ground-radar
    /// default and matches xradar.
    #[getter]
    fn platform_type(&self) -> String {
        self.inner
            .platform_type
            .map(|p| p.to_string())
            .unwrap_or_else(|| "fixed".to_string())
    }

    fn __repr__(&self) -> String {
        format!(
            "VolumeMetadata(instrument='{}', lat={:.4}, lon={:.4}, alt={:.1}, sweeps={})",
            self.inner.instrument_name,
            self.inner.latitude,
            self.inner.longitude,
            self.inner.altitude,
            self.num_sweeps()
        )
    }

    /// NEXRAD-specific volume attrs (MSG_2 + MSG_5). `None` for non-NEXRAD volumes.
    /// The xarray backend merges every field of the returned object into the
    /// root Dataset's `attrs` to match xradar's output verbatim.
    #[getter]
    fn nexrad_attrs(&self) -> Option<PyNexradVolumeAttrs> {
        self.inner
            .nexrad
            .as_ref()
            .cloned()
            .map(|inner| PyNexradVolumeAttrs { inner })
    }

    /// Sigmet/IRIS-specific volume attrs. `None` for non-Sigmet volumes.
    #[getter]
    fn sigmet_attrs(&self) -> Option<PySigmetVolumeAttrs> {
        self.inner
            .sigmet
            .as_ref()
            .cloned()
            .map(|inner| PySigmetVolumeAttrs { inner })
    }
}

/// Volume-level NEXRAD attrs surfaced from MSG_2 + MSG_5. Field names match
/// xradar's `Dataset.attrs` keys for drop-in compatibility.
///
/// `eq` derives `__eq__` from the underlying Rust `PartialEq` so users can
/// `radish.scan(path).nexrad_attrs == radish.scan(bytes).nexrad_attrs` —
/// useful for the parity checks bulk-ingest workflows do per file (e.g.
/// raw2zarr#244's chain-equivalence test).
#[pyclass(name = "NexradVolumeAttrs", eq, from_py_object)]
#[derive(Clone, PartialEq)]
pub struct PyNexradVolumeAttrs {
    inner: RustNexradVolumeAttrs,
}

#[pymethods]
impl PyNexradVolumeAttrs {
    /// Dynamic-scan-type label (e.g. `"SAILS x 1"`, `"MRLE x 2"`,
    /// `"none"`). MSG_5 cut sequence summary.
    #[getter]
    fn dynamic_scan_type(&self) -> &str {
        &self.inner.dynamic_scan_type
    }
    /// True if the VCP is an MPDA (multi-PRF dealiasing) variant.
    #[getter]
    fn mpda_vcp(&self) -> bool {
        self.inner.mpda_vcp
    }
    /// True if the VCP supports base-tilt scheduling.
    #[getter]
    fn base_tilt_vcp(&self) -> bool {
        self.inner.base_tilt_vcp
    }
    /// Number of base tilts in this VCP.
    #[getter]
    fn num_base_tilts(&self) -> u8 {
        self.inner.num_base_tilts
    }
    /// True if the VCP terminated early (AVSET truncation).
    #[getter]
    fn vcp_truncated(&self) -> bool {
        self.inner.vcp_truncated
    }
    /// True if a VCP sequence (multi-VCP rotation) is active.
    #[getter]
    fn vcp_sequence_active(&self) -> bool {
        self.inner.vcp_sequence_active
    }
    /// Number of elevation cuts the VCP intends to scan.
    #[getter]
    fn number_elevation_cuts(&self) -> u32 {
        self.inner.number_elevation_cuts
    }
    /// Doppler velocity resolution in m/s (typically 0.5).
    #[getter]
    fn doppler_velocity_resolution(&self) -> f32 {
        self.inner.doppler_velocity_resolution
    }
    /// VCP pulse-width label (`"short"` / `"long"`).
    #[getter]
    fn vcp_pulse_width(&self) -> &str {
        &self.inner.vcp_pulse_width
    }
    /// True if AVSET (Automated Volume Scan Evaluation and Termination)
    /// is enabled — sweeps may be skipped when no echoes are detected.
    #[getter]
    fn avset_enabled(&self) -> bool {
        self.inner.avset_enabled
    }
    /// True if Enhanced Beam Conditioning is enabled.
    #[getter]
    fn ebc_enabled(&self) -> bool {
        self.inner.ebc_enabled
    }
    /// Super-resolution status flag from MSG_2.
    #[getter]
    fn super_res_status(&self) -> u16 {
        self.inner.super_res_status
    }
    /// RDA build number (e.g. 2310 for build 23.10).
    #[getter]
    fn rda_build_number(&self) -> u16 {
        self.inner.rda_build_number
    }
    /// RDA operational-mode code from MSG_2.
    #[getter]
    fn operational_mode(&self) -> u16 {
        self.inner.operational_mode
    }
    /// Number of elevation cuts that actually got scanned (≤ `number_elevation_cuts`
    /// when AVSET truncates the volume).
    #[getter]
    fn actual_elevation_cuts(&self) -> u32 {
        self.inner.actual_elevation_cuts
    }

    /// Per-sweep MSG_5 attrs in sweep-index order. `len()` matches
    /// `VolumeMetadata.sweep_fixed_angles`. Reachable from
    /// `radish.scan_nexrad(path).nexrad_attrs.sweep_attrs[i]` so
    /// callers can classify SAILS×N / MRLE / MPDA / base-tilt slices
    /// without a full per-ray decode — preserves the metadata-only
    /// speedup of `scan_nexrad` vs xradar's per-ray path while exposing
    /// the per-cut data the classifier needs.
    #[getter]
    fn sweep_attrs(&self) -> Vec<PyNexradSweepAttrs> {
        self.inner
            .sweep_attrs
            .iter()
            .cloned()
            .map(|inner| PyNexradSweepAttrs { inner })
            .collect()
    }

    /// Per-sweep `(time_start, time_end)` ranges as Unix seconds since
    /// 1970-01-01 UTC (float). Matches the `Coordinates::time` axis
    /// convention used by every backend, so consumers can convert with
    /// `pandas.to_datetime(t, unit="s")` or
    /// `np.array(t, dtype="datetime64[s]")`. `None` for sweeps whose
    /// radials don't carry timestamps. Length matches `sweep_attrs`.
    #[getter]
    fn sweep_time_ranges(&self) -> Vec<Option<(f64, f64)>> {
        self.inner.sweep_time_ranges.clone()
    }
}

/// Per-sweep NEXRAD attrs from MSG_5 elevation cuts. Field names match
/// xradar's per-sweep `Dataset.attrs` keys.
///
/// `eq` derives `__eq__` from the underlying Rust `PartialEq` so users
/// can `attrs_a == attrs_b` rather than walking every field — symmetric
/// with `PyNexradVolumeAttrs`.
#[pyclass(name = "NexradSweepAttrs", eq, from_py_object)]
#[derive(Clone, PartialEq)]
pub struct PyNexradSweepAttrs {
    inner: RustNexradSweepAttrs,
}

#[pymethods]
impl PyNexradSweepAttrs {
    /// Waveform-type label for this cut (e.g. `"contiguous_surveillance"`,
    /// `"contiguous_doppler"`, `"batch"`).
    #[getter]
    fn waveform_type(&self) -> &str {
        &self.inner.waveform_type
    }
    /// Channel configuration (`"sz2_phase_coding"`, `"single_polarization"`, …).
    #[getter]
    fn channel_config(&self) -> &str {
        &self.inner.channel_config
    }
    /// Super-resolution control field; 0 means standard resolution.
    #[getter]
    fn super_resolution(&self) -> u8 {
        self.inner.super_resolution
    }
    /// True if this is a SAILS (low-elevation revisit) cut.
    #[getter]
    fn sails_cut(&self) -> bool {
        self.inner.sails_cut
    }
    /// SAILS sequence number (0 when not a SAILS cut).
    #[getter]
    fn sails_sequence_number(&self) -> u8 {
        self.inner.sails_sequence_number
    }
    /// True if this is an MRLE (mid-volume revisit) cut.
    #[getter]
    fn mrle_cut(&self) -> bool {
        self.inner.mrle_cut
    }
    /// MRLE sequence number (0 when not an MRLE cut).
    #[getter]
    fn mrle_sequence_number(&self) -> u8 {
        self.inner.mrle_sequence_number
    }
    /// True if this cut runs in MPDA (multi-PRF dealiasing) mode.
    #[getter]
    fn mpda_cut(&self) -> bool {
        self.inner.mpda_cut
    }
    /// True if this is a base-tilt cut.
    #[getter]
    fn base_tilt_cut(&self) -> bool {
        self.inner.base_tilt_cut
    }
}

/// Volume-level Sigmet/IRIS attrs (`TaskConfiguration` + `IngestHeader`).
/// Field names match xradar's `Dataset.attrs` keys for drop-in compatibility
/// with `xradar.io.open_iris_datatree`.
#[pyclass(name = "SigmetVolumeAttrs", from_py_object)]
#[derive(Clone)]
pub struct PySigmetVolumeAttrs {
    inner: RustSigmetVolumeAttrs,
}

#[pymethods]
impl PySigmetVolumeAttrs {
    /// Free-text task name from `TASK_END_INFO.task_configuration_file_name`
    /// (e.g. `"VOL_A"`).
    #[getter]
    fn task_name(&self) -> &str {
        &self.inner.task_name
    }
    /// IRIS firmware version string from `INGEST_CONFIGURATION.iris_version`
    /// (e.g. `"10.2"`).
    #[getter]
    fn iris_version(&self) -> &str {
        &self.inner.iris_version
    }
    /// High-PRF in Hz. 0 if unset / not derivable from TASK_DSP_INFO.
    #[getter]
    fn prf_hz(&self) -> f32 {
        self.inner.prf_hz
    }
    /// Low-PRF in Hz for dual-PRF schemes (0 if single-PRF).
    #[getter]
    fn prf_low_hz(&self) -> f32 {
        self.inner.prf_low_hz
    }
    /// Nyquist velocity (m/s) computed from wavelength × PRF / 4. 0 if
    /// wavelength wasn't extracted from TASK_CALIB_INFO.
    #[getter]
    fn nyquist_velocity_ms(&self) -> f32 {
        self.inner.nyquist_velocity_ms
    }
    /// Unambiguous range in metres (= c / (2 × prf_hz)).
    #[getter]
    fn unambiguous_range_m(&self) -> f32 {
        self.inner.unambiguous_range_m
    }
    /// Distilled scan mode label: `"PPI"`, `"RHI"`, or `"OTHER"`.
    #[getter]
    fn scan_mode(&self) -> &str {
        &self.inner.scan_mode
    }
}

/// Per-sweep Sigmet/IRIS attrs.
///
/// Reachable via `SweepData.sigmet_attrs` (a getter on
/// [`PySweepData`]). Returns `None` for non-Sigmet sweeps. The same fields surface as
/// FM301 0-d data variables (`sweep_mode`, `sweep_fixed_angle`) inside
/// the per-sweep xarray Dataset; this typed accessor is the
/// lower-level path.
#[pyclass(name = "SigmetSweepAttrs", from_py_object)]
#[derive(Clone)]
pub struct PySigmetSweepAttrs {
    inner: RustSigmetSweepAttrs,
}

#[pymethods]
impl PySigmetSweepAttrs {
    /// `"azimuth_surveillance"` for PPI sweeps, `"rhi"` for RHI sweeps.
    #[getter]
    fn sweep_mode(&self) -> &str {
        &self.inner.sweep_mode
    }
    /// Target elevation angle (PPI) or azimuth (RHI) in degrees.
    #[getter]
    fn fixed_angle_deg(&self) -> f32 {
        self.inner.fixed_angle_deg
    }
}

/// Python wrapper for `MomentData`.
///
/// The (rays × gates) `Array2<f32>` is moved out of `data` on the first
/// `data()` call via `PyArray2::from_owned_array`, which transfers
/// ownership to numpy with no `memcpy`. A second call raises.
///
/// `raw_codes`/`raw_codes_u16` mirror `data()`'s move-out-on-first-access
/// shape, but return `None` rather than raising on a second call (or when
/// the source backend/product never populated one at all — e.g. every
/// backend except NEXRAD Level 3) — there was previously NO Python
/// exposure for either field at all (confirmed by grepping `raw_codes`
/// across every `*.rs`/`*.py` file under `python/` before adding this: it
/// returned nothing), so this is new surface, not a widened existing one —
/// see plan 0012 §0/§3.2 for why that changes this touchpoint's scope and
/// design freedom versus what the plan originally assumed.
#[pyclass(name = "MomentData")]
pub struct PyMomentData {
    name: String,
    units: String,
    standard_name: Option<String>,
    long_name: Option<String>,
    shape: (usize, usize),
    data: Option<Array2<f32>>,
    raw_codes: Option<Array2<u8>>,
    raw_codes_u16: Option<Array2<u16>>,
    declared_scale: Option<RustDeclaredScale>,
}

impl PyMomentData {
    fn from_inner(m: RustMomentData) -> Self {
        let shape = m.shape();
        Self {
            name: m.name,
            units: m.units,
            standard_name: m.standard_name,
            long_name: m.long_name,
            shape,
            data: Some(m.data),
            raw_codes: m.raw_codes,
            raw_codes_u16: m.raw_codes_u16,
            declared_scale: m.declared_scale,
        }
    }
}

#[pymethods]
impl PyMomentData {
    /// ODIM short name (e.g. `"DBZH"`, `"VRADH"`) for ODIM-mapped moments,
    /// or the IRIS short name (e.g. `"DB_HCLASS"`) for Sigmet types
    /// without an ODIM equivalent.
    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    /// CF `units` string (e.g. `"dBZ"`, `"meters per seconds"`,
    /// `"degrees"`). Empty for IRIS-passthrough types where xradar
    /// also leaves the units blank.
    #[getter]
    fn units(&self) -> &str {
        &self.units
    }

    /// CF `standard_name` (e.g. `"radar_equivalent_reflectivity_factor_h"`).
    /// `None` when the backend doesn't have a mapping for the moment.
    #[getter]
    fn standard_name(&self) -> Option<&str> {
        self.standard_name.as_deref()
    }

    /// CF `long_name` (e.g. `"Equivalent reflectivity factor H"`).
    /// `None` when the backend doesn't have a mapping for the moment.
    #[getter]
    fn long_name(&self) -> Option<&str> {
        self.long_name.as_deref()
    }

    /// Array shape as `(num_rays, num_gates)`. Available even after
    /// `data()` has consumed the underlying buffer.
    #[getter]
    fn shape(&self) -> (usize, usize) {
        self.shape
    }

    /// Hand the moment array to numpy without copying. Single-use: a second
    /// call raises `RuntimeError`.
    fn data<'py>(&mut self, py: Python<'py>) -> PyResult<Bound<'py, PyArray2<f32>>> {
        let arr = self.data.take().ok_or_else(|| {
            PyRuntimeError::new_err("MomentData.data() has already been consumed")
        })?;
        Ok(PyArray2::from_owned_array(py, arr))
    }

    /// Verbatim on-wire codes, when the source format defines its wire
    /// format in terms of codes rather than physical values (currently:
    /// NEXRAD Level 3 digital radial products decoded via packet 16/AF1F —
    /// see [`Self::raw_codes_u16`] for packet 28). `None` for every other
    /// backend, or once already consumed — unlike [`Self::data`], a second
    /// call returns `None` rather than raising, since `None` already means
    /// "not applicable" for most moments and a second meaning here would
    /// need its own tri-state to distinguish from "already taken."
    /// Single-use for the same reason `data()` is: the underlying numpy
    /// array is moved out (`PyArray2::from_owned_array`), not copied.
    fn raw_codes<'py>(&mut self, py: Python<'py>) -> Option<Bound<'py, PyArray2<u8>>> {
        self.raw_codes
            .take()
            .map(|arr| PyArray2::from_owned_array(py, arr))
    }

    /// The `u16` counterpart to [`Self::raw_codes`], for a NEXRAD Level 3
    /// product decoded via packet 28 (XDR — e.g. `RATE`). `None` for every
    /// other product, including every packet-16/AF1F NEXRAD Level 3
    /// product, which uses [`Self::raw_codes`] instead — exactly one of
    /// the two is ever populated for a given moment. Same single-use,
    /// move-not-copy contract as `raw_codes()`.
    fn raw_codes_u16<'py>(&mut self, py: Python<'py>) -> Option<Bound<'py, PyArray2<u16>>> {
        self.raw_codes_u16
            .take()
            .map(|arr| PyArray2::from_owned_array(py, arr))
    }

    /// Physical value at [`Self::data_floor_code`], for the source
    /// format's OWN declared linear scale — see
    /// `radish::DeclaredScale`'s Rust doc. `None` when
    /// [`Self::raw_codes`]/[`Self::raw_codes_u16`] are both `None`, or for
    /// a categorical product (e.g. hydrometeor classification) whose codes
    /// index a fixed label table rather than a linear scale.
    #[getter]
    fn value_min(&self) -> Option<f32> {
        self.declared_scale.map(|s| s.value_min)
    }

    /// Physical value change per code above the floor. See
    /// [`Self::value_min`].
    #[getter]
    fn value_increment(&self) -> Option<f32> {
        self.declared_scale.map(|s| s.value_increment)
    }

    /// Number of codes that carry data, from the floor code up — metadata
    /// only, not itself enforced when computing [`Self::data`]. See
    /// [`Self::value_min`].
    #[getter]
    fn n_levels(&self) -> Option<u16> {
        self.declared_scale.map(|s| s.n_levels)
    }

    /// The lowest raw code that carries data — codes below this are
    /// sentinels (e.g. "below threshold"), never
    /// `value_min + (code - data_floor_code) * value_increment`. This
    /// floor always fits `u8` (it's `<= 8`) even for a
    /// [`Self::raw_codes_u16`] product's `u16` codes — apply the formula
    /// directly in Python (there's no per-code decode getter exposed
    /// here; Rust/wasm callers have `radish::DeclaredScale::decode_code`,
    /// generic over the code width, for the same formula). See
    /// [`Self::value_min`].
    #[getter]
    fn data_floor_code(&self) -> Option<u8> {
        self.declared_scale.map(|s| s.data_floor_code)
    }

    fn __repr__(&self) -> String {
        let (nrays, ngates) = self.shape;
        let state = if self.data.is_some() {
            "owned"
        } else {
            "consumed"
        };
        format!(
            "MomentData(name='{}', units='{}', shape=({nrays}, {ngates}), {state})",
            self.name, self.units,
        )
    }
}

/// Python wrapper for `SweepData`.
///
/// Moment slots are moved out on first `get_moment(name)` call. Coordinates
/// are exposed as numpy views via `PyArray1::from_slice` (one C-level
/// `memcpy`); cheap enough that we don't bother moving them.
#[pyclass(name = "SweepData")]
pub struct PySweepData {
    metadata: SweepMetadata,
    coordinates: Coordinates,
    moment_order: Vec<String>,
    moments: HashMap<String, Option<RustMomentData>>,
}

impl PySweepData {
    fn from_inner(s: RustSweepData) -> Self {
        // Stable iteration order, alphabetical so DataTree variable order
        // is deterministic regardless of the moments' HashMap state.
        let mut moment_order: Vec<String> = s.moments.keys().cloned().collect();
        moment_order.sort();
        let moments = s.moments.into_iter().map(|(k, v)| (k, Some(v))).collect();
        Self {
            metadata: s.metadata,
            coordinates: s.coordinates,
            moment_order,
            moments,
        }
    }
}

#[pymethods]
impl PySweepData {
    /// 0-indexed sweep number within the volume (matches xradar's
    /// `sweep_number` data variable, which is also 0-indexed even
    /// though the IRIS RAW format stores it 1-indexed on disk).
    #[getter]
    fn sweep_number(&self) -> u32 {
        self.metadata.sweep_number
    }

    /// `False` when the sweep's data is provably partial — a NEXRAD
    /// volume truncated mid-sweep, joined mid-rotation, or with an
    /// interior chunk gap (real-time chunk streams). Always `True`
    /// for formats without partial-volume semantics.
    #[getter]
    fn is_complete(&self) -> bool {
        self.metadata.is_complete
    }

    /// Target elevation (PPI) or azimuth (RHI) in degrees, from the
    /// sweep's `fixed_angle` field.
    #[getter]
    fn fixed_angle(&self) -> f64 {
        self.metadata.fixed_angle
    }

    /// CfRadial2 / FM301 `sweep_mode` value (e.g. `"azimuth_surveillance"`).
    /// Always set; defaults to `"azimuth_surveillance"` for NEXRAD PPI volumes.
    #[getter]
    fn sweep_mode(&self) -> String {
        self.metadata.sweep_mode.to_string()
    }

    /// CfRadial2 / FM301 `prt_mode` value (`"fixed"`, `"staggered"`, `"dual"`).
    /// Falls back to `"not_set"` when the source format doesn't surface it
    /// (matches xradar's convention so engine-swap users see the same shape).
    #[getter]
    fn prt_mode(&self) -> String {
        self.metadata
            .prt_mode
            .map(|m| m.to_string())
            .unwrap_or_else(|| "not_set".to_string())
    }

    /// CfRadial2 / FM301 `follow_mode` value (`"none"`, `"sun"`, ...).
    /// Falls back to `"not_set"` when the source format doesn't surface it.
    #[getter]
    fn follow_mode(&self) -> String {
        self.metadata
            .follow_mode
            .map(|m| m.to_string())
            .unwrap_or_else(|| "not_set".to_string())
    }

    /// Number of rays in the sweep.
    #[getter]
    fn num_rays(&self) -> usize {
        self.coordinates.num_rays()
    }

    /// Number of range gates per ray.
    #[getter]
    fn num_gates(&self) -> usize {
        self.coordinates.num_gates()
    }

    /// Names of every moment carried by this sweep, in alphabetical
    /// order (so `DataTree` variable order is deterministic across
    /// runs). Returns the names whether or not the moment has been
    /// consumed by [`get_moment`][Self::get_moment] yet.
    fn moment_names(&self) -> Vec<String> {
        self.moment_order.clone()
    }

    /// Take a moment by name. Returns `None` if the moment never existed *or*
    /// has already been taken.
    fn get_moment(&mut self, name: &str) -> Option<PyMomentData> {
        let slot = self.moments.get_mut(name)?;
        slot.take().map(PyMomentData::from_inner)
    }

    /// Per-ray azimuth angles (degrees, float32 ndarray of length `num_rays`).
    /// Sorted ascending — radish azimuth-sorts every PPI sweep so consumers
    /// can rely on the order.
    #[getter]
    fn azimuth<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f32>> {
        PyArray1::from_slice(py, &self.coordinates.azimuth)
    }

    /// Per-ray elevation angles (degrees, float32 ndarray of length
    /// `num_rays`). For PPI sweeps these are nearly constant; for RHI
    /// sweeps they're the swept axis.
    #[getter]
    fn elevation<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f32>> {
        PyArray1::from_slice(py, &self.coordinates.elevation)
    }

    /// Per-gate range-axis values in metres (float32 ndarray of length
    /// `num_gates`).
    #[getter]
    fn range<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f32>> {
        PyArray1::from_slice(py, &self.coordinates.range)
    }

    /// Per-ray timestamps as fractional seconds since the Unix epoch
    /// (float64 ndarray). Convert to numpy `datetime64[ns]` via
    /// `pandas.to_datetime(times, unit="s").values` if needed.
    #[getter]
    fn time<'py>(&self, py: Python<'py>) -> Bound<'py, PyArray1<f64>> {
        PyArray1::from_slice(py, &self.coordinates.time)
    }

    fn __repr__(&self) -> String {
        format!(
            "SweepData(sweep={}, angle={:.2}°, rays={}, gates={}, moments={})",
            self.sweep_number(),
            self.fixed_angle(),
            self.num_rays(),
            self.num_gates(),
            self.moment_order.len()
        )
    }

    /// NEXRAD-specific sweep attrs (MSG_5 elevation cut). `None` for non-NEXRAD sweeps.
    #[getter]
    fn nexrad_attrs(&self) -> Option<PyNexradSweepAttrs> {
        self.metadata
            .nexrad
            .as_ref()
            .cloned()
            .map(|inner| PyNexradSweepAttrs { inner })
    }

    /// Sigmet/IRIS-specific sweep attrs. `None` for non-Sigmet sweeps.
    #[getter]
    fn sigmet_attrs(&self) -> Option<PySigmetSweepAttrs> {
        self.metadata
            .sigmet
            .as_ref()
            .cloned()
            .map(|inner| PySigmetSweepAttrs { inner })
    }
}

/// Python wrapper for `VolumeData`.
///
/// Sweeps are moved out on first `get_sweep(i)` call; the volume metadata is
/// kept around so it stays cheaply accessible after every sweep is consumed
/// (`num_sweeps`, `metadata`, `__repr__`).
#[pyclass(name = "VolumeData")]
pub struct PyVolumeData {
    metadata: RustVolumeMetadata,
    sweeps: Vec<Option<RustSweepData>>,
}

impl PyVolumeData {
    fn from_inner(v: RustVolumeData) -> Self {
        Self {
            metadata: v.metadata,
            sweeps: v.sweeps.into_iter().map(Some).collect(),
        }
    }
}

#[pymethods]
impl PyVolumeData {
    /// Volume-level metadata (site coords, time coverage, sweep
    /// inventory, backend-specific typed attrs). Cheap to access — the
    /// inner data is cloned once and never grows with sweep count.
    #[getter]
    fn metadata(&self) -> PyVolumeMetadata {
        PyVolumeMetadata {
            inner: self.metadata.clone(),
        }
    }

    /// Number of sweeps in the volume (decoded count, not the count
    /// the source format claimed). Stays accessible after every sweep
    /// is consumed.
    #[getter]
    fn num_sweeps(&self) -> usize {
        self.sweeps.len()
    }

    /// Positional indices of provably-partial sweeps — see
    /// `VolumeMetadata.incomplete_sweeps`.
    #[getter]
    fn incomplete_sweeps(&self) -> Vec<usize> {
        self.metadata.incomplete_sweep_indices.clone()
    }

    /// Take a sweep by index. Each sweep can only be taken once; calling
    /// twice raises `RuntimeError` so the caller learns about the bug
    /// instead of silently getting a re-decoded copy.
    fn get_sweep(&mut self, index: usize) -> PyResult<PySweepData> {
        let slot = self
            .sweeps
            .get_mut(index)
            .ok_or_else(|| PyRuntimeError::new_err(format!("Invalid sweep index: {index}")))?;
        let sweep = slot.take().ok_or_else(|| {
            PyRuntimeError::new_err(format!("Sweep {index} has already been consumed"))
        })?;
        Ok(PySweepData::from_inner(sweep))
    }

    fn __repr__(&self) -> String {
        let consumed = self.sweeps.iter().filter(|s| s.is_none()).count();
        format!(
            "VolumeData(instrument='{}', sweeps={}, consumed={consumed})",
            self.metadata.instrument_name,
            self.sweeps.len(),
        )
    }
}

/// Read a CfRadial1 (NetCDF) file and return a fully-decoded
/// [`VolumeData`][PyVolumeData].
///
/// CfRadial1 is the legacy NetCDF-based weather-radar format defined by
/// NCAR. radish reads via `libnetcdf`, so the path must be on a local
/// filesystem (no in-memory `bytes` path — `libnetcdf` doesn't expose an
/// in-memory open). Use [`open_datatree`][crate::open_datatree] for a
/// format-agnostic entry that auto-detects this format from
/// `.nc` / `.nc4` extensions and the HDF5 magic prefix.
///
/// Raises `RuntimeError` if the file is missing, corrupt, or not a
/// valid CfRadial1 NetCDF.
#[pyfunction]
fn read_cfradial1(path: &str) -> PyResult<PyVolumeData> {
    CfRadial1Backend::new()
        .read_volume(Path::new(path))
        .map(PyVolumeData::from_inner)
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to read file: {e}")))
}

/// Scan a CfRadial1 file for metadata only, without decoding any
/// moment data.
///
/// Useful for cheap "what's in this file" queries — returns site
/// coordinates, time coverage, and sweep inventory in a few
/// milliseconds.
#[pyfunction]
fn scan_cfradial1(path: &str) -> PyResult<PyVolumeMetadata> {
    CfRadial1Backend::new()
        .scan_file(Path::new(path))
        .map(|inner| PyVolumeMetadata { inner })
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to scan file: {e}")))
}

/// Read a NEXRAD Level 2 (Archive II / AR2V) file and return a fully-
/// decoded [`VolumeData`][PyVolumeData].
///
/// Accepts uncompressed Archive II files (`KXXX########_######_V06`,
/// `*.ar2v`) and gzip-wrapped legacy archives. Internally LDM-bzip2
/// frames are decompressed in parallel via rayon; per-volume wall-clock
/// is consistently 16-18× faster than `xradar.io.open_nexradlevel2_datatree`.
///
/// The resulting volume carries `metadata.nexrad_attrs` populated from
/// the MSG_2 (RDA Status) and MSG_5 (Volume Coverage Pattern) messages.
#[pyfunction]
#[pyo3(signature = (path, incomplete_sweep = "keep"))]
fn read_nexrad(path: &str, incomplete_sweep: &str) -> PyResult<PyVolumeData> {
    let policy = parse_incomplete_sweep(incomplete_sweep)?;
    NexradBackend::new()
        .read_volume_with(Path::new(path), policy)
        .map(PyVolumeData::from_inner)
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to read NEXRAD file: {e}")))
}

/// Parse the `incomplete_sweep` string shared by the NEXRAD read
/// functions. `"keep"` is the low-level default (today's behavior);
/// `radish.open_datatree` defaults to `"drop"` for xradar #332 parity.
fn parse_incomplete_sweep(value: &str) -> PyResult<IncompleteSweepPolicy> {
    match value {
        "keep" => Ok(IncompleteSweepPolicy::Keep),
        "drop" => Ok(IncompleteSweepPolicy::Drop),
        "pad" => Ok(IncompleteSweepPolicy::Pad),
        other => Err(PyValueError::new_err(format!(
            "invalid incomplete_sweep={other:?}: expected 'keep', 'drop', or 'pad'"
        ))),
    }
}

/// Scan a NEXRAD Level 2 file for metadata only.
///
/// Currently still walks the full file (no MSG-5-only fast path yet),
/// but skips the per-ray moment decode so it returns ~3× faster than
/// `read_nexrad` on the same fixture.
#[pyfunction]
fn scan_nexrad(path: &str) -> PyResult<PyVolumeMetadata> {
    NexradBackend::new()
        .scan_file(Path::new(path))
        .map(|inner| PyVolumeMetadata { inner })
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to scan NEXRAD file: {e}")))
}

/// Scan a NEXRAD Level 2 volume's metadata from a single in-memory
/// byte buffer.
///
/// The bytes-input twin of `scan_nexrad`. Internal building block —
/// the public Python entry point is `radish.scan(data)` in
/// `python/radish/_open.py`. Kept reachable as
/// `radish._radish.scan_nexrad_bytes` so the dispatcher can import it
/// without a Rust-only call path.
///
/// Pair with `radish.read_nexrad_bytes` for the full-decode
/// equivalent. Returns ~3× faster on a typical fixture, matching the
/// `scan_nexrad` vs `read_nexrad` ratio.
///
/// **Compression-agnostic**: caller passes already-decompressed AR2V
/// bytes. radish does not handle gzip; for `.gz` archives use
/// fsspec's `compression="gzip"` filter or `gzip.decompress(raw)`.
#[pyfunction]
fn scan_nexrad_bytes(data: Vec<u8>) -> PyResult<PyVolumeMetadata> {
    NexradBackend::new()
        .scan_bytes_volume(data)
        .map(|inner| PyVolumeMetadata { inner })
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to scan NEXRAD bytes: {e}")))
}

/// Scan a NEXRAD Level 2 volume's metadata from a sequence of chunk
/// byte buffers.
///
/// Mirrors `read_nexrad_chunks` but skips the per-ray decode. Same
/// chunk-order contract: `S` first, then `I00..In`, then `E`.
/// Useful for the `unidata-nexrad-level2-chunks` S3 stream when you
/// only need VCP / instrument / time-coverage metadata without
/// paying for per-ray decode.
#[pyfunction]
fn scan_nexrad_chunks(chunks: Vec<Vec<u8>>) -> PyResult<PyVolumeMetadata> {
    NexradBackend::new()
        .scan_chunks_volume(chunks)
        .map(|inner| PyVolumeMetadata { inner })
        .map_err(chunk_list_err)
}

/// Read a NEXRAD Level 2 volume from a sequence of chunk byte buffers.
///
/// Mirrors xradar's `open_nexradlevel2_datatree(list_of_bytes)` API for the
/// `unidata-nexrad-level2-chunks` S3 stream. Chunks must be passed in scan
/// order (`S` first, then `I00..In`, then `E`) — concatenating them
/// reconstitutes a complete Archive II buffer that's handed to the same
/// decoder used by `read_nexrad`. Truncated volumes (no `E`, or only the
/// first few `I` chunks) decode whatever rays survive; incomplete trailing
/// sweeps come through with fewer rays than the VCP would normally produce.
///
/// Users typically `fs.open(p, "rb").read()` each path or fetch directly
/// from S3 via `fsspec`.
#[pyfunction]
#[pyo3(signature = (chunks, incomplete_sweep = "keep"))]
fn read_nexrad_chunks(chunks: Vec<Vec<u8>>, incomplete_sweep: &str) -> PyResult<PyVolumeData> {
    let policy = parse_incomplete_sweep(incomplete_sweep)?;
    NexradBackend::new()
        .read_chunks_volume_with(chunks, policy)
        .map(PyVolumeData::from_inner)
        .map_err(chunk_list_err)
}

/// Chunk-list errors: a rejected list (empty / out-of-order volume
/// header) is caller-fixable input, so it surfaces as `ValueError` —
/// matching xradar's `_concatenate_chunks`. Everything else (I/O,
/// decode) stays `RuntimeError`.
fn chunk_list_err(e: RadishError) -> PyErr {
    match e {
        RadishError::InvalidFormat(msg) => {
            PyValueError::new_err(format!("Invalid NEXRAD chunk list: {msg}"))
        }
        other => PyRuntimeError::new_err(format!("Failed to read NEXRAD chunks: {other}")),
    }
}

/// Read a NEXRAD Level 2 volume from a single in-memory byte buffer.
///
/// Internal building block — the public Python entry point is
/// `radish.open_datatree(data)` (in `python/radish/_open.py`). Kept
/// reachable as `radish._radish.read_nexrad_bytes` so the dispatcher can
/// import it without a Rust-only call path.
#[pyfunction]
#[pyo3(signature = (data, incomplete_sweep = "keep"))]
fn read_nexrad_bytes(data: Vec<u8>, incomplete_sweep: &str) -> PyResult<PyVolumeData> {
    let policy = parse_incomplete_sweep(incomplete_sweep)?;
    NexradBackend::new()
        .read_bytes_volume_with(data, policy)
        .map(PyVolumeData::from_inner)
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to read NEXRAD bytes: {e}")))
}

/// Read a Sigmet/IRIS RAW file and return a fully-decoded
/// [`VolumeData`][PyVolumeData].
///
/// IRIS RAW is the Vaisala/SIGMET native binary format used by hundreds
/// of operational radars worldwide; magic-byte sniff matches files
/// starting with the `INGEST_HEADER` (id=23) or `PRODUCT_HDR` (id=27)
/// `STRUCTURE_HEADER`. The decoder ports xradar's `iris.py` verbatim
/// for calibration parity but runs ~5–8× faster wall-clock thanks to
/// rayon-parallel per-sweep conversion.
///
/// Resulting volumes carry `metadata.sigmet_attrs` (PRF, Nyquist,
/// task name, IRIS firmware version, scan mode).
#[pyfunction]
fn read_sigmet(path: &str) -> PyResult<PyVolumeData> {
    SigmetBackend::new()
        .read_volume(Path::new(path))
        .map(PyVolumeData::from_inner)
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to read Sigmet file: {e}")))
}

/// Scan a Sigmet/IRIS RAW file for metadata only.
///
/// Reads the `INGEST_HEADER` and `TASK_CONFIGURATION` blocks, returning
/// site coords, sweep count, and Sigmet-specific volume attrs without
/// touching the per-ray RLE-encoded data.
#[pyfunction]
fn scan_sigmet(path: &str) -> PyResult<PyVolumeMetadata> {
    SigmetBackend::new()
        .scan_file(Path::new(path))
        .map(|inner| PyVolumeMetadata { inner })
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to scan Sigmet file: {e}")))
}

/// Read a Sigmet/IRIS RAW volume from a single in-memory byte buffer.
///
/// Same decoder as [`read_sigmet`] but skips the disk-read step —
/// useful when the file came from S3 / HTTP / a fsspec stream and
/// you'd rather not land it on disk first. The dispatcher uses this
/// internally when `radish.open_datatree(<bytes>)` sniffs the IRIS
/// magic prefix.
#[pyfunction]
fn read_sigmet_bytes(data: Vec<u8>) -> PyResult<PyVolumeData> {
    SigmetBackend::new()
        .read_bytes_volume(data)
        .map(PyVolumeData::from_inner)
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to read Sigmet bytes: {e}")))
}

// ────────────────────────────────────────────────────────────────────
// Low-level NEXRAD per-moment decoders (issue #32)
// ────────────────────────────────────────────────────────────────────

// PyO3 0.22's `create_exception!` expands a `cfg(feature = "gil-refs")`
// check that refers to *pyo3's* feature set, not ours, so rustc's
// `unexpected_cfgs` lint fires on code we don't own. Scoped to this
// module so the rest of the crate keeps the lint.
#[allow(unexpected_cfgs)]
mod exceptions {
    pyo3::create_exception!(
        _radish,
        MomentEncodingError,
        pyo3::exceptions::PyValueError,
        "The requested output encoding cannot represent the source data exactly.\n\
         \n\
         Raised by `decode_record_moment` / `decode_sweep_moment` when a moment's\n\
         on-wire `word_size`/`scale`/`offset` cannot be remapped losslessly onto\n\
         the requested dtype and grid, or when the requested `out_shape` is too\n\
         small to hold the data. radish refuses rather than silently returning\n\
         values on the wrong grid. Subclasses `ValueError`."
    );
}

use exceptions::MomentEncodingError;

/// Translate a demux-layer `RadishError` into the right Python exception.
///
/// `MomentEncoding` is the "your request can't be honoured" case and gets
/// its own catchable type; `Unsupported` (unknown moment name, legacy
/// MSG_1 input) is a plain `ValueError`; anything else is a decode
/// failure and stays `RuntimeError` like the rest of this module.
fn demux_err(error: RadishError) -> PyErr {
    match error {
        RadishError::MomentEncoding(msg) => MomentEncodingError::new_err(msg),
        RadishError::Unsupported(msg) => PyValueError::new_err(msg),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// Normalise anything numpy accepts as a dtype (`np.uint16`, `">u2"`,
/// `np.dtype("uint8")`, …) into an [`OutputWord`].
///
/// Resolved through `numpy.dtype()` itself rather than the `numpy` crate's
/// descriptor API so every spelling a caller might use works.
///
/// Output is always **native-endian**, so a non-native byte order is
/// rejected rather than silently ignored. These decoders hand back raw
/// transport words and their audience writes them straight into zarr
/// chunks and reference stores — an array that compares equal
/// element-wise but whose `.tobytes()` is byte-swapped is exactly the
/// kind of silent corruption this module exists to avoid.
fn output_word(py: Python<'_>, dtype: &Bound<'_, PyAny>) -> PyResult<OutputWord> {
    let resolved = py
        .import("numpy")?
        .getattr("dtype")?
        .call1((dtype,))
        // Only remap the errors that mean "that isn't a dtype". Anything
        // else (MemoryError, KeyboardInterrupt raised from a caller's
        // `__index__`, …) must propagate untouched.
        .map_err(|e| {
            if e.is_instance_of::<PyTypeError>(py) || e.is_instance_of::<PyValueError>(py) {
                let mapped =
                    PyTypeError::new_err(format!("{dtype:?} is not a valid numpy dtype: {e}"));
                mapped.set_cause(py, Some(e));
                mapped
            } else {
                e
            }
        })?;
    let kind: String = resolved.getattr("kind")?.extract()?;
    let itemsize: usize = resolved.getattr("itemsize")?.extract()?;
    let byteorder: String = resolved.getattr("byteorder")?.extract()?;
    // numpy reports "=" for native and "|" for not-applicable (1-byte),
    // so `np.uint8` / ">u1" keep working.
    if !matches!(byteorder.as_str(), "=" | "|") {
        return Err(PyTypeError::new_err(format!(
            "dtype {resolved} requests non-native byte order, but these decoders return \
             native-endian raw words. Pass np.uint16 (or '=u2') and call \
             .astype('>u2') / .byteswap() yourself if you need big-endian storage — \
             otherwise .tobytes() would be silently byte-swapped."
        )));
    }
    match (kind.as_str(), itemsize) {
        ("u", 1) => Ok(OutputWord::U8),
        ("u", 2) => Ok(OutputWord::U16),
        _ => Err(PyTypeError::new_err(format!(
            "dtype {resolved} is not supported — these decoders return the raw NEXRAD words, \
             so the output dtype must be uint8 or uint16 (got kind={kind:?}, \
             itemsize={itemsize}). Apply scale_factor/add_offset yourself to get floats."
        ))),
    }
}

/// Assemble the `DemuxOptions` shared by both decoders.
fn demux_options(
    py: Python<'_>,
    moment: &str,
    out_shape: (usize, usize),
    dtype: &Bound<'_, PyAny>,
    fill_value: u16,
    scale: Option<f32>,
    offset: Option<f32>,
) -> PyResult<DemuxOptions> {
    let target = match (scale, offset) {
        (None, None) => None,
        (Some(scale), Some(offset)) => Some(TargetEncoding::new(scale, offset)),
        _ => {
            return Err(PyValueError::new_err(
                "scale and offset must be given together — they jointly define the target raw \
                 grid (physical = (raw - offset) / scale)",
            ))
        }
    };
    let moment = moment.parse::<MomentSelector>().map_err(demux_err)?;
    // Fields are `pub`; set them directly (the crate-wide convention),
    // and `out_shape` flows through as one pair — no chance of
    // transposing rays/gates.
    let mut options = DemuxOptions::new(moment, out_shape, output_word(py, dtype)?);
    options.fill_value = fill_value;
    options.target = target;
    Ok(options)
}

/// Hand a decoded `RawMoment` to numpy as a 2-D array, without copying.
fn raw_moment_to_py(
    py: Python<'_>,
    raw: RawMoment,
    out_shape: (usize, usize),
) -> PyResult<Py<PyAny>> {
    /// The length always matches `out_shape` — both come from the same
    /// `DemuxOptions` — so the error path is an invariant check, not a
    /// user-input path.
    fn to_py<T: numpy::Element>(
        py: Python<'_>,
        values: Vec<T>,
        shape: (usize, usize),
    ) -> PyResult<Py<PyAny>> {
        let array = Array2::from_shape_vec(shape, values)
            .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        Ok(PyArray2::from_owned_array(py, array).into_any().unbind())
    }
    match raw {
        RawMoment::U8(values) => to_py(py, values, out_shape),
        RawMoment::U16(values) => to_py(py, values, out_shape),
    }
}

/// Convert a `RecordInventory` into the plain dict the Python API
/// documents.
fn inventory_to_py(py: Python<'_>, inventory: RecordInventory) -> PyResult<Py<PyAny>> {
    let out = PyDict::new(py);
    out.set_item("radial_count", inventory.radial_count)?;
    out.set_item("azimuth", PyArray1::from_slice(py, &inventory.azimuth))?;
    out.set_item("elevation", PyArray1::from_slice(py, &inventory.elevation))?;
    out.set_item(
        "azimuth_number",
        PyArray1::from_slice(py, &inventory.azimuth_number),
    )?;
    out.set_item(
        "elevation_number",
        PyArray1::from_slice(py, &inventory.elevation_number),
    )?;
    out.set_item(
        "collection_time_ms",
        PyArray1::from_slice(py, &inventory.collection_time_ms),
    )?;
    out.set_item(
        "modified_julian_date",
        PyArray1::from_slice(py, &inventory.modified_julian_date),
    )?;

    let moments = PyDict::new(py);
    for (selector, encoding) in &inventory.moments {
        let entry = PyDict::new(py);
        entry.set_item("word_size", encoding.word_size)?;
        entry.set_item("scale", encoding.scale)?;
        entry.set_item("offset", encoding.offset)?;
        entry.set_item("gate_count", encoding.gate_count)?;
        entry.set_item("max_gate_count", encoding.max_gate_count)?;
        entry.set_item("first_gate_km", encoding.first_gate_km)?;
        entry.set_item("gate_interval_km", encoding.gate_interval_km)?;
        entry.set_item("radials_present", encoding.radials_present)?;
        entry.set_item("uniform", encoding.uniform)?;
        // CF attributes, precomputed so callers don't rederive them.
        //
        // ICD Table XVII-B defines `scale == 0` as "gate words are
        // already in final units", so guard the division — emitting
        // inf/nan here would silently poison the
        // `array * scale_factor + add_offset` expression the docs tell
        // callers to write.
        let (scale_factor, add_offset) = if encoding.scale.is_normal() {
            (
                1.0 / f64::from(encoding.scale),
                -f64::from(encoding.offset) / f64::from(encoding.scale),
            )
        } else {
            (1.0, -f64::from(encoding.offset))
        };
        entry.set_item("scale_factor", scale_factor)?;
        entry.set_item("add_offset", add_offset)?;
        moments.set_item(selector.name(), entry)?;
    }
    out.set_item("moments", moments)?;
    Ok(out.into_any().unbind())
}

/// Demultiplex one moment out of a single **decompressed** LDM record.
///
/// `record` is one LDM record's message stream — the bytes past the
/// 4-byte control word, already bzip2-decompressed. Returns a
/// `(rays, gates)` array of the **raw** NEXRAD words; apply
/// `scale_factor` / `add_offset` yourself (both are in the dict returned
/// by `record_moment_encoding`).
///
/// `moment` is a NEXRAD short name (`"REF"`, `"VEL"`, `"SW"`, `"ZDR"`,
/// `"PHI"`, `"RHO"`, `"CFP"`); the ODIM names radish's volume readers
/// emit (`"DBZH"`, `"VRADH"`, …) are accepted too.
///
/// One row per Message 31 radial in record order. Rows past the radial
/// count, and rows where the moment is absent, are `fill_value`; a short
/// moment is right-padded with raw `0` (xradar parity). A record holding
/// no radials at all — the `S` chunk of a chunked volume, which carries
/// only MSG_2/MSG_5 — yields an all-`fill_value` array rather than an
/// error.
///
/// Pass `scale`/`offset` together to remap onto a target raw grid when
/// the source encoding differs (NEXRAD moment encodings change across RDA
/// builds). The remap is applied only when it is exactly representable;
/// otherwise `MomentEncodingError` is raised. Without them, every block
/// must already match the requested dtype width.
#[pyfunction]
#[pyo3(name = "decode_nexrad_record_moment")]
#[pyo3(signature = (record, moment, out_shape, dtype, fill_value=0, scale=None, offset=None))]
#[allow(clippy::too_many_arguments)]
fn decode_record_moment(
    py: Python<'_>,
    record: PyBackedBytes,
    moment: &str,
    out_shape: (usize, usize),
    dtype: &Bound<'_, PyAny>,
    fill_value: u16,
    scale: Option<f32>,
    offset: Option<f32>,
) -> PyResult<Py<PyAny>> {
    let options = demux_options(py, moment, out_shape, dtype, fill_value, scale, offset)?;
    // The decode is pure Rust over a GIL-independent buffer, so release
    // the GIL — this primitive is called once per moment per record and
    // must not serialise multi-threaded callers.
    let raw = py
        .detach(|| demux::decode_record_moment(&record, &options))
        .map_err(demux_err)?;
    raw_moment_to_py(py, raw, out_shape)
}

/// Demultiplex one moment out of a **compressed** sweep-sized byte span.
///
/// `span` is `[i32 control word][bzip2 payload]` repeated, exactly as it
/// appears in an Archive II file; a leading 24-byte `AR2V` volume header
/// is skipped if present, so a whole-volume buffer works too. Records are
/// decompressed and demultiplexed in parallel via rayon.
///
/// Same output contract, dtype rules, and `scale`/`offset` remap as
/// `decode_record_moment`.
///
/// **Cost:** the span is decompressed on every call, so pulling N
/// moments out of one span costs N bzip2 passes. On a 5.8 MB volume one
/// moment takes ~134 ms while `radish.read_nexrad` decodes all of them
/// in ~181 ms. Reach for this when you want one or two moments out of a
/// sweep-sized span; use `radish.open_datatree` when you want
/// everything, and `decode_record_moment` (which takes already-
/// decompressed bytes, ~0.06 ms per record) for chunk-level work.
///
/// With `sort_by_azimuth=True` rows are stably sorted by azimuth angle
/// before returning, trailing `fill_value` rows staying at the end.
/// Reproduce the same permutation for your coordinate arrays with
/// `np.argsort(sweep_moment_encoding(span)["azimuth"], kind="stable")`.
#[pyfunction]
#[pyo3(name = "decode_nexrad_sweep_moment")]
#[pyo3(signature = (span, moment, out_shape, dtype, fill_value=0, scale=None, offset=None, sort_by_azimuth=false))]
#[allow(clippy::too_many_arguments)]
fn decode_sweep_moment(
    py: Python<'_>,
    span: PyBackedBytes,
    moment: &str,
    out_shape: (usize, usize),
    dtype: &Bound<'_, PyAny>,
    fill_value: u16,
    scale: Option<f32>,
    offset: Option<f32>,
    sort_by_azimuth: bool,
) -> PyResult<Py<PyAny>> {
    let options = demux_options(py, moment, out_shape, dtype, fill_value, scale, offset)?;
    let raw = py
        .detach(|| demux::decode_sweep_moment(&span, &options, sort_by_azimuth))
        .map_err(demux_err)?;
    raw_moment_to_py(py, raw, out_shape)
}

/// Inspect a single **decompressed** LDM record: per-radial headers plus
/// the source encoding of every moment it carries.
///
/// Call this before `decode_record_moment` to size the output array and
/// to learn whether a `scale`/`offset` remap is required. Returns a dict:
///
/// * `radial_count` — row count a demux of this input will produce
/// * `azimuth`, `elevation`, `azimuth_number`, `elevation_number`,
///   `collection_time_ms`, `modified_julian_date` — per-radial arrays in
///   record order
/// * `moments` — `{"ZDR": {"word_size": 8, "scale": 16.0, "offset": 128.0,
///   "gate_count": …, "max_gate_count": …, "first_gate_km": …,
///   "gate_interval_km": …, "radials_present": …, "uniform": True,
///   "scale_factor": …, "add_offset": …}, …}`
///
/// `uniform=False` means at least one radial disagreed with the
/// first-seen `(word_size, scale, offset)` — decoding that moment then
/// *requires* an explicit `scale`/`offset` target. `max_gate_count` is
/// the safe gate dimension; `scale_factor`/`add_offset` are the CF
/// attributes for the first-seen encoding.
#[pyfunction]
#[pyo3(name = "nexrad_record_moment_encoding")]
fn record_moment_encoding(py: Python<'_>, record: PyBackedBytes) -> PyResult<Py<PyAny>> {
    let inventory = py
        .detach(|| demux::record_moment_encoding(&record))
        .map_err(demux_err)?;
    inventory_to_py(py, inventory)
}

/// Inspect a **compressed** sweep-sized byte span — the
/// `decode_sweep_moment` counterpart of `record_moment_encoding`.
///
/// Same input framing and same returned dict; per-radial arrays are
/// concatenated across records in record order, and each moment's
/// `radials_present` / `max_gate_count` / `uniform` are folded over the
/// whole span.
#[pyfunction]
#[pyo3(name = "nexrad_sweep_moment_encoding")]
fn sweep_moment_encoding(py: Python<'_>, span: PyBackedBytes) -> PyResult<Py<PyAny>> {
    let inventory = py
        .detach(|| demux::sweep_moment_encoding(&span))
        .map_err(demux_err)?;
    inventory_to_py(py, inventory)
}

/// Identify which radish backend (`"nexrad_level2"`, `"cfradial1"`, …)
/// owns a file path. Wraps `radish::backends::auto_backend(path).name()`.
///
/// Returns the backend's canonical short name; the Python `_open.py`
/// dispatcher maps that to the right reader. Failure (no backend matched)
/// surfaces as a `PyRuntimeError`.
#[pyfunction]
fn auto_backend_name(path: &str) -> PyResult<String> {
    auto_backend(Path::new(path))
        .map(|b| b.name().to_string())
        .map_err(|e| PyRuntimeError::new_err(format!("No backend matched: {e}")))
}

/// Identify which radish backend recognises an in-memory byte prefix.
/// Mirrors [`auto_backend_name`] for the bytes-input path.
///
/// 16 bytes is enough for every magic check we currently do (HDF5 needs
/// 8, AR2V needs 4, gzip needs 2). Callers may pass shorter slices; they
/// just risk getting "no backend matched" if the magic straddled the cut.
#[pyfunction]
fn auto_backend_name_for_bytes(head: Vec<u8>) -> PyResult<String> {
    auto_backend_for_bytes(&head)
        .map(|b| b.name().to_string())
        .map_err(|e| PyRuntimeError::new_err(format!("No backend matched: {e}")))
}

// ────────────────────────────────────────────────────────────────────
// Velocity dealiasing
// ────────────────────────────────────────────────────────────────────

/// `RadishError::Dealias` is always a caller-fixable input problem (shape
/// mismatch, bad Nyquist, invalid option) — `ValueError`, matching how
/// `demux_err` treats `Unsupported`. No other `RadishError` variant can
/// reach here (`radish::transforms::dealias_region_based`'s only error
/// path is `Dealias`), but the fallback keeps this total rather than
/// panicking on a future variant.
fn dealias_err(error: RadishError) -> PyErr {
    match error {
        RadishError::Dealias(msg) => PyValueError::new_err(msg),
        other => PyRuntimeError::new_err(other.to_string()),
    }
}

/// Dealias one sweep's Doppler velocity using Py-ART's region-based
/// algorithm — bit-exact with `pyart.correct.dealias_region_based` on
/// every unmasked gate (see `docs/NEXRAD_LEVEL3_WASM.md` and
/// `radish/tests/test_dealias_parity.rs` for the parity gate this is
/// checked against).
///
/// `velocity` and `valid_mask` must be the same 2D shape
/// (`[n_rays, n_gates]`). `valid_mask[i, j] = True` means gate `(i, j)`
/// is **valid** and should be dealiased — the opposite polarity of
/// Py-ART's own `gfilter`, which excludes on `True` (see
/// `radish::transforms::dealias_region_based`'s Rust doc for why this
/// binding keeps that convention rather than flipping it back).
///
/// Returns per-gate fold counts (`int32`), not corrected velocities —
/// `corrected = velocity + folds.astype(velocity.dtype) * 2.0 * nyquist`
/// is a cheap multiply-add a caller does itself. A masked-out gate always
/// reads fold `0`.
///
/// `interval_splits`/`skip_between_rays`/`skip_along_ray`/`centered`
/// default to Py-ART's own defaults (3, 100, 100, `True`) — kept in sync
/// with `radish::transforms::DealiasOptions::default()`
/// (`radish/src/transforms/dealias/mod.rs`) BY HAND; nothing enforces
/// this automatically, so update both together.
#[pyfunction]
#[pyo3(signature = (
    velocity, valid_mask, nyquist, rays_wrap_around,
    interval_splits = 3, skip_between_rays = 100, skip_along_ray = 100, centered = true,
))]
#[allow(clippy::too_many_arguments)]
fn dealias_region_based<'py>(
    py: Python<'py>,
    velocity: numpy::PyReadonlyArray2<'_, f32>,
    valid_mask: numpy::PyReadonlyArray2<'_, bool>,
    nyquist: f32,
    rays_wrap_around: bool,
    interval_splits: usize,
    skip_between_rays: i32,
    skip_along_ray: i32,
    centered: bool,
) -> PyResult<Bound<'py, PyArray2<i32>>> {
    // `.to_owned()`, not a borrowed `ArrayView2`: `py.detach()` below
    // releases the GIL for the (potentially ~100s of ms) computation, and
    // holding a view into Python-owned numpy memory across that boundary
    // would be unsound — another Python thread can run while the GIL is
    // released and mutate this same buffer through ordinary Python code,
    // racing the read happening here. Copying first (still under the
    // GIL) makes the buffer this thread reads private and immutable for
    // the duration of the detached call.
    let velocity = velocity.as_array().to_owned();
    let valid_mask = valid_mask.as_array().to_owned();
    let opts = radish::transforms::DealiasOptions {
        interval_splits,
        skip_between_rays,
        skip_along_ray,
        centered,
    };
    let folds = py
        .detach(|| {
            radish::transforms::dealias_region_based(
                &velocity,
                &valid_mask,
                nyquist,
                rays_wrap_around,
                opts,
            )
        })
        .map_err(dealias_err)?;
    Ok(PyArray2::from_owned_array(py, folds))
}

#[pymodule]
fn _radish(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyVolumeData>()?;
    m.add_class::<PyVolumeMetadata>()?;
    m.add_class::<PySweepData>()?;
    m.add_class::<PyMomentData>()?;
    m.add_class::<PyNexradVolumeAttrs>()?;
    m.add_class::<PyNexradSweepAttrs>()?;
    m.add_class::<PySigmetVolumeAttrs>()?;
    m.add_class::<PySigmetSweepAttrs>()?;
    m.add_function(wrap_pyfunction!(read_cfradial1, m)?)?;
    m.add_function(wrap_pyfunction!(scan_cfradial1, m)?)?;
    m.add_function(wrap_pyfunction!(read_nexrad, m)?)?;
    m.add_function(wrap_pyfunction!(scan_nexrad, m)?)?;
    m.add_function(wrap_pyfunction!(scan_nexrad_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(scan_nexrad_chunks, m)?)?;
    m.add_function(wrap_pyfunction!(read_nexrad_chunks, m)?)?;
    m.add_function(wrap_pyfunction!(read_nexrad_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(read_sigmet, m)?)?;
    m.add_function(wrap_pyfunction!(scan_sigmet, m)?)?;
    m.add_function(wrap_pyfunction!(read_sigmet_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(auto_backend_name, m)?)?;
    m.add_function(wrap_pyfunction!(auto_backend_name_for_bytes, m)?)?;
    m.add_function(wrap_pyfunction!(decode_record_moment, m)?)?;
    m.add_function(wrap_pyfunction!(decode_sweep_moment, m)?)?;
    m.add_function(wrap_pyfunction!(record_moment_encoding, m)?)?;
    m.add_function(wrap_pyfunction!(sweep_moment_encoding, m)?)?;
    m.add_function(wrap_pyfunction!(dealias_region_based, m)?)?;
    // `create_exception!` stamps `__module__ = "_radish"`, but the
    // extension actually lives at `radish._radish` and the exception is
    // re-exported from `radish`. Without this the class is unpicklable,
    // so a dask/multiprocessing worker that raises it loses the typed
    // exception when it crosses a process boundary — exactly the
    // parallel workflow these decoders exist for.
    let moment_encoding_error = m.py().get_type::<MomentEncodingError>();
    moment_encoding_error.setattr("__module__", "radish")?;
    m.add("MomentEncodingError", moment_encoding_error)?;
    Ok(())
}
