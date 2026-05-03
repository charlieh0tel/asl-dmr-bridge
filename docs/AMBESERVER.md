# ambeserver

UDP-to-serial proxy for the DVSI AMBE-3000R chip.  Wire-compatible
with OpenDV-protocol clients.

```
cargo build --release -p ambeserver
ambeserver --serial /dev/ttyUSB0 [--baud 460800] [--listen 0.0.0.0:2460]
```

One peer drives the chip at a time; others are refused (clean UDP
timeout) until the holder goes idle for ~1 s.  Clients are
responsible for resetting the chip if they need fresh codec state.
