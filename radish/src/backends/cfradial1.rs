/// CfRadial1 backend for reading CF/Radial NetCDF files

use std::path::Path;
use chrono::{DateTime, Utc, TimeZone};
use ndarray::Array2;
use std::collections::HashMap;

use crate::{
    Result, RadishError,
    VolumeData, VolumeMetadata, SweepData, SweepMetadata, MomentData, Coordinates,
    backends::RadarBackend,
};
use radish_types::{SweepMode, PlatformType};

/// Backend for reading CfRadial1 format (CF/Radial NetCDF)
pub struct CfRadial1Backend;

impl CfRadial1Backend {
    /// Create a new CfRadial1Backend
    pub fn new() -> Self {
        Self
    }

    /// Read volume metadata from NetCDF file
    fn read_volume_metadata(&self, file: &netcdf::File) -> Result<VolumeMetadata> {
        // Read required global attributes
        let instrument_name = read_string_attr(file, "instrument_name")
            .unwrap_or_else(|| "unknown".to_string());
        let institution = read_string_attr(file, "institution")
            .unwrap_or_else(|| "unknown".to_string());

        // Read location
        let latitude = read_scalar_var::<f64>(file, "latitude")?;
        let longitude = read_scalar_var::<f64>(file, "longitude")?;
        let altitude = read_scalar_var::<f64>(file, "altitude")?;
        let altitude_agl = read_scalar_var::<f64>(file, "altitude_agl").ok();

        // Read time coverage
        let time_coverage_start = read_string_attr(file, "time_coverage_start")
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .ok_or_else(|| RadishError::MissingAttribute("time_coverage_start".to_string()))?;

        let time_coverage_end = read_string_attr(file, "time_coverage_end")
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|dt| dt.with_timezone(&Utc))
            .ok_or_else(|| RadishError::MissingAttribute("time_coverage_end".to_string()))?;

        // Read sweep information
        let sweep_number = read_var_1d::<i32>(file, "sweep_number")?;
        let sweep_fixed_angle = read_var_1d::<f64>(file, "fixed_angle")?;

        let num_sweeps = sweep_number.len();
        let sweep_group_names: Vec<String> = (0..num_sweeps)
            .map(|i| format!("sweep_{}", i))
            .collect();

        // Optional fields
        let volume_number = read_scalar_var::<u32>(file, "volume_number").unwrap_or(0);
        let frequency = read_scalar_var::<f64>(file, "frequency").ok();
        let platform_type = read_string_attr(file, "platform_type")
            .and_then(|s| parse_platform_type(&s));

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
        // Read sweep start/end indices
        let sweep_start_ray_index = read_var_1d::<i32>(file, "sweep_start_ray_index")?;
        let sweep_end_ray_index = read_var_1d::<i32>(file, "sweep_end_ray_index")?;

        if sweep_idx >= sweep_start_ray_index.len() {
            return Err(RadishError::InvalidSweepIndex(sweep_idx));
        }

        let start_idx = sweep_start_ray_index[sweep_idx] as usize;
        let end_idx = sweep_end_ray_index[sweep_idx] as usize;
        let num_rays = end_idx - start_idx + 1;

        // Read sweep metadata
        let sweep_number = read_var_1d::<i32>(file, "sweep_number")?;
        let fixed_angle = read_var_1d::<f64>(file, "fixed_angle")?;
        let sweep_mode = read_var_1d_str(file, "sweep_mode")?;

        let metadata = SweepMetadata::new(
            sweep_number[sweep_idx] as u32,
            parse_sweep_mode(&sweep_mode[sweep_idx]),
            fixed_angle[sweep_idx],
        );

        // Read coordinates
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

        // Read moment data
        let mut moments = HashMap::new();

        // Get list of variables
        let var_names = file.variables()
            .map(|v| v.name())
            .collect::<Vec<_>>();

