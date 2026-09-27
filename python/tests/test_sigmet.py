"""End-to-end tests for the Sigmet/IRIS RAW backend.

Gated on `RADISH_SIGMET_FIXTURE`; see `conftest.py::sigmet_fixture`.
"""

import numpy as np
import pytest

import radish


def test_read_sigmet_returns_volume(sigmet_fixture):
    vol = radish.read_sigmet(sigmet_fixture)
    assert vol.num_sweeps >= 1
    md = vol.metadata
    assert md.instrument_name  # non-empty
    assert -90 <= md.latitude <= 90
    assert -180 <= md.longitude <= 180
    s0 = vol.get_sweep(0)
    assert s0.num_rays > 0
    assert s0.num_gates > 0
    # Sigmet typically writes at least one of DBZH / DBTH.
    moments = s0.moment_names()
    assert any(m in moments for m in ("DBZH", "DBTH"))


def test_scan_sigmet_returns_metadata(sigmet_fixture):
    md = radish.scan_sigmet(sigmet_fixture)
    assert md.instrument_name
    assert md.num_sweeps >= 1


def test_xarray_engine_radish_dispatches_to_sigmet(sigmet_fixture):
    xr = pytest.importorskip("xarray")
    dt = xr.open_datatree(sigmet_fixture, engine="radish")
    sweep_keys = [k for k in dt.children if k.startswith("sweep_")]
    assert len(sweep_keys) >= 1
    s0 = dt[sweep_keys[0]]
    assert "azimuth" in s0.coords
    assert "elevation" in s0.coords
    assert "range" in s0.coords
    # IRIS-shaped root attrs (xradar's open_iris_datatree contract):
    # only standard CF strings, no IRIS-specific PRF/Nyquist/task fields
    # (those are reachable via `vol.metadata.sigmet_attrs` instead).
    assert dt.attrs.get("Conventions") == "None"
    assert dt.attrs.get("scan_name") == "VOL_A"
    # IRIS-specific metadata lives in the typed `sigmet_attrs` accessor,
    # not as Dataset.attrs keys (that would diverge from xradar).
    assert "task_name" not in dt.attrs
    assert "iris_version" not in dt.attrs
    # Per-sweep `sweep_mode` and `sweep_fixed_angle` are FM301 0-d
    # data_vars (matching xradar's IRIS shape), not Dataset.attrs —
    # `.attrs` stays empty for sigmet sweeps for parity.
    assert s0.attrs == {}, f"sigmet per-sweep attrs should be empty, got {dict(s0.attrs)}"
    assert str(s0["sweep_mode"].values) in ("azimuth_surveillance", "rhi")
    assert float(s0["sweep_fixed_angle"].values) >= 0.0


def test_radish_open_datatree_path(sigmet_fixture):
    """`radish.open_datatree(path)` auto-detects sigmet."""
    pytest.importorskip("xarray")
    dt = radish.open_datatree(sigmet_fixture)
    assert any(k.startswith("sweep_") for k in dt.children)
    # Sigmet auto-detects through magic-byte sniff; verify the resulting
    # tree carries the IRIS-shaped `scan_name` (radish copies it from
    # the volume's `scan_name` attribute), and `scan_mode` is reachable
    # through the typed `sigmet_attrs` rather than Dataset.attrs.
    assert dt.attrs.get("scan_name")  # non-empty
    vol = radish.read_sigmet(sigmet_fixture)
    assert vol.metadata.sigmet_attrs.scan_mode in ("PPI", "RHI", "OTHER")


def test_radish_open_datatree_bytes(sigmet_fixture):
    """`radish.open_datatree(<bytes>)` auto-detects sigmet via magic bytes."""
    pytest.importorskip("xarray")
    with open(sigmet_fixture, "rb") as f:
        buf = f.read()
    dt = radish.open_datatree(buf)
    assert any(k.startswith("sweep_") for k in dt.children)


def test_detect_backend_sigmet(sigmet_fixture):
    assert radish.detect_backend(sigmet_fixture) == "sigmet"


def test_sigmet_volume_attrs_populated(sigmet_fixture):
    """SigmetVolumeAttrs carries the IRIS metadata block."""
    vol = radish.read_sigmet(sigmet_fixture)
    sattrs = vol.metadata.sigmet_attrs
    assert sattrs is not None
    assert isinstance(sattrs.task_name, str)
    assert sattrs.iris_version  # non-empty for valid IRIS files
    assert sattrs.scan_mode in ("PPI", "RHI", "OTHER")
    # PRF and unambiguous range are positive when wavelength was extracted;
    # either way, they should be finite (no inf/nan from a parse error).
    assert np.isfinite(sattrs.prf_hz)
    assert np.isfinite(sattrs.unambiguous_range_m)


