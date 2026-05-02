//! DV3000 packet format used by DVSI AMBE-3000 devices.
//!
//! Shared between ThumbDV (serial) and AMBEserver (UDP) backends.
//!
//! Packet structure:
//!   start_byte(1) = 0x61
//!   payload_length(2, big-endian)
//!   packet_type(1)
//!   payload(variable)

use crate::AMBE_BITS;
use crate::AMBE_FRAME_SIZE;
use crate::AmbeFrame;
use crate::PCM_SAMPLES;
use crate::PcmFrame;

/// Max receive buffer for DV3000 packets.
/// Largest packet is audio: header(4) + field_id(1) + num_samples(1)
/// + samples(320) + cmode(3) = 329 bytes.
pub(crate) const MAX_PACKET: usize = 512;

pub(crate) const START_BYTE: u8 = 0x61;
const HEADER_SIZE: usize = 4;

/// DV3000 packet types.
const TYPE_CONTROL: u8 = 0x00;
const TYPE_AMBE: u8 = 0x01;
const TYPE_AUDIO: u8 = 0x02;

/// Control field IDs.
const CONTROL_RATEP: u8 = 0x0A;
const CONTROL_GAIN: u8 = 0x4B;
const CONTROL_RESET: u8 = 0x33;
const CONTROL_READY: u8 = 0x39;
#[cfg(feature = "thumbdv")]
const CONTROL_PRODID: u8 = 0x30;

/// DV3000 gain range (dB), inclusive.  Values outside this range are
/// clamped before being sent.  Matches serialDV's setGain clamp.
pub(crate) const GAIN_MIN_DB: i8 = -90;
pub(crate) const GAIN_MAX_DB: i8 = 90;

/// Data field IDs within audio/AMBE payloads.
const FIELD_SPEECH_DATA: u8 = 0x00;
const FIELD_CMODE: u8 = 0x02;
const FIELD_CHANNEL_DATA: u8 = 0x01;

/// AMBE+2 rate parameters for DMR (3600x2450).
/// From serialDV dvcontroller.h DV3000_REQ_3600X2450_RATEP.
const RATEP_DMR: [u8; 12] = [
    0x04, 0x31, 0x07, 0x54, 0x24, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6F, 0x48,
];

/// DV3000 packet parse error.  Public so `VocoderError::Parse` can
/// carry it typed across the backend boundary; the enclosing module
/// `dv3000` is still `pub(crate)`, so effective visibility matches.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("packet too short: {have} bytes, need {need}")]
    TooShort { have: usize, need: usize },
    #[error("bad start byte: 0x{0:02x}")]
    BadStartByte(u8),
    #[error("incomplete packet: have {have} bytes, need {need}")]
    Incomplete { have: usize, need: usize },
    #[error("unknown packet type: 0x{0:02x}")]
    UnknownType(u8),
    #[error("bad field_id: expected 0x{expected:02x}, got 0x{got:02x}")]
    BadFieldId { expected: u8, got: u8 },
    #[error("bad bit count: expected {expected}, got {got}")]
    BadBitCount { expected: u8, got: u8 },
    #[error("bad sample count: expected {expected}, got {got}")]
    BadSampleCount { expected: u8, got: u8 },
    #[error("empty control payload")]
    EmptyControl,
}

/// Parsed DV3000 packet.
#[derive(Debug, Clone)]
pub(crate) enum Packet {
    /// AMBE+2 encoded audio data.
    Ambe(AmbeFrame),
    /// PCM audio samples.
    Audio(Box<PcmFrame>),
    /// Control response (field_id, raw payload).
    Control {
        field_id: u8,
        #[cfg_attr(
            not(feature = "thumbdv"),
            expect(dead_code, reason = "read by thumbdv prodid check")
        )]
        data: Vec<u8>,
    },
}

/// Parse a DV3000 packet from a buffer.
/// Returns the packet and the number of bytes consumed.
pub(crate) fn parse(buf: &[u8]) -> Result<(Packet, usize), ParseError> {
    if buf.len() < HEADER_SIZE {
        return Err(ParseError::TooShort {
            have: buf.len(),
            need: HEADER_SIZE,
        });
    }
    if buf[0] != START_BYTE {
        return Err(ParseError::BadStartByte(buf[0]));
    }

    let payload_len = u16::from_be_bytes([buf[1], buf[2]]) as usize;
    let packet_type = buf[3];
    let total_len = HEADER_SIZE + payload_len;

    if buf.len() < total_len {
        return Err(ParseError::Incomplete {
            have: buf.len(),
            need: total_len,
        });
    }

    let payload = &buf[HEADER_SIZE..total_len];

    let packet = match packet_type {
        TYPE_AMBE => parse_ambe(payload)?,
        TYPE_AUDIO => parse_audio(payload)?,
        TYPE_CONTROL => parse_control(payload)?,
        other => return Err(ParseError::UnknownType(other)),
    };

    Ok((packet, total_len))
}

