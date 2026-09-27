"""Tests for the low-level NEXRAD per-moment decoders (issue #32).

Two tiers:

* **Fixture-free** — synthetic Message 31 records built in-process by
  `_radial()` below, plus argument-validation checks. These run
  everywhere, including CI, and pin the Python-visible contract.
* **Fixture-gated** — real-file parity against xradar, gated on
  `RADISH_NEXRAD_FIXTURE_DIR` / `RADISH_NEXRAD_KVNX_DIR`. See
  `radish/tests/fixtures/CORPUS.md`.
"""

import bz2
import os
import struct
from pathlib import Path

import numpy as np
import pytest

import radish

# ────────────────────────────────────────────────────────────────────
# Synthetic Message 31 builders
#
# Deliberately hand-rolled from `struct` rather than reusing anything
# in radish, so these tests are an independent statement of the wire
# format rather than a restatement of the decoder.
# ────────────────────────────────────────────────────────────────────

DATA_HEADER_SIZE = 72  # Build-12+: 32 fixed bytes + 10 x u32 pointers


def _block(name, word_size, scale, offset, gates):
    """One moment data block: 4-byte id + 24-byte descriptor + gates."""
    buf = bytearray(name)
    buf += struct.pack(
        ">IHHHHhBBff",
        0,  # reserved
        len(gates),  # number of gates
        2125,  # range to first gate, 0.001 km
        250,  # sample interval, 0.001 km
        0,  # TOVER
        0,  # SNR threshold
        0,  # control flags
        word_size,
        scale,
        offset,
    )
    fmt = ">B" if word_size == 8 else ">H"
    for g in gates:
        buf += struct.pack(fmt, g)
    return bytes(buf)


def _radial(azimuth, blocks, elevation_number=1):
    """One Message 31 radial, framed with its 28-byte message header."""
    body = bytearray(b"KVNX")
    body += struct.pack(
        ">IHHfBBHBBBBfBBH",
        0,  # collection time, ms
        20405,  # modified julian date
        1,  # azimuth number
        azimuth,
        0,  # compression
        0,  # spare
        0,  # radial length
        2,  # azimuth resolution
        1,  # radial status
        elevation_number,
        0,  # cut sector
        0.5,  # elevation angle
        0,  # spot blanking
        0,  # azimuth indexing
        len(blocks),  # data block count
    )
    # Blocks pack contiguously into the leading pointer slots; the
    # decoder routes by block name, not by slot index.
    payload, pointers, cursor = bytearray(), [0] * 10, DATA_HEADER_SIZE
    for i, block in enumerate(blocks):
        pointers[i] = cursor
        cursor += len(block)
        payload += block
    body += struct.pack(">10I", *pointers)
    assert len(body) == DATA_HEADER_SIZE
    body += payload
    if len(body) % 2:
        body += b"\0"  # message size is counted in halfwords

    header = bytes(12) + struct.pack(  # TCM prefix, zero-filled
        ">HBBHHIHH", (16 + len(body)) // 2, 0, 31, 0, 0, 0, 1, 1
    )
    return header + bytes(body)


def _ldm(stream):
    """Wrap a decompressed message stream as one LDM record."""
    payload = bz2.compress(stream)
    return struct.pack(">i", len(payload)) + payload


def _ref(gates):
    return _block(b"DREF", 8, 2.0, 66.0, gates)


def _zdr8(gates):
    return _block(b"DZDR", 8, 16.0, 128.0, gates)


def _zdr16(gates):
    return _block(b"DZDR", 16, 32.0, 418.0, gates)


# ────────────────────────────────────────────────────────────────────
# Output contract
# ────────────────────────────────────────────────────────────────────


def test_decode_record_moment_returns_raw_words():
    record = _radial(30.0, [_ref([0, 1, 130, 2])])
    out = radish.decode_record_moment(record, "REF", (1, 4), np.uint8)
    assert out.dtype == np.uint8
    assert out.shape == (1, 4)
    np.testing.assert_array_equal(out, [[0, 1, 130, 2]])