def test_sigmet_sweep_attrs_populated(sigmet_fixture):
    """SigmetSweepAttrs is filled in on every sweep."""
    vol = radish.read_sigmet(sigmet_fixture)
    s0 = vol.get_sweep(0)
    sweep_attrs = s0.sigmet_attrs
    assert sweep_attrs is not None
    assert sweep_attrs.sweep_mode in ("azimuth_surveillance", "rhi")
    # Fixed angles are degrees in [0, 360).
    assert 0.0 <= sweep_attrs.fixed_angle_deg < 360.0


def test_sigmet_moment_set_matches_xradar(sigmet_fixture):
    """End-to-end parity: every 2-D moment xradar emits, radish emits too,
    and vice versa.

    Promotes the notebook spot-check (`smoke_test_sigmet.ipynb` cell on
    the `radish-only` / `xradar-only` set diff) into a CI-runnable
    regression. Skipped if `xradar` isn't installed.
    """
    xr = pytest.importorskip("xarray")
    xradar = pytest.importorskip("xradar")

    rd = xr.open_datatree(sigmet_fixture, engine="radish")
    # xradar's IRIS reader needs a str path (PosixPath fails on its
    # `_check_iris_file` memmap helper).
    xd = xradar.io.open_iris_datatree(str(sigmet_fixture))

    sweep_keys = sorted(k for k in rd.children if k.startswith("sweep_"))
    assert sweep_keys, "radish produced no sweeps"
    common_sweeps = [k for k in sweep_keys if k in xd.children]
    assert common_sweeps, "no overlapping sweep groups between readers"

    for skey in common_sweeps:
        rd_vars = {v for v in rd[skey].data_vars if rd[skey][v].ndim == 2}
        xd_vars = {v for v in xd[skey].data_vars if xd[skey][v].ndim == 2}
        radish_only = rd_vars - xd_vars
        xradar_only = xd_vars - rd_vars
        assert (
            not radish_only
        ), f"{skey}: radish emits moments xradar doesn't: {sorted(radish_only)}"
        assert not xradar_only, (
            f"{skey}: xradar emits moments radish doesn't: {sorted(xradar_only)} — "
            "likely a missing entry in `SUPPORTED_MOMENTS` or `iris_mapping_ids_match_xradar_table`"
        )


def test_sigmet_time_coordinate_is_absolute_and_matches_xradar(sigmet_fixture):
    """The `time` coordinate must be the absolute acquisition time, not a
    1970-epoch offset (issue #28).

    radish orders rays by azimuth; xradar's IRIS reader emits a different ray
    order, so we compare the *set* of ray times (sorted), not element-wise,
    plus the absolute earliest time. Matching to ~1 ms also exercises the
    YMDS millisecond decode.
    """
    np = pytest.importorskip("numpy")
    xr = pytest.importorskip("xarray")
    xradar = pytest.importorskip("xradar")

    rd = xr.open_datatree(sigmet_fixture, engine="radish")
    xd = xradar.io.open_iris_datatree(str(sigmet_fixture))

    sweep_keys = sorted(k for k in rd.children if k.startswith("sweep_") and k in xd.children)
    assert sweep_keys, "no overlapping sweep groups between readers"

    for skey in sweep_keys:
        rt = np.sort(rd[skey]["time"].values.astype("datetime64[ns]"))
        xt = np.sort(xd[skey]["time"].values.astype("datetime64[ns]"))
        assert rt.shape == xt.shape, f"{skey}: time shape mismatch"
        # Sanity: radish time is no longer pinned to the 1970 epoch.
        assert rt[0] > np.datetime64(
            "2000-01-01"
        ), f"{skey}: radish time[0]={rt[0]} looks epoch-relative (issue #28)"
        # Sorted ray-time multisets must match xradar to within 1 ms.
        delta_ms = np.abs((rt - xt) / np.timedelta64(1, "ms"))
        assert (
            delta_ms.max() <= 1.0
        ), f"{skey}: ray-time set diverges from xradar by up to {delta_ms.max()} ms"


