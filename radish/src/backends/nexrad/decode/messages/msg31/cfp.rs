//! Clutter Filter Power (CFP) data block — ICD §3.2.4.17.4
//! Table XVII-Q.
//!
//! The CFP block uses the same 24-byte generic descriptor as the
//! six measurement moments (REF / VEL / SW / ZDR / PHI / RHO), but
//! its raw gate values mean different things:
//!
//! | Raw byte | Meaning                                                 |
//! |---------:|---------------------------------------------------------|
//! |        0 | Filter not applied                                      |
//! |        1 | Point clutter filter applied                            |
//! |        2 | Censor pulses applied                                   |
//! |       ≥3 | Power level — physical = `(raw - offset) / scale` (dB)  |
//!
//! In other words, CFP gates are *power values combined with a
//! status flag in the low values*. We expose decoded gates as a
//! `CfpValue` enum so consumers can branch on the four cases.

use crate::backends::nexrad::decode::error::Result;
use crate::backends::nexrad::decode::reader::SliceReader;

use super::moment::{MomentBlock, MomentDescriptor};

/// One CFP gate. Reuses the moment descriptor's scale/offset for
/// values ≥ 3.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CfpValue {
    /// Raw byte 0 — filter not applied.
    FilterNotApplied,
    /// Raw byte 1 — point clutter filter applied.
    PointClutterFilterApplied,
    /// Raw byte 2 — censor pulses applied.
    CensorPulsesApplied,
    /// Raw byte ≥ 3 — physical power level in dB.
    PowerDb(f32),
}

/// Decoded CFP block: same byte layout as a moment block but with
/// CFP-specific value semantics.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CfpBlock<'a> {
    pub(crate) descriptor: MomentDescriptor,
    pub(crate) gate_bytes: &'a [u8],
}

impl<'a> CfpBlock<'a> {
    pub(crate) fn read(reader: &mut SliceReader<'a>) -> Result<Self> {
        let inner = MomentBlock::read(reader)?;
        Ok(Self {
            descriptor: inner.descriptor,
            gate_bytes: inner.gate_bytes,
        })
    }

    /// Iterate decoded CFP samples per ICD Table XVII-Q.
    pub(crate) fn iter(&self) -> CfpIter<'a> {
        CfpIter {
            bytes: self.gate_bytes,
            scale: self.descriptor.scale,
            offset: self.descriptor.offset,
            word_size_bytes: self.descriptor.word_size_bytes(),
            pos: 0,
        }
    }
}

pub(crate) struct CfpIter<'a> {
    bytes: &'a [u8],
    scale: f32,
    offset: f32,
    word_size_bytes: usize,
    pos: usize,
}

impl<'a> Iterator for CfpIter<'a> {
    type Item = CfpValue;
    fn next(&mut self) -> Option<CfpValue> {
        if self.pos + self.word_size_bytes > self.bytes.len() {
            return None;
        }
        let raw: u32 = match self.word_size_bytes {
            1 => u32::from(self.bytes[self.pos]),
            2 => {
                let hi = u32::from(self.bytes[self.pos]);
                let lo = u32::from(self.bytes[self.pos + 1]);
                (hi << 8) | lo
            }
            _ => unreachable!("MomentDescriptor::read rejects word sizes other than 8 or 16"),
        };
        self.pos += self.word_size_bytes;
        Some(match raw {
            0 => CfpValue::FilterNotApplied,
            1 => CfpValue::PointClutterFilterApplied,
            2 => CfpValue::CensorPulsesApplied,
            n => CfpValue::PowerDb((n as f32 - self.offset) / self.scale),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfp_bytes(gates: &[u8]) -> Vec<u8> {
        // 24-byte descriptor: word_size = 8, scale = 2.0, offset = 0.
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&(gates.len() as u16).to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes()); // range
        buf.extend_from_slice(&250u16.to_be_bytes()); // interval
        buf.extend_from_slice(&0u16.to_be_bytes()); // tover
        buf.extend_from_slice(&0_i16.to_be_bytes()); // snr
        buf.push(0); // ctrl flags
        buf.push(8); // 8-bit
        buf.extend_from_slice(&2.0_f32.to_be_bytes()); // scale
        buf.extend_from_slice(&0.0_f32.to_be_bytes()); // offset
        buf.extend_from_slice(gates);
        buf
    }

    #[test]
    fn cfp_decodes_status_codes_and_power_values() {
        // raw = 0 / 1 / 2 / 4 / 10 → status / status / status /
        // PowerDb((4-0)/2)=2.0 / PowerDb((10-0)/2)=5.0
        let bytes = cfp_bytes(&[0, 1, 2, 4, 10]);
        let mut r = SliceReader::new(&bytes);
        let block = CfpBlock::read(&mut r).unwrap();
        let values: Vec<CfpValue> = block.iter().collect();
        assert_eq!(values.len(), 5);
        assert_eq!(values[0], CfpValue::FilterNotApplied);
        assert_eq!(values[1], CfpValue::PointClutterFilterApplied);
        assert_eq!(values[2], CfpValue::CensorPulsesApplied);
        match values[3] {
            CfpValue::PowerDb(v) => assert!((v - 2.0).abs() < 1e-6),
            _ => panic!("expected PowerDb, got {:?}", values[3]),
        }
        match values[4] {
            CfpValue::PowerDb(v) => assert!((v - 5.0).abs() < 1e-6),
            _ => panic!("expected PowerDb, got {:?}", values[4]),
        }
    }
}
