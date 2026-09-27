"""Unified entry points: ``radish.open_datatree`` / ``radish.open_dataset``.

One canonical entry takes any input shape (path, raw bytes, file-like,
chunk list) and any backend (auto-detected or explicit ``backend="nexrad"``)
and produces an ``xarray.DataTree`` or ``xarray.Dataset``. The
``xr.open_datatree(path, engine="radish")`` plugin entry-point delegates
into the same code path so existing callers keep working.

Adding a new format backend is now a two-step ritual:

1. Implement the Rust ``RadarBackend`` trait (in particular
   ``can_read_bytes`` for the in-memory path).
2. Register the backend's per-shape Rust→Python reader entries in
   ``_DISPATCH`` below. No new Python helpers, no new top-level names.
"""

from __future__ import annotations

import os
import warnings
from typing import Any, Callable, Dict, Iterable, Optional, Tuple

from radish._radish import (
    auto_backend_name,
    auto_backend_name_for_bytes,
    read_cfradial1,
    read_nexrad,
    read_nexrad_bytes,
    read_nexrad_chunks,
    read_sigmet,
    read_sigmet_bytes,
    scan_cfradial1,
    scan_nexrad,
    scan_nexrad_bytes,
    scan_nexrad_chunks,
    scan_sigmet,
)

# Type aliases for input shapes the dispatcher recognises.
InputShape = str  # one of the constants below
SHAPE_PATH = "path"
SHAPE_BYTES = "bytes"
SHAPE_FILELIKE = "file-like"
# `chunk-list` is intentionally restrictive: we accept only `list` / `tuple`,
# not arbitrary iterables. Peeking at element 0 to classify the input would
# consume a generator's first chunk; rejecting generators up front keeps the
# contract honest. Callers with a generator must `list(...)` it first.
SHAPE_CHUNKS = "chunk-list"

# How many bytes to peek when sniffing a file-like or buffer. 16 covers
# every magic we currently sniff (HDF5 = 8, AR2V = 4, gzip = 2).
_PEEK_BYTES = 16


def _is_bytes_like(obj: Any) -> bool:
    return isinstance(obj, (bytes, bytearray, memoryview))


def _is_path_like(obj: Any) -> bool:
    return isinstance(obj, str) or hasattr(obj, "__fspath__")


def _is_file_like(obj: Any) -> bool:
    """A `.read()`-having object that isn't already a path-like wrapper."""
    return hasattr(obj, "read") and not _is_path_like(obj)


def _is_chunk_list(obj: Any) -> bool:
    """A non-empty ``list`` / ``tuple`` whose first element is bytes or path-like.

    We deliberately accept only **materialized sequences** (``list`` / ``tuple``)
    rather than arbitrary iterables: classifying an iterable requires peeking
    at the first element, and that peek would consume a generator. Forcing
    callers to pass a list makes the contract explicit (no hidden chunk
    drops), and the cost is one ``list(...)`` call at the boundary if their
    source is a generator.
    """
    if not isinstance(obj, (list, tuple)):
        return False
    if not obj:
        return False
    first = obj[0]
    return _is_bytes_like(first) or _is_path_like(first)


def _classify_shape(input_obj: Any) -> InputShape:
    """Return one of the SHAPE_* constants for ``input_obj``."""
    if _is_bytes_like(input_obj):
        return SHAPE_BYTES
    if _is_path_like(input_obj):
        return SHAPE_PATH
    if _is_file_like(input_obj):
        return SHAPE_FILELIKE
    if _is_chunk_list(input_obj):
        return SHAPE_CHUNKS
    raise TypeError(
        f"Unsupported input type for radish.open_datatree: {type(input_obj).__name__}. "
        "Expected path-like (str / os.PathLike), bytes-like, file-like with "
        ".read(), or a list/tuple of bytes/paths. (Generator inputs are "
        "rejected explicitly because classification needs to peek at the "
        "first element — wrap your generator in `list(...)` first.)"
    )


