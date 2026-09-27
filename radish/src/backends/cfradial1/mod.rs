//! CfRadial1 backend for reading CF/Radial NetCDF files.
//!
//! Module layout mirrors the other backends (e.g. `nexrad`):
//!
//! * `mod.rs` — `CfRadial1Backend` struct and the `RadarBackend` trait impl,
//!   plus the high-level `read_volume_metadata` / `read_sweep_data` /
//!   `read_moment` helpers that drive a single open file through the
//!   CfRadial1 conventions.
//! * `helpers.rs` — small reusable wrappers around the netcdf crate
//!   (`read_string_attr`, `read_scalar_var`, `read_var_1d`, `read_var_1d_str`,
//!   `parse_sweep_mode`, `parse_platform_type`).

mod helpers;
mod sniff;

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use ndarray::Array2;

use crate::{
    backends::RadarBackend, Coordinates, MomentData, RadishError, Result, SweepData, SweepMetadata,
    VolumeData, VolumeMetadata,
};

use helpers::{
    parse_platform_type, parse_sweep_mode, read_scalar_var, read_string_attr, read_var_1d,
    read_var_1d_str,
};

/// Backend for reading CfRadial1 format (CF/Radial NetCDF)
pub struct CfRadial1Backend;

impl CfRadial1Backend {
    /// Create a new CfRadial1Backend
    pub fn new() -> Self {
        Self
    }

    /// Read volume metadata from NetCDF file
    fn read_volume_metadata(&self, file: &netcdf::File) -> Result<VolumeMetadata> {
        let instrument_name =
            read_string_attr(file, "instrument_name").unwrap_or_else(|| "unknown".to_string());
        let institution =
            read_string_attr(file, "institution").unwrap_or_else(|| "unknown".to_string());

        let latitude = read_scalar_var::<f64>(file, "latitude")?;
        let longitude = read_scalar_var::<f64>(file, "longitude")?;
        let altitude = read_scalar_var::<f64>(file, "altitude")?;
        let altitude_agl = read_scalar_var::<f64>(file, "altitude_agl").ok();

        let time_coverage_start = read_string_attr(file, "time_coverage_start")
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .ok_or_else(|| RadishError::MissingAttribute("time_coverage_start".to_string()))?;

        let time_coverage_end = read_string_attr(file, "time_coverage_end")
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .ok_or_else(|| RadishError::MissingAttribute("time_coverage_end".to_string()))?;

        let sweep_number = read_var_1d::<i32>(file, "sweep_number")?;
        let sweep_fixed_angle = read_var_1d::<f64>(file, "fixed_angle")?;

        let num_sweeps = sweep_number.len();
        let sweep_group_names: Vec<String> =
            (0..num_sweeps).map(|i| format!("sweep_{}", i)).collect();

        let volume_number = read_scalar_var::<u32>(file, "volume_number").unwrap_or(0);
        let frequency = read_scalar_var::<f64>(file, "frequency").ok();
        let platform_type =
            read_string_attr(file, "platform_type").and_then(|s| parse_platform_type(&s));

        let mut metadata = VolumeMetadata::new(
            instrument_name,
            latitude,
            longitude,
            altitude,
            time_coverage_start,
            time_coverage_end,
        );

        metadata.volume_number = volume_number;
        metadata.institution = institution;
        metadata.platform_type = platform_type;
        metadata.altitude_agl = altitude_agl;
        metadata.sweep_group_names = sweep_group_names;
        metadata.sweep_fixed_angles = sweep_fixed_angle;
        metadata.frequency = frequency;

        Ok(metadata)
    }

    /// Read a specific sweep's data
    fn read_sweep_data(&self, file: &netcdf::File, sweep_idx: usize) -> Result<SweepData> {
        let sweep_start_ray_index = read_var_1d::<i32>(file, "sweep_start_ray_index")?;
        let sweep_end_ray_index = read_var_1d::<i32>(file, "sweep_end_ray_index")?;

        if sweep_idx >= sweep_start_ray_index.len() {
            return Err(RadishError::InvalidSweepIndex(sweep_idx));
        }

        let start_idx = sweep_start_ray_index[sweep_idx] as usize;
        let end_idx = sweep_end_ray_index[sweep_idx] as usize;

        let sweep_number = read_var_1d::<i32>(file, "sweep_number")?;
        let fixed_angle = read_var_1d::<f64>(file, "fixed_angle")?;
        let sweep_mode = read_var_1d_str(file, "sweep_mode")?;

        let metadata = SweepMetadata::new(
            sweep_number[sweep_idx] as u32,
            parse_sweep_mode(&sweep_mode[sweep_idx]),
            fixed_angle[sweep_idx],
        );

        let time = read_var_1d::<f64>(file, "time")?;
        let range = read_var_1d::<f32>(file, "range")?;
        let azimuth = read_var_1d::<f32>(file, "azimuth")?;
        let elevation = read_var_1d::<f32>(file, "elevation")?;

        let coordinates = Coordinates::new(
            time[start_idx..=end_idx].to_vec(),
            range.clone(),
            azimuth[start_idx..=end_idx].to_vec(),
            elevation[start_idx..=end_idx].to_vec(),
        );

        let mut moments = HashMap::new();
        let var_names: Vec<String> = file.variables().map(|v| v.name()).collect();

        for var_name in var_names {
            // Skip coordinate variables.
            if ["time", "range", "azimuth", "elevation"].contains(&var_name.as_str()) {
                continue;
            }
            if let Some(var) = file.variable(&var_name) {
                if var.dimensions().len() == 2 {
                    if let Ok(moment) =
                        self.read_moment(file, &var_name, start_idx, end_idx, range.len())
                    {
                        moments.insert(var_name, moment);
                    }
                }
            }
        }

        Ok(SweepData::new(metadata, moments, coordinates))
    }

