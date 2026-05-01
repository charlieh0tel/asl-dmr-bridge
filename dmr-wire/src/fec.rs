//! DMR FEC primitives for burst construction.
//!
//! Golay(20,8) for slot type, Hamming(15,11) / (13,9) for
//! BPTC(196,96), Hamming(16,11) for embedded LC, QR(16,7,6) for EMB.
//!
//! Encoders are always present.  Single-bit error correction for
//! Hamming(15,11,3) and Hamming(13,9,3) is available for future RF
//! reception use (the test-only BPTC decoder applies both).  The
//! production code path for BM-over-UDP does not need correction --
//! the DMRD flag byte carries frame-identification metadata and
//! UDP transport delivers clean bytes or drops them.

/// Golay(20,8,7) encode: 8 data bits -> 20 encoded bits.
/// Returns the 12 parity bits (data is the input byte).
///
/// Lookup table cross-checked against DMRGateway Golay2087.cpp.
pub(crate) fn golay_20_8_parity(data: u8) -> u16 {
    GOLAY_2087_TABLE[data as usize]
}

/// Hamming(15,11,3) encode: 11 data bits -> 4 parity bits.
///
/// Parity equations from DMRGateway Hamming.cpp encode15113_2.
/// This is the variant used by the BPTC(196,96) encoder.
pub(crate) fn hamming_15_11_parity(d: &[u8; 11]) -> [u8; 4] {
    [
        d[0] ^ d[1] ^ d[2] ^ d[3] ^ d[5] ^ d[7] ^ d[8],
        d[1] ^ d[2] ^ d[3] ^ d[4] ^ d[6] ^ d[8] ^ d[9],
        d[2] ^ d[3] ^ d[4] ^ d[5] ^ d[7] ^ d[9] ^ d[10],
        d[0] ^ d[1] ^ d[2] ^ d[4] ^ d[6] ^ d[7] ^ d[10],
    ]
}

/// Hamming(13,9,3) encode: 9 data bits -> 4 parity bits.
///
/// Parity equations from DMRGateway Hamming.cpp encode1393.
pub(crate) fn hamming_13_9_parity(d: &[u8; 9]) -> [u8; 4] {
    [
        d[0] ^ d[1] ^ d[3] ^ d[5] ^ d[6],
        d[0] ^ d[1] ^ d[2] ^ d[4] ^ d[6] ^ d[7],
        d[0] ^ d[1] ^ d[2] ^ d[3] ^ d[5] ^ d[7] ^ d[8],
        d[0] ^ d[2] ^ d[4] ^ d[5] ^ d[8],
    ]
}

/// Hamming(16,11,4) encode: 11 data bits -> 5 parity bits.
///
/// Parity equations from DMRGateway Hamming.cpp encode16114.
/// Used by the embedded LC block: each of the first 7 rows of the
/// 8x16 matrix carries 11 data bits + 5 Hamming parity bits.
pub(crate) fn hamming_16_11_parity(d: &[u8; 11]) -> [u8; 5] {
    [
        d[0] ^ d[1] ^ d[2] ^ d[3] ^ d[5] ^ d[7] ^ d[8],
        d[1] ^ d[2] ^ d[3] ^ d[4] ^ d[6] ^ d[8] ^ d[9],
        d[2] ^ d[3] ^ d[4] ^ d[5] ^ d[7] ^ d[9] ^ d[10],
        d[0] ^ d[1] ^ d[2] ^ d[4] ^ d[6] ^ d[7] ^ d[10],
        d[0] ^ d[2] ^ d[5] ^ d[6] ^ d[8] ^ d[9] ^ d[10],
    ]
}

// --- Single-bit error correction ---
//
// For each Hamming variant, the syndrome is the XOR of the received
// parity bits and the parity recomputed from the received data.
// Column j of the parity-check matrix H is the syndrome induced by
// flipping bit j; a single-bit error on data bit j yields syndrome
// H[.,j], and a single-bit error on parity bit i yields syndrome
// unit_i (value 2^i).
//
// We derive the syndrome -> position lookup by inspection of the
// parity equations in hamming_*_parity above.  The syndrome is
// packed as s0 at bit 0, s1 at bit 1, etc.
//
// Hamming(15,11,3) and Hamming(13,9,3) correct are pub(crate) at
// runtime: the dmr::bptc voice-LC validator uses them on every
// inbound voice header / terminator burst for log-only diagnostic
// detection.  Hamming(16,11,4) correct stays cfg(test) since no
// runtime consumer (the embedded LC decoder is also cfg(test)).

