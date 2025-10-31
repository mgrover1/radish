# Radish Python Bindings

High-performance weather radar data library with Rust core and Python bindings.

## Installation

### From source

```bash
# Install maturin
pip install maturin

# Build and install in development mode
cd python
maturin develop --release

# Or build a wheel
maturin build --release
pip install target/wheels/radish-*.whl
```

### With xarray support

```bash
pip install radish[xarray]
```

## Quick Start

### Basic Usage

```python
import radish

# Read a CfRadial1 file
volume = radish.read_cfradial1("cfrad.nc")

# Access metadata
print(f"Instrument: {volume.metadata.instrument_name}")
print(f"Sweeps: {volume.num_sweeps}")

# Access sweep data
sweep = volume.get_sweep(0)
print(f"Rays: {sweep.num_rays}, Gates: {sweep.num_gates}")
print(f"Moments: {sweep.moment_names()}")

# Access moment data
dbz = sweep.get_moment("DBZH")
data = dbz.data()  # Returns numpy array
print(f"Reflectivity shape: {data.shape}")
```

### With xarray

```python
from datatree import DataTree

# Open as DataTree
radar = DataTree.open_datatree("cfrad.nc", engine="radish")

# Access sweeps
sweep_0 = radar["sweep_0"].ds

# Work with xarray
sweep_0["DBZH"].plot()
```

## Performance

Radish uses Rust for performance-critical operations:

- 10-100x faster file parsing than pure Python
- Memory-efficient data structures
- Minimal Python overhead

## Development

```bash
# Install development dependencies
pip install -e ".[dev]"

# Run tests
pytest

# Format code
black radish/
ruff check radish/

# Build documentation
cd docs && make html
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](../LICENSE-APACHE))
- MIT license ([LICENSE-MIT](../LICENSE-MIT))

at your option.
