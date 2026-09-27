"""End-to-end and parity tests for the NEXRAD Level 2 backend.

These tests are gated on `RADISH_NEXRAD_FIXTURE`; see `conftest.py`.
"""

import numpy as np
import pytest

import radish


def test_read_nexrad_returns_volume(nexrad_fixture):
    vol = radish.read_nexrad(nexrad_fixture)
    assert vol.num_sweeps >= 5
    md = vol.metadata
    assert len(md.instrument_name) == 4
    assert -90 <= md.latitude <= 90
    assert -180 <= md.longitude <= 180
    # Moments mapping: at least one of DBZH/VRADH should appear in any sweep.
    s0 = vol.get_sweep(0)
    assert s0.num_rays > 0
    assert s0.num_gates > 0
    assert "DBZH" in s0.moment_names() or "VRADH" in s0.moment_names()


def test_scan_nexrad_returns_metadata(nexrad_fixture):
    md = radish.scan_nexrad(nexrad_fixture)
    assert len(md.instrument_name) == 4
    assert md.num_sweeps >= 5


def test_moment_raw_codes_getters_are_additive_and_safe_here(nexrad_fixture):
    """`MomentData.raw_codes()`/`raw_codes_u16()`/`value_min`/
    `value_increment`/`n_levels`/`data_floor_code` (added for NEXRAD Level
    3's plan 0012, radish/src/model/moment.rs) are additive fields no
    other backend populates — NEXRAD Level 2 (this test) is one of the
    Python-reachable backends whose behaviour must be unaffected by that
    addition. There was previously NO Python exposure for either field at
    all (confirmed by grep before adding these getters), so this is the
    first real-data confirmation that the additive default is `None`, not
    a crash or an unexpected value, for a backend that never touches
    those fields.
    """
    vol = radish.read_nexrad(nexrad_fixture)
    s0 = vol.get_sweep(0)
    moment = s0.get_moment(s0.moment_names()[0])

    assert moment.raw_codes() is None
    assert moment.raw_codes_u16() is None
    assert moment.value_min is None
    assert moment.value_increment is None
    assert moment.n_levels is None
    assert moment.data_floor_code is None
    # Single-use like data(), but None (not an exception) on a repeat call
    # — see raw_codes()'s own doc comment for why that differs from
    # data()'s raise-on-reconsumption contract.
    assert moment.raw_codes() is None

    # The additive change must not disturb the primary data path.
    data = moment.data()
    assert data.ndim == 2
    assert data.shape[0] > 0 and data.shape[1] > 0


def test_scan_nexrad_exposes_per_sweep_attrs_and_time_ranges(nexrad_fixture):
    """`scan_nexrad` must surface the per-sweep MSG_5 cut attrs and the
    `(start, end)` time ranges so downstream bulk-ingest callers can
    classify SAILS / MRLE / MPDA slices and find sweep boundaries
    without paying for a full per-ray decode.
    """
    md = radish.scan_nexrad(nexrad_fixture)
    nx = md.nexrad_attrs

    assert len(nx.sweep_attrs) == md.num_sweeps
    assert len(nx.sweep_time_ranges) == md.num_sweeps

    # Per-sweep classifier flags must be reachable from each entry.
    # Spot-check one — the first sweep is always a real scan, never a
    # `default()` pad — and verify the field types come through PyO3
    # cleanly.
    first = nx.sweep_attrs[0]
    assert isinstance(first.sails_cut, bool)
    assert isinstance(first.mrle_cut, bool)
    assert isinstance(first.mpda_cut, bool)
    assert isinstance(first.base_tilt_cut, bool)
    assert isinstance(first.waveform_type, str)
    assert isinstance(first.channel_config, str)

    # `scan_nexrad` and `read_nexrad` must produce byte-identical
    # per-sweep attrs — both route through the same `volume_attrs`
    # builder. Pin it so a future refactor can't decouple them.
    vol = radish.read_nexrad(nexrad_fixture)
    for i in range(vol.num_sweeps):
        sweep = vol.get_sweep(i)
        scanned = nx.sweep_attrs[i]
        loaded = sweep.nexrad_attrs
        assert loaded is not None, f"sweep {i} missing nexrad_attrs after read"
        assert scanned.sails_cut == loaded.sails_cut
        assert scanned.mrle_cut == loaded.mrle_cut
        assert scanned.mpda_cut == loaded.mpda_cut
        assert scanned.waveform_type == loaded.waveform_type
        assert scanned.channel_config == loaded.channel_config

    # Time ranges are Unix seconds (float). Sweeps that decoded with
    # timestamped radials produce `(start, end)` with `start <= end`.
    seen_some = False
    for tr in nx.sweep_time_ranges:
        if tr is None:
            continue
        seen_some = True
        start, end = tr
        assert isinstance(start, float)
        assert isinstance(end, float)
        assert start <= end
    assert seen_some, "fixture should have at least one timestamped sweep"


