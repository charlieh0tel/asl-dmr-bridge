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

/// Detect "ticks" --- sample-level discontinuities at AMBE frame/subframe
/// boundaries.  AMBE+2 produces 160 samples per 20 ms frame, internally as
/// two 80-sample subframes.  A buggy decoder leaves a step at sample
/// positions 80, 160, 240, ...
///
/// Drive the codec with real speech (a chunk lifted from the nambe corpus,
/// 8 kHz mono i16).  Real speech makes the encoder pick varied params per
/// frame, exposing any frame-boundary state bug that a stationary tone
/// would mask.  We then compare the p99 jump magnitude at subframe
/// boundaries against the p99 jump in the interior.  A clean decoder has
/// these comparable; a tick-producing decoder has boundary jumps that
/// stick out.
#[test]
fn no_ticks_at_frame_boundaries() {
    let pcm = read_wav_i16("tests/fixtures/voice.wav");
    let frames = pcm.len() / PCM_FRAME_SAMPLES;
    assert!(frames >= 32, "need at least 32 frames of voice; got {frames}");

    softambe::reset();
    let mut ambe_stream = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut frame = [0i16; PCM_FRAME_SAMPLES];
        frame.copy_from_slice(&pcm[f * PCM_FRAME_SAMPLES..(f + 1) * PCM_FRAME_SAMPLES]);
        ambe_stream.push(softambe::encode_fec(&frame));
    }

    softambe::reset();
    let mut out: Vec<i16> = Vec::with_capacity(frames * PCM_FRAME_SAMPLES);
    for frame in &ambe_stream {
        out.extend_from_slice(&softambe::decode_fec(frame));
    }

    // Skip a warm-up region; the first few frames of any AMBE+2 decode are
    // ramping in from cold predictor state.
    const WARMUP_FRAMES: usize = 4;
    let start = WARMUP_FRAMES * PCM_FRAME_SAMPLES;

    let diffs: Vec<i32> = out[start..]
        .windows(2)
        .map(|w| (w[1] as i32 - w[0] as i32).abs())
        .collect();

    // Subframe boundaries land every 80 samples within the concatenated
    // output: indices 80, 160, 240, ...  diffs[i-1] is the jump landing
    // on sample i.
    let mut boundary = Vec::new();
    let mut interior = Vec::new();
    for (i, &d) in diffs.iter().enumerate() {
        let pos = i + 1;
        if pos % 80 == 0 {
            boundary.push(d);
        } else {
            interior.push(d);
        }
    }

    let p = |v: &[i32], pct: usize| -> i32 {
        let mut s: Vec<i32> = v.to_vec();
        s.sort();
        s[s.len() * pct / 100]
    };

    let interior_median = p(&interior, 50);
    let interior_p99 = p(&interior, 99);
    let boundary_median = p(&boundary, 50);
    let boundary_p99 = p(&boundary, 99);
    let boundary_max = *boundary.iter().max().unwrap_or(&0);

    eprintln!(
        "interior: n={}, median={interior_median}, p99={interior_p99}",
        interior.len()
    );
    eprintln!(
        "boundary: n={}, median={boundary_median}, p99={boundary_p99}, max={boundary_max}",
        boundary.len()
    );

    // A clean decoder has boundary p99 within ~2x the interior p99 --- the
    // codec naturally produces some discontinuity at frame transitions
    // even when working correctly, but it shouldn't be much larger than
    // the typical sample-to-sample jump in voiced speech.
    let limit = (interior_p99 as f64 * 2.0) as i32;
    assert!(
        boundary_p99 <= limit,
        "ticks detected: boundary p99={boundary_p99} exceeds 2*interior_p99={limit}\n\
         (interior median={interior_median} p99={interior_p99}, \
         boundary median={boundary_median} p99={boundary_p99} max={boundary_max})"
    );
}

/// Read a 8 kHz mono 16-bit PCM WAV file as `Vec<i16>`.  Trusts a 44-byte
/// canonical RIFF/WAVE/fmt/data layout (no extra chunks).
fn read_wav_i16(path: &str) -> Vec<i16> {
    let bytes = std::fs::read(path).expect("read wav");
    assert!(&bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE");
    assert!(&bytes[36..40] == b"data", "unexpected WAV layout");
    let data_len = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
    let samples = data_len / 2;
    let mut out = Vec::with_capacity(samples);
    for i in 0..samples {
        let off = 44 + i * 2;
        out.push(i16::from_le_bytes([bytes[off], bytes[off + 1]]));
    }
    out
}
