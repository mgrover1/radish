#!/usr/bin/env python3
"""Generate byte-parity golden files for the NEXRAD Level 3 (NIDS) test
corpus, from an independent Python NIDS decoder used as the byte-level
oracle — see `docs/NEXRAD_LEVEL3_WASM.md` for what that oracle needs to
provide and how it was cross-checked.

This is a one-time generation step, run by a developer when the fixture
list changes — NOT part of `cargo test` or CI. The committed OUTPUT
(`expected/<fixture>.json`) is what `test_nexrad_level3_parity.rs` actually
reads; it does not need Python, numpy, or the oracle decoder available at
test time.

Deliberately does NOT commit the raw codes array (up to ~1.3 MB per
fixture, x7): a SHA-256 of the row-major bytes is exactly as strong a
byte-exact assertion for pass/fail purposes, without adding megabytes of
binary blob to the repo — the same pattern `radish/tests/fixtures/CORPUS.md`
already uses for the (uncommitted) raw NEXRAD Level 2 fixtures. A short
prefix sample is kept for a human-readable diff on failure.

Usage (from the radish repo root)::

    NEXRAD_LEVEL3_ORACLE_DIR=/path/to/oracle/checkout \\
    RADISH_NEXRAD_LEVEL3_FIXTURE_DIR=/path/to/oracle/checkout/tests/fixtures/level3 \\
        python3 radish/tests/fixtures/nexrad_level3/generate_expected.py

`NEXRAD_LEVEL3_ORACLE_DIR` must point at a checkout exposing a
`server.nexrad_level3` module with the same `decode(bytes) -> Sweep`
contract described in `docs/NEXRAD_LEVEL3_WASM.md` — this script itself
carries no oracle implementation, only the harness that drives one.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
from pathlib import Path

FIXTURES = [
    "LOT_N0B_2026_07_31_13_06_53",
    "LOT_N3B_2026_07_31_13_02_14",
    "LOT_N0X_2020_03_30_00_02_07",
    "LOT_N0C_2020_03_31_00_05_24",
    "LOT_N0K_2020_03_31_00_05_24",
    "LOT_N0G_2026_08_04_00_09_57",
    "LOT_N2U_2026_08_04_00_09_57",
]


def main() -> int:
    oracle_env = os.environ.get("NEXRAD_LEVEL3_ORACLE_DIR")
    if not oracle_env:
        print(
            "NEXRAD_LEVEL3_ORACLE_DIR must be set to a checkout of the oracle "
            "decoder — see this script's module doc.",
            file=sys.stderr,
        )
        return 1
    oracle_dir = Path(oracle_env)
    fixture_dir = Path(
        os.environ.get(
            "RADISH_NEXRAD_LEVEL3_FIXTURE_DIR",
            str(oracle_dir / "tests/fixtures/level3"),
        )
    )
    sys.path.insert(0, str(oracle_dir / "src"))
    from server import nexrad_level3 as l3  # noqa: E402  (path injected above)

    out_dir = Path(__file__).parent / "expected"
    out_dir.mkdir(exist_ok=True)

    for name in FIXTURES:
        raw = (fixture_dir / name).read_bytes()
        sw = l3.decode(raw)
        codes_bytes = sw.codes.tobytes()  # row-major, exactly what Array2<u8> is
        expected = {
            "fixture": name,
            "fixture_sha256": hashlib.sha256(raw).hexdigest(),
            "site": sw.site,
            "product": sw.product,
            "moment": sw.moment,
            "tilt": sw.tilt,
            "message_code": sw.message_code,
            "vcp": sw.vcp,
            "elevation_deg": sw.elevation_deg,
            "scan_time_unix": sw.scan_time.timestamp(),
            "lat": sw.lat,
            "lon": sw.lon,
            "height_m": sw.height_m,
            "n_radials": sw.n_radials,
            "n_bins": sw.n_bins,
            "first_gate_m": sw.first_gate_m,
            "gate_spacing_m": sw.gate_spacing_m,
            "value_min": sw.value_min,
            "value_increment": sw.value_increment,
            "n_levels": sw.n_levels,
            "azimuths": sw.azimuths.tolist(),
            # Full-array byte-exactness, without committing the array itself.
            "codes_sha256": hashlib.sha256(codes_bytes).hexdigest(),
            "codes_sum": int(sw.codes.astype("uint64").sum()),
            # Human-readable sample for a failure message, not part of the gate.
            "codes_row0_sample": sw.codes[0, :20].tolist(),
        }
        out_path = out_dir / f"{name}.json"
        out_path.write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n")
        print(f"wrote {out_path} (codes_sha256={expected['codes_sha256'][:12]}...)")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
