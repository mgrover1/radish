/// Sweep-level data structures

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use radish_types::{SweepMode, FollowMode, PrtMode};

use super::{MomentData, Coordinates};

/// Sweep data containing moments and coordinates
#[derive(Debug, Clone)]
pub struct SweepData {
    /// Sweep metadata
    pub metadata: SweepMetadata,
    /// Moment data (e.g., "DBZH", "VRADH")
    pub moments: HashMap<String, MomentData>,
    /// Coordinate data
    pub coordinates: Coordinates,
}

impl SweepData {
    /// Create a new SweepData
    pub fn new(
        metadata: SweepMetadata,
        moments: HashMap<String, MomentData>,
        coordinates: Coordinates,
    ) -> Self {
        Self {
            metadata,
            moments,
            coordinates,
        }
    }

    /// Get a specific moment by name
    pub fn get_moment(&self, name: &str) -> Option<&MomentData> {
        self.moments.get(name)
    }

    /// Get a mutable reference to a specific moment
    pub fn get_moment_mut(&mut self, name: &str) -> Option<&mut MomentData> {
        self.moments.get_mut(name)
    }

    /// Get list of available moment names
    pub fn moment_names(&self) -> Vec<&String> {
        self.moments.keys().collect()
    }

    /// Filter moments to keep only specified names
    pub fn filter_moments(&mut self, moment_names: &[&str]) {
        self.moments.retain(|k, _| moment_names.contains(&k.as_str()));
    }

    /// Number of rays in this sweep
    pub fn num_rays(&self) -> usize {
        self.coordinates.azimuth.len()
    }

    /// Number of gates (range bins) in this sweep
    pub fn num_gates(&self) -> usize {
        self.coordinates.range.len()
    }
}

/// Metadata for a single sweep
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SweepMetadata {
    /// Sweep number within volume
    pub sweep_number: u32,

    /// Sweep mode
    pub sweep_mode: SweepMode,

    /// Follow mode
    pub follow_mode: Option<FollowMode>,

    /// PRT mode
    pub prt_mode: Option<PrtMode>,

    /// Fixed angle for this sweep (degrees)
    pub fixed_angle: f64,

    /// Target scan rate (degrees/second)
    pub target_scan_rate: Option<f64>,

    /// Rays are indexed in increasing angle order
    pub rays_are_indexed: Option<bool>,

    /// Ray angle resolution (degrees)
    pub ray_angle_resolution: Option<f64>,

    /// Polarization mode
    pub polarization_mode: Option<String>,

    /// Pulse Repetition Frequency (Hz)
    pub prf: Option<f64>,

    /// Nyquist velocity (m/s)
    pub nyquist_velocity: Option<f64>,

    /// Unambiguous range (m)
    pub unambiguous_range: Option<f64>,
}

impl SweepMetadata {
    /// Create a new SweepMetadata with required fields
    pub fn new(sweep_number: u32, sweep_mode: SweepMode, fixed_angle: f64) -> Self {
        Self {
            sweep_number,
            sweep_mode,
            follow_mode: None,
            prt_mode: None,
            fixed_angle,
            target_scan_rate: None,
            rays_are_indexed: None,
            ray_angle_resolution: None,
            polarization_mode: None,
            prf: None,
            nyquist_velocity: None,
            unambiguous_range: None,
        }
    }
}