def _materialize_chunks(chunks: Any) -> "list[bytes]":
    """Collapse a chunk list/tuple into a list of `bytes`, reading paths eagerly.

    Caller (``_classify_shape``) has already confirmed ``chunks`` is a
    ``list``/``tuple`` whose first element is bytes/path-like; we still
    validate every element here so a heterogeneous list raises before
    we hand truncated input to the Rust decoder.
    """
    out: list[bytes] = []
    for c in chunks:
        if _is_bytes_like(c):
            out.append(bytes(c))
        elif _is_path_like(c):
            with open(os.fspath(c), "rb") as f:
                out.append(f.read())
        else:
            raise TypeError(f"Each chunk must be bytes or a path; got {type(c).__name__}")
    return out


def _peek_filelike(file_obj: Any) -> bytes:
    """Read up to `_PEEK_BYTES` from a file-like and rewind so the next read
    sees the same buffer. Falls back to consuming the stream if `seek` fails.
    """
    head = file_obj.read(_PEEK_BYTES)
    if hasattr(file_obj, "seek"):
        try:
            file_obj.seek(-len(head), 1)
        except (OSError, ValueError):
            # Non-seekable file-like (some HTTP responses, etc.) — caller
            # ends up reading the rest of the stream and we'd lose `head`.
            # Reattach the prefix by wrapping in BytesIO downstream.
            return head
    return head


def _sniff_backend(input_obj: Any, shape: InputShape) -> Optional[str]:
    """Run the per-shape Rust auto-detect for an already-classified input.

    Single source of truth for "which backend can read this input?"
    Both :func:`detect_backend` (which swallows failures and returns
    ``None``) and :func:`_read_volume` (which propagates so the caller
    sees a real error) call this helper.

    May raise ``RuntimeError`` from the Rust side when no backend matches
    a path / bytes prefix; chunk lists short-circuit to ``"nexrad_level2"``
    because chunk streams are NEXRAD-specific.
    """
    if shape == SHAPE_PATH:
        return auto_backend_name(os.fspath(input_obj))
    if shape == SHAPE_BYTES:
        return auto_backend_name_for_bytes(bytes(input_obj[:_PEEK_BYTES]))
    if shape == SHAPE_FILELIKE:
        return auto_backend_name_for_bytes(_peek_filelike(input_obj))
    if shape == SHAPE_CHUNKS:
        return "nexrad_level2"
    return None


def detect_backend(input_obj: Any) -> Optional[str]:
    """Identify which radish backend will own this input.

    Returns the canonical backend name (``"nexrad_level2"`` /
    ``"cfradial1"``) or ``None`` when no backend recognises the input.
    Useful for routing decisions outside the open path. Doesn't decode.
    """
    try:
        shape = _classify_shape(input_obj)
    except TypeError:
        return None
    try:
        return _sniff_backend(input_obj, shape)
    except RuntimeError:
        return None


# Dispatch table: (backend_name, input_shape) -> reader callable returning
# `radish.VolumeData`. Each cell is one Rust→Python entry point. Adding a
# format means appending its rows here.
_DISPATCH: Dict[Tuple[str, InputShape], Callable[..., Any]] = {
    # NEXRAD rows accept **kw so `_read_volume` can thread
    # `incomplete_sweep` through; the other backends have no
    # partial-volume semantics and keep single-argument readers.
    ("nexrad_level2", SHAPE_PATH): lambda obj, **kw: read_nexrad(os.fspath(obj), **kw),
    ("nexrad_level2", SHAPE_BYTES): lambda obj, **kw: read_nexrad_bytes(bytes(obj), **kw),
    ("nexrad_level2", SHAPE_FILELIKE): lambda obj, **kw: read_nexrad_bytes(obj.read(), **kw),
    ("nexrad_level2", SHAPE_CHUNKS): lambda obj, **kw: read_nexrad_chunks(
        _materialize_chunks(obj), **kw
    ),
    ("cfradial1", SHAPE_PATH): lambda obj: read_cfradial1(os.fspath(obj)),
    ("sigmet", SHAPE_PATH): lambda obj: read_sigmet(os.fspath(obj)),
    ("sigmet", SHAPE_BYTES): lambda obj: read_sigmet_bytes(bytes(obj)),
    ("sigmet", SHAPE_FILELIKE): lambda obj: read_sigmet_bytes(obj.read()),
}

