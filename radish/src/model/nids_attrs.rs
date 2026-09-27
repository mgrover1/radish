//! NEXRAD Level 3 (NIDS) sweep attrs, read from the product description
//! block (PDB) and the text header preceding it.
//!
//! Deliberately thin compared to [`super::NexradVolumeAttrs`] /
//! [`super::NexradSweepAttrs`] — a NIDS product is a single already-scanned
//! tilt of a single moment, not a volume coverage pattern radish has to
//! reconstruct. What's here is what the decoder needs to hand back to a
//! caller that wants to know *which* NIDS product this sweep came from,
//! not a re-derivation of anything already in [`super::SweepMetadata`] /
//! [`super::VolumeMetadata`]'s generic fields (site lat/lon/height, scan
//! time, elevation — all already covered).

use serde::{Deserialize, Serialize};

/// Per-sweep NEXRAD Level 3 attrs. `None` on [`super::SweepMetadata::nids`]
/// for every non-NIDS sweep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NidsSweepAttrs {
    /// 3-character NIDS site id as it appears in the AWIPS id (e.g.
    /// `"LOT"`) — **not** the 4-character ICAO id
    /// ([`super::VolumeMetadata::site_name`] carries that).
    pub site: String,

    /// The 3-character AWIPS product token (e.g. `"N0B"`), read from the
    /// text header. Since Phase 3 this is metadata only — moment
    /// resolution comes from the message code
    /// (`backends::nexrad_level3::decode::products::spec_for`), because a
    /// message code identifies the *format* and every file with that code
    /// carries the same moment regardless of tilt (see
    /// `docs/NEXRAD_LEVEL3_WASM.md` §4.4).
    pub awips_id: String,

    /// MHB message code (e.g. 153 for reflectivity, 154/99 for the two
    /// velocity halves).
    pub message_code: u16,

    /// Volume Coverage Pattern number, PDB halfword 9.
    pub vcp: u16,

    /// 0-5 tilt ordinal, resolved from `awips_id` against the small,
    /// S3-verified subset of products with a known AWIPS-letter table
    /// (`decode::products::tilt_letter_lookup`). `None` for every other
    /// product — deliberately not guessed from elevation angle; see
    /// `decode::products`'s module doc. The real elevation angle is
    /// always available regardless, as [`super::SweepMetadata::fixed_angle`].
    pub tilt: Option<u8>,
}
