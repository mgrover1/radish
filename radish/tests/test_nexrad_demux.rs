//! Real-file integration tests for the per-moment demultiplexer
//! (`radish::backends::nexrad::demux`, issue #32).
//!
//! Skipped unless `RADISH_NEXRAD_FIXTURE_DIR` is set; see
//! `radish/tests/fixtures/CORPUS.md`. The Python suite
//! (`python/tests/test_nexrad_demux.py`) owns the xradar parity gate —
//! these tests cover the Rust-level API and the invariants that hold on
//! any real volume.

use std::path::PathBuf;

use radish::backends::nexrad::demux::{
    decode_sweep_moment, sweep_moment_encoding, DemuxOptions, MomentSelector, OutputWord,
    RawMoment, TargetEncoding,
};

fn fixture(name: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("RADISH_NEXRAD_FIXTURE_DIR")?;
    let candidate = PathBuf::from(dir).join(name);
    candidate.is_file().then_some(candidate)
}

fn klot() -> Option<Vec<u8>> {
    std::fs::read(fixture("KLOT20251210_102338_V06")?).ok()
}

fn options(moment: MomentSelector, rays: usize, gates: usize, word: OutputWord) -> DemuxOptions {
    DemuxOptions::new(moment, (rays, gates), word)
}

fn word_for(word_size: u8) -> OutputWord {
    if word_size == 8 {
        OutputWord::U8
    } else {
        OutputWord::U16
    }
}

/// The documented workflow: inspect, allocate, decode. Every moment the
/// inventory reports must decode into an array of exactly the size the
/// inventory implied.
#[test]
fn inventory_sizes_every_moment_on_a_real_volume() {
    let Some(bytes) = klot() else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE_DIR unset or KLOT fixture missing");
        return;
    };

    let inventory = sweep_moment_encoding(&bytes).expect("inventory");
    assert!(
        inventory.radial_count > 5_000,
        "a full VCP should hold thousands of radials, got {}",
        inventory.radial_count
    );
    assert_eq!(inventory.azimuth.len(), inventory.radial_count);
    assert_eq!(inventory.elevation.len(), inventory.radial_count);
    assert_eq!(inventory.elevation_number.len(), inventory.radial_count);

    // Every moment radish's volume reader surfaces must be demuxable.
    for selector in MomentSelector::ALL {
        let Some(encoding) = inventory.moments.get(&selector) else {
            continue;
        };
        assert!(
            encoding.uniform,
            "{} mixes encodings within one volume — the parity tests assume otherwise",
            selector.name()
        );
        let gates = usize::from(encoding.max_gate_count);
        let decoded = decode_sweep_moment(
            &bytes,
            &options(
                selector,
                inventory.radial_count,
                gates,
                word_for(encoding.word_size),
            ),
            false,
        )
        .unwrap_or_else(|e| panic!("{} failed to decode: {e}", selector.name()));
        assert_eq!(decoded.len(), inventory.radial_count * gates);
    }
    assert!(
        inventory.moments.contains_key(&MomentSelector::Ref),
        "every NEXRAD volume carries reflectivity"
    );
}

/// Sorting must be a pure permutation of the unsorted rows — same
/// multiset of values, reordered exactly as `azimuth`'s stable argsort
/// says. This is the invariant callers rely on when they reorder their
/// own coordinate arrays to match.
#[test]
fn azimuth_sort_is_the_stable_argsort_permutation() {
    let Some(bytes) = klot() else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE_DIR unset or KLOT fixture missing");
        return;
    };

    let inventory = sweep_moment_encoding(&bytes).expect("inventory");
    let encoding = inventory.moments[&MomentSelector::Ref];
    let gates = usize::from(encoding.max_gate_count);
    let opts = options(
        MomentSelector::Ref,
        inventory.radial_count,
        gates,
        OutputWord::U8,
    );

    let unsorted = decode_sweep_moment(&bytes, &opts, false).expect("unsorted");
    let sorted = decode_sweep_moment(&bytes, &opts, true).expect("sorted");

    let (RawMoment::U8(unsorted), RawMoment::U8(sorted)) = (&unsorted, &sorted) else {
        panic!("expected uint8 reflectivity");
    };

    let mut order: Vec<usize> = (0..inventory.radial_count).collect();
    order.sort_by(|a, b| inventory.azimuth[*a].total_cmp(&inventory.azimuth[*b]));
    for (destination, &source) in order.iter().enumerate() {
        assert_eq!(
            &sorted[destination * gates..(destination + 1) * gates],
            &unsorted[source * gates..(source + 1) * gates],
            "row {destination} should be unsorted row {source}"
        );
    }
}

