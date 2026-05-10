//! Low-level chip-control client API for the DVSI AMBE-3000R chip
//! (ThumbDV).
//!
//! `Vocoder` covers the routine encode/decode-at-DMR-default-rate
//! case.  This trait exposes the primitives the chip itself supports
//! that don't fit in `Vocoder`: switching `RATEP` and `GAIN` mid-
//! session, encoding at non-72-bit rates, and decoding the matching
//! variable-bit-count input.
//!
//! Two implementations live with their respective transports:
//!
//! - [`crate::ambeserver::AmbeServerClient`] (always available): UDP
//!   to a chip behind an ambeserver.
//! - [`crate::thumbdv::ThumbDvClient`] (`thumbdv` feature): direct
//!   serial.

use crate::VocoderError;
use dmr_types::PcmFrame;
use dv3000_wire::HEADER_SIZE;
use dv3000_wire::START_BYTE;
use dv3000_wire::TYPE_AMBE;

/// Low-level access to a DVSI AMBE-3000R chip.  Use this when you
/// need to switch rates mid-session or inspect non-72-bit AMBE
/// responses.  For routine DMR encode/decode, `Vocoder` is simpler.
pub trait ChipClient: Send {
    /// Reset the chip to default state and wait for the READY ack.
    /// Wipes codec state -- use at the start of a stream that needs
    /// bit-exact output independent of prior chip activity.
    fn reset(&mut self) -> Result<(), VocoderError>;

    /// Send a custom 12-byte RATEP control word (RCW0..RCW5).
    fn set_ratep(&mut self, rcws: &[u8; 12]) -> Result<(), VocoderError>;

    /// Set encoder input + decoder output gain in dB; clamped to
    /// the chip's supported range.
    fn set_gain(&mut self, in_db: i8, out_db: i8) -> Result<(), VocoderError>;

    /// Encode 160 PCM samples at the chip's currently-configured
    /// rate.  Returns `(bit_count, packed_bytes)`; the byte count is
    /// `ceil(bit_count / 8)`.
    fn encode_raw(&mut self, pcm: &PcmFrame) -> Result<(u8, Vec<u8>), VocoderError>;

    /// Decode AMBE bits at the chip's currently-configured rate.
    /// `bits` and `data` must match what `encode_raw` would have
    /// produced for the same rate; mismatches surface as chip
    /// protocol errors.
    fn decode_raw(&mut self, bits: u8, data: &[u8]) -> Result<PcmFrame, VocoderError>;
}

/// Build a `PKT_AMBE` packet for decode-direction with arbitrary bit
/// count: header(4) + field_id(1) + num_bits(1) + data(ceil(bits/8)).
/// Shared helper for both `ChipClient` impls.
pub(crate) fn build_ambe_for_bits(bits: u8, data: &[u8]) -> Vec<u8> {
    const FIELD_CHANNEL_DATA: u8 = 0x01;
    let payload_len = 1 + 1 + data.len();
    let mut buf = Vec::with_capacity(HEADER_SIZE + payload_len);
    buf.push(START_BYTE);
    buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
    buf.push(TYPE_AMBE);
    buf.push(FIELD_CHANNEL_DATA);
    buf.push(bits);
    buf.extend_from_slice(data);
    buf
}
