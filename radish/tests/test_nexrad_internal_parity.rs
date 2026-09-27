//! Phase 6 parity tests: decode the corpus through both radish's
//! internal decoder and `danielway/nexrad`, then compare per-radial
//! structural counts.
//!
//! All tests in this file are `#[ignore]` so the default
//! `cargo test` doesn't pay for the bzip2 + parse round-trip.
//! Gated on `RADISH_NEXRAD_FIXTURE_DIR` — see
//! `radish/tests/fixtures/CORPUS.md`.
//!
//! Three complementary asserts:
//!
//! * **`klot_structural_parity_with_danielway_nexrad`** — pins
//!   per-sweep ray-count equality on the happy-path file
//!   (KLOT VCP-32 / 12). On a clean fixture both decoders agree.
//!
//! * **`kvnx_structural_parity_with_danielway_nexrad`** — the same
//!   for the two cross-RDA-build KVNX volumes, and additionally
//!   cross-checks the per-moment demux entry point's radial count
//!   against danielway's.
//!
//! * **`kilx_structural_parity_with_danielway_nexrad`** — pins
//!   per-sweep ray-count equality on `KILX20230629_154426_V06`,
//!   which is the file flagged as "phantom-radial" upstream. After
//!   ICD §3.2.4.17 field-by-field analysis (monotonic timestamps,
//!   sequential azimuth_numbers, valid `radial_status=1`,
//!   `spot_blank=0`), the two "extra" radials in sweep_10 are
//!   genuinely on-wire and our decoder + danielway both correctly
//!   read all 360. xradar's 358 comes from a stride bug in its
//!   parser (`(recnum - 134) // 120` hard-codes 120 messages per
//!   LDM record, but LDM 49 of this fixture has 122 = 120 MSG_31 +
//!   2 MSG_2). Both our decoder and danielway produce the on-wire
//!   truth; xradar is the off-by-2 outlier.

#![cfg(test)]
// `nexrad` is a [dev-dependencies]-only side-by-side reference (see
// `CLAUDE.md`): radish's production read path is its own in-tree
// decoder, so importing danielway's types here is a genuine second
// implementation, not the same code twice.

use std::path::PathBuf;

fn corpus_dir() -> Option<PathBuf> {
    std::env::var_os("RADISH_NEXRAD_FIXTURE_DIR").map(PathBuf::from)
}

fn klot_path() -> Option<PathBuf> {
    let p = corpus_dir()?.join("KLOT20251210_102338_V06");
    p.is_file().then_some(p)
}

fn kilx_path() -> Option<PathBuf> {
    let p = corpus_dir()?.join("KILX20230629_154426_V06");
    p.is_file().then_some(p)
}

fn kvnx_paths() -> Option<(PathBuf, PathBuf)> {
    let dir = corpus_dir()?;
    let era8 = dir.join("KVNX20200602_123502_V06");
    let era16 = dir.join("KVNX20200602_201830_V06");
    (era8.is_file() && era16.is_file()).then_some((era8, era16))
}

/// Decode the file via `danielway/nexrad`. Returns sweep + radial
/// counts so the parity check can compare against radish without
/// crossing the `pub(crate)` boundary into our `decode` module.
fn danielway_summary(path: &std::path::Path) -> (usize, Vec<usize>) {
    let bytes = std::fs::read(path).expect("read fixture");
    let file = nexrad::data::volume::File::new(bytes)
        .decompress()
        .expect("decompress");
    let scan = file.scan().expect("scan");
    let sweeps = scan.sweeps();
    let per_sweep_ray_counts = sweeps.iter().map(|s| s.radials().len()).collect();
    (sweeps.len(), per_sweep_ray_counts)
}

