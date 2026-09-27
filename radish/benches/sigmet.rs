//! Criterion benchmarks for the Sigmet/IRIS RAW backend.
//!
//! Skipped at run time unless `RADISH_SIGMET_FIXTURE` points at a valid
//! IRIS RAW file. Run with:
//!
//! ```sh
//! RADISH_SIGMET_FIXTURE=/path/to/SIGMET.RAW cargo bench --bench sigmet
//! ```

use std::path::PathBuf;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use radish::backends::{RadarBackend, SigmetBackend};

fn fixture() -> Option<PathBuf> {
    std::env::var_os("RADISH_SIGMET_FIXTURE").map(Into::into)
}

fn bench_sigmet(c: &mut Criterion) {
    let Some(path) = fixture() else {
        eprintln!(
            "RADISH_SIGMET_FIXTURE not set — skipping Sigmet benches. \
             Point it at an IRIS RAW file and re-run."
        );
        return;
    };
    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    let backend = SigmetBackend::new();

    let mut group = c.benchmark_group("sigmet");
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

    group.bench_with_input(
        BenchmarkId::new("read_sweep[0]", &display),
        &path,
        |b, p| {
            b.iter(|| backend.read_sweep(p, 0).expect("read_sweep failed"));
        },
    );

    // `read_bytes_volume` covers the in-memory decode path (S3 / HTTP body)
    // — should match `read_volume` modulo the std::fs::read prelude that
    // Sigmet's path code already uses.
    let buf = std::fs::read(&path).expect("read fixture bytes");
    group.bench_with_input(
        BenchmarkId::new("read_bytes_volume", &display),
        &buf,
        |b, data| {
            b.iter(|| {
                backend
                    .read_bytes_volume(data.clone())
                    .expect("read_bytes_volume failed")
            });
        },
    );

    group.finish();
}

criterion_group!(benches, bench_sigmet);
criterion_main!(benches);
