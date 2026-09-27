//! Mapping from IRIS data-type bytes to ODIM moment names + per-byte
//! decoder selection.
//!
//! Source: xradar's `iris_mapping` (`xradar/io/backends/iris.py:82`) and
//! `SIGMET_DATA_TYPES` (line 1755+). Every IRIS data-type id in
//! `iris_mapping` shows up here under the same ODIM short name xradar
//! emits, with the same calibration formula. Ids that aren't in
//! `iris_mapping` (`DB_HCLASS`, `DB_DBTE8`, `DB_DBZE8`, …) get emitted
//! under their IRIS short name with no ODIM translation, again
//! mirroring xradar.
//!
//! CF metadata strings (units / standard_name / long_name) for the
//! ODIM-mapped moments live in `crate::backends::common::metadata`.
//! Unmapped moments emit minimal CF attrs (units only, derived from
//! the calibration helper) since there's no ODIM standard name for
//! them.

use crate::backends::common::{meta_for, OdimMomentMeta};

use super::calibration::{
    Decoder, DECODE_DBZ_2BYTE, DECODE_DBZ_8BIT, DECODE_KDP_2BYTE, DECODE_KDP_8BIT, DECODE_NONE,
    DECODE_PHIDP_2BYTE, DECODE_PHIDP_8BIT, DECODE_RHOHV_2BYTE, DECODE_RHOHV_8BIT, DECODE_SNR_2BYTE,
    DECODE_SQI_2BYTE, DECODE_VEL_2BYTE, DECODE_VEL_8BIT, DECODE_WIDTH_2BYTE, DECODE_WIDTH_8BIT,
    DECODE_ZDR_2BYTE, DECODE_ZDR_8BIT,
};

/// How a Sigmet/IRIS data type surfaces in the resulting xarray Dataset.
///
/// `Odim` types appear under their ODIM short name (`"DBZH"`, `"VRADH"`,
/// …) and pull full CF metadata (units / standard_name / long_name)
/// from [`crate::backends::common::metadata::TABLE`]. `Iris` types
/// (DB_HCLASS, DB_DBTE8, DB_DBZE8 — anything not in xradar's
/// `iris_mapping`) appear under their IRIS short name and emit only a
/// `units` string; xradar uses the same convention so the Dataset
/// shapes line up.
///
/// The enum exists to keep the two paths *type*-distinct rather than
/// data-distinct: a previous draft used `units: ""` as an implicit
/// "is ODIM-mapped" flag, which made `cf_metadata_for(m).is_some()` the
/// disambiguator and exposed a class of bugs where a typo in
/// `output_name` would silently fall through to the IRIS path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MomentMapping {
    /// xradar's `iris_mapping` translates this IRIS data type to an ODIM
    /// short name. CF metadata comes from
    /// [`crate::backends::common::metadata::TABLE`].
    Odim,
    /// Type isn't in xradar's `iris_mapping`; emit under its IRIS short
    /// name with the carried `units` as the only CF attr. (Mirrors what
    /// xradar does for the same set: pass through under the IRIS name.)
    Iris {
        /// Units string, e.g. `"dBZ"` for DB_DBZE8 or `"1"`
        /// (dimensionless) for DB_HCLASS.
        units: &'static str,
    },
}

