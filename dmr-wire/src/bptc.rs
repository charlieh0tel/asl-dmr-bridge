//! BPTC(196,96) encoder for DMR voice LC header/terminator bursts.
//!
//! Encodes a 96-bit LC payload into a 196-bit block product turbo
//! code matrix (9 data rows x Hamming(15,11) + 13 columns x
//! Hamming(13,9) + 1 overall parity).  The output is interleaved
//! and packed into a 33-byte data sync burst.
//!
//! Data bit positions cross-checked against DMRGateway BPTC19696.cpp.
//! Interleave formula: out[i*181 % 196] = deinter[i].

use super::dmrd::DMR_DATA_SIZE;
use super::fec::golay_20_8_parity;
use super::fec::hamming_13_9_correct;
use super::fec::hamming_13_9_parity;
use super::fec::hamming_15_11_correct;
use super::fec::hamming_15_11_parity;
use super::rs::LC_HEADER_MASK;
use super::rs::LC_TERMINATOR_MASK;
use super::rs::rs_12_9_correct;
use super::rs::rs_12_9_fec;
use super::rs::rs_12_9_syndromes;

const BPTC_BITS: usize = 196;
const DATA_BITS: usize = 96;
const MATRIX_COLS: usize = 15;

/// Linear positions of the 96 data bits in the deinterleaved matrix.
/// Row = pos/15, Col = pos%15.
/// Cross-checked against DMRGateway BPTC19696.cpp encodeExtractData.
const DATA_POSITIONS: [usize; DATA_BITS] = [
    // First row fragment: 8 bits at positions 4-11
    4, 5, 6, 7, 8, 9, 10, 11, // Rows 1-8: 11 bits each at positions row*15+1 .. row*15+11
    16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 46, 47,
    48, 49, 50, 51, 52, 53, 54, 55, 56, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 76, 77, 78, 79,
    80, 81, 82, 83, 84, 85, 86, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 106, 107, 108, 109,
    110, 111, 112, 113, 114, 115, 116, 121, 122, 123, 124, 125, 126, 127, 128, 129, 130, 131,
];

/// Encode 96 data bits into a 196-bit BPTC matrix, interleave, and
/// return the interleaved bits.
///
/// Follows DMRGateway BPTC19696.cpp exactly:
/// 1. Place 96 data bits at DATA_POSITIONS
/// 2. Row Hamming(15,11) on rows 0-8, cols 1-14 (parity at cols 12-14)
/// 3. Column Hamming(13,9) on cols 0-14, rows from offset c+1
///    (parity fills remaining positions including row 9-12)
/// 4. Interleave: out[i*181 % 196] = matrix[i]
fn bptc_encode(data: &[u8; DATA_BITS]) -> [u8; BPTC_BITS] {
    let mut matrix = [0u8; BPTC_BITS];

    // Place data bits.
    for (i, &pos) in DATA_POSITIONS.iter().enumerate() {
        matrix[pos] = data[i];
    }

    // Row Hamming(15,11) on rows 0-8.
    // Each row's 14 data+parity bits start at col 1 (offset +1).
    // Cols 1-11 = data (11 bits), cols 12-14 = parity (4 bits).
    // DMRGateway uses encode15113_2 for all 9 rows.
    for r in 0..9 {
        let base = r * MATRIX_COLS + 1;
        let mut d = [0u8; 11];
        for (i, slot) in d.iter_mut().enumerate() {
            *slot = matrix[base + i];
        }
        let parity = hamming_15_11_parity(&d);
        for (i, &p) in parity.iter().enumerate() {
            matrix[base + 11 + i] = p;
        }
    }

    // Column Hamming(13,9) on 15 columns.
    // Per DMRGateway: column c reads positions c+1, c+16, c+31, ...
    // (13 values, stepping by 15).  encode1393 treats the first 9
    // as data and computes 4 parity for positions 9-12.
    for c in 0..MATRIX_COLS {
        let mut col = [0u8; 13];
        let mut pos = c + 1;
        for slot in &mut col {
            *slot = matrix[pos];
            pos += MATRIX_COLS;
        }
        let p = hamming_13_9_parity(
            &col[..9]
                .try_into()
                .expect("col is [u8; 13], col[..9] is len 9"),
        );
        col[9] = p[0];
        col[10] = p[1];
        col[11] = p[2];
        col[12] = p[3];
        // Write back.
        pos = c + 1;
        for &bit in &col {
            matrix[pos] = bit;
            pos += MATRIX_COLS;
        }
    }

    // Interleave: out[i*181 % 196] = matrix[i].
    let mut interleaved = [0u8; BPTC_BITS];
    for (i, &bit) in matrix.iter().enumerate() {
        interleaved[(i * 181) % BPTC_BITS] = bit;
    }

    interleaved
}

