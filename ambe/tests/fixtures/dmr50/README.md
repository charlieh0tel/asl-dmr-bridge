# Production parity bundle: dmr50

Release gate for the asl-dmr-bridge ``ambe::neural`` backend. Producer:
``scripts/onnx_parity.py`` in the nambe repo.

`parity_expected_49bit.bin` is computed via **PT-canonical**: PyTorch
eager ``DeployModel(ConvBasisMel(center=False) + CoderModel)`` running
on the same PCM slices the production harness will see. This is the
deterministic ground truth for the deployment graph.

## Files

- **`parity_input.wav`** -- 8 kHz mono int16 LE WAV. One held-out
  utterance (`2764-36617-0011`).
- **`parity_expected_49bit.bin`** -- per-frame 49-bit AMBE+2 source
  bits in mbelib `ambe_d[]` order, MSB-first packed into 7 bytes per
  frame. Computed via PT-canonical.
- **`README.md`** -- this file.

The ONNX model itself lives at workspace-root `models/encoder-dmr50.onnx`
(the same file the .deb ships under
`/usr/share/asl-dmr-bridge/models/`).  The parity test resolves the
model from there; the WAV + expected bits stay here.

## Shapes

- WAV samples: 121440
- 20 ms vocoder frames in WAV: 759
- Frames in `parity_expected_49bit.bin`: 752
- `parity_expected_49bit.bin` size: 5264 bytes
  (752 frames * 7 bytes)
- Frame-index alignment: bits[0] corresponds to vocoder frame
  `4` of the WAV; bits[N-1] corresponds to vocoder
  frame `755`. The first
  4 frames are unpredictable at deployment because
  the harness lookback (n_fft//2 = 128 samples) requires
  PCM history before frame 0 that doesn't exist at PTT-up.
  Production harness emits zero AmbeFrames for those.

## Pass criterion

Frame-by-frame bit-equality at **100%**.  This bundle was generated
with CPU-PT eager and matches CPU-tract bit-for-bit on the dmr50
graph after the log-mel clamp fix.  If a future bundle drifts, lower
the threshold deliberately and note why instead of accepting silent
regression.

Both sides emit bits in mbelib `ambe_d[]` order. The Rust harness side
uses `ambe::voice_channel::channel_decode` + `permute_chip_to_mbelib`
to recover 49 bits in mbelib order for comparison.
