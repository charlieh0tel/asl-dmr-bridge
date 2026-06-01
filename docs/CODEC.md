# Codec choices

The bridge transcodes PCM (USRP/ASL3) <-> AMBE+2 (DMR).  Three
backends; each has different licensing and quality implications.

## ThumbDV / AMBEserver

DVSI AMBE+2 silicon (DV3000), accessed over USB-serial
(`backend = "thumbdv"`) or remotely via an AMBEserver daemon
(`backend = "ambeserver"`).  Same hardware vocoder either way.
The licensed reference; encode and decode quality are the on-air
baseline.  **Recommended.**

## dynarmic (MD380 firmware)

Software vocoder via JIT-emulated AMBE+2 firmware extracted from a
Tytera MD380.  Available when built from source with the `dynarmic`
feature.  Usable standalone (`backend = "dynarmic"`) or as the decode
half of the neural backend (`decoder_backend = "dynarmic"` under
`[vocoder.neural]`).  **Not enabled in the pre-built `.deb`
artifacts.**  No legal advice offered -- operators are responsible
for the posture in their jurisdiction.

## neural

Tract-loaded ONNX encoder (`backend = "neural"`).  Encode and decode
are independently configured under `[vocoder.neural]`:

- `encoder_backend` (default `neural`): ONNX encoder selected by
  `encoder_model_path`.
- `decoder_backend` (default `dynarmic`): decode path; one of
  `neural`, `dynarmic`, `thumbdv`, or `ambeserver`.

When `decoder_backend = "neural"`, a `[vocoder.neural.decoder]`
sub-section is required:

```toml
[vocoder.neural.decoder]
step = "native_gru"    # native_gru (default) or onnx
split_dir = "..."      # directory with decoder_frame.onnx (+ decoder_step.onnx for onnx step)
weights_dir = "..."    # required for native_gru; bundled GRU weight files
```

The default decoder step (`native_gru`) runs the GRU synthesis loop
in native Rust using faer.  This is the recommended path for aarch64
(RPi4) deployments.  The `onnx` step runs the full decoder step
through tract.

**Special-frame bypass**: AMBE+2 b0 values >= 120 (erasure 120-123,
silence 124, tone 125-127) bypass the synthesis network and output
near-silence directly.  This prevents vocoder artifacts during
DMR hang / idle frames.

No legal advice offered regarding the models.
