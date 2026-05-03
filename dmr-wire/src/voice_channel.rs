//! AMBE+2 channel coding for DMR voice frames: 49 raw codec bits <->
//! 72 channel-coded bits.
//!
//! The 9-byte channel-coded form is what a DVSI AMBE-3000R chip emits
//! per 20 ms voice frame in DMR rate (index 33), and what the bridge
//! puts into a DMR voice burst's AMBE block.  Internal layout of those
//! 9 bytes:
//!
//! - 36 dibits packed MSB-pair-first (4 dibits per byte, dibit 0 in
//!   bits 7..6 of byte 0).
//! - On-air interleave per ETSI TS 102 361-1 section 9.1: each dibit
//!   `i` writes its MSB to row `RW[i]` column `RX[i]` and its LSB to
//!   row `RY[i]` column `RZ[i]` of a 4-row source matrix.
//! - Row 0 (24 bits): 12 source bits (cols 23..12) + Golay(24,12)
//!   parity (cols 11..0).
//! - Row 1 (23 bits): 12 PN-whitened source bits (cols 22..11) +
//!   Golay(23,12) parity (cols 10..0).  Whitening seed is the 12
//!   row-0 source bits read MSB-first.
//! - Row 2 (11 bits): unprotected source bits (cols 10..0).
//! - Row 3 (14 bits): unprotected source bits (cols 13..0).
//!
//! Total source bits: 12 + 12 + 11 + 14 = 49.
//!
//! References: AMBE-3000R Users Manual, ETSI TS 102 361-1 section 9.1,
//! mbelib's `ambe3600x2450.c` (independent C implementation,
//! cross-checked at every step).

const GOLAY_23_12_GEN: u32 = 0xC75;

/// Pseudo-random whitening LCG: `pr_{i+1} = (mult * pr_i + inc) mod
/// modulus`; output bit is the high bit of the new state.  Matches
/// mbelib's `mbe_demodulateAmbe3600x2450Data`.
const PN_MULT: u32 = 173;
const PN_INC: u32 = 13849;
const PN_MOD: u32 = 0x10000;

const DIBITS_PER_FRAME: usize = 36;
pub const RAW_BYTES: usize = 7;
pub const CODED_BYTES: usize = 9;
const RAW_BITS: usize = 49;

/// `MBELIB_TO_CHIP[i]` is the chip's bit position (0..48) carrying the
/// codec source bit that mbelib calls `ambe_d[i]`.  The two orders are
/// the same multiset (mbelib's ambe_d[] reverse-engineered from the
/// chip; the chip's CHAND-format raw output uses a different
/// permutation).  Discovered empirically by cross-correlating 8208
/// chip-captured (mbelib-order, chip-order) bit-pairs across 12
/// utterances; every mbelib position mapped to a unique chip position.
const MBELIB_TO_CHIP: [usize; RAW_BITS] = [
    0, 3, 6, 9, 12, 15, 18, 21, 24, 27, 30, 33, 36, 39, 41, 43, 45, 47, 1, 4, 7, 10, 13, 16, 19,
    22, 25, 28, 31, 34, 37, 40, 42, 44, 46, 48, 2, 5, 8, 11, 14, 17, 20, 23, 26, 29, 32, 35, 38,
];

/// Each on-air dibit `i` writes its MSB to `fr[RW[i]][RX[i]]`.
const RW: [u8; 36] = [
    0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 2, 0, 2, 0, 2, 0, 2, 0, 2,
    0, 2, 0, 2,
];
const RX: [u8; 36] = [
    23, 10, 22, 9, 21, 8, 20, 7, 19, 6, 18, 5, 17, 4, 16, 3, 15, 2, 14, 1, 13, 0, 12, 10, 11, 9,
    10, 8, 9, 7, 8, 6, 7, 5, 6, 4,
];
/// Each on-air dibit `i` writes its LSB to `fr[RY[i]][RZ[i]]`.
const RY: [u8; 36] = [
    0, 2, 0, 2, 0, 2, 0, 2, 0, 3, 0, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3, 1, 3,
    1, 3, 1, 3,
];
const RZ: [u8; 36] = [
    5, 3, 4, 2, 3, 1, 2, 0, 1, 13, 0, 12, 22, 11, 21, 10, 20, 9, 19, 8, 18, 7, 17, 6, 16, 5, 15, 4,
    14, 3, 13, 2, 12, 1, 11, 0,
];

/// Systematic Golay(23,12) encode: 12 data bits -> 23-bit codeword
/// with data in bits 22..11 (MSB-aligned) and parity in bits 10..0.
/// Generator polynomial `0xC75` (x^11 + x^10 + x^6 + x^5 + x^4 + x^2
/// + 1), the standard P25 / DMR variant.
pub(crate) fn golay_23_12_encode(data: u16) -> u32 {
    debug_assert!(data < (1 << 12), "data must fit in 12 bits");
    let mut remainder = (data as u32) << 11;
    for i in (11..=22).rev() {
        if remainder & (1 << i) != 0 {
            remainder ^= GOLAY_23_12_GEN << (i - 11);
        }
    }
    ((data as u32) << 11) | (remainder & 0x7FF)
}

