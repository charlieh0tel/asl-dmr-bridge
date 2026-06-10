---
allowed-tools: Bash(cp *), Bash(ls *), Bash(git status), Read, Edit
description: Vendor a new neural encoder ONNX into the bridge
---

## Input

Arguments: `$ARGUMENTS` — expected format: `<source-onnx-path> <short-name> <date>`

Example: `/add-encoder ../nambe/runs/ckpt-aug-55-nacf.onnx aug55-nacf 2026-06-09`

Prompt for any missing argument before proceeding.

- **source**: path to the `.onnx` file (absolute or relative to workspace root)
- **short-name**: slug used in the filename, e.g. `aug55-nacf`
- **date**: ISO date for the filename, e.g. `2026-06-09`

Destination filename: `models/encoder-<short-name>-<date>.onnx`
Installed path: `usr/share/asl-dmr-bridge/models/encoder-<short-name>-<date>.onnx`

---

## Steps

### Step 1 — copy ONNX

```
cp <source> models/encoder-<short-name>-<date>.onnx
```

Verify with `ls -lh models/encoder-<short-name>-<date>.onnx`.

### Step 2 — add deb asset

Edit `bridge/Cargo.toml`.  In `[package.metadata.deb]` `assets`, append after
the last `encoder-*` line:

```toml
["../models/encoder-<short-name>-<date>.onnx", "usr/share/asl-dmr-bridge/models/encoder-<short-name>-<date>.onnx", "0644"],
```

### Step 3 — update config.example.toml

Find the commented `encoder_model_path` line under `[vocoder.neural]` and
replace the path with the new installed path:

```toml
# encoder_model_path = "/usr/share/asl-dmr-bridge/models/encoder-<short-name>-<date>.onnx"
```

### Step 4 — report

Tell the user:
- Destination path in `models/`
- File size
- Remind them to commit `models/encoder-<short-name>-<date>.onnx`, `bridge/Cargo.toml`, `config.example.toml` together with a version bump before releasing the deb.
