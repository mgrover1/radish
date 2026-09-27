"""Structural parity tests: radish output vs xradar output, same fixture.

These tests pin invariants that should always agree between the two
readers — variable sets, coord names, FM301 scalar vars, attr keys —
without dragging in xradar's internal-implementation tests. The
numerical-parity tests live in `test_nexrad.py` (and any future
sigmet equivalent); this file is structural only.

Each test loops over backends that have BOTH a fixture env var set
AND `xradar` installed. If neither backend is available the entire
test is skipped — they're regression gates, not gates on first install.
"""

from __future__ import annotations

import os
from typing import Any, Callable, List, Tuple

import pytest

import radish

# ---------------------------------------------------------------------------
# Backend selection
# ---------------------------------------------------------------------------
# Each entry: (id, env-var, xradar-open-fn, radish-open-fn).
# The radish-open-fn is paramerised because xradar's open functions take
# a str (PosixPath fails for IRIS), and the parity tests want to call
# both readers identically on the same fixture path.
_BACKENDS: List[Tuple[str, str, str, str]] = [
    ("nexrad", "RADISH_NEXRAD_FIXTURE", "open_nexradlevel2_datatree", "nexrad"),
    ("sigmet", "RADISH_SIGMET_FIXTURE", "open_iris_datatree", "sigmet"),
]


def _xradar_open(open_fn_name: str) -> Callable[[str], Any]:
    xradar = pytest.importorskip("xradar")
    return getattr(xradar.io, open_fn_name)


def _available_backends() -> List[Tuple[str, str, str, str]]:
    """Yield backend tuples whose fixture is reachable. xradar is checked
    inside the test (`pytest.importorskip`) so a missing xradar shows up
    as a skip per-test, not an empty parametrisation."""
    out = []
    for entry in _BACKENDS:
        _, env, _, _ = entry
        path = os.environ.get(env)
        if path and os.path.exists(path):
            out.append(entry)
    return out


def _backend_param(entry: Tuple[str, str, str, str]) -> Any:
    """pytest.param with a readable id."""
    return pytest.param(entry, id=entry[0])


# Module-level skip when no fixture is set at all — saves the noise of
# every parametrised test reporting a separate skip.
_PARAMS = _available_backends()
if not _PARAMS:
    pytestmark = pytest.mark.skip(
        reason="No RADISH_*_FIXTURE env var set; structural parity tests need at least one"
    )


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(params=_PARAMS, ids=lambda e: e[0])
def parity_pair(request):
    """Return `(backend_id, radish_dt, xradar_dt)` for one backend's
    fixture. xradar import failure surfaces as a per-case skip."""
    backend_id, env, open_fn_name, _ = request.param
    xr = pytest.importorskip("xarray")
    open_fn = _xradar_open(open_fn_name)
    path = os.environ[env]

    rd = xr.open_datatree(path, engine="radish")
    xd = open_fn(str(path))
    return backend_id, rd, xd


# ---------------------------------------------------------------------------
# Structural parity tests
# ---------------------------------------------------------------------------


def test_root_attrs_keys_match_xradar(parity_pair):
    """Both readers should expose the same set of keys on the root
    `Dataset.attrs`. A diff here means radish is emitting an attr xradar
    doesn't (or vice versa) — usually a wiring bug in the per-format
    branch of `_create_root_dataset`."""
    backend_id, rd, xd = parity_pair
    rd_keys = set(rd.attrs)
    xd_keys = set(xd.attrs)
    only_rd = rd_keys - xd_keys
    only_xd = xd_keys - rd_keys
    assert not only_rd, f"{backend_id}: radish-only root attrs: {sorted(only_rd)}"
    assert not only_xd, f"{backend_id}: xradar-only root attrs: {sorted(only_xd)}"


def test_sweep_group_names_contiguous_from_zero(parity_pair):
    """Sweep groups must be `sweep_0`, `sweep_1`, … with no gaps and
    no off-by-one. xradar uses this convention; FM301 says nothing
    about it explicitly but every weather-radar tool in the wild
    expects the contiguous-from-zero pattern."""
    backend_id, rd, _xd = parity_pair
    # Compare as a *set* against the expected `sweep_<idx>` set —
    # avoids the lex-vs-numeric sort pitfall (`sweep_10` < `sweep_2`
    # lexicographically) that was breaking this test on volumes with
    # ≥ 10 sweeps. The contract is "every index in 0..n is present
    # exactly once," not "names sort lexicographically."
    sweep_names = {k for k in rd.children if k.startswith("sweep_")}
    n = len(sweep_names)
    expected = {f"sweep_{i}" for i in range(n)}
    missing = expected - sweep_names
    extra = sweep_names - expected
    assert not missing and not extra, (
        f"{backend_id}: sweep names not contiguous-from-zero. "
        f"missing={sorted(missing)} extra={sorted(extra)}"
    )