@pytest.mark.parametrize(
    "bare,canonical",
    [
        ("decode_record_moment", "decode_nexrad_record_moment"),
        ("decode_sweep_moment", "decode_nexrad_sweep_moment"),
        ("record_moment_encoding", "nexrad_record_moment_encoding"),
        ("sweep_moment_encoding", "nexrad_sweep_moment_encoding"),
    ],
)
def test_unqualified_names_alias_the_canonical_ones(bare, canonical):
    """The format-qualified names are canonical; the bare names issue #32
    introduced are kept as aliases pointing at the same objects. Pin that
    so a refactor can't silently drop the issue's `hasattr` contract."""
    assert getattr(radish, bare) is getattr(radish, canonical)
    assert bare in radish.__all__ and canonical in radish.__all__


def test_rows_follow_record_order():
    record = _radial(90.0, [_ref([10, 11])]) + _radial(10.0, [_ref([20, 21])])
    out = radish.decode_record_moment(record, "REF", (2, 2), np.uint8)
    np.testing.assert_array_equal(out, [[10, 11], [20, 21]])


def test_short_moments_pad_with_raw_zero_but_missing_rows_use_fill_value():
    """The two are distinct: padding is raw 0 (xradar parity) while
    absent rows carry the caller's `fill_value`."""
    record = _radial(30.0, [_ref([7, 8])])
    out = radish.decode_record_moment(record, "REF", (2, 4), np.uint8, fill_value=255)
    np.testing.assert_array_equal(out, [[7, 8, 0, 0], [255, 255, 255, 255]])


def test_absent_moment_leaves_the_row_at_fill_value():
    record = _radial(1.0, [_ref([1, 2]), _zdr8([3, 4])]) + _radial(2.0, [_ref([5, 6])])
    out = radish.decode_record_moment(record, "ZDR", (2, 2), np.uint8, fill_value=99)
    np.testing.assert_array_equal(out, [[3, 4], [99, 99]])


def test_record_without_radials_is_all_fill_not_an_error():
    """The `S` chunk of a chunked volume carries only MSG_2/MSG_5."""
    out = radish.decode_record_moment(b"", "REF", (2, 3), np.uint8, fill_value=7)
    np.testing.assert_array_equal(out, np.full((2, 3), 7, np.uint8))


def test_odim_moment_names_are_accepted():
    record = _radial(30.0, [_ref([5, 6])])
    np.testing.assert_array_equal(
        radish.decode_record_moment(record, "DBZH", (1, 2), np.uint8),
        radish.decode_record_moment(record, "REF", (1, 2), np.uint8),
    )


# ────────────────────────────────────────────────────────────────────
# Refusals — radish never silently returns wrong or partial data
# ────────────────────────────────────────────────────────────────────


def test_too_many_gates_raises_rather_than_truncating():
    record = _radial(30.0, [_ref([1, 2, 3, 4])])
    with pytest.raises(radish.MomentEncodingError, match="4 gates"):
        radish.decode_record_moment(record, "REF", (1, 2), np.uint8)


def test_too_many_radials_raises_rather_than_dropping():
    record = _radial(1.0, [_ref([1])]) + _radial(2.0, [_ref([2])])
    with pytest.raises(radish.MomentEncodingError, match="2 MSG_31 radials"):
        radish.decode_record_moment(record, "REF", (1, 1), np.uint8)


def test_word_size_mismatch_without_a_target_raises():
    record = _radial(30.0, [_zdr8([10, 20])])
    with pytest.raises(radish.MomentEncodingError, match="scale=/offset="):
        radish.decode_record_moment(record, "ZDR", (1, 2), np.uint16)


def test_moment_encoding_error_is_a_value_error():
    """Callers that only care about 'the request was refused' can catch
    ValueError without importing the radish-specific type."""
    assert issubclass(radish.MomentEncodingError, ValueError)


@pytest.mark.parametrize("dtype", [np.float32, np.float64, np.int16, np.uint32])
def test_non_uint8_uint16_dtypes_are_rejected(dtype):
    with pytest.raises(TypeError, match="uint8 or uint16"):
        radish.decode_record_moment(b"", "REF", (1, 1), dtype)


