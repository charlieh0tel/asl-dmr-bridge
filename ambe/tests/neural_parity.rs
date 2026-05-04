//! Frame-by-frame bit-equality test for the run-19 ONNX bundle.
//! Reads `$NEURAL_FIXTURE_DIR/{model.onnx,parity_input.wav,
//! parity_expected_49bit.bin}`; if the env var is unset the test
//! is skipped via `#[ignore]`-equivalent early return.
//!
//! Pass criterion: >= 99.5% of bits match the PT-canonical
//! reference.  Bit-identity isn't achievable at FP32 across
//! distinct execution paths; see the bundle's README.md.

use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use ambe::voice_channel::RAW_BITS;
use ambe::voice_channel::RAW_BYTES;
use ambe::voice_channel::channel_decode;
use ambe::voice_channel::permute_chip_to_mbelib;
use ambe::voice_channel::unpack_msb_first;

const PASS_THRESHOLD: f64 = 0.995;

fn fixture_dir() -> Option<PathBuf> {
    env::var_os("NEURAL_FIXTURE_DIR").map(PathBuf::from)
}

fn read_wav_pcm(path: &Path) -> Vec<i16> {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    // 44-byte canonical PCM WAV header; data follows.
    assert!(
        bytes.len() > 44,
        "{}: too short to be a WAV",
        path.display()
    );
    bytes[44..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

#[test]
fn run19_bit_parity() {
    let Some(dir) = fixture_dir() else {
        eprintln!("NEURAL_FIXTURE_DIR not set; skipping run19_bit_parity");
        return;
    };
    let model = dir.join("model.onnx");
    let pcm = read_wav_pcm(&dir.join("parity_input.wav"));
    let expected =
        fs::read(dir.join("parity_expected_49bit.bin")).expect("read parity_expected_49bit.bin");

    assert_eq!(pcm.len() % ambe::PCM_SAMPLES, 0);
    assert_eq!(expected.len() % RAW_BYTES, 0);
    let total_pcm_frames = pcm.len() / ambe::PCM_SAMPLES;
    let expected_frames = expected.len() / RAW_BYTES;
    let warmup = total_pcm_frames - expected_frames;

    let mut v = ambe::open_neural(&model).expect("open neural");

    let mut total_bits = 0usize;
    let mut matching_bits = 0usize;
    for f in 0..total_pcm_frames {
        let mut frame = [0i16; ambe::PCM_SAMPLES];
        frame.copy_from_slice(&pcm[f * ambe::PCM_SAMPLES..(f + 1) * ambe::PCM_SAMPLES]);
        let coded = v.encode(&frame).expect("encode");
        if f < warmup {
            // Harness emits zeros during warm-up; expected fixture
            // skips these.
            continue;
        }
        // Recover 49 bits in mbelib `ambe_d[]` order.
        let chip_packed = channel_decode(&coded);
        let mbelib_packed = permute_chip_to_mbelib(&chip_packed);
        let actual_bits = unpack_msb_first(&mbelib_packed);

        let exp_idx = f - warmup;
        let exp_packed: &[u8; RAW_BYTES] = expected[exp_idx * RAW_BYTES..(exp_idx + 1) * RAW_BYTES]
            .try_into()
            .unwrap();
        let exp_bits = unpack_msb_first(exp_packed);

        for i in 0..RAW_BITS {
            total_bits += 1;
            if actual_bits[i] == exp_bits[i] {
                matching_bits += 1;
            }
        }
    }

    let match_rate = matching_bits as f64 / total_bits as f64;
    eprintln!(
        "neural_parity: {matching_bits}/{total_bits} bits match ({:.4}%)",
        match_rate * 100.0
    );
    assert!(
        match_rate >= PASS_THRESHOLD,
        "neural_parity match rate {:.4}% < {:.1}%",
        match_rate * 100.0,
        PASS_THRESHOLD * 100.0,
    );
}
