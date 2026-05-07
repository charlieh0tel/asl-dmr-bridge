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

Two ONNX models ship under `/usr/share/asl-dmr-bridge/models/`:
the default `aug50-2026-05-06.onnx` and the prior
`dmr50-2026-05-04.onnx` for rollback.  Switching is a config-edit
+ restart.