@pytest.mark.parametrize("dtype", [np.uint8, ">u1", "uint8", np.dtype("u1")])
def test_dtype_spellings_are_all_accepted(dtype):
    # ">u1" is fine: byte order is meaningless for a 1-byte word, and
    # numpy reports it as "|" (not applicable).
    assert radish.decode_record_moment(b"", "REF", (1, 1), dtype).dtype == np.uint8


@pytest.mark.parametrize("dtype", [np.uint16, "=u2", "uint16", np.dtype("u2")])
def test_native_uint16_spellings_decode_correct_values(dtype):
    record = _radial(30.0, [_zdr16([0x0123, 0x0456])])
    out = radish.decode_record_moment(record, "ZDR", (1, 2), dtype)
    assert out.dtype == np.uint16
    # Values, not just dtype — only this catches a byte swap.
    np.testing.assert_array_equal(out, [[0x0123, 0x0456]])


@pytest.mark.parametrize("dtype", [">u2", np.dtype(">u2"), "<u2" if np.little_endian else ">u2"])
def test_non_native_byte_order_is_rejected(dtype):
    """These decoders return raw transport words. An array that compares
    equal element-wise but whose .tobytes() is byte-swapped would be
    silent corruption for the zarr/reference-store audience, so a
    non-native request is refused rather than quietly ignored."""
    if np.dtype(dtype).byteorder in ("=", "|"):
        pytest.skip("dtype is native on this platform")
    with pytest.raises(TypeError, match="non-native byte order"):
        radish.decode_record_moment(b"", "REF", (1, 1), dtype)


@pytest.mark.parametrize("bad", [object(), "nonsense", 3.5])
def test_non_dtype_arguments_raise_type_error(bad):
    with pytest.raises(TypeError):
        radish.decode_record_moment(b"", "REF", (1, 1), bad)


def test_moment_encoding_error_survives_pickling():
    """Workers that raise this must keep the typed exception when it
    crosses a process boundary — the whole point of the parallel
    chunked workflow."""
    import pickle

    revived = pickle.loads(pickle.dumps(radish.MomentEncodingError("boom")))
    assert isinstance(revived, radish.MomentEncodingError)
    assert isinstance(revived, ValueError)


@pytest.mark.parametrize("wrap", [bytes, bytearray])
def test_bytes_and_bytearray_inputs_are_accepted(wrap):
    record = _radial(30.0, [_ref([1, 2])])
    out = radish.decode_record_moment(wrap(record), "REF", (1, 2), np.uint8)
    np.testing.assert_array_equal(out, [[1, 2]])


@pytest.mark.parametrize("wrap", [memoryview, lambda b: np.frombuffer(b, np.uint8)])
def test_buffer_protocol_inputs_are_rejected_not_silently_misread(wrap):
    """Pinning current behaviour: only bytes/bytearray are accepted.
    Callers holding an mmap slice or memoryview must call bytes() first.
    If this ever becomes a zero-copy path, this test should flip."""
    record = _radial(30.0, [_ref([1, 2])])
    with pytest.raises(TypeError):
        radish.decode_record_moment(wrap(record), "REF", (1, 2), np.uint8)


def test_unknown_moment_name_raises_value_error():
    with pytest.raises(ValueError, match="unknown NEXRAD moment"):
        radish.decode_record_moment(b"", "DBZ", (1, 1), np.uint8)


def test_scale_and_offset_must_be_given_together():
    with pytest.raises(ValueError, match="together"):
        radish.decode_record_moment(b"", "ZDR", (1, 1), np.uint16, scale=32.0)


# ────────────────────────────────────────────────────────────────────
# Cross-RDA-build remapping — the correctness trap from issue #32
# ────────────────────────────────────────────────────────────────────


def test_kvnx_zdr_8bit_remaps_exactly_onto_the_16bit_grid():
    """The issue's case: raw16 = 2 * raw8 + 162, exact in physical units."""
    record = _radial(30.0, [_zdr8([0, 1, 128, 255])])
    out = radish.decode_record_moment(record, "ZDR", (1, 4), np.uint16, scale=32.0, offset=418.0)
    np.testing.assert_array_equal(out, [[162, 164, 418, 672]])

    source = (np.array([0, 1, 128, 255], np.float64) - 128.0) / 16.0
    target = (out[0].astype(np.float64) - 418.0) / 32.0
    np.testing.assert_array_equal(source, target)


