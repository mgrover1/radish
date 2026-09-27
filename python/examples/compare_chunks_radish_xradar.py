#!/usr/bin/env python
"""Compare radish vs xradar on NEXRAD real-time chunk streams.

Exercises the plan-0009 surface end to end — full volumes, truncated
volumes under ``incomplete_sweep="drop" | "pad" | "keep"``, completeness
flags, and an early-stream timeline that shows the cost of re-decoding
the whole prefix on every poll (the motivation for plan 0010's
incremental assembler).

Sources (pick one):

    # Offline — the KLOT fixture from open-radar-data (default when set)
    export RADISH_NEXRAD_CHUNKS_DIR=~/.cache/radish/fixtures/nexrad_chunks_KLOT
    python compare_chunks_radish_xradar.py

    # Live — latest complete volume from the real-time bucket
    python compare_chunks_radish_xradar.py --live KLOT

xradar comparisons run when xradar is importable; the drop/pad
comparisons additionally need an xradar with openradar/xradar#332
(the ``incomplete_sweep`` parameter). Each section degrades gracefully.
"""

import argparse
import inspect
import os
import sys
import time
import warnings
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np

import radish

# --------------------------------------------------------------------------
# Chunk acquisition
# --------------------------------------------------------------------------


def load_local_chunks(chunks_dir: str) -> list[bytes]:
    paths = sorted(p for p in Path(chunks_dir).expanduser().iterdir() if p.is_file())
    if not paths:
        sys.exit(f"no chunk files in {chunks_dir}")
    print(f"source : {chunks_dir} ({len(paths)} chunk objects)")
    return [p.read_bytes() for p in paths]


def fetch_live_chunks(site: str) -> list[bytes]:
    import fsspec

    fs = fsspec.filesystem("s3", anon=True)
    volumes = fs.ls(f"unidata-nexrad-level2-chunks/{site}/")
    # Second-most-recent volume: the most recent may still be filling.
    vol_dir = sorted(volumes)[-2]
    paths = sorted(fs.ls(vol_dir))
    print(f"source : s3://{vol_dir} ({len(paths)} chunk objects)")
    with ThreadPoolExecutor(max_workers=16) as ex:
        return list(ex.map(lambda p: fs.open(p, "rb").read(), paths))


# --------------------------------------------------------------------------
# Helpers
# --------------------------------------------------------------------------


def sweep_children(dt) -> list[str]:
    return sorted(k for k in dt.children if k.startswith("sweep_"))


def nan_parity(a: np.ndarray, b: np.ndarray) -> str:
    """Max |diff| over cells where both are finite + NaN-mask agreement.

    Expect ``max|diff|=0`` with masks that *differ*: radish emits NaN for
    below-threshold gates where xradar emits the sentinel value (e.g.
    -33 dBZ for DBZH) — physically equivalent, documented since the
    original decoder parity work. Identical masks would only appear if
    xradar were also NaN-masking sentinels.
    """
    if a.shape != b.shape:
        return f"SHAPE MISMATCH {a.shape} vs {b.shape}"
    both = np.isfinite(a) & np.isfinite(b)
    mask_agree = np.array_equal(np.isnan(a), np.isnan(b))
    max_diff = float(np.abs(a[both] - b[both]).max()) if both.any() else 0.0
    return f"max|diff|={max_diff:.6g}, NaN masks {'agree' if mask_agree else 'DIFFER'}"


def xradar_open_or_none():
    try:
        import xradar as xd
    except ImportError:
        print("xradar not installed — skipping xradar comparisons")
        return None, False
    opener = xd.io.open_nexradlevel2_datatree
    has_332 = "incomplete_sweep" in inspect.signature(opener).parameters
    if not has_332:
        print("installed xradar predates #332 — full-volume comparison only")
    return opener, has_332


def quiet(fn, *args, **kwargs):
    with warnings.catch_warnings():
        warnings.simplefilter("ignore")
        return fn(*args, **kwargs)


# --------------------------------------------------------------------------
# Sections
# --------------------------------------------------------------------------


def section_full_volume(chunks, xd_open):
    print("\n== 1. Full volume ==")
    t0 = time.perf_counter()
    dt_r = quiet(radish.open_datatree, chunks)
    t_r = time.perf_counter() - t0
    rays = {k: dt_r[k].ds.sizes["azimuth"] for k in sweep_children(dt_r)}
    print(f"radish : {len(rays)} sweeps in {t_r * 1e3:.0f} ms, rays: {sorted(set(rays.values()))}")

    if xd_open is None:
        return dt_r
    t0 = time.perf_counter()
    dt_x = quiet(xd_open, chunks)
    t_x = time.perf_counter() - t0
    print(
        f"xradar : {len(sweep_children(dt_x))} sweeps in {t_x * 1e3:.0f} ms "
        f"(radish {t_x / t_r:.1f}x faster)"
    )
    assert sweep_children(dt_r) == sweep_children(dt_x), "sweep sets differ!"
    for name in sweep_children(dt_r)[:3]:
        a, b = dt_r[name].ds, dt_x[name].ds
        moment = "DBZH" if "DBZH" in a.data_vars else next(iter(a.data_vars))
        # Align on azimuth ordering before comparing values.
        b = b.sortby("azimuth") if not np.allclose(a.azimuth, b.azimuth) else b
        print(f"  {name} {moment}: {nan_parity(a[moment].values, b[moment].values)}")
    return dt_r


