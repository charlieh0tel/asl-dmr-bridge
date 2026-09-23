---
allowed-tools: Bash(git status), Bash(git add *), Bash(git commit *), Bash(git tag *), Bash(cargo check *), Bash(cargo build *), Bash(cargo deb *)
description: Bump version, commit, tag, and build the .deb
---

## Context

- Current git status: !`git status`
- Current workspace version: !`grep '^version' Cargo.toml | head -1`
- Most recent tag: !`git tag --list | sort -V | tail -1`

## Your task

Release asl-dmr-bridge at the version supplied as `$ARGUMENTS` (prompt if not given).

The human-readable process is `RELEASING.md`; keep the two in sync.

### Step 0 — verify clean tree

Run `git status`. If there are any staged changes, unstaged modifications, or untracked files (other than known-ignored files), stop and tell the user what is outstanding. Do not proceed until the tree is clean.

### Step 1 — bump version

Edit `Cargo.toml` (workspace root): change `version = "OLD"` to `version = "NEW"` under `[workspace.package]`.

### Step 2 — update lockfile

```
cargo check -p asl-dmr-bridge
```

### Step 3 — commit

Stage only `Cargo.toml` and `Cargo.lock`, then commit:

```
git commit -m "release: NEW"
```

### Step 4 — tag

```
git tag vNEW
```

### Step 5 — build the .deb

Build the whole workspace first: the deb ships the ambeserver,
ambe-tool and usrp-tool binaries too, which `cargo deb -p` alone
would not rebuild.

```
cargo build --release --workspace --features dynarmic,neural
cargo deb -p asl-dmr-bridge --no-build
```

### Step 6 — report

Tell the user:
- New version and tag
- Path and size of the produced `.deb`
- Reminder: `git push && git push --tags`
