# radish

High-performance weather radar data library — Rust core, Python bindings, and a WASM target for the browser.

[![Rust CI](https://github.com/mgrover1/radish/actions/workflows/rust-ci.yml/badge.svg)](https://github.com/mgrover1/radish/actions/workflows/rust-ci.yml)
[![Python CI](https://github.com/mgrover1/radish/actions/workflows/python-ci.yml/badge.svg)](https://github.com/mgrover1/radish/actions/workflows/python-ci.yml)
[![PyPI](https://img.shields.io/pypi/v/radish-rs.svg)](https://pypi.org/project/radish-rs/)
[![codecov](https://codecov.io/gh/mgrover1/radish/branch/main/graph/badge.svg)](https://codecov.io/gh/mgrover1/radish)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Radish reads several weather radar formats straight into one normalized,
CfRadial2/FM301-shaped data model — a `VolumeData` containing per-sweep
`SweepData` with named `MomentData` arrays and a shared `Coordinates` axis —
regardless of what format it started as. The Rust core does the parsing;
Python gets it through PyO3 bindings and an `xarray` backend plugin
(`engine="radish"`); a WASM target ports the NEXRAD Level 3 decoder straight
into the browser with no server round-trip.

Architecture is inspired by [gribberish](https://github.com/mpiannucci/gribberish)
(Rust-first, PyO3) and [xradar](https://github.com/openradar/xradar)
(plugin-based format backends).

**Status: alpha.** The on-disk format coverage and Rust API are still
evolving. Most users should reach for `radish.open_datatree` /
`radish.open_dataset` / `radish.scan`, or the `xarray` engine, rather than the
per-format `read_*` / `scan_*` functions directly — see Quick start below.

## Supported formats

| Format | Support | Notes |
| --- | --- | --- |
| CfRadial1 (NetCDF) | Read | Full sweep/volume reads |
| NEXRAD Level 2 / Archive II | Read | Pre- and post-Build-12 archives (1991–2012+); in-memory bytes, live S3 real-time chunk streams, incomplete-sweep policies (keep/drop/pad) |
| NEXRAD Level 3 / NIDS | Read | All 26 xradar-tracked (openradar/xradar#392) message codes decode. Also compiles to WASM for browser-side decoding |
| IRIS / Sigmet RAW | Read | PPI + RHI, ported from xradar's `iris.py` for calibration parity |
| CfRadial2 | Not implemented | no writer or backend yet |
| ODIM_H5 | Not implemented | the ODIM short-name / CF-metadata convention is reused internally for moment naming, but there is no ODIM_H5 file reader |

## Installation

### Python

Prebuilt wheels are published to PyPI as `radish-rs` (the plain `radish` name
was already taken) for Linux x86_64 and macOS (Intel + Apple Silicon) on
Python 3.12/3.13 — `import radish` still works normally:

```bash
pip install radish-rs
pip install "radish-rs[xarray]"   # adds the xr.open_datatree(engine="radish") integration
```

Everywhere else (Linux aarch64, Windows, other Python versions) currently
requires building from source via maturin — install the system dependencies
below, then:

```bash
cd python
pip install maturin
maturin develop --release
```

### Rust

Not yet published to crates.io (the `radish` crate name is taken there too).
Build from source:

```bash
cargo build --release
```

### System dependencies

Radish's Rust core links against NetCDF and HDF5 for the CfRadial1 backend,
so both libraries must be installed before building:

- **Ubuntu/Debian**: `sudo apt-get install libnetcdf-dev libhdf5-dev`
- **macOS**: `brew install netcdf hdf5`

If the build can't find them (`Unable to locate HDF5 root directory
and/or headers`), point it at Homebrew's install prefix explicitly —
using `brew --prefix` rather than a hardcoded path keeps this correct on
both Apple Silicon and Intel Macs:

```bash
export HDF5_DIR=$(brew --prefix hdf5)
export NETCDF_DIR=$(brew --prefix netcdf)
export PKG_CONFIG_PATH=$(brew --prefix hdf5)/lib/pkgconfig:$(brew --prefix netcdf)/lib/pkgconfig
```

## Quick start

### Python — local file, via xarray

```python
import xarray as xr

dt = xr.open_datatree("KLOT20260310_231412_V06", engine="radish")  # NEXRAD Level 2
dt = xr.open_datatree("cfrad.nc", engine="radish")                  # CfRadial1
dt = xr.open_datatree("some_file.RAWXXXX", engine="radish")         # Sigmet/IRIS
```

Format detection is automatic. Equivalently, without going through xarray's
plugin loader:

```python
import radish

dt = radish.open_datatree("KLOT20260310_231412_V06")  # -> xarray.DataTree
```

### Python — bytes-in, cloud-native

```python
import fsspec
import radish

with fsspec.open(
    "s3://noaa-nexrad-level2/2011/05/20/KVNX/KVNX20110520_000442_V06.gz",
    "rb", compression="gzip", anon=True,
) as f:
    metadata = radish.scan(f)                # metadata only, no full decode
    volume = radish.open_datatree(f.read())  # full decode -> xarray.DataTree
```

The same bytes-in path also accepts a list of NEXRAD Level 2 real-time chunk
files (`S`/`I`/`E`) fetched from the public
`unidata-nexrad-level2-chunks` S3 bucket — see `python/examples/read_chunks.ipynb`.

### Rust

```rust
use std::path::Path;
use radish::backends::{auto_backend, RadarBackend};

let path = Path::new("path/to/file.RAW");
let backend = auto_backend(path)?;
let volume = backend.read_volume(path)?;
println!("{} sweeps, instrument = {}",
         volume.num_sweeps(), volume.metadata.instrument_name);
```

More examples: `examples/read_cfradial.rs`, `python/examples/`.

## Performance

Radish's Rust core is measurably faster than pure-Python readers on the same
files. These numbers come from the maintainer's own runs against `xradar` on
specific NEXRAD/Sigmet fixtures (KLOT/KILX/KVNX) — reproducible by running
the scripts in `python/examples/bench_*.py` against your own fixture files,
but no fixture data is checked into the repo, so they are not re-verified on
every CI run:

- **NEXRAD Level 2**: ~16–18× faster end-to-end (via the `xarray` engine)
  than `xradar.io.open_nexradlevel2_datatree`.
- **IRIS/Sigmet RAW**: ~5–8× faster, from rayon-parallelized per-sweep
  conversion.
- **Region-based velocity dealiasing**: ~8× faster than Py-ART's own
  implementation on the same sweep, and bit-exact with it on every unmasked
  gate.
- NEXRAD Level 2's LDM bzip2 decompression is parallelized via rayon
  (`radish/src/backends/nexrad/decode/record.rs`); `python/examples/bench_nexrad_vs_xradar.py`
  is the regression gate for that speedup.

## How it works

Every format reader implements the `RadarBackend` trait (`scan_file`,
`read_sweep`, `read_volume`) and registers itself in
`backends::available_backends()`. `auto_backend()` / `auto_backend_for_bytes()`
walk that list and pick the first backend whose `can_read()` returns true, so
adding a new format never requires touching the dispatcher. Every backend
normalizes into the same shape — `VolumeData` → `Vec<SweepData>` →
`HashMap<String, MomentData>`, with a shared `Coordinates` axis — while
format-specific extras (NEXRAD MSG_2/MSG_5 fields, Sigmet task attrs, and so
on) live in typed, optional side-channel fields rather than being dropped.

Beyond reading, radish also ships a Rust port of Py-ART's region-based
velocity dealiasing (`radish::transforms::dealias`), exposed to both Python
and WASM.

See `docs/ARCHITECTURE.md` for the full design writeup and
`docs/GETTING_STARTED.md` for an end-to-end install-to-first-read walkthrough.

## Browser / WASM

`radish-wasm` compiles the NEXRAD Level 3 decoder and the dealiasing
transform to `wasm32-unknown-unknown` for client-side decoding with no
backend server: bytes in, decoded/dealiased sweep out, with zero-copy views
into wasm memory. It's a library only — no fetch, no S3, no worker logic.
See `docs/NEXRAD_LEVEL3_WASM.md`.

## Development

See `CLAUDE.md` for build/test/lint commands and `docs/GETTING_STARTED.md`
for a full walkthrough. `docs/CHANGELOG.md` tracks release history.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.
