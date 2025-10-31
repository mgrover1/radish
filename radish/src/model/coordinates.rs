/// Coordinate data structures

/// Coordinate data for a sweep
#[derive(Debug, Clone)]
pub struct Coordinates {
    /// Time for each ray (seconds since epoch)
    pub time: Vec<f64>,

    /// Range gates (meters from radar)
    pub range: Vec<f32>,

    /// Azimuth angles (degrees)
    pub azimuth: Vec<f32>,

    /// Elevation angles (degrees)
    pub elevation: Vec<f32>,
}

impl Coordinates {
    /// Create new Coordinates
    pub fn new(
        time: Vec<f64>,
        range: Vec<f32>,
        azimuth: Vec<f32>,
        elevation: Vec<f32>,
    ) -> Self {
        Self {
            time,
            range,
            azimuth,
            elevation,
        }
    }

    /// Number of rays (azimuth/time dimension)
    pub fn num_rays(&self) -> usize {
        self.time.len()
    }

    /// Number of gates (range dimension)
    pub fn num_gates(&self) -> usize {
        self.range.len()
    }

    /// Validate coordinate dimensions match
    pub fn validate(&self) -> Result<(), String> {
        let num_rays = self.time.len();

        if self.azimuth.len() != num_rays {
            return Err(format!(
                "Azimuth length ({}) doesn't match time length ({})",
                self.azimuth.len(),
                num_rays
            ));
        }

        if self.elevation.len() != num_rays {
            return Err(format!(
                "Elevation length ({}) doesn't match time length ({})",
                self.elevation.len(),
                num_rays
            ));
        }

        Ok(())
    }
}