/// Hamming(15,11,3): distance 3 corrects one bit, cannot detect two.
/// All 15 non-zero 4-bit syndromes map uniquely to a bit position.
#[rustfmt::skip]
const SYNDROME_TO_POS_15_11: [usize; 16] = [
    // Syndrome bits are [s0, s1, s2, s3] little-endian.
    // Entry at index k is the error position (0..15) whose H column
    // equals k.  Index 0 (no error) is unused by correct().
    usize::MAX, // 0000: no error
    11,         // 0001: p0
    12,         // 0010: p1
    8,          // 0011: d8   (p0 + p1)
    13,         // 0100: p2
    5,          // 0101: d5   (p0 + p2)
    9,          // 0110: d9   (p1 + p2)
    3,          // 0111: d3   (p0 + p1 + p2)
    14,         // 1000: p3
    0,          // 1001: d0   (p0 + p3)
    6,          // 1010: d6   (p1 + p3)
    1,          // 1011: d1   (p0 + p1 + p3)
    10,         // 1100: d10  (p2 + p3)
    7,          // 1101: d7   (p0 + p2 + p3)
    4,          // 1110: d4   (p1 + p2 + p3)
    2,          // 1111: d2   (p0 + p1 + p2 + p3)
];

/// Hamming(13,9,3): distance 3.  13 bit positions fit in a 4-bit
/// syndrome (16 values); 13 single-bit syndromes + no-error = 14
/// valid, leaving 2 "impossible" syndromes that indicate an
/// uncorrectable multi-bit error.
#[rustfmt::skip]
const SYNDROME_TO_POS_13_9: [i8; 16] = [
    // -1 = no error (index 0) or uncorrectable (indices 9, 11).
    -1, //  0: no error
    9,  //  1: p0
    10, //  2: p1
    6,  //  3: d6   (p0 + p1)
    11, //  4: p2
    3,  //  5: d3   (p0 + p2)
    7,  //  6: d7   (p1 + p2)
    1,  //  7: d1   (p0 + p1 + p2)
    12, //  8: p3
    -1, //  9: uncorrectable
    4,  // 10: d4   (p1 + p3)
    -1, // 11: uncorrectable
    8,  // 12: d8   (p2 + p3)
    5,  // 13: d5   (p0 + p2 + p3)
    2,  // 14: d2   (p1 + p2 + p3)
    0,  // 15: d0   (all four)
];

/// Uncorrectable single-bit FEC error (2+ bits flipped).  Only
/// returned by variants with distance >= 4 or with unused syndromes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("uncorrectable Hamming error")]
pub(crate) struct HammingError;

/// Hamming(15,11,3) correct: take a 15-bit codeword and flip the
/// bit indicated by the syndrome if non-zero.  Always succeeds
/// (distance 3 can correct one error; multi-bit errors silently
/// miscorrect -- the caller cannot distinguish).  Returns whether
/// a bit was corrected, for diagnostics.
pub(crate) fn hamming_15_11_correct(cw: &mut [u8; 15]) -> bool {
    let d: [u8; 11] = cw[..11]
        .try_into()
        .expect("cw is [u8; 15], cw[..11] is len 11");
    let expected = hamming_15_11_parity(&d);
    let mut s = 0u8;
    for (i, &e) in expected.iter().enumerate() {
        if (cw[11 + i] & 1) != e {
            s |= 1 << i;
        }
    }
    if s == 0 {
        return false;
    }
    let pos = SYNDROME_TO_POS_15_11[s as usize];
    cw[pos] ^= 1;
    true
}

