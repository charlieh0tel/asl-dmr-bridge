//! Talker Alias encoder (ETSI TS 102 361-2 sec. 7.2.21-22).
//!
//! Embedded in voice bursts B-E alongside the regular voice LC,
//! cycled across superframes so a receiving radio can display the
//! talker's callsign in addition to (or instead of) the bare DMR
//! ID.  Each superframe carries one LC body (voice LC, TA Header,
//! or one TA Block) on a round-robin schedule.
//!
//! 7-bit ASCII format only.  An alias up to `MAX_ALIAS_CHARS` chars
//! is split across:
//!
//! - 1 TA Header (FLCO 4) carrying the total length and the first 7
//!   chars (49 bits of TA_Data).
//! - 0..=3 TA Blocks (FLCO 5/6/7), each carrying up to 8 chars
//!   (56 bits of TA_Data).
//!
//! Non-ASCII, empty, or over-length aliases produce an empty
//! sequence and the caller falls back to voice-only LC.
//!
//! 72-bit LC body layout:
//! ```text
//! Header (FLCO=4):
//!   PF(1) | reserved(1) | FLCO(6=0x04) | FID(8=0)
//!     | TA_Format(2=0) | TA_Length(5) | TA_Data(49 bits)
//!
//! Block (FLCO=5/6/7):
//!   PF(1) | reserved(1) | FLCO(6) | FID(8=0) | TA_Data(56 bits)
//! ```
//! TA_Data carries 7-bit ASCII chars MSB-first, left-justified,
//! with trailing zero-padding when the chunk is shorter than its
//! capacity.

/// Bits of TA_Data in the header LC.
const HEADER_DATA_BITS: usize = 49;
/// Bits of TA_Data in each block LC.
const BLOCK_DATA_BITS: usize = 56;
/// 7-bit ASCII bits per character.
const BITS_PER_CHAR: usize = 7;
/// Chars that fit in the header (49 / 7 = 7).
const HEADER_CHARS: usize = HEADER_DATA_BITS / BITS_PER_CHAR;
/// Chars that fit in one block (56 / 7 = 8).
const BLOCK_CHARS: usize = BLOCK_DATA_BITS / BITS_PER_CHAR;
/// Max blocks after the header (3 * 8 = 24 chars after the first 7).
const MAX_BLOCKS: usize = 3;
/// Max total chars across header + 3 blocks (7 + 24 = 31).
pub(crate) const MAX_ALIAS_CHARS: usize = HEADER_CHARS + MAX_BLOCKS * BLOCK_CHARS;

/// FLCO values for TA LCs (ETSI Table 7.13).
const FLCO_TA_HEADER: u8 = 0x04;
const FLCO_TA_BLOCK_1: u8 = 0x05;
const FLCO_TA_BLOCK_2: u8 = 0x06;
const FLCO_TA_BLOCK_3: u8 = 0x07;

/// Bits in the LC body fed to the embedded LC encoder.
const LC_BITS: usize = 72;

/// Encode `text` as the sequence of LC bodies (header + 0..=3 blocks)
/// the embedded LC encoder will rotate through.  Empty / non-ASCII /
/// over-length input returns an empty Vec and the caller skips TA
/// emission.
pub(crate) fn encode_talker_alias_lcs(text: &str) -> Vec<[u8; LC_BITS]> {
    if text.is_empty() || text.len() > MAX_ALIAS_CHARS || !text.is_ascii() {
        return Vec::new();
    }
    let total = text.len() as u8;
    let head = &text[..text.len().min(HEADER_CHARS)];
    let tail = &text[head.len()..];
    let block_flcos = [FLCO_TA_BLOCK_1, FLCO_TA_BLOCK_2, FLCO_TA_BLOCK_3];

    let mut out = Vec::with_capacity(1 + tail.len().div_ceil(BLOCK_CHARS));
    out.push(bytes_to_bits(&encode_header_bytes(total, head)));
    for (i, chunk) in tail.as_bytes().chunks(BLOCK_CHARS).enumerate() {
        out.push(bytes_to_bits(&encode_block_bytes(block_flcos[i], chunk)));
    }
    out
}

/// Encode the TA Header LC body.  `total` is the alias's total
/// character count (across header + all blocks); `head` is the first
/// `HEADER_CHARS` (or fewer) characters that go into the header's
/// own TA_Data.
fn encode_header_bytes(total: u8, head: &str) -> [u8; 9] {
    debug_assert!(head.is_ascii() && head.len() <= HEADER_CHARS);
    let mut lc = [0u8; 9];
    lc[0] = FLCO_TA_HEADER;
    // lc[1] = FID = 0x00 (standard)

    // Pack head chars MSB-first into a u64, then left-justify in
    // the 49-bit TA_Data field.
    let data = pack_chars_msb_first(head.as_bytes(), HEADER_DATA_BITS);

    // byte 2: TA_Format(2=0) | TA_Length(5) | TA_Data[0] (1 bit)
    let ta_data_bit_0 = ((data >> 48) & 1) as u8;
    lc[2] = (total << 1) | ta_data_bit_0;

    // bytes 3..=8: TA_Data[1..49] = 48 bits, big-endian.
    let lower48 = data & ((1u64 << 48) - 1);
    for (i, byte) in lc[3..].iter_mut().enumerate() {
        *byte = ((lower48 >> (40 - i * 8)) & 0xFF) as u8;
    }
    lc
}

