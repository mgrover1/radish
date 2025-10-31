/// Moment (radar variable) data structures

use ndarray::Array2;
use serde::{Deserialize, Serialize};

/// Radar moment data (e.g., reflectivity, velocity)
#[derive(Debug, Clone)]
pub struct MomentData {
    /// Variable name (e.g., "DBZH", "VRADH")
    pub name: String,

    /// CF standard name
    pub standard_name: Option<String>,

    /// Long descriptive name
    pub long_name: Option<String>,

    /// Units
    pub units: String,

    /// 2D data array [rays × gates]
    pub data: Array2<f32>,

    /// Fill value (missing data indicator)
    pub fill_value: Option<f32>,

    /// Scale factor
    pub scale_factor: Option<f32>,

    /// Add offset
    pub add_offset: Option<f32>,

    /// Valid minimum
    pub valid_min: Option<f32>,

    /// Valid maximum
    pub valid_max: Option<f32>,

    /// Coordinates this variable depends on
    pub coordinates: Option<String>,

    /// Additional attributes
    pub attributes: std::collections::HashMap<String, String>,
}

impl MomentData {
    /// Create a new MomentData
    pub fn new(
        name: String,
        units: String,
        data: Array2<f32>,
    ) -> Self {
        Self {
            name,
            standard_name: None,
            long_name: None,
            units,
            data,
            fill_value: None,
            scale_factor: None,
            add_offset: None,
            valid_min: None,
            valid_max: None,
            coordinates: None,
            attributes: std::collections::HashMap::new(),
        }
    }

    /// Get the shape of the data array
    pub fn shape(&self) -> (usize, usize) {
        let shape = self.data.shape();
        (shape[0], shape[1])
    }

    /// Apply scale and offset to get physical values
    pub fn apply_scale_offset(&mut self) {
        if let (Some(scale), Some(offset)) = (self.scale_factor, self.add_offset) {
            self.data.mapv_inplace(|v| {
                if let Some(fill) = self.fill_value {
                    if v == fill {
                        return v;
                    }
                }
                v * scale + offset
            });
            self.scale_factor = None;
            self.add_offset = None;
        }
    }

    /// Mask invalid values
    pub fn mask_invalid(&mut self, mask_value: f32) {
        if let Some(fill) = self.fill_value {
            self.data.mapv_inplace(|v| {
                if v == fill {
                    mask_value
                } else {
                    v
                }
            });
        }

        if let (Some(min), Some(max)) = (self.valid_min, self.valid_max) {
            self.data.mapv_inplace(|v| {
                if v < min || v > max {
                    mask_value
                } else {
                    v
                }
            });
        }
    }
}

/// Standard moment metadata based on CfRadial2 conventions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentMetadata {
    /// Short name
    pub name: &'static str,
    /// CF standard name
    pub standard_name: &'static str,
    /// Long descriptive name
    pub long_name: &'static str,
    /// Units
    pub units: &'static str,
}

impl MomentMetadata {
    /// Get metadata for a standard moment name
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "DBZH" | "DBZ" | "reflectivity" => Some(Self {
                name: "DBZH",
                standard_name: "equivalent_reflectivity_factor",
                long_name: "Equivalent reflectivity factor (horizontal channel)",
                units: "dBZ",
            }),
            "VRADH" | "VEL" | "velocity" => Some(Self {
                name: "VRADH",
                standard_name: "radial_velocity_of_scatterers_away_from_instrument",
                long_name: "Radial velocity (horizontal channel)",
                units: "m/s",
            }),
            "WRADH" | "WIDTH" | "spectrum_width" => Some(Self {
                name: "WRADH",
                standard_name: "doppler_spectrum_width",
                long_name: "Doppler spectrum width (horizontal channel)",
                units: "m/s",
            }),
            "ZDR" => Some(Self {
                name: "ZDR",
                standard_name: "differential_reflectivity_hv",
                long_name: "Differential reflectivity",
                units: "dB",
            }),
            "PHIDP" => Some(Self {
                name: "PHIDP",
                standard_name: "differential_phase_hv",
                long_name: "Differential propagation phase",
                units: "degrees",
            }),
            "KDP" => Some(Self {
                name: "KDP",
                standard_name: "specific_differential_phase_hv",
                long_name: "Specific differential phase",
                units: "degrees/km",
            }),
            "RHOHV" => Some(Self {
                name: "RHOHV",
                standard_name: "cross_correlation_ratio_hv",
                long_name: "Cross-correlation coefficient",
                units: "",
            }),
            "NCP" => Some(Self {
                name: "NCP",
                standard_name: "normalized_coherent_power",
                long_name: "Normalized coherent power",
                units: "",
            }),
            "SNRH" | "SNR" => Some(Self {
                name: "SNRH",
                standard_name: "signal_to_noise_ratio",
                long_name: "Signal-to-noise ratio (horizontal channel)",
                units: "dB",
            }),
            _ => None,
        }
    }
}
