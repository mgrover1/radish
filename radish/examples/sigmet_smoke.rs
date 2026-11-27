use radish::backends::{RadarBackend, SigmetBackend};
use std::path::Path;

fn main() {
    // Path to an IRIS RAW file: first CLI argument, else `RADISH_SIGMET_FIXTURE`.
    let Some(path) = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("RADISH_SIGMET_FIXTURE").ok())
    else {
        eprintln!("usage: sigmet_smoke <IRIS RAW file>  (or set RADISH_SIGMET_FIXTURE)");
        std::process::exit(2);
    };
    let path = Path::new(&path);
    let backend = SigmetBackend::new();
    println!("can_read: {}", backend.can_read(path));
    let result = backend.read_volume(path);
    match result {
        Ok(v) => {
            println!("OK:");
            println!("  instrument: {}", v.metadata.instrument_name);
            println!(
                "  lat/lon:    {:.4}, {:.4}",
                v.metadata.latitude, v.metadata.longitude
            );
            println!("  num sweeps: {}", v.num_sweeps());
            for (i, s) in v.sweeps.iter().enumerate().take(3) {
                println!(
                    "  sweep {}: {} rays, {} gates, {} moments, fixed_angle={:.2}",
                    i,
                    s.coordinates.azimuth.len(),
                    s.coordinates.range.len(),
                    s.moments.len(),
                    s.metadata.fixed_angle
                );
                let names: Vec<&str> = s.moments.keys().map(|s| s.as_str()).collect();
                println!("        moments: {:?}", names);
            }
        }
        Err(e) => println!("ERR: {e:?}"),
    }
}
