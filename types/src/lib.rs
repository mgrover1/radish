/// Common types and constants shared across the radish ecosystem

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
