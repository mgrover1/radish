"""Xarray backend entrypoint for radish.

Provides the ``engine="radish"`` plugin registration. All format detection
and per-shape dispatch lives in :mod:`radish._open` — this module is just
the xarray-side adapter that delegates to ``radish.open_datatree`` /
``radish.open_dataset`` and houses the ``VolumeData → DataTree`` builder
helpers (``_volume_to_datatree``, ``_create_root_dataset``,
``_sweep_to_dataset``) that those entry points reuse.
"""

from typing import Any, ClassVar, Dict, Iterable, Optional, Tuple

import numpy as np
import pandas as pd

try:
    import xarray as xr
    from xarray.backends import BackendEntrypoint

    XARRAY_AVAILABLE = True
except ImportError:
    XARRAY_AVAILABLE = False
    BackendEntrypoint = object  # type: ignore

try:
    from datatree import DataTree

    DATATREE_AVAILABLE = True
except ImportError:
    DATATREE_AVAILABLE = False
    # xarray ≥ 2024.10 also exposes DataTree natively
    try:
        from xarray import DataTree  # type: ignore

        DATATREE_AVAILABLE = True
    except ImportError:
        pass


def _moment_cf_attrs(moment) -> Dict[str, str]:
    """Resolve the full CF attribute set for a moment from the Rust side.

    The Rust adapter's `radish::backends::nexrad::mapping::moment_meta` is
    the single source of truth for `units`, `standard_name`, and `long_name`.
    PyMomentData exposes all three; if the backend didn't set
    `standard_name` / `long_name` (e.g. an unknown CfRadial1 variable), we
    fall back to the moment name so the variable is at least minimally
    annotated.
    """
    return {
        "units": moment.units,
        "standard_name": moment.standard_name or moment.name,
        "long_name": moment.long_name or moment.name,
    }


def _parse_sweep_index(group: Optional[str], num_sweeps: int) -> int:
    """Resolve a ``group="sweep_N"`` argument to a sweep index, defaulting to 0
    when no group is given. Raises ``ValueError`` for malformed or out-of-range
    group names rather than silently selecting sweep 0.
    """
    if not group:
        return 0
    if not group.startswith("sweep_"):
        raise ValueError(f"unrecognised group {group!r}; expected 'sweep_<N>' or no group")
    suffix = group.split("_", 1)[1]
    try:
        idx = int(suffix)
    except ValueError as e:
        raise ValueError(f"sweep group {group!r} is not numeric") from e
    if not (0 <= idx < num_sweeps):
        raise ValueError(f"sweep group {group!r} out of range [0, {num_sweeps})")
    return idx


