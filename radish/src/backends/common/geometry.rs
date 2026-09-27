//! Per-moment geometry probed from the first ray that carries the moment.
//!
//! When a sweep contains multiple moments at different gate resolutions
//! (NEXRAD super-res reflectivity at 250 m vs Doppler at 1 km being the
//! canonical case), the adapter has to:
//!
//! 1. Pick a **canonical range axis** — the finest gate spacing × largest
//!    gate count across all moments in the sweep.
//! 2. Resize coarser moments by NaN-padding their trailing gates.
//!
//! [`MomentGeometry`] captures the per-moment `(first_gate_km,
//! gate_interval_km, gate_count)` triple needed to make those decisions.
//! It deliberately carries no backend-specific identifier; callers track
//! the moment-to-geometry association in their own data structures.

/// Geometry of a single moment within a sweep.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MomentGeometry {
    /// Distance from the radar to the centre of the first gate, in kilometres.
    pub(crate) first_gate_km: f64,
    /// Spacing between consecutive gate centres, in kilometres.
    pub(crate) gate_interval_km: f64,
    /// Number of gates in this moment for this sweep.
    pub(crate) gate_count: usize,
}

impl MomentGeometry {
    /// First gate distance in metres (kilometres internal, metres on output).
    #[inline]
    pub(crate) fn first_gate_m(&self) -> f32 {
        (self.first_gate_km as f32) * 1000.0
    }

    /// Gate spacing in metres.
    #[inline]
    pub(crate) fn gate_interval_m(&self) -> f32 {
        (self.gate_interval_km as f32) * 1000.0
    }
}

/// Build the per-gate range axis (in metres) for a moment with this geometry.
pub(crate) fn build_range_axis(geom: &MomentGeometry) -> Vec<f32> {
    let first = geom.first_gate_m();
    let step = geom.gate_interval_m();
    (0..geom.gate_count)
        .map(|i| first + (i as f32) * step)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_range_axis_starts_at_first_gate_and_steps_correctly() {
        let geom = MomentGeometry {
            first_gate_km: 2.0,
            gate_interval_km: 0.25,
            gate_count: 4,
        };
        assert_eq!(
            build_range_axis(&geom),
            vec![2000.0, 2250.0, 2500.0, 2750.0]
        );
    }

    #[test]
    fn build_range_axis_handles_zero_count() {
        let geom = MomentGeometry {
            first_gate_km: 1.0,
            gate_interval_km: 0.5,
            gate_count: 0,
        };
        assert_eq!(build_range_axis(&geom), Vec::<f32>::new());
    }
}