/// One entry in the IRIS data-type table.
///
/// `data_type_id` is the bit position in `dsp_data_mask` and the byte
/// the per-ray header records. `bytes_per_bin` is 1 (most legacy types)
/// or 2 (modern double-precision variants).
///
/// `output_name` is what we emit as the variable name in the resulting
/// xarray Dataset. For `MomentMapping::Odim` types it's the ODIM short
/// name; for `MomentMapping::Iris` types it's the IRIS short name.
#[allow(dead_code)]
pub(super) struct SigmetMoment {
    /// IRIS data-type id (e.g. 2 = DB_DBZ, 9 = DB_DBZ2).
    pub(super) data_type_id: u8,
    /// Source name from the IRIS spec (e.g. `"DB_DBZ"`).
    pub(super) iris_name: &'static str,
    /// Variable name to emit in the xarray Dataset.
    pub(super) output_name: &'static str,
    /// Number of raw bytes per gate (1 or 2).
    pub(super) bytes_per_bin: u8,
    /// Decoder applied to each gate's raw value. The no-data / masking
    /// policy is per-type and lives in the decoder body — see the
    /// `calibration.rs` module docs. Most measurement moments are
    /// **unmasked** (a raw `0` decodes to a finite value); only velocity,
    /// KDP, RHOHV/SQI sentinels, and the categorical `DECODE_NONE` fields
    /// yield `NaN`.
    pub(super) decoder: Decoder,
    /// How this moment surfaces as a Dataset variable — see
    /// [`MomentMapping`]. Drives the CF-metadata lookup in the adapter.
    pub(super) mapping: MomentMapping,
}

/// Full mapping table.
///
/// **IDs verified against `xradar.io.backends.iris.SIGMET_DATA_TYPES`**
/// (a previous draft had five IDs swapped: 17 was DB_PHIDP2 instead of
/// DB_VELC, 19 was DB_SQI instead of DB_RHOHV, 20 was DB_RHOHV instead
/// of DB_RHOHV2, 21 was DB_SNR16 instead of DB_DBZC2, 22 was DB_RHOHV2
/// instead of DB_VELC2 — see commit history). Re-syncing the IDs is
/// the only reliable way to keep parity with xradar across IRIS
/// fixtures, since the IRIS ICD numbers data types densely and a
/// single off-by-one silently mis-decodes whole moments. The
/// `iris_mapping_ids_match_xradar_table` test pins all 19 ODIM-mapped
/// IDs against xradar so a future regression fails fast.
pub(super) const SUPPORTED_MOMENTS: &[SigmetMoment] = &[
    // 8-bit legacy variants ------------------------------------------------
    SigmetMoment {
        data_type_id: 1,
        iris_name: "DB_DBT",
        output_name: "DBTH",
        bytes_per_bin: 1,
        decoder: DECODE_DBZ_8BIT,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 2,
        iris_name: "DB_DBZ",
        output_name: "DBZH",
        bytes_per_bin: 1,
        decoder: DECODE_DBZ_8BIT,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 3,
        iris_name: "DB_VEL",
        output_name: "VRADH",
        bytes_per_bin: 1,
        decoder: DECODE_VEL_8BIT,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 4,
        iris_name: "DB_WIDTH",
        output_name: "WRADH",
        bytes_per_bin: 1,
        decoder: DECODE_WIDTH_8BIT,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 5,
        iris_name: "DB_ZDR",
        output_name: "ZDR",
        bytes_per_bin: 1,
        decoder: DECODE_ZDR_8BIT,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 14,
        iris_name: "DB_KDP",
        output_name: "KDP",
        bytes_per_bin: 1,
        decoder: DECODE_KDP_8BIT,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 16,
        iris_name: "DB_PHIDP",
        output_name: "PHIDP",
        bytes_per_bin: 1,
        decoder: DECODE_PHIDP_8BIT,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 18,
        iris_name: "DB_SQI",
        output_name: "SQIH",
        bytes_per_bin: 1,
        decoder: DECODE_RHOHV_8BIT,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 19,
        iris_name: "DB_RHOHV",
        output_name: "RHOHV",
        bytes_per_bin: 1,
        decoder: DECODE_RHOHV_8BIT,
        mapping: MomentMapping::Odim,
    },
    // 16-bit modern variants -----------------------------------------------
    SigmetMoment {
        data_type_id: 8,
        iris_name: "DB_DBT2",
        output_name: "DBTH",
        bytes_per_bin: 2,
        decoder: DECODE_DBZ_2BYTE,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 9,
        iris_name: "DB_DBZ2",
        output_name: "DBZH",
        bytes_per_bin: 2,
        decoder: DECODE_DBZ_2BYTE,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 10,
        iris_name: "DB_VEL2",
        output_name: "VRADH",
        bytes_per_bin: 2,
        decoder: DECODE_VEL_2BYTE,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 11,
        iris_name: "DB_WIDTH2",
        output_name: "WRADH",
        bytes_per_bin: 2,
        decoder: DECODE_WIDTH_2BYTE,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 12,
        iris_name: "DB_ZDR2",
        output_name: "ZDR",
        bytes_per_bin: 2,
        decoder: DECODE_ZDR_2BYTE,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 15,
        iris_name: "DB_KDP2",
        output_name: "KDP",
        bytes_per_bin: 2,
        decoder: DECODE_KDP_2BYTE,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 20,
        iris_name: "DB_RHOHV2",
        output_name: "RHOHV",
        bytes_per_bin: 2,
        decoder: DECODE_RHOHV_2BYTE,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 23,
        iris_name: "DB_SQI2",
        output_name: "SQIH",
        bytes_per_bin: 2,
        decoder: DECODE_SQI_2BYTE,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 24,
        iris_name: "DB_PHIDP2",
        output_name: "PHIDP",
        bytes_per_bin: 2,
        decoder: DECODE_PHIDP_2BYTE,
        mapping: MomentMapping::Odim,
    },
    SigmetMoment {
        data_type_id: 66,
        iris_name: "DB_SNR16",
        output_name: "SNRH",
        bytes_per_bin: 2,
        decoder: DECODE_SNR_2BYTE,
        mapping: MomentMapping::Odim,
    },
    // Iris-passthrough (no ODIM short name in xradar's `iris_mapping`)
    // — emit under the IRIS short name. xradar leaves `units` blank
    // for these (it has no canonical units mapping outside
    // `iris_mapping`); we follow the same convention so per-moment
    // attrs match. The calibration formula still matches xradar's
    // `SIGMET_DATA_TYPES` decode_array fkw entries, so values agree.
    SigmetMoment {
        data_type_id: 55,
        iris_name: "DB_HCLASS",
        output_name: "DB_HCLASS",
        bytes_per_bin: 1,
        decoder: DECODE_NONE,
        mapping: MomentMapping::Iris { units: "" },
    },
    SigmetMoment {
        data_type_id: 71,
        iris_name: "DB_DBTE8",
        output_name: "DB_DBTE8",
        bytes_per_bin: 1,
        decoder: DECODE_DBZ_8BIT,
        mapping: MomentMapping::Iris { units: "" },
    },
    SigmetMoment {
        data_type_id: 73,
        iris_name: "DB_DBZE8",
        output_name: "DB_DBZE8",
        bytes_per_bin: 1,
        decoder: DECODE_DBZ_8BIT,
        mapping: MomentMapping::Iris { units: "" },
    },
];