/// Hamming(13,9,3) correct: like the (15,11,3) variant but with
/// two unused syndromes (9, 11) that flag uncorrectable multi-bit
/// errors.  Returns whether a bit was corrected (Ok(false) means the
/// codeword was already valid, Ok(true) means one bit was flipped).
pub(crate) fn hamming_13_9_correct(cw: &mut [u8; 13]) -> Result<bool, HammingError> {
    let d: [u8; 9] = cw[..9]
        .try_into()
        .expect("cw is [u8; 13], cw[..9] is len 9");
    let expected = hamming_13_9_parity(&d);
    let mut s = 0u8;
    for (i, &e) in expected.iter().enumerate() {
        if (cw[9 + i] & 1) != e {
            s |= 1 << i;
        }
    }
    if s == 0 {
        return Ok(false);
    }
    let pos = SYNDROME_TO_POS_13_9[s as usize];
    if pos < 0 {
        return Err(HammingError);
    }
    cw[pos as usize] ^= 1;
    Ok(true)
}

/// Hamming(16,11,4): distance 4 corrects one bit AND detects two.
/// Of the 32 5-bit syndromes, 17 are valid (no-error + 16 single-bit
/// positions); the other 15 indicate uncorrectable multi-bit errors.
#[cfg(test)]
#[rustfmt::skip]
const SYNDROME_TO_POS_16_11: [i8; 32] = [
    // -1 = no error (0) or uncorrectable.
    -1, //  0: no error
    11, //  1: p0
    12, //  2: p1
    -1, //  3
    13, //  4: p2
    -1, //  5
    -1, //  6
    3,  //  7: d3
    14, //  8: p3
    -1, //  9
    -1, // 10
    1,  // 11: d1
    -1, // 12
    7,  // 13: d7
    4,  // 14: d4
    -1, // 15
    15, // 16: p4
    -1, // 17
    -1, // 18
    8,  // 19: d8
    -1, // 20
    5,  // 21: d5
    9,  // 22: d9
    -1, // 23
    -1, // 24
    0,  // 25: d0
    6,  // 26: d6
    -1, // 27
    10, // 28: d10
    -1, // 29
    -1, // 30
    2,  // 31: d2
];

/// Hamming(16,11,4) correct: distance-4 code, so we can detect 2-bit
/// errors as well as correct 1-bit.  Returns Err on any syndrome
/// outside the 17-element valid set.
#[cfg(test)]
pub(crate) fn hamming_16_11_correct(cw: &mut [u8; 16]) -> Result<(), HammingError> {
    let d: [u8; 11] = cw[..11].try_into().unwrap();
    let expected = hamming_16_11_parity(&d);
    let mut s = 0u8;
    for (i, &e) in expected.iter().enumerate() {
        if (cw[11 + i] & 1) != e {
            s |= 1 << i;
        }
    }
    if s == 0 {
        return Ok(());
    }
    let pos = SYNDROME_TO_POS_16_11[s as usize];
    if pos < 0 {
        return Err(HammingError);
    }
    cw[pos as usize] ^= 1;
    Ok(())
}

/// Decoded data + indication of whether the codeword was a valid
/// table entry.  Used by the lookup-table inverses below.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("codeword not in encoder table")]
pub(crate) struct LookupDecodeError;

/// Golay(20,8,7) decode.  Strips the 12-bit parity from a 20-bit
/// codeword (8 info MSB-first + 12 parity in DMRGateway byte order:
/// low byte then high nibble) and returns the recovered info byte
/// only if the codeword exactly matches an encoder output -- i.e.
/// the parity is consistent.  Distance 7 means up to 3 bit errors
/// could be corrected by a syndrome-based decoder; this lookup
/// variant detects bit errors as a mismatch but cannot correct.
#[cfg(test)]
pub(crate) fn golay_20_8_decode(bits: &[u8; 20]) -> Result<u8, LookupDecodeError> {
    let mut info: u8 = 0;
    for (i, &b) in bits[..8].iter().enumerate() {
        info |= (b & 1) << (7 - i);
    }
    // Re-pack the 12 received parity bits in the same order as
    // golay_20_8_parity: low byte (bits 8..16) MSB-first then high
    // nibble (bits 16..20) MSB-first.
    let mut received: u16 = 0;
    let mut lo: u8 = 0;
    for (i, &b) in bits[8..16].iter().enumerate() {
        lo |= (b & 1) << (7 - i);
    }
    received |= u16::from(lo);
    let mut hi: u8 = 0;
    for (i, &b) in bits[16..20].iter().enumerate() {
        hi |= (b & 1) << (7 - i);
    }
    received |= u16::from(hi) << 8;
    let expected = golay_20_8_parity(info);
    if expected == received {
        Ok(info)
    } else {
        Err(LookupDecodeError)
    }
}