# The one high-level default for NEXRAD's partial-volume policy —
# referenced by both the read path and the warning path so they can
# never drift apart. "drop" mirrors xradar #332.
_DEFAULT_INCOMPLETE_SWEEP = "drop"

# Backends whose readers accept the `incomplete_sweep` keyword. Every
# other backend has no partial-volume semantics and rejects an explicit
# value. New per-backend reader kwargs extend this set + the `**kw`
# rows in `_DISPATCH`, not the shared `_read_volume` body.
_POLICY_BACKENDS = frozenset({"nexrad_level2"})

# Metadata-only twin of `_DISPATCH`. Returns `VolumeMetadata` instead
# of `VolumeData` — same input-shape detection, ~3× faster on the
# Rust side because it skips per-ray moment materialization.
#
# cfradial1 / sigmet bytes/file-like rows are intentionally absent:
# cfradial1 has no in-memory netCDF open path (libnetcdf doesn't
# expose one cleanly), and sigmet's metadata-only path doesn't yet
# have a `scan_sigmet_bytes` PyO3 entry. Falling through to the
# clear-message ValueError in `_scan_volume` is the right behaviour
# until those rows can be added.
_SCAN_DISPATCH: Dict[Tuple[str, InputShape], Callable[[Any], Any]] = {
    ("nexrad_level2", SHAPE_PATH): lambda obj: scan_nexrad(os.fspath(obj)),
    ("nexrad_level2", SHAPE_BYTES): lambda obj: scan_nexrad_bytes(bytes(obj)),
    ("nexrad_level2", SHAPE_FILELIKE): lambda obj: scan_nexrad_bytes(obj.read()),
    ("nexrad_level2", SHAPE_CHUNKS): lambda obj: scan_nexrad_chunks(_materialize_chunks(obj)),
    ("cfradial1", SHAPE_PATH): lambda obj: scan_cfradial1(os.fspath(obj)),
    ("sigmet", SHAPE_PATH): lambda obj: scan_sigmet(os.fspath(obj)),
}


def _read_volume(input_obj: Any, backend: Optional[str], incomplete_sweep: Optional[str] = None):
    """Pick the right Rust reader for `(backend, shape)` and decode.

    ``incomplete_sweep`` (``"drop"`` / ``"pad"`` / ``"keep"``) applies to
    the NEXRAD backend only, where ``None`` means the xradar-parity
    default ``"drop"``. Passing an explicit value with any other backend
    raises — those formats have no partial-volume semantics.
    """
    shape = _classify_shape(input_obj)
    if backend is None:
        backend = _sniff_backend(input_obj, shape)
    if backend is None:
        # `_sniff_backend` returns None only for unknown shapes, which
        # `_classify_shape` would already have rejected — keep the guard
        # for mypy and as a defence-in-depth check, but make the message
        # actionable in case it ever fires.
        raise ValueError(
            f"radish could not auto-detect a backend for input shape {shape!r}. "
            f"Pass `backend='nexrad'` or `backend='cfradial1'` explicitly."
        )
    reader = _DISPATCH.get((backend, shape))
    if reader is None:
        raise ValueError(
            f"radish backend {backend!r} does not support input shape {shape!r}. "
            f"(Common: cfradial1 only accepts paths — `libnetcdf` doesn't expose "
            f"an in-memory open. Pass a path or use NEXRAD bytes/chunks instead.)"
        )
    if backend in _POLICY_BACKENDS:
        return reader(input_obj, incomplete_sweep=incomplete_sweep or _DEFAULT_INCOMPLETE_SWEEP)
    if incomplete_sweep is not None:
        raise ValueError(
            f"incomplete_sweep is only supported by the NEXRAD backend; "
            f"backend {backend!r} has no partial-volume semantics."
        )
    return reader(input_obj)


