# Getting Started with Radish

This guide will help you get started with Radish, a high-performance weather radar data library.

## Project Structure

```
radish/
├── README.md                 # Main project README
├── CLAUDE.md                 # Repo-level instructions for Claude Code agents
├── Cargo.toml                # Rust workspace configuration
├── .gitignore                # Git ignore file
│
├── docs/                     # Long-form documentation
│   ├── README.md             # Index of docs/ contents
│   ├── ARCHITECTURE.md       # Detailed architecture documentation with diagrams
│   ├── GETTING_STARTED.md    # This file
│   ├── PROJECT_SUMMARY.md    # Phased roadmap and status
│   └── CHANGELOG.md          # Version history (Keep a Changelog format)
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

### Python Usage (radish as a decode engine)

If you're building a chunked or lazy reader — a zarr codec, a virtual /
byte-range reference store, a partial-volume read — you usually want *one
moment* out of *one NEXRAD LDM record*, not a whole volume. One record
holds ~120 Message 31 radials with every moment interleaved into the same
byte range, so the low-level decoders let you demultiplex just the bytes
you need.

These are NEXRAD-specific, so the canonical names are
format-qualified — `decode_nexrad_record_moment`,
`decode_nexrad_sweep_moment`, `nexrad_record_moment_encoding`,
`nexrad_sweep_moment_encoding` — matching `read_nexrad` / `scan_nexrad`.
The unqualified spellings (`decode_record_moment`, …) are kept as aliases
for the names issue #32 introduced; they refer to the same objects.

The workflow is **inspect → allocate → decode**:

```python
import bz2, struct
import numpy as np
import radish

raw = open("KLOT20251210_102338_V06", "rb").read()

# Walk the LDM records yourself — you already know where your chunks are.
pos, records = 24, []                      # 24 = AR2V volume header
while pos + 4 <= len(raw):
    (size,) = struct.unpack_from(">i", raw, pos)
    size = abs(size)
    if size == 0 or pos + 4 + size > len(raw):
        break
    records.append(raw[pos + 4 : pos + 4 + size])
    pos += 4 + size

record = bz2.decompress(records[5])

# 1. Inspect: how many radials, and how is each moment encoded?
enc = radish.nexrad_record_moment_encoding(record)
zdr = enc["moments"]["ZDR"]
print(enc["radial_count"], zdr["word_size"], zdr["scale"], zdr["offset"])

# 2. Allocate + 3. decode — ~0.08 ms for a 120 x 1832 block.
array = radish.decode_nexrad_record_moment(
    record, "ZDR", (enc["radial_count"], zdr["max_gate_count"]),
    np.uint8 if zdr["word_size"] == 8 else np.uint16,
)

# Raw words in, CF attributes out — apply them yourself (or hand them to
# xarray as scale_factor/add_offset and let it decode lazily).
physical = array * zdr["scale_factor"] + zdr["add_offset"]
```

**Encodings change across RDA builds.** KVNX flipped ZDR from
`word_size=8, scale=16.0, offset=128.0` to `word_size=16, scale=32.0,
offset=418.0` during a 2020-06-02 upgrade outage. Any array-shaped target
pins a single dtype and a single `scale_factor`/`add_offset`, so pass an
explicit target grid when you need volumes from both eras in one store:

```python
array = radish.decode_nexrad_record_moment(
    record, "ZDR", (rays, gates), np.uint16, scale=32.0, offset=418.0,
)
```

The remap is applied only when it is exactly representable — here
`raw16 = 2 * raw8 + 162`, lossless in physical units. Otherwise
`radish.MomentEncodingError` (a `ValueError`) is raised rather than
approximate values being returned. `enc["moments"][name]["uniform"]` is
`False` when the input itself mixes encodings, which is your signal that a
target grid is required.

For a whole sweep-sized byte span (still compressed, control words and
all) use `radish.decode_nexrad_sweep_moment` / `radish.nexrad_sweep_moment_encoding`,
which decompress records in parallel. Note that each call decompresses the
span, so if you want *every* moment you're better off with
`radish.open_datatree`.

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

1. **Read the Architecture Documentation**: See `docs/ARCHITECTURE.md` for detailed design diagrams

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
# Set library paths (macOS with Homebrew) — use `brew --prefix`, not a
# hardcoded /opt/homebrew, so this works on both Apple Silicon and Intel
# Macs and stays correct if the formula's keg path ever changes.
export NETCDF_DIR=$(brew --prefix netcdf)
export HDF5_DIR=$(brew --prefix hdf5)

# Or use pkg-config
export PKG_CONFIG_PATH=$(brew --prefix hdf5)/lib/pkgconfig:$(brew --prefix netcdf)/lib/pkgconfig
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

- **Architecture**: See `docs/ARCHITECTURE.md` for detailed design diagrams
- **Rust API Docs**: Run `cargo doc --open`
- **Python API Docs**: Coming soon
- **Examples**: `examples/` and `python/examples/`
- **Tests**: `tests/` and `python/tests/`

## License

Licensed under either of:
- Apache License, Version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- MIT license ([LICENSE-MIT](../LICENSE-MIT))

at your option.