def test_mixed_era_radials_land_on_one_common_grid():
    """-8.0 dB is raw 0 on the 8-bit grid and raw 162 on the 16-bit one;
    both must decode to 162 when the 16-bit grid is requested."""
    record = _radial(1.0, [_zdr8([0])]) + _radial(2.0, [_zdr16([162])])
    out = radish.decode_record_moment(record, "ZDR", (2, 1), np.uint16, scale=32.0, offset=418.0)
    np.testing.assert_array_equal(out, [[162], [162]])


def test_same_width_different_scale_is_refused():
    """Adversarial regression: two radials at the same word size but
    different scale/offset used to stack into one array with no error,
    so the single scale_factor/add_offset the inspector hands back
    decoded half of it onto the wrong physical grid."""
    coarse = _block(b"DZDR", 8, 16.0, 128.0, [128, 144, 160])  # 0,1,2 dB
    fine = _block(b"DZDR", 8, 8.0, 64.0, [64, 72, 80])  # 0,1,2 dB
    record = _radial(1.0, [coarse]) + _radial(2.0, [fine])

    with pytest.raises(radish.MomentEncodingError, match="mixes on-wire"):
        radish.decode_record_moment(record, "ZDR", (2, 3), np.uint8)

    # An explicit target grid must still work — that is the remap's job.
    out = radish.decode_record_moment(record, "ZDR", (2, 3), np.uint16, scale=16.0, offset=128.0)
    physical = out / 16.0 - 128.0 / 16.0
    np.testing.assert_array_equal(physical, [[0, 1, 2], [0, 1, 2]])


def test_same_width_different_scale_is_refused_across_records():
    coarse = _block(b"DZDR", 8, 16.0, 128.0, [128])
    fine = _block(b"DZDR", 8, 8.0, 64.0, [64])
    span = _ldm(_radial(1.0, [coarse])) + _ldm(_radial(2.0, [fine]))
    with pytest.raises(radish.MomentEncodingError, match="mixes on-wire"):
        radish.decode_sweep_moment(span, "ZDR", (2, 1), np.uint8)


@pytest.mark.parametrize(
    "azimuths",
    [[0.0, -0.0, 5.0], [1.0, float("nan"), 0.5], [1.0, -float("nan"), 0.5]],
    ids=["signed-zero", "nan", "negative-nan"],
)
def test_azimuth_sort_matches_numpy_argsort_exactly(azimuths):
    """The docs tell callers to reorder coordinates with
    np.argsort(kind="stable"); f32::total_cmp does NOT match it on
    signed zero or NaN, which would misalign rows against coordinates."""
    span = _ldm(b"".join(_radial(az, [_ref([i + 1])]) for i, az in enumerate(azimuths)))
    shape = (len(azimuths), 1)
    unsorted = radish.decode_sweep_moment(span, "REF", shape, np.uint8)
    sorted_rows = radish.decode_sweep_moment(span, "REF", shape, np.uint8, sort_by_azimuth=True)
    order = np.argsort(np.asarray(azimuths, dtype=np.float32), kind="stable")
    np.testing.assert_array_equal(unsorted[order], sorted_rows)


def test_oversized_span_is_refused_without_allocating_every_record():
    """Adversarial regression: per-record buffers were all allocated
    before the total row count was checked, so peak memory was
    records x rays x gates — ~1.5 GiB from 2 MiB of input."""
    stream = b"".join(_radial(float(i), [_ref([1])]) for i in range(4))
    span = _ldm(stream) * 8
    with pytest.raises(radish.MomentEncodingError, match="MSG_31 radials"):
        radish.decode_sweep_moment(span, "REF", (4, 1), np.uint8)


def test_inexact_remap_is_refused_rather_than_approximated():
    record = _radial(30.0, [_zdr8([10])])
    with pytest.raises(radish.MomentEncodingError, match="not an exact integer"):
        radish.decode_record_moment(record, "ZDR", (1, 1), np.uint16, scale=24.0, offset=418.0)