/// Extended Golay(24,12) encode: Golay(23,12) plus an overall-parity
/// bit (XOR of all 23 codeword bits) appended in bit 23.
pub(crate) fn golay_24_12_encode(data: u16) -> u32 {
    let cw23 = golay_23_12_encode(data);
    let parity = cw23.count_ones() & 1;
    (cw23 << 1) | parity
}

/// Generate `n` PN bits from a 12-bit seed.  Seed is the 12 row-0
/// source bits packed MSB-first (col 23 is the MSB).  Output bit `i`
/// is the high bit of state after the `i`-th LCG advance.
pub(crate) fn pn_sequence(seed_12: u16, n: usize) -> Vec<bool> {
    debug_assert!(seed_12 < (1 << 12), "seed must fit in 12 bits");
    let mut pr: u32 = u32::from(seed_12) * 16;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        pr = (PN_MULT * pr + PN_INC) % PN_MOD;
        out.push((pr >> 15) & 1 == 1);
    }
    out
}

/// Deinterleave 9 channel-coded bytes into the 4-row source matrix.
fn deinterleave(coded: &[u8; CODED_BYTES]) -> [[u8; 24]; 4] {
    let mut fr = [[0u8; 24]; 4];
    for i in 0..DIBITS_PER_FRAME {
        let byte = coded[i / 4];
        let dibit = (byte >> (6 - 2 * (i % 4))) & 0x03;
        fr[RW[i] as usize][RX[i] as usize] = (dibit >> 1) & 1;
        fr[RY[i] as usize][RZ[i] as usize] = dibit & 1;
    }
    fr
}

/// Inverse of `deinterleave`: pack a 4-row source matrix into 9 bytes
/// of dibits.
fn interleave(fr: &[[u8; 24]; 4]) -> [u8; CODED_BYTES] {
    let mut out = [0u8; CODED_BYTES];
    for i in 0..DIBITS_PER_FRAME {
        let msb = fr[RW[i] as usize][RX[i] as usize];
        let lsb = fr[RY[i] as usize][RZ[i] as usize];
        let dibit = (msb << 1) | lsb;
        let byte_idx = i / 4;
        let bit_off = 6 - 2 * (i % 4);
        out[byte_idx] |= dibit << bit_off;
    }
    out
}

/// Read the 12 row-0 source bits from cols 23..12 packed MSB-first.
fn row0_seed(fr: &[[u8; 24]; 4]) -> u16 {
    let mut seed = 0u16;
    for col in (12..=23).rev() {
        seed = (seed << 1) | u16::from(fr[0][col]);
    }
    seed
}

/// Pack `bits` (each 0 or 1) MSB-first into the high bits of a 7-byte
/// buffer.  Bit 0 of input becomes bit 7 of byte 0; bit 48 becomes bit
/// 1 of byte 6 (byte 6 bit 0 unused).
fn pack_msb_first(bits: &[u8; RAW_BITS]) -> [u8; RAW_BYTES] {
    let mut out = [0u8; RAW_BYTES];
    for (i, &b) in bits.iter().enumerate() {
        out[i / 8] |= (b & 1) << (7 - (i % 8));
    }
    out
}

/// Inverse of `pack_msb_first`: unpack 49 bits from the high bits of
/// a 7-byte buffer.
fn unpack_msb_first(packed: &[u8; RAW_BYTES]) -> [u8; RAW_BITS] {
    let mut out = [0u8; RAW_BITS];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = (packed[i / 8] >> (7 - (i % 8))) & 1;
    }
    out
}

/// Decode 9 channel-coded bytes into 49 raw codec bits, packed
/// MSB-first into 7 bytes in the chip's natural rate-34 output order
/// (the order an AMBE-3000R produces when configured for raw 2450
/// bps speech-only mode).  No error correction: data bits are read
/// from the systematic positions only.
pub fn channel_decode(coded: &[u8; CODED_BYTES]) -> [u8; RAW_BYTES] {
    let fr = deinterleave(coded);
    let pr = pn_sequence(row0_seed(&fr), 23);

    // mbelib-order bits, in the order ambe_d[0..48] uses internally.
    let mut mbelib_bits = [0u8; RAW_BITS];
    let mut idx = 0;

    // Row 0 source bits: cols 23..12.
    for col in (12..=23).rev() {
        mbelib_bits[idx] = fr[0][col];
        idx += 1;
    }
    // Row 1 source bits: cols 22..11, dewhitened with pr[0..12].
    for (i, col) in (11..=22).rev().enumerate() {
        mbelib_bits[idx] = fr[1][col] ^ u8::from(pr[i]);
        idx += 1;
    }
    // Row 2 source bits: cols 10..0.
    for col in (0..=10).rev() {
        mbelib_bits[idx] = fr[2][col];
        idx += 1;
    }
    // Row 3 source bits: cols 13..0.
    for col in (0..=13).rev() {
        mbelib_bits[idx] = fr[3][col];
        idx += 1;
    }

    let mut chip_bits = [0u8; RAW_BITS];
    for (i, &b) in mbelib_bits.iter().enumerate() {
        chip_bits[MBELIB_TO_CHIP[i]] = b;
    }
    pack_msb_first(&chip_bits)
}

