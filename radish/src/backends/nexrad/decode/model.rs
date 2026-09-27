//! Radish-internal NEXRAD data model: top-level `Scan` + `Sweep` +
//! `Radial` + `Site` types built from the typed Msg31 / Msg2 / Msg5
//! parsers.
//!
//! This file deliberately **doesn't** mirror `nexrad-model`'s full
//! public surface field-for-field. We only expose what radish's
//! adapter (`radish/src/backends/nexrad/adapter.rs`) consumes plus
//! enough hooks for Phase 6's parity tests against
//! `danielway/nexrad`. Phase 7 (wire-in) will adjust the adapter
//! to call our accessors directly.
//!
//! The VCP (`Msg5`), RDA status (`Msg2`), and per-radial moment /
//! info-block types are re-exported from `messages::*` rather than
//! re-defined here — they're already field-for-field aligned with
//! the ICD.

use chrono::{DateTime, TimeZone, Utc};

use super::messages::msg1::Msg1;
use super::messages::msg2::Msg2;
use super::messages::msg31::cfp::CfpBlock;
use super::messages::msg31::header::DataHeader;
use super::messages::msg31::info_blocks::VolumeBlock;
use super::messages::msg31::moment::{MomentBlock, MomentDescriptor, MomentValue};
use super::messages::msg31::Msg31;
use super::messages::msg5::Msg5;

/// Top-level decoded NEXRAD Level 2 volume. Owns its gate bytes
/// (Vec<u8>) so the whole tree is independent of the input buffer's
/// lifetime — matches the existing `nexrad_model::data::Radial`
/// shape that radish's adapter consumes today.
#[derive(Debug)]
pub(crate) struct Scan {
    pub(crate) coverage_pattern: Msg5,
    pub(crate) sweeps: Vec<Sweep>,
    pub(crate) site: Option<Site>,
    pub(crate) rda_status: Option<Msg2>,
}

impl Scan {
    /// Earliest / latest collection_time across every sweep. `None`
    /// if no radial in the volume carries a timestamp. Matches
    /// upstream's `Scan::time_range()` shape so the adapter can
    /// drop in.
    pub(crate) fn time_range(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        let mut earliest: Option<DateTime<Utc>> = None;
        let mut latest: Option<DateTime<Utc>> = None;
        for sweep in &self.sweeps {
            if let Some((s, e)) = sweep.time_range() {
                earliest = Some(match earliest {
                    Some(prev) => prev.min(s),
                    None => s,
                });
                latest = Some(match latest {
                    Some(prev) => prev.max(e),
                    None => e,
                });
            }
        }
        earliest.zip(latest)
    }
}

/// One elevation sweep — a contiguous run of radials with the same
/// `elevation_number` bracketed by ICD radial_status start / end
/// markers (§3.2.4.17 Table XVII-A).
#[derive(Debug)]
pub(crate) struct Sweep {
    pub(crate) elevation_number: u8,
    pub(crate) radials: Vec<Radial>,
    /// `false` when the cut's data is provably partial: the stream
    /// ended before an end marker (truncated volume / partial chunk
    /// list), the first radial is not a start marker (stream joined
    /// mid-rotation), or fewer rays arrived than the rotation's
    /// nominal count (interior chunk gap). See
    /// [`group_radials_into_sweeps`].
    pub(crate) complete: bool,
}

impl Sweep {
    /// Median of per-ray elevation angles (matches xradar's
    /// "achieved" elevation, not the MSG_5 commanded value).
    /// Returns `None` for empty sweeps.
    pub(crate) fn elevation_angle_degrees(&self) -> Option<f32> {
        if self.radials.is_empty() {
            return None;
        }
        let mut angles: Vec<f32> = self
            .radials
            .iter()
            .map(|r| r.elevation_angle_degrees)
            .collect();
        angles.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = angles.len() / 2;
        Some(if angles.len().is_multiple_of(2) {
            (angles[mid - 1] + angles[mid]) / 2.0
        } else {
            angles[mid]
        })
    }

