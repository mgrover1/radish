#!/usr/bin/env python3
"""Generate a value-parity golden sidecar for `RATE`/code 176 (`DPR`) —
packet 28 (XDR, generic data packet), `u16` raw levels — using xradar's
`NEXRADLevel3File.raw_data` as an independent cross-check, the same
methodology `generate_expected_xradar.py` uses for the packet-16/AF1F
(`u8`) families. Split into its own script rather than folded into that
one because the raw-code dtype genuinely differs (`uint16`, not `uint8`)
— a single shared `codes.astype("uint8")` call would silently truncate
every `DPR` raw level above 255, corrupting the comparison it's meant to
protect.

Also records the ray-centre azimuth using xradar's own `get_azimuth()` +
`get_azimuth_delta()/2.0` convention — confirmed (plan 0012 §3.3 step 6,
after an earlier pass got this wrong) against NEXRAD ICD 2620001AC
Appendix E, Figure E-4 to apply to packet 28 exactly as it does to packet
16/AF1F: the `azimuth` field is documented as "Azimuth of the LEADING EDGE
of the radial", not a ray centre, for every packet family including 28.

Same "pinned to a specific, unmerged xradar commit" caveat as
`generate_expected_xradar.py` — see that script's module doc.

Usage (from the radish repo root, with a checkout of xradar#392 and its
`.venv` on PYTHONPATH, or activated)::

    XRADAR_PR392_DIR=/path/to/xradar/checkout/on/the/pr-392/branch \\
    RADISH_NEXRAD_LEVEL3_FIXTURE_DIR=/path/to/fixtures \\
        python3 radish/tests/fixtures/nexrad_level3/generate_expected_xradar_u16.py
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
from pathlib import Path

FIXTURES = [
    ("LOT_DPR_2026_07_17_19_30_15", 176),  # packet 28/XDR, Rate
]


def main() -> int:
    xradar_dir = os.environ.get("XRADAR_PR392_DIR")
    if not xradar_dir:
        print(
            "XRADAR_PR392_DIR must be set to a checkout of xradar with the "
            "Level 3 reader (openradar/xradar#392) — see this script's "
            "module doc.",
            file=sys.stderr,
        )
        return 1
    sys.path.insert(0, xradar_dir)
    from xradar.io.backends.nexrad_level3 import NEXRADLevel3File

    fixture_dir = Path(
        os.environ.get(
            "RADISH_NEXRAD_LEVEL3_FIXTURE_DIR",
            str(Path.home() / ".cache/radish/fixtures/nexrad_level3"),
        )
    )

    out_dir = Path(__file__).parent / "expected"
    out_dir.mkdir(exist_ok=True)

    for name, expected_code in FIXTURES:
        raw = (fixture_dir / name).read_bytes()
        f = NEXRADLevel3File(raw)
        code = f.msg_header["code"]
        if code != expected_code:
            print(
                f"warning: {name} has message code {code}, expected "
                f"{expected_code} — FIXTURES table may be stale",
                file=sys.stderr,
            )

        raw_data = f.raw_data
        assert raw_data.dtype == "uint16", (
            f"{name}: expected xradar's raw_data to be uint16 for a packet-28 "
            f"product, got {raw_data.dtype} — FIXTURES table or this script's "
            f"dtype assumption may be stale"
        )

        azimuth = (f.get_azimuth() + f.get_azimuth_delta() / 2.0) % 360.0
        codes_bytes = raw_data.astype(
            ">u2"
        ).tobytes()  # big-endian, matching radish's own byte order

        expected = {
            "fixture": name,
            "fixture_sha256": hashlib.sha256(raw).hexdigest(),
            "message_code": int(code),
            "n_radials": int(raw_data.shape[0]),
            "n_bins": int(raw_data.shape[1]),
            "azimuths": [round(float(a), 6) for a in azimuth],
            "codes_sha256": hashlib.sha256(codes_bytes).hexdigest(),
            "codes_sum": int(raw_data.astype("uint64").sum()),
            "codes_row0_sample": [int(v) for v in raw_data[0, :20]],
            "oracle": "xradar#392@9c8826c",
        }
        out_path = out_dir / f"{name}.xradar.u16.json"
        out_path.write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n")
        print(f"wrote {out_path} (codes_sha256={expected['codes_sha256'][:12]}...)")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
