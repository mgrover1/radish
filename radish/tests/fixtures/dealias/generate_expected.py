#!/usr/bin/env python
"""Generate the golden-corpus sidecar for the dealiasing parity gate
(`radish/tests/test_dealias_parity.rs`).

Uses a REAL, already-decoded NEXRAD Level 2 velocity sweep — decoded by
`radish` ITSELF (not Py-ART's own NEXRAD reader, which pads every sweep
in a volume to the max gate count across the whole file and would need a
separate, error-prone gate-count-alignment step; using radish's own
per-sweep-native-shape array as the single shared input to BOTH sides
means this gate isolates dealiasing-only parity, not decode parity, which
is already covered by `radish/tests/test_nexrad_internal_parity.rs`) —
then runs Py-ART's ACTUAL internal dealiasing functions
(`_find_regions`/`_edge_sum_and_count`/`_RegionTracker`/`_EdgeTracker`/
`_combine_regions`, called the same way `dealias_region_based`'s own
sweep loop calls them) on that exact array to produce the expected fold
array.

Not part of `cargo test` or CI — a one-time generation step. The
committed OUTPUT (`expected/<fixture>_sweep<N>.json`) is what
`test_dealias_parity.rs` reads; it needs neither Python nor Py-ART at
test time.

Usage (from the radish repo root, with both `radish` (editable install,
`python/.venv`) and `pyart` importable):

    python3 radish/tests/fixtures/dealias/generate_expected.py
"""

from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

FIXTURE_DIR = Path.home() / ".cache" / "radish" / "fixtures" / "nexrad"

# (fixture filename, sweep index, moment, nyquist m/s, rays_wrap_around).
# Nyquist is read once from a real Py-ART `Radar.instrument_parameters`
# for the SAME sweep (confirmed aligned by comparing `fixed_angle`
# sequences between radish's and Py-ART's readers on this file — both
# preserve wire order identically) and hardcoded here since it's a single
# scalar, not the array under test.
CASES = [
    ("KLOT20251210_102338_V06", 1, "VRADH", 11.55, True),
    ("KLOT20251210_102338_V06", 9, "VRADH", 11.55, True),
]


def main() -> int:
    sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "python"))
    import radish

    out_dir = Path(__file__).parent / "expected"
    out_dir.mkdir(exist_ok=True)

    for fixture, sweep_idx, moment, nyquist, wrap in CASES:
        path = FIXTURE_DIR / fixture
        if not path.is_file():
            print(f"skipping {fixture}: not found at {path}", file=sys.stderr)
            continue

        volume = radish.read_nexrad(str(path))
        sweep = volume.get_sweep(sweep_idx)
        velocity = sweep.get_moment(moment).data()  # float32, NaN = missing

        import numpy as np

        valid = np.isfinite(velocity)
        folds = _pyart_dealias_folds(velocity, valid, nyquist, wrap)

        name = f"{fixture}_sweep{sweep_idx}"
        expected = {
            "fixture": fixture,
            "sweep_index": sweep_idx,
            "moment": moment,
            "nyquist": nyquist,
            "rays_wrap_around": wrap,
            "shape": list(velocity.shape),
            "n_valid_gates": int(valid.sum()),
            "folds_sha256": hashlib.sha256(folds.astype("int32").tobytes()).hexdigest(),
            "folds_sum": int(folds.astype("int64").sum()),
            "folds_nonzero_count": int((folds != 0).sum()),
            "folds_row0_sample": [int(x) for x in folds[0, :20]],
        }
        out_path = out_dir / f"{name}.json"
        out_path.write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n")
        print(f"wrote {out_path} (folds_sha256={expected['folds_sha256'][:12]}...)")

    return 0


def _pyart_dealias_folds(velocity, valid, nyquist, rays_wrap_around):
    """Run Py-ART's actual internal per-sweep dealiasing pipeline —
    the SAME sequence `pyart.correct.dealias_region_based`'s sweep loop
    runs — directly on `velocity`/`valid`, bypassing the Radar-object /
    GateFilter machinery entirely (this file already has the mask it
    needs). Mirrors `radish/src/transforms/dealias/sweep.rs`'s
    `dealias_sweep` step for step, including its `valid[[r,c]]==false ->
    fold 0` masking contract (NOT Py-ART's own leftover-value-at-masked-
    gates behavior — see that file's module doc for why)."""
    import numpy as np
    from pyart.correct.region_dealias import (
        _combine_regions,
        _edge_sum_and_count,
        _EdgeTracker,
        _find_regions,
        _find_sweep_interval_splits,
        _RegionTracker,
    )

    gfilter = ~valid
    interval_splits = 3
    nyquist_interval = nyquist * 2.0
    valid_sdata = velocity[valid]
    limits = _find_sweep_interval_splits(nyquist, interval_splits, valid_sdata, 0)
    labels, nfeatures = _find_regions(velocity, gfilter, limits)

    if nfeatures < 2:
        return np.zeros(velocity.shape, dtype="int32")

    bincount = np.bincount(labels.ravel())
    region_sizes = bincount[1:]
    indices, edge_count, velos = _edge_sum_and_count(
        labels, bincount[0], velocity, rays_wrap_around, 100, 100
    )
    if len(edge_count) == 0:
        return np.zeros(velocity.shape, dtype="int32")

    region_tracker = _RegionTracker(region_sizes)
    edge_tracker = _EdgeTracker(indices, edge_count, velos, nyquist_interval, nfeatures + 1)
    while True:
        if _combine_regions(region_tracker, edge_tracker):
            break

    gates_dealiased = region_sizes.sum()
    total_folds = np.sum(region_sizes * region_tracker.unwrap_number[1:])
    sweep_offset = int(round(float(total_folds) / gates_dealiased))
    if sweep_offset != 0:
        region_tracker.unwrap_number -= sweep_offset

    folds = np.take(region_tracker.unwrap_number, labels)
    folds[gfilter] = 0  # radish's documented masked-gate contract
    return folds.astype("int32")


if __name__ == "__main__":
    raise SystemExit(main())
