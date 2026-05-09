//! Integration tests for the dynarmic backend's `Vocoder` impl.
//!
//! AMBE+2 is a predictive codec with inter-frame state.  The same
//! frame decoded at different points in a stream produces different
//! PCM, so tests verify size/shape and basic codec sanity, not
//! exact output.
//!
//! Codec state is process-global (dynarmic emulates a single chip);
//! `#[serial]` forces serialization within this binary so tests
//! don't race each other's predictor history.

use dv3000_wire::AMBE_FRAME_SIZE;
use dv3000_wire::AmbeFrame;
use dv3000_wire::PCM_SAMPLES;
use dv3000_wire::PcmFrame;
use hound::SampleFormat;
use hound::WavReader;
use serial_test::serial;

/// AMBE+2 internally produces two subframes per 160-sample frame.
const SUBFRAME_SAMPLES: usize = PCM_SAMPLES / 2;

/// Roundtripping silence through encode+decode shouldn't produce
/// PCM far from zero; cap at ~3% full scale.
const SILENCE_MAX_ABS: u16 = 1000;

/// Boundary jumps may legitimately exceed interior jumps slightly
/// at frame transitions; cap at 2x interior p99 before treating a
/// jump as a tick artifact.
const BOUNDARY_RATIO_LIMIT: f64 = 2.0;

/// Minimum frames in the voice fixture for percentile statistics
/// to be meaningful.
const MIN_FIXTURE_FRAMES: usize = 32;

/// Frames discarded from the start before measuring tick statistics
/// to let the predictor settle from cold state.
const WARMUP_FRAMES: usize = 4;

const TEST_FRAMES: [AmbeFrame; 8] = [
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
#[serial]
fn decode_all_test_frames() {
    let mut v = ambe::open_dynarmic();
    v.reset();
    for (i, frame) in TEST_FRAMES.iter().enumerate() {
        let pcm = v.decode(Some(frame)).expect("decode");
        assert_eq!(pcm.len(), PCM_SAMPLES, "frame {i}: wrong sample count");
    }
}

#[test]
#[serial]
fn encode_produces_correct_size() {
    let mut v = ambe::open_dynarmic();
    v.reset();
    let pcm: PcmFrame = [0i16; PCM_SAMPLES];
    let frame = v.encode(&pcm).expect("encode");
    assert_eq!(frame.len(), AMBE_FRAME_SIZE);
}

#[test]
#[serial]
fn encode_decode_roundtrip_silence() {
    let mut v = ambe::open_dynarmic();
    v.reset();
    let pcm_in: PcmFrame = [0i16; PCM_SAMPLES];
    let frame = v.encode(&pcm_in).expect("encode");
    let pcm_out = v.decode(Some(&frame)).expect("decode");
    let max_abs = pcm_out.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
    assert!(
        max_abs < SILENCE_MAX_ABS,
        "silence roundtrip max={max_abs} >= {SILENCE_MAX_ABS}",
    );
}

/// Detect "ticks" --- sample-level discontinuities at AMBE
/// subframe boundaries (every SUBFRAME_SAMPLES).  A buggy decoder
/// leaves a step there that wouldn't appear in interior samples.
/// Drive with real speech (varied per-frame params expose any state
/// bug that a stationary tone would mask), then compare boundary-
/// jump p99 against interior-jump p99.
#[test]
#[serial]
fn no_ticks_at_frame_boundaries() {
    let pcm = read_wav_8k_mono_i16("tests/fixtures/voice.wav");
    let frames = pcm.len() / PCM_SAMPLES;
    assert!(
        frames >= MIN_FIXTURE_FRAMES,
        "need at least {MIN_FIXTURE_FRAMES} frames of voice; got {frames}"
    );

    let mut v = ambe::open_dynarmic();
    v.reset();
    let mut ambe_stream: Vec<AmbeFrame> = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut frame: PcmFrame = [0i16; PCM_SAMPLES];
        frame.copy_from_slice(&pcm[f * PCM_SAMPLES..(f + 1) * PCM_SAMPLES]);
        ambe_stream.push(v.encode(&frame).expect("encode"));
    }

    v.reset();
    let mut out: Vec<i16> = Vec::with_capacity(frames * PCM_SAMPLES);
    for frame in &ambe_stream {
        out.extend_from_slice(&v.decode(Some(frame)).expect("decode"));
    }

    let start = WARMUP_FRAMES * PCM_SAMPLES;
    let diffs: Vec<i32> = out[start..]
        .windows(2)
        .map(|w| (w[1] as i32 - w[0] as i32).abs())
        .collect();

    let mut boundary = Vec::new();
    let mut interior = Vec::new();
    for (i, &d) in diffs.iter().enumerate() {
        let pos = i + 1;
        if pos.is_multiple_of(SUBFRAME_SAMPLES) {
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

    let limit = (interior_p99 as f64 * BOUNDARY_RATIO_LIMIT) as i32;
    assert!(
        boundary_p99 <= limit,
        "ticks detected: boundary p99={boundary_p99} exceeds {BOUNDARY_RATIO_LIMIT}*interior_p99={limit}\n\
         (interior median={interior_median} p99={interior_p99}, \
         boundary median={boundary_median} p99={boundary_p99} max={boundary_max})"
    );
}

fn read_wav_8k_mono_i16(path: &str) -> Vec<i16> {
    let mut reader = WavReader::open(path).unwrap_or_else(|e| panic!("open {path}: {e}"));
    let spec = reader.spec();
    assert!(
        spec.channels == 1
            && spec.sample_rate == 8000
            && spec.bits_per_sample == 16
            && spec.sample_format == SampleFormat::Int,
        "{path}: expected 8 kHz mono i16 PCM, got {spec:?}",
    );
    reader.samples::<i16>().collect::<Result<_, _>>().unwrap()
}