/// QR(16,7,6) decode.  Recovers the 7 info bits (info << 0) from a
/// 16-bit codeword by table lookup; distance 6 means a syndrome
/// decoder could correct up to 2 errors but this variant is
/// lookup-only and detects mismatch as an error.
#[cfg(test)]
pub(crate) fn qr_16_7_decode(codeword: u16) -> Result<u8, LookupDecodeError> {
    // Linear scan of the 128-entry table -- small enough that the
    // overhead is negligible for tests.
    for info in 0..=0x7Fu8 {
        if QR_1676_TABLE[info as usize] == codeword {
            return Ok(info);
        }
    }
    Err(LookupDecodeError)
}

/// QR(16,7,6) encode: 7 data bits -> 16-bit codeword.
///
/// Lookup table cross-checked against DMRGateway QR1676.cpp.
pub(crate) fn qr_16_7_encode(data: u8) -> u16 {
    QR_1676_TABLE[(data & 0x7F) as usize]
}

// Golay(20,8) encoding table: 256 entries, indexed by 8-bit data.
// Each entry is the 12-bit parity.
// Cross-checked against DMRGateway Golay2087.cpp ENCODING_TABLE_2087.
#[rustfmt::skip]
const GOLAY_2087_TABLE: [u16; 256] = [
    0x0000, 0xB08E, 0xE093, 0x501D, 0x70A9, 0xC027, 0x903A, 0x20B4,
    0x60DC, 0xD052, 0x804F, 0x30C1, 0x1075, 0xA0FB, 0xF0E6, 0x4068,
    0x7036, 0xC0B8, 0x90A5, 0x202B, 0x009F, 0xB011, 0xE00C, 0x5082,
    0x10EA, 0xA064, 0xF079, 0x40F7, 0x6043, 0xD0CD, 0x80D0, 0x305E,
    0xD06C, 0x60E2, 0x30FF, 0x8071, 0xA0C5, 0x104B, 0x4056, 0xF0D8,
    0xB0B0, 0x003E, 0x5023, 0xE0AD, 0xC019, 0x7097, 0x208A, 0x9004,
    0xA05A, 0x10D4, 0x40C9, 0xF047, 0xD0F3, 0x607D, 0x3060, 0x80EE,
    0xC086, 0x7008, 0x2015, 0x909B, 0xB02F, 0x00A1, 0x50BC, 0xE032,
    0x90D9, 0x2057, 0x704A, 0xC0C4, 0xE070, 0x50FE, 0x00E3, 0xB06D,
    0xF005, 0x408B, 0x1096, 0xA018, 0x80AC, 0x3022, 0x603F, 0xD0B1,
    0xE0EF, 0x5061, 0x007C, 0xB0F2, 0x9046, 0x20C8, 0x70D5, 0xC05B,
    0x8033, 0x30BD, 0x60A0, 0xD02E, 0xF09A, 0x4014, 0x1009, 0xA087,
    0x40B5, 0xF03B, 0xA026, 0x10A8, 0x301C, 0x8092, 0xD08F, 0x6001,
    0x2069, 0x90E7, 0xC0FA, 0x7074, 0x50C0, 0xE04E, 0xB053, 0x00DD,
    0x3083, 0x800D, 0xD010, 0x609E, 0x402A, 0xF0A4, 0xA0B9, 0x1037,
    0x505F, 0xE0D1, 0xB0CC, 0x0042, 0x20F6, 0x9078, 0xC065, 0x70EB,
    0xA03D, 0x10B3, 0x40AE, 0xF020, 0xD094, 0x601A, 0x3007, 0x8089,
    0xC0E1, 0x706F, 0x2072, 0x90FC, 0xB048, 0x00C6, 0x50DB, 0xE055,
    0xD00B, 0x6085, 0x3098, 0x8016, 0xA0A2, 0x102C, 0x4031, 0xF0BF,
    0xB0D7, 0x0059, 0x5044, 0xE0CA, 0xC07E, 0x70F0, 0x20ED, 0x9063,
    0x7051, 0xC0DF, 0x90C2, 0x204C, 0x00F8, 0xB076, 0xE06B, 0x50E5,
    0x108D, 0xA003, 0xF01E, 0x4090, 0x6024, 0xD0AA, 0x80B7, 0x3039,
    0x0067, 0xB0E9, 0xE0F4, 0x507A, 0x70CE, 0xC040, 0x905D, 0x20D3,
    0x60BB, 0xD035, 0x8028, 0x30A6, 0x1012, 0xA09C, 0xF081, 0x400F,
    0x30E4, 0x806A, 0xD077, 0x60F9, 0x404D, 0xF0C3, 0xA0DE, 0x1050,
    0x5038, 0xE0B6, 0xB0AB, 0x0025, 0x2091, 0x901F, 0xC002, 0x708C,
    0x40D2, 0xF05C, 0xA041, 0x10CF, 0x307B, 0x80F5, 0xD0E8, 0x6066,
    0x200E, 0x9080, 0xC09D, 0x7013, 0x50A7, 0xE029, 0xB034, 0x00BA,
    0xE088, 0x5006, 0x001B, 0xB095, 0x9021, 0x20AF, 0x70B2, 0xC03C,
    0x8054, 0x30DA, 0x60C7, 0xD049, 0xF0FD, 0x4073, 0x106E, 0xA0E0,
    0x90BE, 0x2030, 0x702D, 0xC0A3, 0xE017, 0x5099, 0x0084, 0xB00A,
    0xF062, 0x40EC, 0x10F1, 0xA07F, 0x80CB, 0x3045, 0x6058, 0xD0D6,
];