def _warn_dropped_sweeps(volume: Any, incomplete_sweep: Optional[str]) -> None:
    """xradar-#332-parity warnings for the default ``"drop"`` mode."""
    if (incomplete_sweep or _DEFAULT_INCOMPLETE_SWEEP) != "drop":
        return
    dropped = sorted(volume.incomplete_sweeps)
    if not dropped:
        return
    warnings.warn(
        f"Dropped {len(dropped)} incomplete sweep(s): {dropped}. "
        f"Use incomplete_sweep='pad' to include them with NaN-filled rays.",
        UserWarning,
        stacklevel=3,
    )
    if volume.num_sweeps == 0:
        warnings.warn(
            "All sweeps are incomplete. Returning a root-only DataTree.",
            UserWarning,
            stacklevel=3,
        )


def _scan_volume(input_obj: Any, backend: Optional[str]):
    """Pick the right Rust scanner for `(backend, shape)` and extract metadata.

    Metadata-only twin of :func:`_read_volume`. Same shape-detection
    + backend-sniff path; different dispatch table that calls the
    `scan_*` PyO3 entries instead of the `read_*` ones. Returns
    `VolumeMetadata` (not `VolumeData`) — no per-ray moment data.
    """
    shape = _classify_shape(input_obj)
    if backend is None:
        backend = _sniff_backend(input_obj, shape)
    if backend is None:
        raise ValueError(
            f"radish could not auto-detect a backend for input shape {shape!r}. "
            f"Pass `backend='nexrad'` or `backend='cfradial1'` explicitly."
        )
    scanner = _SCAN_DISPATCH.get((backend, shape))
    if scanner is None:
        raise ValueError(
            f"radish.scan: backend {backend!r} does not support input "
            f"shape {shape!r}. (cfradial1/sigmet bytes/chunks scan paths "
            f"are not implemented yet — fall back to "
            f"`radish.open_datatree(obj).attrs` if you need metadata "
            f"from those formats over an in-memory buffer.)"
        )
    return scanner(input_obj)


def open_datatree(
    filename_or_obj: Any,
    backend: Optional[str] = None,
    *,
    drop_variables: Optional[Iterable[str]] = None,
    incomplete_sweep: Optional[str] = None,
) -> "Any":  # xarray.DataTree
    """Open a radar volume as an ``xarray.DataTree``.

    Parameters
    ----------
    filename_or_obj
        One of: a path-like (``str`` / ``os.PathLike``), raw ``bytes`` /
        ``bytearray`` / ``memoryview``, a file-like with ``.read()``, or a
        ``list`` / ``tuple`` of bytes/paths (NEXRAD chunk stream). Generators
        are rejected — wrap with ``list(...)`` first.
    backend
        ``"nexrad_level2"`` (or alias ``"nexrad"``) or ``"cfradial1"``.
        ``None`` (default) auto-detects from the input.
    drop_variables
        Reserved for API parity with xarray's plugin ``open_datatree``;
        currently unused.
    incomplete_sweep
        NEXRAD only — what to do with sweeps whose data is provably
        partial (real-time chunk streams / truncated volumes).
        ``None`` (default) behaves as ``"drop"``: incomplete sweeps are
        omitted with a warning, and surviving sweeps keep their original
        ``sweep_N`` names (gaps stay gaps — xradar #332 parity).
        ``"pad"`` reindexes them onto the full 360/720-ray azimuth grid
        with NaN-filled missing rays. ``"keep"`` passes them through
        as-is with fewer rays (flagged via each sweep's ``is_complete``).
        Passing an explicit value with a non-NEXRAD backend raises
        ``ValueError``.

    Returns
    -------
    xarray.DataTree

    Examples
    --------
    The ``radish.open_datatree`` API mirrors ``xr.open_datatree(path,
    engine="radish")`` but accepts in-memory inputs and chunk lists too::

        radish.open_datatree("KLOT20260310_231412_V06")
        radish.open_datatree(open("file.gz", "rb").read())     # bytes
        radish.open_datatree([s_bytes, i01_bytes, e_bytes])    # chunks
        radish.open_datatree("foo.nc", backend="cfradial1")    # explicit
    """
    backend = _normalize_backend(backend)
    volume = _read_volume(filename_or_obj, backend, incomplete_sweep)
    _warn_dropped_sweeps(volume, incomplete_sweep)
    return _build_datatree(volume, _format_for_root(backend or _infer_backend_from_volume(volume)))