fn parse_ambe(payload: &[u8]) -> Result<Packet, ParseError> {
    // field_id(1) + num_bits(1) + data(AMBE_FRAME_SIZE) + cmode(3)
    let min_len = 2 + AMBE_FRAME_SIZE;
    if payload.len() < min_len {
        return Err(ParseError::TooShort {
            have: payload.len(),
            need: min_len,
        });
    }
    if payload[0] != FIELD_CHANNEL_DATA {
        return Err(ParseError::BadFieldId {
            expected: FIELD_CHANNEL_DATA,
            got: payload[0],
        });
    }
    if payload[1] != AMBE_BITS {
        return Err(ParseError::BadBitCount {
            expected: AMBE_BITS,
            got: payload[1],
        });
    }
    let mut frame = [0u8; AMBE_FRAME_SIZE];
    frame.copy_from_slice(&payload[2..2 + AMBE_FRAME_SIZE]);
    Ok(Packet::Ambe(frame))
}

fn parse_audio(payload: &[u8]) -> Result<Packet, ParseError> {
    // field_id(1) + num_samples(1) + samples(PCM_SAMPLES * 2) + cmode(3)
    let samples_offset = 2;
    let samples_bytes = PCM_SAMPLES * 2;
    if payload.len() < samples_offset + samples_bytes {
        return Err(ParseError::TooShort {
            have: payload.len(),
            need: samples_offset + samples_bytes,
        });
    }
    if payload[0] != FIELD_SPEECH_DATA {
        return Err(ParseError::BadFieldId {
            expected: FIELD_SPEECH_DATA,
            got: payload[0],
        });
    }
    if payload[1] != PCM_SAMPLES as u8 {
        return Err(ParseError::BadSampleCount {
            expected: PCM_SAMPLES as u8,
            got: payload[1],
        });
    }
    let mut samples = [0i16; PCM_SAMPLES];
    for (i, sample) in samples.iter_mut().enumerate() {
        let off = samples_offset + i * 2;
        // DV3000 uses big-endian audio samples
        *sample = i16::from_be_bytes([payload[off], payload[off + 1]]);
    }
    Ok(Packet::Audio(Box::new(samples)))
}

fn parse_control(payload: &[u8]) -> Result<Packet, ParseError> {
    if payload.is_empty() {
        return Err(ParseError::EmptyControl);
    }
    Ok(Packet::Control {
        field_id: payload[0],
        data: payload[1..].to_vec(),
    })
}

/// Build a DV3000 audio packet (for encode: PCM -> AMBE).
pub(crate) fn build_audio(pcm: &PcmFrame) -> Vec<u8> {
    // payload: field_id(1) + num_samples(1) + samples(PCM_SAMPLES*2) + cmode_field(1) + cmode(2)
    let payload_len = 1 + 1 + PCM_SAMPLES * 2 + 1 + 2;
    let mut buf = Vec::with_capacity(HEADER_SIZE + payload_len);

    buf.push(START_BYTE);
    buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
    buf.push(TYPE_AUDIO);

    buf.push(FIELD_SPEECH_DATA);
    buf.push(PCM_SAMPLES as u8);
    for sample in pcm {
        buf.extend_from_slice(&sample.to_be_bytes());
    }
    buf.push(FIELD_CMODE);
    buf.extend_from_slice(&0u16.to_be_bytes());

    buf
}

/// Build a DV3000 AMBE packet (for decode: AMBE -> PCM).
pub(crate) fn build_ambe(ambe: &AmbeFrame) -> Vec<u8> {
    // payload: field_id(1) + num_bits(1) + data(AMBE_FRAME_SIZE).
    // Matches serialDV dvcontroller.cpp decodeIn; no trailing CMODE.
    let payload_len = 1 + 1 + AMBE_FRAME_SIZE;
    let mut buf = Vec::with_capacity(HEADER_SIZE + payload_len);

    buf.push(START_BYTE);
    buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
    buf.push(TYPE_AMBE);

    buf.push(FIELD_CHANNEL_DATA);
    buf.push(AMBE_BITS);
    buf.extend_from_slice(ambe);

    buf
}

/// Build a reset control packet.
pub(crate) fn build_reset() -> Vec<u8> {
    let payload_len: u16 = 1;
    let mut buf = Vec::with_capacity(HEADER_SIZE + payload_len as usize);
    buf.push(START_BYTE);
    buf.extend_from_slice(&payload_len.to_be_bytes());
    buf.push(TYPE_CONTROL);
    buf.push(CONTROL_RESET);
    buf
}

