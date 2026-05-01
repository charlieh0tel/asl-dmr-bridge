//! Regression test: mbelib must decode `TEST_FRAMES` byte-for-byte
//! identically to the committed golden.
//!
//! Regenerate the golden via:
//!   cargo run -p ambe --features mbelib --example gen_golden -- mbelib

#![cfg(feature = "mbelib")]

use ambe::test_harness::decode_test_frames;

const GOLDEN: &[u8] = include_bytes!("fixtures/mbelib_golden.bin");

#[test]
fn mbelib_matches_golden() {
    let mut vocoder = ambe::open_mbelib();
    let actual = decode_test_frames(vocoder.as_mut());
    assert_eq!(actual.len(), GOLDEN.len(), "output length mismatch");
    assert_eq!(
        actual, GOLDEN,
        "mbelib output differs from committed golden; \
         regenerate with `cargo run -p ambe --features mbelib --example gen_golden -- mbelib` \
         if this is intentional"
    );
}