    /// Earliest / latest `collection_time` across the sweep's
    /// radials; `None` if no radial carries a timestamp.
    pub(crate) fn time_range(&self) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
        let mut earliest: Option<DateTime<Utc>> = None;
        let mut latest: Option<DateTime<Utc>> = None;
        for r in &self.radials {
            if let Some(t) = r.collection_time {
                earliest = Some(match earliest {
                    Some(e) => e.min(t),
                    None => t,
                });
                latest = Some(match latest {
                    Some(l) => l.max(t),
                    None => t,
                });
            }
        }
        earliest.zip(latest)
    }
}

/// One radial's per-ray fields plus owned moment + CFP data.
#[derive(Debug, Clone)]
pub(crate) struct Radial {
    pub(crate) azimuth_number: u16,
    pub(crate) azimuth_angle_degrees: f32,
    /// ICD §3.2.4.17: `1` = 0.5° spacing (720 rays / rotation),
    /// `2` = 1.0° (360). `0` = unknown (MSG_1 legacy has no such
    /// field) — consumers must fall back to inference.
    pub(crate) azimuth_resolution_spacing: u8,
    pub(crate) elevation_number: u8,
    pub(crate) elevation_angle_degrees: f32,
    pub(crate) radial_status: u8,
    pub(crate) collection_time: Option<DateTime<Utc>>,
    pub(crate) reflectivity: Option<OwnedMoment>,
    pub(crate) velocity: Option<OwnedMoment>,
    pub(crate) spectrum_width: Option<OwnedMoment>,
    pub(crate) differential_reflectivity: Option<OwnedMoment>,
    pub(crate) differential_phase: Option<OwnedMoment>,
    pub(crate) correlation_coefficient: Option<OwnedMoment>,
    pub(crate) clutter_filter_power: Option<OwnedCfp>,
}

/// Owned moment block. Functionally identical to
/// `messages::msg31::moment::MomentBlock<'a>` but with a `Vec<u8>`
/// gate buffer so the surrounding `Radial` doesn't carry a
/// borrowed lifetime. Allocation cost is unavoidable in any
/// API that returns a self-contained `Scan` (matches the existing
/// `nexrad_model::data::Radial` ownership model).
#[derive(Debug, Clone)]
pub(crate) struct OwnedMoment {
    pub(crate) descriptor: MomentDescriptor,
    pub(crate) gate_bytes: Vec<u8>,
}

impl OwnedMoment {
    fn from_borrowed(b: MomentBlock<'_>) -> Self {
        Self {
            descriptor: b.descriptor,
            gate_bytes: b.gate_bytes.to_vec(),
        }
    }

    /// Iterate decoded moment values per ICD Table XVII-I
    /// (`raw == 0 → BelowThreshold`, `raw == 1 → RangeFolded`,
    /// else `(raw - offset) / scale`).
    pub(crate) fn iter(&self) -> impl Iterator<Item = MomentValue> + '_ {
        MomentBlock {
            descriptor: self.descriptor,
            gate_bytes: &self.gate_bytes,
        }
        .iter()
    }
}

/// Owned CFP block. Same relationship to `CfpBlock<'a>` as
/// `OwnedMoment` to `MomentBlock<'a>`.
#[derive(Debug, Clone)]
pub(crate) struct OwnedCfp {
    pub(crate) descriptor: MomentDescriptor,
    pub(crate) gate_bytes: Vec<u8>,
}

impl OwnedCfp {
    fn from_borrowed(b: CfpBlock<'_>) -> Self {
        Self {
            descriptor: b.descriptor,
            gate_bytes: b.gate_bytes.to_vec(),
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = super::messages::msg31::cfp::CfpValue> + '_ {
        CfpBlock {
            descriptor: self.descriptor,
            gate_bytes: &self.gate_bytes,
        }
        .iter()
    }

    /// Iterate the CFP block as `CfpMomentValue` (the adapter's
    /// preferred shape — `Status(_)` collapses the three filter
    /// states; `Value(f32)` carries decoded power dB).
    pub(crate) fn iter_moment_value(
        &self,
    ) -> impl Iterator<Item = super::products::CfpMomentValue> + '_ {
        self.iter().map(super::products::CfpMomentValue::from)
    }
}