def section_partial_policies(chunks, xd_open, has_332, n_partial):
    print(f"\n== 2. Partial volume (first {n_partial} of {len(chunks)} chunks) ==")
    partial = chunks[:n_partial]

    v = radish.read_nexrad_chunks(partial)  # low-level default: keep
    flags = [v.get_sweep(i).is_complete for i in range(v.num_sweeps)]
    print(
        f"radish keep : {v.num_sweeps} sweeps, is_complete={flags}, "
        f"incomplete_sweeps={v.incomplete_sweeps}"
    )

    with warnings.catch_warnings(record=True) as w:
        warnings.simplefilter("always")
        dt_drop = radish.open_datatree(partial)  # default: drop
    print(f"radish drop : children={sweep_children(dt_drop)}")
    for x in w:
        print(f"  warning: {x.message}")

    dt_pad = quiet(radish.open_datatree, partial, incomplete_sweep="pad")
    last = sweep_children(dt_pad)[-1]
    s = dt_pad[last].ds
    moment = "DBZH" if "DBZH" in s.data_vars else next(iter(s.data_vars))
    observed = int(np.isfinite(s[moment].values).any(axis=1).sum())
    print(
        f"radish pad  : {last} on {s.sizes['azimuth']}-ray grid, "
        f"{observed} observed / {s.sizes['azimuth'] - observed} NaN rays"
    )

    if xd_open is None or not has_332:
        return
    dt_xd = quiet(xd_open, partial, incomplete_sweep="drop")
    dt_xp = quiet(xd_open, partial, incomplete_sweep="pad")
    print(
        f"xradar drop : children={sweep_children(dt_xd)} "
        f"({'MATCH' if sweep_children(dt_xd) == sweep_children(dt_drop) else 'MISMATCH'})"
    )
    sx = dt_xp[last].ds
    grid_match = s.sizes["azimuth"] == sx.sizes["azimuth"] and np.allclose(
        s.azimuth.values, sx.azimuth.values
    )
    print(
        f"xradar pad  : {last} grid {'MATCH' if grid_match else 'MISMATCH'}; "
        f"{moment} on observed rays: {nan_parity(s[moment].values, sx[moment].values)}"
    )


def section_early_stream_timeline(chunks):
    print("\n== 3. Early-stream timeline (re-decode per poll — plan 0010 motivation) ==")
    print(f"{'chunks':>7} {'sweeps kept':>11} {'trailing rays':>13} {'decode ms':>10}")
    total = 0.0
    for n in [2, 5, 10, 20, len(chunks) // 2, len(chunks)]:
        n = min(n, len(chunks))
        t0 = time.perf_counter()
        v = radish.read_nexrad_chunks(chunks[:n])  # full prefix re-decode
        dt = (time.perf_counter() - t0) * 1e3
        total += dt
        last = v.get_sweep(v.num_sweeps - 1)
        print(f"{n:>7} {v.num_sweeps:>11} {last.num_rays:>13} {dt:>10.0f}")
    print(f"cumulative re-decode cost for this polling schedule: {total:.0f} ms")
    print("(an incremental assembler pays each chunk's decode exactly once)")


# --------------------------------------------------------------------------


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--live", metavar="SITE", help="fetch from the live bucket (e.g. KLOT)")
    ap.add_argument(
        "--chunks-dir",
        default=os.environ.get("RADISH_NEXRAD_CHUNKS_DIR"),
        help="local chunk directory (default: $RADISH_NEXRAD_CHUNKS_DIR)",
    )
    ap.add_argument(
        "--partial",
        type=int,
        default=None,
        help="chunk count for the truncated-volume section (default: ~20%%)",
    )
    args = ap.parse_args()

    if args.live:
        chunks = fetch_live_chunks(args.live)
    elif args.chunks_dir:
        chunks = load_local_chunks(args.chunks_dir)
    else:
        sys.exit("no source: pass --live SITE or set RADISH_NEXRAD_CHUNKS_DIR")

    xd_open, has_332 = xradar_open_or_none()
    section_full_volume(chunks, xd_open)
    section_partial_policies(chunks, xd_open, has_332, args.partial or max(2, len(chunks) // 5))
    section_early_stream_timeline(chunks)
    print("\nall sections completed")


if __name__ == "__main__":
    main()
