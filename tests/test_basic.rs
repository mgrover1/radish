/// Basic tests for radish core library

use radish::{
    model::{VolumeMetadata, SweepMetadata, MomentData, Coordinates},
    backends::CfRadial1Backend,
};
use chrono::Utc;
use ndarray::Array2;
use radish_types::SweepMode;

#[test]
fn test_volume_metadata_creation() {
    let metadata = VolumeMetadata::new(
        "TEST_RADAR".to_string(),
        40.0,
        -105.0,
        1000.0,
        Utc::now(),
        Utc::now(),
    );

    assert_eq!(metadata.instrument_name, "TEST_RADAR");
    assert_eq!(metadata.latitude, 40.0);
    assert_eq!(metadata.longitude, -105.0);
    assert_eq!(metadata.altitude, 1000.0);
}

#[test]
fn test_sweep_metadata_creation() {
    let metadata = SweepMetadata::new(0, SweepMode::Azimuth, 0.5);

    assert_eq!(metadata.sweep_number, 0);
    assert_eq!(metadata.sweep_mode, SweepMode::Azimuth);
    assert_eq!(metadata.fixed_angle, 0.5);
}

#[test]
fn test_moment_data_creation() {
    let data = Array2::zeros((360, 1000));
    let moment = MomentData::new("DBZH".to_string(), "dBZ".to_string(), data);

    assert_eq!(moment.name, "DBZH");
    assert_eq!(moment.units, "dBZ");
    assert_eq!(moment.shape(), (360, 1000));
}

#[test]
fn test_coordinates_validation() {
    let coords = Coordinates::new(
        vec![0.0; 360],
        vec![0.0; 1000],
        vec![0.0; 360],
        vec![0.0; 360],
    );

    assert!(coords.validate().is_ok());
    assert_eq!(coords.num_rays(), 360);
    assert_eq!(coords.num_gates(), 1000);
}

#[test]
fn test_coordinates_validation_fails() {
    let coords = Coordinates::new(
        vec![0.0; 360],
        vec![0.0; 1000],
        vec![0.0; 350],  // Wrong size
        vec![0.0; 360],
    );

    assert!(coords.validate().is_err());
}

#[test]
fn test_backend_name() {
    let backend = CfRadial1Backend::new();
    assert_eq!(backend.name(), "cfradial1");
    assert!(backend.supported_extensions().contains(&"nc"));
}
