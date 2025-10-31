/// Python bindings for radish

use pyo3::prelude::*;
use pyo3::exceptions::PyRuntimeError;
use numpy::{PyArray2, ToPyArray};
use ndarray::Array2;
use std::path::PathBuf;

use radish::{
    backends::{RadarBackend, CfRadial1Backend},
    VolumeData as RustVolumeData,
    VolumeMetadata as RustVolumeMetadata,
    SweepData as RustSweepData,
    MomentData as RustMomentData,
};

/// Python wrapper for VolumeMetadata
#[pyclass(name = "VolumeMetadata")]
#[derive(Clone)]
pub struct PyVolumeMetadata {
    inner: RustVolumeMetadata,
}

#[pymethods]
impl PyVolumeMetadata {
    #[getter]
    fn instrument_name(&self) -> &str {
        &self.inner.instrument_name
    }

    #[getter]
    fn latitude(&self) -> f64 {
        self.inner.latitude
    }

    #[getter]
    fn longitude(&self) -> f64 {
        self.inner.longitude
    }

    #[getter]
    fn altitude(&self) -> f64 {
        self.inner.altitude
    }

    #[getter]
    fn sweep_fixed_angles(&self) -> Vec<f64> {
        self.inner.sweep_fixed_angles.clone()
    }

    #[getter]
    fn num_sweeps(&self) -> usize {
        self.inner.sweep_group_names.len()
    }

    fn __repr__(&self) -> String {
        format!(
            "VolumeMetadata(instrument='{}', lat={:.4}, lon={:.4}, alt={:.1}, sweeps={})",
            self.inner.instrument_name,
            self.inner.latitude,
            self.inner.longitude,
            self.inner.altitude,
            self.num_sweeps()
        )
    }
}

/// Python wrapper for MomentData
#[pyclass(name = "MomentData")]
pub struct PyMomentData {
    inner: RustMomentData,
}

#[pymethods]
impl PyMomentData {
    #[getter]
    fn name(&self) -> &str {
        &self.inner.name
    }

    #[getter]
    fn units(&self) -> &str {
        &self.inner.units
    }

    #[getter]
    fn shape(&self) -> (usize, usize) {
        self.inner.shape()
    }

    fn data<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyArray2<f32>>> {
        Ok(self.inner.data.to_pyarray_bound(py))
    }

    fn __repr__(&self) -> String {
        let (nrays, ngates) = self.shape();
        format!(
            "MomentData(name='{}', units='{}', shape=({}, {}))",
            self.name(),
            self.units(),
            nrays,
            ngates
        )
    }
}

/// Python wrapper for SweepData
#[pyclass(name = "SweepData")]
pub struct PySweepData {
    inner: RustSweepData,
}

#[pymethods]
impl PySweepData {
    #[getter]
    fn sweep_number(&self) -> u32 {
        self.inner.metadata.sweep_number
    }

    #[getter]
    fn fixed_angle(&self) -> f64 {
        self.inner.metadata.fixed_angle
    }

    #[getter]
    fn num_rays(&self) -> usize {
        self.inner.num_rays()
    }

    #[getter]
    fn num_gates(&self) -> usize {
        self.inner.num_gates()
    }

    fn moment_names(&self) -> Vec<String> {
        self.inner.moment_names().into_iter().cloned().collect()
    }

    fn get_moment(&self, name: &str) -> Option<PyMomentData> {
        self.inner.get_moment(name).map(|m| PyMomentData {
            inner: m.clone(),
        })
    }

    #[getter]
    fn azimuth(&self) -> Vec<f32> {
        self.inner.coordinates.azimuth.clone()
    }

    #[getter]
    fn elevation(&self) -> Vec<f32> {
        self.inner.coordinates.elevation.clone()
    }

    #[getter]
    fn range(&self) -> Vec<f32> {
        self.inner.coordinates.range.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "SweepData(sweep={}, angle={:.2}°, rays={}, gates={}, moments={})",
            self.sweep_number(),
            self.fixed_angle(),
            self.num_rays(),
            self.num_gates(),
            self.inner.moments.len()
        )
    }
}

/// Python wrapper for VolumeData
#[pyclass(name = "VolumeData")]
pub struct PyVolumeData {
    inner: RustVolumeData,
}

#[pymethods]
impl PyVolumeData {
    #[getter]
    fn metadata(&self) -> PyVolumeMetadata {
        PyVolumeMetadata {
            inner: self.inner.metadata.clone(),
        }
    }

    #[getter]
    fn num_sweeps(&self) -> usize {
        self.inner.num_sweeps()
    }

    fn get_sweep(&self, index: usize) -> PyResult<PySweepData> {
        self.inner
            .get_sweep(index)
            .map(|s| PySweepData {
                inner: s.clone(),
            })
            .ok_or_else(|| PyRuntimeError::new_err(format!("Invalid sweep index: {}", index)))
    }

    fn __repr__(&self) -> String {
        format!(
            "VolumeData(instrument='{}', sweeps={})",
            self.inner.metadata.instrument_name,
            self.num_sweeps()
        )
    }
}

/// Read a CfRadial1 file
#[pyfunction]
fn read_cfradial1(path: String) -> PyResult<PyVolumeData> {
    let backend = CfRadial1Backend::new();
    let path = PathBuf::from(path);

    backend
        .read_volume(&path)
        .map(|volume| PyVolumeData { inner: volume })
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to read file: {}", e)))
}

/// Scan a CfRadial1 file for metadata only
#[pyfunction]
fn scan_cfradial1(path: String) -> PyResult<PyVolumeMetadata> {
    let backend = CfRadial1Backend::new();
    let path = PathBuf::from(path);

    backend
        .scan_file(&path)
        .map(|metadata| PyVolumeMetadata { inner: metadata })
        .map_err(|e| PyRuntimeError::new_err(format!("Failed to scan file: {}", e)))
}

/// Python module
#[pymodule]
fn _radish(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyVolumeData>()?;
    m.add_class::<PyVolumeMetadata>()?;
    m.add_class::<PySweepData>()?;
    m.add_class::<PyMomentData>()?;
    m.add_function(wrap_pyfunction!(read_cfradial1, m)?)?;
    m.add_function(wrap_pyfunction!(scan_cfradial1, m)?)?;
    Ok(())
}
