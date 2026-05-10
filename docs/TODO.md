# TODO

Tracked-but-deferred work.  Each entry is sized roughly: a single
line of context + the rough effort + the trigger that would make it
worth picking up.

## Maybe

Plausible but value-uncertain; defer until a real-world need
appears.

- **Investigate the v1.8.0 SIGSEGV.**  Crash hit on the first RX
  header on arm64 with `backend = "neural"` (decode delegated to
  dynarmic at the time).  Print line was dynarmic's signal handler
  `Unhandled SIGSEGV at pc 0x...`, suggesting a fault outside JIT
  memory regions.  Coredump was not captured at the time.  Trigger:
  any dynarmic-on-arm64 crash.  Capture core via systemd-coredump
  and follow the backtrace.

- **Pre-encode LP 3000 -> 3400 Hz A/B.**  AMBE+2 at DMR rate is
  documented for 250-3400 Hz; current LP at 3000 Hz is the
  conservative P25/DMR pre-emphasis target and loses sibilance
  above 3 kHz.  Stretch only if a listener reports dull FM-side
  audio; the codec at half-rate may not preserve the extra 400 Hz
  anyway.

- **AGC upgrade path; only if anyone wants FM->DMR AGC again.**
  AGC on FM->DMR has been ruled out by listener tests
  (`docs/AGC_LOOKAHEAD_LIMITER.md`, Field result section): the
  peak-tracking gain follower + limiter combination pumps at
  speech-syllable rate in a way the AMBE+2 vocoder encodes
  audibly badly, even though the in-bridge metrics
  (`clipped = 0`, `peak_out` at ceiling, `voiced_rms` at target)
  look clean.  Static `fm_to_dmr_db` pad before the vocoder is
  the working answer.  AGC on DMR->FM remains useful because the
  listener is downstream of the codec.
  If FM->DMR AGC is ever wanted again, the only path that can
  work is switching the level detector from peak (|x|) to
  RMS / syllabic envelope so the gain follower stops chasing
  inter-syllable pauses.  That's a meaningful DSP change
  (~100 LOC + careful tuning), not a small tweak.  Optional
  follow-on: dual-decay release.

