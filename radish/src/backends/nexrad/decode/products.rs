//! `Product` enum + `DataMoment` trait — the upstream-API-shaped
//! surface that radish's NEXRAD adapter consumes.
//!
//! Mirrors `nexrad_model::data::Product` plus the `DataMoment`
//! geometry trait (first_gate, interval, gate_count) so the adapter
//! can probe sweep geometry uniformly across the six measurement
//! moments and the CFP overlay block.

use super::messages::msg31::cfp::CfpValue;
use super::messages::msg31::moment::MomentValue as InnerMomentValue;
use super::model::{OwnedCfp, OwnedMoment, Radial};

/// Common geometry for any per-radial moment block. Used by the
/// adapter's `probe_geometry` to size the (rays × gates) array
/// without decoding gate values. f64 return matches
/// `backends::common::MomentGeometry` so the adapter doesn't need
/// per-call casts.
pub(crate) trait DataMoment {
    fn first_gate_range_km(&self) -> f64;
    fn gate_interval_km(&self) -> f64;
    fn gate_count(&self) -> u16;
}

impl DataMoment for OwnedMoment {
    fn first_gate_range_km(&self) -> f64 {
        f64::from(self.descriptor.range_to_first_gate_km)
    }
    fn gate_interval_km(&self) -> f64 {
        f64::from(self.descriptor.gate_interval_km)
    }
    fn gate_count(&self) -> u16 {
        self.descriptor.gate_count
    }
}

impl DataMoment for OwnedCfp {
    fn first_gate_range_km(&self) -> f64 {
        f64::from(self.descriptor.range_to_first_gate_km)
    }
    fn gate_interval_km(&self) -> f64 {
        f64::from(self.descriptor.gate_interval_km)
    }
    fn gate_count(&self) -> u16 {
        self.descriptor.gate_count
    }
}

/// One sample value from a measurement moment. Re-exports
/// `messages::msg31::moment::MomentValue` so the adapter can
/// `match` on it without reaching into the message-internals path.
pub(crate) type MomentValue = InnerMomentValue;

/// One sample value from the CFP block. Wraps `CfpValue` so the
/// adapter can `match` on `Value(_)` vs `Status(_)` like upstream's
/// `CFPMomentValue` enum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CfpMomentValue {
    Value(f32),
    Status(CfpStatus),
}

// Variant names mirror ICD Table XVII-Q semantic distinctions
// (filter not applied / point clutter filter applied / censor
// pulses applied) and upstream's `CFPStatus`. The shared `Applied`
// postfix reflects the spec wording, not stutter.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CfpStatus {
    FilterNotApplied,
    PointClutterFilterApplied,
    CensorPulsesApplied,
}

impl From<CfpValue> for CfpMomentValue {
    fn from(v: CfpValue) -> Self {
        match v {
            CfpValue::FilterNotApplied => CfpMomentValue::Status(CfpStatus::FilterNotApplied),
            CfpValue::PointClutterFilterApplied => {
                CfpMomentValue::Status(CfpStatus::PointClutterFilterApplied)
            }
            CfpValue::CensorPulsesApplied => CfpMomentValue::Status(CfpStatus::CensorPulsesApplied),
            CfpValue::PowerDb(v) => CfpMomentValue::Value(v),
        }
    }
}

/// The seven moments radish surfaces from a NEXRAD sweep, in xradar-
/// matching order. Mirrors `nexrad_model::data::Product`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Product {
    Reflectivity,
    Velocity,
    SpectrumWidth,
    DifferentialReflectivity,
    DifferentialPhase,
    CorrelationCoefficient,
    ClutterFilterPower,
}

