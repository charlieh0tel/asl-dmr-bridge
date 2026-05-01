//! USRP voice-frame wire format.
//!
//! USRP is the UDP framing used by AllStarLink's chan_usrp,
//! analog_bridge, MMDVM_Bridge, and friends to carry 8 kHz / 160-
//! sample / 20 ms PCM frames between FM voice peers.  Each packet
//! is either:
//!
//! * **Header-only** (32 bytes): keyup transitions, unkey, control
//!   metadata.  Carries no audio.
//! * **Header + voice** (32 + 320 bytes): one PCM frame.
//!
//! Layout (big-endian unless noted):
//! ```text
//!   0..4    "USRP" magic
//!   4..8    seq (u32, monotonic per stream)
//!   8..12   memory (unused, zero)
//!   12..16  keyup (u32, 0 == unkey, !=0 == keyup)
//!   16..20  talkgroup (u32)
//!   20..24  frame_type (u32: 0=Voice, 1=DTMF, 2=Text)
//!   24..28  mpxid (unused, zero)
//!   28..32  reserved (zero)
//!   32..352 voice audio (160 i16 samples, native byte order on the
//!           sender; byte_swap option handles cross-endian peers)
//! ```
//!
//! The wire layout is fossilized -- chan_usrp.c hasn't moved in
//! years, and BroadAccess / DMRGateway / analog_bridge all assume
//! the same shape.  Encode/decode here is a pure pass; the calling
//! crate adds tokio plumbing, pacing, and audio routing.

use std::time::Duration;

const MAGIC: &[u8; 4] = b"USRP";

/// Header bytes preceding the optional 320-byte audio payload.
pub const HEADER_SIZE: usize = 32;

/// Samples per voice frame: 160 == 20 ms at 8 kHz.
pub const VOICE_SAMPLES: usize = 160;

/// Bytes for the audio payload (`VOICE_SAMPLES` i16s).
pub const VOICE_FRAME_SIZE: usize = VOICE_SAMPLES * size_of::<i16>();

/// Bytes for a full voice packet (header + audio).
pub const PACKET_SIZE: usize = HEADER_SIZE + VOICE_FRAME_SIZE;

/// Suggested extra bytes beyond `PACKET_SIZE` when sizing a recv
/// buffer.  Lets a receiver detect oversized packets without
/// silently truncating them.
pub const RECV_SLACK: usize = 64;