/// Encode a TA Block LC body for `flco` (FLCO 5/6/7) holding up to
/// `BLOCK_CHARS` chars in 7-bit ASCII.
fn encode_block_bytes(flco: u8, chunk: &[u8]) -> [u8; 9] {
    debug_assert!(matches!(
        flco,
        FLCO_TA_BLOCK_1 | FLCO_TA_BLOCK_2 | FLCO_TA_BLOCK_3
    ));
    debug_assert!(chunk.len() <= BLOCK_CHARS);
    let mut lc = [0u8; 9];
    lc[0] = flco;
    // lc[1] = FID = 0x00 (standard)

    // Pack chunk MSB-first, left-justified in 56 bits.  Result fits
    // in u64 since BLOCK_DATA_BITS == 56 < 64.
    let data = pack_chars_msb_first(chunk, BLOCK_DATA_BITS);

    // bytes 2..=8: 7 bytes = 56 bits, big-endian.
    for (i, byte) in lc[2..].iter_mut().enumerate() {
        *byte = ((data >> (48 - i * 8)) & 0xFF) as u8;
    }
    lc
}

/// Pack 7-bit ASCII bytes MSB-first into a u64 and left-justify the
/// result in `field_bits` (the data field width).  Excess bits at
/// the top of the u64 are zero; the caller masks/shifts to extract
/// the field.
fn pack_chars_msb_first(chars: &[u8], field_bits: usize) -> u64 {
    debug_assert!(field_bits <= 56);
    debug_assert!(chars.len() * BITS_PER_CHAR <= field_bits);
    let mut data: u64 = 0;
    let mut data_bits: u32 = 0;
    for &ch in chars {
        // Caller guarantees ASCII (top bit unset); mask defensively.
        data = (data << BITS_PER_CHAR) | u64::from(ch & 0x7F);
        data_bits += BITS_PER_CHAR as u32;
    }
    // Shift left so populated bits sit at the top of the field.
    data << (field_bits as u32 - data_bits)
}

