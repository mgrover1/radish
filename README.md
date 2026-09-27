# radish
Weather radar reading package using rust

## System Dependencies

Radish's Rust core links against NetCDF and HDF5, so both libraries must be
installed before building:

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

See `docs/GETTING_STARTED.md` for the full build and install walkthrough.
