"""Wall-clock benchmark: radish (engine='radish') vs xradar on a Sigmet/IRIS file.

Set ``RADISH_SIGMET_FIXTURE`` to the path of an IRIS RAW file before running.
Reports median of N runs and the speedup factor.
"""

import os
import time

import xarray as xr
import xradar


N = 5


def time_n(fn, label, n=N):
    times = []
    for _ in range(n):
        t = time.perf_counter()
        fn()
        times.append(time.perf_counter() - t)
    times.sort()
    median = times[n // 2]
    print(f"  {label}: median={median:.3f}s  runs={[round(x, 3) for x in times]}")
    return median


def main():
    path = os.environ.get("RADISH_SIGMET_FIXTURE")
    if not path or not os.path.exists(path):
        raise SystemExit(
            "RADISH_SIGMET_FIXTURE must point at an IRIS RAW file"
        )
    print(f"Fixture: {path}  ({os.path.getsize(path) / 1e6:.1f} MB)")
    print(f"Runs: {N}\n")

    print("xradar:")
    xradar_t = time_n(lambda: xradar.io.open_iris_datatree(path), "xradar")

    print("radish (xarray engine):")
    radish_t = time_n(lambda: xr.open_datatree(path, engine="radish"), "radish")

    speedup = xradar_t / radish_t if radish_t > 0 else float("inf")
    print(f"\nSpeedup (xradar / radish): {speedup:.2f}x")


if __name__ == "__main__":
    main()
