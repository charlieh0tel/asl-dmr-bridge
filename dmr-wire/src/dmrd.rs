//! Parse Homebrew-wire DMRD packets (53 bytes).
//!
//! Layout per DESIGN.md "Voice Frame -- DMRD":
//! ```text
//!   0..4    b"DMRD"
//!   4       seq
//!   5..8    src_id (24-bit BE)
//!   8..11   dst_id (24-bit BE)
//!   11..15  repeater_id (32-bit BE)
//!   15      flags (slot | call_type | frame_type | dtype_vseq)
//!   16..20  stream_id (32-bit BE)
//!   20..53  dmr_data (ETSI Layer 2 burst, parsed separately)
//! ```
//!
//! The `dmr_data` payload is a Layer 2 burst; disassembly into AMBE
//! codewords and embedded signaling happens in sibling modules.

use dmr_types::Slot;

const MAGIC: &[u8; 4] = b"DMRD";
const HEADER_SIZE: usize = 20;
pub const DMR_DATA_SIZE: usize = 33;
pub const PACKET_SIZE: usize = HEADER_SIZE + DMR_DATA_SIZE;

// Flag byte bit layout.
const FLAG_SLOT: u8 = 0x80;
const FLAG_CALL_TYPE: u8 = 0x40;
const FLAG_FRAME_TYPE_MASK: u8 = 0x30;
const FLAG_FRAME_TYPE_SHIFT: u32 = 4;
const FLAG_DTYPE_VSEQ_MASK: u8 = 0x0F;

/// Group vs unit (private) call.  `non_exhaustive` because the
/// underlying flag bit is one of two values today, but the wider
/// DMR-protocol family reserves combinations (e.g., direct-mode
/// peer call) that a future profile could surface here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CallType {
    Group,
    Unit,
}

/// High-level DMRD frame class (bits 5:4 of flags).  `non_exhaustive`
/// because the wire reserves four 2-bit values; today we surface
/// three and reject the fourth as `DmrdError::ReservedFrameType`,
/// but a future protocol extension could promote it to a real
/// variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameType {
    /// Mid-superframe voice burst (A..F cycle carried in `dtype_vseq`).
    Voice,
    /// Voice burst with sync pattern (start of superframe).
    VoiceSync,
    /// Data sync burst: voice LC header, voice terminator, or data.
    /// `dtype_vseq` carries the data type.
    DataSync,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DmrdError {
    #[error("packet too short: {0} bytes (want {PACKET_SIZE})")]
    TooShort(usize),
    #[error("bad magic: {0:?}")]
    BadMagic([u8; 4]),
    #[error("reserved frame_type bits (0b11)")]
    ReservedFrameType,
}

/// Parsed DMRD packet.  `dtype_vseq` carries voice-sequence 0..5 (A..F)
/// when `frame_type == Voice`, or a DMR data-type code when
/// `frame_type == DataSync`.  `src_id` and `dst_id` are 24-bit values
/// zero-extended into u32.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Dmrd {
    pub seq: u8,
    pub src_id: u32,
    pub dst_id: u32,
    pub repeater_id: u32,
    pub slot: Slot,
    pub call_type: CallType,
    pub frame_type: FrameType,
    pub dtype_vseq: u8,
    pub stream_id: u32,
    pub dmr_data: [u8; DMR_DATA_SIZE],
}

impl Dmrd {
    /// Serialize to a 53-byte DMRD packet.
    ///
    /// Panics if `src_id` or `dst_id` exceeds 2^24.  See `types::DmrId`
    /// type-level doc: these are 24-bit on-air subscriber IDs, and
    /// silent truncation would impersonate an unrelated user.
    pub fn serialize(&self) -> [u8; PACKET_SIZE] {
        let mut buf = [0u8; PACKET_SIZE];
        buf[0..4].copy_from_slice(MAGIC);
        buf[4] = self.seq;
        buf[5..8].copy_from_slice(&super::id_to_24_be(self.src_id));
        buf[8..11].copy_from_slice(&super::id_to_24_be(self.dst_id));
        buf[11..15].copy_from_slice(&self.repeater_id.to_be_bytes());

        let mut flags: u8 = 0;
        if matches!(self.slot, Slot::Two) {
            flags |= FLAG_SLOT;
        }
        if matches!(self.call_type, CallType::Unit) {
            flags |= FLAG_CALL_TYPE;
        }
        flags |= match self.frame_type {
            FrameType::Voice => 0,
            FrameType::VoiceSync => 1,
            FrameType::DataSync => 2,
        } << FLAG_FRAME_TYPE_SHIFT;
        flags |= self.dtype_vseq & FLAG_DTYPE_VSEQ_MASK;
        buf[15] = flags;

        buf[16..20].copy_from_slice(&self.stream_id.to_be_bytes());
        buf[HEADER_SIZE..].copy_from_slice(&self.dmr_data);
        buf
    }

