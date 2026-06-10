---
allowed-tools: Bash(cp -r *), Bash(ls *), Bash(git status), Read, Edit
description: Vendor a new neural decoder weights directory into the bridge
---

## Input

Arguments: `$ARGUMENTS` — expected format: `<source-weights-dir> <short-name>`

Example: `/add-decoder ../nambe/runs/decoder-h128-v7-weights h128-v7`

Prompt for any missing argument before proceeding.

- **source-weights-dir**: path to the directory containing `decoder_frame.onnx`,
  `decoder_step.onnx`, `meta.json`, and `*.bin` weight files
- **short-name**: slug used in the directory name, e.g. `h128-v7`

Destination directory: `models/decoder-<short-name>-weights/`
Installed path: `usr/share/asl-dmr-bridge/models/decoder-<short-name>-weights/`

---

## Steps

### Step 1 — copy weights directory

```
cp -r <source-weights-dir> models/decoder-<short-name>-weights
```

Verify with `ls models/decoder-<short-name>-weights/` — expect
`decoder_frame.onnx`, `decoder_step.onnx`, `meta.json`, and one or more `.bin`
files.

### Step 2 — add deb assets

Edit `bridge/Cargo.toml`.  In `[package.metadata.deb]` `assets`, append after
the last `decoder-*` block (four lines per decoder):

```toml
["../models/decoder-<short-name>-weights/decoder_frame.onnx", "usr/share/asl-dmr-bridge/models/decoder-<short-name>-weights/decoder_frame.onnx", "0644"],
["../models/decoder-<short-name>-weights/decoder_step.onnx", "usr/share/asl-dmr-bridge/models/decoder-<short-name>-weights/decoder_step.onnx", "0644"],
["../models/decoder-<short-name>-weights/meta.json", "usr/share/asl-dmr-bridge/models/decoder-<short-name>-weights/meta.json", "0644"],
["../models/decoder-<short-name>-weights/*.bin", "usr/share/asl-dmr-bridge/models/decoder-<short-name>-weights/", "0644"],
```

### Step 3 — update config.example.toml

Find the commented `weights_dir` line under `[vocoder.neural.decoder]` and
replace the path with the new installed path:

```toml
# weights_dir = "/usr/share/asl-dmr-bridge/models/decoder-<short-name>-weights" # required for native_gru
```

### Step 4 — report

Tell the user:
- Destination path in `models/`
- Files copied and total size
- Remind them to commit `models/decoder-<short-name>-weights/`, `bridge/Cargo.toml`, `config.example.toml` together with a version bump before releasing the deb.