/// Encode 49 raw codec bits (packed MSB-first into 7 bytes in the
/// chip's natural rate-34 output order) into the 9-byte channel-coded
/// form.
///
/// Uses the standard P25 / DMR Golay generator polynomial (`0xC75`).
/// Round-trips bit-for-bit with `channel_decode`, but does NOT
/// reproduce the AMBE-3000R chip's parity bytes byte-for-byte: the
/// chip uses a different Golay variant whose specifics are not yet
/// pinned down.  Source-bit positions in the output match the chip;
/// only Golay parity bits differ.
pub fn channel_encode(raw: &[u8; RAW_BYTES]) -> [u8; CODED_BYTES] {
    let chip_bits = unpack_msb_first(raw);
    let mut bits = [0u8; RAW_BITS];
    for (i, slot) in bits.iter_mut().enumerate() {
        *slot = chip_bits[MBELIB_TO_CHIP[i]];
    }
    let mut fr = [[0u8; 24]; 4];

    // Row 0: 12 source bits (bits[0..12]) -> Golay(24,12) -> 24-bit
    // codeword in cols 23..0.  golay_24_12_encode returns codeword
    // with data in bits 23..12 and parity in bits 11..0.
    let mut row0_data = 0u16;
    for &b in &bits[0..12] {
        row0_data = (row0_data << 1) | u16::from(b);
    }
    let cw0 = golay_24_12_encode(row0_data);
    #[expect(
        clippy::needless_range_loop,
        reason = "col is used both as the matrix index and as the codeword shift amount"
    )]
    for col in 0..24 {
        fr[0][col] = ((cw0 >> col) & 1) as u8;
    }

    // Row 1: 12 source bits (bits[12..24]) PN-whitened by pr seeded
    // from row-0 source, then Golay(23,12) -> 23-bit codeword in cols
    // 22..0.
    let pr = pn_sequence(row0_data, 23);
    let mut whitened = 0u16;
    for (i, &b) in bits[12..24].iter().enumerate() {
        whitened = (whitened << 1) | u16::from(b ^ u8::from(pr[i]));
    }
    let cw1 = golay_23_12_encode(whitened);
    #[expect(
        clippy::needless_range_loop,
        reason = "col is used both as the matrix index and as the codeword shift amount"
    )]
    for col in 0..23 {
        fr[1][col] = ((cw1 >> col) & 1) as u8;
    }

    // Row 2: 11 unprotected source bits (bits[24..35]) at cols 10..0.
    for (i, &b) in bits[24..35].iter().enumerate() {
        fr[2][10 - i] = b;
    }

    // Row 3: 14 unprotected source bits (bits[35..49]) at cols 13..0.
    for (i, &b) in bits[35..49].iter().enumerate() {
        fr[3][13 - i] = b;
    }

    interleave(&fr)
}

#[cfg(test)]
mod tests {
    use super::*;

    use proptest::prelude::*;

    #[test]
    fn golay_23_12_known_zero() {
        assert_eq!(golay_23_12_encode(0), 0);
    }

    #[test]
    fn golay_23_12_systematic_data_position() {
        // Data goes into bits 22..11; for data = 0x801 (bits 11 + 0
        // set) the codeword's bits 22 and 11 are set.
        let cw = golay_23_12_encode(0x801);
        assert_eq!((cw >> 22) & 1, 1);
        assert_eq!((cw >> 11) & 1, 1);
    }

    #[test]
    fn golay_24_12_extended_parity_balances() {
        for data in 0u16..(1 << 12) {
            let cw = golay_24_12_encode(data);
            assert_eq!(
                cw.count_ones() % 2,
                0,
                "data={data:x}: codeword parity not even"
            );
        }
    }

    proptest! {
        #[test]
        fn channel_encode_decode_round_trip(raw in any::<[u8; RAW_BYTES]>()) {
            // pack_msb_first packs bit i into byte i/8 at position
            // 7-(i%8), so bit 48 lands at byte 6 bit 7 and bits 6..0
            // of byte 6 are unused.  Mask those off so the round-trip
            // compares like-with-like.
            let mut masked = raw;
            masked[6] &= 0x80;
            let coded = channel_encode(&masked);
            let decoded = channel_decode(&coded);
            prop_assert_eq!(decoded, masked);
        }
    }
}