impl Radial {
    /// Convert a parsed `Msg1` (legacy pre-Build-12 radial) into an
    /// owned `Radial`. Dual-pol moments stay `None` — MSG_1 predates
    /// the Build-12 dual-polarization upgrade. Gate scaling per
    /// ICD §3.2.4.5 Table III-E:
    /// `dBZ = (raw - 66)/2`, velocity offset 129.0 with scale 2.0
    /// (0.5 m/s LSB) or 1.0 (1.0 m/s LSB), spectrum width same as
    /// 0.5-m/s velocity.
    pub(crate) fn from_msg1(m: Msg1) -> Self {
        let collection_time = m.collection_time();
        let surv_range_km = f32::from(m.surveillance_first_gate_range_m.unsigned_abs()) / 1000.0;
        let surv_interval_km = f32::from(m.surveillance_gate_interval_m.max(0)) / 1000.0;
        let dopp_range_km = f32::from(m.doppler_first_gate_range_m.unsigned_abs()) / 1000.0;
        let dopp_interval_km = f32::from(m.doppler_gate_interval_m.max(0)) / 1000.0;
        let vel_scale = if m.doppler_velocity_resolution == 4 {
            1.0
        } else {
            2.0
        };
        let azimuth_angle_degrees = m.azimuth_angle_degrees();
        let elevation_angle_degrees = m.elevation_angle_degrees();
        // ICD-defined ranges: radial_status 0..=5 (Table III-C),
        // elevation_number 1..=N (N <= ~25), azimuth_number 1..=720.
        // Fall back to 0 on out-of-range — the lenient
        // `parse_fixed_frame_payload` already routed garbage frames
        // to Raw, so anything reaching here is plausible-enough.
        let radial_status = u8::try_from(m.radial_status).unwrap_or(0);
        let elevation_number = u8::try_from(m.elevation_number).unwrap_or(0);
        let azimuth_number = u16::try_from(m.azimuth_number).unwrap_or(0);
        let num_surv = u16::try_from(m.num_surveillance_gates.max(0)).unwrap_or(0);
        let num_dopp = u16::try_from(m.num_doppler_gates.max(0)).unwrap_or(0);

        let reflectivity = m.reflectivity_gates.map(|gates| OwnedMoment {
            descriptor: legacy_descriptor(num_surv, surv_range_km, surv_interval_km, 2.0, 66.0),
            gate_bytes: gates,
        });
        let velocity = m.velocity_gates.map(|gates| OwnedMoment {
            descriptor: legacy_descriptor(
                num_dopp,
                dopp_range_km,
                dopp_interval_km,
                vel_scale,
                129.0,
            ),
            gate_bytes: gates,
        });
        let spectrum_width = m.spectrum_width_gates.map(|gates| OwnedMoment {
            descriptor: legacy_descriptor(num_dopp, dopp_range_km, dopp_interval_km, 2.0, 129.0),
            gate_bytes: gates,
        });

        Self {
            azimuth_number,
            azimuth_angle_degrees,
            azimuth_resolution_spacing: 0,
            elevation_number,
            elevation_angle_degrees,
            radial_status,
            collection_time,
            reflectivity,
            velocity,
            spectrum_width,
            differential_reflectivity: None,
            differential_phase: None,
            correlation_coefficient: None,
            clutter_filter_power: None,
        }
    }

    /// Convert a parsed `Msg31` into an owned `Radial` by copying
    /// the per-product gate-byte slices into owned `Vec<u8>`s.
    pub(crate) fn from_msg31(m: Msg31<'_>) -> Self {
        let collection_time = msg31_collection_time(&m.header);
        let Msg31 {
            header,
            volume: _,
            elevation: _,
            radial: _,
            reflectivity,
            velocity,
            spectrum_width,
            zdr,
            phi,
            rho,
            cfp,
        } = m;
        Self {
            azimuth_number: header.azimuth_number,
            azimuth_angle_degrees: header.azimuth_angle_degrees,
            azimuth_resolution_spacing: header.azimuth_resolution_spacing,
            elevation_number: header.elevation_number,
            elevation_angle_degrees: header.elevation_angle_degrees,
            radial_status: header.radial_status,
            collection_time,
            reflectivity: reflectivity.map(OwnedMoment::from_borrowed),
            velocity: velocity.map(OwnedMoment::from_borrowed),
            spectrum_width: spectrum_width.map(OwnedMoment::from_borrowed),
            differential_reflectivity: zdr.map(OwnedMoment::from_borrowed),
            differential_phase: phi.map(OwnedMoment::from_borrowed),
            correlation_coefficient: rho.map(OwnedMoment::from_borrowed),
            clutter_filter_power: cfp.map(OwnedCfp::from_borrowed),
        }
    }
}

