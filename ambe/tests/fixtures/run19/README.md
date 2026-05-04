# run-19 parity fixture

Inputs for `tests/neural_parity.rs`.  Frame-by-frame bit-equality
gate on the production neural-encoder graph.

## Files

- `model.onnx` -- ONNX opset 18, full PCM-to-logits pipeline
  (mel front-end + encoder).  Metadata under `nambe.*` keys.
- `parity_input.wav` -- 8 kHz mono i16 LE WAV; one held-out
  utterance.
- `parity_expected_49bit.bin` -- per-frame 49 source bits in
  mbelib `ambe_d[]` order, MSB-first packed into 7 bytes per frame.
  Computed via PyTorch eager reference.

## Pass criterion

100% match.  Documented in `tests/neural_parity.rs::PASS_THRESHOLD`.

## Frame alignment

The harness needs `harness_lookback_samples` (read from ONNX metadata,
typically 128) of PCM history before its first prediction.  At PTT-up
that history doesn't exist, so the first few vocoder frames are
unpredictable -- production emits zero AmbeFrames and this fixture
trims them.  `parity_expected_49bit.bin` aligns with frame 4 of the
WAV; total frames = `(WAV samples / 160) - 4`.
