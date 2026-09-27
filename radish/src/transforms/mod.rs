//! Transformation utilities for radar data.
//!
//! This module will contain functions for:
//! - Georeferencing (converting polar to geographic coordinates)
//! - Velocity dealiasing
//! - Quality control and filtering
//! - Attenuation correction
//! - KDP calculation
//!
//! To be implemented in future phases.

pub mod dealias;
pub mod georeference;

pub use dealias::{dealias_region_based, DealiasOptions};
pub use georeference::*;
