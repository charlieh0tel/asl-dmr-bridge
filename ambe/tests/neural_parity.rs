//! Frame-by-frame bit-equality test for the aug50 ONNX bundle.
//! Reads `model.onnx`, `parity_input.wav`, and
//! `parity_expected_49bit.bin` from `tests/fixtures/aug50/`.
//! `$NEURAL_FIXTURE_DIR` overrides for ad-hoc bundles.
//!
//! Pass criterion: 100% of bits match the PT-canonical reference.
//! If a future bundle drifts, lower the threshold deliberately and
//! note why instead of accepting silent regression.

use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use ambe::voice_channel::RAW_BITS;
use ambe::voice_channel::RAW_BYTES;
use ambe::voice_channel::channel_decode;
use ambe::voice_channel::permute_chip_to_mbelib;
use ambe::voice_channel::unpack_msb_first;
use tract_onnx::prelude::Framework;

const PASS_THRESHOLD: f64 = 1.0;

struct FixturePaths {
    model: PathBuf,
    wav: PathBuf,
    expected_bin: PathBuf,
}

fn fixture_paths() -> FixturePaths {
    if let Some(p) = env::var_os("NEURAL_FIXTURE_DIR") {
        let dir = PathBuf::from(p);
        return FixturePaths {
            model: dir.join("model.onnx"),
            wav: dir.join("parity_input.wav"),
            expected_bin: dir.join("parity_expected_49bit.bin"),
        };
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = manifest.join("tests").join("fixtures").join("aug50");
    FixturePaths {
        model: fixture.join("model.onnx"),
        wav: fixture.join("parity_input.wav"),
        expected_bin: fixture.join("parity_expected_49bit.bin"),
    }
}

/// Read `nambe.harness_lookback_samples` from the ONNX metadata so
/// the test can cross-check the fixture's warmup-frame count.
fn harness_lookback_samples(model: &Path) -> usize {
    let proto = tract_onnx::onnx()
        .proto_model_for_path(model)
        .unwrap_or_else(|e| panic!("load {}: {e}", model.display()));
    let kv = proto
        .metadata_props
        .iter()
        .find(|kv| kv.key == "nambe.harness_lookback_samples")
        .expect("ONNX missing nambe.harness_lookback_samples");
    kv.value
        .parse::<usize>()
        .unwrap_or_else(|e| panic!("nambe.harness_lookback_samples: {e}"))
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
fn aug50_bit_parity() {
    let paths = fixture_paths();
    if !paths.model.exists() {
        eprintln!(
            "neural_parity: model missing at {}; skipping",
            paths.model.display()
        );
        return;
    }
    let pcm = read_wav_pcm(&paths.wav);
    let expected = fs::read(&paths.expected_bin).expect("read parity_expected_49bit.bin");

    assert_eq!(pcm.len() % ambe::PCM_SAMPLES, 0);
    assert_eq!(expected.len() % RAW_BYTES, 0);
    let total_pcm_frames = pcm.len() / ambe::PCM_SAMPLES;
    let expected_frames = expected.len() / RAW_BYTES;
    let warmup = total_pcm_frames - expected_frames;

    // Cross-check: the fixture's warmup-frame count must hold enough
    // PCM history for the model's lookback.  A future bundle that
    // ships mismatched fixture vs. metadata would silently pass
    // against misaligned frames otherwise.
    let lookback = harness_lookback_samples(&paths.model);
    assert!(
        warmup * ambe::PCM_SAMPLES >= lookback,
        "fixture warmup={warmup} frames ({} samples) < harness_lookback_samples={lookback}",
        warmup * ambe::PCM_SAMPLES,
    );

    let mut v = ambe::open_neural(&paths.model).expect("open neural");

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
