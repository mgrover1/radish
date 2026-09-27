//! MSG_31 generic data moment block (ICD §3.2.4.17.2 Table XVII-B).
//!
//! One layout serves REF / VEL / SW / ZDR / PHI / RHO. CFP shares
//! the same descriptor but its values mean different things — see
//! `cfp.rs` for the value-decoding overlay.
//!
//! Wire layout (28-byte descriptor + N gate bytes):
//!
//! | Offset | Bytes | Field                                                 |
//! |-------:|------:|-------------------------------------------------------|
//! |      0 |     4 | Reserved (set to 0)                                   |
//! |      4 |     2 | Number of Data Moment Gates (NG, 0 to 1840)           |
//! |      6 |     2 | Range to first gate (ScaledInteger2, 0.001 km)        |
//! |      8 |     2 | Sample interval (ScaledInteger2, 0.001 km, 0.25–4.0)  |
//! |     10 |     2 | TOVER (ScaledInteger2, 0.1 dB)                        |
//! |     12 |     2 | SNR threshold (ScaledSInteger2, 0.125 dB)             |
//! |     14 |     1 | Control flags                                         |
//! |     15 |     1 | Data Word Size (8 or 16)                              |
//! |     16 |     4 | Scale (Real4)                                         |
//! |     20 |     4 | Offset (Real4)                                        |
//! |     24 |  N×W | N gate bytes, W = data_word_size / 8 (1 or 2)         |
//!
//! Per ICD Table XVII-I, raw gate values map to physical units as:
//!
//! * `raw == 0` → BelowThreshold (sub-detection)
//! * `raw == 1` → RangeFolded
//! * `raw >= 2` → physical = (raw - offset) / scale

use crate::backends::nexrad::decode::error::{NexradDecodeError, Result};
use crate::backends::nexrad::decode::reader::SliceReader;

pub(crate) const DESCRIPTOR_SIZE: usize = 24;

/// Decoded moment-data descriptor (the 24 bytes after the 4-byte
/// `DataBlockId`). Gate bytes are stored as a borrowed slice in
/// `MomentBlock` below.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct MomentDescriptor {
    pub(crate) gate_count: u16,
    pub(crate) range_to_first_gate_km: f32,
    pub(crate) gate_interval_km: f32,
    pub(crate) tover_db: f32,
    pub(crate) snr_threshold_db: f32,
    pub(crate) control_flags: u8,
    pub(crate) data_word_size_bits: u8,
    pub(crate) scale: f32,
    pub(crate) offset: f32,
}

impl MomentDescriptor {
    pub(crate) fn read(reader: &mut SliceReader<'_>) -> Result<Self> {
        let _reserved = reader.read_u32_be()?;
        let gate_count = reader.read_u16_be()?;
        let range_raw = reader.read_u16_be()?;
        let interval_raw = reader.read_u16_be()?;
        let tover_raw = reader.read_u16_be()?;
        let snr_raw = reader.read_i16_be()?;
        let control_flags = reader.read_u8()?;
        let data_word_size_bits = reader.read_u8()?;
        let scale = reader.read_f32_be()?;
        let offset = reader.read_f32_be()?;
        if data_word_size_bits != 8 && data_word_size_bits != 16 {
            return Err(NexradDecodeError::MalformedHeader {
                offset: 0,
                reason: "MSG_31 moment data_word_size must be 8 or 16",
            });
        }
        Ok(Self {
            gate_count,
            range_to_first_gate_km: f32::from(range_raw) / 1000.0,
            gate_interval_km: f32::from(interval_raw) / 1000.0,
            tover_db: f32::from(tover_raw) / 10.0,
            snr_threshold_db: f32::from(snr_raw) / 8.0,
            control_flags,
            data_word_size_bits,
            scale,
            offset,
        })
    }

    /// Width of one gate's raw value in bytes (1 or 2).
    pub(crate) fn word_size_bytes(&self) -> usize {
        usize::from(self.data_word_size_bits) / 8
    }

