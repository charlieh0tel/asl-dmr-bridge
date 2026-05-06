# softambe-rs

Rust crate providing an AMBE+2 encode/decode API and an AMBEServer-compatible
UDP server, using the codec extracted from MD380 radio firmware JIT-emulated
via [yuzu-mirror/dynarmic](https://github.com/yuzu-mirror/dynarmic).

Runs on x86_64 and aarch64.

## Prerequisites

### System packages

| Package | Purpose |
|---------|---------|
| `cmake` | Builds the codec library |
| `git` | Fetches dynarmic at build time |
| `python3` | Firmware unwrap script |
| `unzip` | Firmware archive extraction |
| `xxd` | Embeds firmware as a C array |
| `libboost-dev` (>= 1.57) | Required by dynarmic |

On Debian/Ubuntu:

```
apt install cmake git python3 unzip xxd libboost-dev
```

On macOS (Homebrew):

```
brew install cmake git python3 unzip xxd boost
```

### Firmware

The MD380 firmware is downloaded, SHA256-verified, and embedded automatically
at build time.  It is not committed to this repo.

## Building

```
git clone --recurse-submodules <repo>
cd softambe-rs
cargo build --release
```

## Library usage

PCM frames are 160 samples at 8 kHz (20 ms).

```rust
// Raw AMBE+2 (7 bytes)
let ambe = softambe::encode(&pcm);
let pcm  = softambe::decode(&ambe);

// With DMR FEC layer (9 bytes)
let ambe = softambe::encode_fec(&pcm);
let pcm  = softambe::decode_fec(&ambe);
```

## AMBEServer

Listens on UDP port 2460.  Supports reset, RATEP (DMR 3600x2450), gain
(acknowledged, not applied), encode, and decode.  Resets codec state when
the client address changes.

```
softambeserver [--bind <addr>] [--port <port>]
```

| Option | Default | Description |
|--------|---------|-------------|
| `--bind` | `0.0.0.0` | IP address to listen on |
| `--port` | `2460` | UDP port |

Log verbosity via `RUST_LOG` (e.g. `RUST_LOG=debug`).
