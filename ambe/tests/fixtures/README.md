# Vocoder test fixtures

## Synthetic test vectors

`ambe/src/test_vectors.rs::TEST_FRAMES` defines 8 deterministic AMBE+2
frames.  Each backend's decoded output is committed as a golden file:

- `mbelib_golden.bin`      -- szechyjs/mbelib output (committed)
- `thumbdv_golden.bin`     -- DVSI AMBE-3000 output (not committed;
                              requires hardware)
- `ambeserver_golden.bin`  -- AMBEserver daemon output (not committed;
                              requires daemon)

Each golden is 2560 bytes (8 frames x 160 samples x 2 bytes LE i16).

Regenerate (the `testing` feature exposes `test_harness` /
`test_vectors` to the example):

```
cargo run -p ambe --features mbelib,testing --example gen_golden -- mbelib
cargo run -p ambe --features thumbdv,testing --example gen_golden -- thumbdv /dev/ttyUSB0
cargo run -p ambe --features testing --example gen_golden -- ambeserver 127.0.0.1:2460
```

Each `.bin` ships with a companion `_golden.meta.toml` recording the
regen timestamp, ambe-crate version, and the `TEST_FRAMES` content
that was decoded.  Diff both files together when reviewing a regen.

The matching integration tests (`ambe/tests/*_golden.rs`) verify each
backend reproduces its golden byte-for-byte.  Non-mbelib tests are
`#[ignore]`'d by default since they require hardware or a running
daemon.

## Real AMBE+2 captures

Real captured DMR voice frames from pbarfuss/mbelib-testing (ISC
licensed code, captured audio).  Fetched at test time -- not
committed.

Fetch:

```
cargo run -p ambe --example fetch_amb_samples
```

This populates `fixtures/amb/` (gitignored) with:
- bmh_gasline.amb
- bmh_gasline_redux.amb
- davis_center_doors.amb

Run the sanity test:

```
cargo test -p ambe --features mbelib -- --ignored real_amb_samples
```

The test decodes every frame and asserts non-zero output.  No
authoritative reference PCM is available, so this is a stress/crash
test, not a regression check.
