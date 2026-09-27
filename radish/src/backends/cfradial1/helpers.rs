//! Low-level NetCDF helpers used by the CfRadial1 backend.
//!
//! These wrap the netcdf crate's getters with radish's `Result` type and
//! collapse a handful of `AttributeValue` variants into the `Option<String>`
//! / `Option<f32>` shapes the backend wants. Kept separate from the trait
//! impl in `mod.rs` so the high-level read flow stays readable.

use radish_types::{PlatformType, SweepMode};

use crate::{RadishError, Result};

/// Read a string-valued global or variable attribute. Returns `None` for any
/// missing or non-string value (nullable on purpose — callers fall back to
/// defaults like `"unknown"` rather than failing the whole read).
pub(super) fn read_string_attr(file: &netcdf::File, name: &str) -> Option<String> {
    file.attribute(name)
        .and_then(|a| a.value().ok())
        .and_then(|v| match v {
            netcdf::AttributeValue::Str(s) => Some(s),
            netcdf::AttributeValue::Uchar(u) => Some(u.to_string()),
            netcdf::AttributeValue::Uchars(u) => Some(String::from_utf8_lossy(&u).to_string()),
            _ => None,
        })
}

/// Read a single value from a scalar netCDF variable.
pub(super) fn read_scalar_var<T: netcdf::NcTypeDescriptor + Copy>(
    file: &netcdf::File,
    name: &str,
) -> Result<T> {
    let var = file
        .variable(name)
        .ok_or_else(|| RadishError::MissingVariable(name.to_string()))?;
    var.get_value(0).map_err(RadishError::NetCdf)
}

/// Read a 1-D netCDF variable into a `Vec<T>` of decoded values.
pub(super) fn read_var_1d<T: netcdf::NcTypeDescriptor + Copy>(
    file: &netcdf::File,
    name: &str,
) -> Result<Vec<T>> {
    let var = file
        .variable(name)
        .ok_or_else(|| RadishError::MissingVariable(name.to_string()))?;
    var.get_values(..).map_err(RadishError::NetCdf)
}

/// Read a 1-D netCDF string variable. Falls back to `"unknown"` for individual
/// indices that fail to decode rather than aborting the entire read.
pub(super) fn read_var_1d_str(file: &netcdf::File, name: &str) -> Result<Vec<String>> {
    let var = file
        .variable(name)
        .ok_or_else(|| RadishError::MissingVariable(name.to_string()))?;

    let dims = var.dimensions();
    if dims.is_empty() {
        return Ok(vec![]);
    }

    let len = dims[0].len();
    let mut result = Vec::with_capacity(len);
    for i in 0..len {
        match var.get_string(i) {
            Ok(s) => result.push(s),
            Err(_) => result.push("unknown".to_string()),
        }
    }
    Ok(result)
}

/// Map a CfRadial `sweep_mode` string to the radish enum. Defaults to
/// `Azimuth` (PPI) — the most common surveillance mode — when the input
/// is unrecognised.
pub(super) fn parse_sweep_mode(mode_str: &str) -> SweepMode {
    match mode_str.to_lowercase().as_str() {
        "azimuth_surveillance" | "ppi" | "sur" => SweepMode::Azimuth,
        "elevation_surveillance" | "rhi" => SweepMode::Elevation,
        "sector" | "sec" => SweepMode::Sector,
        "pointing" | "pnt" => SweepMode::Pointing,
        "vertical_pointing" | "vert" => SweepMode::VerticalPointing,
        "calibration" | "cal" => SweepMode::Calibration,
        _ => SweepMode::Azimuth,
    }
}

/// Parse a CfRadial `platform_type` attribute. `None` for unrecognised
/// values so the caller can leave the field unset.
pub(super) fn parse_platform_type(type_str: &str) -> Option<PlatformType> {
    match type_str.to_lowercase().as_str() {
        "fixed" => Some(PlatformType::Fixed),
        "vehicle" => Some(PlatformType::Vehicle),
        "ship" => Some(PlatformType::Ship),
        "aircraft" => Some(PlatformType::Aircraft),
        "satellite" => Some(PlatformType::Satellite),
        _ => None,
    }
}
