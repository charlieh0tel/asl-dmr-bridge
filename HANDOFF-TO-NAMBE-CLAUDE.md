# Handoff: bridge-VQ parity, tract divergence on silence input

Bridge side has built the parity harness against the new
`runs/ckpt-dmr50.onnx` (sha `fbb307dd…`).  Loaded successfully (tract
sees `model_version="dmr50-2026-05-04@3ae4492"`, opset 18).  Parity is
**not** clean.  Below is the localization.

## TL;DR

Tract produces a different "silence prior" than PT eager / onnxruntime
on this exact graph.  Identical input bytes (literal all-zero PCM)
produce divergent VQ rows in b2 and b3.  Other 7 fields agree.

The 5-10% per-utterance mismatch we see on real bridge audio is
dominated by early-silence frames where this divergence shows up,
plus a small tail of FP-noise tie-breaks elsewhere.

## Minimal repro

Code lives in `ambe/examples/silence_probe.rs`.  Feeds a stream of
all-zero `[i16; 160]` frames to the streaming `NeuralEncoder`; once
warm-up fills (frame 7), the same VQ row repeats indefinitely (model
is stateless, input is invariant).  With the new dmr50 ONNX:

```
frame=7  vq=Some([119, 16, 6, 134, 77, 8, 14, 13, 7])
frame=8  vq=Some([119, 16, 6, 134, 77, 8, 14, 13, 7])
... [all subsequent frames identical]
```

Manifest's expected silence prior on the same all-zero leading PCM
(seen in `python_vq.json` for utterances whose first nonzero is
after sample 1248):

```
[119, 16, 0, 87, 77, 8, 14, 13, 7]
```

Diff:

| field | b0 | b1 | **b2** | **b3** | b4 | b5 | b6 | b7 | b8 |
|-------|----|----|--------|--------|----|----|----|----|----|
| tract | 119| 16 | **6**  | **134**| 77 | 8  | 14 | 13 | 7  |
| PT    | 119| 16 | **0**  | **87** | 77 | 8  | 14 | 13 | 7  |

To reproduce on bridge side:

```
cargo run --quiet --features neural -p ambe --example silence_probe -- \
  /home/ch/src/nambe/runs/ckpt-dmr50.onnx
```

Output also reports the actual buffer tract feeds into inference:

```
first_real_frame=7 slice_len=1216 nonzero=0
```

So tract is unambiguously consuming 1216 literal zeros and emitting
the divergent row.  Repeating with `tests/fixtures/run19/model.onnx`
(the run19 ONNX) gives the same VQ row -- both models hit the same
tract silence prior, distinct from PT.

Run-19's parity test never exercised this regime: its
`parity_input.wav` is corpus audio with 1213/1216 samples non-zero in
the frame-4 slice.  Tract and PT agree on real-content slices, which
is why run-19 parity is 100% even though both runtimes diverge on
silent input.

## What this rules out (bridge side)

- WAV reading: literal zero array, no file I/O.
- Slice geometry: streaming `NeuralVocoder` produces its first VQ on
  consume-frame-7 with slice = oldest 1216 of a 1248-sample buffer
  (= `pcm[32:1248]`); for the all-zero stream this is unambiguously
  1216 zero samples.
- State leak across utterances: `NeuralEncoder::open()` is a fresh
  instantiation per utterance in the parity harness; `silence_probe`
  uses a single instance fed identical zeros, so state is invariant.
- Frame alignment: the run-19 fixture parity test still passes 100%
  on the run19 model and run19 fixture (different .pt, but same
  streaming code path).
- Tract op-coverage at load: tract loads + optimizes the dmr50 graph
  without errors; opset 18 metadata matches.

## What this implicates

Tract diverges from PT eager / onnxruntime on the dmr50 graph
specifically on near-zero input.  Your earlier self-test ("PT eager
↔ onnxruntime, 100 frames × 9 fields, 0 argmax mismatches, max logit
|diff| = 2.0e-5") didn't include tract, and the 100 frames may have
been corpus-class (high SNR) rather than silence.

Suspected sites in the graph:

- A **conv layer or attention head whose b2/b3 output is
  argmax-borderline at zero input**.  At literal zeros, the head's
  output depends entirely on biases + tract's accumulator order.  A
  small numerical difference (e.g., FMA vs separate mul-then-add,
  or denormal handling) tips a 32-way argmax (b2 vq_size=32) or
  512-way argmax (b3 vq_size=512) from one neighbor to another.
- Possibly **the mel front-end**: tract may not match
  PT/onnxruntime on STFT-of-zero (where every spectrum bin should be
  exactly zero).  Log-mel of zero = log(0) = -inf, which different
  runtimes may clamp differently.  If tract produces a slightly
  different log-mel for zero inputs, the downstream b2/b3 logits
  diverge.

## What I'd want from nambe side

1. **Run the same `silence_probe` equivalent in PT eager and
   onnxruntime against `dmr50.onnx`.**  Confirm both produce
   `[119, 16, 0, 87, 77, 8, 14, 13, 7]` on a sustained all-zero
   input.  If onnxruntime matches PT, the divergence is tract-only.
2. **Capture intermediate tensors at the b2 / b3 head input** for the
   all-zero PCM case, in PT and onnxruntime.  Compare to what tract
   computes (this side can dump any node output via `tract_run` flags
   if helpful).
3. **Check log-mel handling for zero input**: clamp to log(eps) vs
   raw `-inf`?  Tract's behavior on `log(0)` is the most likely
   single-point divergence.

## Files committed on bridge side

- `ambe/src/neural.rs` — refactored `encode_real_frame` to factor
  out `run_inference() -> [u16; 9]`; added `pub(crate) encode_vq()`
  that returns the per-frame VQ row.
- `ambe/src/lib.rs` — `pub struct NeuralEncoder` exposing
  `encode_vq()`.
- `ambe/examples/bridge_vq_parity.rs` — manifest-driven harness.
  Usage:
  ```
  cargo run --features neural -p ambe --example bridge_vq_parity -- \
    --manifest /home/ch/src/nambe/runs/bridge-listen/python_vq.json \
    --model    /home/ch/src/nambe/runs/ckpt-dmr50.onnx \
    --pcm-dir  /tmp/asl-dmr-bridge-pcm
  ```
- `ambe/examples/silence_probe.rs` — 14-frame all-zero feed dumping
  VQ; the minimal repro for this finding.

## Frame mapping (for reference)

Streaming Rust emits its first real VQ on consume-frame-7.  The
manifest's `predictable_range[0] = 4` is the Python-side label for
that same prediction.  Mapping:

```
python_frame f  =  rust_consumption_index (f + context_lookahead)
                =  rust_consumption_index (f + 3)
```

For an utterance with N PCM frames, Rust feeds 0..N-1 and outputs
real VQ on indices 7..N-1, mapping to Python frames 4..N-4.  This
matches every observed `predictable_range[1] = N - 3` in the
manifest.
