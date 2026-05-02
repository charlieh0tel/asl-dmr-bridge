# asl-dmr-bridge

Bridge AllStarLink / ASL3 to Brandmeister using Homebrew.

## Design

- [DESIGN.md](DESIGN.md) -- architecture and protocol details.
- [docs/TEST-VECTORS.md](docs/TEST-VECTORS.md) -- encoder test
  coverage.
- [docs/BRANDMEISTER-API.md](docs/BRANDMEISTER-API.md) -- Halligan
  API integration (`bmcli` + bridge auto-provisioning).
- [docs/USRP-METADATA.md](docs/USRP-METADATA.md) -- USRP TEXT
  call-metadata wire shape.
- [docs/TODO.md](docs/TODO.md) -- tracked deferred work.

Per-module detail lives in module-level rustdoc.

## Building

```
cargo build --release
```

Feature flags:
- `--features mbelib` -- software AMBE decode via mbelib (decode only)
- `--features thumbdv` -- ThumbDV serial backend (encode + decode)

Both can be combined: `--features mbelib,thumbdv`.

## Usage

```
RUST_LOG=info asl-dmr-bridge config.toml
```

The BM hotspot password takes one of: `BRANDMEISTER_PASSWORD` env,
`[network] password = "..."` inline, `[network] password_file =
"<path>"`, or `--password-file <path>`.  Setting more than one is
a startup error.  The API key uses the parallel set:
`BRANDMEISTER_API_KEY`, `api_key` / `api_key_file` under
`[brandmeister_api]`, or `--api-key-file`.

Optional Brandmeister Halligan API integration: with a
`[brandmeister_api]` section in the config (or an API key in
`BRANDMEISTER_API_KEY`), the bridge logs the peer's BM-side
subscription state at startup and -- when desired static talkgroup
lists are supplied -- reconciles them on each run.  `bmcli` is a
standalone CLI over the same API.  See
[docs/BRANDMEISTER-API.md](docs/BRANDMEISTER-API.md) for the full
guide and `config.example.toml` for the config schema.

The bridge emits a per-call summary INFO line at every call's end
(direction, frame count, drops, transcode p50/p99, termination
reason) and a periodic cumulative-counter heartbeat.  See `[stats]`
in `config.example.toml` to tune the heartbeat interval, idle-skip,
and the per-call duration floor.

See `config.example.toml` for the configuration schema.

Key config fields:
```toml
[dmr]
gateway = "both"      # "both", "dmr_to_fm", or "fm_to_dmr"
slot = 1              # DMR timeslot (1 or 2)
talkgroup = 91        # talkgroup to bridge
call_type = "group"   # "group" or "private"

[vocoder]
backend = "mbelib"    # "mbelib", "thumbdv", or "ambeserver"
```

## Packaging

Tagged releases are built and published as `.deb` artifacts by the
`Build Debian Package` GitHub Actions workflow (amd64 + arm64,
glibc-bookworm compatible).  Push a `v*` tag to trigger a release.

To build a `.deb` locally:
```
cargo install cargo-deb
cargo deb -p asl-dmr-bridge
```

The packaged unit stays dormant via `ConditionPathExists` until
`/etc/asl-dmr-bridge/config.toml` exists.  Template lives at
`/usr/share/doc/asl-dmr-bridge/examples/config.example.toml`.

Secrets go in `/etc/default/asl-dmr-bridge` (mode 600) as
`BRANDMEISTER_PASSWORD=...` / `BRANDMEISTER_API_KEY=...`.  The unit's
`DynamicUser=yes` precludes reading root-owned files in `/etc/`
directly; the env-var path works because systemd sources
`/etc/default/` as PID 1 before fork.

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

## Pre-commit guard

`scripts/githooks/pre-commit` is a small shell script that scans
staged additions for personal DMR IDs, bare credentials, and JWT-
shape tokens.  Wire it up once per clone:

```
git config core.hooksPath scripts/githooks
```

Override on a known-safe hit (e.g. a documented public test JWT)
with `git commit --no-verify`.

## License

Copyright (C) 2026 Christopher Hoover (AI6KG)

This program is free software; you can redistribute it and/or modify it
under the terms of the GNU General Public License as published by the
Free Software Foundation; either version 2 of the License, or (at your
option) any later version.

See [LICENSE](LICENSE) for the full text.