/// Build a RATEP control packet for DMR (AMBE+2 3600x2450).
pub(crate) fn build_ratep_dmr() -> Vec<u8> {
    let payload_len = 1 + RATEP_DMR.len();
    let mut buf = Vec::with_capacity(HEADER_SIZE + payload_len);

    buf.push(START_BYTE);
    buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
    buf.push(TYPE_CONTROL);
    buf.push(CONTROL_RATEP);
    buf.extend_from_slice(&RATEP_DMR);

    buf
}

/// Build a gain control packet.  `in_db` sets encoder input gain,
/// `out_db` sets decoder output gain; both in dB, clamped to
/// `GAIN_MIN_DB..=GAIN_MAX_DB`.  Matches serialDV's DV3000_REQ_GAIN.
pub(crate) fn build_gain(in_db: i8, out_db: i8) -> Vec<u8> {
    let in_db = in_db.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    let out_db = out_db.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    // payload: field_id(1) + in_gain(1) + out_gain(1) = 3
    let payload_len: u16 = 3;
    let mut buf = Vec::with_capacity(HEADER_SIZE + payload_len as usize);
    buf.push(START_BYTE);
    buf.extend_from_slice(&payload_len.to_be_bytes());
    buf.push(TYPE_CONTROL);
    buf.push(CONTROL_GAIN);
    buf.push(in_db as u8);
    buf.push(out_db as u8);
    buf
}

/// Check if a control packet is a GAIN acknowledgment.
pub(crate) fn is_gain_ack(packet: &Packet) -> bool {
    matches!(packet, Packet::Control { field_id, .. } if *field_id == CONTROL_GAIN)
}

/// Build a product ID query packet.
#[cfg(feature = "thumbdv")]
pub(crate) fn build_prodid() -> Vec<u8> {
    let payload_len: u16 = 1;
    let mut buf = Vec::with_capacity(HEADER_SIZE + payload_len as usize);
    buf.push(START_BYTE);
    buf.extend_from_slice(&payload_len.to_be_bytes());
    buf.push(TYPE_CONTROL);
    buf.push(CONTROL_PRODID);
    buf
}

/// Check if a control packet is a READY response.
pub(crate) fn is_ready(packet: &Packet) -> bool {
    matches!(packet, Packet::Control { field_id, .. } if *field_id == CONTROL_READY)
}

