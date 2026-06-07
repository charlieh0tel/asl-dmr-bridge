# Developer and Test Tools

## ambe-tool

Standalone encode / decode / roundtrip utility for AMBE+2 files.
Supports all vocoder backends (thumbdv, ambeserver, dynarmic, neural)
and both frame formats (`.ambe` channel-coded, `.bin` 49-bit source bits).
See [FILE-FORMATS.md](FILE-FORMATS.md) for format details.

```
cargo build --release -p ambe-tool --features thumbdv,dynarmic,neural
ambe-tool encode --encoder dynarmic --in audio.wav --out utt.ambe
ambe-tool encode --encoder dynarmic --out-format bin --in audio.wav --out utt.bin
ambe-tool decode --decoder neural --decoder-model /path/to/weights --in utt.ambe --out decoded.wav
ambe-tool decode --decoder dynarmic --in-format bin --in utt.bin --out decoded.wav
ambe-tool roundtrip --encoder thumbdv --decoder dynarmic --in audio.wav --out rt.wav
```

## USRP test examples

These run against a live bridge without needing a second radio.

```
# Listen to decoded DMR audio through speakers
cargo run --example usrp_play

# Dump decoded DMR audio to raw PCM (pipe to aplay)
cargo run --example usrp_dump | aplay -f S16_LE -r 8000 -c 1

# Send raw PCM to the bridge as USRP (emulates chan_usrp)
cargo run --example usrp_send < voice.raw
```

## Parrot end-to-end TX test

Exercises the full encode chain via the BM TG 9990 parrot (private call).
See [PARROT-TEST.md](PARROT-TEST.md) for setup and failure interpretation.

```
cargo run --example parrot_test
```

## Vocoder fixture tools

### Golden file regeneration

Backend golden files commit the expected decoded output for 8 deterministic
test frames.  Regenerate after hardware replacement or library updates:

```
cargo run -p ambe --features thumbdv,testing --example gen_golden -- thumbdv /dev/ttyUSB0
cargo run -p ambe --features testing --example gen_golden -- ambeserver 127.0.0.1:2460
```

Each `.bin` ships with a companion `_golden.meta.toml` (regen timestamp,
ambe-crate version, `TEST_FRAMES` content).  Diff both files together when
reviewing a regen.  The matching integration tests (`ambe/tests/*_golden.rs`)
are `#[ignore]`'d by default since they require hardware or a running daemon.

### Real capture stress test

Fetches real captured DMR voice frames from pbarfuss/mbelib-testing (ISC
licensed) into `ambe/tests/fixtures/amb/` (gitignored):

```
cargo run -p ambe --example fetch_amb_samples
```

Then run the stress test (decodes every frame, asserts non-zero output):

```
cargo test -p ambe --features mbelib -- --ignored real_amb_samples
```
