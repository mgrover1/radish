//! Format detection for NEXRAD Level 3 (NIDS) products.
//!
//! NIDS files have no reliable extension and no fixed-offset magic bytes —
//! detection is content-based. Since Phase 3, [`super::decode::find_awips_token`]
//! accepts ANY well-formed 6-character token (moment resolution moved to
//! the message code, not the AWIPS letter — see `decode::products`'s
//! module doc), so a token match alone is a weaker signal than Phase 2's
//! letter-gated one. Sniffing compensates by ALSO requiring
//! [`super::decode::find_message_header`] to validate within the same
//! peeked bytes — both together are the same "validate, don't just
//! pattern-match" property Phase 2 got from the letter table, just spread
//! across two checks instead of one.

use std::path::Path;

use crate::backends::common::SniffConfig;

use super::decode::{find_awips_token, find_message_header};

/// `true` if `head` has both a well-formed AWIPS token AND a message
/// header that validates within the same bytes. Wraps the real parsers
/// rather than a lighter-weight heuristic: sniffing runs once per file,
/// and a false positive here would route a non-NIDS buffer into a decoder
/// that just errors anyway — but a false *negative* would silently fall
/// through to "no backend found" for a perfectly good product, the worse
/// failure mode to guard against.
pub(crate) fn looks_like_nids_bytes(head: &[u8]) -> bool {
    find_awips_token(head).is_ok() && find_message_header(head).is_ok()
}

pub(crate) static SNIFF_CONFIG: SniffConfig = SniffConfig {
    extensions: &[],
    magic_prefixes: &[],
    filename_pattern: None,
    head_pattern: Some(looks_like_nids_bytes),
};

pub(crate) fn can_read(path: &Path) -> bool {
    crate::backends::common::looks_like(path, &SNIFF_CONFIG)
}

pub(crate) fn can_read_bytes(head: &[u8]) -> bool {
    crate::backends::common::looks_like_bytes(head, &SNIFF_CONFIG)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real message header needs a known code and a `-1` PDB divider 18
    /// bytes later — build one for message code 153 (reflectivity) so
    /// these tests exercise the same combined check production traffic
    /// does, not just the token half. Pads the prefix to 30 bytes first:
    /// `find_message_header` only scans offsets `0..raw.len().min(128)-20`,
    /// so the message header must start early enough in the buffer for the
    /// scan to reach it.
    fn with_message_header(prefix: &[u8]) -> Vec<u8> {
        let mut buf = prefix.to_vec();
        while buf.len() < 30 {
            buf.push(b' ');
        }
        buf.extend_from_slice(&153i16.to_be_bytes());
        buf.extend_from_slice(&[0u8; 16]); // rest of the 18-byte MHB
        buf.extend_from_slice(&(-1i16).to_be_bytes()); // PDB divider
                                                       // A few bytes of trailing slack: `find_message_header`'s scan
                                                       // window is `0..min(len,128)-20` (exclusive, matching the Python
                                                       // oracle's `range()` exactly), so a header sitting in the buffer's
                                                       // final 20 bytes with nothing after it falls just outside the
                                                       // window — real files always have PDB bytes following, this just
                                                       // stands in for them.
        buf.extend_from_slice(&[0u8; 8]);
        buf
    }

    #[test]
    fn recognises_a_bare_awips_token_with_a_message_header() {
        assert!(looks_like_nids_bytes(&with_message_header(b"N0BLOT\r\r\n")));
    }

    #[test]
    fn recognises_a_token_behind_a_noaaport_prefix_line() {
        assert!(looks_like_nids_bytes(&with_message_header(
            b"SDUS53 KLOT 010000\r\r\nN0BLOT\r\r\n"
        )));
    }

    #[test]
    fn rejects_a_token_with_no_message_header() {
        // Phase 3 regression guard: a bare token is no longer sufficient
        // on its own, now that any well-formed token passes.
        assert!(!looks_like_nids_bytes(b"N0BLOT\r\r\n"));
    }

    #[test]
    fn rejects_unrelated_bytes() {
        assert!(!looks_like_nids_bytes(b"\x89HDF\r\n\x1a\n"));
        assert!(!looks_like_nids_bytes(b"AR2V0006.001"));
        assert!(!looks_like_nids_bytes(b""));
    }
}