    /// Total payload size of the gate data array.
    pub(crate) fn gate_array_bytes(&self) -> usize {
        usize::from(self.gate_count) * self.word_size_bytes()
    }
}

/// One sample value from a moment block — either a physical f32 or
/// a sentinel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum MomentValue {
    /// Below detection threshold (raw byte == 0).
    BelowThreshold,
    /// Velocity range-folded; magnitude unreliable (raw byte == 1).
    RangeFolded,
    /// Physical value in the moment's natural units (dBZ, m/s, ...).
    Value(f32),
}

/// Decoded moment block: descriptor + the raw gate bytes (borrowed
/// from the input). Decoding to `MomentValue` is on-demand via
/// `iter()` to avoid materialising a `Vec<MomentValue>` for every
/// moment of every radial.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MomentBlock<'a> {
    pub(crate) descriptor: MomentDescriptor,
    pub(crate) gate_bytes: &'a [u8],
}

impl<'a> MomentBlock<'a> {
    /// Read the 24-byte descriptor then borrow `gate_count *
    /// word_size_bytes` bytes for the gate array.
    pub(crate) fn read(reader: &mut SliceReader<'a>) -> Result<Self> {
        let descriptor = MomentDescriptor::read(reader)?;
        let gate_bytes = reader.take_bytes(descriptor.gate_array_bytes())?;
        Ok(Self {
            descriptor,
            gate_bytes,
        })
    }

    /// Iterate decoded sample values, applying ICD Table XVII-I
    /// `raw == 0 → BelowThreshold`, `raw == 1 → RangeFolded`, else
    /// `(raw - offset) / scale`.
    pub(crate) fn iter(&self) -> MomentIter<'a> {
        MomentIter {
            bytes: self.gate_bytes,
            scale: self.descriptor.scale,
            offset: self.descriptor.offset,
            word_size_bytes: self.descriptor.word_size_bytes(),
            pos: 0,
        }
    }
}

pub(crate) struct MomentIter<'a> {
    bytes: &'a [u8],
    scale: f32,
    offset: f32,
    word_size_bytes: usize,
    pos: usize,
}

