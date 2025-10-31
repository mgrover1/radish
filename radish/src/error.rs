/// Error types for the radish library

use thiserror::Error;

/// Result type alias for radish operations
pub type Result<T> = std::result::Result<T, RadishError>;

/// Main error type for radish operations
#[derive(Error, Debug)]
pub enum RadishError {
    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// HDF5 error
    #[error("HDF5 error: {0}")]
    Hdf5(#[from] hdf5::Error),

    /// NetCDF error
    #[error("NetCDF error: {0}")]
    NetCdf(#[from] netcdf::Error),

    /// File format error
    #[error("Invalid file format: {0}")]
    InvalidFormat(String),

    /// Missing required attribute
    #[error("Missing required attribute: {0}")]
    MissingAttribute(String),

    /// Missing required variable
    #[error("Missing required variable: {0}")]
    MissingVariable(String),

    /// Invalid sweep index
    #[error("Invalid sweep index: {0}")]
    InvalidSweepIndex(usize),

    /// Data conversion error
    #[error("Data conversion error: {0}")]
    Conversion(String),

    /// Unsupported feature
    #[error("Unsupported feature: {0}")]
    Unsupported(String),

    /// General error
    #[error("Error: {0}")]
    General(String),
}

impl From<String> for RadishError {
    fn from(s: String) -> Self {
        RadishError::General(s)
    }
}

impl From<&str> for RadishError {
    fn from(s: &str) -> Self {
        RadishError::General(s.to_string())
    }
}
