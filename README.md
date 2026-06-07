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
- [docs/FILE-FORMATS.md](docs/FILE-FORMATS.md) -- `.ambe`, `.bin`,
  and WAV format reference.
- [docs/TOOLS.md](docs/TOOLS.md) -- USRP test examples, parrot TX
  test, vocoder fixture tools.
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
- `--features neural` -- neural encoder + decoder backend via tract-loaded
  ONNX.  Encode and decode independently configurable via
  `[vocoder.neural]`; default decode is `dynarmic`.

Combinable: `--features thumbdv,neural,dynarmic`.

**Codec licensing**: the `dynarmic` backend involves firmware extracted
from commercial hardware; legal implications vary by jurisdiction.
See [docs/CODEC.md](docs/CODEC.md) before deploying it.

## Tools

See [docs/TOOLS.md](docs/TOOLS.md).

## License

Copyright (C) 2026 Christopher Hoover (AI6KG)

This program is free software; you can redistribute it and/or modify it
under the terms of the GNU General Public License as published by the
Free Software Foundation; either version 2 of the License, or (at your
option) any later version.

See [LICENSE](LICENSE) for the full text.
