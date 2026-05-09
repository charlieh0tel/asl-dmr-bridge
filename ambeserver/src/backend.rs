//! Backend dispatch for the UDP relay loop.
//!
//! Two impls behind a uniform UDP wire surface:
//!   - [`thumbdv::ThumbDvBackend`]: byte-for-byte serial relay to a
//!     real DVSI AMBE-3000R chip.
//!   - [`soft::SoftBackend`]: in-process software vocoder.

use anyhow::Result;

#[cfg(feature = "thumbdv")]
pub(crate) mod thumbdv;

#[cfg(any(feature = "dynarmic", feature = "neural"))]
pub(crate) mod soft;

pub(crate) trait Backend {
    /// Process one inbound packet.  Returns `Some(bytes)` to send
    /// back to the peer, `None` to drop silently (chip-faithful for
    /// rejected control packets and unsupported control field IDs).
    fn handle(&mut self, request: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Called when a different peer takes over the session.  Soft
    /// impl uses this to clear vocoder per-stream state; chip impl
    /// is a no-op since clients send their own RESET.
    fn on_takeover(&mut self) {}
}