def open_dataset(
    filename_or_obj: Any,
    backend: Optional[str] = None,
    *,
    group: Optional[str] = None,
    drop_variables: Optional[Iterable[str]] = None,
    incomplete_sweep: Optional[str] = None,
) -> "Any":  # xarray.Dataset
    """Open a single sweep as an ``xarray.Dataset``. Use ``open_datatree`` for
    multi-sweep volumes.

    See :func:`open_datatree` for input-shape, ``backend``, and
    ``incomplete_sweep`` semantics. ``group`` refers to *original*
    sweep names: with the default ``incomplete_sweep="drop"`` on a
    truncated NEXRAD volume, ``group="sweep_2"`` still means the sweep
    originally at index 2 even when ``sweep_1`` was dropped.
    """
    backend = _normalize_backend(backend)
    volume = _read_volume(filename_or_obj, backend, incomplete_sweep)
    # Lazy import to break the radish ↔ xarray_backend module cycle.
    from radish.backends.xarray_backend import RadishBackendEntrypoint

    # Resolve `group` against the volume's sweep names so dropped
    # sweeps leave gaps instead of silently shifting every later index.
    if not group:
        sweep_idx = 0
    else:
        sweep_names = list(volume.metadata.sweep_group_names)
        try:
            sweep_idx = sweep_names.index(group)
        except ValueError:
            raise ValueError(
                f"sweep group {group!r} not present; available: {sweep_names}"
            ) from None
    sweep = volume.get_sweep(sweep_idx)
    return RadishBackendEntrypoint()._sweep_to_dataset(sweep, volume.metadata)


def scan(
    filename_or_obj: Any,
    backend: Optional[str] = None,
) -> "Any":  # radish.VolumeMetadata
    """Scan a radar volume's metadata without per-ray decode.

    Format-agnostic dispatcher mirroring :func:`open_datatree` — accepts
    the same heterogeneous input shapes (path, bytes, file-like, chunk
    list) and returns a :class:`radish.VolumeMetadata` (instrument
    name, lat/lon, time coverage, sweep fixed angles, plus
    format-specific extras like NEXRAD's MSG_2/MSG_5 attrs).
    Equivalent to :func:`open_datatree` minus the per-sweep
    ``Array2<f32>`` materialization — ~3× faster on the same input.

    Parameters
    ----------
    filename_or_obj
        Same input shapes as :func:`open_datatree`: a path-like
        (``str`` / ``os.PathLike``), raw ``bytes`` / ``bytearray`` /
        ``memoryview``, a file-like with ``.read()``, or a ``list`` /
        ``tuple`` of bytes/paths (NEXRAD chunk stream). Generators
        are rejected — wrap with ``list(...)`` first.

        **Compression**: radish is compression-agnostic. Bytes-shaped
        inputs must be already-decompressed AR2V bytes. For ``.gz``
        archives, use fsspec's transparent decompression filter
        (``fsspec.open(uri, "rb", compression="gzip")``) or
        ``gzip.decompress(raw)`` before passing in.

    backend
        ``"nexrad_level2"`` (or alias ``"nexrad"``), ``"cfradial1"``,
        or ``"sigmet"``. ``None`` (default) auto-detects from the
        input.

    Returns
    -------
    radish.VolumeMetadata

    Examples
    --------
    The canonical use case is fast metadata extraction at scale on
    cloud storage. Pre-fetch bytes once via fsspec (or obstore), then
    let radish parse them — no S3 client bundled in radish-rs::

        import fsspec, radish

        # Local file
        md = radish.scan("KXXX...V06")

        # S3 via fsspec — auto-decompresses .gz transparently
        with fsspec.open(
            "s3://noaa-nexrad-level2/2011/05/20/KVNX/KVNX20110520_000442_V06.gz",
            "rb",
            compression="gzip",
            anon=True,
        ) as f:
            md = radish.scan(f)

        # Equivalently — call .read() yourself, pass bytes
        with fsspec.open(uri, "rb", compression="gzip", anon=True) as f:
            md = radish.scan(f.read())

        # obstore — no built-in decompression; caller decompresses
        import gzip
        import obstore as obs
        store = obs.store.S3Store("noaa-nexrad-level2", region="us-east-1", anonymous=True)
        raw = obs.get(store, "2011/05/20/KVNX/KVNX20110520_000442_V06.gz").bytes()
        md = radish.scan(gzip.decompress(raw))

        # obstore-as-fsspec backend — best of both worlds
        # (obstore's S3 perf + fsspec's transparent compression filter)
        from obstore.fsspec import register
        register()  # makes obstore the default for s3:// in fsspec
        with fsspec.open(uri, "rb", compression="gzip", anon=True) as f:
            md = radish.scan(f.read())

        # Chunk stream (unidata-nexrad-level2-chunks)
        chunks = [open(p, "rb").read() for p in [s_chunk, i01, ..., e_chunk]]
        md = radish.scan(chunks)

    Notes
    -----
    For S3-hosted bulk-ingest workflows, ``radish.scan(s3_bytes)`` is
    ~10× faster than the closest xradar equivalent
    (``NEXRADLevel2File(path, loaddata=False)`` + fsspec). On a
    KLOT V06 fixture the per-file timing is ~150 ms (radish) vs
    ~1,432 ms (xradar). The savings compound on multi-year backfills
    — see the radish CHANGELOG for measured numbers.
    """
    backend = _normalize_backend(backend)
    return _scan_volume(filename_or_obj, backend)