/// Expand 9 bytes into 72 individual bits, MSB-first per byte.
/// Matches the `[u8; 72]` shape that `embedded_lc::build_fragments`
/// expects (one element = one bit value).
fn bytes_to_bits(bytes: &[u8; 9]) -> [u8; LC_BITS] {
    let mut bits = [0u8; LC_BITS];
    for (byte_idx, &b) in bytes.iter().enumerate() {
        for bit in 0..8 {
            bits[byte_idx * 8 + bit] = (b >> (7 - bit)) & 1;
        }
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_alias() {
        assert!(encode_talker_alias_lcs("").is_empty());
    }

    #[test]
    fn rejects_over_max() {
        // 32 chars > MAX_ALIAS_CHARS (31).
        let too_long = "A".repeat(MAX_ALIAS_CHARS + 1);
        assert!(encode_talker_alias_lcs(&too_long).is_empty());
    }

    #[test]
    fn rejects_non_ascii() {
        assert!(encode_talker_alias_lcs("ABC\u{00FC}DE").is_empty());
    }

    #[test]
    fn one_lc_for_seven_or_fewer_chars() {
        for n in 1..=HEADER_CHARS {
            let s = "A".repeat(n);
            assert_eq!(encode_talker_alias_lcs(&s).len(), 1);
        }
    }

    #[test]
    fn two_lcs_for_eight_to_fifteen_chars() {
        for n in (HEADER_CHARS + 1)..=(HEADER_CHARS + BLOCK_CHARS) {
            let s = "A".repeat(n);
            assert_eq!(encode_talker_alias_lcs(&s).len(), 2);
        }
    }

    #[test]
    fn four_lcs_for_max_chars() {
        let s = "A".repeat(MAX_ALIAS_CHARS);
        assert_eq!(encode_talker_alias_lcs(&s).len(), 1 + MAX_BLOCKS);
    }

    #[test]
    fn header_abcde_matches_hand_derived_bytes() {
        // Hand-derived from ETSI TS 102 361-2 sec. 7.2.21:
        //   FLCO=4, FID=0
        //   TA_Format=0, TA_Length=5, then 49-bit TA_Data left-
        //     justified holding 'A','B','C','D','E' (5 * 7 = 35 bits)
        //     plus 14 zero pad bits.
        // 'A'=0x41=0b1000001 'B'=0x42=0b1000010 'C'=0x43=0b1000011
        // 'D'=0x44=0b1000100 'E'=0x45=0b1000101
        let lc = encode_header_bytes(5, "ABCDE");
        assert_eq!(lc, [0x04, 0x00, 0x0B, 0x06, 0x14, 0x38, 0x91, 0x40, 0x00]);
    }

    #[test]
    fn header_seven_chars_fully_fills_ta_data() {
        // 7 chars = 49 bits = exactly the TA_Data field, no padding.
        let lc = encode_header_bytes(7, "ABCDEFG");
        assert_eq!(lc[0], 0x04);
        // TA_Length = 7 = 0b00111 -> byte 2 bits 1..6
        // TA_Data bit 0 = MSB of 'A' = 1
        // byte 2 = 00 00111 1 = 0b00001111 = 0x0F
        assert_eq!(lc[2], 0x0F);
    }

    #[test]
    fn header_stamps_total_length_not_head_length() {
        // Alias of 15 chars: header carries 7 chars, but TA_Length
        // must reflect the full 15.
        let lc = encode_header_bytes(15, "ABCDEFG");
        // TA_Length = 15 = 0b01111 -> byte 2 bits 1..6
        // TA_Data bit 0 = MSB of 'A' = 1
        // byte 2 = 00 01111 1 = 0b00011111 = 0x1F
        assert_eq!(lc[2], 0x1F);
    }

    #[test]
    fn block_flco_byte_0() {
        assert_eq!(encode_block_bytes(FLCO_TA_BLOCK_1, b"")[0], 0x05);
        assert_eq!(encode_block_bytes(FLCO_TA_BLOCK_2, b"")[0], 0x06);
        assert_eq!(encode_block_bytes(FLCO_TA_BLOCK_3, b"")[0], 0x07);
    }

    #[test]
    fn block_eight_chars_fully_fills_ta_data() {
        // 8 chars = 56 bits = exactly the block's TA_Data, no pad.
        // 'A'..'H' MSB-first 7-bit -> packed bits:
        //   1000001 1000010 1000011 1000100 1000101 1000110 1000111 1001000
        // = 10000011 00001010 00011100 01001000 10110001 10100011 11001000
        // = 0x83 0x0A 0x1C 0x48 0xB1 0xA3 0xC8
        let lc = encode_block_bytes(FLCO_TA_BLOCK_1, b"ABCDEFGH");
        assert_eq!(&lc[2..9], &[0x83, 0x0A, 0x1C, 0x48, 0xB1, 0xA3, 0xC8]);
    }

    #[test]
    fn block_short_chunk_zero_pads_right() {
        // 1 char = 7 bits used, 49 bits zero pad.
        // 'A' = 0x41 = 0b1000001 left-justified in 56 bits
        // top 7 bits set: 1000001 0000... = byte 2 = 10000010 = 0x82
        let lc = encode_block_bytes(FLCO_TA_BLOCK_1, b"A");
        assert_eq!(lc[2], 0x82);
        assert_eq!(&lc[3..9], &[0; 6]);
    }

    #[test]
    fn lcs_for_15_char_alias_split_correctly() {
        // 15 chars: header carries 'ABCDEFG', block 1 carries 'HIJKLMNO'.
        // Verify TA_Length stamping and FLCO sequence.
        let lcs = encode_talker_alias_lcs("ABCDEFGHIJKLMNO");
        assert_eq!(lcs.len(), 2);
        // Header byte 0 = FLCO 4 -> bits 0..8 = 0,0,0,0,0,1,0,0
        assert_eq!(&lcs[0][0..8], &[0, 0, 0, 0, 0, 1, 0, 0]);
        // Block 1 byte 0 = FLCO 5 -> bits 0..8 = 0,0,0,0,0,1,0,1
        assert_eq!(&lcs[1][0..8], &[0, 0, 0, 0, 0, 1, 0, 1]);
    }

    #[test]
    fn bytes_to_bits_round_trips_msb_first() {
        let bytes = [0x80, 0x01, 0x55, 0xAA, 0x00, 0xFF, 0x42, 0x69, 0x96];
        let bits = bytes_to_bits(&bytes);
        // 0x80 = 1,0,0,0,0,0,0,0
        assert_eq!(&bits[0..8], &[1, 0, 0, 0, 0, 0, 0, 0]);
        // 0x01 = 0,0,0,0,0,0,0,1
        assert_eq!(&bits[8..16], &[0, 0, 0, 0, 0, 0, 0, 1]);
        // 0x55 = 0,1,0,1,0,1,0,1
        assert_eq!(&bits[16..24], &[0, 1, 0, 1, 0, 1, 0, 1]);
    }
}
