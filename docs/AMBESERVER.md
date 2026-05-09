# ambeserver

UDP <-> AMBE-3000R proxy.  Wire-compatible with OpenDV-protocol
clients regardless of which backend serves them: a real ThumbDV
chip on the host, an in-process software vocoder, or both via
separate processes.

## Backends

```
ambeserver --backend thumbdv  --serial /dev/ttyUSB0 [--baud 460800]
ambeserver --backend dynarmic
ambeserver --backend neural   --model-path path/to/model.onnx
```

`--listen` defaults to `127.0.0.1:2460`.

- **thumbdv** (default): byte-for-byte serial relay to a ThumbDV (or
  any DVSI AMBE-3000R over a serial port).  Clients init the chip
  themselves at startup (`RESET` -> `RATEP` -> optional `GAIN`); the
  proxy never parses, so chip-side semantics pass through unchanged.

- **dynarmic**: in-process software vocoder.  ambeserver fabricates
  the chip's responses for control packets (`RESET` -> `READY`,
  `RATEP` -> short ack, `GAIN` -> short ack, `PRODID` -> `"AMBE3000R
  / dynarmic"`) and runs encode/decode through `ambe::Vocoder`.

- **neural**: same wrapper, but the encoder is the ONNX model
  selected by `--model-path`.  Decode delegates to dynarmic.

Build with the matching cargo feature:

```
cargo build -p ambeserver                                      # default = thumbdv
cargo build -p ambeserver --no-default-features --features dynarmic
cargo build -p ambeserver --features neural
```

## RATEP rejection (software backends)

The software backends only support DMR (AMBE+2 3600x2450, RATEP
index 33).  A RATEP packet with any other rate is rejected on the
wire (chip-style negative ack) and the session is poisoned: every
subsequent packet is dropped until the client sends a fresh `RESET`.
Voice packets received before a successful RATEP DMR are dropped on
the same defense-in-depth principle.  The chip backend has no such
filter -- the chip itself is the source of truth.

## Holder exclusivity

One peer drives the backend at a time; others are refused (clean
UDP timeout) until the holder goes idle for ~1 s.  When a new peer
takes over, the soft backend resets vocoder per-stream state to
keep one client's predictor history out of the next client's stream.