/// Build a synthetic `MomentDescriptor` for MSG_1 gate arrays. MSG_1
/// has no on-wire descriptor block — fields like `tover_db` and
/// `snr_threshold_db` weren't part of the legacy format, so we
/// fill them with zeros. The downstream `MomentValue::iter` only
/// reads `gate_count`, `data_word_size_bits`, `scale`, and `offset`.
///
/// Scale/offset convention reminder: `MomentValue::iter` decodes via
/// `value = (raw - offset) / scale` (msg31/moment.rs:167). The ICD
/// Table III-E reflectivity formula is `dBZ = (raw - 2)/2 - 32`,
/// which rearranges to `(raw - 66)/2` — hence `offset=66.0,
/// scale=2.0`. Velocity at 0.5 m/s LSB: `m/s = (raw - 2)*0.5 - 63.5`
/// → `(raw - 129)/2` → `offset=129.0, scale=2.0`. Velocity at 1.0
/// m/s LSB: `(raw - 129)/1` → `offset=129.0, scale=1.0`.
fn legacy_descriptor(
    gate_count: u16,
    range_to_first_gate_km: f32,
    gate_interval_km: f32,
    scale: f32,
    offset: f32,
) -> MomentDescriptor {
    MomentDescriptor {
        gate_count,
        range_to_first_gate_km,
        gate_interval_km,
        tover_db: 0.0,
        snr_threshold_db: 0.0,
        control_flags: 0,
        data_word_size_bits: 8,
        scale,
        offset,
    }
}

/// Build a `DateTime<Utc>` from the MSG_31 header's
/// `modified_julian_date` and `collection_time_ms`. Returns `None`
/// if the values fall outside the chrono-representable range.
///
/// Per ICD 2620002R Table III §3.2.4.17, `modified_julian_date` is
/// **1-indexed days since 1970-01-01**: day 1 = 1970-01-01,
/// day 2 = 1970-01-02, … `collection_time_ms` is milliseconds since
/// midnight of that day. The `-1` below is the difference between
/// "1-indexed day-of-epoch" (ICD convention) and "0-indexed
/// seconds-since-epoch" (Unix convention). Matches xradar's
/// `(date - 1) * 86400e3 + ms` (`nexrad_level2.py`) and
/// `danielway/nexrad`'s `volume/record.rs` byte-for-byte.
pub(crate) fn msg31_collection_time(h: &DataHeader) -> Option<DateTime<Utc>> {
    let days = i64::from(h.modified_julian_date);
    let secs = i64::from(h.collection_time_ms / 1_000);
    let nanos = (h.collection_time_ms % 1_000) * 1_000_000;
    let total_secs = days
        .checked_sub(1)?
        .checked_mul(86_400)?
        .checked_add(secs)?;
    Utc.timestamp_opt(total_secs, nanos).single()
}

/// Radar site location — extracted once from the first MSG_31's
/// VOL block (Volume Data Constant Type, ICD §3.2.4.17.5).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Site {
    /// 4-byte ASCII ICAO from the MSG_31 data header.
    pub(crate) identifier: [u8; 4],
    pub(crate) latitude_degrees: f32,
    pub(crate) longitude_degrees: f32,
    /// Height of site base above sea level (m).
    pub(crate) site_height_m: i16,
    /// Height of radar tower above ground (m).
    pub(crate) tower_height_m: u16,
}

impl Site {
    pub(crate) fn from_vol(identifier: [u8; 4], vol: &VolumeBlock) -> Self {
        Self {
            identifier,
            latitude_degrees: vol.latitude_degrees,
            longitude_degrees: vol.longitude_degrees,
            site_height_m: vol.site_height_m,
            tower_height_m: vol.tower_height_m,
        }
    }

