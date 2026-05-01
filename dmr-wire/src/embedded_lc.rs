//! Embedded LC encoder for DMR voice bursts B-E.
//!
//! ETSI TS 102 361-1 section 9.3.5.  The 72-bit Link Control is
//! spread across four voice bursts (B, C, D, E) in the middle of a
//! voice superframe, plus a 5-bit CRC, Hamming(16,11,4) row parity,
//! and a 16-bit column parity row -- 128 bits total, arranged as an
//! 8-row x 16-column matrix, then serialized column-wise into four
//! 32-bit fragments.  Burst F carries LCSS=0 (null / reverse
//! channel).
//!
//! LCSS sequencing on the air:
//!   B: LCSS=1 (first)  -> fragment 0
//!   C: LCSS=3 (cont.)  -> fragment 1
//!   D: LCSS=3 (cont.)  -> fragment 2
//!   E: LCSS=2 (last)   -> fragment 3
//!   F: LCSS=0 (null)
//!
//! Algorithm cross-checked against DMRGateway DMREmbeddedData.cpp
//! (encodeEmbeddedData), Hamming.cpp (encode16114), and CRC.cpp
//! (encodeFiveBit).

#[cfg(test)]
use super::fec::HammingError;
#[cfg(test)]
use super::fec::hamming_16_11_correct;
use super::fec::hamming_16_11_parity;

const RAW_BITS: usize = 128;
const DATA_BITS: usize = 72;
const ROW_BITS: usize = 16;
const FRAGMENT_BITS: usize = 32;
const FRAGMENT_BYTES: usize = FRAGMENT_BITS / 8;

/// LCSS values per ETSI TS 102 361-1 section 9.3.5.
pub(crate) const LCSS_FIRST: u8 = 1;
pub(crate) const LCSS_CONTINUATION: u8 = 3;
pub(crate) const LCSS_LAST: u8 = 2;
pub(crate) const LCSS_NULL: u8 = 0;

/// 5-bit CRC for embedded LC: sum of the 9 LC bytes, mod 31.
/// Per DMRGateway CRC.cpp::encodeFiveBit.
fn crc5_embedded(lc_bytes: &[u8; 9]) -> u8 {
    let mut total: u16 = 0;
    for &b in lc_bytes {
        total += u16::from(b);
    }
    (total % 31) as u8
}

/// Pack the 72-bit LC + 5-bit CRC + row/column parity into the
/// 128-bit raw bitstream ready for fragmentation.  Column-serialized
/// (step 16, wrap at 127).
///
/// `lc_bits` is the same 72-bit LC used in the BPTC voice header
/// (PF + FLCO + FID + opts + dst_id + src_id, without RS parity).
fn encode_raw(lc_bits: &[u8; DATA_BITS]) -> [u8; RAW_BITS] {
    // Matrix layout (rows are groups of 16 consecutive bits):
    //   row 0 (bits   0.. 15): lc_bits[ 0..11] at cols 0..10, Hamming at 11..15
    //   row 1 (bits  16.. 31): lc_bits[11..22] at cols 0..10, Hamming at 11..15
    //   rows 2-6 (bits 32..111): 10 lc bits at cols 0..9, CRC bit at col 10, Hamming at 11..15
    //   row 7 (bits 112..127): column parity over rows 0..6
    //
    // 11*2 + 10*5 = 72 lc bits used; 5 CRC bits placed at col 10 of rows 2..6.
    let mut lc_bytes = [0u8; 9];
    for (i, byte) in lc_bytes.iter_mut().enumerate() {
        let mut v: u8 = 0;
        for bit in 0..8 {
            v |= (lc_bits[i * 8 + bit] & 1) << (7 - bit);
        }
        *byte = v;
    }
    let crc = crc5_embedded(&lc_bytes);

    let mut mat = [0u8; RAW_BITS];

    // Fill LC data bits.  Row 0: 11 bits at 0..11.  Row 1: 11 bits at
    // 16..27.  Rows 2..6: 10 bits each at r*16..r*16+10.
    mat[0..11].copy_from_slice(&lc_bits[0..11]);
    mat[16..27].copy_from_slice(&lc_bits[11..22]);
    let mut b = 22;
    for row_start in [32, 48, 64, 80, 96] {
        mat[row_start..row_start + 10].copy_from_slice(&lc_bits[b..b + 10]);
        b += 10;
    }
    debug_assert_eq!(b, DATA_BITS);

    // Place CRC bits at col 10 of rows 2..6 (bit positions 42, 58, 74, 90, 106).
    // Order: MSB of CRC at row 2, LSB at row 6.
    mat[42] = (crc >> 4) & 1;
    mat[58] = (crc >> 3) & 1;
    mat[74] = (crc >> 2) & 1;
    mat[90] = (crc >> 1) & 1;
    mat[106] = crc & 1;

    // Hamming(16,11,4) row parity on rows 0..6 (positions 11..15 of each row).
    for row in mat[..112].chunks_exact_mut(ROW_BITS) {
        let d: [u8; 11] = row[..11]
            .try_into()
            .expect("chunks_exact_mut yields ROW_BITS-sized slices");
        let parity = hamming_16_11_parity(&d);
        row[11..].copy_from_slice(&parity);
    }

    // Column parity on row 7 (positions 112..127) over rows 0..6.
    let (rows, row7) = mat.split_at_mut(112);
    for (col, slot) in row7.iter_mut().enumerate() {
        *slot = rows[col]
            ^ rows[16 + col]
            ^ rows[32 + col]
            ^ rows[48 + col]
            ^ rows[64 + col]
            ^ rows[80 + col]
            ^ rows[96 + col];
    }

    // Serialize column-wise.  Step 16 through the 128-bit matrix; when
    // the index overflows 127, wrap by subtracting 127.  Equivalent to
    // reading column 0 (rows 0..7), then column 1, etc.
    let mut raw = [0u8; RAW_BITS];
    let mut idx = 0usize;
    for slot in raw.iter_mut() {
        *slot = mat[idx];
        idx += ROW_BITS;
        if idx > 127 {
            idx -= 127;
        }
    }
    raw
}