def test_remap_overflowing_the_output_width_is_refused():
    record = _radial(30.0, [_zdr8([10])])
    with pytest.raises(radish.MomentEncodingError, match="overflows the uint8 ZDR grid"):
        radish.decode_record_moment(record, "ZDR", (1, 1), np.uint8, scale=32.0, offset=256.0)


def test_zdr16_and_phi16_are_masked_to_their_significant_bits():
    record = _radial(
        30.0,
        [
            _zdr16([0xF7FF]),
            _block(b"DPHI", 16, 2.8361, 2.0, [0xFFFF]),
        ],
    )
    zdr = radish.decode_record_moment(record, "ZDR", (1, 1), np.uint16)
    phi = radish.decode_record_moment(record, "PHI", (1, 1), np.uint16)
    assert zdr[0, 0] == 0x07FF, "ZDR is an 11-bit field"
    assert phi[0, 0] == 0x03FF, "PHI is a 10-bit field"


# ────────────────────────────────────────────────────────────────────
# Sweep-span path
# ────────────────────────────────────────────────────────────────────


def test_sweep_span_stitches_records_in_order():
    span = _ldm(_radial(90.0, [_ref([1, 2])])) + _ldm(
        _radial(10.0, [_ref([3, 4])]) + _radial(20.0, [_ref([5, 6])])
    )
    out = radish.decode_sweep_moment(span, "REF", (3, 2), np.uint8)
    np.testing.assert_array_equal(out, [[1, 2], [3, 4], [5, 6]])


def test_sweep_span_skips_a_leading_ar2v_volume_header():
    span = b"AR2V0006.001-XYZWXYZWXYZW"[:24] + _ldm(_radial(5.0, [_ref([9, 8])]))
    out = radish.decode_sweep_moment(span, "REF", (1, 2), np.uint8)
    np.testing.assert_array_equal(out, [[9, 8]])


def test_sort_by_azimuth_matches_a_stable_numpy_argsort():
    stream = b"".join(
        _radial(az, [_ref([val])]) for az, val in [(270.0, 1), (10.0, 2), (10.0, 3), (90.0, 4)]
    )
    span = _ldm(stream)
    out = radish.decode_sweep_moment(
        span, "REF", (6, 1), np.uint8, fill_value=200, sort_by_azimuth=True
    )
    # Ties keep record order, and trailing fill rows stay at the end.
    np.testing.assert_array_equal(out.ravel(), [2, 3, 4, 1, 200, 200])

    # The documented way to reorder coordinates the same way.
    unsorted = radish.decode_sweep_moment(span, "REF", (6, 1), np.uint8, fill_value=200)
    azimuth = radish.sweep_moment_encoding(span)["azimuth"]
    order = np.argsort(azimuth, kind="stable")
    np.testing.assert_array_equal(unsorted[: len(order)][order], out[: len(order)])


# ────────────────────────────────────────────────────────────────────
# Encoding inspector
# ────────────────────────────────────────────────────────────────────


def test_record_moment_encoding_reports_headers_and_encodings():
    record = _radial(10.5, [_ref([1, 2, 3]), _zdr8([4, 5])]) + _radial(11.5, [_ref([6, 7])])
    enc = radish.record_moment_encoding(record)

    assert enc["radial_count"] == 2
    np.testing.assert_allclose(enc["azimuth"], [10.5, 11.5])
    np.testing.assert_allclose(enc["elevation"], [0.5, 0.5])
    np.testing.assert_array_equal(enc["modified_julian_date"], [20405, 20405])

    ref = enc["moments"]["REF"]
    assert (ref["word_size"], ref["scale"], ref["offset"]) == (8, 2.0, 66.0)
    assert ref["gate_count"] == 3
    assert ref["max_gate_count"] == 3
    assert ref["radials_present"] == 2
    assert ref["uniform"] is True
    # CF attributes, precomputed so callers don't rederive them.
    assert ref["scale_factor"] == pytest.approx(0.5)
    assert ref["add_offset"] == pytest.approx(-33.0)

    assert enc["moments"]["ZDR"]["radials_present"] == 1
    assert "VEL" not in enc["moments"]


