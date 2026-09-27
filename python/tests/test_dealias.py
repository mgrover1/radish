"""Tests for `radish.dealias_region_based`.

The Py-ART cross-check (`test_matches_real_pyart_on_a_dense_multi_region_sweep`)
skips cleanly when `pyart` isn't importable — it's a bonus confirmation on
top of the Rust-side unit tests (`radish/src/transforms/dealias/*.rs`),
which carry the same fixture and are the actual CI gate; this file exists
to also exercise the full Python binding path (numpy in, numpy out, error
mapping), not just the pure-Rust algorithm.
"""

import numpy as np
import pytest

pytest.importorskip("radish")

import radish


def _dense_sweep():
    """The 6x10 sinusoidal, multi-region, wrap-around, partly-masked
    sweep used in `radish/src/transforms/dealias/sweep.rs`'s
    `matches_a_real_pyart_run_on_a_denser_multi_region_wrapped_sweep`
    test — kept in sync deliberately so a Rust-side regression there and
    a Python-side regression here point at the same fixture."""
    n_rays, n_bins = 6, 10
    nyquist = 15.0
    velocity = np.zeros((n_rays, n_bins), dtype="float32")
    for r in range(n_rays):
        for b in range(n_bins):
            velocity[r, b] = 12.0 * np.sin((r / n_rays) * 2 * np.pi + b * 0.3)
    valid = np.ones((n_rays, n_bins), dtype=bool)
    valid[2, 3:6] = False
    valid[5, 0] = False
    return velocity, valid, nyquist


# `_dense_sweep()` is deliberately tiny (6x10) to keep the region/edge
# graph dense and hand-checkable — but `radish.dealias_region_based`'s
# own Py-ART-default `skip_between_rays`/`skip_along_ray` (100) now
# error if they exceed the sweep's own dimensions (a real CRITICAL
# CPU-hang bug found via adversarial review: with `rays_wrap_around=True`
# and a skip larger than the ray count, the gap-search loop re-traverses
# the same ring repeatedly instead of just once — see
# `radish/src/transforms/dealias/mod.rs`'s `# Errors` doc). `100` is sized
# for real WSR-88D sweeps (hundreds of rays/gates), not this 6x10 test
# fixture, so every call below caps skip at the fixture's own dimensions
# instead of relying on the (now-rejected-here) defaults. Verified this
# session to produce the IDENTICAL fold pattern as `skip=100` on this
# specific data — the fixture's masked patches are at most 3 gates wide,
# far under either bound.
_SAFE_SKIP = {"skip_between_rays": 6, "skip_along_ray": 10}


def test_dealias_region_based_is_exposed():
    assert hasattr(radish, "dealias_region_based")


def test_returns_int32_folds_of_the_input_shape():
    velocity, valid, nyquist = _dense_sweep()
    folds = radish.dealias_region_based(velocity, valid, nyquist, True, **_SAFE_SKIP)
    assert folds.dtype == np.int32
    assert folds.shape == velocity.shape


def test_masked_gates_always_fold_zero():
    velocity, valid, nyquist = _dense_sweep()
    folds = radish.dealias_region_based(velocity, valid, nyquist, True, **_SAFE_SKIP)
    assert folds[2, 3] == 0
    assert folds[2, 4] == 0
    assert folds[2, 5] == 0
    assert folds[5, 0] == 0


