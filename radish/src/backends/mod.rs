/// Backend system for reading different radar formats

use std::path::Path;
use crate::{Result, VolumeData, VolumeMetadata, SweepData};

pub mod cfradial1;

pub use cfradial1::CfRadial1Backend;

/// Trait for radar file format backends
///
/// Each backend implements parsing for a specific file format (CfRadial1, IRIS, etc.)
/// and normalizes the data to the common data model.
pub trait RadarBackend: Send + Sync {
    /// Backend name (e.g., "cfradial1", "iris", "nexrad")
    fn name(&self) -> &str;

    /// Backend description
    fn description(&self) -> &str;

    /// Supported file extensions (e.g., &["nc", "nc4"])
    fn supported_extensions(&self) -> &[&str];

    /// Scan file to extract volume metadata without reading all data
    ///
    /// This is useful for quickly determining what's in a file before
    /// committing to reading the full volume.
    fn scan_file(&self, path: &Path) -> Result<VolumeMetadata>;

    /// Read a specific sweep from the file
    ///
    /// This allows lazy loading of sweep data.
    fn read_sweep(&self, path: &Path, sweep_idx: usize) -> Result<SweepData>;

    /// Read the entire volume including all sweeps
    ///
    /// This is the primary method for loading radar data.
    fn read_volume(&self, path: &Path) -> Result<VolumeData>;

    /// Check if this backend can read the given file
    ///
    /// Default implementation checks file extension.
    fn can_read(&self, path: &Path) -> bool {
        if let Some(ext) = path.extension() {
            if let Some(ext_str) = ext.to_str() {
                return self.supported_extensions().contains(&ext_str);
            }
        }
        false
    }
}

/// Get all available backends
pub fn available_backends() -> Vec<Box<dyn RadarBackend>> {
    vec![
        Box::new(CfRadial1Backend::new()),
        // Add more backends here as they're implemented
    ]
}

/// Automatically select the appropriate backend for a file
pub fn auto_backend(path: &Path) -> Result<Box<dyn RadarBackend>> {
    for backend in available_backends() {
        if backend.can_read(path) {
            return Ok(backend);
        }
    }

    Err(crate::RadishError::InvalidFormat(format!(
        "No backend found for file: {}",
        path.display()
    )))
}