    pub(crate) fn icao_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.identifier)
    }
}

/// Group an arrival-order list of radials into sweeps using ICD
/// §3.2.4.17 radial_status markers (HW 11):
///
/// * `0` = start of new elevation
/// * `1` = intermediate radial
/// * `2` = end of elevation
/// * `3` = start of new volume
/// * `4` = end of volume
/// * `5` = start of new elevation, last in VCP
///
/// Falls back to `elevation_number` change detection when status
/// markers are missing or noisy. **Audit-required:** matches the
/// upstream `Sweep::from_radials` divergence noted in the
/// `danielway/nexrad` audit (which only used elevation_number) —
/// our grouping is ICD-correct so SAILS / MRLE supplemental cuts
/// re-using a previous elevation_number form their own short
/// sweep instead of merging into the parent.
pub(crate) fn group_radials_into_sweeps(radials: Vec<Radial>) -> Vec<Sweep> {
    /// A sweep being accumulated, plus what its completeness verdict
    /// needs: whether its first radial was a start marker.
    struct InFlight {
        elevation_number: u8,
        saw_start: bool,
        radials: Vec<Radial>,
    }

    impl InFlight {
        fn open(first: Radial) -> Self {
            Self {
                elevation_number: first.elevation_number,
                saw_start: matches!(first.radial_status, 0 | 3 | 5),
                radials: vec![first],
            }
        }

        /// Close the sweep. `at_data_end` marks the one close that is
        /// never voluntary: the stream ran out with the sweep still
        /// open (truncated volume / partial chunk list).
        ///
        /// A sweep is complete only when it saw its start marker, was
        /// not cut off by data end, and — when the nominal rotation
        /// size is known — carries at least that many rays. The ray
        /// count guards against interior gaps (a chunk list missing a
        /// middle `I` chunk still shows start + end markers).
        fn close(self, at_data_end: bool) -> Sweep {
            let complete = self.saw_start
                && !at_data_end
                && nominal_ray_count(&self.radials).is_none_or(|n| self.radials.len() >= n);
            Sweep {
                elevation_number: self.elevation_number,
                radials: self.radials,
                complete,
            }
        }
    }

    let mut sweeps = Vec::new();
    let mut current: Option<InFlight> = None;

    for radial in radials {
        let status = radial.radial_status;

        match &mut current {
            // Same cut, no start marker: append to the in-flight sweep.
            Some(inflight)
                if inflight.elevation_number == radial.elevation_number
                    && !matches!(status, 0 | 3 | 5) =>
            {
                inflight.radials.push(radial);
            }
            // Sweep boundary: an explicit start marker (0/3/5), or —
            // fallback for legacy files / corrupt status bytes — an
            // elevation_number change without one. Close any in-flight
            // sweep and open the new one with this radial.
            _ => {
                if let Some(prev) = current.take() {
                    sweeps.push(prev.close(false));
                }
                current = Some(InFlight::open(radial));
                continue;
            }
        }

        // Status 2/4 = end of elevation/volume — close the current
        // sweep. Done after the append so the terminator radial
        // makes it into the sweep.
        if matches!(status, 2 | 4) {
            if let Some(inflight) = current.take() {
                sweeps.push(inflight.close(false));
            }
        }
    }

    // Trailing sweep without an explicit end marker.
    if let Some(inflight) = current {
        sweeps.push(inflight.close(true));
    }

    sweeps
}

/// Nominal rays per full rotation, from the first radial carrying a
/// known MSG_31 `azimuth_resolution_spacing` (`1` = 0.5° → 720,
/// `2` = 1.0° → 360). `None` when no radial has one (MSG_1 legacy) —
/// completeness then rests on status markers alone.
pub(crate) fn nominal_ray_count(radials: &[Radial]) -> Option<usize> {
    radials
        .iter()
        .find_map(|r| match r.azimuth_resolution_spacing {
            1 => Some(720),
            2 => Some(360),
            _ => None,
        })
}

// Phase 7 will add convenience re-exports of `Msg2 as RdaStatus`,
// `Msg5 as VolumeCoveragePattern`, etc., once the adapter starts
// consuming this model directly. Until then the per-message
// modules (`messages::msg2`, `messages::msg5`, `messages::msg31`)
// are the source of truth and importing through them keeps the
// dependency graph explicit.

