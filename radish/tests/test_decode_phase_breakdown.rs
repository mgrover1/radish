//! Phase-by-phase breakdown of `decode_volume` to identify
//! where the gap to `danielway/nexrad` lives. Skipped unless
//! `RADISH_NEXRAD_FIXTURE` points at an Archive II file.
//!
//! ```sh
//! RADISH_NEXRAD_FIXTURE=$HOME/.cache/radish/fixtures/nexrad/KLOT20251210_102338_V06 \
//!   cargo test --release --package radish --test test_decode_phase_breakdown \
//!   -- --ignored --nocapture
//! ```

use std::path::PathBuf;

#[test]
#[ignore = "needs RADISH_NEXRAD_FIXTURE; one-shot profiling aid"]
fn phase_breakdown_decode_volume() {
    let Some(path) = std::env::var_os("RADISH_NEXRAD_FIXTURE").map(PathBuf::from) else {
        eprintln!("skipping: RADISH_NEXRAD_FIXTURE not set");
        return;
    };
    let bytes = std::fs::read(&path).expect("read fixture");
    let size_mb = bytes.len() as f64 / 1_000_000.0;
    eprintln!("\nFixture: {} ({size_mb:.1} MB)\n", path.display());

    // Warm up (first run includes any one-time alloc / JIT-ish noise).
    radish::backends::nexrad::bench_decode_phases(&bytes).expect("decode");
    eprintln!("\n--- run 1 ---");
    radish::backends::nexrad::bench_decode_phases(&bytes).expect("decode");
    eprintln!("\n--- run 2 ---");
    radish::backends::nexrad::bench_decode_phases(&bytes).expect("decode");
    eprintln!("\n--- run 3 ---");
    radish::backends::nexrad::bench_decode_phases(&bytes).expect("decode");
}
