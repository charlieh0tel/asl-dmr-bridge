// Integration tests for the softambe codec.
//
// TEST_FRAMES are the same synthetic AMBE+2 FEC frames used by the
// asl-dmr-bridge golden tests, chosen to exercise different bit patterns.
//
// Note: AMBE+2 is a predictive codec with inter-frame state.  The same
// frame decoded at different points in a stream will produce different PCM.
// Tests here verify size/shape and basic codec sanity, not exact output.

use softambe::{AMBE_FEC_BYTES, PCM_FRAME_SAMPLES};

const TEST_FRAMES: [[u8; AMBE_FEC_BYTES]; 8] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
    [0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55],
    [0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA, 0x55, 0xAA],
    [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x12],
    [0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54, 0x32, 0x10, 0xED],
    [0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80],
    [0xA5, 0x5A, 0x33, 0xCC, 0x0F, 0xF0, 0x3C, 0xC3, 0x5A],
];

#[test]
fn decode_fec_all_test_frames() {
    for (i, frame) in TEST_FRAMES.iter().enumerate() {
        let pcm = softambe::decode_fec(frame);
        assert_eq!(
            pcm.len(),
            PCM_FRAME_SAMPLES,
            "frame {i}: wrong sample count"
        );
    }
}

#[test]
fn encode_fec_produces_correct_size() {
    let pcm = [0i16; PCM_FRAME_SAMPLES];
    let frame = softambe::encode_fec(&pcm);
    assert_eq!(frame.len(), AMBE_FEC_BYTES);
}

#[test]
fn encode_decode_fec_roundtrip_silence() {
    // AMBE+2 is lossy; silence should roundtrip to near-silence.
    let pcm_in = [0i16; PCM_FRAME_SAMPLES];
    let frame = softambe::encode_fec(&pcm_in);
    let pcm_out = softambe::decode_fec(&frame);
    let max_abs = pcm_out.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    assert!(
        max_abs < 1000,
        "silence roundtrip produced unexpectedly large samples: max={max_abs}"
    );
}
