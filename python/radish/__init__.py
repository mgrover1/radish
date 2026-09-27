"""
Radish: High-performance weather radar data library

A Rust-powered library for reading weather radar data with Python bindings.
"""

# Lower-level building blocks (Rust → Python). Most users should reach for
# `radish.open_datatree` / `radish.open_dataset` instead — these stay
# exported for power users who want a `VolumeData` directly without
# building an xarray tree, or for callers who want explicit format
# selection on a path. `read_nexrad_bytes` is intentionally **not** here
# (it's reachable as `radish._radish.read_nexrad_bytes` for the dispatcher's
# internal use; the public way to decode bytes is `radish.open_datatree(data)`).
from radish._radish import (
    MomentData,
    MomentEncodingError,
    NexradSweepAttrs,
    NexradVolumeAttrs,
    SigmetSweepAttrs,
    SigmetVolumeAttrs,
    SweepData,
    VolumeData,
    VolumeMetadata,
    dealias_region_based,
    decode_nexrad_record_moment,
    decode_nexrad_sweep_moment,
    nexrad_record_moment_encoding,
    nexrad_sweep_moment_encoding,
    read_cfradial1,
    read_nexrad,
    read_nexrad_chunks,
    read_sigmet,
    scan_cfradial1,
    scan_nexrad,
    scan_nexrad_chunks,
    scan_sigmet,
)

# Canonical entry points: format-agnostic, input-shape-agnostic.
from radish._open import detect_backend, open_dataset, open_datatree, scan

# Single source of truth: the version baked into the wheel by maturin
# from `python/pyproject.toml`. Avoids the long-standing drift where
# this constant was hard-coded at 0.1.0 while the wheels shipped 0.2.x.
from importlib.metadata import PackageNotFoundError, version as _pkg_version

# Unqualified aliases for the NEXRAD per-moment decoders. Issue #32
# specified the bare names (its reproducer does
# `hasattr(radish, "decode_record_moment")`), so they stay as first-class
# aliases pointing at the same objects. The `nexrad_`-qualified names
# imported above are canonical — they match the rest of the API
# (`read_nexrad` / `scan_nexrad`) and leave room for a future Sigmet/ODIM
# equivalent (`decode_sigmet_record_moment`, …).
decode_record_moment = decode_nexrad_record_moment
decode_sweep_moment = decode_nexrad_sweep_moment
record_moment_encoding = nexrad_record_moment_encoding
sweep_moment_encoding = nexrad_sweep_moment_encoding

try:
    __version__ = _pkg_version("radish-rs")
except PackageNotFoundError:  # editable install before maturin develop
    __version__ = "0.0.0+unknown"

__all__ = [
    # Data model
    "VolumeData",
    "VolumeMetadata",
    "SweepData",
    "MomentData",
    "NexradVolumeAttrs",
    "NexradSweepAttrs",
    "SigmetVolumeAttrs",
    "SigmetSweepAttrs",
    # Canonical entry points
    "open_datatree",
    "open_dataset",
    "scan",
    "detect_backend",
    # Lower-level building blocks (per-format, path-only readers)
    "read_cfradial1",
    "scan_cfradial1",
    "read_nexrad",
    "scan_nexrad",
    "read_nexrad_chunks",
    "scan_nexrad_chunks",
    "read_sigmet",
    "scan_sigmet",
    # Velocity dealiasing (region-based, bit-exact with Py-ART on every
    # unmasked gate — see docs/NEXRAD_LEVEL3_WASM.md).
    "dealias_region_based",
    # Low-level NEXRAD per-moment decoders. These return the **raw**
    # NEXRAD words for one moment out of one LDM record (or one
    # sweep-sized byte span) so chunked/lazy consumers — zarr codecs,
    # virtual reference stores, partial-volume reads — can decode
    # exactly the bytes they need. Pair the decoders with the
    # `*_moment_encoding` inspectors, which report each moment's
    # word_size/scale/offset before you allocate.
    #
    # Canonical (format-qualified) names:
    "decode_nexrad_record_moment",
    "decode_nexrad_sweep_moment",
    "nexrad_record_moment_encoding",
    "nexrad_sweep_moment_encoding",
    # Unqualified aliases (issue #32 names) — same objects:
    "decode_record_moment",
    "decode_sweep_moment",
    "record_moment_encoding",
    "sweep_moment_encoding",
    "MomentEncodingError",
]
