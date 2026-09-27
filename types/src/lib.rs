//! Common types and constants shared across the radish ecosystem.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Sweep mode enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SweepMode {
    /// Azimuth surveillance (PPI)
    Azimuth,
    /// Elevation surveillance (RHI)
    Elevation,
    /// Sector
    Sector,
    /// Coplane
    Coplane,
    /// Pointing
    Pointing,
    /// Manual PPI
    ManualPpi,
    /// Manual RHI
    ManualRhi,
    /// Idle
    Idle,
    /// Calibration
    Calibration,
    /// Vertical pointing
    VerticalPointing,
}

/// Follow mode enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FollowMode {
    /// None
    None,
    /// Sun
    Sun,
    /// Vehicle
    Vehicle,
    /// Aircraft
    Aircraft,
    /// Target
    Target,
    /// Manual
    Manual,
}

/// PRT (Pulse Repetition Time) mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrtMode {
    /// Fixed PRT
    Fixed,
    /// Staggered PRT 2/3
    Staggered2_3,
    /// Staggered PRT 3/4
    Staggered3_4,
    /// Staggered PRT 4/5
    Staggered4_5,
    /// Dual PRT
    Dual,
}

/// Platform type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlatformType {
    /// Fixed ground station
    Fixed,
    /// Mobile ground vehicle
    Vehicle,
    /// Ship
    Ship,
    /// Aircraft
    Aircraft,
    /// Satellite
    Satellite,
}

// `Display` impls below emit the WMO FM 301 / CfRadial2 spec strings. These
// are the canonical names that go into per-sweep `sweep_mode`/`prt_mode`/
// `follow_mode`/`platform_type` variables when serialising to xarray or
// netCDF. Keeping the conversion next to the type definitions avoids drift
// across backends.

impl fmt::Display for SweepMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            SweepMode::Azimuth => "azimuth_surveillance",
            SweepMode::Elevation => "elevation_surveillance",
            SweepMode::Sector => "sector",
            SweepMode::Coplane => "coplane",
            SweepMode::Pointing => "pointing",
            SweepMode::ManualPpi => "manual_ppi",
            SweepMode::ManualRhi => "manual_rhi",
            SweepMode::Idle => "idle",
            SweepMode::Calibration => "calibration",
            SweepMode::VerticalPointing => "vertical_pointing",
        };
        f.write_str(s)
    }
}

impl fmt::Display for FollowMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            FollowMode::None => "none",
            FollowMode::Sun => "sun",
            FollowMode::Vehicle => "vehicle",
            FollowMode::Aircraft => "aircraft",
            FollowMode::Target => "target",
            FollowMode::Manual => "manual",
        };
        f.write_str(s)
    }
}

impl fmt::Display for PrtMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // CfRadial2 enumerates `fixed`, `staggered`, `dual`. The detailed
        // staggered ratio lives in radar_parameters; keep it out of this label.
        let s = match self {
            PrtMode::Fixed => "fixed",
            PrtMode::Staggered2_3 | PrtMode::Staggered3_4 | PrtMode::Staggered4_5 => "staggered",
            PrtMode::Dual => "dual",
        };
        f.write_str(s)
    }
}

impl fmt::Display for PlatformType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PlatformType::Fixed => "fixed",
            PlatformType::Vehicle => "vehicle",
            PlatformType::Ship => "ship",
            PlatformType::Aircraft => "aircraft",
            PlatformType::Satellite => "satellite",
        };
        f.write_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_mode_strings_match_fm301() {
        assert_eq!(SweepMode::Azimuth.to_string(), "azimuth_surveillance");
        assert_eq!(SweepMode::Elevation.to_string(), "elevation_surveillance");
        assert_eq!(SweepMode::VerticalPointing.to_string(), "vertical_pointing");
    }

    #[test]
    fn prt_mode_collapses_staggered_variants_per_fm301() {
        assert_eq!(PrtMode::Fixed.to_string(), "fixed");
        assert_eq!(PrtMode::Staggered2_3.to_string(), "staggered");
        assert_eq!(PrtMode::Staggered3_4.to_string(), "staggered");
        assert_eq!(PrtMode::Staggered4_5.to_string(), "staggered");
        assert_eq!(PrtMode::Dual.to_string(), "dual");
    }

    #[test]
    fn follow_mode_lowercase() {
        assert_eq!(FollowMode::None.to_string(), "none");
        assert_eq!(FollowMode::Sun.to_string(), "sun");
    }

    #[test]
    fn platform_type_lowercase() {
        assert_eq!(PlatformType::Fixed.to_string(), "fixed");
    }
}

/// CfRadial2 standard moment names and metadata
pub mod moments {
    /// Reflectivity (Horizontal)
    pub const DBZH: &str = "DBZH";
    /// Reflectivity (Vertical)
    pub const DBZV: &str = "DBZV";
    /// Velocity (Horizontal)
    pub const VRADH: &str = "VRADH";
    /// Velocity (Vertical)
    pub const VRADV: &str = "VRADV";
    /// Spectrum Width (Horizontal)
    pub const WRADH: &str = "WRADH";
    /// Spectrum Width (Vertical)
    pub const WRADV: &str = "WRADV";
    /// Differential Reflectivity
    pub const ZDR: &str = "ZDR";
    /// Differential Phase
    pub const PHIDP: &str = "PHIDP";
    /// Specific Differential Phase
    pub const KDP: &str = "KDP";
    /// Cross-correlation Coefficient
    pub const RHOHV: &str = "RHOHV";
    /// Linear Depolarization Ratio (Horizontal)
    pub const LDRH: &str = "LDRH";
    /// Linear Depolarization Ratio (Vertical)
    pub const LDRV: &str = "LDRV";
    /// Signal-to-Noise Ratio (Horizontal)
    pub const SNRH: &str = "SNRH";
    /// Signal-to-Noise Ratio (Vertical)
    pub const SNRV: &str = "SNRV";
    /// Normalized Coherent Power
    pub const NCP: &str = "NCP";
}

/// CfRadial2 conventions version
pub const CFRADIAL2_VERSION: &str = "CfRadial-2.0";

/// CfRadial1 conventions version
pub const CFRADIAL1_VERSION: &str = "Cf/Radial";
