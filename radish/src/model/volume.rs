/// Volume-level data structures

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use radish_types::PlatformType;

use super::{SweepData, SweepMetadata};

/// Complete radar volume data
#[derive(Debug, Clone)]
pub struct VolumeData {
    /// Volume metadata
    pub metadata: VolumeMetadata,
    /// Sweep data
    pub sweeps: Vec<SweepData>,
    /// Optional radar calibration data
    pub calibration: Option<RadarCalibration>,
}

impl VolumeData {
    /// Create a new VolumeData
    pub fn new(metadata: VolumeMetadata, sweeps: Vec<SweepData>) -> Self {
        Self {
            metadata,
            sweeps,
            calibration: None,
        }
    }

    /// Get a specific sweep by index
    pub fn get_sweep(&self, index: usize) -> Option<&SweepData> {
        self.sweeps.get(index)
    }

    /// Get number of sweeps
    pub fn num_sweeps(&self) -> usize {
        self.sweeps.len()
    }

    /// Filter moments across all sweeps
    pub fn filter_moments(&mut self, moment_names: &[&str]) {
        for sweep in &mut self.sweeps {
            sweep.filter_moments(moment_names);
        }
    }
}

/// Metadata for a radar volume
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeMetadata {
    /// Volume number
    pub volume_number: u32,

    /// Instrument name (e.g., "SPOL", "NEXRAD")
    pub instrument_name: String,

    /// Institution operating the radar
    pub institution: String,

    /// Platform type
    pub platform_type: Option<PlatformType>,

    /// Site name
    pub site_name: Option<String>,

    /// Radar latitude (degrees North)
    pub latitude: f64,

    /// Radar longitude (degrees East)
    pub longitude: f64,

    /// Radar altitude above MSL (meters)
    pub altitude: f64,

    /// Radar altitude above ground level (meters)
    pub altitude_agl: Option<f64>,

    /// Time coverage start
    pub time_coverage_start: DateTime<Utc>,

    /// Time coverage end
    pub time_coverage_end: DateTime<Utc>,

    /// Sweep group names (e.g., ["sweep_0", "sweep_1", ...])
    pub sweep_group_names: Vec<String>,

    /// Fixed angles for each sweep (degrees)
    pub sweep_fixed_angles: Vec<f64>,

    /// Radar frequency (Hz)
    pub frequency: Option<f64>,

    /// Additional attributes
    pub attributes: std::collections::HashMap<String, String>,
}

impl VolumeMetadata {
    /// Create a new VolumeMetadata with required fields
    pub fn new(
        instrument_name: String,
        latitude: f64,
        longitude: f64,
        altitude: f64,
        time_coverage_start: DateTime<Utc>,
        time_coverage_end: DateTime<Utc>,
    ) -> Self {
        Self {
            volume_number: 0,
            instrument_name,
            institution: String::new(),
            platform_type: None,
            site_name: None,
            latitude,
            longitude,
            altitude,
            altitude_agl: None,
            time_coverage_start,
            time_coverage_end,
            sweep_group_names: Vec::new(),
            sweep_fixed_angles: Vec::new(),
            frequency: None,
            attributes: std::collections::HashMap::new(),
        }
    }

    /// Generate sweep group names based on number of sweeps
    pub fn generate_sweep_names(&mut self, num_sweeps: usize) {
        self.sweep_group_names = (0..num_sweeps)
            .map(|i| format!("sweep_{}", i))
            .collect();
    }
}

/// Radar calibration data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RadarCalibration {
    /// Calibration time
    pub time: Option<DateTime<Utc>>,

    /// Pulse width (microseconds)
    pub pulse_width: Option<f64>,

    /// Transmit power (horizontal, dBm)
    pub xmit_power_h: Option<f64>,

    /// Transmit power (vertical, dBm)
    pub xmit_power_v: Option<f64>,

    /// Two-way waveguide loss (horizontal, dB)
    pub two_way_waveguide_loss_h: Option<f64>,

    /// Two-way waveguide loss (vertical, dB)
    pub two_way_waveguide_loss_v: Option<f64>,

    /// Two-way radome loss (horizontal, dB)
    pub two_way_radome_loss_h: Option<f64>,

    /// Two-way radome loss (vertical, dB)
    pub two_way_radome_loss_v: Option<f64>,

    /// Receiver gain (horizontal, dB)
    pub receiver_gain_h: Option<f64>,

    /// Receiver gain (vertical, dB)
    pub receiver_gain_v: Option<f64>,

    /// Base 1km calibration for reflectivity (horizontal, dBZ)
    pub base_dbz_1km_h: Option<f64>,

    /// Base 1km calibration for reflectivity (vertical, dBZ)
    pub base_dbz_1km_v: Option<f64>,

    /// Sun power (horizontal, dBm)
    pub sun_power_h: Option<f64>,

    /// Sun power (vertical, dBm)
    pub sun_power_v: Option<f64>,

    /// Noise power (horizontal, dBm)
    pub noise_power_h: Option<f64>,

    /// Noise power (vertical, dBm)
    pub noise_power_v: Option<f64>,

    /// Receiver slope (horizontal)
    pub receiver_slope_h: Option<f64>,

    /// Receiver slope (vertical)
    pub receiver_slope_v: Option<f64>,

    /// Dynamic range (horizontal, dB)
    pub dynamic_range_h: Option<f64>,

    /// Dynamic range (vertical, dB)
    pub dynamic_range_v: Option<f64>,

    /// ZDR correction (dB)
    pub zdr_correction: Option<f64>,

    /// LDR correction (horizontal, dB)
    pub ldr_correction_h: Option<f64>,

    /// LDR correction (vertical, dB)
    pub ldr_correction_v: Option<f64>,

    /// System PHIDP (degrees)
    pub system_phidp: Option<f64>,
}

impl Default for RadarCalibration {
    fn default() -> Self {
        Self {
            time: None,
            pulse_width: None,
            xmit_power_h: None,
            xmit_power_v: None,
            two_way_waveguide_loss_h: None,
            two_way_waveguide_loss_v: None,
            two_way_radome_loss_h: None,
            two_way_radome_loss_v: None,
            receiver_gain_h: None,
            receiver_gain_v: None,
            base_dbz_1km_h: None,
            base_dbz_1km_v: None,
            sun_power_h: None,
            sun_power_v: None,
            noise_power_h: None,
            noise_power_v: None,
            receiver_slope_h: None,
            receiver_slope_v: None,
            dynamic_range_h: None,
            dynamic_range_v: None,
            zdr_correction: None,
            ldr_correction_h: None,
            ldr_correction_v: None,
            system_phidp: None,
        }
    }
}
