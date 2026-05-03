//! Verify `voice_channel::channel_decode` against chip-captured
//! goldens in `ambe/tests/fixtures/channel_coding/`.
//!
//! For each `uttNNN` in that directory we have:
//!
//!   .coded72 -- chip output at rate index 33 (DMR/FEC, 9 bytes/frame)
//!   .raw49   -- chip output at rate index 34 (raw 2450, 7 bytes/frame)
//!
//! Both files cover the same input PCM frame-for-frame, so frame `i`
//! of `.raw49` is the codec's pre-FEC bits for the same audio that
//! produced frame `i` of `.coded72`.  `channel_decode` should recover
//! `.raw49[i]` exactly from `.coded72[i]` for every frame.

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use dmr_wire::voice_channel::CODED_BYTES;
use dmr_wire::voice_channel::RAW_BYTES;
use dmr_wire::voice_channel::channel_decode;
use dmr_wire::voice_channel::channel_encode;

const FIXTURES_REL: &str = "../ambe/tests/fixtures/channel_coding";

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES_REL)
}

/// Iterate `(coded_72_frame, raw_49_frame)` pairs across every utterance.
fn pairs() -> Vec<(String, Vec<u8>, Vec<u8>)> {
    let dir = fixtures_dir();
    let mut entries: Vec<_> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| s == "coded72")
                .unwrap_or(false)
        })
        .map(|e| e.path())
        .collect();
    entries.sort();
    assert!(
        !entries.is_empty(),
        "no .coded72 fixtures in {}",
        dir.display()
    );

    entries
        .into_iter()
        .map(|coded_path| {
            let raw_path = coded_path.with_extension("raw49");
            let name = coded_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("?")
                .to_string();
            let coded = fs::read(&coded_path)
                .unwrap_or_else(|e| panic!("read {}: {e}", coded_path.display()));
            let raw =
                fs::read(&raw_path).unwrap_or_else(|e| panic!("read {}: {e}", raw_path.display()));
            (name, coded, raw)
        })
        .collect()
}

#[test]
fn channel_decode_matches_chip_raw_bits() {
    let utterances = pairs();
    let mut total_frames = 0usize;
    let mut total_mismatches = 0usize;
    for (name, coded, raw) in &utterances {
        assert!(
            coded.len() % CODED_BYTES == 0,
            "{name}: coded length not aligned"
        );
        assert!(raw.len() % RAW_BYTES == 0, "{name}: raw length not aligned");
        let n_frames = coded.len() / CODED_BYTES;
        assert_eq!(
            n_frames,
            raw.len() / RAW_BYTES,
            "{name}: coded vs raw frame count mismatch"
        );

        for i in 0..n_frames {
            let mut coded_frame = [0u8; CODED_BYTES];
            coded_frame.copy_from_slice(&coded[i * CODED_BYTES..(i + 1) * CODED_BYTES]);
            let mut expected = [0u8; RAW_BYTES];
            expected.copy_from_slice(&raw[i * RAW_BYTES..(i + 1) * RAW_BYTES]);
            let decoded = channel_decode(&coded_frame);
            if decoded != expected {
                total_mismatches += 1;
                if total_mismatches <= 5 {
                    eprintln!(
                        "{name} frame {i}: coded={:02x?} expected={:02x?} got={:02x?}",
                        coded_frame, expected, decoded
                    );
                }
            }
            total_frames += 1;
        }
    }
    assert_eq!(
        total_mismatches,
        0,
        "{} / {} frames decoded incorrectly across {} utterances",
        total_mismatches,
        total_frames,
        utterances.len()
    );
    eprintln!(
        "channel_decode: all {} frames across {} utterances match chip",
        total_frames,
        utterances.len()
    );
}

#[test]
fn channel_encode_matches_chip_coded_bytes() {
    let utterances = pairs();
    let mut total_frames = 0usize;
    let mut total_mismatches = 0usize;
    for (name, coded, raw) in &utterances {
        let n_frames = coded.len() / CODED_BYTES;
        for i in 0..n_frames {
            let mut raw_frame = [0u8; RAW_BYTES];
            raw_frame.copy_from_slice(&raw[i * RAW_BYTES..(i + 1) * RAW_BYTES]);
            let mut expected = [0u8; CODED_BYTES];
            expected.copy_from_slice(&coded[i * CODED_BYTES..(i + 1) * CODED_BYTES]);
            let encoded = channel_encode(&raw_frame);
            if encoded != expected {
                total_mismatches += 1;
                if total_mismatches <= 5 {
                    eprintln!(
                        "{name} frame {i}: raw49={:02x?} expected coded72={:02x?} got={:02x?}",
                        raw_frame, expected, encoded
                    );
                }
            }
            total_frames += 1;
        }
    }
    assert_eq!(
        total_mismatches,
        0,
        "{} / {} frames encoded incorrectly across {} utterances",
        total_mismatches,
        total_frames,
        utterances.len()
    );
    eprintln!(
        "channel_encode: all {} frames across {} utterances match chip",
        total_frames,
        utterances.len()
    );
}