def test_inspector_flags_mixed_encodings_as_non_uniform():
    record = _radial(1.0, [_zdr8([1, 2])]) + _radial(2.0, [_zdr16([3, 4])])
    zdr = radish.record_moment_encoding(record)["moments"]["ZDR"]
    assert zdr["word_size"] == 8, "first-seen encoding wins"
    assert zdr["uniform"] is False, "a later radial switched to 16-bit"


def test_sweep_path_refusals_raise_moment_encoding_error():
    """Error translation must work on the sweep entry point too, not
    just the record one."""
    span = _ldm(_radial(1.0, [_ref([1, 2, 3])]))
    with pytest.raises(radish.MomentEncodingError, match="3 gates"):
        radish.decode_sweep_moment(span, "REF", (1, 2), np.uint8)
    with pytest.raises(radish.MomentEncodingError, match="MSG_31 radials"):
        radish.decode_sweep_moment(span, "REF", (0, 3), np.uint8)


def test_implausible_out_shape_raises_instead_of_aborting():
    """A shape that fits usize but not RAM must come back as an
    exception — an allocator abort cannot be turned into one, and would
    take a long-lived dask/zarr worker down with it."""
    for shape in [(2**40, 1000), (1, 2**20)]:
        with pytest.raises(radish.MomentEncodingError):
            radish.decode_record_moment(b"", "REF", shape, np.uint8)
    # A realistically large sweep still works.
    assert radish.decode_record_moment(b"", "REF", (8000, 1840), np.uint8).shape == (
        8000,
        1840,
    )


def test_empty_record_inventory_is_empty_not_an_error():
    enc = radish.record_moment_encoding(b"")
    assert enc["radial_count"] == 0
    assert enc["moments"] == {}
    assert len(enc["azimuth"]) == 0


def test_concurrent_decodes_agree_with_serial_ones():
    """The decode runs with the GIL released; concurrent callers must
    still get identical results."""
    from concurrent.futures import ThreadPoolExecutor

    span = _ldm(
        b"".join(_radial(float(i), [_ref([i, i + 1]), _zdr16([i, i + 2])]) for i in range(20))
    )
    work = [("REF", np.uint8), ("ZDR", np.uint16)] * 8
    serial = [radish.decode_sweep_moment(span, m, (20, 2), d) for m, d in work]
    with ThreadPoolExecutor(max_workers=8) as pool:
        concurrent = list(
            pool.map(lambda a: radish.decode_sweep_moment(span, a[0], (20, 2), a[1]), work)
        )
    for want, got in zip(serial, concurrent):
        np.testing.assert_array_equal(want, got)


def test_sweep_moment_encoding_merges_across_records():
    span = _ldm(_radial(1.0, [_ref([1, 2, 3])])) + _ldm(_radial(2.0, [_ref([4])]))
    enc = radish.sweep_moment_encoding(span)
    assert enc["radial_count"] == 2
    np.testing.assert_allclose(enc["azimuth"], [1.0, 2.0])
    ref = enc["moments"]["REF"]
    assert (ref["gate_count"], ref["max_gate_count"]) == (3, 3)
    assert ref["radials_present"] == 2


def test_inspector_output_sizes_an_array_end_to_end():
    """The documented workflow: inspect, allocate, decode."""
    span = _ldm(_radial(1.0, [_ref([1, 2, 3])]) + _radial(2.0, [_ref([4, 5, 6])]))
    enc = radish.sweep_moment_encoding(span)
    ref = enc["moments"]["REF"]
    dtype = np.uint8 if ref["word_size"] == 8 else np.uint16
    out = radish.decode_sweep_moment(
        span, "REF", (enc["radial_count"], ref["max_gate_count"]), dtype
    )
    assert out.shape == (2, 3)
    physical = out * ref["scale_factor"] + ref["add_offset"]
    np.testing.assert_allclose(physical, [[-32.5, -32.0, -31.5], [-31.0, -30.5, -30.0]])