#[cfg(test)]
mod tests {
    use super::*;

    fn radial(azimuth_number: u16, elevation_number: u8, radial_status: u8) -> Radial {
        Radial {
            azimuth_number,
            azimuth_angle_degrees: f32::from(azimuth_number) * 0.5,
            // 0 = unknown spacing → completeness rests on markers
            // alone, keeping these grouping tests independent of the
            // nominal-ray-count rule (exercised by its own tests).
            azimuth_resolution_spacing: 0,
            elevation_number,
            elevation_angle_degrees: 0.5,
            radial_status,
            collection_time: None,
            reflectivity: None,
            velocity: None,
            spectrum_width: None,
            differential_reflectivity: None,
            differential_phase: None,
            correlation_coefficient: None,
            clutter_filter_power: None,
        }
    }

    #[test]
    fn group_emits_one_sweep_per_start_end_pair() {
        // status 0 = start, 1 = intermediate, 2 = end. Two sweeps.
        let radials = vec![
            radial(1, 1, 0),
            radial(2, 1, 1),
            radial(3, 1, 2),
            radial(1, 2, 0),
            radial(2, 2, 2),
        ];
        let sweeps = group_radials_into_sweeps(radials);
        assert_eq!(sweeps.len(), 2);
        assert_eq!(sweeps[0].elevation_number, 1);
        assert_eq!(sweeps[0].radials.len(), 3);
        assert_eq!(sweeps[1].elevation_number, 2);
        assert_eq!(sweeps[1].radials.len(), 2);
    }

    #[test]
    fn group_falls_back_to_elevation_number_change_when_status_missing() {
        // No 0/2/3/4/5 markers — only intermediate-status radials.
        // Sweeps form on elevation_number changes.
        let radials = vec![
            radial(1, 1, 1),
            radial(2, 1, 1),
            radial(3, 2, 1),
            radial(4, 2, 1),
        ];
        let sweeps = group_radials_into_sweeps(radials);
        assert_eq!(sweeps.len(), 2);
        assert_eq!(sweeps[0].elevation_number, 1);
        assert_eq!(sweeps[1].elevation_number, 2);
    }

    /// **Audit regression test.** SAILS / MRLE supplemental cuts
    /// re-use an earlier elevation_number partway through the
    /// volume. Status markers force them into a new sweep instead
    /// of merging back into the parent — the divergence the
    /// danielway audit identified.
    #[test]
    fn sails_supplemental_cut_with_status_marker_forms_separate_sweep() {
        let radials = vec![
            radial(1, 1, 0), // sweep A: elev=1
            radial(2, 1, 1),
            radial(3, 1, 2), // sweep A ends
            radial(1, 5, 0), // sweep B: elev=5
            radial(2, 5, 2), // sweep B ends
            radial(1, 1, 0), // sweep C: SAILS revisit at elev=1 — start marker forces new sweep
            radial(2, 1, 2),
        ];
        let sweeps = group_radials_into_sweeps(radials);
        assert_eq!(sweeps.len(), 3, "got: {sweeps:?}");
        assert_eq!(sweeps[0].elevation_number, 1);
        assert_eq!(sweeps[1].elevation_number, 5);
        assert_eq!(sweeps[2].elevation_number, 1);
        assert_eq!(sweeps[0].radials.len(), 3);
        assert_eq!(sweeps[2].radials.len(), 2);
    }

    #[test]
    fn group_handles_volume_start_and_end_status_codes() {
        // status 3 = start of volume (== start of new sweep).
        // status 4 = end of volume (== end of sweep).
        let radials = vec![radial(1, 1, 3), radial(2, 1, 1), radial(3, 1, 4)];
        let sweeps = group_radials_into_sweeps(radials);
        assert_eq!(sweeps.len(), 1);
        assert_eq!(sweeps[0].radials.len(), 3);
        assert!(sweeps[0].complete);
    }