impl<'a> Iterator for MomentIter<'a> {
    type Item = MomentValue;
    fn next(&mut self) -> Option<MomentValue> {
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
            0 => MomentValue::BelowThreshold,
            1 => MomentValue::RangeFolded,
            n => MomentValue::Value((n as f32 - self.offset) / self.scale),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct a 24-byte descriptor with 8-bit gates, scale=2.0,
    /// offset=66.0 (the legacy MSG_1 reflectivity defaults so the
    /// math is hand-checkable).
    fn descriptor_8bit(gate_count: u16) -> Vec<u8> {
        let mut buf = Vec::with_capacity(DESCRIPTOR_SIZE);
        buf.extend_from_slice(&0u32.to_be_bytes()); // reserved
        buf.extend_from_slice(&gate_count.to_be_bytes());
        buf.extend_from_slice(&2_000u16.to_be_bytes()); // first gate 2.000 km
        buf.extend_from_slice(&250u16.to_be_bytes()); // interval 0.250 km
        buf.extend_from_slice(&18u16.to_be_bytes()); // tover 1.8 dB
        buf.extend_from_slice(&8_i16.to_be_bytes()); // snr 1.0 dB (8 / 8)
        buf.push(0); // control flags
        buf.push(8); // data_word_size_bits
        buf.extend_from_slice(&2.0_f32.to_be_bytes()); // scale
        buf.extend_from_slice(&66.0_f32.to_be_bytes()); // offset
        debug_assert_eq!(buf.len(), DESCRIPTOR_SIZE);
        buf
    }

    #[test]
    fn descriptor_round_trips() {
        let bytes = descriptor_8bit(100);
        let mut r = SliceReader::new(&bytes);
        let d = MomentDescriptor::read(&mut r).unwrap();
        assert_eq!(d.gate_count, 100);
        assert!((d.range_to_first_gate_km - 2.0).abs() < 1e-6);
        assert!((d.gate_interval_km - 0.250).abs() < 1e-6);
        assert!((d.tover_db - 1.8).abs() < 1e-3);
        assert!((d.snr_threshold_db - 1.0).abs() < 1e-3);
        assert_eq!(d.data_word_size_bits, 8);
        assert!((d.scale - 2.0).abs() < 1e-6);
        assert!((d.offset - 66.0).abs() < 1e-6);
        assert_eq!(d.word_size_bytes(), 1);
        assert_eq!(d.gate_array_bytes(), 100);
    }

    #[test]
    fn descriptor_rejects_bad_word_size() {
        let mut bytes = descriptor_8bit(0);
        bytes[15] = 4; // invalid data_word_size
        let mut r = SliceReader::new(&bytes);
        assert!(MomentDescriptor::read(&mut r).is_err());
    }

    #[test]
    fn moment_block_decodes_gates_with_sentinels() {
        // 4 8-bit gates: raw=0 (below threshold), raw=1 (range
        // folded), raw=130 (real reflectivity = (130-66)/2 = 32 dBZ),
        // raw=2 (= -32 dBZ).
        let mut bytes = descriptor_8bit(4);
        bytes.extend_from_slice(&[0, 1, 130, 2]);
        let mut r = SliceReader::new(&bytes);
        let m = MomentBlock::read(&mut r).unwrap();
        let values: Vec<MomentValue> = m.iter().collect();
        assert_eq!(values.len(), 4);
        assert_eq!(values[0], MomentValue::BelowThreshold);
        assert_eq!(values[1], MomentValue::RangeFolded);
        match values[2] {
            MomentValue::Value(v) => assert!((v - 32.0).abs() < 1e-6, "got {v}"),
            _ => panic!("expected Value, got {:?}", values[2]),
        }
        match values[3] {
            MomentValue::Value(v) => assert!((v - (-32.0)).abs() < 1e-6, "got {v}"),
            _ => panic!("expected Value, got {:?}", values[3]),
        }
    }

    #[test]
    fn moment_block_errors_on_truncated_gate_bytes() {
        // Descriptor declares 100 gates × 1 byte but only 4 bytes
        // of gate data follow. `MomentBlock::read` must error
        // rather than panic / return junk.
        let mut bytes = descriptor_8bit(100);
        bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let mut r = SliceReader::new(&bytes);
        assert!(MomentBlock::read(&mut r).is_err());
    }

    #[test]
    fn moment_block_supports_16bit_gates() {
        // 2 16-bit gates with scale=1, offset=0 so raw values pass
        // through unchanged.
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&2u16.to_be_bytes()); // gate count
        buf.extend_from_slice(&0u16.to_be_bytes()); // range
        buf.extend_from_slice(&250u16.to_be_bytes()); // interval
        buf.extend_from_slice(&0u16.to_be_bytes()); // tover
        buf.extend_from_slice(&0_i16.to_be_bytes()); // snr
        buf.push(0); // ctrl flags
        buf.push(16); // 16-bit gates
        buf.extend_from_slice(&1.0_f32.to_be_bytes()); // scale
        buf.extend_from_slice(&0.0_f32.to_be_bytes()); // offset
        buf.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]); // 2 gates: 0x1234, 0x5678

        let mut r = SliceReader::new(&buf);
        let m = MomentBlock::read(&mut r).unwrap();
        let values: Vec<MomentValue> = m.iter().collect();
        assert_eq!(values.len(), 2);
        match values[0] {
            MomentValue::Value(v) => assert!((v - 0x1234 as f32).abs() < 1e-6),
            _ => panic!(),
        }
        match values[1] {
            MomentValue::Value(v) => assert!((v - 0x5678 as f32).abs() < 1e-6),
            _ => panic!(),
        }
    }
}
