# Releasing

1. Clean `main`; fmt, clippy, tests pass.
2. Bump `version` in root `Cargo.toml` `[workspace.package]`.
3. `cargo check -p asl-dmr-bridge` to update `Cargo.lock`.
4. `git commit -m "release: NEW" Cargo.toml Cargo.lock && git tag vNEW`
5. Optional local build:
   `cargo build --release --workspace --features dynarmic,neural &&
   cargo deb -p asl-dmr-bridge --no-build`
6. `git push && git push --tags` -- the tag triggers CI to build the
   .deb and rebuild the APT repo.

Claude: `/release NEW` automates steps 2-5.