    #[test]
    fn marker_closed_sweeps_are_complete_when_spacing_unknown() {
        // Both the end-marker close and the next-start-marker close
        // count as voluntary; spacing 0 disables the ray-count rule.
        let radials = vec![
            radial(1, 1, 0),
            radial(2, 1, 1),
            radial(3, 1, 2),
            radial(1, 2, 0),
            radial(2, 2, 1), // closed by data end, not a marker
        ];
        let sweeps = group_radials_into_sweeps(radials);
        assert_eq!(sweeps.len(), 2);
        assert!(sweeps[0].complete);
        assert!(!sweeps[1].complete, "trailing force-emitted sweep");
    }

    #[test]
    fn leading_sweep_without_start_marker_is_incomplete() {
        // Stream joined mid-rotation (I-chunks-only case): first
        // radial is intermediate, yet the sweep ends with a marker.
        let radials = vec![radial(100, 1, 1), radial(101, 1, 1), radial(102, 1, 2)];
        let sweeps = group_radials_into_sweeps(radials);
        assert_eq!(sweeps.len(), 1);
        assert!(!sweeps[0].complete);
    }

    #[test]
    fn interior_ray_gap_marks_sweep_incomplete_despite_markers() {
        // A chunk list missing a middle `I` chunk: start + end markers
        // are both present but the rotation is short. spacing 2 →
        // nominal 360 rays; only 102 arrive.
        let mut radials = vec![radial(1, 1, 0)];
        radials.extend((2..=100).map(|i| radial(i, 1, 1)));
        radials.extend((301..=360).map(|i| radial(i, 1, 1)));
        radials.push(radial(361, 1, 2));
        for r in &mut radials {
            r.azimuth_resolution_spacing = 2;
        }
        let sweeps = group_radials_into_sweeps(radials);
        assert_eq!(sweeps.len(), 1);
        assert!(!sweeps[0].complete);
    }

    #[test]
    fn nominal_ray_count_satisfied_keeps_sweep_complete() {
        // Full 1.0°-spacing rotation: 360 rays bracketed by markers.
        let mut radials: Vec<Radial> = (1..=360)
            .map(|i| {
                radial(
                    i,
                    1,
                    if i == 1 {
                        0
                    } else if i == 360 {
                        2
                    } else {
                        1
                    },
                )
            })
            .collect();
        for r in &mut radials {
            r.azimuth_resolution_spacing = 2;
        }
        let sweeps = group_radials_into_sweeps(radials);
        assert_eq!(sweeps.len(), 1);
        assert_eq!(sweeps[0].radials.len(), 360);
        assert!(sweeps[0].complete);
    }

    /// Invariant pin for the grouping rewrite: whatever the
    /// status/elevation sequence, grouping never panics, never loses
    /// or duplicates a radial, and every emitted sweep is non-empty.
    #[test]
    fn proptest_grouping_preserves_radials_on_arbitrary_sequences() {
        use proptest::prelude::*;
        proptest!(|(seq in prop::collection::vec((0u8..=7, 1u8..=4), 0..64))| {
            let radials: Vec<Radial> = seq
                .iter()
                .enumerate()
                .map(|(i, &(status, elev))| radial(i as u16 + 1, elev, status))
                .collect();
            let n_in = radials.len();
            let sweeps = group_radials_into_sweeps(radials);
            let n_out: usize = sweeps.iter().map(|s| s.radials.len()).sum();
            prop_assert_eq!(n_in, n_out);
            prop_assert!(sweeps.iter().all(|s| !s.radials.is_empty()));
        });
    }

    #[test]
    fn trailing_truncation_marks_only_last_sweep_incomplete() {
        // Two sweeps; the stream dies mid-way through the second —
        // the partial-chunk-list shape observed on the real KLOT
        // fixture (sweep 1 of the S+10I subset has 480/720 rays).
        let radials = vec![
            radial(1, 1, 0),
            radial(2, 1, 1),
            radial(3, 1, 2),
            radial(1, 2, 0),
            radial(2, 2, 1),
            radial(3, 2, 1),
        ];
        let sweeps = group_radials_into_sweeps(radials);
        assert_eq!(sweeps.len(), 2);
        assert!(sweeps[0].complete);
        assert!(!sweeps[1].complete);
    }