/// Parity: every-radial structural equality on KLOT (clean
/// fixture, no skipped azimuth_numbers). On a clean file both
/// decoders must agree on:
///
/// * number of sweeps
/// * per-sweep radial count
/// * total radial count
///
/// The deeper gate-by-gate parity lives in the inline
/// `cargo test --lib` integration tests
/// (`backends::nexrad::decode::integration_test`). That layer is
/// where we exercise our internal decoder's typed types directly;
/// here we just confirm the structural shape matches.
#[test]
#[ignore = "needs RADISH_NEXRAD_FIXTURE_DIR + danielway/nexrad parity dep"]
fn klot_structural_parity_with_danielway_nexrad() {
    let Some(path) = klot_path() else {
        eprintln!("skipping: KLOT fixture not found in RADISH_NEXRAD_FIXTURE_DIR");
        return;
    };

    let (their_sweep_count, their_rays_per_sweep) = danielway_summary(&path);
    let their_total: usize = their_rays_per_sweep.iter().sum();

    // Our decoder is reachable via the runtime path (NexradBackend),
    // which since Phase 7 runs radish's own in-tree `decode_volume`.
    use radish::backends::{NexradBackend, RadarBackend};
    let backend = NexradBackend::new();
    let our = backend.read_volume(&path).expect("our read_volume");
    let our_sweep_count = our.num_sweeps();
    let our_rays_per_sweep: Vec<usize> = our
        .sweeps
        .iter()
        .map(|s| s.coordinates.azimuth.len())
        .collect();
    let our_total: usize = our_rays_per_sweep.iter().sum();

    eprintln!(
        "KLOT parity: ours={} sweeps ({} rays), danielway={} sweeps ({} rays)",
        our_sweep_count, our_total, their_sweep_count, their_total
    );

    // Phase 7 of plan 0003 swapped `read_volume` onto radish's own
    // in-tree decoder (see `CLAUDE.md`) and there are no upstream
    // `nexrad` *runtime* dependencies left, so this is a genuine
    // two-implementation parity gate: our byte walker against
    // danielway's.
    assert_eq!(our_sweep_count, their_sweep_count);
    assert_eq!(our_rays_per_sweep, their_rays_per_sweep);
}

/// **KILX structural parity.** Pins both decoders to the same
/// on-wire ground truth: 6840 MSG_31 records total, 360 in
/// sweep_10 (full 1° azimuth circle). ICD §3.2.4.17 field
/// analysis confirms every radial in elevation 11 is valid
/// (monotonic collection-time, sequential azimuth_number,
/// `radial_status=1`, `spot_blank=0`).
///
/// Phase 7 of plan 0003 landed, so `radish::read_volume` runs our
/// own `decode_volume`: this is a real per-sweep-count parity gate
/// between two independent implementations, not a tautology.
///
/// Note: xradar reports 6838 / 358 on this fixture due to a
/// stride bug in its byte walker
/// (`xradar/io/backends/nexrad_level2.py:397`), where the formula
/// `(recnum - 134) // 120` hard-codes 120 messages per LDM
/// record. LDM record 49 of this fixture contains 122 messages
/// (120 MSG_31 + 2 MSG_2), so xradar drops 2 valid MSG_31s at
/// the LDM boundary. Both danielway and our decoder correctly
/// walk all 122. Confirmed on the wire: `azimuth_number` runs
/// 1..360 contiguously in elevation 11, with 119 and 120 present as
/// messages 120 and 121 of LDM record 49. Filed upstream as
/// openradar/xradar#376, fix in openradar/xradar#377 (open at the
/// time of writing; not in 0.12.0, not on their `main`).
#[test]
#[ignore = "needs RADISH_NEXRAD_FIXTURE_DIR"]
fn kilx_structural_parity_with_danielway_nexrad() {
    let Some(path) = kilx_path() else {
        eprintln!("skipping: KILX fixture not found in RADISH_NEXRAD_FIXTURE_DIR");
        return;
    };

    let (their_sweep_count, their_rays_per_sweep) = danielway_summary(&path);
    let their_total: usize = their_rays_per_sweep.iter().sum();

    eprintln!(
        "KILX danielway: {} sweeps, {} total rays, sweep_10 has {} rays",
        their_sweep_count,
        their_total,
        their_rays_per_sweep.get(10).copied().unwrap_or(0)
    );

    // Pin the ground-truth on-wire counts. If danielway ever
    // changes (or our decoder regresses) this assertion catches
    // it.
    assert_eq!(their_total, 6840, "expected 6840 on-wire MSG_31 records");
    assert_eq!(
        their_rays_per_sweep[10], 360,
        "expected 360 rays in sweep_10 (full 1° circle)"
    );

    use radish::backends::{NexradBackend, RadarBackend};
    let backend = NexradBackend::new();
    let our = backend.read_volume(&path).expect("our read_volume");
    let our_sweep_count = our.num_sweeps();
    let our_rays_per_sweep: Vec<usize> = our
        .sweeps
        .iter()
        .map(|s| s.coordinates.azimuth.len())
        .collect();

    assert_eq!(our_sweep_count, their_sweep_count);
    assert_eq!(our_rays_per_sweep, their_rays_per_sweep);
}

