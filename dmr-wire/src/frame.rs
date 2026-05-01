//! DMR voice burst disassembly.
//!
//! Extracts three AMBE+2 codewords from a 33-byte `dmr_data` payload
//! and assembles bursts from three codewords.
//!
//! Physical layout (264 bits = 132 dibits):
//! ```text
//!   Frame1 (36 dibits)  Frame2a (18 dibits)  SYNC/EMB (24 dibits)
//!   Frame2b (18 dibits) Frame3 (36 dibits)
//! ```
//!
//! Frame2 straddles the 48-bit SYNC/EMB gap.  AmbeFrame carries the
//! raw on-air 36 dibits packed 4 dibits/byte (dibit 0 in bits 7..6),
//! which is what the DVSI AMBE-3000 chip expects.  mbelib deinterleaves
//! internally via codeword.rs.
//!
//! Reference: ETSI TS 102 361-1 Section 9.1.
//! See DESIGN.md "DMR Voice Burst Disassembly" for the full spec.

use ambe::AMBE_FRAME_SIZE;
use ambe::AmbeFrame;

use super::dmrd::DMR_DATA_SIZE;

/// Dibits per AMBE codeword (72 bits / 2).
const DIBITS_PER_CODEWORD: usize = 36;

/// Dibits in the SYNC/EMB middle section.
const SYNC_DIBITS: usize = 24;

/// Total dibits in a burst.
const BURST_DIBITS: usize = DMR_DATA_SIZE * 4;

/// Extract dibits from a byte buffer.  Each byte contains 4 dibits,
/// MSB-first: bits 7-6 = dibit 0, bits 5-4 = dibit 1, etc.
fn byte_to_dibits(byte: u8) -> [u8; 4] {
    [
        (byte >> 6) & 0x03,
        (byte >> 4) & 0x03,
        (byte >> 2) & 0x03,
        byte & 0x03,
    ]
}

/// Convert the 33-byte dmr_data into 132 dibits.
fn burst_to_dibits(data: &[u8; DMR_DATA_SIZE]) -> [u8; BURST_DIBITS] {
    let mut dibits = [0u8; BURST_DIBITS];
    for (i, &byte) in data.iter().enumerate() {
        let d = byte_to_dibits(byte);
        dibits[i * 4] = d[0];
        dibits[i * 4 + 1] = d[1];
        dibits[i * 4 + 2] = d[2];
        dibits[i * 4 + 3] = d[3];
    }
    dibits
}

/// Pack 132 dibits into 33 bytes, MSB-first (4 dibits per byte).
/// Inverse of burst_to_dibits.
fn dibits_to_bytes(dibits: &[u8; BURST_DIBITS]) -> [u8; DMR_DATA_SIZE] {
    let mut data = [0u8; DMR_DATA_SIZE];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (dibits[i * 4] << 6)
            | (dibits[i * 4 + 1] << 4)
            | (dibits[i * 4 + 2] << 2)
            | dibits[i * 4 + 3];
    }
    data
}

/// Pack 36 on-air dibits into a 9-byte AmbeFrame: 4 dibits/byte,
/// dibit 0 in bits 7..6, dibit 1 in bits 5..4, etc.  Matches DVSI /
/// dsdcc storeSymbolDV.
///
/// Masks to 2 bits per dibit so out-of-range inputs (values > 3)
/// can't corrupt neighbouring dibits via the shift.
fn pack_dibits(dibits: &[u8; DIBITS_PER_CODEWORD]) -> AmbeFrame {
    let mut frame = [0u8; AMBE_FRAME_SIZE];
    for (i, &d) in dibits.iter().enumerate() {
        frame[i / 4] |= (d & 0x03) << (6 - 2 * (i % 4));
    }
    frame
}

/// Unpack a 9-byte AmbeFrame into 36 dibits.  Inverse of pack_dibits.
fn unpack_dibits(frame: &AmbeFrame) -> [u8; DIBITS_PER_CODEWORD] {
    let mut dibits = [0u8; DIBITS_PER_CODEWORD];
    for i in 0..DIBITS_PER_CODEWORD {
        dibits[i] = (frame[i / 4] >> (6 - 2 * (i % 4))) & 0x03;
    }
    dibits
}