    #[test]
    fn sweep_elevation_angle_is_median_of_radials() {
        let mut radials = vec![radial(1, 1, 1), radial(2, 1, 1), radial(3, 1, 1)];
        // Override per-radial elevation_angle_degrees.
        radials[0].elevation_angle_degrees = 0.4;
        radials[1].elevation_angle_degrees = 0.5;
        radials[2].elevation_angle_degrees = 0.6;
        let sweep = Sweep {
            elevation_number: 1,
            radials,
            complete: true,
        };
        let med = sweep.elevation_angle_degrees().expect("non-empty");
        assert!((med - 0.5).abs() < 1e-6, "got {med}");
    }

    #[test]
    fn empty_sweep_elevation_angle_returns_none() {
        let sweep: Sweep = Sweep {
            elevation_number: 1,
            radials: vec![],
            complete: true,
        };
        assert!(sweep.elevation_angle_degrees().is_none());
    }

    /// Build a minimal `DataHeader` carrying just the date+time fields
    /// needed by `msg31_collection_time` — every other field is zero
    /// or default. Used by the timestamp regression tests below.
    fn header_with_date(modified_julian_date: u16, collection_time_ms: u32) -> DataHeader {
        use super::super::messages::msg31::header::PointerLayout;
        DataHeader {
            radar_identifier: *b"KLOT",
            collection_time_ms,
            modified_julian_date,
            azimuth_number: 1,
            azimuth_angle_degrees: 0.0,
            compression_indicator: 0,
            radial_length: 0,
            azimuth_resolution_spacing: 2,
            radial_status: 0,
            elevation_number: 1,
            cut_sector_number: 0,
            elevation_angle_degrees: 0.0,
            radial_spot_blanking_status: 0,
            azimuth_indexing_mode: 0,
            data_block_count: 0,
            pointers: [0; 10],
            layout: PointerLayout::Modern,
        }
    }

    /// ICD §3.2.4.17: `modified_julian_date` is **1-indexed days
    /// since 1970-01-01**, so day 1 with `collection_time_ms=0` must
    /// decode to exactly the Unix epoch. Catches off-by-one in either
    /// direction (no `-1` → +1 day; double-`-1` → -1 day).
    #[test]
    fn msg31_collection_time_day_1_decodes_to_unix_epoch() {
        let h = header_with_date(1, 0);
        let dt = msg31_collection_time(&h).expect("decoded");
        assert_eq!(dt.timestamp(), 0);
        assert_eq!(dt.timestamp_subsec_nanos(), 0);
    }

    /// Property test: for any valid (modified_julian_date, ms),
    /// decoded date equals `1970-01-01 + (jd - 1) days`. Catches
    /// future "fixes" that re-introduce off-by-one in a different
    /// direction (`-2`, `+1`, dropping the `-1` again, etc.) by
    /// testing the spec relationship rather than a single fixture.
    #[test]
    fn msg31_collection_time_property_jd_to_date_relationship() {
        use proptest::prelude::*;
        proptest!(|(jd in 1u16..50_000, ms in 0u32..86_400_000)| {
            let h = header_with_date(jd, ms);
            let dt = msg31_collection_time(&h).expect("decoded");
            let expected = chrono::NaiveDate::from_ymd_opt(1970, 1, 1)
                .unwrap()
                .checked_add_days(chrono::Days::new(u64::from(jd) - 1))
                .unwrap();
            prop_assert_eq!(dt.date_naive(), expected);
        });
    }

    /// Pin the absolute date for the bug-report's KLOT 2025-12-13
    /// fixture. Pre-fix this would have decoded to 2025-12-14 (off
    /// by +86,400 s); post-fix it matches the V06 filename truth and
    /// xradar's reference implementation.
    #[test]
    fn msg31_collection_time_klot_2025_12_13_fixture_pins_filename_truth() {
        let h = header_with_date(20_436, 64_872_367);
        let dt = msg31_collection_time(&h).expect("decoded");
        // 2025-12-13T18:01:12.367Z → unix ms 1_765_648_872_367.
        assert_eq!(dt.timestamp_millis(), 1_765_648_872_367);
        let expected = chrono::NaiveDate::from_ymd_opt(2025, 12, 13).unwrap();
        assert_eq!(dt.date_naive(), expected);
    }
}
