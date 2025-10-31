# Getting Started with Radish

This guide will help you get started with Radish, a high-performance weather radar data library.

## Project Structure

```
radish/
├── ARCHITECTURE.md           # Detailed architecture documentation with diagrams
├── README.md                 # Main project README
├── Cargo.toml                # Rust workspace configuration
├── .gitignore                # Git ignore file
│
├── radish/                   # Core Rust library
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs            # Main library entry point
│       ├── error.rs          # Error types
│       ├── model/            # Data model (VolumeData, SweepData, etc.)
│       │   ├── mod.rs
│       │   ├── volume.rs
│       │   ├── sweep.rs
│       │   ├── moment.rs
│       │   └── coordinates.rs
│       ├── backends/         # Format readers
│       │   ├── mod.rs
│       │   └── cfradial1.rs  # CfRadial1 NetCDF backend
│       ├── io/               # I/O utilities
│       │   ├── mod.rs
│       │   └── netcdf_utils.rs
│       └── transforms/       # Data transformations (future)
│           ├── mod.rs
│           └── georeference.rs
│
├── python/                   # Python bindings
│   ├── Cargo.toml            # PyO3 configuration
│   ├── pyproject.toml        # Python package configuration
│   ├── src/
│   │   └── lib.rs            # PyO3 bindings
│   ├── radish/
│   │   ├── __init__.py       # Python package entry point
│   │   └── backends/
│   │       ├── __init__.py
│   │       └── xarray_backend.py  # xarray integration
│   ├── examples/
│   │   └── read_cfradial.py
│   ├── tests/
│   │   └── test_radish.py
│   └── README.md
│
├── types/                    # Shared type definitions
│   ├── Cargo.toml
│   └── src/
│       └── lib.rs
│
├── examples/                 # Rust examples
│   └── read_cfradial.rs
│
└── tests/                    # Rust tests
    └── test_basic.rs
```

## Building the Project

### Prerequisites

**Rust:**
```bash
# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Make sure you have the latest version
rustup update
```

**Python (for Python bindings):**
```bash
# Python 3.9 or later
python --version

# Install maturin
pip install maturin
```

**System dependencies:**
- NetCDF library (for CfRadial1 support)
- HDF5 library (for ODIM and other HDF5-based formats)

On macOS:
```bash
brew install netcdf hdf5
```

On Ubuntu/Debian:
```bash
sudo apt-get install libnetcdf-dev libhdf5-dev
```

### Build Rust Library

```bash
# Check that everything compiles
cargo check

# Build in release mode
cargo build --release

# Run tests
cargo test

# Run example
cargo run --example read_cfradial
```

### Build Python Package

```bash
# From the python directory
cd python

# Development build (installs in-place)
maturin develop --release

# Or build a wheel
maturin build --release

# Install the wheel
pip install target/wheels/radish-*.whl
```

### Install Python Package with xarray support

```bash
# Install with optional dependencies
pip install -e ".[xarray]"

# Or just the dependencies
pip install xarray datatree
```

## Usage Examples

### Rust Usage

```rust
use radish::backends::{RadarBackend, CfRadial1Backend};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend = CfRadial1Backend::new();
    let path = Path::new("path/to/cfrad.nc");

    // Read entire volume
    let volume = backend.read_volume(path)?;

    println!("Instrument: {}", volume.metadata.instrument_name);
    println!("Sweeps: {}", volume.num_sweeps());

    // Access first sweep
    let sweep = &volume.sweeps[0];
    println!("Rays: {}, Gates: {}", sweep.num_rays(), sweep.num_gates());

    // Access moment
    if let Some(dbz) = sweep.get_moment("DBZH") {
        println!("Reflectivity shape: {:?}", dbz.shape());
    }

    Ok(())
}
```

### Python Usage (Direct API)

```python
import radish

# Read volume
volume = radish.read_cfradial1("path/to/cfrad.nc")

print(f"Instrument: {volume.metadata.instrument_name}")
print(f"Sweeps: {volume.num_sweeps}")

# Access sweep
sweep = volume.get_sweep(0)
print(f"Rays: {sweep.num_rays}, Gates: {sweep.num_gates}")

# Access moment data
dbz = sweep.get_moment("DBZH")
data = dbz.data()  # NumPy array
print(f"Shape: {data.shape}")
```

### Python Usage (xarray)

```python
from datatree import DataTree
import matplotlib.pyplot as plt

# Open as DataTree (xarray backend)
radar = DataTree.open_datatree("path/to/cfrad.nc", engine="radish")

# Access root metadata
print(radar["/"].ds)

# Access first sweep
sweep_0 = radar["sweep_0"].ds
print(sweep_0)

# Plot reflectivity
sweep_0["DBZH"].plot()
plt.show()
```

## Next Steps

### For Developers

1. **Add More Backends**: Implement `RadarBackend` trait for other formats:
   - CfRadial2
   - ODIM H5
   - IRIS/Sigmet
   - NEXRAD Level 2

2. **Implement Transforms**: Add functionality in `transforms/` module:
   - Georeferencing
   - Velocity dealiasing
   - Attenuation correction
   - KDP calculation

3. **Optimize Performance**:
   - Add memory-mapped I/O
   - Implement parallel sweep loading
   - Add compression support

4. **Expand Testing**:
   - Add integration tests with real data
   - Add benchmark suite
   - Test with various radar formats

### For Users

1. **Read the Architecture Documentation**: See `ARCHITECTURE.md` for detailed design diagrams

2. **Try the Examples**:
   - Rust: `examples/read_cfradial.rs`
   - Python: `python/examples/read_cfradial.py`

3. **Explore the API**:
   - Core data model: `radish/src/model/`
   - Backend system: `radish/src/backends/`
   - Python bindings: `python/src/lib.rs`

4. **Contribute**: See issues at https://github.com/mgrover1/radish/issues

## Troubleshooting

### Build Errors

**NetCDF/HDF5 not found:**
```bash
# Set library paths (macOS with Homebrew)
export NETCDF_DIR=/opt/homebrew
export HDF5_DIR=/opt/homebrew

# Or use pkg-config
export PKG_CONFIG_PATH=/opt/homebrew/lib/pkgconfig
```

**Rust toolchain issues:**
```bash
rustup update
rustup default stable
```

### Python Import Errors

**Module not found:**
```bash
# Make sure you're in the right directory
cd python
maturin develop --release

# Or reinstall
pip uninstall radish
maturin develop --release
```

**NumPy version mismatch:**
```bash
pip install --upgrade numpy
```

## Resources

- **Architecture**: See `ARCHITECTURE.md` for detailed design diagrams
- **Rust API Docs**: Run `cargo doc --open`
- **Python API Docs**: Coming soon
- **Examples**: `examples/` and `python/examples/`
- **Tests**: `tests/` and `python/tests/`

## License

Licensed under either of:
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
