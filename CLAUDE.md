# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Radish is a high-performance weather radar data library with a Rust core and Python bindings. It reads multiple radar formats (CfRadial1/2, ODIM H5, IRIS/Sigmet, NEXRAD) and normalizes them to the CfRadial2/FM301 standard. The architecture is inspired by gribberish (Rust-first with PyO3) and xradar (plugin-based format system).

## Development Commands

### Rust Development

```bash
# Build the Rust library
cargo build --release

# Run all tests
cargo test --all-features

# Run specific test
cargo test --package radish --lib model::tests::test_volume_creation

# Check formatting
cargo fmt --all -- --check

# Run linter (clippy)
cargo clippy --all-targets --all-features -- -D warnings

# Build and run example
cargo run --example read_cfradial

# Generate documentation
cargo doc --open
```

### Python Development

```bash
# Build and install Python package in development mode (from python/ directory)
cd python
maturin develop --release

# Build wheel
maturin build --release

# Install with xarray support
pip install -e ".[xarray]"

# Install dev dependencies
pip install -e ".[dev]"

# Run Python tests (from python/ directory)
pytest tests/ -v

# Run a single Python test
pytest tests/test_radish.py::TestReadCfRadial1::test_read -v

# Format Python code
black radish/

# Lint Python code
ruff check radish/
```

### System Dependencies

NetCDF and HDF5 libraries are required:
- **Ubuntu/Debian**: `sudo apt-get install libnetcdf-dev libhdf5-dev`
- **macOS**: `brew install netcdf hdf5`
- **Environment variables** (if needed): `export HDF5_DIR=$(brew --prefix hdf5)` and `export NETCDF_DIR=$(brew --prefix netcdf)` (use `brew --prefix`, not a hardcoded `/opt/homebrew` — that's Apple Silicon-only and breaks on Intel Macs or when the formula's keg path changes)

## Architecture

### Workspace Structure

