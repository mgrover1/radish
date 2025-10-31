/// Radish: High-performance weather radar data library
///
/// This library provides fast, memory-efficient reading of multiple weather radar
/// formats with a unified interface, normalizing to the CfRadial2/FM301 standard.

pub mod error;
pub mod model;
pub mod backends;
pub mod io;
pub mod transforms;

// Re-export commonly used types
pub use error::{RadishError, Result};
pub use model::{VolumeData, VolumeMetadata, SweepData, SweepMetadata, MomentData, Coordinates};
pub use backends::RadarBackend;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic() {
        // Basic smoke test
        assert_eq!(2 + 2, 4);
    }
}
