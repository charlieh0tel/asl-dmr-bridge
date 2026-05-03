//! DMR Layer 2 handling: DMRD wire packets, voice burst
//! disassembly + assembly, FEC encoders, and the PTT-driven voice
//! task for both RX and TX directions.
//!
//! See DESIGN.md "Voice Frame -- DMRD", "DMR Voice Burst
//! Disassembly", and "DMR Frame Assembly".  Primary reference for
//! the ETSI wire format is G4KLX DMRGateway: `DMRData.cpp`,
//! `DMRSlotType.cpp`, `DMREMB.cpp`, `DMREmbeddedData.cpp`,
//! `DMRFullLC.cpp`.  Normative spec is ETSI TS 102 361-1 (v2.5.1
//! is the version cross-referenced by `docs/TEST-VECTORS.md`),
//! download from <https://www.etsi.org/deliver/etsi_ts/102300_102399/10236101/>.
//!
//! # FEC policy
//!
//! Encode-only, no error correction on the RX path:
//!
//! * AMBE voice codewords (RX): the 49 source bits are extracted at
//!   known systematic positions from each 72-bit codeword; vocoders
//!   (mbelib `processAmbe2450Data`, ThumbDV, AMBEserver) consume 49
//!   bits, not the 72-bit AMBE+FEC form.  Golay(24,12) / Golay(23,12)
//!   error correction is the vocoder's job, not ours.
//! * Slot type, EMB, voice LC (RX): not decoded.  The DMRD flag byte
//!   and header already carry slot, frame type, voice-sequence A-F,
//!   src and dst IDs -- so parsing the on-wire FEC fields is not
//!   needed to forward a voice frame received over UDP.
//! * Slot type, EMB, voice LC (TX): fully encoded.  See `bptc.rs`
//!   (BPTC(196,96) for voice LC header/terminator), `embedded_lc.rs`
//!   (BPTC(128,77) for embedded LC on voice bursts B-E), `fec.rs`
//!   (Golay(20,8,7) for slot type, Hamming(13,9,3) / (15,11,3) /
//!   (16,11,4), QR(16,7,6) for EMB), `rs.rs` (RS(12,9) for Full LC
//!   checksum), and `sync.rs` (SYNC patterns, EMB section builder).
//!   Encoders are byte-exact against 11 live Brandmeister captures
//!   (see `docs/TEST-VECTORS.md`).
//!
//! Decoders for every encoder live under `#[cfg(test)]` so we can
//! verify encoder correctness across the entire input space, not
//! just at single golden-vector points: BPTC(196,96) and BPTC(128,77)
//! decoders with single-bit Hamming correction; lookup-inverse
//! decoders for Golay(20,8,7) and QR(16,7,6); and an RS(12,9)
//! syndrome calculator.  None of these are used at runtime --
//! promote to `pub(crate)` when an RF reception path appears.
//! See `docs/TEST-VECTORS.md` for the full coverage matrix.

pub mod audio;
pub(crate) mod bptc;
pub mod dmrd;
pub(crate) mod embedded_lc;
pub(crate) mod fec;
pub(crate) mod frame;
pub(crate) mod rs;
pub(crate) mod sync;
pub(crate) mod talker_alias;
pub mod voice;
pub mod voice_channel;

/// Maximum value of a 24-bit DMR on-air subscriber ID.
const MAX_DMR_ID_24BIT: u32 = 0x00FF_FFFF;

/// Encode a raw `u32` DMR ID as 3 big-endian bytes for the 24-bit
/// on-air src_id/dst_id fields.  Panics if `id > 2^24` -- silent
/// truncation would impersonate an unrelated subscriber.
pub(crate) fn id_to_24_be(id: u32) -> [u8; 3] {
    assert!(
        id <= MAX_DMR_ID_24BIT,
        "DMR ID {id} exceeds 24-bit max for 3-byte wire encoding"
    );
    let b = id.to_be_bytes();
    [b[1], b[2], b[3]]
}