impl Product {
    /// For the six measurement moments: borrow the radial's
    /// matching `OwnedMoment` if present. Returns `None` for
    /// `ClutterFilterPower` — use [`cfp_moment_data`] for that.
    pub(crate) fn moment_data<'a>(&self, r: &'a Radial) -> Option<&'a OwnedMoment> {
        match self {
            Product::Reflectivity => r.reflectivity.as_ref(),
            Product::Velocity => r.velocity.as_ref(),
            Product::SpectrumWidth => r.spectrum_width.as_ref(),
            Product::DifferentialReflectivity => r.differential_reflectivity.as_ref(),
            Product::DifferentialPhase => r.differential_phase.as_ref(),
            Product::CorrelationCoefficient => r.correlation_coefficient.as_ref(),
            Product::ClutterFilterPower => None,
        }
    }

    /// For `ClutterFilterPower`: borrow the radial's CFP block if
    /// present. Returns `None` for the six measurement moments.
    pub(crate) fn cfp_moment_data<'a>(&self, r: &'a Radial) -> Option<&'a OwnedCfp> {
        match self {
            Product::ClutterFilterPower => r.clutter_filter_power.as_ref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::nexrad::decode::messages::msg31::moment::MomentDescriptor;

    fn make_moment(scale: f32, offset: f32, gates: Vec<u8>) -> OwnedMoment {
        OwnedMoment {
            descriptor: MomentDescriptor {
                gate_count: gates.len() as u16,
                range_to_first_gate_km: 2.0,
                gate_interval_km: 0.25,
                tover_db: 0.0,
                snr_threshold_db: 0.0,
                control_flags: 0,
                data_word_size_bits: 8,
                scale,
                offset,
            },
            gate_bytes: gates,
        }
    }

    #[test]
    fn moment_data_dispatches_to_correct_field() {
        let mut radial = Radial {
            azimuth_number: 0,
            azimuth_angle_degrees: 0.0,
            azimuth_resolution_spacing: 0,
            elevation_number: 1,
            elevation_angle_degrees: 0.5,
            radial_status: 0,
            collection_time: None,
            reflectivity: Some(make_moment(2.0, 66.0, vec![10])),
            velocity: None,
            spectrum_width: None,
            differential_reflectivity: None,
            differential_phase: None,
            correlation_coefficient: None,
            clutter_filter_power: None,
        };
        assert!(Product::Reflectivity.moment_data(&radial).is_some());
        assert!(Product::Velocity.moment_data(&radial).is_none());
        assert!(Product::ClutterFilterPower.moment_data(&radial).is_none());

        radial.velocity = Some(make_moment(2.0, 129.0, vec![10]));
        assert!(Product::Velocity.moment_data(&radial).is_some());
    }

    #[test]
    fn cfp_moment_data_only_dispatches_for_clutter_filter_power() {
        let radial = Radial {
            azimuth_number: 0,
            azimuth_angle_degrees: 0.0,
            azimuth_resolution_spacing: 0,
            elevation_number: 1,
            elevation_angle_degrees: 0.5,
            radial_status: 0,
            collection_time: None,
            reflectivity: Some(make_moment(2.0, 66.0, vec![10])),
            velocity: None,
            spectrum_width: None,
            differential_reflectivity: None,
            differential_phase: None,
            correlation_coefficient: None,
            clutter_filter_power: Some(OwnedCfp {
                descriptor: MomentDescriptor {
                    gate_count: 1,
                    range_to_first_gate_km: 2.0,
                    gate_interval_km: 0.25,
                    tover_db: 0.0,
                    snr_threshold_db: 0.0,
                    control_flags: 0,
                    data_word_size_bits: 8,
                    scale: 2.0,
                    offset: 0.0,
                },
                gate_bytes: vec![10],
            }),
        };
        assert!(Product::Reflectivity.cfp_moment_data(&radial).is_none());
        assert!(Product::ClutterFilterPower
            .cfp_moment_data(&radial)
            .is_some());
    }

    #[test]
    fn data_moment_geometry_for_owned_moment() {
        let m = make_moment(2.0, 66.0, vec![10, 20]);
        assert_eq!(m.gate_count(), 2);
        assert!((m.first_gate_range_km() - 2.0).abs() < 1e-6);
        assert!((m.gate_interval_km() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn cfp_value_to_cfp_moment_value() {
        assert_eq!(
            CfpMomentValue::from(CfpValue::FilterNotApplied),
            CfpMomentValue::Status(CfpStatus::FilterNotApplied)
        );
        assert_eq!(
            CfpMomentValue::from(CfpValue::PointClutterFilterApplied),
            CfpMomentValue::Status(CfpStatus::PointClutterFilterApplied)
        );
        assert_eq!(
            CfpMomentValue::from(CfpValue::PowerDb(0.5)),
            CfpMomentValue::Value(0.5)
        );
    }
}