def test_canonical_coord_names_present_on_every_sweep(parity_pair):
    """Every sweep group must expose the four canonical FM301 coords
    (`azimuth`, `elevation`, `time`, `range`). Missing one breaks
    downstream xarray-radar tooling that assumes the FM301 shape."""
    backend_id, rd, _xd = parity_pair
    required = {"azimuth", "elevation", "time", "range"}
    for sweep_key in (k for k in rd.children if k.startswith("sweep_")):
        coords = set(rd[sweep_key].coords)
        missing = required - coords
        assert not missing, (
            f"{backend_id}: {sweep_key} missing coords {sorted(missing)}; "
            f"present: {sorted(coords)}"
        )


def test_fm301_scalar_data_vars_present_on_every_sweep(parity_pair):
    """Every sweep should carry the five FM301 0-d scalar data
    variables. xradar emits them; we should too. Missing any of these
    breaks tools that switch on `sweep_mode` or use `sweep_number` for
    indexing."""
    backend_id, rd, _xd = parity_pair
    fm301_scalars = {
        "sweep_mode",
        "sweep_number",
        "sweep_fixed_angle",
        "prt_mode",
        "follow_mode",
    }
    for sweep_key in (k for k in rd.children if k.startswith("sweep_")):
        sweep = rd[sweep_key]
        missing = {v for v in fm301_scalars if v not in sweep.data_vars or sweep[v].ndim != 0}
        assert (
            not missing
        ), f"{backend_id}: {sweep_key} missing FM301 scalar data_vars: {sorted(missing)}"


def test_sweep_coord_dtypes_match_xradar(parity_pair):
    """Coord dtypes must align with xradar's conventions:
    `azimuth`/`elevation` → float64, `range` → float32, `time` →
    datetime64. A mismatch means downstream `concat`/`merge` ops will
    silently upcast or fail. Compare on the first sweep."""
    import numpy as np

    backend_id, rd, xd = parity_pair
    sweeps = sorted(k for k in rd.children if k.startswith("sweep_"))
    if not sweeps:
        pytest.skip(f"{backend_id}: no sweeps")

    rd0 = rd[sweeps[0]]
    # `azimuth` and `elevation` must be float64 (xradar convention).
    assert (
        rd0["azimuth"].dtype == np.float64
    ), f"{backend_id}: azimuth dtype {rd0['azimuth'].dtype}, expected float64"
    assert (
        rd0["elevation"].dtype == np.float64
    ), f"{backend_id}: elevation dtype {rd0['elevation'].dtype}, expected float64"
    # `range` is float32 to keep memory low for high-gate-count volumes.
    assert (
        rd0["range"].dtype == np.float32
    ), f"{backend_id}: range dtype {rd0['range'].dtype}, expected float32"
    # `time` is datetime64 (any precision); pin via numpy `np.issubdtype`.
    assert np.issubdtype(
        rd0["time"].dtype, np.datetime64
    ), f"{backend_id}: time dtype {rd0['time'].dtype}, expected datetime64"


def test_moment_set_matches_xradar(parity_pair):
    """The set of 2-D moment variables must agree on every sweep both
    readers expose. Any 'radish-only' moment usually means a wrong ID
    in `SUPPORTED_MOMENTS`; any 'xradar-only' moment means we're
    missing a row.

    This subsumes the older `test_sigmet_moment_set_matches_xradar`;
    the parametrisation runs it for nexrad too. The numerical-parity
    test in `test_nexrad.py` covers the value-level check separately."""
    backend_id, rd, xd = parity_pair
    rd_sweeps = sorted(k for k in rd.children if k.startswith("sweep_"))
    xd_sweeps = sorted(k for k in xd.children if k.startswith("sweep_"))
    common = [k for k in rd_sweeps if k in xd_sweeps]
    assert common, f"{backend_id}: no overlapping sweeps (rd={rd_sweeps}, xd={xd_sweeps})"

    for skey in common:
        rd_vars = {v for v in rd[skey].data_vars if rd[skey][v].ndim == 2}
        xd_vars = {v for v in xd[skey].data_vars if xd[skey][v].ndim == 2}
        only_rd = rd_vars - xd_vars
        only_xd = xd_vars - rd_vars
        assert (
            not only_rd
        ), f"{backend_id} {skey}: radish emits moments xradar doesn't: {sorted(only_rd)}"
        assert (
            not only_xd
        ), f"{backend_id} {skey}: xradar emits moments radish doesn't: {sorted(only_xd)}"


