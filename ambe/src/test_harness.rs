//! Shared test harness for any `Vocoder` implementation.
//!
//! Used by the per-backend integration tests in `ambe/tests/` and by
//! the golden-file generator examples in `ambe/examples/`.

use crate::Vocoder;
use crate::test_vectors::TEST_FRAMES;

/// Decode all `TEST_FRAMES` through the given vocoder and concatenate
/// the PCM output as little-endian i16 bytes.  State carries across
/// frames, matching real-world use.
pub fn decode_test_frames(vocoder: &mut dyn Vocoder) -> Vec<u8> {
    let mut out = Vec::with_capacity(TEST_FRAMES.len() * crate::PCM_SAMPLES * 2);
    for (i, frame) in TEST_FRAMES.iter().enumerate() {
        let pcm = vocoder
            .decode(frame)
            .unwrap_or_else(|e| panic!("test frame {i} decode failed: {e}"));
        for sample in pcm {
            out.extend_from_slice(&sample.to_le_bytes());
        }
    }
    out
}
