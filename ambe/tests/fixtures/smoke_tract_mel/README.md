# §0.0 smoke-test bundle: tract × Conv-basis mel

Goal: verify tract can load and run a model containing the mel
front-end, with output numerically matching onnxruntime's. See
`PLAN-encoder-impl.md` §0.0.

History: an earlier revision of this bundle wrapped
`torchaudio.transforms.MelSpectrogram` directly. That exported through
torch's dynamo path on torch>=2.11, but the resulting ONNX `STFT` op
got rank-2 signal input where opset 17 mandates rank 3 -- tract correctly
rejected it (`Failed analyse for node STFT`). Workaround in this bundle:
hand-build the mel via `Conv1d` whose weights are a precomputed Fourier
basis (cos/sin) windowed by Hann, plus a precomputed mel filterbank
matrix, plus elementwise log. Mathematically equivalent to
`MelSpectrogram` to within feature-level FP noise (verified vs eager
torchaudio at < 2e-5 max-abs-diff in log-mel space). The graph contains
no ONNX `STFT` op; only ops tract handles cleanly.

## Model

`SmokeModel` from `scripts/smoke_tract_mel_export.py`. ONNX
opset **17**. Mel parameters match
`nambe/models/features.py` (run-19 config):

| Parameter | Value |
|---|---|
| sample_rate | 8000 |
| n_fft | 256 |
| hop_length | 160 |
| n_mels | 40 |
| f_min | 100.0 |
| f_max | 3800.0 |
| log offset | 1e-08 |

torchaudio defaults in effect: `win_length=n_fft`, `window=hann`,
`power=2.0`, `center=True`, `pad_mode=reflect`, `mel_scale=htk`,
`normalized=False`.

Conv head: `Conv1d(40, 8, kernel=3)`.

ONNX op types in the exported graph:

  Add, Conv, Log, MatMul, Pad, Pow, Unsqueeze

## Files

| File | Shape | Dtype | Layout |
|---|---|---|---|
| `model.onnx` | (see below) | n/a | n/a |
| `input_pcm.bin` | [1, 1280] | f32 | LE |
| `reference_output.bin` | [1, 8, 7] | f32 | LE |

`model.onnx` tensors:

- input `pcm`: [1, 1280] f32
- output `features`: [1, 8, 7] f32

PCM is fixed-seed random ints in [-30000, 30001] divided by 32768.0,
matching the production normalization (`i16 / 32768.0`, see
`nambe/training/dataset.py:94`).

Reference output produced by onnxruntime 1.25.1 on CPU.

## Pass criterion

For each element of the output tensor:

    max_abs_diff(tract_output, reference_output) <= 1e-05

## Reproducing

```
uv run --group deploy python scripts/smoke_tract_mel_export.py
```

Requires **torch >= 2.11** (older versions can't export `MelSpectrogram`
to ONNX cleanly: legacy exporter rejects complex STFT, dynamo path at
torch 2.5 produces a graph onnxruntime can't load). Re-running with
the same SEED (0) is bit-stable.
