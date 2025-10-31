"""
Radish: High-performance weather radar data library

A Rust-powered library for reading weather radar data with Python bindings.
"""

from radish._radish import (
    VolumeData,
    VolumeMetadata,
    SweepData,
    MomentData,
    read_cfradial1,
    scan_cfradial1,
)

__version__ = "0.1.0"

__all__ = [
    "VolumeData",
    "VolumeMetadata",
    "SweepData",
    "MomentData",
    "read_cfradial1",
    "scan_cfradial1",
]
