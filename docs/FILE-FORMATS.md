# File Formats

Binary and audio formats used by asl-dmr-bridge tools and the diagnostic
recorder.

---

## `.ambe` -- channel-coded AMBE+2 frames

Used by `ambe-tool encode`, `ambe-tool decode`, and `ambe-tool roundtrip`.

- **Frame size:** 9 bytes
- **Layout:** raw AMBE+2 channel-coded frame as transmitted on the DMR wire
  (Golay FEC intact, interleaved)
- **File:** concatenated frames, no header, no framing

Relationship to `.bin`: a `.ambe` frame is the channel-coded form of a `.bin`
frame.  `channel_decode` + `permute_chip_to_mbelib` on a `.ambe` frame gives
the corresponding `.bin` frame.

---

## `.bin` -- 49-bit source bits (mbelib order)

Used by the bridge diagnostic recorder and as input to `ambe_bits2wav`.

- **Frame size:** 7 bytes
- **Layout:** 49 AMBE+2 source bits packed MSB-first into 7 bytes, in mbelib
  `ambe_d[]` bit order.  Bit 0 is byte 0 bit 7; bit 48 is byte 6 bit 7; the
  low 7 bits of byte 6 are zero-padded.
- **File:** concatenated frames, no header; length is always a multiple of 7
- **Timing:** one frame per 20 ms

This is the FEC-stripped, chip-to-mbelib-permuted form.  To recover it from
a `.ambe` file: `channel_decode` then `permute_chip_to_mbelib`.

### Diagnostic recorder files

The bridge writes `.bin` files to the `pcm_record_dir` alongside WAV files
when `[diagnostics] pcm_record_dir` is set in config:

| Filename pattern | Content |
|------------------|---------|
| `fm_to_dmr_encoded_<ts>_<id>.bin` | TX path: source bits out of the encoder (encode input WAV is `fm_to_dmr_encode_in_*.wav`) |
| `dmr_to_fm_decode_in_<ts>_<id>.bin` | RX path: source bits into the decoder (decode output WAV is `dmr_to_fm_decode_out_*.wav`) |

These `(WAV, .bin)` pairs are suitable as oracle-labeled encoder training data:
WAV is the audio fed to the encoder; `.bin` contains the encoder's output bits.

### Using ambe-tool with .bin files

`ambe-tool decode --in-format bin` accepts `.bin` input directly:

```
ambe-tool decode --decoder <backend> --in-format bin --in utt.bin --out decoded.wav
```

`ambe-tool encode --out-format bin` writes `.bin` output:

```
ambe-tool encode --encoder <backend> --out-format bin --in audio.wav --out utt.bin
```

The default for both flags is `ambe`.

---

## WAV -- PCM audio

Convention used throughout the bridge, ambe-tool, and ambe_bits2wav:

- **Sample rate:** 8000 Hz
- **Channels:** 1 (mono)
- **Sample format:** int16 little-endian
- **Header:** 44-byte canonical RIFF/WAVE/fmt/data
- **Samples per frame:** 160 (20 ms)
