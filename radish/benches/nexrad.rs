//! Criterion benchmarks for the NEXRAD Level 2 backend.
//!
//! Skipped at run time unless `RADISH_NEXRAD_FIXTURE` points at a valid
//! NEXRAD Archive II file. Run with:
//!
//! ```sh
//! RADISH_NEXRAD_FIXTURE=/path/to/KXXX...V06 cargo bench --bench nexrad
//! ```

use std::path::PathBuf;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use radish::backends::{NexradBackend, RadarBackend};

fn fixture() -> Option<PathBuf> {
    std::env::var_os("RADISH_NEXRAD_FIXTURE").map(Into::into)
}

fn bench_nexrad(c: &mut Criterion) {
    let Some(path) = fixture() else {
        eprintln!(
            "RADISH_NEXRAD_FIXTURE not set — skipping NEXRAD benches. \
             Point it at a NEXRAD Archive II file and re-run."
        );
        return;
    };
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    let backend = NexradBackend::new();

    // `read_volume` is the hot path used by `xr.open_datatree(..., engine="radish")`.
    // We report throughput in bytes so changes show up as MB/s deltas.
    let mut group = c.benchmark_group("nexrad");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));
    if bytes > 0 {
        group.throughput(Throughput::Bytes(bytes));
    }

    let display = path.display().to_string();
    group.bench_with_input(BenchmarkId::new("read_volume", &display), &path, |b, p| {
        b.iter(|| backend.read_volume(p).expect("read_volume failed"));
    });

    group.bench_with_input(BenchmarkId::new("scan_file", &display), &path, |b, p| {
        b.iter(|| backend.scan_file(p).expect("scan_file failed"));
    });

    // Read just sweep 0 — proxy for "lazy single-elevation read" workloads,
    // even though Phase 1 still decodes the full volume internally.
    group.bench_with_input(
        BenchmarkId::new("read_sweep[0]", &display),
        &path,
        |b, p| {
            b.iter(|| backend.read_sweep(p, 0).expect("read_sweep failed"));
        },
    );

    group.finish();
}

criterion_group!(benches, bench_nexrad);
criterion_main!(benches);
