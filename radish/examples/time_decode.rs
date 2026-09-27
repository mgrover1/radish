//! Manual timing harness for the NEXRAD decode pipeline. Used to triangulate
//! against criterion / the Python wall-clock bench.
//!
//! Run: `RADISH_NEXRAD_FIXTURE=<path> cargo run --release -p radish --example time_decode`

use std::time::Instant;

use radish::backends::{NexradBackend, RadarBackend};

fn main() {
    let path = std::env::var("RADISH_NEXRAD_FIXTURE")
        .expect("set RADISH_NEXRAD_FIXTURE to a NEXRAD Archive II file");
    let path = std::path::Path::new(&path);

    let backend = NexradBackend::new();

    // Warm-up.
    let _ = backend.read_volume(path).unwrap();

    let runs = 5;
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        let v = backend.read_volume(path).unwrap();
        let elapsed = t.elapsed();
        times.push(elapsed);
        // Drop happens after timing — exclude drop cost from the measurement.
        std::mem::drop(v);
    }
    times.sort();
    println!("read_volume (drop excluded):");
    println!("  runs:   {:?}", times);
    println!("  median: {:?}", times[runs / 2]);

    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        let v = backend.read_volume(path).unwrap();
        std::mem::drop(v);
        times.push(t.elapsed());
    }
    times.sort();
    println!("read_volume (drop included):");
    println!("  runs:   {:?}", times);
    println!("  median: {:?}", times[runs / 2]);

    // Isolate upstream decode vs. our adapter.
    let mut t_upstream = Vec::with_capacity(runs);
    for _ in 0..runs {
        let data = std::fs::read(path).unwrap();
        let t = Instant::now();
        let scan = nexrad::data::volume::File::new(data)
            .decompress()
            .unwrap()
            .scan()
            .unwrap();
        t_upstream.push(t.elapsed());
        std::mem::drop(scan);
    }
    t_upstream.sort();
    println!("upstream decode (read+decompress+scan, no adapter):");
    println!("  runs:   {:?}", t_upstream);
    println!("  median: {:?}", t_upstream[runs / 2]);

    // Apples-to-apples with the Phase 0 spike (which timed `nexrad::load_file`).
    let mut t_load_file = Vec::with_capacity(runs);
    for _ in 0..runs {
        let t = Instant::now();
        let scan = nexrad::load_file(path).unwrap();
        t_load_file.push(t.elapsed());
        std::mem::drop(scan);
    }
    t_load_file.sort();
    println!("nexrad::load_file (matches the Phase 0 spike harness):");
    println!("  runs:   {:?}", t_load_file);
    println!("  median: {:?}", t_load_file[runs / 2]);
}
