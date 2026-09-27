//! Tiny checked big-endian read helpers shared across this backend's
//! decode submodules. `Option`-returning, never panics on a short buffer —
//! matching this crate's no-panics-on-untrusted-input discipline (see
//! `docs/NEXRAD_LEVEL3_WASM.md` §4.7).

pub(crate) fn read_i16_be(buf: &[u8], off: usize) -> Option<i16> {
    buf.get(off..off + 2)
        .map(|b| i16::from_be_bytes([b[0], b[1]]))
}

pub(crate) fn read_u16_be(buf: &[u8], off: usize) -> Option<u16> {
    buf.get(off..off + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
}

pub(crate) fn read_i32_be(buf: &[u8], off: usize) -> Option<i32> {
    buf.get(off..off + 4)
        .map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}
