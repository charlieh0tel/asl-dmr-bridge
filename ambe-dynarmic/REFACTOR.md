# ambe-rs Architecture

## Goal

Extract the `ambe` crate from `asl-dmr-bridge` and generalize it into a
publishable workspace that supports multiple AMBE+2 backends.  The current
`softambe-rs` repo becomes `ambe-dynarmic-rs`, one backend among several.

## Repo layout

```
ambe-rs/                    (workspace root, no submodules, edition 2024)
  Cargo.toml                workspace manifest
  ambe-rs/                  core crate
  ambe-dynarmic-rs/         dynarmic/MD380 backend
  ambe-neural-rs/           neural backend (tract ONNX encoder + dynarmic decoder)
  ambeserver/               server binary
```

mbelib is excluded (see Open questions).

## Crate responsibilities

### ambe-rs  (core)

Lifted verbatim from `asl-dmr-bridge/ambe/` (with history; see below).

- `Vocoder` trait: `encode`, `decode(Option<&AmbeFrame>)`, `reset`
  - `decode` takes `Option` to support erasure/packet-drop (None -> silence)
- Shared types: `PcmFrame = [i16; 160]`, `AmbeFrame = [u8; 9]`
- Pure-Rust codec support: `voice_channel`, `codeword`, `rates`, `wire`
- DV3000 protocol framing
- AMBEServer UDP client backend
- ThumbDV USB dongle backend
- `NeuralEncoder` diagnostic wrapper (exposes raw VQ indices for parity testing)
- Backend factory functions, each gated on a feature flag:
  ```rust
  #[cfg(feature = "dynarmic")]
  pub fn open_dynarmic() -> Box<dyn Vocoder> { ... }

  #[cfg(feature = "neural")]
  pub fn open_neural(model_path: &Path) -> Result<Box<dyn Vocoder>, VocoderError> { ... }
  ```
- Backend discovery:
  ```rust
  pub fn available_backends() -> &'static [Backend] { ... }
  ```

### ambe-dynarmic-rs  (backend)

Current `softambe-rs` absorbed here.  Exposes a type implementing `Vocoder`
using the MD380 firmware JIT via dynarmic.  No binary.

dynarmic is fetched by cmake `FetchContent` at build time (no submodules);
`md380_vocoder_dynarmic` is similarly fetched rather than submoduled.

### ambe-neural-rs  (backend)

Neural encoder (tract ONNX, 9-field VQ, streaming warm-up buffer) +
dynarmic decoder (replaces mbelib; eliminates the mbelib dependency
entirely).  Depends on `ambe-dynarmic-rs` for the decode half.

The ONNX model and weights are loaded from a path at runtime; not embedded.

### ambeserver  (binary)

Replaces `softambeserver`.  Links ambe-rs with whichever backend features
are enabled at compile time.  CLI selects backend; `available_backends()`
drives the help text and validation.

## Trait notes

The existing `Vocoder` trait from `asl-dmr-bridge/ambe/src/lib.rs` is
adopted as-is.  It differs from the current `softambe` API in two ways:

- Takes `&mut self` (backends are stateful).
- `decode` takes `Option<&AmbeFrame>` for erasure support.

The raw 7-byte (no FEC) encode/decode from `softambe-rs` is not exposed
via the trait; FEC is always applied at the wire layer.

## crates.io publishing

Each workspace member is published independently.  `ambe-rs` must not have
required dependencies on backend crates; backends are optional peer crates
pulled in via feature flags (path deps locally, registry deps when published).

`ambe-dynarmic-rs` will not be published to crates.io (proprietary firmware,
legal gray area).  `ambe-neural-rs` depends on it, so it also stays off
crates.io.  `ambe-rs` core and `ambeserver` are publishable.

## Migration path

1. Create `ambe-rs` repo and preserve history (see below).
2. Rename GitHub `softambe-rs` -> `ambe-dynarmic-rs`; update local remote.
3. Add `softambe-rs` tree as `ambe-rs/ambe-dynarmic-rs/` workspace member.
4. Move `softambeserver` binary into `ambe-rs/ambeserver/`.
5. Wire `Vocoder` impl in `ambe-dynarmic-rs` against the trait from `ambe-rs`.
6. Add `ambe-neural-rs`; replace its mbelib decoder with dynarmic.
7. Update `asl-dmr-bridge` to depend on the new `ambe-rs` workspace.

---

## Preserving asl-dmr-bridge/ambe/ git history

`git filter-repo` rewrites a repo keeping only commits that touch a given
path.  Run it on a **fresh clone** (filter-repo refuses to run on a clone
with a configured remote unless you pass `--force`):

```sh
# 1. Fresh clone of asl-dmr-bridge (keeps original untouched).
git clone /path/to/asl-dmr-bridge ambe-history-scratch
cd ambe-history-scratch

# 2. Keep only the ambe/ subtree; move it to the repo root.
git filter-repo --path ambe/ --path-rename ambe/:

# 3. The repo now contains only commits that touched ambe/,
#    with all paths rewritten: ambe/foo -> foo.

# 4. Add it as a remote in the new ambe-rs repo and merge the history.
cd /path/to/ambe-rs
git remote add ambe-history /path/to/ambe-history-scratch
git fetch ambe-history
git merge --allow-unrelated-histories ambe-history/main
git remote remove ambe-history
```

After the merge the ambe-rs core crate files carry the full commit history
from asl-dmr-bridge.

Install `git filter-repo`:

```sh
pip install git-filter-repo
# or: apt install git-filter-repo
```

---

## Open questions

- mbelib: superseded by dynarmic as the neural backend's decoder; retire once
  that switch is made.