// QR(16,7,6) encoding table: 128 entries, indexed by 7-bit data.
// Each entry is the full 16-bit codeword.
// Cross-checked against DMRGateway QR1676.cpp ENCODING_TABLE_1676.
#[rustfmt::skip]
const QR_1676_TABLE: [u16; 128] = [
    0x0000, 0x0273, 0x04E5, 0x0696, 0x09C9, 0x0BBA, 0x0D2C, 0x0F5F,
    0x11E2, 0x1391, 0x1507, 0x1774, 0x182B, 0x1A58, 0x1CCE, 0x1EBD,
    0x21B7, 0x23C4, 0x2552, 0x2721, 0x287E, 0x2A0D, 0x2C9B, 0x2EE8,
    0x3055, 0x3226, 0x34B0, 0x36C3, 0x399C, 0x3BEF, 0x3D79, 0x3F0A,
    0x417B, 0x4308, 0x459E, 0x47ED, 0x48B2, 0x4AC1, 0x4C57, 0x4E24,
    0x5099, 0x52EA, 0x547C, 0x560F, 0x5950, 0x5B23, 0x5DB5, 0x5FC6,
    0x60CC, 0x62BF, 0x6429, 0x665A, 0x6905, 0x6B76, 0x6DE0, 0x6F93,
    0x712E, 0x735D, 0x75CB, 0x77B8, 0x78E7, 0x7A94, 0x7C02, 0x7E71,
    0x82F6, 0x8085, 0x8613, 0x8460, 0x8B3F, 0x894C, 0x8FDA, 0x8DA9,
    0x9314, 0x9167, 0x97F1, 0x9582, 0x9ADD, 0x98AE, 0x9E38, 0x9C4B,
    0xA341, 0xA132, 0xA7A4, 0xA5D7, 0xAA88, 0xA8FB, 0xAE6D, 0xAC1E,
    0xB2A3, 0xB0D0, 0xB646, 0xB435, 0xBB6A, 0xB919, 0xBF8F, 0xBDFC,
    0xC38D, 0xC1FE, 0xC768, 0xC51B, 0xCA44, 0xC837, 0xCEA1, 0xCCD2,
    0xD26F, 0xD01C, 0xD68A, 0xD4F9, 0xDBA6, 0xD9D5, 0xDF43, 0xDD30,
    0xE23A, 0xE049, 0xE6DF, 0xE4AC, 0xEBF3, 0xE980, 0xEF16, 0xED65,
    0xF3D8, 0xF1AB, 0xF73D, 0xF54E, 0xFA11, 0xF862, 0xFEF4, 0xFC87,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golay_zero() {
        assert_eq!(golay_20_8_parity(0), 0);
    }

    #[test]
    fn golay_known_values() {
        // From DMRGateway table.
        assert_eq!(golay_20_8_parity(1), 0xB08E);
        assert_eq!(golay_20_8_parity(2), 0xE093);
    }

    #[test]
    fn hamming_15_11_zero() {
        let d = [0u8; 11];
        assert_eq!(hamming_15_11_parity(&d), [0, 0, 0, 0]);
    }

    #[test]
    fn hamming_15_11_all_ones() {
        let d = [1u8; 11];
        // encode15113_2 with all ones:
        // p0 = d0^d1^d2^d3^d5^d7^d8 = 1 (7 ones)
        // p1 = d1^d2^d3^d4^d6^d8^d9 = 1 (7 ones)
        // p2 = d2^d3^d4^d5^d7^d9^d10 = 1 (7 ones)
        // p3 = d0^d1^d2^d4^d6^d7^d10 = 1 (7 ones)
        assert_eq!(hamming_15_11_parity(&d), [1, 1, 1, 1]);
    }

    #[test]
    fn hamming_13_9_zero() {
        let d = [0u8; 9];
        assert_eq!(hamming_13_9_parity(&d), [0, 0, 0, 0]);
    }

    #[test]
    fn hamming_13_9_all_ones() {
        // p0 = d0^d1^d3^d5^d6 = 1 (5 ones = odd)
        // p1 = d0^d1^d2^d4^d6^d7 = 0 (6 ones = even)
        // p2 = d0^d1^d2^d3^d5^d7^d8 = 1 (7 ones = odd)
        // p3 = d0^d2^d4^d5^d8 = 1 (5 ones = odd)
        let d = [1u8; 9];
        assert_eq!(hamming_13_9_parity(&d), [1, 0, 1, 1]);
    }

    #[test]
    fn hamming_15_11_okdmrlib_vectors() {
        // Valid codeword from OK-DMR/ok-dmrlib's test_hamming_15_11_3.py.
        let d = [0, 1, 0, 1, 1, 0, 1, 0, 1, 1, 0];
        assert_eq!(hamming_15_11_parity(&d), [1, 0, 1, 1]);
    }

    #[test]
    fn hamming_13_9_okdmrlib_vectors() {
        // Valid codewords from OK-DMR/ok-dmrlib's test_hamming_13_9_3.py.
        let cases: [(&[u8; 9], &[u8; 4]); 3] = [
            (&[0, 1, 0, 1, 0, 1, 0, 1, 0], &[1, 0, 0, 1]),
            (&[0, 1, 1, 1, 0, 0, 1, 0, 1], &[1, 1, 0, 0]),
            (&[1, 1, 1, 1, 0, 0, 1, 0, 0], &[0, 0, 0, 0]),
        ];
        for (data, expected) in cases {
            assert_eq!(hamming_13_9_parity(data), *expected);
        }
    }

    #[test]
    fn hamming_16_11_zero() {
        let d = [0u8; 11];
        assert_eq!(hamming_16_11_parity(&d), [0; 5]);
    }

    #[test]
    fn hamming_16_11_all_ones() {
        // Each parity equation is the same XOR set as hamming_15_11
        // plus one additional bit for the 5th parity:
        // p0..p3 are the 4 Hamming(15,11) equations (each 7 ones = 1).
        // p4 = d0^d2^d5^d6^d8^d9^d10 = 7 ones = 1.
        let d = [1u8; 11];
        assert_eq!(hamming_16_11_parity(&d), [1, 1, 1, 1, 1]);
    }

    #[test]
    fn hamming_16_11_okdmrlib_vectors() {
        // Independent valid codewords from OK-DMR/ok-dmrlib's
        // test_hamming_16_11_4.py: data bits 0..11 + parity bits
        // 11..16 form a self-consistent encoded 16-bit word.
        let cases: [(&[u8; 11], &[u8; 5]); 2] = [
            (&[1, 1, 0, 1, 0, 1, 1, 0, 1, 1, 0], &[1, 1, 1, 1, 1]),
            (&[1, 0, 1, 1, 0, 0, 0, 1, 1, 0, 0], &[1, 1, 1, 1, 1]),
        ];
        for (data, expected) in cases {
            assert_eq!(hamming_16_11_parity(data), *expected);
        }
    }

    /// Build a Hamming(15,11,3) codeword from 11 data bits by
    /// appending the computed parity.
    fn make_15_11_codeword(data: [u8; 11]) -> [u8; 15] {
        let p = hamming_15_11_parity(&data);
        let mut cw = [0u8; 15];
        cw[..11].copy_from_slice(&data);
        cw[11..].copy_from_slice(&p);
        cw
    }

    fn make_13_9_codeword(data: [u8; 9]) -> [u8; 13] {
        let p = hamming_13_9_parity(&data);
        let mut cw = [0u8; 13];
        cw[..9].copy_from_slice(&data);
        cw[9..].copy_from_slice(&p);
        cw
    }

    #[test]
    fn hamming_15_11_correct_clean_passthrough() {
        let cw = make_15_11_codeword([1, 0, 1, 1, 0, 1, 0, 0, 1, 1, 0]);
        let mut c = cw;
        hamming_15_11_correct(&mut c);
        assert_eq!(c, cw, "clean codeword modified by correct");
    }

    #[test]
    fn hamming_15_11_correct_flips_every_bit() {
        // For each of the 15 bit positions, flip it in a valid
        // codeword, run correct, and verify recovery.
        let base = make_15_11_codeword([1, 1, 0, 1, 0, 1, 1, 0, 1, 1, 0]);
        for pos in 0..15 {
            let mut cw = base;
            cw[pos] ^= 1;
            hamming_15_11_correct(&mut cw);
            assert_eq!(cw, base, "did not correct bit {pos}");
        }
    }

    #[test]
    fn hamming_13_9_correct_clean_passthrough() {
        let cw = make_13_9_codeword([0, 1, 0, 1, 0, 1, 0, 1, 0]);
        let mut c = cw;
        hamming_13_9_correct(&mut c).unwrap();
        assert_eq!(c, cw);
    }

    #[test]
    fn hamming_13_9_correct_flips_every_bit() {
        let base = make_13_9_codeword([1, 1, 1, 1, 0, 0, 1, 0, 0]);
        for pos in 0..13 {
            let mut cw = base;
            cw[pos] ^= 1;
            hamming_13_9_correct(&mut cw).unwrap();
            assert_eq!(cw, base, "did not correct bit {pos}");
        }
    }

    fn make_16_11_codeword(data: [u8; 11]) -> [u8; 16] {
        let p = hamming_16_11_parity(&data);
        let mut cw = [0u8; 16];
        cw[..11].copy_from_slice(&data);
        cw[11..].copy_from_slice(&p);
        cw
    }

    #[test]
    fn hamming_16_11_correct_clean_passthrough() {
        let cw = make_16_11_codeword([1, 1, 0, 1, 0, 1, 1, 0, 1, 1, 0]);
        let mut c = cw;
        hamming_16_11_correct(&mut c).unwrap();
        assert_eq!(c, cw);
    }

    #[test]
    fn hamming_16_11_correct_flips_every_bit() {
        let base = make_16_11_codeword([1, 0, 1, 1, 0, 0, 0, 1, 1, 0, 0]);
        for pos in 0..16 {
            let mut cw = base;
            cw[pos] ^= 1;
            hamming_16_11_correct(&mut cw).unwrap();
            assert_eq!(cw, base, "did not correct bit {pos}");
        }
    }

    #[test]
    fn hamming_16_11_correct_detects_double_errors() {
        // Distance-4 code detects all 2-bit errors.  Any pair of
        // single-bit syndromes XOR'd together either equals another
        // single-bit syndrome (would miscorrect, but the code is
        // designed to avoid this for distance 4) or hits an invalid
        // syndrome we report as Err.
        //
        // Sweep all C(16,2) = 120 distinct pairs and assert that at
        // most no miscorrection silently passes -- i.e., every pair
        // either returns Err or returns Ok with miscorrection that we
        // can verify by re-encoding.
        let base = make_16_11_codeword([0, 1, 1, 0, 1, 0, 0, 1, 1, 0, 1]);
        let mut detected = 0usize;
        for i in 0..16 {
            for j in (i + 1)..16 {
                let mut cw = base;
                cw[i] ^= 1;
                cw[j] ^= 1;
                if hamming_16_11_correct(&mut cw).is_err() {
                    detected += 1;
                }
            }
        }
        // Distance 4 -> all 120 double-bit errors should be detected
        // (some go to invalid syndromes; the rest map to a single-bit
        // position and would miscorrect, but in distance 4 the
        // syndromes from any 2-bit error are NOT in the single-bit
        // set, so all should be detected).
        assert_eq!(detected, 120, "expected all double-errors detected");
    }

    #[test]
    fn hamming_13_9_correct_detects_uncorrectable() {
        // Syndromes 9 and 11 are not reachable by any single-bit
        // error.  Flipping specific bit pairs produces those
        // syndromes and must return HammingError.
        //
        // Syndrome 9 = 0b1001 = syndrome(p0) XOR syndrome(p3) =
        // single-bit error on p0 XOR'd with single-bit error on p3.
        // So flipping both parity bits 0 and 3 yields syndrome 9.
        let mut cw = make_13_9_codeword([1, 0, 0, 1, 0, 1, 1, 0, 1]);
        cw[9] ^= 1; // flip p0
        cw[12] ^= 1; // flip p3
        assert_eq!(hamming_13_9_correct(&mut cw), Err(HammingError));

        // Syndrome 11 = 0b1011 similarly comes from a 2-bit error.
        let mut cw = make_13_9_codeword([0, 1, 1, 0, 1, 0, 0, 1, 1]);
        cw[9] ^= 1; // flip p0
        cw[10] ^= 1; // flip p1
        cw[12] ^= 1; // flip p3
        // 3-bit error that happens to produce syndrome 11 -- also
        // flagged as uncorrectable.
        assert_eq!(hamming_13_9_correct(&mut cw), Err(HammingError));
    }

    #[test]
    fn qr_zero() {
        assert_eq!(qr_16_7_encode(0), 0x0000);
    }

    #[test]
    fn qr_known_values() {
        assert_eq!(qr_16_7_encode(1), 0x0273);
        assert_eq!(qr_16_7_encode(0x7F), 0xFC87);
    }

    /// Pack 8 info bits (MSB-first) + 12 parity bits (low byte
    /// MSB-first then high nibble MSB-first) into a 20-bit array,
    /// matching what bptc.rs::encode_slot_type emits.
    #[expect(
        clippy::needless_range_loop,
        reason = "index used for both position and shift"
    )]
    fn encode_golay_20_8(info: u8) -> [u8; 20] {
        let parity = golay_20_8_parity(info);
        let mut bits = [0u8; 20];
        for i in 0..8 {
            bits[i] = (info >> (7 - i)) & 1;
        }
        let lo = (parity & 0xFF) as u8;
        let hi = ((parity >> 8) & 0xFF) as u8;
        for i in 0..8 {
            bits[8 + i] = (lo >> (7 - i)) & 1;
        }
        for i in 0..4 {
            bits[16 + i] = (hi >> (7 - i)) & 1;
        }
        bits
    }

    #[test]
    fn golay_20_8_round_trip_all_inputs() {
        // Every 8-bit input must round-trip through encode -> decode.
        for info in 0..=255u8 {
            let bits = encode_golay_20_8(info);
            assert_eq!(golay_20_8_decode(&bits), Ok(info));
        }
    }

    #[test]
    fn golay_20_8_decode_detects_single_bit_flip() {
        // Distance-7 code: any 1-, 2-, or 3-bit error is detected
        // as a mismatch (this lookup variant doesn't correct).
        let info = 0xA5;
        let base = encode_golay_20_8(info);
        for pos in 0..20 {
            let mut bits = base;
            bits[pos] ^= 1;
            assert_eq!(
                golay_20_8_decode(&bits),
                Err(LookupDecodeError),
                "bit flip at {pos} not detected"
            );
        }
    }

    #[test]
    fn qr_16_7_round_trip_all_inputs() {
        // Every 7-bit input must round-trip through encode -> decode.
        for info in 0..=0x7Fu8 {
            let cw = qr_16_7_encode(info);
            assert_eq!(qr_16_7_decode(cw), Ok(info));
        }
    }

    #[test]
    fn qr_16_7_decode_detects_random_codeword() {
        // A 16-bit value not in the encoder table must Err.  All
        // table entries differ from each other; pick any one and
        // flip a bit -- the result is unlikely (1/2^9 chance) to
        // collide.
        let mut cw = qr_16_7_encode(0x42);
        cw ^= 1; // flip LSB
        assert_eq!(qr_16_7_decode(cw), Err(LookupDecodeError));
    }
}
