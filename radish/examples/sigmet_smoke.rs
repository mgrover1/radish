use radish::backends::{RadarBackend, SigmetBackend};
use std::path::Path;

fn main() {
    let path = "/home/alfonso-ladino/python/imhpa/data/Radar Tolé/20260124/CHI260124100554.RAWE5P1";
    let backend = SigmetBackend::new();
    println!("can_read: {}", backend.can_read(Path::new(path)));
    let result = backend.read_volume(Path::new(path));
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