    pub fn parse(buf: &[u8]) -> Result<Self, DmrdError> {
        if buf.len() < PACKET_SIZE {
            return Err(DmrdError::TooShort(buf.len()));
        }
        if &buf[0..4] != MAGIC {
            let mut m = [0u8; 4];
            m.copy_from_slice(&buf[0..4]);
            return Err(DmrdError::BadMagic(m));
        }

        // All fixed-offset indexing below is bounded by PACKET_SIZE,
        // checked at the top of the function; the try_into().expect()
        // calls cannot fail.
        let seq = buf[4];
        let src_id = u32::from_be_bytes([0, buf[5], buf[6], buf[7]]);
        let dst_id = u32::from_be_bytes([0, buf[8], buf[9], buf[10]]);
        let repeater_id = u32::from_be_bytes(
            buf[11..15]
                .try_into()
                .expect("buf len >= PACKET_SIZE checked above"),
        );
        let flags = buf[15];
        let stream_id = u32::from_be_bytes(
            buf[16..20]
                .try_into()
                .expect("buf len >= PACKET_SIZE checked above"),
        );

        let slot = if flags & FLAG_SLOT != 0 {
            Slot::Two
        } else {
            Slot::One
        };
        let call_type = if flags & FLAG_CALL_TYPE != 0 {
            CallType::Unit
        } else {
            CallType::Group
        };
        let frame_type = match (flags & FLAG_FRAME_TYPE_MASK) >> FLAG_FRAME_TYPE_SHIFT {
            0 => FrameType::Voice,
            1 => FrameType::VoiceSync,
            2 => FrameType::DataSync,
            _ => return Err(DmrdError::ReservedFrameType),
        };
        let dtype_vseq = flags & FLAG_DTYPE_VSEQ_MASK;

        let mut dmr_data = [0u8; DMR_DATA_SIZE];
        dmr_data.copy_from_slice(&buf[HEADER_SIZE..HEADER_SIZE + DMR_DATA_SIZE]);

        Ok(Dmrd {
            seq,
            src_id,
            dst_id,
            repeater_id,
            slot,
            call_type,
            frame_type,
            dtype_vseq,
            stream_id,
            dmr_data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_packet(flags: u8) -> Vec<u8> {
        let mut buf = Vec::with_capacity(PACKET_SIZE);
        buf.extend_from_slice(MAGIC);
        buf.push(0x2A); // seq
        buf.extend_from_slice(&[0x01, 0x23, 0x45]); // src_id = 0x012345
        buf.extend_from_slice(&[0x00, 0x00, 0x09]); // dst_id = TG 9
        buf.extend_from_slice(&[0x00, 0x12, 0xD6, 0x87]); // repeater_id
        buf.push(flags);
        buf.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // stream_id
        buf.extend_from_slice(&[0xAA; DMR_DATA_SIZE]);
        buf
    }

    #[test]
    fn parse_voice_ts1_group() {
        // flags: slot=0 (TS1), call=0 (group), frame_type=0 (voice), vseq=3 (D)
        let buf = sample_packet(0b0000_0011);
        let d = Dmrd::parse(&buf).unwrap();
        assert_eq!(d.seq, 0x2A);
        assert_eq!(d.src_id, 0x0001_2345);
        assert_eq!(d.dst_id, 9);
        assert_eq!(d.repeater_id, 1234567);
        assert_eq!(d.slot, Slot::One);
        assert_eq!(d.call_type, CallType::Group);
        assert_eq!(d.frame_type, FrameType::Voice);
        assert_eq!(d.dtype_vseq, 3);
        assert_eq!(d.stream_id, 0xDEAD_BEEF);
        assert_eq!(d.dmr_data, [0xAA; DMR_DATA_SIZE]);
    }

    #[test]
    fn parse_data_sync_ts2_unit() {
        // flags: slot=1 (TS2), call=1 (unit), frame_type=2 (data_sync), dtype=1
        let buf = sample_packet(0b1110_0001);
        let d = Dmrd::parse(&buf).unwrap();
        assert_eq!(d.slot, Slot::Two);
        assert_eq!(d.call_type, CallType::Unit);
        assert_eq!(d.frame_type, FrameType::DataSync);
        assert_eq!(d.dtype_vseq, 1);
    }

    #[test]
    fn parse_voice_sync() {
        // frame_type=1 (voice_sync)
        let buf = sample_packet(0b0001_0000);
        let d = Dmrd::parse(&buf).unwrap();
        assert_eq!(d.frame_type, FrameType::VoiceSync);
    }

    #[test]
    fn parse_reserved_frame_type() {
        let buf = sample_packet(0b0011_0000);
        assert!(matches!(
            Dmrd::parse(&buf),
            Err(DmrdError::ReservedFrameType)
        ));
    }

    #[test]
    fn parse_bad_magic() {
        let mut buf = sample_packet(0);
        buf[0..4].copy_from_slice(b"NOPE");
        assert!(matches!(Dmrd::parse(&buf), Err(DmrdError::BadMagic(_))));
    }

    #[test]
    fn parse_too_short() {
        let buf = [0u8; 10];
        assert!(matches!(Dmrd::parse(&buf), Err(DmrdError::TooShort(10))));
    }

    #[test]
    fn parse_tolerates_oversized_buffer() {
        // Homebrew DMRD is fixed-size 53 bytes.  If a peer ever
        // appends trailing data, parse reads the first PACKET_SIZE
        // bytes (treating the rest as garbage to be dropped) rather
        // than rejecting.  Document the behavior with a test.
        let mut buf = sample_packet(0);
        buf.extend_from_slice(&[0xFF; 16]); // trailing junk
        let d = Dmrd::parse(&buf).expect("oversized buffer should parse");
        assert_eq!(d.seq, 0x2A);
    }

    #[test]
    fn serialize_round_trip() {
        let orig = Dmrd {
            seq: 0x2A,
            src_id: 0x0001_2345,
            dst_id: 9,
            repeater_id: 1234567,
            slot: Slot::Two,
            call_type: CallType::Unit,
            frame_type: FrameType::DataSync,
            dtype_vseq: 5,
            stream_id: 0xDEAD_BEEF,
            dmr_data: [0xBB; DMR_DATA_SIZE],
        };
        let buf = orig.serialize();
        assert_eq!(buf.len(), PACKET_SIZE);
        assert_eq!(&buf[0..4], MAGIC);

        let parsed = Dmrd::parse(&buf).unwrap();
        assert_eq!(parsed.seq, orig.seq);
        assert_eq!(parsed.src_id, orig.src_id);
        assert_eq!(parsed.dst_id, orig.dst_id);
        assert_eq!(parsed.repeater_id, orig.repeater_id);
        assert_eq!(parsed.slot, orig.slot);
        assert_eq!(parsed.call_type, orig.call_type);
        assert_eq!(parsed.frame_type, orig.frame_type);
        assert_eq!(parsed.dtype_vseq, orig.dtype_vseq);
        assert_eq!(parsed.stream_id, orig.stream_id);
        assert_eq!(parsed.dmr_data, orig.dmr_data);
    }

    #[test]
    fn serialize_matches_sample_packet() {
        // Verify serialize produces the same bytes as our hand-built sample.
        let buf = sample_packet(0b0000_0011);
        let d = Dmrd::parse(&buf).unwrap();
        let serialized = d.serialize();
        assert_eq!(&serialized[..], &buf[..]);
    }

    #[test]
    #[should_panic(expected = "exceeds 24-bit max")]
    fn serialize_panics_on_src_id_over_24_bit() {
        // A hotspot repeater_id like 310770201 (AI6KG-01) fits in
        // 32 bits but NOT 24.  Must panic rather than silently
        // truncate onto an impostor subscriber ID.
        let d = Dmrd {
            seq: 0,
            src_id: 310_770_201,
            dst_id: 91,
            repeater_id: 310_770_201,
            slot: Slot::One,
            call_type: CallType::Group,
            frame_type: FrameType::Voice,
            dtype_vseq: 0,
            stream_id: 0,
            dmr_data: [0; DMR_DATA_SIZE],
        };
        let _ = d.serialize();
    }
}
