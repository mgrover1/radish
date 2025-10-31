/// Example: Reading CfRadial1 data with Rust

use radish::backends::{RadarBackend, CfRadial1Backend};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Path to your CfRadial1 file
    let file_path = "path/to/cfrad.nc";
    let path = Path::new(file_path);

    // Create backend
    let backend = CfRadial1Backend::new();

    // Option 1: Quick scan for metadata only
    println!("Scanning file for metadata...");
    let metadata = backend.scan_file(path)?;
    println!("Instrument: {}", metadata.instrument_name);
    println!("Location: {:.4}°N, {:.4}°E, {:.1}m",
             metadata.latitude, metadata.longitude, metadata.altitude);
    println!("Number of sweeps: {}", metadata.sweep_group_names.len());
    println!("Fixed angles: {:?}", metadata.sweep_fixed_angles);

    // Option 2: Read entire volume
    println!("\nReading full volume...");
    let volume = backend.read_volume(path)?;

    println!("Volume has {} sweeps", volume.num_sweeps());

    // Iterate through sweeps
    for (i, sweep) in volume.sweeps.iter().enumerate() {
        println!("\nSweep {}: {:.2}° elevation", i, sweep.metadata.fixed_angle);
        println!("  Rays: {}, Gates: {}", sweep.num_rays(), sweep.num_gates());
        println!("  Available moments: {:?}", sweep.moment_names());

        // Access specific moment
        if let Some(dbz) = sweep.get_moment("DBZ").or_else(|| sweep.get_moment("DBZH")) {
            let (nrays, ngates) = dbz.shape();
            println!("  Reflectivity shape: {} × {}", nrays, ngates);
            println!("  Units: {}", dbz.units);

            // Access data
            let data = &dbz.data;
            let max_val = data.iter()
                .filter(|v| !v.is_nan())
                .fold(f32::NEG_INFINITY, |a, &b| a.max(b));
            println!("  Max reflectivity: {:.2} dBZ", max_val);
        }
    }

    // Option 3: Read single sweep
    println!("\nReading just first sweep...");
    let sweep_0 = backend.read_sweep(path, 0)?;
    println!("Sweep 0: {} rays, {} moments",
             sweep_0.num_rays(), sweep_0.moments.len());

    Ok(())
}
