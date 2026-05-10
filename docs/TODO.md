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

- **AGC per-call stats.**  Wire min/max/mean gain, gate-frozen
  sample count, and peak in/out into the existing per-call summary
  log path (`dmr-wire/src/voice/diagnostics.rs`).  Lets us evaluate
  AGC behavior on real BM sessions instead of guessing.

- **Bump `agc.max_gain_db` default 12 -> 15-18.**  Quiet talkers
  (-25 dBFS, common on BM) currently cap at -13 dBFS instead of
  reaching the -6 dBFS target.  Right value is empirical; defer
  until per-call AGC stats land so we can pick a number from a
  real listening pass instead of guessing.

- **Pre-encode LP 3000 -> 3400 Hz A/B.**  AMBE+2 at DMR rate is
  documented for 250-3400 Hz; current LP at 3000 Hz is the
  conservative P25/DMR pre-emphasis target and loses sibilance
  above 3 kHz.  Stretch only if a listener reports dull FM-side
  audio; the codec at half-rate may not preserve the extra 400 Hz
  anyway.

