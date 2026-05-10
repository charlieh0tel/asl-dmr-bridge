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

- **AGC upgrade path; defer until calls demand it.**  Current
  per-direction AGC is peak-tracking with a hard +/-1.0 output
  clamp.  Background and references in `docs/FUTURE_AGC.md`.  If
  `call_agc` summaries show pumping, repeated `gain_max`
  saturation that target / `max_gain_db` tuning cannot fix, or
  voiced_rms stuck well below the listener's expected loudness
  while peaks are already at target, climb in this order:
  (1) drop release toward ~120 ms (one-line tuning, lands solidly
  in syllabic territory);
  (2) add a look-ahead limiter (1-3 ms ring buffer, level
  detector on the undelayed path) before the +/-1.0 clamp so
  transients are caught in time-coincidence rather than after;
  (3) switch the level detector from |x| to RMS / syllabic
  envelope so voiced_rms can be lifted independent of peak
  headroom (the actual fix for quiet-but-peaky talkers);
  (4) dual-decay release, only after (3) is in.
  Skip until step-(0) tuning of `target_dbfs` and `max_gain_db`
  per direction proves insufficient.

