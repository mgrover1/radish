//! Radish: high-performance weather-radar data library.
//!
//! Reads multiple radar formats (CfRadial1, NEXRAD Level 2, Sigmet/IRIS RAW)
//! and normalises them to the CfRadial2/FM301 data model: a [`VolumeData`]
//! containing per-sweep [`SweepData`] with named [`MomentData`] arrays and
//! a shared [`Coordinates`] axis.
//!
//! # Architecture
//!
//! Every format reader implements the [`backends::RadarBackend`] trait and
//! registers itself in [`backends::available_backends`]. The
//! [`backends::auto_backend`] / [`backends::auto_backend_for_bytes`]
//! dispatchers walk that list and pick the first reader whose
//! `can_read` / `can_read_bytes` returns true — so adding a new format
//! never requires editing the dispatcher.
//!
//! # Quick start
//!
//! ```no_run
//! use std::path::Path;
//! use radish::backends::{auto_backend, RadarBackend};
//!
//! let backend = auto_backend(Path::new("path/to/file.RAW"))?;
//! let volume = backend.read_volume(Path::new("path/to/file.RAW"))?;
//! println!("{} sweeps, instrument = {}",
//!          volume.num_sweeps(), volume.metadata.instrument_name);
//! # Ok::<_, radish::RadishError>(())
//! ```
//!
//! # Backend-specific typed attrs
//!
//! Each format that surfaces extras beyond the core FM301 fields exposes
//! them as a typed `Option<…Attrs>` slot on [`VolumeMetadata`] /
//! [`SweepMetadata`]:
//!
//! * NEXRAD: [`NexradVolumeAttrs`] (MSG_2 / MSG_5 RDA + VCP fields) and
//!   [`NexradSweepAttrs`] (per-elevation-cut waveform / SAILS / MRLE flags).
//! * Sigmet: [`SigmetVolumeAttrs`] (TASK_CONFIGURATION + INGEST_HEADER:
//!   PRF, Nyquist, scan mode, IRIS firmware) and [`SigmetSweepAttrs`]
//!   (per-sweep mode + fixed angle).
//! * NEXRAD Level 3 (NIDS): [`NidsSweepAttrs`] (AWIPS id, message code,
//!   VCP, tilt ordinal) — a single already-scanned tilt has no
//!   volume-level counterpart, so there is no `NidsVolumeAttrs`.
//!
//! # Errors
//!
//! Every fallible operation returns [`Result<T>`] (alias for
//! `Result<T, RadishError>`). The [`RadishError`] enum covers I/O,
//! HDF5/NetCDF, format-validation, and conversion errors.

pub mod backends;
pub mod error;
pub mod model;
pub mod transforms;

// Re-export commonly used types
pub use backends::RadarBackend;
pub use error::{RadishError, Result};
pub use model::{
    Coordinates, DeclaredScale, MomentData, NexradSweepAttrs, NexradVolumeAttrs, NidsSweepAttrs,
    SigmetSweepAttrs, SigmetVolumeAttrs, SweepData, SweepMetadata, VolumeData, VolumeMetadata,
};

#[cfg(test)]
mod tests {
    #[test]
    fn test_basic() {
        // Basic smoke test
        assert_eq!(2 + 2, 4);
    }
}
