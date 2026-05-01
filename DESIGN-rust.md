# asl-dmr-bridge -- Rust Implementation

See [DESIGN.md](DESIGN.md) for architecture, protocol specifications,
component behavior, and configuration schema.  Per-module detail
lives in module-level rustdoc.  This file records only the Rust-
specific design decisions whose *why* doesn't fit naturally inside
any single module.

## Vocoder offload

The `Vocoder` trait is synchronous (blocking I/O on serial / socket /
FFI).  `PttMachine::decode` / `encode` offload via
`tokio::task::spawn_blocking` and race the result against the
cancellation token.  The vocoder is held in `Arc<Mutex<Box<dyn
Vocoder>>>` to make it movable into the blocking closure; the Mutex
is never actually contended (single-task serialization) but guards
against the detached-`spawn_blocking` race if cancel fires mid-call.
A poisoned mutex (from a panic inside decode/encode, e.g. mbelib
FFI) surfaces as `VocoderError` rather than being swallowed.

## DMRD egress: try_reserve_many for voice bursts

The DMR voice burst RX path uses `mpsc::Sender::try_reserve_many` to
atomically reserve all three USRP-out slots up front.  A partial
burst would produce an audible 20 ms gap mid-60 ms, so we drop the
whole burst with one warning when the channel can't take all three.

## Tokio feature pin

`tokio` is feature-pinned (`fs`, `macros`, `net`,
`rt-multi-thread`, `signal`, `sync`, `time`).  `["full"]` pulls in
`process`, `parking_lot`, and `io-util`, none of which we use.

## Deliberately not depended on

Recorded so they don't get re-litigated:

- `hmac`: BM uses plain SHA256, not HMAC.
- `uuid`: stream IDs are u32, no UUIDs on the wire.
- `bytes`: `Vec<u8>` and `&[u8]` suffice.

`secrecy` *is* included.  It wraps the BM password in `SecretString`,
auto-redacts in `Debug`, and zeroizes on drop.  Only the SHA256 hash
goes on the wire; the plaintext never escapes the auth path.

## Wire-crate isolation

`dmr-wire`, `dmr-events`, `dmr-types`, and `usrp-wire` take no
dependency on `bridge::config`, `bridge::types`, or any binary-only
module.  Enforced by their being separate workspace crates rather
than `pub(crate)` modules in the binary.
