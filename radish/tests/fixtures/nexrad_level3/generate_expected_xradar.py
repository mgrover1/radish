#!/usr/bin/env python3
"""Generate value-parity golden sidecars for the AF1F (packet `AF1F`) and
categorical (packet 16, `ClassInt`) NEXRAD Level 3 decode paths, using
xradar's `NEXRADLevel3File.raw_data` as an independent cross-check.

Unlike `generate_expected.py` (Tier 1, byte-exact against the local Python
oracle, packet-16 `LinearInteger`/`FloatScale` products only), this covers
product families that oracle cannot decode at all — it hard-rejects
anything but packet 16 (see `docs/NEXRAD_LEVEL3_WASM.md`). xradar's reader
supports packets 16, `AF1F`, and 28 (generic/XDR), so it's used here
instead, scoped to the two families radish actually implements: `AF1F`
(N0S, storm-relative velocity) and packet-16 `ClassInt` (N0H, hydro
class), plus two extra packet-16 tilts (N1B/N2B) for grid-shape
diversity, plus (added by plan 0012) `DAA`/170, `DU3`/173 (`Precip`) and
`HHC`/177 (`ClassInt`, confirmed via real fixtures to be packet 16, not
packet 28 as originally assumed — see `products.rs`'s module doc).
radish's `raw_codes` output is exactly xradar's `raw_data` attribute for
all of these: the same on-wire per-gate byte grid, before either side
applies a scale/category table — WITH ONE DOCUMENTED EXCEPTION, see below.

**`DTA`/172 deliberately NOT in `FIXTURES`.** xradar's PR-392 branch
emits `UserWarning: Product version 3 is newer than the latest
implemented version 1 for message code 172; decoded values may be wrong`
when decoding a real `DTA` object — its own author flags version 3's
byte layout as unverified in that branch. radish's `Precip` decode logic
is scheme-generic (170/172-175 all dispatch through the exact same
`pdb::precip_family_scale` + `decode/mod.rs` match arm, no code-172
special-casing), so 172 is NOT untested — `DAA`/170 and `DU3`/173 above
exercise the identical code path xradar itself doesn't warn about — but
it does mean 172 has no BYTE-EXACT real-fixture oracle confirmation of
its own the way the other four codes in this family do. Flagged as a
known residual gap, not silently absent.

**Packet 16's halfword-alignment pad byte (ICD Note 1) — trimmed here,
not left for the Rust test to special-case.** NEXRAD ICD 2620001AC,
Figure 3-11c ("Digital Radial Data Array Packet - Packet Code 16"), Note
1: "The RPG clips radials to 70 kft. This could result in an odd number
of bins in a radial. However, the radial will always be on a halfword
boundary, so the number of bytes in a radial may be number of bins in a
radial + 1." The packet header's `n_bins` field is the authoritative gate
count; on an odd `n_bins`, one real, on-the-wire trailing byte per radial
is inert padding, not a 1688th gate. radish's decoder honors this (see
`decode_symbology_tolerates_pad_byte_beyond_n_bins` in
`radish/src/backends/nexrad_level3/decode/symbology.rs`). xradar's
PR-392 branch (commit `9c8826c`) does NOT implement Note 1 — its own
internal `nbins` gets silently overridden to the wider, padded byte
count whenever the packet header and the first radial's declared byte
count disagree (`_read_packet16`'s "occasionally the header nbins
disagrees with the per-radial byte count; the byte count wins" comment),
so `raw_data.shape[1]` reports 1688 instead of 1687 for a real KLOT N1B
fixture. Confirmed empirically this session against 13 real fixtures:
the two odd-`n_bins` cases (N1B, N3B) both show exactly a uniform +1
byte across every radial, always 0 — matching Note 1's padding, not
real gate data — while all 11 even-`n_bins` cases show zero mismatch.
This script reads xradar's OWN unmodified `packet_header["nbins"]`
(parsed straight from the wire, upstream of that override) as the true,
ICD-authoritative bin count, and trims `raw_data` to it before hashing —
so the sidecar records the ICD-correct value, not xradar's padded one.

**Pinned to a specific, unmerged xradar commit — not a live dependency.**
xradar's NEXRAD Level 3 reader is still open as openradar/xradar#392 at
generation time; this script was run once, offline, against a checkout
of that PR pinned at commit `9c8826c` (`TST: full branch coverage for
the Level 3 reader`). The output committed under `expected/` is what
`test_nexrad_level3_xradar_oracle.rs` actually reads — it needs neither
Python nor xradar at test time, and is unaffected by that branch being
rebased, force-pushed, or deleted after the PR merges. Re-run this
generator against the released PyPI version once openradar/xradar#392
ships (and re-check whether Note 1 got fixed upstream — if so, the trim
below becomes a no-op, not wrong), updating this comment with the
release version — same discipline as the Py-ART version pin in
`test_dealias_parity.rs`.

Usage (from the radish repo root, with a checkout of xradar#392 and its
`.venv` on PYTHONPATH, or activated)::

    XRADAR_PR392_DIR=/path/to/xradar/checkout/on/the/pr-392/branch \\
    RADISH_NEXRAD_LEVEL3_FIXTURE_DIR=/path/to/fixtures \\
        python3 radish/tests/fixtures/nexrad_level3/generate_expected_xradar.py
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
from pathlib import Path

# (fixture filename, expected message code).
FIXTURES = [
    ("LOT_N0S_2026_07_17_19_30_15", 56),  # AF1F, storm-relative velocity
    ("LOT_N0H_2026_07_17_19_30_15", 165),  # packet 16, ClassInt hydro class
    ("LOT_N1B_2026_07_17_19_30_15", 153),  # packet 16, LinearInteger (extra tilt)
    ("LOT_N2B_2026_07_17_19_30_15", 153),  # packet 16, LinearInteger (extra tilt)
    # Added by plan 0012 (closing the 7 previously-deferred codes): both
    # confirmed packet 16 (`u8` raw levels, same sidecar shape as the four
    # above) by independently re-checking against real fixtures — DAA's
    # `Precip` scheme and HHC/177's `ClassInt` scheme (NOT packet 28, the
    # plan's original assumption — see `products.rs`'s module doc).
    ("LOT_DAA_2026_07_17_19_30_15", 170),  # packet 16, Precip (surface accumulation)
    ("LOT_HHC_2026_07_17_19_30_15", 177),  # packet 16, ClassInt (best-tilt composite)
    # A second, independently-fetched Precip fixture (different scene,
    # different AWIPS id — DU3, the 3h/6h user-selectable window sharing
    # message code 173 with DU6) — xradar decodes it with no product-
    # version warning (unlike DTA/172, see this script's module doc for
    # why that one is deliberately NOT in this list).
    (
        "LOT_DU3_2026_08_09_21_12_25",
        173,
    ),  # packet 16, Precip (user-selectable accumulation)
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
        # The ICD-authoritative bin count, read from xradar's own
        # unmodified packet header — upstream of its Note-1-unaware
        # override. `packet_header["nbins"]` is populated identically
        # for AF1F too (the same struct, read before xradar dispatches
        # on packet code), but AF1F's RLE expansion is exact by
        # construction (xradar's own `_read_packet_af1f` requires every
        # radial's runs to sum to exactly `nbins`, no tolerance) — so a
        # mismatch is only ever expected on packet 16.
        icd_n_bins = getattr(f, "packet_header", {}).get("nbins")
        actual_n_bins = raw_data.shape[1]
        padded = icd_n_bins is not None and actual_n_bins != icd_n_bins
        if padded:
            # ICD Note 1 is specific: exactly ONE inert pad byte, and
            # only when the wire declares MORE than the header claims
            # (never fewer). Anything else — a smaller actual width, or
            # a gap bigger than 1 — is a real anomaly, not this known
            # case; assert on the exact relationship first so a mismatch
            # in the wrong direction fails LOUDLY here rather than
            # slicing `raw_data[:, icd_n_bins:]` into a silently-empty,
            # vacuously-"all zero" array (numpy does not raise on an
            # out-of-range slice) and writing a mislabeled sidecar.
            assert actual_n_bins == icd_n_bins + 1, (
                f"{name}: header n_bins={icd_n_bins} but raw_data has "
                f"{actual_n_bins} columns — not the documented ICD Note 1 "
                f"case (exactly +1), do not trim without investigating"
            )
            pad_col = raw_data[:, icd_n_bins:]
            assert (pad_col == 0).all(), (
                f"{name}: expected the ICD Note 1 pad column to be all-zero, "
                f"got nonzero values — this is NOT a safe case to trim, "
                f"investigate before trusting this sidecar"
            )
            raw_data = raw_data[:, :icd_n_bins]

        azimuth = (f.get_azimuth() + f.get_azimuth_delta() / 2.0) % 360.0
        codes_bytes = raw_data.astype("uint8").tobytes()

        expected = {
            "fixture": name,
            "fixture_sha256": hashlib.sha256(raw).hexdigest(),
            "message_code": int(code),
            "n_radials": int(raw_data.shape[0]),
            "n_bins": int(raw_data.shape[1]),
            "azimuths": [round(float(a), 6) for a in azimuth],
            # Full-array value-exactness, without committing the array
            # itself — same convention as generate_expected.py.
            "codes_sha256": hashlib.sha256(codes_bytes).hexdigest(),
            "codes_sum": int(raw_data.astype("uint64").sum()),
            "codes_row0_sample": [int(v) for v in raw_data[0, :20]],
            "oracle": "xradar#392@9c8826c",
            "icd_note1_pad_trimmed": padded,
        }
        out_path = out_dir / f"{name}.xradar.json"
        out_path.write_text(json.dumps(expected, indent=2, sort_keys=True) + "\n")
        trim_note = " (ICD Note 1 pad column trimmed)" if padded else ""
        print(
            f"wrote {out_path} (codes_sha256={expected['codes_sha256'][:12]}...)"
            f"{trim_note}"
        )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
