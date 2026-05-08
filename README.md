# asl-dmr-bridge

Bridge AllStarLink / ASL3 to Brandmeister using Homebrew.

## Documentation

- [DESIGN.md](DESIGN.md) -- architecture and protocol details.
- [docs/CODEC.md](docs/CODEC.md) -- vocoder choices, licensing, and
  quality posture.
- [docs/INSTALL.md](docs/INSTALL.md) -- packaging notes.
- [docs/BRANDMEISTER-API.md](docs/BRANDMEISTER-API.md) -- Halligan
  API integration (`bmcli` + bridge auto-provisioning).
- [docs/USRP-METADATA.md](docs/USRP-METADATA.md) -- USRP TEXT
  call-metadata wire shape.
- [docs/TEST-VECTORS.md](docs/TEST-VECTORS.md) -- encoder test
  coverage.
- [docs/TODO.md](docs/TODO.md) -- tracked deferred work.
- `config.example.toml` -- canonical configuration reference (every
  field documented inline).

Per-module detail lives in module-level rustdoc.

## Building

```
cargo build --release
```

Feature flags:
- `--features thumbdv` -- ThumbDV serial backend (encode + decode)
- `--features dynarmic` -- software AMBE codec via the MD380 firmware
  JIT-emulated by dynarmic (encode + decode).  **Not in pre-built
  debs** -- source builds only.
- `--features neural` -- neural-encoder backend via tract-loaded ONNX
  (encode neural; decode delegates to a configurable backend, default
  thumbdv).

Combinable: `--features thumbdv,neural,dynarmic`.

## Test tools

Examples for testing without an ASL3 instance:

```
# Listen to decoded DMR audio through speakers
cargo run --example usrp_play

# Dump decoded DMR audio to raw PCM (pipe to aplay)
cargo run --example usrp_dump | aplay -f S16_LE -r 8000 -c 1

# Send raw PCM to the bridge as USRP (emulates chan_usrp)
cargo run --example usrp_send < voice.raw

# End-to-end TX test via BM TG 9990 parrot.  Set talkgroup = 9990
# in the bridge config first, then run.  See docs/PARROT-TEST.md.
cargo run --example parrot_test
```

## License

Copyright (C) 2026 Christopher Hoover (AI6KG)

This program is free software; you can redistribute it and/or modify it
under the terms of the GNU General Public License as published by the
Free Software Foundation; either version 2 of the License, or (at your
option) any later version.

See [LICENSE](LICENSE) for the full text.