/// Look up a moment definition by its IRIS data-type id. Returns `None`
/// for unknown / unsupported ids — adapter callers skip those rather
/// than failing the whole decode.
pub(super) fn moment_for_id(data_type_id: u8) -> Option<&'static SigmetMoment> {
    SUPPORTED_MOMENTS
        .iter()
        .find(|m| m.data_type_id == data_type_id)
}

/// Look up CF metadata for an ODIM-mapped moment. Returns `None` for
/// `MomentMapping::Iris` types — those carry their own units inline
/// and skip the central metadata table.
///
/// **Invariant**: for `MomentMapping::Odim` moments this returns
/// `Some(_)` (the corresponding row in
/// [`crate::backends::common::metadata::TABLE`] must exist; the
/// `cf_metadata_present_for_every_odim_mapped_moment` test pins this).
pub(super) fn cf_metadata_for(moment: &SigmetMoment) -> Option<&'static OdimMomentMeta> {
    match moment.mapping {
        MomentMapping::Odim => meta_for(moment.output_name),
        MomentMapping::Iris { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Authoritative reference: every entry from xradar's `iris_mapping`
    /// (id, IRIS short name, ODIM short name). Pinning all 19 IDs in one
    /// place catches the class of bug where the IRIS ICD's dense
    /// numbering causes a one-off swap to silently mis-decode a moment.
    /// If radish is built against a future xradar that adds entries to
    /// `iris_mapping`, this test surfaces the diff explicitly.
    const XRADAR_IRIS_MAPPING_REFERENCE: &[(u8, &str, &str)] = &[
        // (id, iris_name, odim_short_name)
        (1, "DB_DBT", "DBTH"),
        (2, "DB_DBZ", "DBZH"),
        (3, "DB_VEL", "VRADH"),
        (4, "DB_WIDTH", "WRADH"),
        (5, "DB_ZDR", "ZDR"),
        (8, "DB_DBT2", "DBTH"),
        (9, "DB_DBZ2", "DBZH"),
        (10, "DB_VEL2", "VRADH"),
        (11, "DB_WIDTH2", "WRADH"),
        (12, "DB_ZDR2", "ZDR"),
        (14, "DB_KDP", "KDP"),
        (15, "DB_KDP2", "KDP"),
        (16, "DB_PHIDP", "PHIDP"),
        (18, "DB_SQI", "SQIH"),
        (19, "DB_RHOHV", "RHOHV"),
        (20, "DB_RHOHV2", "RHOHV"),
        (23, "DB_SQI2", "SQIH"),
        (24, "DB_PHIDP2", "PHIDP"),
        (66, "DB_SNR16", "SNRH"),
    ];

    /// IRIS-passthrough types (xradar surfaces them under their IRIS
    /// short name without ODIM translation). xradar leaves `units`
    /// blank for these; we match. Pinned explicitly so a future ODIM
    /// expansion that renames any of them or sets a units string
    /// fails loudly.
    const IRIS_PASSTHROUGH_REFERENCE: &[(u8, &str, &str)] = &[
        // (id, iris_name == output_name, units — empty for parity)
        (55, "DB_HCLASS", ""),
        (71, "DB_DBTE8", ""),
        (73, "DB_DBZE8", ""),
    ];

    #[test]
    fn dbz_maps_to_dbzh() {
        let m = moment_for_id(2).expect("DB_DBZ is supported");
        assert_eq!(m.iris_name, "DB_DBZ");
        assert_eq!(m.output_name, "DBZH");
        assert_eq!(m.bytes_per_bin, 1);
        assert_eq!(m.mapping, MomentMapping::Odim);
    }

    #[test]
    fn dbz2_and_dbz_share_output_name() {
        let m1 = moment_for_id(2).unwrap();
        let m2 = moment_for_id(9).unwrap();
        assert_eq!(m1.output_name, m2.output_name);
        assert_eq!(m1.bytes_per_bin, 1);
        assert_eq!(m2.bytes_per_bin, 2);
    }

    /// Every entry in xradar's `iris_mapping` must be present in
    /// SUPPORTED_MOMENTS at the right id with the right output_name.
    /// Catches the historic five-ID-swap bug (and any future variant).
    #[test]
    fn iris_mapping_ids_match_xradar_table() {
        for (id, iris_name, odim_name) in XRADAR_IRIS_MAPPING_REFERENCE {
            let m = moment_for_id(*id)
                .unwrap_or_else(|| panic!("missing IRIS id {id} ({iris_name} → {odim_name})"));
            assert_eq!(
                m.iris_name, *iris_name,
                "id {id}: expected iris_name {iris_name:?}, got {:?}",
                m.iris_name
            );
            assert_eq!(
                m.output_name, *odim_name,
                "id {id} ({iris_name}): expected output_name {odim_name:?}, got {:?}",
                m.output_name
            );
            assert_eq!(
                m.mapping,
                MomentMapping::Odim,
                "id {id} ({iris_name}) must be MomentMapping::Odim"
            );
        }
    }

    /// The IRIS-passthrough types (no ODIM equivalent) emit under their
    /// IRIS short name with the right units. Pinning all three keeps a
    /// future contributor from accidentally renaming them or dropping
    /// the units string.
    #[test]
    fn iris_passthrough_types_present_with_units() {
        for (id, iris_name, units) in IRIS_PASSTHROUGH_REFERENCE {
            let m = moment_for_id(*id)
                .unwrap_or_else(|| panic!("missing IRIS-passthrough id {id} ({iris_name})"));
            assert_eq!(m.iris_name, *iris_name, "id {id}: iris_name");
            assert_eq!(
                m.output_name, *iris_name,
                "id {id}: output_name should equal iris_name for passthrough"
            );
            match m.mapping {
                MomentMapping::Iris { units: u } => assert_eq!(
                    u, *units,
                    "id {id} ({iris_name}): expected units {units:?}, got {u:?}"
                ),
                MomentMapping::Odim => {
                    panic!("id {id} ({iris_name}) must be MomentMapping::Iris, got Odim")
                }
            }
        }
    }

    /// Every `MomentMapping::Odim` row must resolve to a CF metadata
    /// entry, and every `MomentMapping::Iris` row must NOT (it carries
    /// its units inline). This pins the invariant on the `cf_metadata_for`
    /// match arm so a future change can't accidentally route an ODIM
    /// row through the no-metadata path or vice versa.
    #[test]
    fn cf_metadata_present_for_every_odim_mapped_moment() {
        for m in SUPPORTED_MOMENTS {
            match m.mapping {
                MomentMapping::Odim => assert!(
                    cf_metadata_for(m).is_some(),
                    "no CF metadata for ODIM moment {:?} (IRIS {})",
                    m.output_name,
                    m.iris_name,
                ),
                MomentMapping::Iris { .. } => assert!(
                    cf_metadata_for(m).is_none(),
                    "Iris-mapping moment {} should not have CF metadata",
                    m.iris_name,
                ),
            }
        }
    }

    /// **Wiring invariant.** No `MomentMapping::Odim` (measurement)
    /// moment may be wired to `DECODE_NONE`, the raw-count passthrough
    /// reserved for the categorical `DB_HCLASS`. This is the guard that
    /// actually catches the SQI2/SNR16 class of bug — those were live
    /// ODIM moments left on `DECODE_NONE`, a *wiring* defect the
    /// decoder-level mask-policy test in `calibration.rs` is blind to.
    /// One-directional (Odim ⇒ not passthrough): `Iris` rows like
    /// `DB_DBTE8` legitimately use a real decoder, and `DB_HCLASS` is the
    /// only `DECODE_NONE` user, so it holds for the current table.
    #[test]
    fn no_odim_measurement_moment_routes_to_passthrough() {
        for m in SUPPORTED_MOMENTS {
            if matches!(m.mapping, MomentMapping::Odim) {
                assert!(
                    !std::ptr::fn_addr_eq(m.decoder, DECODE_NONE),
                    "ODIM measurement moment {} (id {}) is wired to \
                     DECODE_NONE (raw-count passthrough) — it must use a \
                     real decoder",
                    m.iris_name,
                    m.data_type_id,
                );
            }
        }
    }

    #[test]
    fn unknown_iris_id_returns_none() {
        assert!(moment_for_id(99).is_none());
    }

    /// SUPPORTED_MOMENTS has exactly 22 entries: 19 ODIM-mapped + 3
    /// IRIS-passthrough. If a future contributor adds a row, the
    /// counts here must be updated alongside — failing fast forces
    /// them to think about which mapping kind the new row belongs in.
    #[test]
    fn supported_moments_table_size_invariant() {
        let n_odim = SUPPORTED_MOMENTS
            .iter()
            .filter(|m| matches!(m.mapping, MomentMapping::Odim))
            .count();
        let n_iris = SUPPORTED_MOMENTS
            .iter()
            .filter(|m| matches!(m.mapping, MomentMapping::Iris { .. }))
            .count();
        assert_eq!(n_odim, XRADAR_IRIS_MAPPING_REFERENCE.len());
        assert_eq!(n_iris, IRIS_PASSTHROUGH_REFERENCE.len());
        assert_eq!(SUPPORTED_MOMENTS.len(), n_odim + n_iris);
    }
}
