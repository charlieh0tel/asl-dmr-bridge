//! Regression test: ThumbDV must decode `TEST_FRAMES` byte-for-byte
//! identically to the committed golden.
//!
//! Requires hardware.  The serial port path is taken from
//! `AMBE_THUMBDV_PORT` (e.g., `/dev/ttyUSB0`).  The test is `ignored`
//! by default; run with:
//!   AMBE_THUMBDV_PORT=/dev/ttyUSB0 cargo test -p ambe -- --ignored thumbdv_matches_golden
//!
//! Regenerate the golden via:
//!   AMBE_THUMBDV_PORT=/dev/ttyUSB0 cargo run -p ambe --example gen_golden -- thumbdv $AMBE_THUMBDV_PORT

#![cfg(feature = "thumbdv")]

use ambe::test_harness::decode_test_frames;

const GOLDEN_PATH: &str = "tests/fixtures/thumbdv_golden.bin";

#[test]
#[ignore = "requires ThumbDV hardware; run with AMBE_THUMBDV_PORT and --ignored"]
fn thumbdv_matches_golden() {
    let port = std::env::var("AMBE_THUMBDV_PORT").expect("AMBE_THUMBDV_PORT must be set");
    let golden = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_PATH))
        .expect("thumbdv_golden.bin missing; generate via `gen_golden thumbdv`");

    let mut vocoder = ambe::open_thumbdv(&port, None, None).expect("open thumbdv");
    let actual = decode_test_frames(vocoder.as_mut());
    assert_eq!(actual, golden);
}