/// Check if a control packet is a RATEP acknowledgment.
pub(crate) fn is_ratep_ack(packet: &Packet) -> bool {
    matches!(packet, Packet::Control { field_id, .. } if *field_id == CONTROL_RATEP)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_audio() {
        let mut pcm = [0i16; PCM_SAMPLES];
        for (i, s) in pcm.iter_mut().enumerate() {
            *s = (i as i16) * 100 - 8000;
        }
        let buf = build_audio(&pcm);
        let (packet, consumed) = parse(&buf).unwrap();
        assert_eq!(consumed, buf.len());
        match packet {
            Packet::Audio(decoded) => assert_eq!(*decoded, pcm),
            other => panic!("expected Audio, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_ambe() {
        let ambe = [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x42];
        let buf = build_ambe(&ambe);
        let (packet, consumed) = parse(&buf).unwrap();
        assert_eq!(consumed, buf.len());
        match packet {
            Packet::Ambe(decoded) => assert_eq!(decoded, ambe),
            other => panic!("expected Ambe, got {other:?}"),
        }
    }

    #[test]
    fn reset_packet_format() {
        let buf = build_reset();
        assert_eq!(buf[0], START_BYTE);
        assert_eq!(buf[3], TYPE_CONTROL);
        assert_eq!(buf[4], CONTROL_RESET);
    }

    #[test]
    fn ratep_dmr_packet_format() {
        let buf = build_ratep_dmr();
        assert_eq!(buf[0], START_BYTE);
        assert_eq!(buf[3], TYPE_CONTROL);
        assert_eq!(buf[4], CONTROL_RATEP);
        assert_eq!(&buf[5..], &RATEP_DMR);
    }

    #[test]
    fn gain_packet_format() {
        let buf = build_gain(-3, 6);
        assert_eq!(buf[0], START_BYTE);
        assert_eq!(u16::from_be_bytes([buf[1], buf[2]]), 3);
        assert_eq!(buf[3], TYPE_CONTROL);
        assert_eq!(buf[4], CONTROL_GAIN);
        assert_eq!(buf[5] as i8, -3);
        assert_eq!(buf[6] as i8, 6);
    }

    #[test]
    fn gain_packet_clamps() {
        let buf = build_gain(-127, 127);
        assert_eq!(buf[5] as i8, GAIN_MIN_DB);
        assert_eq!(buf[6] as i8, GAIN_MAX_DB);
    }

    #[test]
    fn gain_ack_detection() {
        let packet = Packet::Control {
            field_id: CONTROL_GAIN,
            data: vec![],
        };
        assert!(is_gain_ack(&packet));
        assert!(!is_ratep_ack(&packet));
        assert!(!is_ready(&packet));
    }

    #[test]
    fn parse_bad_start_byte() {
        let buf = [0x00, 0x00, 0x01, TYPE_CONTROL, CONTROL_READY];
        assert!(parse(&buf).is_err());
    }

    #[test]
    fn parse_truncated() {
        let buf = [START_BYTE, 0x00];
        assert!(parse(&buf).is_err());
    }

    #[test]
    fn parse_bad_ambe_field_id() {
        let mut buf = build_ambe(&[0; AMBE_FRAME_SIZE]);
        buf[HEADER_SIZE] = 0xFF;
        assert!(parse(&buf).is_err());
    }

    #[test]
    fn parse_bad_audio_field_id() {
        let mut buf = build_audio(&[0; PCM_SAMPLES]);
        buf[HEADER_SIZE] = 0xFF;
        assert!(parse(&buf).is_err());
    }

    #[test]
    fn parse_incomplete_payload() {
        let buf = [START_BYTE, 0x00, 0x64, TYPE_AMBE];
        assert!(parse(&buf).is_err());
    }

    #[test]
    fn parse_unknown_type() {
        let buf = [START_BYTE, 0x00, 0x01, 0xFF, 0x00];
        assert!(parse(&buf).is_err());
    }

    #[test]
    fn parse_empty_control() {
        let buf = [START_BYTE, 0x00, 0x00, TYPE_CONTROL];
        assert!(parse(&buf).is_err());
    }

    #[test]
    #[cfg(feature = "thumbdv")]
    fn prodid_packet_format() {
        let buf = build_prodid();
        assert_eq!(buf[0], START_BYTE);
        assert_eq!(buf[3], TYPE_CONTROL);
        assert_eq!(buf[4], CONTROL_PRODID);
    }

    #[test]
    fn ready_detection() {
        let packet = Packet::Control {
            field_id: CONTROL_READY,
            data: vec![],
        };
        assert!(is_ready(&packet));
        assert!(!is_ratep_ack(&packet));
    }

    #[test]
    fn parse_stream_of_concatenated_packets() {
        // `parse` returns a `consumed` byte count so callers can
        // advance through a buffer holding multiple back-to-back
        // packets (e.g. from a serial read that batched two
        // responses).  Exercise that path: concatenate reset +
        // RATEP requests, parse, step, parse, assert each matches
        // the original.
        let a = build_reset();
        let b = build_ratep_dmr();
        let mut buf = Vec::with_capacity(a.len() + b.len());
        buf.extend_from_slice(&a);
        buf.extend_from_slice(&b);

        let (first, consumed1) = parse(&buf).expect("first packet");
        assert_eq!(consumed1, a.len());
        assert!(matches!(
            first,
            Packet::Control { field_id, .. } if field_id == CONTROL_RESET
        ));

        let (second, consumed2) = parse(&buf[consumed1..]).expect("second packet");
        assert_eq!(consumed2, b.len());
        assert!(matches!(
            second,
            Packet::Control { field_id, .. } if field_id == CONTROL_RATEP
        ));
    }

    // --- Fuzz: parse must never panic on adversarial input ---
    //
    // The dv3000 parser eats bytes that come from outside the trust
    // boundary (FTDI serial from ThumbDV, UDP from AMBEserver).  Any
    // panic here would crash the bridge; we want every malformed
    // input to return a typed `ParseError` instead.

    use proptest::prelude::*;

    proptest! {
        // 4 bytes is the minimum for a non-trivial header; cap at 1 KiB
        // so the test runs fast.  parse() peels off one packet at a
        // time, so longer inputs don't add coverage proportionally.
        #[test]
        fn parse_never_panics_on_arbitrary_bytes(
            bytes in prop::collection::vec(any::<u8>(), 0..1024)
        ) {
            let _ = parse(&bytes);
        }

        // Same property, but the buffer always starts with the valid
        // start byte: forces parse() past the BadStartByte fast path
        // into the type-dispatch / payload-parse arms.
        #[test]
        fn parse_never_panics_after_valid_start_byte(
            tail in prop::collection::vec(any::<u8>(), 0..1024)
        ) {
            let mut buf = Vec::with_capacity(tail.len() + 1);
            buf.push(START_BYTE);
            buf.extend_from_slice(&tail);
            let _ = parse(&buf);
        }
    }
}
