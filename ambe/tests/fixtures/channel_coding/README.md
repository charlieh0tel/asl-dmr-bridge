# AMBE+2 channel-coding goldens

12 utterances captured through a DVSI AMBE-3000R chip in two passes
each, producing matched `(raw_49, coded_72)` pairs the bridge needs to
verify a chip-equivalent 49->72 channel encoder.

## Files (per utterance `uttNNN`)

- `uttNNN.pcm.gz` -- gzipped i16 LE 8 kHz PCM input, framed at 160
  samples (20 ms) per frame.
- `uttNNN.coded72` -- chip output at rate index 33 (DMR / P25 half-rate,
  2450 voice + 1150 FEC), 9 bytes per frame, concatenated.
- `uttNNN.raw49` -- chip output at rate index 34 (raw 2450 voice, 0
  FEC), 7 bytes per frame, concatenated; the 49 bits live in the high
  bits of those 7 bytes (chip CHAND format, MSB first).

Frame `i` in `coded72` and `raw49` corresponds to the same 20 ms of
audio, so `(raw49[i], coded72[i])` is one chip-emitted golden pair.

## Source

Twelve utterances from the LibriSpeech `train-clean-100` corpus, three
speakers.

## Regenerate

Stop any process holding the chip (e.g. `pkill ambeserver`), then for
each input PCM:

```
cargo run -p ambe --features thumbdv --example dv3000_capture -- \
    /dev/ttyUSB0 input.pcm output_prefix
```

Outputs `output_prefix.{pcm,coded72,raw49}`.  Compress the `.pcm` with
`gzip -9` before committing.  See `ambe/examples/dv3000_capture.rs` for
the chip protocol details.