/// Encode the 72-bit LC into four 32-bit embedded LC fragments
/// (as 4-byte arrays, MSB-first).  fragments[0] is burst B, [1] is C,
/// [2] is D, [3] is E.  Burst F has no fragment -- use LCSS_NULL and
/// zeroed embedded bytes.
pub(crate) fn build_fragments(lc_bits: &[u8; DATA_BITS]) -> [[u8; FRAGMENT_BYTES]; 4] {
    let raw = encode_raw(lc_bits);
    let mut out = [[0u8; FRAGMENT_BYTES]; 4];
    for (n, fragment) in out.iter_mut().enumerate() {
        let bits = &raw[n * FRAGMENT_BITS..(n + 1) * FRAGMENT_BITS];
        for (byte_idx, byte) in fragment.iter_mut().enumerate() {
            let mut v: u8 = 0;
            for bit in 0..8 {
                v |= (bits[byte_idx * 8 + bit] & 1) << (7 - bit);
            }
            *byte = v;
        }
    }
    out
}

/// LCSS to use for embedded LC fragment `n` (0..4).  Fragment 0 is
/// "first" (LCSS=1), 1 and 2 are "continuation" (3), 3 is "last" (2).
pub(crate) fn lcss_for_fragment(n: usize) -> u8 {
    match n {
        0 => LCSS_FIRST,
        1 | 2 => LCSS_CONTINUATION,
        3 => LCSS_LAST,
        _ => LCSS_NULL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc5_zeros() {
        assert_eq!(crc5_embedded(&[0u8; 9]), 0);
    }

    #[test]
    fn crc5_tg91_from_3151238() {
        // Same LC payload bytes 0..9 as the BM capture compare test:
        // group call to TG 91 from src 3151238.  Sum = 0+0+0+0+0+91
        // +48+21+134 = 294.  294 % 31 = 15.
        let data: [u8; 9] = [0x00, 0x00, 0x00, 0x00, 0x00, 0x5B, 0x30, 0x15, 0x86];
        assert_eq!(crc5_embedded(&data), 15);
    }

    #[test]
    fn crc5_mod31_wraps() {
        // A single byte of 0x1F (= 31) wraps to 0.
        let data: [u8; 9] = [0x1F, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(crc5_embedded(&data), 0);
        // 32 -> 1.
        let data: [u8; 9] = [0x20, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(crc5_embedded(&data), 1);
    }

    #[test]
    fn encode_raw_all_zero_input() {
        // All-zero LC -> zero CRC -> zero data + parity.
        let lc = [0u8; DATA_BITS];
        let raw = encode_raw(&lc);
        assert!(raw.iter().all(|&b| b == 0));
    }

    #[test]
    fn encode_raw_parity_holds() {
        // For any non-trivial LC, each of rows 0..6 must satisfy the
        // Hamming(16,11) constraint, and row 7 must equal the XOR of
        // rows 0..6 column-wise.  We check this on the post-column-
        // serialize matrix after undoing the serialization.
        let mut lc = [0u8; DATA_BITS];
        for (i, b) in lc.iter_mut().enumerate() {
            *b = ((i * 7 + 3) & 1) as u8;
        }
        let raw = encode_raw(&lc);

        // Undo column serialization to get the matrix back.
        let mut mat = [0u8; RAW_BITS];
        let mut idx = 0usize;
        for &bit in raw.iter() {
            mat[idx] = bit;
            idx += ROW_BITS;
            if idx > 127 {
                idx -= 127;
            }
        }

        // Row parity (each row's cols 11..16 must match hamming_16_11).
        for row_start in (0..112).step_by(ROW_BITS) {
            let mut d = [0u8; 11];
            d.copy_from_slice(&mat[row_start..row_start + 11]);
            let expected = hamming_16_11_parity(&d);
            let got = &mat[row_start + 11..row_start + ROW_BITS];
            assert_eq!(got, &expected, "row at {row_start}");
        }

        // Column parity (row 7 = XOR of rows 0..6).
        for col in 0..ROW_BITS {
            let xor = mat[col]
                ^ mat[16 + col]
                ^ mat[32 + col]
                ^ mat[48 + col]
                ^ mat[64 + col]
                ^ mat[80 + col]
                ^ mat[96 + col];
            assert_eq!(mat[112 + col], xor, "col {col}");
        }
    }

    #[test]
    fn fragments_split_evenly() {
        let lc = [0u8; DATA_BITS];
        let f = build_fragments(&lc);
        assert_eq!(f.len(), 4);
        for frag in &f {
            assert_eq!(frag.len(), 4);
        }
    }

    #[test]
    fn fragments_all_zero_for_zero_lc() {
        let lc = [0u8; DATA_BITS];
        let f = build_fragments(&lc);
        for frag in &f {
            assert_eq!(*frag, [0u8; 4]);
        }
    }

    /// Build a 72-bit bit-array from 9 bytes MSB-first.  Test helper
    /// mirroring what voice.rs does with build_voice_lc output slicing.
    fn lc_bits_from_bytes(lc_bytes: [u8; 9]) -> [u8; DATA_BITS] {
        let mut bits = [0u8; DATA_BITS];
        for (i, byte) in lc_bytes.iter().enumerate() {
            for bit in 0..8 {
                bits[i * 8 + bit] = (byte >> (7 - bit)) & 1;
            }
        }
        bits
    }

    #[test]
    fn fragments_match_dmrpy_reference() {
        // Golden vector from thomastoye/dmr-from-scratch's
        // dmrpy/pdu/full_lc_test.py (ETSI TS 102 361-1 B.2 worked
        // example).  The 9-byte LC body and four embedded-LC fragments
        // on bursts B..E are independently computed in Python and
        // must agree with our encoder bit-for-bit.
        let lc_bytes = [0x00, 0x10, 0x20, 0x00, 0x0C, 0x30, 0x2F, 0x9B, 0xE5];
        let expected = [
            [0x4E, 0x0F, 0x06, 0x06], // B (LCSS=1)
            [0x17, 0x11, 0x00, 0x47], // C (LCSS=3)
            [0x0C, 0x03, 0x18, 0x1B], // D (LCSS=3)
            [0x17, 0x5A, 0x0F, 0x4E], // E (LCSS=2)
        ];
        let lc_bits = lc_bits_from_bytes(lc_bytes);
        assert_eq!(build_fragments(&lc_bits), expected);
    }

    #[test]
    fn crc5_matches_dmrpy_reference() {
        // Same LC as fragments_match_dmrpy_reference; dmrpy's
        // full_lc_test asserts cs_5bit == 0xC for this payload.
        let lc_bytes = [0x00, 0x10, 0x20, 0x00, 0x0C, 0x30, 0x2F, 0x9B, 0xE5];
        assert_eq!(crc5_embedded(&lc_bytes), 0xC);
    }

    #[test]
    fn lcss_sequence() {
        assert_eq!(lcss_for_fragment(0), LCSS_FIRST);
        assert_eq!(lcss_for_fragment(1), LCSS_CONTINUATION);
        assert_eq!(lcss_for_fragment(2), LCSS_CONTINUATION);
        assert_eq!(lcss_for_fragment(3), LCSS_LAST);
        assert_eq!(lcss_for_fragment(4), LCSS_NULL);
    }

    /// Embedded LC decoder error.
    #[derive(Debug, PartialEq, Eq)]
    enum DecodeError {
        Hamming,
        Crc { received: u8, computed: u8 },
    }

    impl From<HammingError> for DecodeError {
        fn from(_: HammingError) -> Self {
            DecodeError::Hamming
        }
    }

    /// Decode a 128-bit raw embedded LC block back into 9 LC bytes.
    /// Inverse of `encode_raw`: undoes column serialization, runs
    /// Hamming(16,11,4) correction on rows 0..6, extracts the LC bits
    /// + CRC-5, and verifies the CRC.
    ///
    /// cfg(test) only -- there's no runtime consumer.  An RF reception
    /// path that decodes embedded LC across bursts B-E would promote
    /// this to pub(crate).
    fn decode_raw(raw: &[u8; RAW_BITS]) -> Result<[u8; 9], DecodeError> {
        // Undo column serialization.
        let mut mat = [0u8; RAW_BITS];
        let mut idx = 0usize;
        for &bit in raw.iter() {
            mat[idx] = bit;
            idx += ROW_BITS;
            if idx > 127 {
                idx -= 127;
            }
        }

        // Hamming correction on rows 0..6 (row 7 is column parity, no
        // Hamming).  A double-bit error in any row surfaces as
        // DecodeError::Hamming.
        for row_start in (0..112).step_by(ROW_BITS) {
            let mut row: [u8; 16] = mat[row_start..row_start + ROW_BITS]
                .try_into()
                .expect("slice of ROW_BITS = 16");
            hamming_16_11_correct(&mut row)?;
            mat[row_start..row_start + ROW_BITS].copy_from_slice(&row);
        }

        // Extract LC bits.  Layout mirrors encode_raw.
        let mut lc_bits = [0u8; DATA_BITS];
        lc_bits[0..11].copy_from_slice(&mat[0..11]);
        lc_bits[11..22].copy_from_slice(&mat[16..27]);
        let mut b = 22;
        for row_start in [32, 48, 64, 80, 96] {
            lc_bits[b..b + 10].copy_from_slice(&mat[row_start..row_start + 10]);
            b += 10;
        }

        // Pack 72 bits into 9 LC bytes.
        let mut lc_bytes = [0u8; 9];
        for (i, byte) in lc_bytes.iter_mut().enumerate() {
            let mut v: u8 = 0;
            for bit in 0..8 {
                v |= (lc_bits[i * 8 + bit] & 1) << (7 - bit);
            }
            *byte = v;
        }

        // Extract CRC-5 from col 10 of rows 2..6 (positions 42..106).
        let received_crc =
            (mat[42] << 4) | (mat[58] << 3) | (mat[74] << 2) | (mat[90] << 1) | mat[106];
        let computed_crc = crc5_embedded(&lc_bytes);
        if received_crc != computed_crc {
            return Err(DecodeError::Crc {
                received: received_crc,
                computed: computed_crc,
            });
        }
        Ok(lc_bytes)
    }

    #[test]
    fn decode_raw_clean_round_trip() {
        // dmrpy ETSI worked-example LC: encode then decode must
        // recover the exact 9 bytes (including a non-zero CRC).
        let lc_bytes = [0x00, 0x10, 0x20, 0x00, 0x0C, 0x30, 0x2F, 0x9B, 0xE5];
        let lc_bits = lc_bits_from_bytes(lc_bytes);
        let raw = encode_raw(&lc_bits);
        assert_eq!(decode_raw(&raw), Ok(lc_bytes));
    }

    #[test]
    fn decode_raw_recovers_single_bit_errors() {
        // Encode the dmrpy worked-example LC, flip every one of the
        // 128 raw bits in turn, decode with Hamming correction.
        // Bits in rows 0..6 (positions 0..112 *in the matrix*) are
        // recovered by the row Hamming pass; bits in row 7
        // (positions 112..128) are the column parity row -- they
        // don't carry LC content, so a flip there leaves the LC
        // intact and decode succeeds without correction.
        //
        // Note: the FLIP is on the column-serialized `raw` array;
        // the matrix index a flip lands on depends on the
        // serialization permutation.  Either way, every single bit
        // flip in `raw` leaves the LC recoverable.
        let lc_bytes = [0x00, 0x10, 0x20, 0x00, 0x0C, 0x30, 0x2F, 0x9B, 0xE5];
        let lc_bits = lc_bits_from_bytes(lc_bytes);
        let raw = encode_raw(&lc_bits);
        for pos in 0..RAW_BITS {
            let mut corrupted = raw;
            corrupted[pos] ^= 1;
            assert_eq!(
                decode_raw(&corrupted),
                Ok(lc_bytes),
                "did not recover from raw[{pos}] flip"
            );
        }
    }

    #[test]
    fn decode_raw_detects_double_errors_in_a_row() {
        // Flip two bits in row 0 of the matrix (positions 0 and 1
        // *of mat*, which after column serialization land at raw[0]
        // and raw[8]).  Hamming(16,11,4) detects this as
        // uncorrectable.
        let lc_bytes = [0x00, 0x10, 0x20, 0x00, 0x0C, 0x30, 0x2F, 0x9B, 0xE5];
        let lc_bits = lc_bits_from_bytes(lc_bytes);
        let mut raw = encode_raw(&lc_bits);
        // Walk the column-serialization step (16 mod 127) to find
        // raw indices that map to mat[0] and mat[1]:
        // raw[a] = mat[a*16 mod 127] (with idx wrap rule).
        // mat[0] is raw[0]; mat[1] is raw[8] (since 8*16 = 128 -> 1).
        raw[0] ^= 1;
        raw[8] ^= 1;
        assert_eq!(decode_raw(&raw), Err(DecodeError::Hamming));
    }

    #[test]
    fn embedded_lc_reconstructs_dmrpy_voice_superframe() {
        // dmrpy VOICE_SUPERFRAME (bursts A-F) carries a 4-fragment
        // embedded LC across bursts B-E that reconstructs to a Full
        // LC body matching the LC dmrpy's test_full_lc_create_from_binary
        // asserts (fid=0x10, raw=0x1020000C302F9BE5).
        //
        // (dmrpy's test_create_from_superframe asserts a different LC
        // -- raw=0x20400018605F37CA -- but the file is marked "TODO
        // experimentation, clean up" at the top, those assertions are
        // inconsistent with the actual VOICE_SUPERFRAME embedded
        // fragments, and our reconstructed LC matches the working
        // half of dmrpy's tests.)
        //
        // This is non-BM Python ground truth covering the burst-to-LC
        // end-to-end pipeline (sync-section extraction + EMB header
        // strip + 4-fragment concatenation + decode_raw), beyond the
        // fragment-level coverage in fragments_match_dmrpy_reference.
        use super::super::frame::extract_sync_section;
        const BURSTS: &[&str] = &[
            // burst B
            "B9E881526173002A6BB9E881526134E0F060691173002A6BB9E881526173002A6A",
            // burst C
            "B9E881526173002A6BB9E881526171711004774173002A6BB9E881526173002A6A",
            // burst D
            "B9E881526173002A6B954BE6500170C03181B74310B00777A6C6CB53732789483A",
            // burst E
            "865AE7617555B50601B758E665115175A0F4E07124815001FFF5A337706128A7CA",
        ];

        let mut raw = [0u8; RAW_BITS];
        for (i, hex_burst) in BURSTS.iter().enumerate() {
            let burst_bytes = hex::decode(hex_burst).unwrap();
            let burst: [u8; super::super::dmrd::DMR_DATA_SIZE] = burst_bytes.try_into().unwrap();
            let sync_section = extract_sync_section(&burst);
            // sync_section is [emb_hi, lc[0..4], emb_lo].  The LC
            // fragment is bytes 1..5; expand each byte MSB-first.
            for byte_idx in 0..FRAGMENT_BYTES {
                for bit in 0..8 {
                    raw[i * FRAGMENT_BITS + byte_idx * 8 + bit] =
                        (sync_section[1 + byte_idx] >> (7 - bit)) & 1;
                }
            }
        }

        let lc_bytes = decode_raw(&raw).expect("dmrpy VOICE_SUPERFRAME should decode cleanly");
        assert_eq!(
            lc_bytes,
            [0x00, 0x10, 0x20, 0x00, 0x0C, 0x30, 0x2F, 0x9B, 0xE5],
            "VOICE_SUPERFRAME LC mismatch vs dmrpy"
        );
    }

    #[test]
    fn decode_raw_all_double_flips_never_miscorrect() {
        // Exhaustively flip every pair (i, j) with i < j across all
        // 128 raw bits.  For each pair, decode must either recover
        // the original LC or return an error -- it must never return
        // a DIFFERENT LC silently (silent miscorrection).  Catches
        // the full C(128, 2) = 8128 cases; docs previously claimed
        // this coverage but only one pair was actually tested.
        let lc_bytes = [0x00, 0x10, 0x20, 0x00, 0x0C, 0x30, 0x2F, 0x9B, 0xE5];
        let lc_bits = lc_bits_from_bytes(lc_bytes);
        let raw = encode_raw(&lc_bits);
        for i in 0..RAW_BITS {
            for j in (i + 1)..RAW_BITS {
                let mut corrupted = raw;
                corrupted[i] ^= 1;
                corrupted[j] ^= 1;
                if let Ok(got) = decode_raw(&corrupted) {
                    assert_eq!(
                        got, lc_bytes,
                        "miscorrection at flips ({i}, {j}): got wrong LC"
                    );
                }
            }
        }
    }
}