def test_sigmet_moments_match_xradar(sigmet_fixture):
    """Per-moment parity with xradar (issue #28): the finite-cell fraction
    and the full sorted value multiset must match.

    Covers all three fixes: the no-longer-over-masked power/phase moments
    (DBZH/DBTH/ZDR/PHIDP/WRADH/SQIH go from ~6-25% to 100% finite), the
    velocity `raw==0 -> 0.0` parity (VRADH), and the real KDP decode (was a
    raw passthrough). Compared as sorted multisets because radish and xradar
    order rays differently.
    """
    np = pytest.importorskip("numpy")
    xr = pytest.importorskip("xarray")
    xradar = pytest.importorskip("xradar")

    rd = xr.open_datatree(sigmet_fixture, engine="radish")
    xd = xradar.io.open_iris_datatree(str(sigmet_fixture))

    sweep_keys = sorted(k for k in rd.children if k.startswith("sweep_") and k in xd.children)
    assert sweep_keys, "no overlapping sweep groups between readers"

    # The ODIM-mapped moments issue #28 is about. The IRIS-extended /
    # categorical passthrough types (DB_HCLASS, DB_DBTE8, DB_DBZE8) are out
    # of scope here: xradar decodes/keeps them differently and radish routes
    # them through the no-op passthrough decoder — tracked separately.
    MOMENTS = ["DBZH", "DBTH", "VRADH", "ZDR", "RHOHV", "KDP", "PHIDP", "WRADH", "SQIH"]

    for skey in sweep_keys:
        rd_ds, xd_ds = rd[skey].to_dataset(), xd[skey].to_dataset()
        common = [
            v
            for v in MOMENTS
            if v in rd_ds.data_vars and v in xd_ds.data_vars and rd_ds[v].ndim == 2
        ]
        assert common, f"{skey}: no common issue-#28 moments to compare"
        for v in common:
            r = rd_ds[v].values
            x = xd_ds[v].values
            rf = np.isfinite(r).mean()
            xf = np.isfinite(x).mean()
            assert abs(rf - xf) < 0.001, (
                f"{skey}/{v}: finite fraction radish={rf:.3f} xradar={xf:.3f} "
                "— masking/decoding diverges from xradar (issue #28)"
            )
            rs_vals = np.sort(r[np.isfinite(r)])
            xs_vals = np.sort(x[np.isfinite(x)])
            np.testing.assert_allclose(
                rs_vals,
                xs_vals,
                rtol=1e-3,
                atol=1e-3,
                err_msg=f"{skey}/{v}: decoded value multiset differs from xradar",
            )


def test_sigmet_azimuth_coordinate_matches_xradar(sigmet_fixture):
    """The per-ray azimuth label must match xradar, including the ray that
    straddles the 0deg/360deg seam (issue #40).

    A ray crossing north has begin ~= 359.5deg and end ~= 0.5deg; the old
    naive `(begin + end) / 2` put its azimuth at ~180deg — a half-turn off,
    landing it next to a legitimate 180deg ray as a near-duplicate while the
    true ~0deg ray went missing. The circular-midpoint fix folds it back to
    ~0deg. xradar computes this bearing correctly and its azimuth coordinate
    is the same in every xradar release (unaffected by the #357 data-rotation
    bug), so a sorted element-wise comparison is a valid gate against any
    installed xradar. Compared sorted because radish and xradar order rays
    differently.
    """
    np = pytest.importorskip("numpy")
    xr = pytest.importorskip("xarray")
    xradar = pytest.importorskip("xradar")

    rd = xr.open_datatree(sigmet_fixture, engine="radish")
    xd = xradar.io.open_iris_datatree(str(sigmet_fixture))

    sweep_keys = sorted(k for k in rd.children if k.startswith("sweep_") and k in xd.children)
    assert sweep_keys, "no overlapping sweep groups between readers"

    for skey in sweep_keys:
        raz = np.sort(np.asarray(rd[skey]["azimuth"].values, dtype=float))
        xaz = np.sort(np.asarray(xd[skey]["azimuth"].values, dtype=float))
        assert raz.shape == xaz.shape, f"{skey}: azimuth shape mismatch"
        # A 180deg-off seam ray shows up as a ~180deg discrepancy at two
        # sorted positions; 1e-2 deg comfortably passes genuine f32/f64
        # rounding while failing decisively on the #40 bug.
        np.testing.assert_allclose(
            raz,
            xaz,
            atol=1e-2,
            err_msg=f"{skey}: azimuth coordinate diverges from xradar (issue #40)",
        )
