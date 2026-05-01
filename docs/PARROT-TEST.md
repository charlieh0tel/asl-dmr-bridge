# BM Parrot end-to-end TX test

End-to-end verification of the FM->DMR encode path via the Brandmeister
parrot/echo service (subscriber 9990, **private call**).  Parrot
records anything transmitted to it and plays it back to the same peer
~1-2 seconds later, so a single bridge running with `gateway = "both"`
becomes its own listener -- no second radio or extra account needed.

Parrot on Brandmeister is invoked as a **private** (unit-to-unit) call
to subscriber 9990, not a group call to TG 9990.  The bridge config
must set `call_type = "private"`; group-call DMRD to 9990 is silently
dropped network-side.

## What the test verifies

The full TX chain end-to-end against a real BM relay:

```
  test signal (1 kHz tone, or PCM file)
    -> USRP frames -> [bridge USRP rx]
    -> [bridge AMBE encode (chip)]
    -> DMR voice burst (assemble_burst, BPTC voice LC, embedded LC)
    -> DMRD packet -> [bridge -> BM master]
    -> BM parrot (subscriber 9990, private call)
    -> DMRD reply (private, src=9990 dst=our src_id) -> [bridge DMRD rx]
    -> [bridge AMBE decode (chip)]
    -> USRP frames -> captured by test
```

Anything wrong in the encode chain (RATEP misconfig, bit ordering,
RS / BPTC / Hamming / Golay / QR encoders, embedded LC, slot type,
sync) shows up as garbled or empty playback.

## Setup

1. Edit your bridge config so DMR addressing targets the parrot:
   ```toml
   [dmr]
   gateway = "both"
   talkgroup = 9990         # parrot subscriber ID (used as DMRD dst_id)
   call_type = "private"    # required: parrot is unit-to-unit
   ```

2. Pick whichever vocoder backend you're testing (`thumbdv`,
   `ambeserver`, or `mbelib` -- though `mbelib` cannot encode, so
   the parrot test is meaningful only for hardware backends).

3. Start the bridge in one terminal:
   ```
   cargo run --release -p asl-dmr-bridge --features mbelib,thumbdv -- config.toml
   ```
   Wait for `authenticated with master` in the bridge log.

## Run

```
cargo run --example parrot_test
```

Optional positional args:
1. bridge USRP listen addr (default `127.0.0.1:34001`)
2. bridge USRP send target  (default `127.0.0.1:34002`)
3. tone duration in seconds (default `3`)

To send a recorded voice clip instead of the synthetic tone, set
`PARROT_TEST_INPUT` to an S16_LE 8 kHz mono raw PCM file.  A canonical
"check one two three" sample lives in the repo:

```
PARROT_TEST_INPUT=data/check123-s16le-8000-c1.raw \
  cargo run --example parrot_test
```

When `PARROT_TEST_INPUT` is set, the duration argument is ignored and
the file's whole length is transmitted.  Voice samples survive AMBE+2
much better than pure tones, so a recorded clip is a stronger
end-to-end audio check than the 1 kHz tone default.

## Outputs

- `/tmp/parrot_in.raw`  -- the input we sent (S16_LE, 8 kHz mono)
- `/tmp/parrot_out.raw` -- the parrot's playback as captured

Listen with:
```
aplay -f S16_LE -r 8000 -c 1 /tmp/parrot_out.raw
```

The test reports input/output RMS + peak.  Pass: output is voice-
shaped (RMS in the hundreds-to-low-thousands, max above ~2000).
Fail: empty capture, or output near-zero.

## Failure interpretations

- **Empty capture**: the bridge isn't authenticated to BM, the bridge
  isn't routing parrot's reply to USRP-out (check `gateway`,
  `talkgroup`, and `call_type = "private"`), or BM dropped the call
  before it reached parrot.  Check the bridge log for
  `RX header stream_id=...` lines after the test transmits; if
  absent, BM didn't echo.  If `DMRD rx` events appear at debug level
  but `RX header` does not, the bridge filter is rejecting the reply
  (e.g. wrong call_type / dst_id mismatch).
- **Voice-shaped but garbled**: encode bit-layout regression -- the
  hardware loopback test (`encode_loopback`) catches the same
  defect locally, run that first.
- **Voice-shaped at very low amplitude**: chip output gain is set
  low; tune `vocoder.gain_out_db` in the config (try `+6`).
- **Drop-outs in the middle**: backpressure or chip stall on the
  serial path.  Check bridge log for `USRP tx channel full,
  dropping voice burst` warnings.

## Avoid TG 91 for tests

TG 91 is the world-wide chat -- transmitting test tones to it
disrupts real users.  Always use parrot (subscriber 9990, private
call) for tone tests.
