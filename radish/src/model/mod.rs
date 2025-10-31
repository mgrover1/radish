/// Data model for radar volumes, sweeps, and moments
///
/// This module defines the core data structures that represent weather radar data
/// in a format-agnostic way, following the CfRadial2/FM301 standard.

mod volume;
mod sweep;
mod moment;
mod coordinates;

pub use volume::{VolumeData, VolumeMetadata};
pub use sweep::{SweepData, SweepMetadata};
pub use moment::MomentData;
pub use coordinates::Coordinates;
