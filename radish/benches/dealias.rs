//! Criterion benchmark for region-based velocity dealiasing.
//!
//! Skipped at run time unless `RADISH_NEXRAD_FIXTURE_DIR` points at the
//! NEXRAD Level 2 fixture corpus (see `radish/tests/fixtures/CORPUS.md`).
//! Run with:
//!
//! ```sh
//! RADISH_NEXRAD_FIXTURE_DIR=~/.cache/radish/fixtures/nexrad cargo bench --bench dealias
//! ```
//!
//! Benchmarks radish's own cost only — Py-ART isn't callable from a Rust
//! criterion harness. `docs/NEXRAD_LEVEL3_WASM.md` records a separate,
//! one-off `timeit`-based measurement of Py-ART's own
//! `dealias_region_based` on the same sweep for a rough side-by-side
//! comparison; treat that number as a single-machine anecdote, not a
//! tracked regression gate the way this benchmark is.

use std::path::PathBuf;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use ndarray::Array2;

use radish::backends::{NexradBackend, RadarBackend};
use radish::transforms::{dealias_region_based, DealiasOptions};

fn fixture() -> Option<PathBuf> {
    let dir = std::env::var_os("RADISH_NEXRAD_FIXTURE_DIR")?;
    let candidate = PathBuf::from(dir).join("KLOT20251210_102338_V06");
    candidate.is_file().then_some(candidate)
}

fn bench_dealias(c: &mut Criterion) {
    let Some(path) = fixture() else {
        eprintln!(
            "RADISH_NEXRAD_FIXTURE_DIR not set (or KLOT20251210_102338_V06 missing) — \
             skipping dealias benches. See radish/tests/fixtures/CORPUS.md."
        );
        return;
    };

    let volume = NexradBackend::new()
        .read_volume(&path)
        .expect("read_volume failed");
    // Sweep 1 — the same real sweep `test_dealias_parity.rs` checks for
    // bit-exactness, so this benchmark's input has already been verified
    // correct, not just fast.
    let sweep = &volume.sweeps[1];
    let velocity = &sweep.moments.get("VRADH").expect("VRADH present").data;
    let valid: Array2<bool> = velocity.mapv(|v| v.is_finite());
    let nyquist = 11.55f32;
    let n_gates = (velocity.nrows() * velocity.ncols()) as u64;

    let mut group = c.benchmark_group("dealias");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(10));
    group.throughput(Throughput::Elements(n_gates));

    let display = format!("{}x{}", velocity.nrows(), velocity.ncols());
    group.bench_with_input(
        BenchmarkId::new("dealias_region_based", &display),
        &(),
        |b, ()| {
            b.iter(|| {
                dealias_region_based(velocity, &valid, nyquist, true, DealiasOptions::default())
                    .expect("dealias_region_based failed")
            });
        },
    );

    group.finish();
}

criterion_group!(benches, bench_dealias);
criterion_main!(benches);
