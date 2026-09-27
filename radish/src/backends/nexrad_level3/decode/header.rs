//! WMO/AWIPS text-header parsing and message-header location — the part of
//! the file before the binary Message Header Block (MHB). Mirrors
//! `nexrad_level3.py:285-364`.

use chrono::{DateTime, Utc};

use super::bytes::read_i16_be;
use super::error::{Level3DecodeError, Result};
use super::products::{is_known_message_code, known_message_codes};

const HEADER_SEP: &[u8] = b"\r\r\n";
const MAX_SCAN_SKEW_SECONDS: i64 = 86_400;

/// `(product_token, site)` — e.g. for the text-header token `N0BLOT`:
/// `("N0B", "LOT")`.
///
/// Extracts ANY well-formed 6-character alphanumeric token, with **no**
/// dependency on recognising the product letter — moment resolution is
/// message-code-driven (`products::spec_for`), not AWIPS-id-driven; this
/// only supplies the site string and (via `products::tilt_letter_lookup`)
/// the tilt ordinal for the subset of products with a verified letter
/// table. See `products.rs`'s module doc for why those are two separate
/// concerns. Scans the first 128 bytes, split on `\r\r\n`, and keeps
/// scanning past a malformed token: a NOAAPORT-style prefix line can put
/// one ahead of the real AWIPS id (`nexrad_level3.py:305-309`).
pub(crate) fn find_awips_token(raw: &[u8]) -> Result<(String, String)> {
    let head = &raw[..raw.len().min(128)];
    for part in split_on(head, HEADER_SEP) {
        let token = part.trim_ascii();
        if token.len() != 6 || !token.iter().all(u8::is_ascii_alphanumeric) {
            continue;
        }
        let (product, site) = token.split_at(3);
        // `token` was just validated all-ASCII above, so this is always
        // valid UTF-8 — `from_utf8` (not `_lossy`) makes that guarantee
        // visible at the call site instead of implying a possible (never
        // taken) replacement-character path.
        let to_string = |b: &[u8]| {
            std::str::from_utf8(b)
                .expect("ascii-validated above")
                .to_owned()
        };
        return Ok((to_string(product), to_string(site)));
    }
    Err(Level3DecodeError::NoAwipsToken)
}

/// Split `haystack` on every non-overlapping occurrence of `sep`, keeping
/// empty pieces — mirrors Python's `bytes.split`. `std::slice::split` only
/// splits on a predicate over single elements, so a multi-byte separator
/// needs this.
fn split_on<'a>(haystack: &'a [u8], sep: &[u8]) -> Vec<&'a [u8]> {
    if sep.is_empty() {
        return vec![haystack];
    }
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i + sep.len() <= haystack.len() {
        if &haystack[i..i + sep.len()] == sep {
            parts.push(&haystack[start..i]);
            i += sep.len();
            start = i;
        } else {
            i += 1;
        }
    }
    parts.push(&haystack[start..]);
    parts
}

/// Offset of the message header block (MHB).
///
/// Located by VALIDATING rather than searching for a byte pattern: at the
/// right offset the message code is one we know and the product
/// description block that follows opens with its `-1` divider. Searching
/// for the code as raw bytes would happily match the same pair occurring
/// inside the text header (`nexrad_level3.py:319-337`).
pub(crate) fn find_message_header(raw: &[u8]) -> Result<usize> {
    let scan_end = raw.len().min(128).saturating_sub(20);
    for off in 0..scan_end {
        let Some(code) = read_i16_be(raw, off) else {
            break;
        };
        if !is_known_message_code(code) {
            continue;
        }
        if read_i16_be(raw, off + 18) == Some(-1) {
            return Ok(off);
        }
    }
    Err(Level3DecodeError::NoMessageHeader {
        known: known_message_codes(),
    })
}

/// PDB date/time -> UTC. The date is days since 1970-01-01 with `1` =
/// epoch. Mirrors `nexrad_level3.py:340-364`.
///
/// `days` is read **unsigned** by the caller: the ICD declares it
/// `1..32767`, and a signed read wraps into the 1880s the moment the real
/// count passes 32767 — a corrupt object could then hand a caller a
/// timestamp 90 years in the past, defeating an "is this newer?" dedupe.
pub(crate) fn scan_time(days: u16, seconds: i32) -> Result<DateTime<Utc>> {
    if days < 1 || !(0..86_400).contains(&seconds) {
        return Err(Level3DecodeError::ImplausibleScanTime { days, seconds });
    }
    let epoch_seconds = (days as i64 - 1) * 86_400 + seconds as i64;
    let when = DateTime::<Utc>::from_timestamp(epoch_seconds, 0)
        .ok_or(Level3DecodeError::ImplausibleScanTime { days, seconds })?;
    // Only the FUTURE direction is bounded — a stale timestamp merely loses
    // a newest-first comparison, but one far in the future would
    // permanently win it. See the oracle's comment at
    // `nexrad_level3.py:355-360` for why the past stays unbounded.
    let ahead_seconds = (when - Utc::now()).num_seconds();
    if ahead_seconds > MAX_SCAN_SKEW_SECONDS {
        return Err(Level3DecodeError::ScanTimeTooFarAhead {
            days_ahead: ahead_seconds as f64 / 86_400.0,
        });
    }
    Ok(when)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_on_matches_python_bytes_split() {
        assert_eq!(
            split_on(b"a\r\r\nb\r\r\nc", b"\r\r\n"),
            vec![&b"a"[..], &b"b"[..], &b"c"[..]]
        );
        assert_eq!(
            split_on(b"noseparator", b"\r\r\n"),
            vec![&b"noseparator"[..]]
        );
        assert_eq!(split_on(b"\r\r\n", b"\r\r\n"), vec![&b""[..], &b""[..]]);
    }

    #[test]
    fn find_awips_token_finds_token_after_noaaport_prefix_line() {
        let raw = b"SDUS53 KLOT 010000\r\r\nN0BLOT\r\r\n";
        let (product, site) = find_awips_token(raw).unwrap();
        assert_eq!(product, "N0B");
        assert_eq!(site, "LOT");
    }

    #[test]
    fn find_awips_token_accepts_a_letter_with_no_verified_tilt_table() {
        // Unlike Phase 2, an unrecognised product letter (hydro class,
        // "N0H") is still a valid token now — moment resolution moved to
        // the message code, not this letter.
        let (product, site) = find_awips_token(b"N0HLOT\r\r\n").unwrap();
        assert_eq!(product, "N0H");
        assert_eq!(site, "LOT");
    }

    #[test]
    fn find_awips_token_errors_when_no_six_char_token_exists() {
        let raw = b"too short\r\r\nalso not six";
        assert!(matches!(
            find_awips_token(raw),
            Err(Level3DecodeError::NoAwipsToken)
        ));
    }

    #[test]
    fn scan_time_rejects_zero_days() {
        assert!(scan_time(0, 0).is_err());
    }

    #[test]
    fn scan_time_rejects_out_of_range_seconds() {
        assert!(scan_time(20000, 86_400).is_err());
        assert!(scan_time(20000, -1).is_err());
    }

    #[test]
    fn scan_time_decodes_a_plausible_recent_date() {
        let when = scan_time(20000, 3600).unwrap();
        assert_eq!(when.timestamp(), (20000i64 - 1) * 86_400 + 3600);
    }

    #[test]
    fn scan_time_rejects_far_future() {
        let far_future_days = (chrono::Utc::now().timestamp() / 86_400 + 1000) as u16;
        assert!(scan_time(far_future_days, 0).is_err());
    }
}