    /// Read a moment variable
    fn read_moment(
        &self,
        file: &netcdf::File,
        var_name: &str,
        start_ray: usize,
        end_ray: usize,
        num_gates: usize,
    ) -> Result<MomentData> {
        let var = file
            .variable(var_name)
            .ok_or_else(|| RadishError::MissingVariable(var_name.to_string()))?;

        let num_rays = end_ray - start_ray + 1;

        // netcdf 0.12 takes ranges instead of (start, count) tuples and returns a Result.
        let data_raw: Vec<f32> = var
            .get_values((start_ray..start_ray + num_rays, 0..num_gates))
            .map_err(RadishError::NetCdf)?;

        let data = Array2::from_shape_vec((num_rays, num_gates), data_raw)
            .map_err(|e| RadishError::Conversion(e.to_string()))?;

        let units = var
            .attribute("units")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttributeValue::Str(s) => Some(s),
                netcdf::AttributeValue::Uchar(u) => Some(u.to_string()),
                netcdf::AttributeValue::Uchars(u) => Some(String::from_utf8_lossy(&u).to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "unknown".to_string());

        let fill_value = var
            .attribute("_FillValue")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttributeValue::Float(f) => Some(f),
                _ => None,
            });

        let scale_factor = var
            .attribute("scale_factor")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttributeValue::Float(f) => Some(f),
                _ => None,
            });

        let add_offset = var
            .attribute("add_offset")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttributeValue::Float(f) => Some(f),
                _ => None,
            });

        let standard_name = var
            .attribute("standard_name")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttributeValue::Str(s) => Some(s),
                _ => None,
            });

        let long_name = var
            .attribute("long_name")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttributeValue::Str(s) => Some(s),
                _ => None,
            });

        let mut moment = MomentData::new(var_name.to_string(), units, data);
        moment.fill_value = fill_value;
        moment.scale_factor = scale_factor;
        moment.add_offset = add_offset;
        moment.standard_name = standard_name;
        moment.long_name = long_name;

        Ok(moment)
    }
}

impl RadarBackend for CfRadial1Backend {
    fn name(&self) -> &str {
        "cfradial1"
    }

    fn description(&self) -> &str {
        "CF/Radial NetCDF format (version 1)"
    }

    fn supported_extensions(&self) -> &[&str] {
        &["nc", "nc4", "netcdf"]
    }

    /// Sniff a byte-prefix for HDF5 / netCDF-3 magic. Used by the central
    /// `auto_backend_for_bytes` dispatcher. The CfRadial1 reader itself
    /// can't decode in-memory buffers (libnetcdf needs a filename), so
    /// `read_bytes_volume` keeps the trait default that returns
    /// `RadishError::Unsupported`.
    fn can_read_bytes(&self, head: &[u8]) -> bool {
        sniff::looks_like_cfradial1(head)
    }

    fn scan_file(&self, path: &Path) -> Result<VolumeMetadata> {
        let file = netcdf::open(path)?;
        self.read_volume_metadata(&file)
    }

    fn read_sweep(&self, path: &Path, sweep_idx: usize) -> Result<SweepData> {
        let file = netcdf::open(path)?;
        self.read_sweep_data(&file, sweep_idx)
    }

    fn read_volume(&self, path: &Path) -> Result<VolumeData> {
        let file = netcdf::open(path)?;

        let metadata = self.read_volume_metadata(&file)?;
        let num_sweeps = metadata.sweep_group_names.len();

        let mut sweeps = Vec::with_capacity(num_sweeps);
        for i in 0..num_sweeps {
            let sweep = self.read_sweep_data(&file, i)?;
            sweeps.push(sweep);
        }

        Ok(VolumeData::new(metadata, sweeps))
    }
}

impl Default for CfRadial1Backend {
    fn default() -> Self {
        Self::new()
    }
}
