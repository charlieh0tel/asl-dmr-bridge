//! DMR SYNC patterns and EMB generation.
//!
//! The 48-bit center section of each voice burst is either a SYNC
//! pattern (for VoiceSync / burst A) or an EMB + embedded signaling
//! section (for voice bursts B-F).
//!
//! Values cross-checked against DMRGateway DMRDefines.h.

use super::fec::qr_16_7_encode;

/// 48-bit BS-sourced voice SYNC pattern, packed as 6 bytes.
/// Used in the VoiceSync burst (burst A, start of superframe).
///
/// Derived from DMRGateway BS_SOURCED_AUDIO_SYNC: extract the middle
/// 48 bits from the 56-bit pattern (mask 0x0FFFFFFFFFFF0).
pub(crate) const BS_VOICE_SYNC: [u8; 6] = [0x75, 0x5F, 0xD7, 0xDF, 0x75, 0xF7];

/// 48-bit BS-sourced data SYNC pattern (for voice LC header/terminator).
pub(crate) const BS_DATA_SYNC: [u8; 6] = [0xDF, 0xF5, 0x7D, 0x75, 0xDF, 0x5D];

/// Build the 48-bit (6-byte) EMB + embedded signaling section for
/// voice bursts B-F.
///
/// EMB layout (16 bits): CC(4) | PI(1) | LCSS(2) | QR_parity(9).
/// Split across the burst center: EMB_hi(8) | embedded_lc(32) | EMB_lo(8).
///
/// `embedded_lc` is the 32-bit LC fragment (4 bytes).  Use all zeros
/// for null embedded LC.
pub(crate) fn build_emb_section(color_code: u8, lcss: u8, embedded_lc: &[u8; 4]) -> [u8; 6] {
    // 7 info bits: CC(4) | PI(1)=0 | LCSS(2)
    let info: u8 = ((color_code & 0x0F) << 3) | (lcss & 0x03);
    let codeword = qr_16_7_encode(info);
    let emb_hi = (codeword >> 8) as u8;
    let emb_lo = (codeword & 0xFF) as u8;

    [
        emb_hi,
        embedded_lc[0],
        embedded_lc[1],
        embedded_lc[2],
        embedded_lc[3],
        emb_lo,
    ]
}

/// Build a null EMB section (no embedded LC data).
pub(crate) fn build_null_emb(color_code: u8) -> [u8; 6] {
    build_emb_section(color_code, 0, &[0; 4])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emb_cc_zero_lcss_zero() {
        let emb = build_emb_section(0, 0, &[0; 4]);
        // info = 0b0000000 = 0, QR(0) = 0x0000
        assert_eq!(emb[0], 0x00); // emb_hi
        assert_eq!(emb[5], 0x00); // emb_lo
        // embedded LC = zeros
        assert_eq!(&emb[1..5], &[0, 0, 0, 0]);
    }

    #[test]
    fn emb_cc1_lcss0() {
        let emb = build_emb_section(1, 0, &[0; 4]);
        // info = 0b0001000 = 8, QR(8) = QR_1676_TABLE[8] = 0x11E2
        assert_eq!(emb[0], 0x11); // emb_hi
        assert_eq!(emb[5], 0xE2); // emb_lo
    }

    #[test]
    fn emb_cc1_lcss1_matches_dmrpy() {
        // thomastoye/dmr-from-scratch dmrpy/pdu/emb_test.py:
        //   Emb.create_from_binary(0x1391) -> cc=1, lcss=1
        // Independent cross-check of QR_1676_TABLE[9] = 0x1391.
        let emb = build_emb_section(1, 1, &[0; 4]);
        assert_eq!(emb[0], 0x13);
        assert_eq!(emb[5], 0x91);
    }

    #[test]
    fn emb_embeds_lc_bytes() {
        let lc = [0xAA, 0xBB, 0xCC, 0xDD];
        let emb = build_emb_section(0, 0, &lc);
        assert_eq!(&emb[1..5], &lc);
    }

    #[test]
    fn emb_roundtrips_all_cc_and_lcss() {
        // For every (color_code, lcss) in 16 x 4 = 64 combinations,
        // build the EMB section, reassemble the 16-bit QR codeword
        // from the first and last bytes, and verify that QR decoding
        // yields back the same 7-bit info field.  Confirms both the
        // split layout (emb_hi | embedded_lc | emb_lo) and that the
        // encoded info survives a round trip.
        use super::super::fec::qr_16_7_decode;
        for cc in 0..16u8 {
            for lcss in 0..4u8 {
                let emb = build_emb_section(cc, lcss, &[0; 4]);
                let codeword = (u16::from(emb[0]) << 8) | u16::from(emb[5]);
                let info = qr_16_7_decode(codeword).expect("clean codeword decodes");
                assert_eq!(
                    info,
                    ((cc & 0x0F) << 3) | (lcss & 0x03),
                    "cc={cc} lcss={lcss}"
                );
            }
        }
    }
}