/// Encode a slot type field: 4-bit data_type + 4-bit color_code ->
/// Golay(20,8) -> 20 bits.  Returns the 20 bits as an array.
#[expect(
    clippy::needless_range_loop,
    reason = "index used for both position and shift"
)]
fn encode_slot_type(data_type: u8, color_code: u8) -> [u8; 20] {
    let info = ((color_code & 0x0F) << 4) | (data_type & 0x0F);
    let parity = golay_20_8_parity(info);

    let mut bits = [0u8; 20];
    // Info bits (8): MSB first.
    for i in 0..8 {
        bits[i] = (info >> (7 - i)) & 1;
    }
    // Parity bits (12): stored in DMRGateway byte order.
    // Low byte (8 bits) then high byte top nibble (4 bits).
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

/// Build a complete 33-byte data sync burst (voice LC header or
/// terminator).
///
/// `lc_bits`: 96-bit LC payload (individual bits, 0 or 1).
/// `data_type`: 1 for voice header, 2 for voice terminator.
/// `color_code`: the configured color code.
/// `sync`: 6-byte SYNC pattern (typically BS_DATA_SYNC).
pub(crate) fn build_data_burst(
    lc_bits: &[u8; DATA_BITS],
    data_type: u8,
    color_code: u8,
    sync: &[u8; 6],
) -> [u8; DMR_DATA_SIZE] {
    let interleaved = bptc_encode(lc_bits);
    let slot_type = encode_slot_type(data_type, color_code);

    // Burst layout (264 bits = 33 bytes):
    //   Info1[98] + SlotType1[10] + SYNC[48] + SlotType2[10] + Info2[98]
    let mut burst_bits = [0u8; 264];

    // Info1: interleaved bits 0..98
    burst_bits[..98].copy_from_slice(&interleaved[..98]);

    // SlotType first half: bits 0..10
    burst_bits[98..108].copy_from_slice(&slot_type[..10]);

    // SYNC: 48 bits from 6 bytes
    for (i, &byte) in sync.iter().enumerate() {
        for bit in 0..8 {
            burst_bits[108 + i * 8 + bit] = (byte >> (7 - bit)) & 1;
        }
    }

    // SlotType second half: bits 10..20
    burst_bits[156..166].copy_from_slice(&slot_type[10..]);

    // Info2: interleaved bits 98..196
    burst_bits[166..264].copy_from_slice(&interleaved[98..]);

    // Pack 264 bits into 33 bytes.
    let mut data = [0u8; DMR_DATA_SIZE];
    for (i, byte) in data.iter_mut().enumerate() {
        for bit in 0..8 {
            if burst_bits[i * 8 + bit] != 0 {
                *byte |= 1 << (7 - bit);
            }
        }
    }
    data
}

/// Build the 96-bit LC payload for a voice call.
///
/// Layout: PF(1) + reserved(1) + FLCO(6) + FID(8) + service_opts(8)
///         + dst_id(24) + src_id(24) + RS(12,9) parity(24)
///
/// `data_type`: 1 = voice header, 2 = voice terminator (selects RS mask).
/// Returns individual bits.
pub(crate) fn build_voice_lc(
    group_call: bool,
    dst_id: u32,
    src_id: u32,
    data_type: u8,
) -> [u8; DATA_BITS] {
    let mut payload = [0u8; 12]; // 96 bits = 12 bytes

    // PF = 0, reserved = 0
    // FLCO: 0b000000 = group voice, 0b000011 = unit-to-unit voice
    if !group_call {
        payload[0] = 0x03;
    }
    // FID = 0x00 (standard)
    // Service options = 0x00

    // dst_id (bytes 3-5) and src_id (bytes 6-8) are 24-bit on-air
    // subscriber fields; panic on overflow (see types::DmrId doc --
    // silent truncation would impersonate an unrelated user).
    payload[3..6].copy_from_slice(&super::id_to_24_be(dst_id));
    payload[6..9].copy_from_slice(&super::id_to_24_be(src_id));

    // RS(12,9) FEC over bytes 0-8 with per-data-type mask.
    let mask = if data_type == 2 {
        &LC_TERMINATOR_MASK
    } else {
        &LC_HEADER_MASK
    };
    let data9: [u8; 9] = payload[..9]
        .try_into()
        .expect("payload is [u8; 12], payload[..9] is len 9");
    let fec = rs_12_9_fec(&data9, mask);
    payload[9] = fec[0];
    payload[10] = fec[1];
    payload[11] = fec[2];

    // Expand to individual bits.
    let mut bits = [0u8; DATA_BITS];
    for (i, &byte) in payload.iter().enumerate() {
        for bit in 0..8 {
            bits[i * 8 + bit] = (byte >> (7 - bit)) & 1;
        }
    }
    bits
}

// --- RX-side validators (log-only, never gating) ---
//
// We don't normally need to decode inbound voice headers /
// terminators since the DMRD flag byte already carries frame_type,
// dtype_vseq, slot, src_id, and dst_id.  But running the BPTC FEC
// + RS syndrome calc gives us cheap visibility into corruption
// upstream of us (BM forwarding bad bursts, parser bugs, etc.).
// All checks are log-only -- we never drop a frame on validation
// failure.

/// Result of decoding a captured voice LC header / terminator burst
/// for log-only validation.  All fields are diagnostic, not
/// authoritative -- the DMRD header remains the source of truth.
pub(crate) struct DecodedVoiceLc {
    /// LC body src_id (bytes 6..8 packed big-endian, 24-bit).
    /// If RS correction succeeded, this is the corrected value.
    pub(crate) src_id: u32,
    /// LC body dst_id (bytes 3..5 packed big-endian, 24-bit).
    pub(crate) dst_id: u32,
    /// RS(12,9) syndromes BEFORE correction (all-zero means the
    /// original codeword was already valid; non-zero means either
    /// correction was applied or the error was uncorrectable).
    pub(crate) rs_syndromes: [u8; 3],
    /// Whether RS single-byte correction succeeded.  true if the
    /// codeword was already valid OR was corrected; false if >= 2
    /// byte errors were detected and the LC body may be wrong.
    pub(crate) rs_corrected: bool,
    /// Bits flipped by BPTC Hamming correction (row + column passes).
    /// 0 means the BPTC matrix was already valid; small counts are
    /// expected on noisy paths; high counts hint at miscorrection
    /// since row-pass Hamming(15,11,3) cannot detect multi-bit errors.
    pub(crate) bptc_corrected_bits: u8,
}

/// BPTC decoder with single-bit error correction across rows
/// (Hamming(15,11,3)) and then columns (Hamming(13,9,3)).  A
/// single bit error anywhere in the 196-bit matrix is recovered
/// by one of the two passes.
///
/// Returns the corrected 96 data bits and the number of bits
/// flipped during correction (0..=25, summed across the 9 row +
/// 16 column passes).  A column-pass Hamming(13,9,3) uncorrectable
/// syndrome yields None.  Row-pass errors always "succeed"
/// (distance 3, no detection) so a multi-bit error may silently
/// miscorrect; a high corrected-bit count is the only diagnostic
/// signal upstream gets.
pub(crate) fn bptc_decode_correct(interleaved: &[u8; BPTC_BITS]) -> Option<([u8; DATA_BITS], u8)> {
    let mut matrix = [0u8; BPTC_BITS];
    for (i, slot) in matrix.iter_mut().enumerate() {
        *slot = interleaved[(i * 181) % BPTC_BITS];
    }
    let mut corrected_bits: u8 = 0;

    // Row pass: each of rows 0..9 carries 11 data + 4 Hamming
    // parity bits, offset by 1 from row_start (col 0 of row 0 is
    // R(3) at matrix[0]; data rows start at matrix[1]).
    for r in 0..9 {
        let base = r * MATRIX_COLS + 1;
        let mut row: [u8; 15] = matrix[base..base + 15]
            .try_into()
            .expect("slice of length 15");
        if hamming_15_11_correct(&mut row) {
            corrected_bits += 1;
        }
        matrix[base..base + 15].copy_from_slice(&row);
    }

    // Column pass: each of columns 0..15 has 13 values stepping
    // by 15 positions starting at c+1.  First 9 are data rows,
    // last 4 are column parity rows.
    for c in 0..MATRIX_COLS {
        let mut col = [0u8; 13];
        let mut pos = c + 1;
        for slot in &mut col {
            *slot = matrix[pos];
            pos += MATRIX_COLS;
        }
        if hamming_13_9_correct(&mut col).ok()? {
            corrected_bits += 1;
        }
        pos = c + 1;
        for &bit in &col {
            matrix[pos] = bit;
            pos += MATRIX_COLS;
        }
    }

    let mut data = [0u8; DATA_BITS];
    for (i, &pos) in DATA_POSITIONS.iter().enumerate() {
        data[i] = matrix[pos];
    }
    Some((data, corrected_bits))
}

/// Extract the 196 BPTC info bits from a 33-byte data burst,
/// inverse of the Info1/SlotType1/SYNC/SlotType2/Info2 packing in
/// build_data_burst.
pub(crate) fn extract_burst_interleaved(burst: &[u8; DMR_DATA_SIZE]) -> [u8; BPTC_BITS] {
    let mut burst_bits = [0u8; 264];
    for (i, &byte) in burst.iter().enumerate() {
        for bit in 0..8 {
            burst_bits[i * 8 + bit] = (byte >> (7 - bit)) & 1;
        }
    }
    let mut interleaved = [0u8; BPTC_BITS];
    interleaved[..98].copy_from_slice(&burst_bits[..98]);
    interleaved[98..].copy_from_slice(&burst_bits[166..264]);
    interleaved
}

/// Decode a voice LC header (`data_type=1`) or terminator
/// (`data_type=2`) burst into its src_id/dst_id and the RS(12,9)
/// syndromes of its (unmasked) Full LC.  Returns None if the BPTC
/// matrix is uncorrectable -- e.g. a column had >= 2 bit errors.
///
/// All-zero syndromes mean the LC is a valid RS codeword.  The
/// caller compares src_id/dst_id against the DMRD header's values
/// for additional cross-check; mismatches are interesting log
/// signals but not authoritative.
pub(crate) fn decode_voice_lc_burst(
    burst: &[u8; DMR_DATA_SIZE],
    data_type: u8,
) -> Option<DecodedVoiceLc> {
    let interleaved = extract_burst_interleaved(burst);
    let (bits, bptc_corrected_bits) = bptc_decode_correct(&interleaved)?;

    // Pack 96 bits MSB-first into 12 LC bytes.
    let mut lc = [0u8; 12];
    for (i, byte) in lc.iter_mut().enumerate() {
        let mut v: u8 = 0;
        for bit in 0..8 {
            v |= (bits[i * 8 + bit] & 1) << (7 - bit);
        }
        *byte = v;
    }

    // Unmask the parity bytes per data_type, then build the
    // codeword in ETSI's (m_8..m_0, p_2, p_1, p_0) order so the
    // syndrome calc lines up.
    let mask = if data_type == 2 {
        &LC_TERMINATOR_MASK
    } else {
        &LC_HEADER_MASK
    };
    let mut codeword = [0u8; 12];
    codeword[..9].copy_from_slice(&lc[..9]);
    codeword[9] = lc[9] ^ mask[0]; // p_2
    codeword[10] = lc[10] ^ mask[1]; // p_1
    codeword[11] = lc[11] ^ mask[2]; // p_0

    // Snapshot the pre-correction syndromes for logging, then
    // attempt single-byte correction on the unmasked codeword.
    let rs_syndromes = rs_12_9_syndromes(&codeword);
    let rs_corrected = rs_12_9_correct(&mut codeword);

    // Extract src/dst from the (possibly corrected) LC body.
    let dst_id = u32::from_be_bytes([0, codeword[3], codeword[4], codeword[5]]);
    let src_id = u32::from_be_bytes([0, codeword[6], codeword[7], codeword[8]]);

    Some(DecodedVoiceLc {
        src_id,
        dst_id,
        rs_syndromes,
        rs_corrected,
        bptc_corrected_bits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_positions_count() {
        assert_eq!(DATA_POSITIONS.len(), DATA_BITS);
    }

    #[test]
    fn data_positions_in_range() {
        for &pos in &DATA_POSITIONS {
            assert!(pos < BPTC_BITS, "position {pos} out of range");
        }
    }

    #[test]
    fn data_positions_no_duplicates() {
        let mut seen = [false; BPTC_BITS];
        for &pos in &DATA_POSITIONS {
            assert!(!seen[pos], "duplicate position {pos}");
            seen[pos] = true;
        }
    }

    /// Minimal BPTC decoder used only for the encode/decode
    /// round-trip test below: inverse interleave + data-bit
    /// extraction with no Hamming correction.  Production validators
    /// use bptc_decode_correct (defined at module scope above).
    fn bptc_decode(interleaved: &[u8; BPTC_BITS]) -> [u8; DATA_BITS] {
        let mut matrix = [0u8; BPTC_BITS];
        for (i, slot) in matrix.iter_mut().enumerate() {
            *slot = interleaved[(i * 181) % BPTC_BITS];
        }
        let mut data = [0u8; DATA_BITS];
        for (i, &pos) in DATA_POSITIONS.iter().enumerate() {
            data[i] = matrix[pos];
        }
        data
    }

    #[test]
    fn bptc_decode_correct_recovers_single_bit_errors() {
        // Encode non-trivial data, then for each of the 196 bit
        // positions in the interleaved codeword: flip that bit,
        // run the corrected decoder, and assert the original 96
        // data bits come back unchanged.  Validates that a single
        // bit error anywhere in the matrix is recoverable via the
        // row+column Hamming correction passes.
        let mut data = [0u8; DATA_BITS];
        for (i, b) in data.iter_mut().enumerate() {
            *b = ((i * 5 + 1) & 1) as u8;
        }
        let encoded = bptc_encode(&data);
        for pos in 0..BPTC_BITS {
            let mut corrupted = encoded;
            corrupted[pos] ^= 1;
            let (decoded, corrected) = bptc_decode_correct(&corrupted)
                .unwrap_or_else(|| panic!("uncorrectable at bit {pos}"));
            assert_eq!(decoded, data, "did not recover from flip at bit {pos}");
            // Most flips trigger one row + one column correction (=2);
            // the reserved R(3) bit (matrix[0], interleaved index 0) is
            // outside both Hamming passes and stays untouched at 0.
            let expected_min = if pos == 0 { 0 } else { 1 };
            assert!(
                corrected >= expected_min,
                "corrected count {corrected} too low for bit {pos}"
            );
        }
    }

    #[test]
    fn bptc_decode_correct_passes_clean_codeword() {
        // Clean encode -> corrected decode must return the input
        // unchanged (syndromes are all zero; no bit is flipped, so
        // corrected count must be 0).
        let mut data = [0u8; DATA_BITS];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i & 1) as u8;
        }
        let encoded = bptc_encode(&data);
        assert_eq!(bptc_decode_correct(&encoded), Some((data, 0)));
    }

    #[test]
    fn bptc_encode_decode_roundtrip() {
        // Self-consistency: encode(data) -> decode must return data
        // for arbitrary 96-bit inputs.  Proves the two transformations
        // are strict inverses on the data bits (FEC bits are discarded
        // by the decoder).
        let mut data = [0u8; DATA_BITS];
        for (i, b) in data.iter_mut().enumerate() {
            *b = ((i * 7 + 3) & 1) as u8;
        }
        let encoded = bptc_encode(&data);
        assert_eq!(bptc_decode(&encoded), data);
    }

    /// Pack 96 extracted data bits to 12 LC bytes, MSB-first.
    fn pack_lc_bytes(data_bits: &[u8; DATA_BITS]) -> [u8; 12] {
        let mut bytes = [0u8; 12];
        for (i, byte) in bytes.iter_mut().enumerate() {
            for bit in 0..8 {
                *byte |= (data_bits[i * 8 + bit] & 1) << (7 - bit);
            }
        }
        bytes
    }

    /// Decode a 33-byte captured burst to 12 LC bytes via our BPTC
    /// decoder, for asserting ground-truth LC content.
    fn decode_burst_to_lc_bytes(burst_hex: &str) -> [u8; 12] {
        let bytes = hex::decode(burst_hex).unwrap();
        let burst: [u8; DMR_DATA_SIZE] = bytes.try_into().unwrap();
        let interleaved = extract_burst_interleaved(&burst);
        pack_lc_bytes(&bptc_decode(&interleaved))
    }

    /// Captured Brandmeister bursts for TG 91, 2026-04-16.  Each row
    /// is (src_id, header hex, terminator hex).  All are group calls
    /// (FLCO=0) at color_code=1 on BM default.
    const BM_CAPTURES: &[(u32, &str, &str)] = &[
        (
            3151238, // 2026-04-15 capture (original compare_against_bm_capture)
            "018b0aec1234093072c00500446dff57d75df5de327414f832a00ac008c12f82b3",
            "", // no terminator captured for this call
        ),
        (
            5201886,
            "030f0f321b981db07bf02860c46dff57d75df5de33880d001d6050204d015383ed",
            "03600fe61b281dc87b8028c0c4adff57d75df5d9669c0e381a005c6045014a83de",
        ),
        (
            3221731,
            "00580a70171c0e2066701360046dff57d75df5de32e01a5021a025403a81518387",
            "00370aa417ac0e58660013c004adff57d75df5d967f4196826c0290032814883b4",
        ),
        (
            3222582,
            "00d10a9c103404886ee00d80446dff57d75df5de33241c7024603a403e4118031f",
            "00be0a48108404f06e900d2044adff57d75df5d966301f4823003600364101032c",
        ),
        (
            2342361,
            "039f09fc16f817304fe01500446dff57d75df5de316808b03ab033400a01f203e4",
            "03f00928164817484f9015a044adff57d75df5d9647c0b883dd03f000201eb03d7",
        ),
        (
            3023805,
            "038d0f141b701aa053403ba0046dff57d75df5de313004702f203c00360128829c",
            "03e20fc01bc01ad853303b0004adff57d75df5d964240748284030403e013182af",
        ),
    ];

    #[test]
    fn bptc_encode_matches_bm_header_captures() {
        // For each captured header, our encoder -- given the known
        // (group, TG 91, src, color_code=1) -- must produce the exact
        // 33-byte burst seen on the wire.  Exercises a variety of
        // different 96-bit inputs through BPTC + RS(12,9) + slot-type
        // Golay(20,8) in one pass.
        let sync = super::super::sync::BS_DATA_SYNC;
        for &(src_id, header_hex, _) in BM_CAPTURES {
            let captured = hex::decode(header_hex).unwrap();
            let lc = build_voice_lc(true, 91, src_id, 1);
            let burst = build_data_burst(&lc, 1, 1, &sync);
            assert_eq!(burst[..], captured[..], "header mismatch for src={src_id}");
        }
    }

    #[test]
    fn bptc_encode_matches_bm_terminator_captures() {
        // Terminators use LC_TERMINATOR_MASK for RS(12,9) and
        // data_type=2 in the slot type -- different 96-bit input and
        // different slot-type encoding than headers, even for the
        // same call.
        let sync = super::super::sync::BS_DATA_SYNC;
        for &(src_id, _, term_hex) in BM_CAPTURES {
            if term_hex.is_empty() {
                continue;
            }
            let captured = hex::decode(term_hex).unwrap();
            let lc = build_voice_lc(true, 91, src_id, 2);
            let burst = build_data_burst(&lc, 2, 1, &sync);
            assert_eq!(
                burst[..],
                captured[..],
                "terminator mismatch for src={src_id}"
            );
        }
    }

    #[test]
    fn bptc_decode_bm_captures_yield_group_call_lc() {
        // Ground truth: each captured header decodes back to an LC
        // body matching (FLCO=group=0, FID=0, opts=0, dst=91, src=X).
        // This validates our decoder against many wire inputs beyond
        // the single compare_against_bm_capture test.
        for &(src_id, header_hex, _) in BM_CAPTURES {
            let bytes = decode_burst_to_lc_bytes(header_hex);
            let [s2, s1, s0] = src_id.to_be_bytes()[1..4].try_into().unwrap();
            assert_eq!(
                &bytes[..9],
                &[0x00, 0x00, 0x00, 0x00, 0x00, 0x5B, s2, s1, s0],
                "header LC body mismatch for src={src_id}"
            );
        }
    }

    #[test]
    fn bptc_roundtrips_okdmrlib_wire_bursts() {
        // Two 33-byte data+control bursts from OK-DMR/ok-dmrlib's
        // test_bptc_196_96.py test_decode_encode.  These aren't our
        // captures; they're real wire bursts independently vetted by
        // ok-dmrlib's BPTC encoder+decoder pair.
        //
        // Test: extract the 196-bit info from each burst, decode to
        // 96 data bits, re-encode, and verify the resulting 196 bits
        // match the original.  This is the "captured burst is a valid
        // BPTC codeword under our encoder" invariant -- holds only if
        // our encode and decode are strict inverses AND agree with the
        // wire format ok-dmrlib's tests are built around.
        const BURSTS: &[&str] = &[
            "53df0a83b7a8282c2509625014fdff57d75df5dcadde429028c87ae3341e24191c",
            "51cf0ded894c0dec1ff8fcf294fdff57d75df5dcae7a16d064197982bf5824914c",
        ];
        for hex_burst in BURSTS {
            let bytes = hex::decode(hex_burst).unwrap();
            let burst: [u8; DMR_DATA_SIZE] = bytes.try_into().unwrap();
            let original_interleaved = extract_burst_interleaved(&burst);
            let data_bits = bptc_decode(&original_interleaved);
            let reencoded = bptc_encode(&data_bits);
            assert_eq!(
                reencoded, original_interleaved,
                "round-trip failed for burst {hex_burst}"
            );
        }
    }

    #[test]
    fn bptc_decode_bm_capture_yields_known_lc_bytes() {
        // Independent wire-to-bytes check: take the real Brandmeister
        // voice LC header capture, run it through our decoder, pack
        // the 96 bits back to 12 bytes, and verify the LC content
        // matches what we know the call was (group to TG 91 from
        // src 3151238, plus RS parity from rs::tests::rs_known_lc).
        //
        // This validates the BPTC *decoder* against the same wire
        // data that compare_against_bm_capture validates the
        // *encoder* against -- both must agree with the capture.
        let captured =
            hex::decode("018b0aec1234093072c00500446dff57d75df5de327414f832a00ac008c12f82b3")
                .unwrap();
        let burst: [u8; DMR_DATA_SIZE] = captured.try_into().unwrap();
        let interleaved = extract_burst_interleaved(&burst);
        let data_bits = bptc_decode(&interleaved);

        let mut bytes = [0u8; 12];
        for (i, byte) in bytes.iter_mut().enumerate() {
            for bit in 0..8 {
                *byte |= (data_bits[i * 8 + bit] & 1) << (7 - bit);
            }
        }
        // LC body (PF=0, FLCO=group, FID=0, svc=0, dst=91, src=3151238).
        assert_eq!(
            &bytes[..9],
            &[0x00, 0x00, 0x00, 0x00, 0x00, 0x5B, 0x30, 0x15, 0x86]
        );
        // Masked RS(12,9) parity for the header -- matches the
        // expected value in rs::tests::rs_known_lc.
        assert_eq!(&bytes[9..12], &[0x28, 0x0A, 0x75]);
    }

    #[test]
    fn bptc_encode_matches_dmrpy_data_head() {
        // dmrpy worked example: voice LC HEADER for a call whose Full
        // LC body is fid=0x10, raw=0x1020000C302F9BE5 (the LC that
        // dmrpy's test_full_lc_create_from_binary asserts and that
        // VOICE_SUPERFRAME's embedded fragments reconstruct to -- see
        // embedded_lc::tests::embedded_lc_reconstructs_dmrpy_voice_superframe
        // for the burst-level cross-check).
        //
        // Source: thomastoye/dmr-from-scratch dmrpy/pdu/full_lc_test.py
        // (header data is the comment on VOICE_SUPERFRAME line 7.1.1).
        //
        // This is a non-BM Python ground truth for the BPTC(196,96)
        // encoder: our 11 BM captures already validate against BM's
        // own encoder; this validates against an independent reference.
        let captured =
            hex::decode("2B6004101F842DD00DF07D41046DFF57D75DF5DE30152E2070B20F803F88C695E2")
                .unwrap();
        let data_head: [u8; DMR_DATA_SIZE] = captured.try_into().unwrap();
        let interleaved = extract_burst_interleaved(&data_head);
        let bits = bptc_decode(&interleaved);

        let mut lc_bytes = [0u8; 12];
        for (i, byte) in lc_bytes.iter_mut().enumerate() {
            for bit in 0..8 {
                *byte |= (bits[i * 8 + bit] & 1) << (7 - bit);
            }
        }
        // Decoder agrees with dmrpy on the LC body.
        assert_eq!(
            &lc_bytes[..9],
            &[0x00, 0x10, 0x20, 0x00, 0x0C, 0x30, 0x2F, 0x9B, 0xE5],
            "data_head LC body mismatch vs dmrpy"
        );
        // Encoder agrees with dmrpy: re-encoding the same 96 data bits
        // reproduces the wire interleaved payload byte-exact.
        let reencoded = bptc_encode(&bits);
        assert_eq!(
            reencoded, interleaved,
            "bptc_encode did not match dmrpy data_head wire"
        );
    }

    #[test]
    fn bptc_encode_zeros() {
        let data = [0u8; DATA_BITS];
        let encoded = bptc_encode(&data);
        // All-zero data should produce all-zero output (parity of
        // zeros is zero).
        assert!(encoded.iter().all(|&b| b == 0));
    }

    #[test]
    fn bptc_interleave_formula_matches_spec() {
        // ETSI TS 102 361-1 Table B.2: sampled (Index, Interleave Index)
        // pairs from the normative interleaving table.  Our encoder
        // uses out[(i*181) % 196] = matrix[i], which must agree with
        // every row of Table B.2.
        const SAMPLES: &[(usize, usize)] = &[
            (0, 0),     // R(3) - first row
            (1, 181),   // R(2)
            (4, 136),   // I(95) - first data bit
            (11, 31),   // I(88) - end of first row data
            (15, 167),  // H_R1(0) - end of row 0
            (16, 152),  // I(87) - start of row 1
            (131, 191), // I(0) - last data bit
            (132, 176), // H_R9(3)
            (135, 131), // H_R9(0) - last info row
            (136, 116), // H_C1(3) - first column parity
            (195, 15),  // H_C15(0) - last entry
        ];
        for &(index, expected) in SAMPLES {
            assert_eq!((index * 181) % BPTC_BITS, expected, "Index {index}");
        }
    }

    #[test]
    fn bptc_data_positions_match_spec() {
        // ETSI TS 102 361-1 Table B.2 assigns matrix Index values to
        // the 96 info bits I(95)..I(0).  Our DATA_POSITIONS maps the
        // linear data index (0..96) to matrix position; linear 0 is
        // I(95), linear 95 is I(0).  Spot-check that the full 96-entry
        // table matches the spec at its row/column boundaries.
        const SPEC_INDEX_FOR_I: &[(usize, usize)] = &[
            (95, 4),  // I(95) -> row 0 col 3 = Index 4
            (94, 5),  // I(94) -> row 0 col 4 = Index 5
            (88, 11), // I(88) -> row 0 col 10 = Index 11 (last of row 0 data)
            (87, 16), // I(87) -> row 1 col 0 = Index 16
            (77, 26), // I(77) -> row 1 col 10
            (76, 31), // I(76) -> row 2 col 0 = Index 31 (spec B.2)
            (0, 131), // I(0)  -> row 8 col 10 = last info bit
        ];
        for &(lc_bit, expected_pos) in SPEC_INDEX_FOR_I {
            // linear data index = 95 - lc_bit (since DATA_POSITIONS[0] = I(95)).
            let linear = 95 - lc_bit;
            assert_eq!(
                DATA_POSITIONS[linear], expected_pos,
                "I({lc_bit}) at linear {linear}"
            );
        }
    }

    #[test]
    fn bptc_encode_nonzero() {
        let mut data = [0u8; DATA_BITS];
        data[0] = 1;
        let encoded = bptc_encode(&data);
        assert!(encoded.iter().any(|&b| b != 0));
    }

    #[test]
    fn build_data_burst_length() {
        let lc = [0u8; DATA_BITS];
        let sync = [0u8; 6];
        let burst = build_data_burst(&lc, 1, 1, &sync);
        assert_eq!(burst.len(), DMR_DATA_SIZE);
    }

    #[test]
    fn build_voice_lc_group() {
        let bits = build_voice_lc(true, 9, 12345, 1);
        // FLCO should be 0 (group voice) -> bits[2..8] = 0
        for (i, &bit) in bits[2..8].iter().enumerate() {
            assert_eq!(bit, 0, "FLCO bit {}", i + 2);
        }
    }

    #[test]
    fn build_voice_lc_private() {
        let bits = build_voice_lc(false, 12345, 67890, 1);
        // FLCO = 0b000011 -> bits[6]=1, bits[7]=1
        assert_eq!(bits[6], 1);
        assert_eq!(bits[7], 1);
    }

    #[test]
    #[should_panic(expected = "exceeds 24-bit max")]
    fn build_voice_lc_panics_on_src_id_over_24_bit() {
        // 24-bit subscriber ID field must reject > 2^24 (e.g. a
        // 32-bit hotspot repeater_id mistakenly routed as src_id)
        // rather than truncate onto another user's ID.
        let _ = build_voice_lc(true, 91, 310_770_201, 1);
    }

    #[test]
    fn slot_type_encode_length() {
        let bits = encode_slot_type(1, 1);
        assert_eq!(bits.len(), 20);
    }

    #[test]
    fn compare_against_bm_capture() {
        // Real voice LC header captured from BM TG 91, 2026-04-15.
        // The hotspot that sent it was configured with color_code=1
        // (the BM default), so we pin the expected cc rather than
        // scanning for any match -- otherwise a future encoder bug
        // that happened to match some other cc would go undetected.
        let captured =
            hex::decode("018b0aec1234093072c00500446dff57d75df5de327414f832a00ac008c12f82b3")
                .unwrap();
        let sync = super::super::sync::BS_DATA_SYNC;
        let lc = build_voice_lc(true, 91, 3151238, 1);
        let burst = build_data_burst(&lc, 1, 1, &sync);
        assert_eq!(
            burst[..],
            captured[..],
            "ours={} theirs={}",
            hex::encode(burst),
            hex::encode(&captured)
        );
    }

    #[test]
    fn decode_voice_lc_burst_rs_corrects_single_byte_error() {
        // Inject a one-byte error into the LC body (byte 6, the MSB
        // of src_id), BPTC-encode + burst, then decode.  RS(12,9)
        // should detect + correct the byte and report rs_corrected
        // = true with the ORIGINAL src_id recovered -- exercises the
        // rs_corrected=true branch that live tests alone don't hit.
        let sync = super::super::sync::BS_DATA_SYNC;
        let src_id = 3_151_238u32;
        let lc_bits = build_voice_lc(true, 91, src_id, 1);
        // LC byte 6 spans bits [48..56] MSB-first.  XOR the entire
        // byte to flip all 8 bits so it's a single-byte RS error
        // rather than a BPTC-correctable single-bit error.
        let mut corrupted = lc_bits;
        for b in &mut corrupted[48..56] {
            *b ^= 1;
        }
        let burst = build_data_burst(&corrupted, 1, 1, &sync);
        let decoded = decode_voice_lc_burst(&burst, 1).expect("BPTC decode");
        assert!(decoded.rs_corrected, "expected RS to correct single byte");
        assert_eq!(decoded.src_id, src_id, "RS correction produced wrong src");
        assert_eq!(decoded.dst_id, 91);
    }
}