def test_xarray_engine_radish_dispatches_to_nexrad(nexrad_fixture):
    xr = pytest.importorskip("xarray")
    dt = xr.open_datatree(nexrad_fixture, engine="radish")
    sweep_keys = [k for k in dt.children if k.startswith("sweep_")]
    assert len(sweep_keys) >= 5
    s0 = dt[sweep_keys[0]]
    assert "DBZH" in s0.data_vars or "VRADH" in s0.data_vars
    assert "azimuth" in s0.coords
    assert "elevation" in s0.coords
    assert "range" in s0.coords
    # Root identifies the radar (4-char ICAO) and the scan strategy.
    assert len(dt.attrs.get("instrument_name", "")) == 4
    assert dt.attrs.get("scan_name", "").startswith("VCP-")


def test_radish_matches_xradar_per_moment(nexrad_fixture):
    """Acceptance gate: numerically match xradar within tight tolerance."""
    xr = pytest.importorskip("xarray")
    xradar = pytest.importorskip("xradar")

    rd = xr.open_datatree(nexrad_fixture, engine="radish")
    xd = xradar.io.open_nexradlevel2_datatree(nexrad_fixture)

    rd_keys = sorted(k for k in rd.children if k.startswith("sweep_"))
    xd_keys = sorted(k for k in xd.children if k.startswith("sweep_"))
    common = [k for k in rd_keys if k in xd_keys]
    assert common, f"no common sweep groups: rd={rd_keys} xd={xd_keys}"

    # xradar represents below-threshold/no-data with negative sentinels (e.g.
    # DBZH=-33.0) while radish (via nexrad-decode) uses NaN. Both are valid
    # representations; we only verify that the *valid-gate* physical values
    # agree. Per-moment plausibility ranges below filter the sentinels.
    plausibility = {
        "DBZH": (-32.5, 95.5),
        "VRADH": (-100.0, 100.0),
        "WRADH": (0.0, 30.0),
        "ZDR": (-7.875, 7.9375),
        "PHIDP": (-180.0, 360.0),
        "RHOHV": (0.2, 1.05),
    }
    # Align by azimuth bin so ray-count mismatches (e.g. AVSET truncation or
    # one reader dropping a duplicate ray) become explicit and the comparison
    # still happens on the rays that exist in both. Tolerance is half the
    # super-res spacing (0.25°) so each radish ray maps to its nearest xradar
    # ray with no ambiguity at 0.5° spacing.
    AZ_TOL = 0.25
    compared = 0
    skipped_rays_total = 0
    for k in common:
        rd_ds = rd[k]
        xd_ds = xd[k]
        rd_az = np.asarray(rd_ds["azimuth"].values, dtype=np.float64)
        xd_az = np.asarray(xd_ds["azimuth"].values, dtype=np.float64)

        # Both should already be azimuth-sorted, but don't rely on it.
        rd_order = np.argsort(rd_az)
        xd_order = np.argsort(xd_az)
        rd_az_s = rd_az[rd_order]
        xd_az_s = xd_az[xd_order]

        # For each radish ray, find the closest xradar ray. Mark unmatched.
        idx = np.searchsorted(xd_az_s, rd_az_s)
        idx_lo = np.clip(idx - 1, 0, len(xd_az_s) - 1)
        idx_hi = np.clip(idx, 0, len(xd_az_s) - 1)
        d_lo = np.abs(xd_az_s[idx_lo] - rd_az_s)
        d_hi = np.abs(xd_az_s[idx_hi] - rd_az_s)
        nearest = np.where(d_lo <= d_hi, idx_lo, idx_hi)
        nearest_dist = np.minimum(d_lo, d_hi)
        rd_keep = nearest_dist <= AZ_TOL
        xd_keep_idx = nearest[rd_keep]
        skipped = int((~rd_keep).sum()) + max(0, len(xd_az_s) - len(xd_keep_idx))
        skipped_rays_total += skipped

        # If we couldn't match anything, that's a real failure for this sweep.
        assert rd_keep.any(), (
            f"sweep {k}: no rays matched within {AZ_TOL}° "
            f"(rd n={len(rd_az_s)}, xd n={len(xd_az_s)})"
        )
        # Allow up to 1% of rays to be unmatched (AVSET / dropped-duplicate edge
        # cases); flag larger discrepancies.
        max_unmatched = max(1, int(0.01 * max(len(rd_az_s), len(xd_az_s))))
        n_unmatched = int((~rd_keep).sum()) + (len(xd_az_s) - len(xd_keep_idx))
        assert n_unmatched <= max_unmatched, (
            f"sweep {k}: {n_unmatched} unmatched rays exceeds {max_unmatched} "
            f"(rd n={len(rd_az_s)}, xd n={len(xd_az_s)})"
        )

        # Sanity check: matched azimuths agree.
        np.testing.assert_allclose(
            rd_az_s[rd_keep],
            xd_az_s[xd_keep_idx],
            atol=AZ_TOL,
            err_msg=f"sweep {k}: matched azimuths disagree",
        )

        for m, (lo, hi) in plausibility.items():
            if m not in rd_ds.data_vars or m not in xd_ds.data_vars:
                continue
            rd_arr = np.asarray(rd_ds[m].values, dtype=np.float64)[rd_order][rd_keep]
            xd_arr = np.asarray(xd_ds[m].values, dtype=np.float64)[xd_order][xd_keep_idx]
            # Gate count may differ between readers (super-res REF in some
            # sweeps): compare on the overlap.
            if rd_arr.shape != xd_arr.shape:
                n_gates = min(rd_arr.shape[1], xd_arr.shape[1])
                rd_arr = rd_arr[:, :n_gates]
                xd_arr = xd_arr[:, :n_gates]
            # Mask anything either side considers "missing" (NaN OR outside the
            # plausibility window). Compare only on the intersection.
            valid = (
                np.isfinite(rd_arr)
                & np.isfinite(xd_arr)
                & (rd_arr >= lo)
                & (rd_arr <= hi)
                & (xd_arr >= lo)
                & (xd_arr <= hi)
            )
            if not valid.any():
                continue
            np.testing.assert_allclose(
                rd_arr[valid],
                xd_arr[valid],
                atol=1e-3,
                err_msg=(
                    f"sweep {k} moment {m}: valid gates disagree " f"({valid.sum()} compared)"
                ),
            )
            compared += 1
    assert compared > 0, "expected to compare at least one moment"
    print(
        f"\nparity: {compared} (sweep,moment) pairs verified, "
        f"{skipped_rays_total} unmatched rays skipped across {len(common)} sweeps"
    )