/// Extract three AMBE+2 codewords from a 33-byte DMR voice burst.
///
/// Returns `[AmbeFrame; 3]` -- one per 20 ms audio segment (60 ms
/// total per burst).  Each AmbeFrame holds the raw on-air dibit stream
/// for that codeword (no deinterleave); the DV3000 chip does its own
/// deinterleave and FEC internally.
pub(crate) fn extract_ambe(data: &[u8; DMR_DATA_SIZE]) -> [AmbeFrame; 3] {
    let dibits = burst_to_dibits(data);

    // Frame1: dibits 0..36
    let mut cw1 = [0u8; DIBITS_PER_CODEWORD];
    cw1.copy_from_slice(&dibits[0..DIBITS_PER_CODEWORD]);

    // Frame2: dibits 36..54 (before SYNC) + dibits 78..96 (after SYNC)
    let split = DIBITS_PER_CODEWORD / 2; // 18
    let sync_start = DIBITS_PER_CODEWORD + split; // 54
    let sync_end = sync_start + SYNC_DIBITS; // 78
    let mut cw2 = [0u8; DIBITS_PER_CODEWORD];
    cw2[..split].copy_from_slice(&dibits[DIBITS_PER_CODEWORD..sync_start]);
    cw2[split..].copy_from_slice(&dibits[sync_end..sync_end + split]);

    // Frame3: dibits 96..132
    let frame3_start = sync_end + split; // 96
    let mut cw3 = [0u8; DIBITS_PER_CODEWORD];
    cw3.copy_from_slice(&dibits[frame3_start..frame3_start + DIBITS_PER_CODEWORD]);

    [pack_dibits(&cw1), pack_dibits(&cw2), pack_dibits(&cw3)]
}

/// Extract the 48-bit (6-byte) SYNC/EMB section from a burst.  Inverse
/// of the sync placement in `assemble_burst`: returns the 6 bytes that
/// occupy dibits 54..78 of the burst.
///
/// `cfg(test)` only -- no runtime consumer.  Used by encoder/decoder
/// cross-check tests against external superframe references.  Promote
/// to `pub(crate)` when an RF reception path needs embedded LC.
#[cfg(test)]
pub(crate) fn extract_sync_section(data: &[u8; DMR_DATA_SIZE]) -> [u8; 6] {
    let dibits = burst_to_dibits(data);
    let split = DIBITS_PER_CODEWORD / 2;
    let sync_start = DIBITS_PER_CODEWORD + split;
    let mut sync = [0u8; 6];
    for (i, byte) in sync.iter_mut().enumerate() {
        let base = sync_start + i * 4;
        *byte = (dibits[base] << 6)
            | (dibits[base + 1] << 4)
            | (dibits[base + 2] << 2)
            | dibits[base + 3];
    }
    sync
}

