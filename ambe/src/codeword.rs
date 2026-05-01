//! AMBE+2 source-bit extraction for mbelib.
//!
//! `AmbeFrame` is the 72-bit DMR on-air codeword packed as 36 dibits,
//! 4 dibits per byte, dibit 0 in bits 7..6 (DVSI/dsdcc convention --
//! this is what the AMBE-3000 chip expects).
//!
//! mbelib consumes 49 source bits, not the 72 on-air bits.  To get
//! there we:
//!
//!   1. Deinterleave the 36 dibits into ambe_fr[4][24] via the DMR
//!      rW/rX/rY/rZ tables (ETSI TS 102 361-1 Section 9.1).  This
//!      yields four logical rows:
//!
//!      Row 0 (24 bits): Golay(24,12) -- 12 source + 12 parity
//!      Row 1 (23 bits): Golay(23,12) -- 12 source + 11 parity (PRNG whitened)
//!      Row 2 (11 bits): unprotected source
//!      Row 3 (14 bits): unprotected source
//!
//!   2. Dewhiten row 1 (XOR with PRNG seeded from row 0 data bits).
//!      Matches mbelib's `mbe_demodulateAmbe3600x2450Data`.
//!
//!   3. Extract 49 source bits in mbelib's order: each row's source
//!      bits read from HIGH columns DOWN (reversed).

use crate::AmbeFrame;

/// Dibits per DMR AMBE+2 codeword.
const DIBITS_PER_CODEWORD: usize = 36;

/// Number of AMBE+2 source bits.
pub(crate) const AMBE_SOURCE_BITS: usize = 49;

// DMR AMBE deinterleave tables (ETSI TS 102 361-1 Section 9.1).
// Values cross-checked against szechyjs/dsd dmr_const.h and
// MMDVMHost AMBEFEC.cpp.  Each dibit i writes its MSB to
// fr[RW[i]][RX[i]] and LSB to fr[RY[i]][RZ[i]].

const RW: [usize; DIBITS_PER_CODEWORD] = [
    0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 2, 0, 2, 0, 2, 0, 2, 0, 2,
    0, 2, 0, 2,
];
const RX: [usize; DIBITS_PER_CODEWORD] = [
    23, 10, 22, 9, 21, 8, 20, 7, 19, 6, 18, 5, 17, 4, 16, 3, 15, 2, 14, 1, 13, 0, 12, 10, 11, 9,
    10, 8, 9, 7, 8, 6, 7, 5, 6, 4,
];
const RY: [usize; DIBITS_PER_CODEWORD] = [
    0, 2, 0, 2, 0, 2, 0, 2, 0, 3, 0, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3,
    1, 3, 1, 3,
];
const RZ: [usize; DIBITS_PER_CODEWORD] = [
    5, 3, 4, 2, 3, 1, 2, 0, 1, 13, 0, 12, 22, 11, 21, 10, 20, 9, 19, 8, 18, 7, 17, 6, 16, 5, 15, 4,
    14, 3, 13, 2, 12, 1, 11, 0,
];

/// Source-bit column ranges per row (read high -> low, inclusive).
/// Row 0 source cols 23..=12 (12 bits), row 1 cols 22..=11 (12 bits
/// post-dewhitening), row 2 cols 10..=0 (11 bits), row 3 cols 13..=0
/// (14 bits).  Total = 49.
const ROW_EXTRACT: [(usize, usize); 4] = [(23, 12), (22, 11), (10, 0), (13, 0)];

/// Deinterleaved bit store: 4 rows x 24 cols.  Row widths vary
/// (see ROW_BITS); unused high columns stay zero.
type AmbeFr = [[u8; 24]; 4];

/// Unpack 36 dibits from a raw-dibit AmbeFrame and deinterleave into
/// ambe_fr[4][24] via the rW/rX/rY/rZ tables.
fn deinterleave(raw: &AmbeFrame) -> AmbeFr {
    let mut fr: AmbeFr = [[0; 24]; 4];
    for i in 0..DIBITS_PER_CODEWORD {
        let byte = raw[i / 4];
        let dibit = (byte >> (6 - 2 * (i % 4))) & 0x03;
        fr[RW[i]][RX[i]] = (dibit >> 1) & 1;
        fr[RY[i]][RZ[i]] = dibit & 1;
    }
    fr
}

/// Read bit at (row, col) from the deinterleaved row store.
fn read_bit(fr: &AmbeFr, row: usize, col: usize) -> u8 {
    fr[row][col] & 1
}

/// Generate 23-bit PRNG dewhitening sequence for row 1.  Seeded from
/// row 0 cols 23..12.  Matches mbelib's `mbe_demodulateAmbe3600x2450Data`.
fn dewhiten_row1(fr: &AmbeFr) -> [u8; 23] {
    let mut seed: u16 = 0;
    for col in (12..=23).rev() {
        seed <<= 1;
        seed |= read_bit(fr, 0, col) as u16;
    }
    // PRNG: pr[0] = seed * 16, pr[i] = (173 * pr[i-1] + 13849) mod 65536
    let mut pr = [0u32; 24];
    pr[0] = (seed as u32) * 16;
    for i in 1..24 {
        pr[i] = (173 * pr[i - 1] + 13849) % 65536;
    }
    let mut whitening = [0u8; 23];
    for i in 0..23 {
        whitening[i] = (pr[i + 1] / 32768) as u8;
    }
    whitening
}

/// Extract 49 source bits from a raw-dibit AmbeFrame in the order
/// mbelib expects.  Performs deinterleave, PRNG dewhitening of row 1,
/// and source-bit extraction.
pub(crate) fn extract_source_bits(ambe: &AmbeFrame) -> [u8; AMBE_SOURCE_BITS] {
    let fr = deinterleave(ambe);
    let whitening = dewhiten_row1(&fr);

    let mut out = [0u8; AMBE_SOURCE_BITS];
    let mut out_idx = 0;

    for (row, &(high, low)) in ROW_EXTRACT.iter().enumerate() {
        let mut col = high;
        loop {
            let mut bit = read_bit(&fr, row, col);
            if row == 1 {
                let k = 22 - col;
                bit ^= whitening[k];
            }
            out[out_idx] = bit;
            out_idx += 1;
            if col == low {
                break;
            }
            col -= 1;
        }
    }

    debug_assert_eq!(out_idx, AMBE_SOURCE_BITS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AMBE_FRAME_SIZE;

    #[test]
    fn extract_from_zeros() {
        // All-zero dibits -> fr all zeros -> rows 0/2/3 source bits
        // all zero; row 1 comes from PRNG (seed = 0), a fixed pattern.
        // Reference vector freezes that pattern as a regression guard.
        let frame = [0u8; AMBE_FRAME_SIZE];
        let bits = extract_source_bits(&frame);
        let expected: [u8; AMBE_SOURCE_BITS] = [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(bits, expected);
    }

    #[test]
    fn source_count() {
        let total: usize = ROW_EXTRACT.iter().map(|&(high, low)| high - low + 1).sum();
        assert_eq!(total, AMBE_SOURCE_BITS);
    }

    #[test]
    fn extract_deterministic() {
        let a: AmbeFrame = [0xA5, 0x5A, 0x33, 0xCC, 0x0F, 0xF0, 0x3C, 0xC3, 0x5A];
        assert_eq!(extract_source_bits(&a), extract_source_bits(&a));
    }
}