def test_open_datatree_bytes_auto(nexrad_fixture):
    """`radish.open_datatree(open(path, 'rb').read())` must produce the
    same DataTree as `radish.open_datatree(path)`. Pinning the contract for
    the common 'fetch from S3 / HTTP / fsspec then decode' workflow."""
    import xarray as xr  # noqa: F401  (used implicitly via DataTree)

    with open(nexrad_fixture, "rb") as f:
        data = f.read()
    dt_path = radish.open_datatree(nexrad_fixture)
    dt_bytes = radish.open_datatree(data)
    assert sorted(dt_path.children) == sorted(dt_bytes.children)
    assert sorted(dt_path.attrs.keys()) == sorted(dt_bytes.attrs.keys())


def test_open_datatree_bytes_matches_path(nexrad_fixture):
    """Same as the auto-detect bytes test but with explicit `backend=`."""
    with open(nexrad_fixture, "rb") as f:
        data = f.read()
    dt_path = radish.open_datatree(nexrad_fixture)
    dt_explicit = radish.open_datatree(data, backend="nexrad")
    assert sorted(dt_path.children) == sorted(dt_explicit.children)


def test_open_datatree_filelike_auto(nexrad_fixture):
    """`io.BytesIO` (and other `.read()`-having objects) must be sniffable
    and seek back so the actual decode sees the full buffer."""
    import io

    with open(nexrad_fixture, "rb") as f:
        data = f.read()
    buf = io.BytesIO(data)
    dt = radish.open_datatree(buf)
    n_sweeps = sum(1 for k in dt.children if k.startswith("sweep_"))
    assert n_sweeps > 0
    assert dt.attrs["instrument_name"] == "KLOT"