/// 20 ms inter-frame interval matching one voice frame.  Real-time
/// USRP producers (analog_bridge, MMDVM_Bridge) emit at this
/// cadence; consumers (chan_usrp, usrp_play) assume it.
pub const VOICE_FRAME_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UsrpError {
    #[error("packet too short: {0} bytes")]
    TooShort(usize),
    #[error("bad magic: {0:?}")]
    BadMagic([u8; 4]),
    #[error("unknown frame type: {0}")]
    UnknownFrameType(u32),
    /// Length is between `HEADER_SIZE` and `PACKET_SIZE` -- cannot
    /// tell if it's an unkey (header-only) or a truncated voice
    /// packet, so reject rather than silently treat as unkey.
    #[error("packet length {0} is neither header-only ({1}) nor full voice ({2})")]
    AmbiguousLength(usize, usize, usize),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// USRP packet type field.  `#[repr(u32)]` makes the `as u32` cast
/// in `serialize` well-defined and matches the on-wire u32 encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
#[non_exhaustive]
pub enum FrameType {
    Voice = 0,
    Dtmf = 1,
    Text = 2,
}

impl FrameType {
    fn from_u32(v: u32) -> Result<Self, UsrpError> {
        match v {
            0 => Ok(FrameType::Voice),
            1 => Ok(FrameType::Dtmf),
            2 => Ok(FrameType::Text),
            other => Err(UsrpError::UnknownFrameType(other)),
        }
    }
}

/// Parsed USRP frame.
#[derive(Debug, Clone)]
pub struct Frame {
    pub seq: u32,
    pub keyup: bool,
    pub talkgroup: u32,
    pub frame_type: FrameType,
    /// Voice audio samples. None for unkey (header-only) packets.
    pub audio: Option<[i16; VOICE_SAMPLES]>,
    /// Variable-length payload for Text / DTMF frames (NUL terminator
    /// stripped on parse).  `None` for Voice and unkey packets.
    pub text: Option<Vec<u8>>,
}

/// Read an i16 sample, optionally byte-swapping for cross-endian peers.
fn read_sample(bytes: [u8; 2], byte_swap: bool) -> i16 {
    if byte_swap {
        i16::from_ne_bytes([bytes[1], bytes[0]])
    } else {
        i16::from_ne_bytes(bytes)
    }
}

/// Write an i16 sample, optionally byte-swapping for cross-endian peers.
fn write_sample(sample: i16, byte_swap: bool) -> [u8; 2] {
    let bytes = sample.to_ne_bytes();
    if byte_swap {
        [bytes[1], bytes[0]]
    } else {
        bytes
    }
}

impl Frame {
    /// Parse a USRP packet from a received buffer.
    /// `byte_swap`: swap audio sample bytes for cross-endian peers.
    #[must_use = "parse result should be checked"]
    pub fn parse(buf: &[u8], byte_swap: bool) -> Result<Self, UsrpError> {
        if buf.len() < HEADER_SIZE {
            return Err(UsrpError::TooShort(buf.len()));
        }
        if &buf[0..4] != MAGIC {
            let mut m = [0u8; 4];
            m.copy_from_slice(&buf[0..4]);
            return Err(UsrpError::BadMagic(m));
        }

        let seq = u32::from_be_bytes(buf[4..8].try_into().unwrap());
        let keyup = u32::from_be_bytes(buf[12..16].try_into().unwrap()) != 0;
        let talkgroup = u32::from_be_bytes(buf[16..20].try_into().unwrap());
        let frame_type = FrameType::from_u32(u32::from_be_bytes(buf[20..24].try_into().unwrap()))?;

        let (audio, text) = match frame_type {
            FrameType::Voice => {
                // Voice frames must be exactly HEADER_SIZE (unkey /
                // header-only) or PACKET_SIZE (header + audio).
                // Anything between is ambiguous: a short keyup looks
                // identical to an unkey and would silently drive PTT
                // wrongly.
                if buf.len() != HEADER_SIZE && buf.len() != PACKET_SIZE {
                    return Err(UsrpError::AmbiguousLength(
                        buf.len(),
                        HEADER_SIZE,
                        PACKET_SIZE,
                    ));
                }
                let audio = if buf.len() >= PACKET_SIZE {
                    let mut samples = [0i16; VOICE_SAMPLES];
                    for (i, sample) in samples.iter_mut().enumerate() {
                        let offset = HEADER_SIZE + i * 2;
                        *sample =
                            read_sample(buf[offset..offset + 2].try_into().unwrap(), byte_swap);
                    }
                    Some(samples)
                } else {
                    None
                };
                (audio, None)
            }
            FrameType::Text | FrameType::Dtmf => {
                let mut payload = buf[HEADER_SIZE..].to_vec();
                if payload.last() == Some(&0) {
                    payload.pop();
                }
                (None, Some(payload))
            }
        };

        Ok(Frame {
            seq,
            keyup,
            talkgroup,
            frame_type,
            audio,
            text,
        })
    }

    /// Serialize a USRP TEXT frame (`frame_type=2`) carrying an
    /// arbitrary string payload.  Layout: 32-byte header (only the
    /// magic, seq, and frame_type fields populated), then payload
    /// bytes, then a trailing NUL.  chan_usrp / DVSwitch consumers
    /// expect the terminator to bound the string.
    ///
    /// Voice TX is unrelated to this; this is for out-of-band call
    /// metadata (talker info, JSON blobs, etc.).
    #[must_use]
    pub fn serialize_text(seq: u32, payload: &str) -> Vec<u8> {
        let mut buf = vec![0u8; HEADER_SIZE + payload.len() + 1];
        buf[0..4].copy_from_slice(MAGIC);
        buf[4..8].copy_from_slice(&seq.to_be_bytes());
        // memory [8..12], keyup [12..16], talkgroup [16..20] stay zero
        buf[20..24].copy_from_slice(&(FrameType::Text as u32).to_be_bytes());
        // mpxid [24..28], reserved [28..32] stay zero
        buf[HEADER_SIZE..HEADER_SIZE + payload.len()].copy_from_slice(payload.as_bytes());
        // trailing NUL at HEADER_SIZE + payload.len() (zero-init from vec!)
        buf
    }

    /// Serialize this frame to a USRP packet.
    /// `byte_swap`: swap audio sample bytes for cross-endian peers.
    #[must_use]
    pub fn serialize(&self, byte_swap: bool) -> Vec<u8> {
        let has_audio = self.audio.is_some();
        let len = if has_audio { PACKET_SIZE } else { HEADER_SIZE };
        let mut buf = vec![0u8; len];

        buf[0..4].copy_from_slice(MAGIC);
        buf[4..8].copy_from_slice(&self.seq.to_be_bytes());
        // memory field at [8..12] left as zero
        buf[12..16].copy_from_slice(&u32::from(self.keyup).to_be_bytes());
        buf[16..20].copy_from_slice(&self.talkgroup.to_be_bytes());
        buf[20..24].copy_from_slice(&(self.frame_type as u32).to_be_bytes());
        // mpxid [24..28] and reserved [28..32] left as zero

        if let Some(ref samples) = self.audio {
            for (i, sample) in samples.iter().enumerate() {
                let offset = HEADER_SIZE + i * 2;
                buf[offset..offset + 2].copy_from_slice(&write_sample(*sample, byte_swap));
            }
        }

        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_voice_frame() -> Frame {
        let mut audio = [0i16; VOICE_SAMPLES];
        for (i, sample) in audio.iter_mut().enumerate() {
            *sample = i as i16 * 100;
        }
        Frame {
            seq: 42,
            keyup: true,
            talkgroup: 2,
            frame_type: FrameType::Voice,
            audio: Some(audio),
            text: None,
        }
    }

    fn make_unkey_frame() -> Frame {
        Frame {
            seq: 43,
            keyup: false,
            talkgroup: 2,
            frame_type: FrameType::Voice,
            audio: None,
            text: None,
        }
    }

    #[test]
    fn round_trip_voice() {
        let frame = make_voice_frame();
        let buf = frame.serialize(false);
        assert_eq!(buf.len(), PACKET_SIZE);
        assert_eq!(&buf[0..4], b"USRP");

        let parsed = Frame::parse(&buf, false).unwrap();
        assert_eq!(parsed.seq, 42);
        assert!(parsed.keyup);
        assert_eq!(parsed.talkgroup, 2);
        assert_eq!(parsed.frame_type, FrameType::Voice);
        assert_eq!(parsed.audio.unwrap(), frame.audio.unwrap());
    }

    #[test]
    fn round_trip_unkey() {
        let frame = make_unkey_frame();
        let buf = frame.serialize(false);
        assert_eq!(buf.len(), HEADER_SIZE);

        let parsed = Frame::parse(&buf, false).unwrap();
        assert_eq!(parsed.seq, 43);
        assert!(!parsed.keyup);
        assert!(parsed.audio.is_none());
    }

    #[test]
    fn parse_bad_magic() {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(b"NOPE");
        assert!(Frame::parse(&buf, false).is_err());
    }

    #[test]
    fn parse_too_short() {
        let buf = [0u8; 16];
        assert!(Frame::parse(&buf, false).is_err());
    }

    #[test]
    fn parse_rejects_ambiguous_length() {
        // Between header-only and full-voice: cannot decide if this
        // is an unkey or a truncated voice packet.  Must reject.
        let mut buf = [0u8; HEADER_SIZE + 100];
        buf[0..4].copy_from_slice(MAGIC);
        // keyup=true flag so the accept-as-unkey trap would bite.
        buf[12..16].copy_from_slice(&1u32.to_be_bytes());
        assert!(matches!(
            Frame::parse(&buf, false),
            Err(UsrpError::AmbiguousLength(_, _, _))
        ));
    }

    #[test]
    fn header_byte_order() {
        let frame = Frame {
            seq: 0x01020304,
            keyup: true,
            talkgroup: 0x00000002,
            frame_type: FrameType::Voice,
            audio: None,
            text: None,
        };
        let buf = frame.serialize(false);
        assert_eq!(&buf[4..8], &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(&buf[12..16], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(&buf[16..20], &[0x00, 0x00, 0x00, 0x02]);
    }

    #[test]
    fn round_trip_byte_swap() {
        let frame = make_voice_frame();
        let buf = frame.serialize(true);
        let parsed = Frame::parse(&buf, true).unwrap();
        assert_eq!(parsed.audio.unwrap(), frame.audio.unwrap());
    }

    #[test]
    fn byte_swap_flips_samples() {
        let frame = make_voice_frame();
        let native_buf = frame.serialize(false);
        let swapped_buf = frame.serialize(true);
        // Sample index 1 in the audio payload (2 bytes per i16 sample).
        let offset = HEADER_SIZE + 2;
        assert_ne!(
            native_buf[offset..offset + 2],
            swapped_buf[offset..offset + 2]
        );
    }

    #[test]
    fn serialize_text_layout() {
        let buf = Frame::serialize_text(7, "hello");
        // header (32) + payload (5) + NUL (1) = 38
        assert_eq!(buf.len(), HEADER_SIZE + 5 + 1);
        assert_eq!(&buf[0..4], b"USRP");
        // seq
        assert_eq!(&buf[4..8], &[0, 0, 0, 7]);
        // keyup, talkgroup, mpxid, reserved all zero
        assert!(buf[8..20].iter().all(|&b| b == 0));
        // frame_type = 2 (Text)
        assert_eq!(&buf[20..24], &[0, 0, 0, 2]);
        assert!(buf[24..32].iter().all(|&b| b == 0));
        // payload + trailing NUL
        assert_eq!(&buf[HEADER_SIZE..HEADER_SIZE + 5], b"hello");
        assert_eq!(buf[HEADER_SIZE + 5], 0);
    }

    #[test]
    fn serialize_text_empty_payload() {
        // Empty payload is just header + a single NUL byte.
        let buf = Frame::serialize_text(0, "");
        assert_eq!(buf.len(), HEADER_SIZE + 1);
        assert_eq!(&buf[0..4], b"USRP");
        assert_eq!(buf[HEADER_SIZE], 0);
    }

    #[test]
    fn serialize_text_round_trips_through_parse() {
        let buf = Frame::serialize_text(7, "hello");
        let frame = Frame::parse(&buf, false).expect("text frame parses");
        assert_eq!(frame.seq, 7);
        assert_eq!(frame.frame_type, FrameType::Text);
        assert!(frame.audio.is_none());
        assert_eq!(frame.text.as_deref(), Some(&b"hello"[..]));
    }

    #[test]
    fn parse_empty_text_payload() {
        let buf = Frame::serialize_text(0, "");
        let frame = Frame::parse(&buf, false).expect("empty text parses");
        assert_eq!(frame.frame_type, FrameType::Text);
        assert_eq!(frame.text.as_deref(), Some(&[][..]));
    }

    #[test]
    fn parse_still_rejects_ambiguous_voice_length() {
        // Voice (frame_type=0) between header-only and full-voice
        // sizes is still rejected as ambiguous; the per-type relaxation
        // applies only to Text/DTMF.
        let mut buf = [0u8; HEADER_SIZE + 100];
        buf[0..4].copy_from_slice(MAGIC);
        buf[12..16].copy_from_slice(&1u32.to_be_bytes());
        // frame_type stays 0 (Voice) by zero-init.
        assert!(matches!(
            Frame::parse(&buf, false),
            Err(UsrpError::AmbiguousLength(_, _, _))
        ));
    }

    #[test]
    fn unknown_frame_type() {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(MAGIC);
        buf[20..24].copy_from_slice(&99u32.to_be_bytes());
        assert!(matches!(
            Frame::parse(&buf, false),
            Err(UsrpError::UnknownFrameType(99))
        ));
    }
}