def test_per_moment_units_match_xradar(parity_pair):
    """For every common moment on the first sweep, the `units` attr
    should match xradar's. Diverging units (e.g. dBZ vs dBz) break
    CF-conventions consumers and any plotting tool that auto-labels
    color bars from `units`."""
    backend_id, rd, xd = parity_pair
    sweeps = sorted(k for k in rd.children if k.startswith("sweep_"))
    if not sweeps:
        pytest.skip(f"{backend_id}: no sweeps")
    skey = sweeps[0]
    if skey not in xd.children:
        pytest.skip(f"{backend_id}: xradar lacks {skey}")

    rd0 = rd[skey]
    xd0 = xd[skey]
    common = {v for v in rd0.data_vars if v in xd0.data_vars and rd0[v].ndim == 2}
    if not common:
        pytest.skip(f"{backend_id}: no common 2-D moments in {skey}")

    mismatches = []
    for v in sorted(common):
        rd_units = rd0[v].attrs.get("units", "")
        xd_units = xd0[v].attrs.get("units", "")
        if rd_units != xd_units:
            mismatches.append((v, rd_units, xd_units))

    # If we discover a backend / xradar version where xradar deliberately
    # omits a `units` attr while radish emits one, expand this test to
    # treat that as expected. Today we want strict parity on every
    # common variable.
    assert not mismatches, f"{backend_id}: per-moment units mismatch:\n  " + "\n  ".join(
        f"{v}: radish={r!r}  xradar={x!r}" for v, r, x in mismatches
    )


def test_root_data_vars_match_xradar(parity_pair):
    """Root-level `data_vars` should match xradar's set. xradar's IRIS
    and NEXRAD readers both emit `volume_number`, `platform_type`,
    `instrument_type`, `time_coverage_start`, `time_coverage_end` as
    0-d scalars at the root — radish's `_create_root_dataset` mirrors
    that. A diff here would mean we drifted on the FM301 root shape."""
    backend_id, rd, xd = parity_pair
    rd_vars = set(rd.data_vars)
    xd_vars = set(xd.data_vars)
    only_rd = rd_vars - xd_vars
    only_xd = xd_vars - rd_vars
    assert not only_rd, f"{backend_id}: radish-only root data_vars: {sorted(only_rd)}"
    assert not only_xd, f"{backend_id}: xradar-only root data_vars: {sorted(only_xd)}"


def test_sweep_fixed_angle_matches_xradar(parity_pair):
    """Per-sweep `sweep_fixed_angle` must match xradar to within the
    backend's natural quantization.

    Backgrounds:

    * **NEXRAD**: tightened to 1e-4° after the MSG_5 commanded-vs-
      MSG_31-median fix (`radish/src/backends/nexrad/adapter.rs::fixed_angle_for`).
      The previous divergence was ~0.044° — enough to break downstream
      code comparing against the VCP elevation table (e.g. raw2zarr's
      `check_dynamic_scan` with a 0.05° tolerance).

    * **Sigmet**: tolerated to 0.01°. radish reads the per-sweep
      `IngestDataHeader.fixed_angle` (BIN2-encoded, achieved beam
      angle); xradar reads the operator-commanded angle from a mode-
      dependent task block (`TASK_PPI_SCAN_INFO` for PPI,
      `TASK_RHI_SCAN_INFO` for RHI) we don't fully wire through yet.
      The diff is sub-milli-degree (~0.002°), well below any antenna's
      pointing accuracy and any meaningful VCP step. Wiring the
      mode-dependent commanded angle is a tracked follow-up.
    """
    import numpy as np

    backend_id, rd, xd = parity_pair
    rd_sweeps = sorted(k for k in rd.children if k.startswith("sweep_"))
    xd_sweeps = sorted(k for k in xd.children if k.startswith("sweep_"))
    common = [k for k in rd_sweeps if k in xd_sweeps]
    if not common:
        pytest.skip(f"{backend_id}: no overlapping sweeps")

    # Per-backend tolerance: NEXRAD reads identical values from MSG_5;
    # sigmet's BIN2 quantization noise is ~0.002°.
    atol = {"nexrad": 1e-4, "sigmet": 1e-2}[backend_id]

    mismatches = []
    for skey in common:
        if "sweep_fixed_angle" not in rd[skey].data_vars:
            continue
        if "sweep_fixed_angle" not in xd[skey].data_vars:
            continue
        rd_a = float(rd[skey]["sweep_fixed_angle"].values)
        xd_a = float(xd[skey]["sweep_fixed_angle"].values)
        if not np.isclose(rd_a, xd_a, atol=atol):
            mismatches.append((skey, rd_a, xd_a))

    assert not mismatches, (
        f"{backend_id}: per-sweep `sweep_fixed_angle` differs from xradar "
        f"by more than the backend's tolerance ({atol}°):\n  "
        + "\n  ".join(
            f"{s}: radish={r:.6f}  xradar={x:.6f}  diff={r - x:+.6f}" for s, r, x in mismatches
        )
    )
