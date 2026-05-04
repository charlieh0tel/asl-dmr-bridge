# Production parity bundle: dmr50

Release gate for the asl-dmr-bridge ``ambe::neural`` backend. Producer:
``scripts/onnx_parity.py`` in the nambe repo.

`parity_expected_49bit.bin` is computed via **PT-canonical**: PyTorch
eager ``DeployModel(ConvBasisMel(center=False) + CoderModel)`` running
on the same PCM slices the production harness will see. This is the
deterministic ground truth for the deployment graph.

## Files

- **`model.onnx`** -- ONNX opset 18, full PCM-to-logits pipeline.
  See `nambe.*` metadata keys in the file.
- **`parity_input.wav`** -- 8 kHz mono int16 LE WAV. One held-out
  utterance (`2764-36617-0011`).
- **`parity_expected_49bit.bin`** -- per-frame 49-bit AMBE+2 source
  bits in mbelib `ambe_d[]` order, MSB-first packed into 7 bytes per
  frame. Computed via PT-canonical.
- **`README.md`** -- this file.

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

Frame-by-frame byte-equality at **>= 99.5%**:

    matches = sum(tract_bits[f] == expected_bits[f] for f in range(N))
    assert matches / N >= 0.995

**Bit-identity is not achievable at FP32** across distinct execution
paths even when both implement the same mathematical graph. The
ONNX-vs-PT-canonical comparison floor on real speech is ~99.85% (FP-
ordering noise tips argmax on borderline frames where the top-1/top-2
logit gap is below the noise floor). tract reads the same `model.onnx`
that ONNX runtime does, so tract-vs-PT-canonical should sit in the
same range; the 99.5% gate gives margin.

If the rate falls *below* 99.5%, something is structurally wrong:
wrong PCM normalization, wrong slice positioning, byte-order issues,
metadata misread, or a tract op-coverage gap on a non-trivial path
in the graph.

Both sides emit bits in mbelib `ambe_d[]` order. The Rust harness side
uses `dmr_wire::voice_channel::channel_decode` + `permute_chip_to_mbelib`
(or a test-only shortcut that returns the pre-channel-coded bits) to
recover 49 bits in mbelib order for comparison.
