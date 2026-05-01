//! Regression test: AMBEserver must decode `TEST_FRAMES` byte-for-byte
//! identically to the committed golden.
//!
//! Requires a running AMBEserver instance.  The address is taken from
//! `AMBE_SERVER_ADDR` (e.g., `127.0.0.1:2460`).  The test is `ignored`
//! by default; run with:
//!   AMBE_SERVER_ADDR=127.0.0.1:2460 cargo test -p ambe -- --ignored ambeserver_matches_golden
//!
//! Regenerate the golden via:
//!   cargo run -p ambe --example gen_golden -- ambeserver 127.0.0.1:2460

use ambe::test_harness::decode_test_frames;

const GOLDEN_PATH: &str = "tests/fixtures/ambeserver_golden.bin";

#[test]
#[ignore = "requires AMBEserver daemon; run with AMBE_SERVER_ADDR and --ignored"]
fn ambeserver_matches_golden() {
    let addr: std::net::SocketAddr = std::env::var("AMBE_SERVER_ADDR")
        .expect("AMBE_SERVER_ADDR must be set")
        .parse()
        .expect("parse addr");
    let golden = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN_PATH))
        .expect("ambeserver_golden.bin missing; generate via `gen_golden ambeserver`");

    let mut vocoder = ambe::open_ambeserver(addr, None).expect("connect ambeserver");
    let actual = decode_test_frames(vocoder.as_mut());
    assert_eq!(actual, golden);
}
