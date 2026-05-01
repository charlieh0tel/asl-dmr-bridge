//! Talker Alias Header LC encoder (ETSI TS 102 361-2 §7.2.21).
//!
//! Embedded in voice bursts B-E alongside the regular voice LC,
//! cycled once per superframe so a receiving radio can display the
//! talker's callsign in addition to (or instead of) the bare DMR ID.
//!
//! Scope: TA Header only (FLCO = 4), 7-bit ASCII format, callsigns up
//! to 7 characters.  Longer aliases need TA Blocks (FLCO 5/6/7) which
//! span four superframes; not implemented here -- see docs/TODO.md.
//!
//! 72-bit LC body layout:
//! ```text
//!   PF(1) | reserved(1) | FLCO(6=0x04) | FID(8=0)
//!     | TA_Format(2=0) | TA_Length(5) | TA_Data(49 bits)
//! ```
//! TA_Data carries up to 7 chars * 7 bits each = 49 bits, MSB-first,
//! left-justified, with trailing zero-padding when the alias is
//! shorter.

/// Maximum chars that fit in the TA header alone (49 bits / 7 bits
/// per ASCII char).
const MAX_ASCII_CHARS: usize = 7;

/// FLCO value for "Talker Alias Header" (ETSI Table 7.13).
const FLCO_TA_HEADER: u8 = 0x04;

/// Bits in the LC body fed to the embedded LC encoder.
const LC_BITS: usize = 72;

/// Encode a callsign / short alias as a 72-bit LC body suitable for
/// passing to `embedded_lc::build_fragments`.  Returns `None` if the
/// alias is empty, longer than `MAX_ASCII_CHARS`, or contains
/// non-ASCII characters -- caller should fall back to voice-only LC
/// in that case.
pub(crate) fn encode_ta_header_bits(text: &str) -> Option<[u8; LC_BITS]> {
    if text.is_empty() || text.len() > MAX_ASCII_CHARS || !text.is_ascii() {
        return None;
    }
    Some(bytes_to_bits(&encode_ta_header_bytes(text)))
}

/// Lower-level: produce the 9-byte LC body.  Split out so tests can
/// assert against known byte values rather than expanded bit arrays.
fn encode_ta_header_bytes(text: &str) -> [u8; 9] {
    debug_assert!(text.is_ascii() && (1..=MAX_ASCII_CHARS).contains(&text.len()));
    let mut lc = [0u8; 9];
    lc[0] = FLCO_TA_HEADER;
    // lc[1] = FID = 0x00 (standard)

    // Pack the 7-bit ASCII chars into a u64, MSB-first.  After the
    // loop, `data_bits` is the actual number of populated bits
    // (chars * 7); shift left by `49 - data_bits` so the data is
    // left-justified within the 49-bit TA_Data field.
    let mut data: u64 = 0;
    let mut data_bits: u32 = 0;
    for ch in text.bytes() {
        // is_ascii() above guarantees ch <= 0x7F.
        data = (data << 7) | u64::from(ch & 0x7F);
        data_bits += 7;
    }
    data <<= 49 - data_bits;

    // byte 2: TA_Format(2=0) | TA_Length(5) | TA_Data[0] (1 bit)
    let ta_length = text.len() as u8;
    let ta_data_bit_0 = ((data >> 48) & 1) as u8;
    lc[2] = (ta_length << 1) | ta_data_bit_0;

    // bytes 3..=8: TA_Data[1..49] = 48 bits, big-endian.
    let lower48 = data & ((1u64 << 48) - 1);
    for i in 0..6 {
        lc[3 + i] = ((lower48 >> (40 - i * 8)) & 0xFF) as u8;
    }
    lc
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
        assert!(encode_ta_header_bits("").is_none());
    }

    #[test]
    fn rejects_over_seven_chars() {
        assert!(encode_ta_header_bits("EIGHTCHR").is_none());
    }

    #[test]
    fn rejects_non_ascii() {
        assert!(encode_ta_header_bits("ABC\u{00FC}DE").is_none());
    }

    #[test]
    fn abcde_matches_hand_derived_bytes() {
        // Hand-derived from ETSI TS 102 361-2 §7.2.21:
        //   FLCO=4, FID=0
        //   TA_Format=0, TA_Length=5, then 49-bit TA_Data left-
        //     justified holding 'A','B','C','D','E' (5 * 7 = 35 bits)
        //     plus 14 zero pad bits.
        // 'A'=0x41=0b1000001 'B'=0x42=0b1000010 'C'=0x43=0b1000011
        // 'D'=0x44=0b1000100 'E'=0x45=0b1000101
        let lc = encode_ta_header_bytes("ABCDE");
        assert_eq!(lc, [0x04, 0x00, 0x0B, 0x06, 0x14, 0x38, 0x91, 0x40, 0x00]);
    }

    #[test]
    fn single_char_fits() {
        let lc = encode_ta_header_bytes("A");
        assert_eq!(lc[0], 0x04); // FLCO
        assert_eq!(lc[1], 0x00); // FID
        // TA_Length = 1 -> bits 2..6 = 00001
        // TA_Data bit 0 = MSB of 'A' = 1
        // byte 2 = 00 00001 1 = 0b00000011 = 0x03
        assert_eq!(lc[2], 0x03);
    }

    #[test]
    fn seven_chars_fully_fills_ta_data() {
        // 7 chars = 49 bits = exactly the TA_Data field, no padding.
        let lc = encode_ta_header_bytes("ABCDEFG");
        assert_eq!(lc[0], 0x04);
        // TA_Length = 7 = 0b00111 -> byte 2 bits 1..6
        // TA_Data bit 0 = MSB of 'A' = 1
        // byte 2 = 00 00111 1 = 0b00001111 = 0x0F
        assert_eq!(lc[2], 0x0F);
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

    #[test]
    fn bits_output_matches_byte_packing_for_abcde() {
        let bits = encode_ta_header_bits("ABCDE").unwrap();
        // Spot-check: byte 0 = 0x04 -> bits 0..8 = 0,0,0,0,0,1,0,0
        assert_eq!(&bits[0..8], &[0, 0, 0, 0, 0, 1, 0, 0]);
        // byte 2 = 0x0B -> bits 16..24 = 0,0,0,0,1,0,1,1
        assert_eq!(&bits[16..24], &[0, 0, 0, 0, 1, 0, 1, 1]);
    }
}
