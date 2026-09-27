//! Sigmet/IRIS-specific volume and sweep attributes.
//!
//! Mirrors what `xradar.io.open_iris_datatree` puts on the root `Dataset`
//! and per-sweep `Dataset.attrs`, so a `DataTree` produced through radish
//! is a drop-in replacement for an xradar one. Defaults are deliberate:
//! xradar's reader fills them in only when the corresponding TASK_*
//! sub-block is present, and a fresh `Default` value matches what xradar
//! emits for files where the field is missing.

use serde::{Deserialize, Serialize};

/// Volume-level Sigmet attrs (TASK_CONFIGURATION + INGEST_HEADER).
///
/// `#[non_exhaustive]` because we expect to add fields as more
/// TASK_* sub-blocks get parsed (TASK_CALIB_INFO wavelength is the
/// next planned addition); downstream `match { .. }` callers shouldn't
/// be broken by that.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SigmetVolumeAttrs {
    /// Free-text task name from `TASK_END_INFO.task_configuration_file_name`.
    pub task_name: String,
    /// IRIS firmware version string (`INGEST_CONFIGURATION.iris_version`).
    pub iris_version: String,
    /// High-PRF in Hz. 0 if unset / not derivable from TASK_DSP_INFO.
    pub prf_hz: f32,
    /// Low-PRF in Hz for dual-PRF schemes (0 if single-PRF).
    pub prf_low_hz: f32,
    /// Nyquist velocity (m/s) computed from wavelength × PRF / 4.
    /// 0 if wavelength wasn't extracted.
    pub nyquist_velocity_ms: f32,
    /// Unambiguous range in metres (= c / (2 × prf_hz)).
    pub unambiguous_range_m: f32,
    /// Distilled scan mode: `"PPI"`, `"RHI"`, or `"OTHER"`.
    pub scan_mode: String,
}

/// Per-sweep Sigmet attrs. Index-aligned with the volume's sweep list.
///
/// Kept minimal in PR-B; we expand if/when xradar surfaces sweep-level
/// IRIS-specific attrs we want to mirror. Marked `#[non_exhaustive]`
/// for the same reason as [`SigmetVolumeAttrs`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SigmetSweepAttrs {
    /// `"azimuth_surveillance"` for PPI, `"rhi"` for RHI.
    pub sweep_mode: String,
    /// Target elevation (PPI) or azimuth (RHI) in degrees.
    pub fixed_angle_deg: f32,
}