def test_matches_the_known_fold_pattern():
    # Same expected array as the Rust-side test — see this module's
    # docstring for why both exist.
    velocity, valid, nyquist = _dense_sweep()
    folds = radish.dealias_region_based(velocity, valid, nyquist, True, **_SAFE_SKIP)
    expected = np.array(
        [
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            [0, 0, 0, 0, 0, 0, 1, 1, 1, 1],
            [0, 0, 0, 0, 0, 0, 1, 1, 1, 1],
            [1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
            [1, 1, 1, 1, 1, 1, 0, 0, 0, 0],
            [0, 1, 1, 0, 0, 0, 0, 0, 0, 0],
        ],
        dtype=np.int32,
    )
    np.testing.assert_array_equal(folds, expected)


def test_corrected_velocity_is_continuous_across_the_unfolded_boundary():
    velocity, valid, nyquist = _dense_sweep()
    folds = radish.dealias_region_based(velocity, valid, nyquist, True, **_SAFE_SKIP)
    corrected = velocity + folds.astype("float32") * 2.0 * nyquist
    # Row 3 (all one region, fold=1) vs row 2's unfolded tail (fold=1,
    # cols 6-9): adjacent rays, should be a small, physically plausible
    # jump rather than a raw +-2*nyquist discontinuity.
    assert abs(float(corrected[2, 6] - corrected[3, 6])) < 2.0 * nyquist


def test_rejects_shape_mismatch():
    velocity, valid, nyquist = _dense_sweep()
    with pytest.raises(ValueError, match="shape"):
        radish.dealias_region_based(velocity, valid[:2, :2], nyquist, True, **_SAFE_SKIP)


@pytest.mark.parametrize("bad_nyquist", [0.0, -5.0, float("nan"), float("inf")])
def test_rejects_non_finite_or_non_positive_nyquist(bad_nyquist):
    velocity, valid, _ = _dense_sweep()
    with pytest.raises(ValueError, match="nyquist"):
        radish.dealias_region_based(velocity, valid, bad_nyquist, True, **_SAFE_SKIP)


def test_rejects_zero_interval_splits():
    velocity, valid, nyquist = _dense_sweep()
    with pytest.raises(ValueError, match="interval_splits"):
        radish.dealias_region_based(velocity, valid, nyquist, True, interval_splits=0, **_SAFE_SKIP)


@pytest.mark.parametrize("bad_skip", [{"skip_between_rays": -1}, {"skip_along_ray": -1}])
def test_rejects_negative_skip_values(bad_skip):
    velocity, valid, nyquist = _dense_sweep()
    kwargs = {**_SAFE_SKIP, **bad_skip}
    with pytest.raises(ValueError, match="non-negative"):
        radish.dealias_region_based(velocity, valid, nyquist, True, **kwargs)


def test_rejects_skip_values_larger_than_the_sweep_itself():
    # The actual CRITICAL bug this session's adversarial review found:
    # skip_between_rays/skip_along_ray fed an unbounded gap-search loop
    # with no upper bound at all (an i32::MAX skip with
    # rays_wrap_around=True on a real-sized sweep was a
    # ~10**13-iteration CPU hang). Py-ART's own default (100) exceeds
    # THIS fixture's 6-ray/10-gate dimensions, which is exactly why every
    # other test in this file overrides it with `_SAFE_SKIP`.
    velocity, valid, nyquist = _dense_sweep()
    with pytest.raises(ValueError, match="must not exceed"):
        radish.dealias_region_based(velocity, valid, nyquist, True, skip_between_rays=1_000_000_000)


def test_rejects_nan_or_infinite_velocity_at_a_valid_gate():
    velocity, valid, nyquist = _dense_sweep()
    velocity[0, 0] = np.nan
    with pytest.raises(ValueError, match="NaN|infinite"):
        radish.dealias_region_based(velocity, valid, nyquist, True, **_SAFE_SKIP)


def test_default_options_match_pyart_defaults():
    # No error with only the required positional args -- confirms the
    # keyword defaults (interval_splits=3, skip_between_rays=100,
    # skip_along_ray=100, centered=True) are wired, not just documented.
    # Uses a bigger, sparser array than `_dense_sweep()` specifically so
    # the real Py-ART-sized default skip values (100) fit — see
    # `_SAFE_SKIP`'s comment for why the dense fixture can't be reused
    # here.
    n_rays, n_gates = 150, 150
    velocity = np.zeros((n_rays, n_gates), dtype="float32")
    for r in range(n_rays):
        velocity[r, :] = 8.0 if r < n_rays // 2 else -8.0
    valid = np.ones((n_rays, n_gates), dtype=bool)
    nyquist = 10.0

    default = radish.dealias_region_based(velocity, valid, nyquist, True)
    explicit = radish.dealias_region_based(
        velocity,
        valid,
        nyquist,
        True,
        interval_splits=3,
        skip_between_rays=100,
        skip_along_ray=100,
        centered=True,
    )
    np.testing.assert_array_equal(default, explicit)


def test_matches_real_pyart_on_a_dense_multi_region_sweep():
    """The strongest available check in this file: drives actual Py-ART
    internals on the SAME array `radish.dealias_region_based` just ran,
    end to end, and compares fold counts. Skips cleanly if Py-ART isn't
    installed in this environment (it's a dev/cross-check dependency,
    not a runtime one — see `radish/tests/test_dealias_parity.rs` for the
    formal, pinned-version golden-corpus gate this is a lightweight
    preview of)."""
    pytest.importorskip("pyart")
    from pyart.correct.region_dealias import (
        _combine_regions,
        _edge_sum_and_count,
        _EdgeTracker,
        _find_regions,
        _find_sweep_interval_splits,
        _RegionTracker,
    )

    velocity, valid, nyquist = _dense_sweep()
    gfilter = ~valid  # Py-ART's polarity is inverted from radish's `valid`

    interval_splits = 3
    nyquist_interval = nyquist * 2.0
    valid_sdata = velocity[~gfilter]
    limits = _find_sweep_interval_splits(nyquist, interval_splits, valid_sdata, 0)
    labels, nfeatures = _find_regions(velocity, gfilter, limits)
    bincount = np.bincount(labels.ravel())
    region_sizes = bincount[1:]

    # 6/10 (== _SAFE_SKIP), not Py-ART's own 100/100 default — kept
    # identical on both sides of this comparison; see _SAFE_SKIP's
    # comment for why 100 doesn't fit this fixture's own dimensions.
    indices, edge_count, velos = _edge_sum_and_count(
        labels,
        bincount[0],
        velocity,
        True,
        _SAFE_SKIP["skip_between_rays"],
        _SAFE_SKIP["skip_along_ray"],
    )
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
    pyart_folds = np.take(region_tracker.unwrap_number, labels)
    pyart_folds[gfilter] = 0  # radish's documented masked-gate contract

    radish_folds = radish.dealias_region_based(velocity, valid, nyquist, True, **_SAFE_SKIP)
    np.testing.assert_array_equal(radish_folds, pyart_folds)