/// **KVNX cross-era third-party parity.**
///
/// The two `KVNX20200602_*` volumes straddle the 2020-06-02 RDA
/// upgrade and are the corpus's regression gate for the per-moment
/// decoders' remap logic (issue #32). This test pins them against
/// `danielway/nexrad` so the ray-count claims radish documents rest on
/// a second independent implementation rather than on radish alone.
///
/// It also cross-checks the **demux** entry point specifically:
/// `sweep_moment_encoding`'s `radial_count` walks the same bytes
/// through a different code path than `read_volume`, so agreeing with
/// danielway on the total is a real signal, not a restatement.
///
/// Context for the 8-bit-era assertion: xradar reports 719 rays in the
/// first cut of `KVNX20200602_123502_V06` where the wire carries 720.
/// Same root cause as the KILX case above — `NEXRADRecordFile.init_record`
/// hard-codes a 120-message LDM stride, so trailing MSG_31s are dropped
/// from any record that also carries MSG_2. Filed upstream as
/// openradar/xradar#376 with a fix in openradar/xradar#377 (open at the
/// time of writing; not in xradar 0.12.0 and not on their `main`).
#[test]
#[ignore = "needs RADISH_NEXRAD_FIXTURE_DIR + danielway/nexrad parity dep"]
fn kvnx_structural_parity_with_danielway_nexrad() {
    let Some((era8, era16)) = kvnx_paths() else {
        eprintln!("skipping: KVNX fixtures not found in RADISH_NEXRAD_FIXTURE_DIR");
        return;
    };

    use radish::backends::nexrad::demux::sweep_moment_encoding;
    use radish::backends::{NexradBackend, RadarBackend};

    for (label, path, expected_first_cut) in [
        ("8-bit era", era8, 720usize),
        ("16-bit era", era16, 720usize),
    ] {
        let (their_sweep_count, their_rays_per_sweep) = danielway_summary(&path);
        let their_total: usize = their_rays_per_sweep.iter().sum();

        let our = NexradBackend::new()
            .read_volume(&path)
            .expect("our read_volume");
        let our_rays_per_sweep: Vec<usize> = our
            .sweeps
            .iter()
            .map(|s| s.coordinates.azimuth.len())
            .collect();

        // Third code path: the demux inventory, walking the raw span.
        let bytes = std::fs::read(&path).expect("read fixture");
        let inventory = sweep_moment_encoding(&bytes).expect("demux inventory");

        eprintln!(
            "KVNX {label}: danielway={} sweeps / {} rays, ours={} / {}, demux={} rays",
            their_sweep_count,
            their_total,
            our.num_sweeps(),
            our_rays_per_sweep.iter().sum::<usize>(),
            inventory.radial_count,
        );

        assert_eq!(our.num_sweeps(), their_sweep_count, "{label} sweep count");
        assert_eq!(
            our_rays_per_sweep, their_rays_per_sweep,
            "{label} per-sweep"
        );
        assert_eq!(
            inventory.radial_count, their_total,
            "{label}: the demux path must see the same radials as danielway"
        );
        assert_eq!(
            their_rays_per_sweep[0], expected_first_cut,
            "{label}: first cut is a complete circle on the wire (xradar reports 719 \
             on the 8-bit era — see openradar/xradar#377)"
        );
    }
}
