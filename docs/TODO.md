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

- **Talker alias TA Blocks (FLCO 5/6/7).**  Current scope is TA
  Header only (FLCO 4); covers callsigns up to 7 ASCII chars.
  Adding TA Blocks lets us emit longer aliases ("N0CALL Operator")
  but takes ~4 superframes to deliver, so calls under ~1.4s see
  only partial TA.  ~100 LOC.  Trigger: a callsign over 7 chars or
  a desire to send name as well as call.