# ────────────────────────────────────────────────────────────────────
# Real-file parity against xradar
# ────────────────────────────────────────────────────────────────────

NEXRAD_TO_ODIM = {
    "REF": "DBZH",
    "VEL": "VRADH",
    "SW": "WRADH",
    "ZDR": "ZDR",
    "PHI": "PHIDP",
    "RHO": "RHOHV",
    "CFP": "CCORH",
}


def _sweep0(span):
    """Rows of the first elevation cut: the leading contiguous run of
    radials sharing the first radial's `elevation_number`."""
    enc = radish.sweep_moment_encoding(span)
    elnum = enc["elevation_number"]
    changed = np.flatnonzero(elnum != elnum[0])
    return enc, int(changed[0]) if changed.size else len(elnum)


def _xradar_raw(var, dtype):
    """Re-encode an xradar DataArray back to the raw words it decoded
    from, using its own CF attributes.

    Asserts the values are NaN-free first: `np.round(nan).astype(uint8)`
    is an undefined float->int cast that happens to yield 0 on x86 —
    which is also radish's below-threshold raw value. Without this guard
    the strongest test in the suite could pass for the wrong reason on
    exactly the gates that matter, and flip on another architecture.
    """
    finite = np.isfinite(var.values)
    assert finite.all(), (
        f"{var.name} has {(~finite).sum()} non-finite values; the raw re-encode below "
        "would go through an undefined float->int cast"
    )
    scale_factor = var.encoding.get("scale_factor", var.attrs.get("scale_factor"))
    add_offset = var.encoding.get("add_offset", var.attrs.get("add_offset"))
    return np.round((var.values - add_offset) / scale_factor).astype(dtype)


def _shared_rays(mine_az, their_az):
    """Boolean mask over `mine_az` selecting the rays xradar also has.

    Both decoders read the same float32 azimuth off the same bytes, so
    the values are bit-identical — but they must be compared in one
    dtype, since rounding a float32 and a float64 to the same number of
    decimals does not generally give the same number.
    """
    mine = np.asarray(mine_az, dtype=np.float64)
    theirs = np.asarray(their_az, dtype=np.float64)
    mask = np.isin(mine, theirs)
    # Must be exact: the caller compares `mine[mask]` against xradar's
    # full array, so anything less than a complete match fails on shape
    # anyway — assert it here where the message is actually useful.
    assert mask.sum() == theirs.size, (
        f"azimuth matching failed: {mask.sum()} of {mine.size} radish rays matched "
        f"xradar's {theirs.size}"
    )
    return mask


@pytest.fixture
def kvnx_fixtures():
    """The two KVNX volumes straddling the 2020-06-02 RDA upgrade.

    Resolved from `RADISH_NEXRAD_KVNX_DIR`, falling back to
    `RADISH_NEXRAD_FIXTURE_DIR`. See `radish/tests/fixtures/CORPUS.md`.
    """
    for var in ("RADISH_NEXRAD_KVNX_DIR", "RADISH_NEXRAD_FIXTURE_DIR"):
        raw = os.environ.get(var)
        if not raw:
            continue
        directory = Path(raw).expanduser()
        era8 = directory / "KVNX20200602_123502_V06"
        era16 = directory / "KVNX20200602_201830_V06"
        if era8.is_file() and era16.is_file():
            return era8, era16
    pytest.skip(
        "KVNX cross-era fixtures not found — set RADISH_NEXRAD_KVNX_DIR; "
        "see radish/tests/fixtures/CORPUS.md"
    )


