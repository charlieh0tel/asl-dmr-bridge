# TODO

Tracked-but-deferred work.  Each entry is sized roughly: a single
line of context + the rough effort + the trigger that would make it
worth picking up.

## Maybe

Plausible but value-uncertain; defer until a real-world need
appears.

- **Bandpass filter on the USRP-rx (analog -> digital) input.**
  DVSwitch's `tlvAudio = AUDIO_BPF` filters PCM before AMBE encode
  to remove DC and out-of-band content.  ~80 LOC + freq-response
  tests.  Trigger: noisy or DC-offset analog source degrading
  encoded audio quality on the DMR side.

- **DTMF macros / runtime control.**  DVSwitch's `[MACROS]` lets a
  remote operator dial `*5678` to switch TGs, disconnect, etc.
  Would need DTMF detection on USRP-rx, a macro table, and an
  integration with the BM API for runtime TG changes.  Big feature,
  rough estimate ~500 LOC + careful testing.  Trigger: a real user
  asking for radio-side control of the bridge.

- **Talker alias TA Blocks (FLCO 5/6/7).**  Current scope is TA
  Header only (FLCO 4); covers callsigns up to 7 ASCII chars.
  Adding TA Blocks lets us emit longer aliases ("N0CALL Operator")
  but takes ~4 superframes to deliver, so calls under ~1.4s see
  only partial TA.  ~100 LOC.  Trigger: a callsign over 7 chars or
  a desire to send name as well as call.

## Intentionally not doing

These have been considered and rejected for stated reasons -- listed
so they don't get re-proposed.

- **AGC on USRP-rx (analog -> digital).**  ASL3's chan_usrp
  pre-applies operator-tuned gain; AGC there would compete with the
  static `vocoder.gain_in_db` knob and offers little benefit for
  our typical single-repeater deployment.

- **OpenBridge / cross-network bridging.**  The bridge is single-
  repeater BM peer use, not network-to-network.  See the
  Brandmeister policy section in README.

- **DVSwitch WebProxy / `pcmPort`.**  Niche web UI bridge; no
  user demand and we've added no equivalent web surface to consume
  it.
