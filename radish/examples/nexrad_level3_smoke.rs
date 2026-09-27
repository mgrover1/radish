//! Manual smoke test for the NEXRAD Level 3 (NIDS) backend — decodes a real
//! product and prints the decoded fields for eyeball cross-checking against
//! the byte-level oracle used by `radish/tests/test_nexrad_level3_parity.rs`
//! (see `radish/tests/fixtures/CORPUS.md`). That test is the automated,
//! byte-for-byte version of this comparison; this example is the fast
//! manual version useful while iterating on the decoder.
//!
//! ```text
//! cargo run --example nexrad_level3_smoke -- /path/to/LOT_N0B_...
//! ```
//!
//! Falls back to `$RADISH_NEXRAD_LEVEL3_FIXTURE_DIR/LOT_N0B_2026_07_31_13_06_53`
//! if no argument is given (see `radish/tests/fixtures/CORPUS.md` for how to
//! populate that directory).

use radish::backends::{NexradLevel3Backend, RadarBackend};
use std::path::Path;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        let dir = std::env::var("RADISH_NEXRAD_LEVEL3_FIXTURE_DIR")
            .expect("pass a NIDS file path, or set RADISH_NEXRAD_LEVEL3_FIXTURE_DIR — see radish/tests/fixtures/CORPUS.md");
        format!("{dir}/LOT_N0B_2026_07_31_13_06_53")
    });

    let backend = NexradLevel3Backend::new();
    println!("can_read: {}", backend.can_read(Path::new(&path)));

    match backend.read_volume(Path::new(&path)) {
        Ok(v) => {
            let s = &v.sweeps[0];
            println!("OK:");
            println!("  site (instrument_name): {}", v.metadata.instrument_name);
            println!(
                "  lat/lon/height_m: {:.6}, {:.6}, {:.6}",
                v.metadata.latitude, v.metadata.longitude, v.metadata.altitude
            );
            println!("  scan_time: {}", v.metadata.time_coverage_start);
            if let Some(nids) = &s.metadata.nids {
                println!(
                    "  product/message_code/vcp: {} / {} / {}",
                    nids.awips_id, nids.message_code, nids.vcp
                );
            }
            println!("  elevation_deg: {}", s.metadata.fixed_angle);
            println!(
                "  shape: ({}, {})",
                s.coordinates.azimuth.len(),
                s.coordinates.range.len()
            );
            println!(
                "  first_gate_m/gate_spacing_m: {} / {}",
                s.coordinates.range[0],
                s.coordinates.range[1] - s.coordinates.range[0]
            );
            println!("  azimuths[:5]: {:?}", &s.coordinates.azimuth[..5]);
            for (name, m) in &s.moments {
                let scale = m
                    .declared_scale
                    .expect("NIDS moments always carry a declared scale");
                println!(
                    "  moment {name}: value_min={} value_increment={} n_levels={}",
                    scale.value_min, scale.value_increment, scale.n_levels
                );
                let codes = m
                    .raw_codes
                    .as_ref()
                    .expect("NIDS moments always carry raw_codes");
                println!(
                    "    codes[0, :20]: {:?}",
                    &codes.row(0).as_slice().unwrap()[..20]
                );
                let sum: u64 = codes.iter().map(|&c| c as u64).sum();
                println!("    codes sum: {sum}");
            }
        }
        Err(e) => println!("ERR: {e}"),
    }
}