/// Assemble a 33-byte DMR voice burst from three AMBE+2 codewords
/// and a 6-byte (48-bit) SYNC/EMB pattern.  Inverse of extract_ambe.
///
/// `sync` is 6 bytes = 24 dibits for the center SYNC/EMB section.
pub(crate) fn assemble_burst(codewords: &[AmbeFrame; 3], sync: &[u8; 6]) -> [u8; DMR_DATA_SIZE] {
    let cw1 = unpack_dibits(&codewords[0]);
    let cw2 = unpack_dibits(&codewords[1]);
    let cw3 = unpack_dibits(&codewords[2]);

    let split = DIBITS_PER_CODEWORD / 2; // 18

    // Convert sync bytes to dibits.
    let mut sync_dibits = [0u8; SYNC_DIBITS];
    for (i, &byte) in sync.iter().enumerate() {
        let d = byte_to_dibits(byte);
        sync_dibits[i * 4] = d[0];
        sync_dibits[i * 4 + 1] = d[1];
        sync_dibits[i * 4 + 2] = d[2];
        sync_dibits[i * 4 + 3] = d[3];
    }

    // Place: cw1 (36) + cw2a (18) + sync (24) + cw2b (18) + cw3 (36)
    let mut dibits = [0u8; BURST_DIBITS];
    dibits[..DIBITS_PER_CODEWORD].copy_from_slice(&cw1);
    dibits[DIBITS_PER_CODEWORD..DIBITS_PER_CODEWORD + split].copy_from_slice(&cw2[..split]);
    let sync_start = DIBITS_PER_CODEWORD + split;
    dibits[sync_start..sync_start + SYNC_DIBITS].copy_from_slice(&sync_dibits);
    let cw2b_start = sync_start + SYNC_DIBITS;
    dibits[cw2b_start..cw2b_start + split].copy_from_slice(&cw2[split..]);
    let cw3_start = cw2b_start + split;
    dibits[cw3_start..cw3_start + DIBITS_PER_CODEWORD].copy_from_slice(&cw3);

    dibits_to_bytes(&dibits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_dibits_length() {
        let data = [0u8; DMR_DATA_SIZE];
        let dibits = burst_to_dibits(&data);
        assert_eq!(dibits.len(), BURST_DIBITS);
    }

    #[test]
    fn byte_to_dibits_known() {
        // 0xA5 = 0b10_10_01_01 -> dibits [2, 2, 1, 1]
        assert_eq!(byte_to_dibits(0xA5), [2, 2, 1, 1]);
        // 0xFF = 0b11_11_11_11 -> dibits [3, 3, 3, 3]
        assert_eq!(byte_to_dibits(0xFF), [3, 3, 3, 3]);
        // 0x00 -> [0, 0, 0, 0]
        assert_eq!(byte_to_dibits(0x00), [0, 0, 0, 0]);
    }

    #[test]
    fn extract_ambe_does_not_panic() {
        let data = [0u8; DMR_DATA_SIZE];
        let frames = extract_ambe(&data);
        assert_eq!(frames.len(), 3);
        // All-zero input -> all-zero dibits -> all-zero AmbeFrame.
        for frame in &frames {
            assert_eq!(frame, &[0u8; AMBE_FRAME_SIZE]);
        }
    }

    #[test]
    fn extract_ambe_nonzero_input() {
        let mut data = [0u8; DMR_DATA_SIZE];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(0x37);
        }
        let frames = extract_ambe(&data);
        let any_nonzero = frames.iter().any(|f: &AmbeFrame| f.iter().any(|&b| b != 0));
        assert!(any_nonzero, "non-zero input should produce non-zero output");
    }

    #[test]
    fn extract_ignores_sync_bytes() {
        // Build a burst where every dibit is 0 except the 24-dibit SYNC
        // section (dibits 54..78), which is all 0b11.  extract_ambe
        // reads dibits 0..36, 36..54, 78..96, 96..132 and skips 54..78.
        // No 1-bit from SYNC must appear in any codeword.
        let mut data = [0u8; DMR_DATA_SIZE];
        data[13] = 0x0F;
        for b in &mut data[14..=18] {
            *b = 0xFF;
        }
        data[19] = 0xF0;

        let frames = extract_ambe(&data);
        for (i, frame) in frames.iter().enumerate() {
            assert_eq!(
                frame, &[0u8; AMBE_FRAME_SIZE],
                "frame {i} picked up SYNC bits"
            );
        }
    }

    #[test]
    fn extract_sync_section_inverts_assemble() {
        let cw = [
            [0x12u8; AMBE_FRAME_SIZE],
            [0x34u8; AMBE_FRAME_SIZE],
            [0x56u8; AMBE_FRAME_SIZE],
        ];
        let sync = [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE];
        let burst = assemble_burst(&cw, &sync);
        assert_eq!(extract_sync_section(&burst), sync);
    }

    #[test]
    fn pack_unpack_dibits_roundtrip() {
        let mut dibits = [0u8; DIBITS_PER_CODEWORD];
        for (i, d) in dibits.iter_mut().enumerate() {
            *d = (i % 4) as u8;
        }
        let frame = pack_dibits(&dibits);
        let back = unpack_dibits(&frame);
        assert_eq!(back, dibits);
    }

    #[test]
    fn pack_unpack_dibits_all_zero() {
        let dibits = [0u8; DIBITS_PER_CODEWORD];
        let frame = pack_dibits(&dibits);
        assert_eq!(frame, [0u8; AMBE_FRAME_SIZE]);
        assert_eq!(unpack_dibits(&frame), dibits);
    }

    #[test]
    fn pack_unpack_dibits_all_ones() {
        let dibits = [0x03u8; DIBITS_PER_CODEWORD];
        let frame = pack_dibits(&dibits);
        assert_eq!(frame, [0xFFu8; AMBE_FRAME_SIZE]);
        assert_eq!(unpack_dibits(&frame), dibits);
    }

    #[test]
    fn pack_unpack_dibits_exhaustive_per_position() {
        // For each dibit position i and each value v in 0..4, a
        // single non-zero dibit must survive the pack -> unpack.
        // Catches per-position shift errors without needing proptest.
        for i in 0..DIBITS_PER_CODEWORD {
            for v in 0..4u8 {
                let mut dibits = [0u8; DIBITS_PER_CODEWORD];
                dibits[i] = v;
                let back = unpack_dibits(&pack_dibits(&dibits));
                assert_eq!(back, dibits, "position {i} value {v}");
            }
        }
    }

    #[test]
    fn pack_dibits_masks_out_of_range_input() {
        // Values > 3 must not leak into the neighbouring dibit.
        // Feed 0xFF at position 0; only the low 2 bits should land.
        let mut dibits = [0u8; DIBITS_PER_CODEWORD];
        dibits[0] = 0xFF;
        let frame = pack_dibits(&dibits);
        // Position 0 occupies byte 0 bits 7..6; rest of byte 0 stays 0.
        assert_eq!(frame[0], 0b1100_0000);
        for b in &frame[1..] {
            assert_eq!(*b, 0, "bleed into later bytes");
        }
    }

    proptest::proptest! {
        /// pack -> unpack must be identity for ANY 36-dibit input
        /// (random multi-position combinations the exhaustive
        /// per-position sweep doesn't reach).
        #[test]
        fn pack_unpack_dibits_roundtrip_property(
            v in proptest::collection::vec(0u8..4u8, DIBITS_PER_CODEWORD..=DIBITS_PER_CODEWORD),
        ) {
            let mut dibits = [0u8; DIBITS_PER_CODEWORD];
            dibits.copy_from_slice(&v);
            let packed = pack_dibits(&dibits);
            let unpacked = unpack_dibits(&packed);
            proptest::prop_assert_eq!(unpacked, dibits);
        }

        /// assemble_burst -> extract_ambe is the inverse for ANY
        /// AmbeFrame triple and SYNC pattern.  Existing single-vector
        /// test catches one input; this proves it for the whole
        /// 9-byte^3 + 6-byte input space.
        #[test]
        fn assemble_extract_roundtrip_property(
            cw_bytes in proptest::collection::vec(0u8..=255u8, 27..=27),
            sync_bytes in proptest::collection::vec(0u8..=255u8, 6..=6),
        ) {
            let mut cw1 = [0u8; AMBE_FRAME_SIZE];
            let mut cw2 = [0u8; AMBE_FRAME_SIZE];
            let mut cw3 = [0u8; AMBE_FRAME_SIZE];
            cw1.copy_from_slice(&cw_bytes[0..9]);
            cw2.copy_from_slice(&cw_bytes[9..18]);
            cw3.copy_from_slice(&cw_bytes[18..27]);
            let mut sync = [0u8; 6];
            sync.copy_from_slice(&sync_bytes);
            let codewords = [cw1, cw2, cw3];
            let burst = assemble_burst(&codewords, &sync);
            proptest::prop_assert_eq!(extract_ambe(&burst), codewords);
        }
    }

    #[test]
    fn assemble_extract_roundtrip() {
        let codewords: [AmbeFrame; 3] = [
            [0xA5, 0x5A, 0x33, 0xCC, 0x0F, 0xF0, 0x3C, 0xC3, 0x5A],
            [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x11],
            [0xFF, 0x00, 0xAA, 0x55, 0xCC, 0x33, 0x66, 0x99, 0x01],
        ];
        let sync = [0x75, 0x5F, 0xD7, 0xDF, 0x75, 0xF7];
        let burst = assemble_burst(&codewords, &sync);
        let extracted = extract_ambe(&burst);
        assert_eq!(extracted, codewords);
    }

    #[test]
    fn assemble_preserves_sync() {
        let codewords = [[0u8; AMBE_FRAME_SIZE]; 3];
        let sync = [0x75, 0x5F, 0xD7, 0xDF, 0x75, 0xF7];
        let burst = assemble_burst(&codewords, &sync);
        // SYNC is at bits 108-155 = dibits 54-77.
        let dibits = burst_to_dibits(&burst);
        let mut sync_bytes = [0u8; 6];
        for (i, byte) in sync_bytes.iter_mut().enumerate() {
            *byte = (dibits[54 + i * 4] << 6)
                | (dibits[54 + i * 4 + 1] << 4)
                | (dibits[54 + i * 4 + 2] << 2)
                | dibits[54 + i * 4 + 3];
        }
        assert_eq!(sync_bytes, sync);
    }
}
