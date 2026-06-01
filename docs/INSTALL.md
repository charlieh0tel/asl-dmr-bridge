# Installation notes

Tagged releases publish amd64 + arm64 `.deb` artifacts under
[GitHub releases](https://github.com/charlieh0tel/asl-dmr-bridge/releases).
Local build: `cargo deb -p asl-dmr-bridge`.

## What's package-specific

The bridge unit stays dormant via `ConditionPathExists` until
`/etc/asl-dmr-bridge/config.toml` exists.  Copy
`/usr/share/doc/asl-dmr-bridge/examples/config.example.toml` and
edit; every field is documented inline.

The unit runs under `DynamicUser=yes`, so it can't read root-owned
files in `/etc/`.  Put secrets in `/etc/default/asl-dmr-bridge`
(mode 0600) -- systemd sources it as PID 1 before fork:

```
BRANDMEISTER_PASSWORD=...
BRANDMEISTER_API_KEY=...
```

Three ONNX encoder models ship under `/usr/share/asl-dmr-bridge/models/`:
the default `encoder-aug52-2026-05-15.onnx` (±12 dB gain augmentation),
`encoder-aug50-2026-05-06.onnx` for rollback, and `encoder-dmr50-2026-05-04.onnx`.
Switching is a config-edit + restart.

Two neural decoder weight bundles also ship (for `decoder_backend = "neural"`):
`decoder-h128-v6-weights/` (current default) and `decoder-h128-v5-weights/`
(rollback).  Each directory contains `decoder_frame.onnx`, the GRU weight
`.bin` files, and `meta.json`.

To enable the neural decoder, set `decoder_backend = "neural"` under
`[vocoder.neural]` and add a `[vocoder.neural.decoder]` section:

```toml
[vocoder.neural.decoder]
step = "native_gru"
split_dir = "/usr/share/asl-dmr-bridge/models/decoder-h128-v6-weights"
weights_dir = "/usr/share/asl-dmr-bridge/models/decoder-h128-v6-weights"
```

The `step = "native_gru"` kernel (default) runs the GRU in native Rust and
is the recommended path on aarch64.  Rollback: change both paths to
`decoder-h128-v5-weights`.
