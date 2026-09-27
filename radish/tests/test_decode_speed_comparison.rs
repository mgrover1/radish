//! Three-way decode-speed comparison: radish in-tree decoder vs
//! `danielway/nexrad` (Rust-vs-Rust head-to-head). Skipped unless
//! `RADISH_NEXRAD_FIXTURE` points at an Archive II file.
//!
//! Run with:
//!
//! ```sh
//! RADISH_NEXRAD_FIXTURE=$HOME/.cache/radish/fixtures/nexrad/KLOT20251210_102338_V06 \
//!   cargo test --release --package radish --test test_decode_speed_comparison \
//!   -- --ignored --nocapture
//! ```
//!
//! `nexrad` is a `[dev-dependencies]`-only parity reference (Phase
//! 7c moved it there); this test is the side-by-side Rust gate.
//! Python-level xradar comparison lives at
//! `python/examples/bench_nexrad_vs_xradar.py`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

const RUNS: usize = 7;

fn fixture() -> Option<PathBuf> {
    std::env::var_os("RADISH_NEXRAD_FIXTURE").map(PathBuf::from)
}

fn time_n<F>(label: &str, mut f: F) -> Duration
where
    F: FnMut(),
{
    let mut times: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    times.sort();
    let median = times[RUNS / 2];
    let min = times[0];
    let max = times[RUNS - 1];
    eprintln!(
        "  {label}: median={:.1}ms  min={:.1}ms  max={:.1}ms",
        median.as_secs_f64() * 1000.0,
        min.as_secs_f64() * 1000.0,
        max.as_secs_f64() * 1000.0,
    );
    median
}

#[test]
#[ignore = "needs RADISH_NEXRAD_FIXTURE; long-running benchmark"]
fn radish_in_tree_vs_danielway_nexrad_decode_only() {
    let Some(path) = fixture() else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE not set");
        return;
    };
    let bytes = std::fs::read(&path).expect("read fixture");
    let size_mb = bytes.len() as f64 / 1_000_000.0;

    eprintln!(
        "\nFixture: {} ({size_mb:.1} MB)\nRuns: {RUNS}\n",
        path.display()
    );

    use radish::backends::{nexrad::time_decode_volume, NexradBackend, RadarBackend};

    eprintln!("=== DECODE-ONLY (apples-to-apples: bytes -> Scan, no adapter) ===");
    eprintln!("radish::decode_volume:");
    let radish_decode_t = time_n("radish-decode", || {
        time_decode_volume(&bytes).expect("decode_volume");
    });

    eprintln!("\ndanielway::File::new(...).decompress().scan():");
    let danielway_t = time_n("danielway", || {
        let file = nexrad::data::volume::File::new(bytes.clone())
            .decompress()
            .expect("decompress");
        let _scan = file.scan().expect("scan");
    });

    eprintln!("\n=== END-TO-END (decode + adapter to VolumeData with Array2 moments) ===");
    eprintln!("radish::NexradBackend::read_bytes_volume:");
    let radish_full_t = time_n("radish-full", || {
        let backend = NexradBackend::new();
        let _vol = backend
            .read_bytes_volume(bytes.clone())
            .expect("read_bytes_volume");
    });

    let decode_ratio = danielway_t.as_secs_f64() / radish_decode_t.as_secs_f64();
    eprintln!(
        "\nDecode-only ratio (danielway / radish-decode): {decode_ratio:.2}× \
         (radish faster when > 1)"
    );
    eprintln!("Throughput on {size_mb:.1} MB:");
    eprintln!(
        "  radish-decode: {:.1} MB/s",
        size_mb / radish_decode_t.as_secs_f64()
    );
    eprintln!(
        "  danielway:     {:.1} MB/s",
        size_mb / danielway_t.as_secs_f64()
    );
    eprintln!(
        "  radish-full:   {:.1} MB/s   (decode + adapter)",
        size_mb / radish_full_t.as_secs_f64()
    );
}
