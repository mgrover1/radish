/// Transformation utilities for radar data
///
/// This module will contain functions for:
/// - Georeferencing (converting polar to geographic coordinates)
/// - Velocity dealiasing
/// - Quality control and filtering
/// - Attenuation correction
/// - KDP calculation
///
/// To be implemented in future phases.

pub mod georeference;

pub use georeference::*;
