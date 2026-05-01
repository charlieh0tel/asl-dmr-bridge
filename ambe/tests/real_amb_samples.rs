//! Decode real AMBE+2 captures and sanity-check the output.
//!
//! Fetch the .amb files first:
//!   cargo run -p ambe --example fetch_amb_samples
//!
//! Then run the ignored test:
//!   cargo test -p ambe --features mbelib -- --ignored real_amb_samples
//!
//! These files are captured real-world DMR voice frames.  We decode
//! every frame and assert properties consistent with speech audio (non-
//! silent, not saturated, mean magnitude in voice range).  No reference
//! PCM is available so this is a property-based sanity check, not a
//! byte-exact regression test.
//!
//! File format (from pbarfuss/mbelib-testing decode_ambe.c):
//!   4-byte cookie ".amb"
//!   repeating 8-byte frames:
//!     byte 0: error-correction byte (ignored here)
//!     bytes 1-6: bits 0..47 of ambe_d, MSB-first (48 bits)
//!     byte 7: LSB is bit 48 of ambe_d (1 bit)

#![cfg(feature = "mbelib")]

use std::path::PathBuf;

use ambe::AMBE_FRAME_SIZE;
use ambe::AmbeFrame;

const FILES: &[&str] = &[
    "bmh_gasline.amb",
    "bmh_gasline_redux.amb",
    "davis_center_doors.amb",
];

const AMB_HEADER_LEN: usize = 4;
const AMB_FRAME_LEN: usize = 8;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("amb")
}

/// Convert one 8-byte .amb frame to our 9-byte `AmbeFrame` format.
/// Drops the errs byte.  Packs 49 ambe_d bits MSB-first into bytes
/// 0..6 (with bit 48 as the MSB of byte 6); remaining bits are zero.
fn amb_to_ambe_frame(amb: &[u8; AMB_FRAME_LEN]) -> AmbeFrame {
    let mut out: AmbeFrame = [0; AMBE_FRAME_SIZE];
    // .amb bytes 1..=6 already hold bits 0..47 MSB-first.
    out[0..6].copy_from_slice(&amb[1..7]);
    // .amb byte 7 LSB is bit 48; map to MSB of our byte 6.
    out[6] = (amb[7] & 1) << 7;
    out
}

#[test]
#[ignore = "requires fetched .amb files; run fetch_amb_samples example first"]
fn real_amb_samples_decode() {
    let dir = fixtures_dir();
    assert!(
        dir.exists(),
        "fixtures/amb/ missing; run `cargo run -p ambe --example fetch_amb_samples` first"
    );

    for name in FILES {
        let path = dir.join(name);
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        assert!(bytes.len() > AMB_HEADER_LEN, "{name}: truncated");
        assert_eq!(&bytes[..AMB_HEADER_LEN], b".amb", "{name}: bad cookie");
        let frames = &bytes[AMB_HEADER_LEN..];
        assert_eq!(
            frames.len() % AMB_FRAME_LEN,
            0,
            "{name}: {} payload bytes not a multiple of {AMB_FRAME_LEN}",
            frames.len()
        );
        let frame_count = frames.len() / AMB_FRAME_LEN;

        let mut vocoder = ambe::open_mbelib();
        let mut total_abs: u64 = 0;
        let mut max_abs: i32 = 0;
        let mut saturated: u64 = 0;
        let total_samples = frame_count as u64 * 160;

        for i in 0..frame_count {
            let mut amb: [u8; AMB_FRAME_LEN] = [0; AMB_FRAME_LEN];
            amb.copy_from_slice(&frames[i * AMB_FRAME_LEN..(i + 1) * AMB_FRAME_LEN]);
            let frame = amb_to_ambe_frame(&amb);
            let pcm = vocoder
                .decode(&frame)
                .unwrap_or_else(|e| panic!("{name} frame {i}: {e}"));
            for s in pcm {
                total_abs += u64::from(s.unsigned_abs());
                max_abs = max_abs.max(i32::from(s.abs()));
                if s == i16::MAX || s == i16::MIN {
                    saturated += 1;
                }
            }
        }

        let mean_abs = total_abs / total_samples;
        let sat_pct = (saturated * 100) as f64 / total_samples as f64;
        println!(
            "{name}: {frame_count} frames, mean|sample|={mean_abs}, \
             max|sample|={max_abs}, saturated={saturated} ({sat_pct:.3}%)"
        );

        // Voice audio: not silent, but not DC-loud either.  Real speech
        // captures we've seen sit around mean|sample| ~1000-3000.
        assert!(
            (200..=8000).contains(&mean_abs),
            "{name}: mean|sample|={mean_abs} outside voice range [200, 8000]"
        );
        assert!(
            max_abs > 1000,
            "{name}: output suspiciously quiet (max_abs={max_abs})"
        );
        // Healthy decode shouldn't pin to the rails for more than a
        // handful of samples.
        assert!(
            sat_pct < 0.5,
            "{name}: too many saturated samples ({sat_pct:.3}%)"
        );
    }
}