def test_read_nexrad_chunks_round_trips_full_file(nexrad_fixture):
    """Splitting the fixture into N byte chunks and feeding them to
    `read_nexrad_chunks` must reconstruct the same VolumeData as the
    path-based read. Covers the low-level building block that the unified
    `radish.open_datatree([...])` dispatches into."""
    with open(nexrad_fixture, "rb") as f:
        data = f.read()
    n = len(data)
    chunks = [data[: n // 4], data[n // 4 : n // 2], data[n // 2 : 3 * n // 4], data[3 * n // 4 :]]

    v_path = radish.read_nexrad(nexrad_fixture)
    v_chunks = radish.read_nexrad_chunks(chunks)

    assert v_path.metadata.instrument_name == v_chunks.metadata.instrument_name
    assert v_path.num_sweeps == v_chunks.num_sweeps
    a, b = v_path.metadata.nexrad_attrs, v_chunks.metadata.nexrad_attrs
    assert a.dynamic_scan_type == b.dynamic_scan_type
    assert a.rda_build_number == b.rda_build_number
    assert a.actual_elevation_cuts == b.actual_elevation_cuts
    assert a.avset_enabled == b.avset_enabled


def test_open_datatree_path_list(nexrad_fixture):
    """Single-path chunk list: the unified API must accept it and produce
    a DataTree equivalent to the regular path open."""
    import xarray as xr

    dt_chunks = radish.open_datatree([nexrad_fixture])
    dt_file = xr.open_datatree(nexrad_fixture, engine="radish")
    assert sorted(dt_chunks.children) == sorted(dt_file.children)
    assert sorted(dt_chunks.attrs.keys()) == sorted(dt_file.attrs.keys())
    for skey in dt_file.children:
        if not skey.startswith("sweep_"):
            continue
        assert dict(dt_chunks[skey].sizes) == dict(dt_file[skey].sizes)


def test_open_datatree_bytes_list(nexrad_fixture):
    """Two-bytes chunk list: split the fixture in half, decode via the
    unified API, confirm the concatenated buffer reconstructs the same
    DataTree as the regular engine."""
    import xarray as xr

    with open(nexrad_fixture, "rb") as f:
        data = f.read()
    n = len(data)
    dt_chunks = radish.open_datatree([data[: n // 2], data[n // 2 :]])
    dt_file = xr.open_datatree(nexrad_fixture, engine="radish")
    assert sorted(dt_chunks.children) == sorted(dt_file.children)
    assert dt_chunks.attrs.get("scan_name") == dt_file.attrs.get("scan_name")


def test_xarray_engine_accepts_backend_kwarg(nexrad_fixture):
    """`xr.open_datatree(path, engine="radish", backend="nexrad")` should
    pass `backend=` through to radish.open_datatree. Pins the engine-plugin
    contract so adding new backends remains useful through xarray, not
    only through `radish.open_datatree(...)` directly."""
    import xarray as xr

    # Auto-detect path: equivalent to engine="radish" only.
    dt_auto = xr.open_datatree(nexrad_fixture, engine="radish")
    # Explicit backend selection through the plugin.
    dt_explicit = xr.open_datatree(nexrad_fixture, engine="radish", backend="nexrad")

    assert sorted(dt_auto.children) == sorted(dt_explicit.children)
    assert sorted(dt_auto.attrs.keys()) == sorted(dt_explicit.attrs.keys())

    # Aliasing — `backend="nexrad_level2"` is the canonical name; `nexrad` is the alias.
    dt_canonical = xr.open_datatree(nexrad_fixture, engine="radish", backend="nexrad_level2")
    assert sorted(dt_canonical.children) == sorted(dt_auto.children)


def test_open_datatree_rejects_generator_chunks(nexrad_fixture):
    """Regression for the silent-first-chunk-drop bug: classification needs
    to peek at the chunk list's first element, and generators don't
    survive that peek (``next(iter(gen))`` advances the generator). The
    contract is "pass a list/tuple, not a generator" and the dispatcher
    raises a clear ``TypeError`` for anything else."""
    with open(nexrad_fixture, "rb") as f:
        data = f.read()
    n = len(data)

    # Generator yielding chunk bytes — the previous implementation would
    # have classified this as a chunk-list input and then iterated the
    # generator a second time, dropping the first chunk silently.
    def gen():
        yield data[: n // 2]
        yield data[n // 2 :]

    with pytest.raises(TypeError) as exc_info:
        radish.open_datatree(gen())
    msg = str(exc_info.value).lower()
    assert "generator" in msg or "list/tuple" in msg

    # The escape hatch — wrap the generator with `list(...)`.
    dt = radish.open_datatree(list(gen()))
    n_sweeps = sum(1 for k in dt.children if k.startswith("sweep_"))
    assert n_sweeps > 0


def test_open_datatree_explicit_backend_skips_sniff(tmp_path, nexrad_fixture):
    """`backend="nexrad"` must skip format sniffing — verified by feeding a
    file whose name and extension do NOT look like NEXRAD and confirming
    the decode succeeds anyway. (If sniffing ran, an extension-less,
    non-canonical filename would not produce a NEXRAD path on the
    auto-detect side because the file's first 4 bytes are still `AR2V`,
    so we go a step further: drop the magic too.)
    """
    # Read the fixture, prepend 32 bytes of garbage (so AR2V magic is
    # gone from the head), and assert that the auto-detect path raises
    # — but `backend="nexrad"` still tries the decode and then fails at
    # the parser stage rather than at the sniff stage.
    with open(nexrad_fixture, "rb") as f:
        data = f.read()
    garbage_then_data = b"X" * 32 + data

    # Auto-detect: no backend matches the buffer head.
    with pytest.raises((RuntimeError, ValueError)):
        radish.open_datatree(garbage_then_data)

    # Explicit backend: skips sniff, hits the parser, fails with a
    # decode-stage error (not the "no backend matched" sniff error).
    with pytest.raises(RuntimeError) as exc_info:
        radish.open_datatree(garbage_then_data, backend="nexrad")
    msg = str(exc_info.value)
    assert "No backend matched" not in msg


def test_open_datatree_cfradial1_bytes_raises(tmp_path):
    """CfRadial1 doesn't support in-memory bytes (libnetcdf needs a file).
    The unified API must surface a clear ValueError instead of a confusing
    parser error from deep in the stack."""
    head = b"\x89HDF\r\n\x1a\n" + b"\x00" * 64
    with pytest.raises((ValueError, RuntimeError)) as exc_info:
        radish.open_datatree(head, backend="cfradial1")
    msg = str(exc_info.value).lower()
    assert "cfradial1" in msg or "in-memory" in msg or "not supported" in msg


def test_detect_backend_introspection(nexrad_fixture, tmp_path):
    """`radish.detect_backend(input)` covers all input shapes."""
    with open(nexrad_fixture, "rb") as f:
        data = f.read()

    assert radish.detect_backend(nexrad_fixture) == "nexrad_level2"
    assert radish.detect_backend(data) == "nexrad_level2"
    assert radish.detect_backend([data]) == "nexrad_level2"

    import io

    assert radish.detect_backend(io.BytesIO(data)) == "nexrad_level2"

    # Path-by-extension routing: the file doesn't need to exist for the
    # extension-based check to succeed (auto_backend(path) only inspects
    # the path string for path-like backends like CfRadial1).
    assert radish.detect_backend("foo.nc") == "cfradial1"
    assert radish.detect_backend("foo.nc4") == "cfradial1"
    assert radish.detect_backend("foo.ar2v") == "nexrad_level2"
    assert radish.detect_backend("KLOT20260310_231412_V06") == "nexrad_level2"

    # Unknown filename / unrecognised buffer
    assert radish.detect_backend("garbage_filename_with_no_extension") is None
    assert radish.detect_backend(b"GARBAGE!" * 8) is None


def test_sweep_emits_fm301_scalar_variables(nexrad_fixture):
    """CfRadial2 / WMO FM301 spec: each sweep group must carry sweep_mode,
    sweep_number, sweep_fixed_angle, prt_mode, and follow_mode as 0-d
    variables (NOT attributes). Pins the structural contract so downstream
    CfRadial2 validators don't reject the output."""
    xr = pytest.importorskip("xarray")
    dt = xr.open_datatree(nexrad_fixture, engine="radish")
    for sweep_key in (k for k in dt.children if k.startswith("sweep_")):
        s = dt[sweep_key]
        for var in ("sweep_mode", "sweep_number", "sweep_fixed_angle", "prt_mode", "follow_mode"):
            assert var in s.data_vars, f"{sweep_key}: missing FM301 var {var!r}"
            assert s[var].ndim == 0, f"{sweep_key}: {var!r} must be 0-d"
            if var == "sweep_mode":
                assert str(s[var].values) == "azimuth_surveillance"
            elif var == "prt_mode":
                assert str(s[var].values) in {"fixed", "staggered", "dual", "not_set"}
            elif var == "follow_mode":
                assert str(s[var].values) in {
                    "none",
                    "sun",
                    "vehicle",
                    "aircraft",
                    "target",
                    "manual",
                    "not_set",
                }
        # Old attribute-based form must NOT come back.
        assert "fixed_angle" not in s.attrs, f"{sweep_key}: fixed_angle attr leaked"
        assert "sweep_number" not in s.attrs, f"{sweep_key}: sweep_number attr leaked"


def test_root_emits_msg2_msg5_attrs(nexrad_fixture):
    """Phase B: every MSG_2 / MSG_5 root attr xradar emits is on radish's root,
    with the same Python type (str / int / float / bool — never numpy scalars)."""
    xr = pytest.importorskip("xarray")
    dt = xr.open_datatree(nexrad_fixture, engine="radish")

    expected_types = {
        "dynamic_scan_type": str,
        "mpda_vcp": bool,
        "base_tilt_vcp": bool,
        "num_base_tilts": int,
        "vcp_truncated": bool,
        "vcp_sequence_active": bool,
        "number_elevation_cuts": int,
        "doppler_velocity_resolution": float,
        "vcp_pulse_width": str,
        "avset_enabled": bool,
        "ebc_enabled": bool,
        "super_res_status": int,
        "rda_build_number": int,
        "operational_mode": int,
        "actual_elevation_cuts": int,
    }
    for key, ty in expected_types.items():
        assert key in dt.attrs, f"missing root attr {key!r}"
        # `bool` must be checked first because `isinstance(True, int)` is True.
        actual = dt.attrs[key]
        if ty is bool:
            assert isinstance(actual, bool), f"{key!r}: expected bool, got {type(actual)}"
        elif ty is int:
            assert isinstance(actual, int) and not isinstance(
                actual, bool
            ), f"{key!r}: expected int, got {type(actual)}"
        elif ty is float:
            assert isinstance(actual, float), f"{key!r}: expected float, got {type(actual)}"
        else:
            assert isinstance(actual, ty), f"{key!r}: expected {ty}, got {type(actual)}"

    # `actual_elevation_cuts` must equal the number of decoded sweeps; xradar
    # uses the same definition.
    n_sweeps = sum(1 for k in dt.children if k.startswith("sweep_"))
    assert dt.attrs["actual_elevation_cuts"] == n_sweeps


def test_sweep_emits_msg5_per_cut_attrs(nexrad_fixture):
    """Phase B: every MSG_5 elevation-cut attr xradar emits per sweep is on
    every radish sweep group with the right Python type."""
    xr = pytest.importorskip("xarray")
    dt = xr.open_datatree(nexrad_fixture, engine="radish")

    expected_types = {
        "waveform_type": str,
        "channel_config": str,
        "super_resolution": int,
        "sails_cut": bool,
        "sails_sequence_number": int,
        "mrle_cut": bool,
        "mrle_sequence_number": int,
        "mpda_cut": bool,
        "base_tilt_cut": bool,
    }
    valid_waveforms = {
        "contiguous_surveillance",
        "contiguous_doppler",
        "batch",
        "staggered_pulse_pair",
        "not_applicable",
    }
    valid_channels = {"constant_phase", "random_phase", "sz2_phase_coding"}

    for sweep_key in (k for k in dt.children if k.startswith("sweep_")):
        s = dt[sweep_key]
        for key, ty in expected_types.items():
            assert key in s.attrs, f"{sweep_key}: missing attr {key!r}"
            actual = s.attrs[key]
            if ty is bool:
                assert isinstance(
                    actual, bool
                ), f"{sweep_key}.{key!r}: expected bool, got {type(actual)}"
            elif ty is int:
                assert isinstance(actual, int) and not isinstance(
                    actual, bool
                ), f"{sweep_key}.{key!r}: expected int, got {type(actual)}"
            else:
                assert isinstance(
                    actual, ty
                ), f"{sweep_key}.{key!r}: expected {ty}, got {type(actual)}"
        assert s.attrs["waveform_type"] in valid_waveforms
        assert s.attrs["channel_config"] in valid_channels


def test_root_attrs_match_xradar(nexrad_fixture):
    """Phase B acceptance gate: the MSG_2 / MSG_5 root attrs we emit equal
    xradar's values verbatim."""
    pytest.importorskip("xarray")
    xradar = pytest.importorskip("xradar")
    import xarray as xr  # noqa: E402

    rd = xr.open_datatree(nexrad_fixture, engine="radish")
    xd = xradar.io.open_nexradlevel2_datatree(nexrad_fixture)

    keys = (
        "dynamic_scan_type",
        "mpda_vcp",
        "base_tilt_vcp",
        "num_base_tilts",
        "vcp_truncated",
        "vcp_sequence_active",
        "number_elevation_cuts",
        "doppler_velocity_resolution",
        "vcp_pulse_width",
        "avset_enabled",
        "ebc_enabled",
        "super_res_status",
        "rda_build_number",
        "operational_mode",
        "actual_elevation_cuts",
    )
    for k in keys:
        if k not in xd.attrs:
            # xradar may omit a key when the source field is zero — skip.
            continue
        rd_v, xd_v = rd.attrs[k], xd.attrs[k]
        assert rd_v == xd_v, f"root attr {k!r}: radish={rd_v!r} xradar={xd_v!r}"


def test_sweep_attrs_match_xradar(nexrad_fixture):
    """Phase B acceptance gate: per-sweep MSG_5 attrs match xradar's values
    on the sweeps both readers produce."""
    pytest.importorskip("xarray")
    xradar = pytest.importorskip("xradar")
    import xarray as xr  # noqa: E402

    rd = xr.open_datatree(nexrad_fixture, engine="radish")
    xd = xradar.io.open_nexradlevel2_datatree(nexrad_fixture)

    rd_keys = sorted(k for k in rd.children if k.startswith("sweep_"))
    xd_keys = sorted(k for k in xd.children if k.startswith("sweep_"))
    common = [k for k in rd_keys if k in xd_keys]
    assert common, "no sweep groups in common"

    keys = (
        "waveform_type",
        "channel_config",
        "super_resolution",
        "sails_cut",
        "sails_sequence_number",
        "mrle_cut",
        "mrle_sequence_number",
        "mpda_cut",
        "base_tilt_cut",
    )
    for sweep_key in common:
        rs, xs = rd[sweep_key], xd[sweep_key]
        for k in keys:
            if k not in xs.attrs:
                continue
            rd_v, xd_v = rs.attrs[k], xs.attrs[k]
            assert rd_v == xd_v, f"{sweep_key}.{k!r}: radish={rd_v!r} xradar={xd_v!r}"


def test_radish_matches_xradar_structure(nexrad_fixture):
    """Pin the structural parity with `xradar.io.open_nexradlevel2_datatree`:
    same dim names, same coord set + dtypes, same per-DataArray CF attrs
    (units / standard_name / long_name), same root data_vars, same
    variable set per sweep. Numeric values may differ within tolerance
    (different missing-data sentinels, etc.) — that's covered by the
    parity test above. Sweep-level attrs (waveform_type, etc.) and the
    15 MSG_2/MSG_5 root attrs are out of scope here; they need the
    lower-level `nexrad-decode` API and land in a follow-up commit.
    """
    xr = pytest.importorskip("xarray")
    xradar = pytest.importorskip("xradar")

    rd = xr.open_datatree(nexrad_fixture, engine="radish")
    xd = xradar.io.open_nexradlevel2_datatree(nexrad_fixture)

    # Root data_vars set + dtypes match.
    assert set(rd.data_vars) == set(xd.data_vars)
    for v in rd.data_vars:
        assert (
            rd[v].dtype == xd[v].dtype
        ), f"root[{v}] dtype mismatch: {rd[v].dtype} vs {xd[v].dtype}"

    rd_keys = sorted(k for k in rd.children if k.startswith("sweep_"))
    xd_keys = sorted(k for k in xd.children if k.startswith("sweep_"))
    assert rd_keys == xd_keys, "sweep group keys differ"

    for k in rd_keys:
        rs, xs = rd[k], xd[k]
        # Dim names match exactly. Per-sweep dim *lengths* may differ by a
        # few rays on AVSET-truncated sweeps (xradar drops to expected cut
        # count; radish keeps everything decoded). Allow that.
        assert set(rs.sizes) == set(xs.sizes), f"{k}: dim names differ"
        for d in rs.sizes:
            rd_n, xd_n = rs.sizes[d], xs.sizes[d]
            assert abs(rd_n - xd_n) <= max(
                3, int(0.01 * max(rd_n, xd_n))
            ), f"{k}.{d}: lengths differ too much: rd={rd_n} xd={xd_n}"
        # Coord set + dim layout + dtype.
        assert set(rs.coords) == set(xs.coords), f"{k}: coords differ"
        for c in rs.coords:
            assert rs[c].dims == xs[c].dims, f"{k}.{c}: coord dims differ"
            assert rs[c].dtype == xs[c].dtype, f"{k}.{c}: coord dtype differ"
        # Variable set + dtype + dims for every moment / scalar var.
        assert set(rs.data_vars) == set(xs.data_vars), f"{k}: data_vars differ"
        for v in rs.data_vars:
            assert rs[v].dims == xs[v].dims, f"{k}.{v}: dims differ"
            # Per-DataArray CF metadata: units / standard_name / long_name.
            for key in ("units", "standard_name", "long_name"):
                if key in xs[v].attrs:
                    assert (
                        rs[v].attrs.get(key) == xs[v].attrs[key]
                    ), f"{k}.{v}.attrs[{key!r}]: {rs[v].attrs.get(key)!r} != {xs[v].attrs[key]!r}"


def test_scan_path_bytes_filelike_chunks_all_yield_same_metadata(nexrad_fixture):
    """Phase 4 of plan 0005: `radish.scan` accepts the same 4 input shapes
    as `radish.open_datatree`, all returning identical `VolumeMetadata`.

    This is the core acceptance check for the bytes/file-like/chunks input
    path that closes the asymmetry between `radish.scan_nexrad(path)` and
    `radish.open_datatree(filename_or_obj)`. Pinning strict equality on
    `attributes` and `nexrad_attrs` (not just instrument_name +
    sweep_fixed_angles) per the downstream issue's verification plan.
    """
    md_path = radish.scan(nexrad_fixture)

    with open(nexrad_fixture, "rb") as f:
        blob = f.read()
    md_bytes = radish.scan(blob)

    with open(nexrad_fixture, "rb") as f:
        md_file = radish.scan(f)

    # Three-way byte split → chunk-list input.
    n = len(blob)
    chunks = [blob[: n // 3], blob[n // 3 : 2 * n // 3], blob[2 * n // 3 :]]
    md_chunks = radish.scan(chunks)

    # Load-bearing scalar fields.
    for md in (md_bytes, md_file, md_chunks):
        assert md.instrument_name == md_path.instrument_name
        assert md.sweep_fixed_angles == md_path.sweep_fixed_angles

    # Strict equality from the downstream issue's verification plan —
    # full attrs dict + per-sweep MSG_5 list bit-identical (the fields
    # raw2zarr#244's chain inspects on every file).
    assert md_path.attributes == md_bytes.attributes
    assert md_path.attributes == md_file.attributes
    assert md_path.attributes == md_chunks.attributes

    assert md_path.nexrad_attrs == md_bytes.nexrad_attrs
    assert md_path.nexrad_attrs == md_file.nexrad_attrs
    assert md_path.nexrad_attrs == md_chunks.nexrad_attrs

    assert md_path.nexrad_attrs.sweep_attrs == md_bytes.nexrad_attrs.sweep_attrs


def test_scan_rejects_unsupported_input_types():
    """`radish.scan` rejects shapes the dispatcher doesn't recognise —
    same contract as `open_datatree`. Generators and arbitrary objects
    must raise a clear error rather than silently degrading."""
    with pytest.raises(TypeError, match="Unsupported input type"):
        radish.scan(12345)  # int — not path-like, not bytes-like
    with pytest.raises(TypeError, match="Unsupported input type"):
        radish.scan(iter([b"chunk1", b"chunk2"]))  # generator


def test_scan_explicit_backend_override(nexrad_fixture):
    """`radish.scan(obj, backend="nexrad")` skips the auto-detect and
    routes directly. Both the canonical name and the alias must work."""
    md_canonical = radish.scan(nexrad_fixture, backend="nexrad_level2")
    md_alias = radish.scan(nexrad_fixture, backend="nexrad")
    md_auto = radish.scan(nexrad_fixture)
    assert md_canonical.instrument_name == md_alias.instrument_name == md_auto.instrument_name


# ---------------------------------------------------------------------------
# Plan 0009: real-time chunk streams (S/I/E objects), incomplete-sweep
# detection, and the incomplete_sweep="drop"|"pad"|"keep" policy.
# Gated on RADISH_NEXRAD_CHUNKS_DIR via the `nexrad_chunks_dir` fixture.
# ---------------------------------------------------------------------------


@pytest.fixture
def klot_chunks(nexrad_chunks_dir):
    """The 55 KLOT chunk objects as bytes, in scan order."""
    paths = sorted(p for p in nexrad_chunks_dir.iterdir() if p.is_file())
    return [p.read_bytes() for p in paths]


class TestChunkStreamFullVolume:
    def test_full_chunk_list_decodes_all_sweeps_without_warning(self, klot_chunks):
        import warnings as _warnings

        with _warnings.catch_warnings(record=True) as w:
            _warnings.simplefilter("always")
            dt = radish.open_datatree(klot_chunks)
        assert len(dt.children) == 12
        assert not [x for x in w if "incomplete" in str(x.message)]

    def test_full_chunk_list_matches_concatenated_blob(self, klot_chunks):
        v_chunks = radish.read_nexrad_chunks(klot_chunks)
        v_blob = radish._radish.read_nexrad_bytes(b"".join(klot_chunks))
        assert v_chunks.num_sweeps == v_blob.num_sweeps
        assert v_chunks.incomplete_sweeps == []
        a = v_chunks.get_sweep(0).get_moment("DBZH").data()
        b = v_blob.get_sweep(0).get_moment("DBZH").data()
        np.testing.assert_array_equal(a, b)


class TestIncompleteSweepPolicy:
    def test_default_drop_warns_and_keeps_original_names(self, klot_chunks):
        with pytest.warns(UserWarning, match=r"Dropped 1 incomplete sweep\(s\): \[1\]"):
            dt = radish.open_datatree(klot_chunks[:11])
        assert sorted(dt.children) == ["sweep_0"]

    def test_pad_reindexes_to_full_uniform_grid(self, klot_chunks):
        dt = radish.open_datatree(klot_chunks[:11], incomplete_sweep="pad")
        s1 = dt["sweep_1"].ds
        assert s1.sizes["azimuth"] == 720
        np.testing.assert_allclose(np.diff(s1.azimuth.values), 0.5)
        observed_rows = int(np.isfinite(s1.DBZH.values).any(axis=1).sum())
        assert 0 < observed_rows < 720
        # Complete sweep untouched by pad.
        assert dt["sweep_0"].ds.sizes["azimuth"] == 720

    def test_keep_passes_partial_sweep_through(self, klot_chunks):
        dt = radish.open_datatree(klot_chunks[:11], incomplete_sweep="keep")
        assert dt["sweep_1"].ds.sizes["azimuth"] < 720

    def test_incomplete_sweeps_property_on_low_level_read(self, klot_chunks):
        v = radish.read_nexrad_chunks(klot_chunks[:11])
        assert v.incomplete_sweeps == [1]
        assert v.metadata.incomplete_sweeps == [1]
        assert v.get_sweep(0).is_complete
        assert not v.get_sweep(1).is_complete

    def test_scan_chunks_reports_incomplete_indices(self, klot_chunks):
        md = radish.scan_nexrad_chunks(klot_chunks[:11])
        assert md.incomplete_sweeps == [1]

    def test_headerless_i_only_stream_flags_leading_sweep(self, klot_chunks):
        v = radish.read_nexrad_chunks(klot_chunks[1:6])
        assert v.incomplete_sweeps == [0]

    def test_invalid_policy_value_raises(self, klot_chunks):
        with pytest.raises(ValueError, match="keep.*drop.*pad|expected"):
            radish.read_nexrad_chunks(klot_chunks[:11], incomplete_sweep="bogus")

    def test_policy_rejected_for_non_nexrad_backend(self, klot_chunks):
        # bytes shape so the (backend, shape) dispatch row exists — the
        # policy check must fire before any decode is attempted.
        with pytest.raises(ValueError, match="only supported by the NEXRAD backend"):
            radish.open_datatree(klot_chunks[0], backend="sigmet", incomplete_sweep="pad")

    def test_open_dataset_group_uses_original_indices(self, klot_chunks):
        ds = radish.open_dataset(klot_chunks[:11], group="sweep_0")
        assert ds.sizes["azimuth"] == 720
        with pytest.raises(ValueError, match="not present"):
            radish.open_dataset(klot_chunks[:11], group="sweep_1")


class TestChunkListValidation:
    def test_out_of_order_volume_header_raises(self, klot_chunks):
        with pytest.raises(ValueError, match="chunk 1"):
            radish.read_nexrad_chunks([klot_chunks[1], klot_chunks[0]])

    def test_empty_chunk_list_raises(self):
        with pytest.raises(ValueError, match="empty chunk list"):
            radish.read_nexrad_chunks([])

    def test_single_i_chunk_bytes_auto_detect(self, klot_chunks):
        assert radish.detect_backend(klot_chunks[5]) == "nexrad_level2"


class TestXradarChunkParity:
    """Compare drop/pad sweep sets against xradar when the installed
    xradar carries openradar/xradar#332 (incomplete_sweep support)."""

    @pytest.fixture
    def xd_open(self):
        xd = pytest.importorskip("xradar")
        import inspect

        sig = inspect.signature(xd.io.open_nexradlevel2_datatree)
        if "incomplete_sweep" not in sig.parameters:
            pytest.skip("installed xradar predates openradar/xradar#332")
        return xd.io.open_nexradlevel2_datatree

    def test_drop_mode_sweep_sets_match(self, klot_chunks, xd_open):
        import warnings as _warnings

        with _warnings.catch_warnings():
            _warnings.simplefilter("ignore")
            dt_x = xd_open(klot_chunks[:11], incomplete_sweep="drop")
            dt_r = radish.open_datatree(klot_chunks[:11], incomplete_sweep="drop")
        x_sweeps = sorted(c for c in dt_x.children if c.startswith("sweep_"))
        r_sweeps = sorted(c for c in dt_r.children if c.startswith("sweep_"))
        assert r_sweeps == x_sweeps

    def test_pad_mode_azimuth_grids_match(self, klot_chunks, xd_open):
        """Grid parity holds on this pinned fixture because xradar's
        median-diff inference lands on the nominal 0.5 deg here. On live
        partial sweeps with azimuth jitter, xradar can infer an
        off-nominal resolution (observed: 0.49 deg -> a 735-ray grid on a
        KLOT super-res cut, which no WSR-88D produces); radish always
        uses MSG31 azimuth_resolution_spacing, so its grid stays the
        ICD-nominal 720/360. That divergence is intentional."""
        import warnings as _warnings

        with _warnings.catch_warnings():
            _warnings.simplefilter("ignore")
            dt_x = xd_open(klot_chunks[:11], incomplete_sweep="pad")
            dt_r = radish.open_datatree(klot_chunks[:11], incomplete_sweep="pad")
        sx = dt_x["sweep_1"].ds
        sr = dt_r["sweep_1"].ds
        assert sr.sizes["azimuth"] == sx.sizes["azimuth"]
        np.testing.assert_allclose(sr.azimuth.values, sx.azimuth.values)
