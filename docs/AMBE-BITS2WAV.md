# ambe_bits2wav

CLI binary that converts AMBE+2 source bits to 8 kHz mono int16 WAV
PCM via the chip-equivalent channel encoder + an ambeserver decode
round trip.  Lives in `asl-dmr-bridge` as a cargo example.

## Build

```
cargo build --release --example ambe_bits2wav -p asl-dmr-bridge
```

The resulting binary is at
`target/release/examples/ambe_bits2wav`.

## Invoke

```
ambe_bits2wav --input bits.bin --output audio.wav \
    [--ambeserver host:port] [--no-decode] [--quiet]
```

Required: `--input`, `--output`.  Defaults: `--ambeserver
127.0.0.1:2460`.  Exit code 0 on success, non-zero on failure with
an explanation on stderr.  `--quiet` suppresses progress chatter.

## Input format

Concatenated 7-byte frames, one per 20 ms of speech.  Each frame is
49 source bits packed MSB-first into 7 bytes (the high bit of byte 0
is bit 0; bit 48 lands at byte 6 bit 7; the low 7 bits of byte 6 are
zero-padded).  Bit ordering is mbelib's `ambe_d[0..49]` semantic
order: `ambe_d[0]` is the first bit of the frame, `ambe_d[48]` is
the last.

The input length must be a multiple of 7; otherwise the binary
errors out and writes nothing.

## Output format

By default: PCM WAV, 8 kHz mono int16 LE, 44-byte canonical RIFF
header followed by interleaved samples.  One frame's audio is 160
samples (20 ms).

With `--no-decode`: the 9-byte-per-frame channel-coded stream
written verbatim, no WAV header.  No ambeserver connection is
attempted in this mode (useful when the chip is not available).

## Pipeline

```
input bits.bin
  -> chunk into 7-byte frames
  -> permute mbelib-order -> chip-natural-order (per-frame)
  -> channel_encode (49 -> 72) bit-exact against AMBE-3000R rate-33
  -> [if not --no-decode] ambeserver decode (72 -> PCM)
  -> write WAV (or write coded stream for --no-decode)
```

## Verification

The encoder + permutation in `dmr-wire::voice_channel` were
verified bit-for-bit against 8208 chip-captured `(raw_49, coded_72)`
frames across 12 utterances; both `channel_decode` and
`channel_encode` reproduce the chip's wire form exactly (golden
tests in `dmr-wire/tests/voice_channel_goldens.rs`).

End-to-end smoke: a captured `.raw49` frame stream (chip-order)
was permuted to mbelib-order, fed through `ambe_bits2wav`, and the
resulting WAV's PCM body compared to an independently-captured
chip-decoded round trip of the same speech.  Match was bit-exact
(225600 PCM bytes equal byte-for-byte).

## Caveats

- ambeserver must be reachable on `--ambeserver` (default
  `127.0.0.1:2460`) for the decode path.  Start it before
  invocation; the binary doesn't manage chip lifecycle.
- The ambeserver / chip is single-consumer.  Don't run multiple
  invocations concurrently against the same chip.
- WAV is not memory-streamed; the full PCM is buffered then written.
  At 16 kB/s this is fine for utterance-scale inputs (minutes of
  audio fits in MB).
