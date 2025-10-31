use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: cargo run --example test_netcdf_open <netcdf_file>");
        std::process::exit(1);
    }

    let filename = &args[1];
    println!("Attempting to open: {}", filename);

    match netcdf::open(filename) {
        Ok(file) => {
            println!("Successfully opened file!");
            println!("Dimensions: {:?}", file.dimensions().map(|d| d.name()).collect::<Vec<_>>());
            println!("Variables: {}", file.variables().map(|v| v.name()).collect::<Vec<_>>().len());

            // Test reading time_coverage_start
            if let Some(var) = file.variable("time_coverage_start") {
                println!("\ntime_coverage_start variable found!");
                println!("  Dimensions: {:?}", var.dimensions().iter().map(|d| (d.name(), d.len())).collect::<Vec<_>>());

                // Try get_string
                match var.get_string(()) {
                    Ok(s) => println!("  get_string succeeded: {}", s),
                    Err(e) => println!("  get_string failed: {}", e),
                }

                // Try reading as char array (u8)
                match var.get::<u8, _>(..) {
                    Ok(data) => {
                        println!("  get::<u8> succeeded, shape: {:?}", data.shape());
                        let bytes: Vec<u8> = data.into_iter().filter(|&b| b != 0).collect();
                        let s = String::from_utf8_lossy(&bytes);
                        println!("  Value: {}", s);
                    }
                    Err(e) => println!("  get::<u8> failed: {}", e),
                }

                // Try reading as i8
                match var.get::<i8, _>(..) {
                    Ok(data) => {
                        println!("  get::<i8> succeeded, shape: {:?}", data.shape());
                        let bytes: Vec<u8> = data.into_iter()
                            .take_while(|&c| c != 0)
                            .map(|c| c as u8)
                            .collect();
                        let s = String::from_utf8_lossy(&bytes);
                        println!("  Value: '{}'", s);
                    }
                    Err(e) => println!("  get::<i8> failed: {}", e),
                }
            }
        }
        Err(e) => {
            eprintln!("Error opening file: {:?}", e);
            eprintln!("Error details: {}", e);
            std::process::exit(1);
        }
    }
}
