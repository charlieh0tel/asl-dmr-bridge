# TODO

Tracked-but-deferred work.  Each entry is sized roughly: a single
line of context + the rough effort + the trigger that would make it
worth picking up.

## Maybe

Plausible but value-uncertain; defer until a real-world need
appears.

- **Software-vocoder mode for `ambeserver`.**  Currently a chip-only
  UDP-to-serial proxy; the prior `softambeserver` capability got
  retired when softambe-rs folded into `ambe::dynarmic`.  Bring it
  back as an `ambeserver` mode (e.g. `--backend dynarmic`), speaking
  the same DV3000 UDP protocol so existing clients don't know the
  difference.  Gain handling is uniform via `Vocoder::set_gain`, so
  callers see the same semantics regardless of backend.

- **Investigate the v1.8.0 SIGSEGV.**  Crash hit on the first RX
  header on arm64 with `backend = "neural"` (decode delegated to
  dynarmic at the time).  Print line was dynarmic's signal handler
  `Unhandled SIGSEGV at pc 0x...`, suggesting a fault outside JIT
  memory regions.  Coredump was not captured at the time.  Trigger:
  reproduces on the v2.0.0 deb, or any other dynarmic-on-arm64
  crash.  Capture core via systemd-coredump and follow the
  backtrace.

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
  static `[gain].fm_to_dmr_db` knob and offers little benefit for
  our typical single-repeater deployment.

- **OpenBridge / cross-network bridging.**  The bridge is single-
  repeater BM peer use, not network-to-network.  See the
  Brandmeister policy section in README.

- **DVSwitch WebProxy / `pcmPort`.**  Niche web UI bridge; no
  user demand and we've added no equivalent web surface to consume
  it.
