"""Xarray backend for radish"""

from typing import Any, Dict, Iterable, Optional
import numpy as np

try:
    import xarray as xr
    from xarray.backends import BackendEntrypoint
    from xarray.core import indexing
    XARRAY_AVAILABLE = True
except ImportError:
    XARRAY_AVAILABLE = False
    BackendEntrypoint = object  # type: ignore

try:
    from datatree import DataTree
    DATATREE_AVAILABLE = True
except ImportError:
    DATATREE_AVAILABLE = False

from radish import read_cfradial1, VolumeData


class RadishBackendEntrypoint(BackendEntrypoint):
    """Xarray backend for reading radar files with radish"""

    description = "Read weather radar data files using the radish library"
    url = "https://github.com/mgrover1/radish"

    def open_dataset(
        self,
        filename_or_obj,
        *,
        drop_variables: Optional[Iterable[str]] = None,
        **kwargs
    ):
        """
        Open a single dataset (sweep).

        For multi-sweep files, use open_datatree instead.
        """
        # Read the volume
        volume = read_cfradial1(str(filename_or_obj))

        # Get the first sweep
        sweep = volume.get_sweep(0)

        # Convert to xarray Dataset
        dataset = self._sweep_to_dataset(sweep, volume.metadata)

        return dataset

    def open_datatree(
        self,
        filename_or_obj,
        *,
        drop_variables: Optional[Iterable[str]] = None,
        **kwargs
    ):
        """
        Open a radar volume as a DataTree with multiple sweeps.

        Returns a DataTree with:
        - Root group: volume metadata
        - sweep_N groups: individual sweep data
        """
        if not DATATREE_AVAILABLE:
            raise ImportError(
                "datatree is required for open_datatree. "
                "Install with: pip install datatree"
            )

        # Read the volume
        volume = read_cfradial1(str(filename_or_obj))

        # Create datasets for each sweep
        datasets = {}

        # Root dataset with volume metadata
        root_ds = self._create_root_dataset(volume.metadata)
        datasets["/"] = root_ds

        # Sweep datasets
        for i in range(volume.num_sweeps):
            sweep = volume.get_sweep(i)
            sweep_ds = self._sweep_to_dataset(sweep, volume.metadata)
            datasets[f"/sweep_{i}"] = sweep_ds

        # Create DataTree
        return DataTree.from_dict(datasets)

    def _create_root_dataset(self, metadata) -> "xr.Dataset":
        """Create root dataset with volume metadata"""
        coords = {
            "latitude": metadata.latitude,
            "longitude": metadata.longitude,
            "altitude": metadata.altitude,
        }

        data_vars = {
            "sweep_fixed_angle": (["sweep"], np.array(metadata.sweep_fixed_angles)),
        }

        attrs = {
            "instrument_name": metadata.instrument_name,
            "Conventions": "CF/Radial",
        }

        return xr.Dataset(data_vars=data_vars, coords=coords, attrs=attrs)

    def _sweep_to_dataset(self, sweep, volume_metadata) -> "xr.Dataset":
        """Convert a sweep to an xarray Dataset"""
        # Coordinates
        coords = {
            "azimuth": (["time"], np.array(sweep.azimuth)),
            "elevation": (["time"], np.array(sweep.elevation)),
            "range": (["range"], np.array(sweep.range)),
        }

        # Data variables (moments)
        data_vars = {}
        for moment_name in sweep.moment_names():
            moment = sweep.get_moment(moment_name)
            if moment is not None:
                data_vars[moment_name] = (
                    ["time", "range"],
                    moment.data(),
                    {"units": moment.units}
                )

        # Attributes
        attrs = {
            "sweep_number": int(sweep.sweep_number),
            "fixed_angle": float(sweep.fixed_angle),
            "instrument_name": volume_metadata.instrument_name,
        }

        return xr.Dataset(data_vars=data_vars, coords=coords, attrs=attrs)

    @classmethod
    def guess_can_open(cls, filename_or_obj):
        """Guess if the file can be opened by this backend"""
        try:
            path = str(filename_or_obj)
            # Check file extension
            if path.endswith((".nc", ".nc4", ".netcdf")):
                # Could do more sophisticated checking here
                return True
        except (TypeError, AttributeError):
            pass
        return False


# For backwards compatibility
RadishBackend = RadishBackendEntrypoint