# ---- internals -----------------------------------------------------------


_BACKEND_ALIASES: Dict[str, str] = {
    "nexrad": "nexrad_level2",
    "nexrad_level2": "nexrad_level2",
    "cfradial1": "cfradial1",
    "sigmet": "sigmet",
    "iris": "sigmet",
}


def _normalize_backend(backend: Optional[str]) -> Optional[str]:
    if backend is None:
        return None
    canonical = _BACKEND_ALIASES.get(backend.lower())
    if canonical is None:
        valid = ", ".join(sorted(set(_BACKEND_ALIASES.values())))
        raise ValueError(f"Unknown backend {backend!r}. Valid: {valid}")
    return canonical


def _format_for_root(backend_name: Optional[str]) -> str:
    """Translate a backend name to the format key used by
    `_create_root_dataset`. The xarray backend switches on the
    short format string for per-format root attrs."""
    if backend_name is None:
        return "cfradial1"  # safe default — produces minimal root
    if backend_name == "nexrad_level2":
        return "nexrad"
    if backend_name == "sigmet":
        return "sigmet"
    return backend_name


def _infer_backend_from_volume(volume: Any) -> str:
    """Best-effort backend identification when we never sniffed (chunk-
    list inputs go straight to read_nexrad_chunks). Matches the
    `_format_for_root` contract.
    """
    if getattr(volume.metadata, "nexrad_attrs", None) is not None:
        return "nexrad_level2"
    if getattr(volume.metadata, "sigmet_attrs", None) is not None:
        return "sigmet"
    return "cfradial1"


def _build_datatree(volume: Any, fmt: str):
    """Build an `xr.DataTree` from a `VolumeData`. Reuses the xarray
    backend's existing `_volume_to_datatree` helper so the dim/coord/attr
    layout is identical to `xr.open_datatree(path, engine="radish")`.
    """
    from radish.backends.xarray_backend import RadishBackendEntrypoint

    return RadishBackendEntrypoint()._volume_to_datatree(volume, fmt)


__all__ = ["open_datatree", "open_dataset", "scan", "detect_backend"]