        for var_name in var_names {
            // Skip coordinate variables
            if ["time", "range", "azimuth", "elevation"].contains(&var_name.as_str()) {
                continue;
            }

            if let Ok(var) = file.variable(&var_name) {
                // Check if it's a 2D moment variable [time, range]
                if var.dimensions().len() == 2 {
                    if let Ok(moment) = self.read_moment(file, &var_name, start_idx, end_idx, range.len()) {
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
        let var = file.variable(var_name)
            .ok_or_else(|| RadishError::MissingVariable(var_name.to_string()))?;

        let num_rays = end_ray - start_ray + 1;

        // Read data for this sweep
        let data_raw: Vec<f32> = var.get((start_ray, 0), (num_rays, num_gates))
            .map_err(|e| RadishError::NetCdf(e))?;

        let data = Array2::from_shape_vec((num_rays, num_gates), data_raw)
            .map_err(|e| RadishError::Conversion(e.to_string()))?;

        // Read attributes
        let units = var.attribute("units")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttrValue::Str(s) => Some(s),
                netcdf::AttrValue::Uchar(u) => Some(String::from_utf8_lossy(&u).to_string()),
                _ => None,
            })
            .unwrap_or_else(|| "unknown".to_string());

        let fill_value = var.attribute("_FillValue")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttrValue::Float(f) => Some(f),
                _ => None,
            });

        let scale_factor = var.attribute("scale_factor")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttrValue::Float(f) => Some(f),
                _ => None,
            });

        let add_offset = var.attribute("add_offset")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttrValue::Float(f) => Some(f),
                _ => None,
            });

        let standard_name = var.attribute("standard_name")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttrValue::Str(s) => Some(s),
                _ => None,
            });

        let long_name = var.attribute("long_name")
            .and_then(|a| a.value().ok())
            .and_then(|v| match v {
                netcdf::AttrValue::Str(s) => Some(s),
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

        // Read metadata
        let metadata = self.read_volume_metadata(&file)?;
        let num_sweeps = metadata.sweep_group_names.len();

        // Read all sweeps
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

// Helper functions

fn read_string_attr(file: &netcdf::File, name: &str) -> Option<String> {
    file.attribute(name)
        .and_then(|a| a.value().ok())
        .and_then(|v| match v {
            netcdf::AttrValue::Str(s) => Some(s),
            netcdf::AttrValue::Uchar(u) => Some(String::from_utf8_lossy(&u).to_string()),
            _ => None,
        })
}

fn read_scalar_var<T: netcdf::Numeric>(file: &netcdf::File, name: &str) -> Result<T> {
    let var = file.variable(name)
        .ok_or_else(|| RadishError::MissingVariable(name.to_string()))?;

    let value: T = var.get((0,))
        .map_err(|e| RadishError::NetCdf(e))?;

    Ok(value)
}

fn read_var_1d<T: netcdf::Numeric>(file: &netcdf::File, name: &str) -> Result<Vec<T>> {
    let var = file.variable(name)
        .ok_or_else(|| RadishError::MissingVariable(name.to_string()))?;

    let data: Vec<T> = var.get(..)
        .map_err(|e| RadishError::NetCdf(e))?;

    Ok(data)
}

fn read_var_1d_str(file: &netcdf::File, name: &str) -> Result<Vec<String>> {
    let var = file.variable(name)
        .ok_or_else(|| RadishError::MissingVariable(name.to_string()))?;

    // For string variables in NetCDF, we need to handle them carefully
    // This is a simplified version - you may need to adjust based on how strings are stored
    let dims = var.dimensions();
    if dims.is_empty() {
        return Ok(vec![]);
    }

    let len = dims[0].len();
    let mut result = Vec::with_capacity(len);

    for i in 0..len {
        // Try to read as string - this may need adjustment based on actual file format
        if let Ok(s) = var.get_string((i,)) {
            result.push(s);
        } else {
            result.push("unknown".to_string());
        }
    }

    Ok(result)
}

fn parse_sweep_mode(mode_str: &str) -> SweepMode {
    match mode_str.to_lowercase().as_str() {
        "azimuth_surveillance" | "ppi" | "sur" => SweepMode::Azimuth,
        "elevation_surveillance" | "rhi" => SweepMode::Elevation,
        "sector" | "sec" => SweepMode::Sector,
        "pointing" | "pnt" => SweepMode::Pointing,
        "vertical_pointing" | "vert" => SweepMode::VerticalPointing,
        "calibration" | "cal" => SweepMode::Calibration,
        _ => SweepMode::Azimuth, // default
    }
}

fn parse_platform_type(type_str: &str) -> Option<PlatformType> {
    match type_str.to_lowercase().as_str() {
        "fixed" => Some(PlatformType::Fixed),
        "vehicle" => Some(PlatformType::Vehicle),
        "ship" => Some(PlatformType::Ship),
        "aircraft" => Some(PlatformType::Aircraft),
        "satellite" => Some(PlatformType::Satellite),
        _ => None,
    }
}
