"""pytest fixtures for radish tests."""

import os
from pathlib import Path

import pytest


def _fixture_dir(env_var: str) -> Path | None:
    """Resolve a fixture directory from `env_var`, returning ``None`` if
    unset/missing. See ``radish/tests/fixtures/CORPUS.md`` for layouts.
    """
    raw = os.environ.get(env_var)
    if not raw:
        return None
    p = Path(raw).expanduser()
    return p if p.is_dir() else None


def _nexrad_dir() -> Path | None:
    return _fixture_dir("RADISH_NEXRAD_FIXTURE_DIR")


@pytest.fixture
def nexrad_fixture():
    """Path to a NEXRAD Level 2 fixture file, or skip if unset.

    Resolution order:
    1. ``RADISH_NEXRAD_FIXTURE`` (single explicit file path) — the
       legacy convention; honoured for back-compat.
    2. ``RADISH_NEXRAD_FIXTURE_DIR/KLOT20251210_102338_V06`` — the
       canonical happy-path fixture documented in CORPUS.md.
    """
    path = os.environ.get("RADISH_NEXRAD_FIXTURE")
    if path and os.path.exists(path):
        return path
    fdir = _nexrad_dir()
    if fdir is not None:
        candidate = fdir / "KLOT20251210_102338_V06"
        if candidate.exists():
            return str(candidate)
    pytest.skip(
        "neither RADISH_NEXRAD_FIXTURE nor "
        "RADISH_NEXRAD_FIXTURE_DIR is set/populated — see "
        "radish/tests/fixtures/CORPUS.md"
    )


@pytest.fixture
def nexrad_kilx_fixture():
    """Path to the KILX missing-radial divergence fixture, or skip.

    ``sweep_10`` carries **360** MSG_31 records on the wire — a full 1°
    circle. radish and ``danielway/nexrad`` (an independent Rust
    implementation, wired up as a dev-dependency) both read all 360;
    xradar reports 358.

    On the wire, ``azimuth_number`` runs 1..360 contiguously in
    elevation 11, with 119 and 120 present as messages 120 and 121 of
    LDM record 49 — that record holds 122 messages (120 MSG_31 + 2
    MSG_2). xradar's ``init_record`` hard-codes a 120-message LDM
    stride, so it drops the trailing MSG_31s. Filed upstream as
    openradar/xradar#376 with a fix in openradar/xradar#377, open at
    the time of writing (not in 0.12.0, not on their ``main``).

    Pinned by ``radish/tests/test_nexrad_internal_parity.rs``. See
    CORPUS.md.
    """
    fdir = _nexrad_dir()
    if fdir is None:
        pytest.skip("RADISH_NEXRAD_FIXTURE_DIR not set — see " "radish/tests/fixtures/CORPUS.md")
    candidate = fdir / "KILX20230629_154426_V06"
    if not candidate.exists():
        pytest.skip(
            f"KILX fixture missing at {candidate} — see "
            "radish/tests/fixtures/CORPUS.md to download"
        )
    return str(candidate)


@pytest.fixture
def nexrad_chunks_dir():
    """Directory of real KLOT S/I/E chunk objects, or skip.

    55 raw objects from the `unidata-nexrad-level2-chunks` live feed
    (1 `S` + 53 `I` + 1 `E`, KLOT 2026-03-28 20:14:57 UTC), resolved
    from ``RADISH_NEXRAD_CHUNKS_DIR``. Lexicographic sort of the
    directory listing = scan order. See CORPUS.md for acquisition.
    """
    p = _fixture_dir("RADISH_NEXRAD_CHUNKS_DIR")
    if p is not None and any(p.iterdir()):
        return p
    pytest.skip(
        "RADISH_NEXRAD_CHUNKS_DIR not set or empty — see "
        "radish/tests/fixtures/CORPUS.md (Real-time chunk fixture)"
    )


@pytest.fixture
def sigmet_fixture():
    """Path to a Sigmet/IRIS RAW fixture file, or skip if unset."""
    path = os.environ.get("RADISH_SIGMET_FIXTURE")
    if not path or not os.path.exists(path):
        pytest.skip(
            "RADISH_SIGMET_FIXTURE not set or missing — set it to a "
            "Sigmet/IRIS RAW file to run Sigmet tests"
        )
    return path