def test_sweep0_is_bit_identical_to_xradar(nexrad_fixture):
    """The real gate: every moment of the first cut must match xradar's
    raw words exactly, for the rays both decoders agree exist."""
    xradar = pytest.importorskip("xradar")

    span = Path(nexrad_fixture).read_bytes()
    enc, nrays = _sweep0(span)
    order = np.argsort(enc["azimuth"][:nrays], kind="stable")
    sweep0 = xradar.io.open_nexradlevel2_datatree(nexrad_fixture)["sweep_0"].ds

    # Ray alignment is a property of the sweep, not of any one moment —
    # compute it once so it can't read as if it varied per moment. The
    # two decoders can disagree on ray *count* (see
    # test_radish_keeps_the_radial_xradar_drops), so compare the rays
    # they both produced rather than assuming aligned indices.
    shared = _shared_rays(enc["azimuth"][:nrays][order], sweep0["azimuth"].values)

    compared = 0
    for name, moment in enc["moments"].items():
        odim = NEXRAD_TO_ODIM[name]
        if odim not in sweep0:
            continue  # moment isn't in this cut (surveillance sweeps lack VEL/SW)
        var = sweep0[odim]
        dtype = np.uint8 if moment["word_size"] == 8 else np.uint16
        mine = radish.decode_sweep_moment(span, name, (enc["radial_count"], var.shape[1]), dtype)[
            :nrays
        ][order]
        np.testing.assert_array_equal(
            mine[shared],
            _xradar_raw(var, dtype),
            err_msg=f"{name} -> {odim} diverged from xradar",
        )
        compared += 1

    assert compared >= 4, "expected at least REF/ZDR/PHI/RHO in the first cut"


def test_radish_keeps_the_radial_xradar_drops(kvnx_fixtures):
    """Pinned divergence, radish-is-right.

    On the 8-bit-era KVNX volume xradar's first cut has 719 rays with a
    1.0° azimuth gap at ~90.75°, where the true super-res spacing is
    0.5°. radish returns all 720. Confirmed against a hand-rolled
    `bz2` + `struct` walk of the Message 31 headers, and against
    radish's own independent `read_nexrad` volume reader.
    """
    xradar = pytest.importorskip("xradar")
    era8, _ = kvnx_fixtures

    span = era8.read_bytes()
    enc, nrays = _sweep0(span)
    assert nrays == 720, "720 rays x 0.5 deg is a complete super-res cut"
    azimuth = np.sort(enc["azimuth"][:nrays])
    assert np.diff(azimuth).max() < 0.55, "radish's rays are evenly spaced"

    theirs = np.sort(xradar.io.open_nexradlevel2_datatree(era8)["sweep_0"].ds["azimuth"].values)
    assert theirs.size == 719
    assert np.diff(theirs).max() > 0.95, "xradar leaves a one-ray hole"


def test_cross_era_zdr_decodes_onto_one_grid(kvnx_fixtures):
    """The issue's headline case: two volumes either side of the
    2020-06-02 RDA upgrade, decoded onto a single 16-bit grid, must
    agree with xradar's physical values exactly."""
    xradar = pytest.importorskip("xradar")
    era8, era16 = kvnx_fixtures

    # The eras really do differ, or this test proves nothing.
    encodings = {}
    for path in (era8, era16):
        zdr = radish.sweep_moment_encoding(path.read_bytes())["moments"]["ZDR"]
        encodings[path.name] = (zdr["word_size"], zdr["scale"], zdr["offset"])
    assert encodings[era8.name] == (8, 16.0, 128.0)
    assert encodings[era16.name] == (16, 32.0, 418.0)

    # Both remap onto the 16-bit grid with zero physical error.
    for path in (era8, era16):
        span = path.read_bytes()
        enc, nrays = _sweep0(span)
        order = np.argsort(enc["azimuth"][:nrays], kind="stable")
        sweep0 = xradar.io.open_nexradlevel2_datatree(path)["sweep_0"].ds

        mine = radish.decode_sweep_moment(
            span,
            "ZDR",
            (enc["radial_count"], sweep0["ZDR"].shape[1]),
            np.uint16,
            scale=32.0,
            offset=418.0,
        )[:nrays][order]
        physical = mine.astype(np.float64) / 32.0 - 418.0 / 32.0

        shared = _shared_rays(enc["azimuth"][:nrays][order], sweep0["azimuth"].values)
        # atol=0: the remap is exact, so every gate — including the
        # padded tail, which goes through the same remap — must land on
        # xradar's physical value bit for bit.
        np.testing.assert_allclose(
            physical[shared],
            sweep0["ZDR"].values,
            atol=0.0,
            rtol=0.0,
            err_msg=f"{path.name} ZDR diverged after the cross-era remap",
        )
