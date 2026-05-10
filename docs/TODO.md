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

