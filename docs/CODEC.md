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
Tytera MD380.  Available when built from source with the dynarmic
feature; selected as `[vocoder].neural_decoder = "dynarmic"` under
`backend = "neural"`.  **Not enabled in the pre-built `.deb`
artifacts.**  No legal advice offered -- operators are responsible
for the posture in their jurisdiction.

## neural

Tract-loaded ONNX encoder (`backend = "neural"`).  Encode-only;
decode delegates to a `[vocoder].neural_decoder` (`dynarmic`,
`thumbdv`, or `ambeserver`).  Quality and on-air behavior are
still being characterized; treat as **work in progress**.  No
legal advice offered regarding the model.