class RadishBackendEntrypoint(BackendEntrypoint):
    """Xarray backend for reading radar files with radish"""

    description = "Read weather radar data files using the radish library"
    url = "https://github.com/mgrover1/radish"

    # xarray's plugin discovery introspects the signature and rejects *args/**kwargs.
    # `BackendEntrypoint.open_dataset_parameters` is a class variable; mark ours
    # `ClassVar` too so mypy doesn't read this as an instance-attribute override.
    # `backend` is listed here so xarray's plugin loader passes it through
    # `xr.open_datatree(path, engine="radish", backend="nexrad")` instead of
    # rejecting it as an unknown kwarg.
    open_dataset_parameters: ClassVar[Optional[Tuple[str, ...]]] = (
        "filename_or_obj",
        "drop_variables",
        "group",
        "backend",
        "incomplete_sweep",
    )

    def open_dataset(
        self,
        filename_or_obj,
        *,
        drop_variables: Optional[Iterable[str]] = None,
        group: Optional[str] = None,
        backend: Optional[str] = None,
        incomplete_sweep: Optional[str] = None,
    ):
        """Delegate to :func:`radish.open_dataset`. Existing
        ``xr.open_dataset(path, engine="radish")`` callers go through this
        path; ``backend="nexrad"`` / ``backend="cfradial1"`` skips the
        format-sniff and forces the chosen radish backend.
        """
        from radish import open_dataset as _open_dataset

        return _open_dataset(
            filename_or_obj,
            backend=backend,
            group=group,
            drop_variables=drop_variables,
            incomplete_sweep=incomplete_sweep,
        )

    def open_datatree(
        self,
        filename_or_obj,
        *,
        drop_variables: Optional[Iterable[str]] = None,
        backend: Optional[str] = None,
        incomplete_sweep: Optional[str] = None,
    ):
        """Delegate to :func:`radish.open_datatree`. Existing
        ``xr.open_datatree(path, engine="radish")`` callers go through this
        path; ``backend="nexrad"`` / ``backend="cfradial1"`` skips the
        format-sniff and forces the chosen radish backend.
        """
        if not DATATREE_AVAILABLE:
            raise ImportError(
                "DataTree support requires xarray>=2024.10 or the legacy "
                "datatree package. Install with: pip install -U xarray"
            )
        from radish import open_datatree as _open_datatree

        return _open_datatree(
            filename_or_obj,
            backend=backend,
            drop_variables=drop_variables,
            incomplete_sweep=incomplete_sweep,
        )

    def _volume_to_datatree(self, volume, fmt: str) -> "DataTree":
        """Build a DataTree from an already-decoded `VolumeData`.

        Factored out so chunk-based readers (`open_nexrad_chunks_datatree`)
        can hit the same dim/coord/attr emission path used by
        `open_datatree`.
        """
        datasets = {"/": self._create_root_dataset(volume.metadata, fmt)}
        # Children are named from `sweep_group_names`, not a positional
        # enumerate: under NEXRAD's incomplete_sweep="drop" the survivors
        # keep their original `sweep_N` names with gaps (xradar #332
        # parity — dropping sweep_1 must not rename sweep_2 to sweep_1).
        # Every backend fills the names 1:1 with the sweep list; a
        # mismatch is a producer bug, so index loudly rather than mask
        # it (a fallback name could silently overwrite a sibling key).
        names = list(volume.metadata.sweep_group_names)
        for i in range(volume.num_sweeps):
            sweep = volume.get_sweep(i)
            datasets[f"/{names[i]}"] = self._sweep_to_dataset(sweep, volume.metadata)
        return DataTree.from_dict(datasets)

    def _create_root_dataset(self, metadata, fmt: str = "cfradial1") -> "xr.Dataset":
        """Create root dataset with volume metadata.

        For NEXRAD: matches xradar's root layout — site coords plus the
        radar-station attribute set; sweep-level information lives in the
        ``/sweep_N`` groups, not at the root. CfRadial1 keeps a top-level
        ``sweep_fixed_angle(sweep)`` array for backward compatibility with
        the original radish backend.
        """
        coords = {
            "latitude": metadata.latitude,
            "longitude": metadata.longitude,
            "altitude": metadata.altitude,
        }
        data_vars: Dict[str, Any] = {}
        if fmt == "sigmet":
            # xradar's `open_iris_datatree` emits `sweep_fixed_angle(sweep)`
            # AND `sweep_group_name(sweep)` at the root, in addition to the
            # FM301 0-d scalars inside each sweep group. Match that shape
            # so xarray-radar tooling (which is built around xradar's
            # output) sees the same `sweep` dim broadcast it expects.
            data_vars["sweep_fixed_angle"] = (
                ["sweep"],
                np.array(metadata.sweep_fixed_angles),
            )
            data_vars["sweep_group_name"] = (
                ["sweep"],
                np.array(metadata.sweep_group_names),
            )
        elif fmt != "nexrad":
            # Pre-existing CfRadial1 shape; xradar's NEXRAD reader doesn't
            # advertise a root-level sweep_fixed_angle array, so we skip it
            # for NEXRAD to avoid an extra `sweep` dim leaking into sweep_0.
            data_vars["sweep_fixed_angle"] = (
                ["sweep"],
                np.array(metadata.sweep_fixed_angles),
            )
        attrs: Dict[str, Any] = {
            "instrument_name": metadata.instrument_name,
        }
        if fmt == "nexrad":
            # Mirror xradar's `open_nexradlevel2_datatree` root attribute set
            # exactly. xradar emits these as the literal string "None" rather
            # than asserting any convention — they don't claim CfRadial2 /
            # FM301 compliance.
            extra = dict(metadata.attributes) if getattr(metadata, "attributes", None) else {}
            attrs.update(
                {
                    "Conventions": "None",
                    "version": "None",
                    "title": "None",
                    "institution": "None",
                    "references": "None",
                    "source": "None",
                    "history": "None",
                    "comment": "im/exported using radish",
                    "scan_name": extra.get("scan_name", ""),
                }
            )
            # MSG_2 / MSG_5 attrs decoded by the Rust adapter
            # (`backends::nexrad::attrs::volume_attrs`). Each attr gets a Python
            # primitive (bool/int/float/str — no numpy scalars) so
            # `xr.DataTree.equals` against xradar can match.
            nattrs = getattr(metadata, "nexrad_attrs", None)
            if nattrs is not None:
                attrs.update(
                    {
                        "dynamic_scan_type": nattrs.dynamic_scan_type,
                        "mpda_vcp": bool(nattrs.mpda_vcp),
                        "base_tilt_vcp": bool(nattrs.base_tilt_vcp),
                        "num_base_tilts": int(nattrs.num_base_tilts),
                        "vcp_truncated": bool(nattrs.vcp_truncated),
                        "vcp_sequence_active": bool(nattrs.vcp_sequence_active),
                        "number_elevation_cuts": int(nattrs.number_elevation_cuts),
                        "doppler_velocity_resolution": float(nattrs.doppler_velocity_resolution),
                        "vcp_pulse_width": nattrs.vcp_pulse_width,
                        "avset_enabled": bool(nattrs.avset_enabled),
                        "ebc_enabled": bool(nattrs.ebc_enabled),
                        "super_res_status": int(nattrs.super_res_status),
                        "rda_build_number": int(nattrs.rda_build_number),
                        "operational_mode": int(nattrs.operational_mode),
                        "actual_elevation_cuts": int(nattrs.actual_elevation_cuts),
                    }
                )
            # 0-d root data_vars that xradar exposes alongside the latitude/
            # longitude/altitude coords. NEXRAD is always a fixed ground
            # radar, so `instrument_type = 'radar'` is hard-coded; the
            # rest come from `VolumeMetadata`.
            data_vars.update(
                {
                    "volume_number": ((), int(metadata.volume_number)),
                    "platform_type": ((), str(metadata.platform_type)),
                    "instrument_type": ((), "radar"),
                    "time_coverage_start": ((), str(metadata.time_coverage_start)),
                    "time_coverage_end": ((), str(metadata.time_coverage_end)),
                }
            )
        elif fmt == "sigmet":
            # Mirror xradar's `open_iris_datatree` root attrs verbatim:
            # only the standard CF-style strings, no IRIS-specific PRF /
            # Nyquist / task fields. xradar drops those from the
            # DataTree entirely; users who want them typed reach for
            # `radish.read_sigmet(path).metadata.sigmet_attrs`.
            extra = dict(metadata.attributes) if getattr(metadata, "attributes", None) else {}
            attrs.update(
                {
                    "Conventions": "None",
                    "version": "None",
                    "title": "None",
                    "institution": "None",
                    "references": "None",
                    "source": "None",
                    "history": "None",
                    "comment": "im/exported using radish",
                    "scan_name": extra.get("scan_name", ""),
                }
            )
            data_vars.update(
                {
                    "volume_number": ((), int(metadata.volume_number)),
                    "platform_type": ((), str(metadata.platform_type)),
                    "instrument_type": ((), "radar"),
                    "time_coverage_start": ((), str(metadata.time_coverage_start)),
                    "time_coverage_end": ((), str(metadata.time_coverage_end)),
                }
            )
        else:
            attrs["Conventions"] = "CF/Radial"
        return xr.Dataset(data_vars=data_vars, coords=coords, attrs=attrs)

    def _sweep_to_dataset(self, sweep, volume_metadata) -> "xr.Dataset":
        """Convert a sweep to an xarray Dataset matching xradar's NEXRAD shape.

        Concretely:

        * ray dim is ``azimuth`` (xradar/ODIM convention), not ``time``;
        * ``azimuth`` and ``elevation`` are float64 coords on the ``azimuth`` dim;
        * ``time`` is a ``datetime64[ns]`` coord on the ``azimuth`` dim;
        * ``range`` is a float32 coord on the ``range`` dim with the standard
          radar attrs (``meters_between_gates``, ``meters_to_center_of_first_gate``,
          ``spacing_is_constant``);
        * each moment is a float64 ``(azimuth, range)`` DataArray with
          ``units``/``standard_name``/``long_name`` matching xradar verbatim
          (see ``mapping.rs::moment_meta``);
        * ``sweep_mode``, ``sweep_number``, ``sweep_fixed_angle``, ``prt_mode``,
          and ``follow_mode`` are 0-d data variables (FM301-style scalar vars).

        Per-sweep MSG_5 attrs (``waveform_type``, ``channel_config``,
        ``super_resolution``, ``sails_cut``, ``mrle_cut``, ``mpda_cut``,
        ``base_tilt_cut``) are deferred to a follow-up commit that drops to
        ``nexrad-decode`` for the VCP message.
        """
        # `sweep.time` is float64 seconds-since-epoch from PyO3; xradar wants
        # datetime64[ns] for the time coord. pandas' to_datetime handles the
        # conversion crisply (xarray hard-depends on pandas anyway).
        time_dt = pd.to_datetime(np.asarray(sweep.time, dtype=np.float64), unit="s").to_numpy()

        coords = {
            "azimuth": (
                ["azimuth"],
                np.asarray(sweep.azimuth, dtype=np.float64),
                {
                    "standard_name": "ray_azimuth_angle",
                    "long_name": "azimuth_angle_from_true_north",
                    "units": "degrees",
                    "axis": "radial_azimuth_coordinate",
                },
            ),
            "elevation": (
                ["azimuth"],
                np.asarray(sweep.elevation, dtype=np.float64),
                {
                    "standard_name": "ray_elevation_angle",
                    "long_name": "elevation_angle_from_horizontal_plane",
                    "units": "degrees",
                    "axis": "radial_elevation_coordinate",
                },
            ),
            "time": (["azimuth"], time_dt, {"standard_name": "time"}),
            "range": (
                ["range"],
                np.asarray(sweep.range, dtype=np.float32),
                {
                    "units": "meters",
                    "standard_name": "projection_range_coordinate",
                    "long_name": "range_to_measurement_volume",
                    "axis": "radial_range_coordinate",
                    "meters_between_gates": np.float32(
                        sweep.range[1] - sweep.range[0] if len(sweep.range) > 1 else 0.0
                    ),
                    "spacing_is_constant": "true",
                    "meters_to_center_of_first_gate": np.float32(sweep.range[0]),
                },
            ),
        }

        data_vars: Dict[str, Any] = {}
        for moment_name in sweep.moment_names():
            moment = sweep.get_moment(moment_name)
            if moment is None:
                continue
            data_vars[moment_name] = (
                ["azimuth", "range"],
                moment.data().astype(np.float64),
                _moment_cf_attrs(moment),
            )

        # FM301 scalar sweep variables (also matches xradar).
        data_vars["sweep_mode"] = ((), sweep.sweep_mode)
        data_vars["sweep_number"] = ((), int(sweep.sweep_number))
        data_vars["sweep_fixed_angle"] = ((), float(sweep.fixed_angle))
        data_vars["prt_mode"] = ((), sweep.prt_mode)
        data_vars["follow_mode"] = ((), sweep.follow_mode)

        # Sweep-level attrs from MSG_5 elevation cut. xradar emits these via
        # `_assign_sweep_attrs`; we populate the same 9 keys with the same
        # types (str / int / bool) so `set(rd_sweep.attrs) == set(xd_sweep.attrs)`.
        sweep_attrs: Dict[str, Any] = {}
        nattrs = getattr(sweep, "nexrad_attrs", None)
        if nattrs is not None:
            sweep_attrs = {
                "waveform_type": nattrs.waveform_type,
                "channel_config": nattrs.channel_config,
                "super_resolution": int(nattrs.super_resolution),
                "sails_cut": bool(nattrs.sails_cut),
                "sails_sequence_number": int(nattrs.sails_sequence_number),
                "mrle_cut": bool(nattrs.mrle_cut),
                "mrle_sequence_number": int(nattrs.mrle_sequence_number),
                "mpda_cut": bool(nattrs.mpda_cut),
                "base_tilt_cut": bool(nattrs.base_tilt_cut),
            }
        # Sigmet has no per-sweep extras to surface as `Dataset.attrs`:
        # `sweep_mode` and `sweep_fixed_angle` already live in `data_vars`
        # via the FM301 scalar convention above, so duplicating them
        # into `.attrs` would diverge from `xradar.io.open_iris_datatree`
        # (which leaves the per-sweep `.attrs` empty for IRIS files).
        # The typed `sweep.sigmet_attrs` accessor is still reachable for
        # callers that want lower-level access without xarray.
        return xr.Dataset(data_vars=data_vars, coords=coords, attrs=sweep_attrs)

    @classmethod
    def guess_can_open(cls, filename_or_obj):
        """Return True if radish can open this file (any registered backend)."""
        from radish import detect_backend

        return detect_backend(filename_or_obj) is not None


# For backwards compatibility
RadishBackend = RadishBackendEntrypoint