The project uses a Cargo workspace with three crates:
- **radish/**: Core Rust library with data model, backend trait, and format readers
- **python/**: PyO3 bindings and xarray integration
- **types/**: Shared type definitions

### Core Architecture Pattern

**Backend Trait System**: All radar format readers implement the `RadarBackend` trait:

```rust
pub trait RadarBackend: Send + Sync {
    fn scan_file(&self, path: &Path) -> Result<VolumeMetadata>;  // Fast metadata only
    fn read_sweep(&self, path: &Path, sweep_idx: usize) -> Result<SweepData>;  // Lazy loading
    fn read_volume(&self, path: &Path) -> Result<VolumeData>;  // Full read
}
```

This enables:
1. **Format auto-detection** via `auto_backend()` function
2. **Lazy loading** - read sweeps on-demand, not all at once
3. **Pluggable formats** - add new formats by implementing the trait

### Data Model Hierarchy

```
VolumeData
├── VolumeMetadata (instrument info, location, time coverage, sweep list)
├── Vec<SweepData>
│   ├── SweepMetadata (sweep number, mode, fixed angle)
│   ├── Coordinates (time, range, azimuth, elevation)
│   └── HashMap<String, MomentData> (e.g., "DBZH", "VRADH")
└── Option<RadarCalibration>
```

All data is normalized to CfRadial2/FM301 standard regardless of input format.

### Python Integration Flow

1. User calls `xarray.open_datatree("file.nc", engine="radish")`
2. xarray routes to `RadishBackendEntrypoint` (registered via the `xarray.backends` entry point in `python/pyproject.toml`)
3. Entry point calls PyO3 bindings (`radish._radish.read_cfradial1` / `scan_cfradial1`)
4. PyO3 calls the Rust backend which returns `VolumeData`
5. Backend converts to xarray `DataTree` with hierarchical structure
6. User gets native xarray/numpy arrays with zero-copy where possible

The currently exposed Python surface is intentionally narrow: `VolumeData`, `VolumeMetadata`, `SweepData`, `MomentData`, plus `read_cfradial1` / `scan_cfradial1`. New backends must be wired through PyO3 in `python/src/lib.rs` and re-exported from `python/radish/__init__.py` to be reachable from Python.

### Module Organization

**Core Rust (`radish/src/`)**:
- `model/`: Data structures (volume.rs, sweep.rs, moment.rs, coordinates.rs)
- `backends/`: Format readers (cfradial1.rs, plus mod.rs for trait)
- `io/`: I/O utilities (netcdf_utils.rs for HDF5/NetCDF helpers)
- `transforms/`: Future home for georeferencing, dealiasing, QC filters
- `error.rs`: Custom error types using thiserror

**Python Bindings (`python/src/`)**:
- `lib.rs`: PyO3 module with Python classes and functions
- `radish/backends/xarray_backend.py`: xarray backend entry point

## Key Implementation Details

### Adding a New Radar Format Backend

1. Create new file in `radish/src/backends/` (e.g., `nexrad.rs`)
2. Implement `RadarBackend` trait — required methods are `name`, `description`, `supported_extensions`, `scan_file`, `read_sweep`, `read_volume` (`can_read` has a default impl based on extension)
3. Add a `pub mod` and `pub use` for the backend in `radish/src/backends/mod.rs`, then push it into the `available_backends()` vec — `auto_backend()` iterates that list and selects the first backend whose `can_read()` returns true
4. Make `supported_extensions()` return the right extensions so auto-detection works
5. Normalize data to `VolumeData`/`SweepData` model in your implementation
6. To expose it to Python, add a PyO3 wrapper function in `python/src/lib.rs` and re-export it from `python/radish/__init__.py`

### Performance Considerations

- **Zero-copy**: Use memory-mapped files for large datasets
- **Lazy loading**: Implement `read_sweep()` for on-demand access
- **Parallel processing**: Use rayon for multi-threaded sweep processing
- **Minimal Python overhead**: Keep hot paths in Rust, expose minimal API surface

### Error Handling

Use the custom `RadishError` enum (in `error.rs`) which covers:
- I/O errors
- NetCDF/HDF5 errors
- Format validation errors
- Data conversion errors

Always return `Result<T>` (which is aliased to `Result<T, RadishError>`).

## Current Implementation Status

**Completed (Phase 1)**:
- Core data model
- Backend trait system
- CfRadial1 backend (NetCDF)
- Python bindings with PyO3
- xarray integration with automatic backend registration
- Example code and basic tests

**Planned (Phase 2+)**:
- Additional backends: CfRadial2, ODIM H5, IRIS/Sigmet, NEXRAD Level 2
- Transforms: georeferencing, velocity dealiasing, QC filters
- Optimizations: memory-mapped I/O, parallel sweep loading, streaming API

## Testing

- **Rust tests**: Located in `tests/test_basic.rs` (workspace-level integration tests) and inline `#[cfg(test)]` module tests under `radish/src/`
- **Python tests**: Located in `python/tests/test_radish.py`
- Many tests require actual radar data files (marked with pytest markers)
- CI runs tests on Ubuntu, macOS, Windows with multiple Python versions. Workflows live in `.github/workflows/`: `rust-ci.yml`, `python-ci.yml`, `release.yml`, `docs.yml`, `benchmark.yml`

## Build System

- **Rust**: Standard Cargo workspace with release profile optimizations (LTO, strip)
- **Python**: Maturin for building PyO3 extension modules. Module name is `radish._radish` (set in `[tool.maturin]` in `python/pyproject.toml`); the Python source lives in `python/radish/`
- **Entry points**: xarray backend auto-registers via `project.entry-points."xarray.backends"` in `python/pyproject.toml`

## Further Documentation

Longer-form docs live in `docs/` (read these when more context is needed than this file provides):
- `docs/ARCHITECTURE.md` — deeper dive on the data model, backend trait, and design rationale
- `docs/GETTING_STARTED.md` — end-to-end install + first-read walkthrough
- `docs/PROJECT_SUMMARY.md` — phased roadmap and status
- `docs/CHANGELOG.md` — version history (Keep a Changelog format)
- `docs/README.md` — index of the `docs/` folder

## NEXRAD decoder location

The NEXRAD Level 2 / Archive II decoder lives in-tree at
`radish/src/backends/nexrad/decode/`. As of Phase 7 of plan 0003 it
is the production read path for `NexradBackend` — there are no
upstream `nexrad`/`nexrad-data`/`nexrad-decode`/`nexrad-model`
runtime dependencies. The upstream `nexrad` crate is a
`[dev-dependencies]`-only side-by-side reference for
`radish/tests/test_nexrad_internal_parity.rs`.

Parallel LDM bzip2 decompression lives at
`decode::record::decompress_all` and uses rayon directly, so the
~6× speedup the workspace previously got from
`nexrad/parallel` is now ours to own. The `bench_nexrad_vs_xradar.py`
benchmark is the regression gate — if `decompress_all` ever stops
parallelising, that bench drops to 2-3× over xradar instead of
10×+.