/// A gate dimension one short of what the data needs must be refused,
/// not silently truncated — the failure mode that would quietly corrupt
/// a chunked store.
#[test]
fn undersized_output_is_refused_on_a_real_volume() {
    let Some(bytes) = klot() else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE_DIR unset or KLOT fixture missing");
        return;
    };

    let inventory = sweep_moment_encoding(&bytes).expect("inventory");
    let encoding = inventory.moments[&MomentSelector::Ref];
    let opts = options(
        MomentSelector::Ref,
        inventory.radial_count,
        usize::from(encoding.max_gate_count) - 1,
        OutputWord::U8,
    );
    assert!(
        decode_sweep_moment(&bytes, &opts, false).is_err(),
        "a too-narrow gate dimension must error"
    );
}

/// The two KVNX volumes straddling the 2020-06-02 RDA upgrade encode
/// ZDR differently on the wire, yet must produce identical physical
/// values once remapped onto one common grid.
#[test]
fn kvnx_cross_era_zdr_lands_on_one_grid() {
    let (Some(era8), Some(era16)) = (
        fixture("KVNX20200602_123502_V06"),
        fixture("KVNX20200602_201830_V06"),
    ) else {
        eprintln!("skipping: KVNX cross-era fixtures missing — see CORPUS.md");
        return;
    };

    // The 16-bit grid both eras get remapped onto.
    let target = TargetEncoding::new(32.0, 418.0);
    let expected_source = [(era8, 8u8, 16.0f32, 128.0f32), (era16, 16, 32.0, 418.0)];

    for (path, word_size, scale, offset) in expected_source {
        let bytes = std::fs::read(&path).expect("read fixture");
        let inventory = sweep_moment_encoding(&bytes).expect("inventory");
        let encoding = inventory.moments[&MomentSelector::Zdr];
        assert_eq!(
            (encoding.word_size, encoding.scale, encoding.offset),
            (word_size, scale, offset),
            "{path:?} ZDR wire encoding changed — the cross-era test is no longer testing anything"
        );

        let gates = usize::from(encoding.max_gate_count);
        let mut opts = options(
            MomentSelector::Zdr,
            inventory.radial_count,
            gates,
            OutputWord::U16,
        );
        opts.target = Some(target);
        // ZDR is absent from the surveillance cuts, so most rows of a
        // whole-volume decode are `fill_value`. Pick one the remap can
        // never produce (the widest ZDR value is 0x7FF = 2047 native,
        // or 255 * 2 + 162 = 672 remapped) so fill rows are
        // unambiguous. This is the pattern callers should use whenever
        // "absent" has to be distinguishable from real data.
        opts.fill_value = u16::MAX;
        let decoded = decode_sweep_moment(&bytes, &opts, false).expect("remapped decode");

        let RawMoment::U16(values) = &decoded else {
            panic!("expected uint16 output");
        };

        // For every radial that *does* carry ZDR, no gate — data or
        // padding — may fall below the source's physical floor. Padding
        // goes through the same remap, so the array stays physically
        // self-consistent end to end.
        let floor = -f64::from(offset) / f64::from(scale);
        let mut checked = 0usize;
        for row in values.chunks_exact(gates) {
            if row[0] == u16::MAX {
                continue; // whole row is fill: this radial had no ZDR
            }
            for &raw in row {
                assert_ne!(raw, u16::MAX, "{path:?}: fill leaked into a decoded row");
                let physical =
                    (f64::from(raw) - f64::from(target.offset)) / f64::from(target.scale);
                assert!(
                    physical >= floor - 1e-9,
                    "{path:?}: {physical} fell below the source floor {floor}"
                );
            }
            checked += 1;
        }
        assert_eq!(
            checked, encoding.radials_present,
            "{path:?}: rows carrying ZDR should match the inventory's radials_present"
        );
    }
}
