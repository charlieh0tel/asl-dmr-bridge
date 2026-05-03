# ambe_bits2wav

CLI binary that converts AMBE+2 source bits to 8 kHz mono int16 WAV
via the chip-equivalent channel encoder + a vocoder backend.  Cargo
example in `asl-dmr-bridge`.

## Build

```
cargo build --release --example ambe_bits2wav -p asl-dmr-bridge \
    [--features thumbdv] [--features mbelib]
```

Binary at `target/release/examples/ambe_bits2wav`.  Default features
include only the `ambeserver` backend; add `thumbdv` for direct
serial, `mbelib` for the software decoder.

## Invoke

```
ambe_bits2wav --input bits.bin --output audio.wav \
    [--backend ambeserver|thumbdv|mbelib] \
    [--ambeserver host:port] [--serial path] [--baud rate] \
    [--no-decode] [--quiet]
```

| `--backend`  | Per-backend flags        | Default                |
|--------------|--------------------------|------------------------|
| `ambeserver` | `--ambeserver host:port` | `127.0.0.1:2460`       |
| `thumbdv`    | `--serial`, `--baud`     | `/dev/ttyUSB0`, 460800 |
| `mbelib`     | none                     | --                     |

`--no-decode` skips the backend round trip and writes the 9-byte
channel-coded stream to `--output` instead of a WAV.

## Input format

Concatenated 7-byte frames, one per 20 ms.  Each frame: 49 source
bits packed MSB-first (bit 0 at byte 0 bit 7, bit 48 at byte 6 bit
7, low 7 bits of byte 6 zero-padded).  Bit ordering is mbelib's
`ambe_d[0..49]`.  Input length must be a multiple of 7.

## Output format

WAV: 8 kHz mono int16 LE, 44-byte RIFF header + 160 samples per
frame.  `--no-decode`: raw 9-byte coded stream, no header.

## Verification

`dmr-wire::voice_channel` encode + decode + permutation are
bit-exact on 8208 chip-captured `(raw_49, coded_72)` frames
(`dmr-wire/tests/voice_channel_goldens.rs`).  End-to-end smoke
against `--backend ambeserver`: a captured `.raw49` stream
permuted to mbelib-order produces a WAV whose PCM body equals
an independently-captured chip-decoded reference of the same
speech, byte-for-byte.

## Caveats

- The chip (ambeserver or thumbdv) is single-consumer.
- WAV is buffered, not streamed.
