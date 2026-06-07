# Vocoder test fixtures

See [docs/TOOLS.md](../../../../docs/TOOLS.md) for fixture tool usage
(golden file regeneration, real capture fetch).

## Files

- `mbelib_golden.bin` / `mbelib_golden.meta.toml` -- szechyjs/mbelib reference output
- `thumbdv_golden.bin` / `thumbdv_golden.meta.toml` -- DVSI AMBE-3000 output
- `ambeserver_golden.bin` / `ambeserver_golden.meta.toml` -- AMBEserver daemon output
- `amb/` (gitignored) -- real captured DMR voice frames fetched by `fetch_amb_samples`

Each `.bin` is 2560 bytes (8 frames x 160 samples x 2 bytes LE i16).
