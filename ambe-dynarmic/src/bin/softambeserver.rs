use anyhow::{bail, Result};
use clap::Parser;
use std::net::{IpAddr, SocketAddr};
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_PORT: u16 = 2460;

#[derive(Parser)]
#[command(about = "Software AMBEServer: AMBEServer-compatible UDP vocoder")]
struct Args {
    /// IP address to bind to
    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,

    /// UDP port to listen on
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
}

mod dv3000 {
    use softambe::{AMBE_FEC_BYTES, PCM_FRAME_SAMPLES};

    pub const START_BYTE: u8 = 0x61;
    const HDR_LEN: usize = 4; // start(1) + length(2 BE) + type(1)

    // Packet types.
    const TYPE_CONTROL: u8 = 0x00;
    const TYPE_AMBE: u8 = 0x01;
    const TYPE_AUDIO: u8 = 0x02;

    // Control field IDs.
    const CTRL_RESET: u8 = 0x33;
    const CTRL_READY: u8 = 0x39;
    const CTRL_RATEP: u8 = 0x0A;
    const CTRL_GAIN: u8 = 0x4B;

    // Data field IDs.
    const FIELD_SPEECH: u8 = 0x00;
    const FIELD_CHANNEL: u8 = 0x01;
    const FIELD_CMODE: u8 = 0x02;

    const AMBE_BITS: u8 = (AMBE_FEC_BYTES * 8) as u8;

    /// AMBE+2 rate parameters for DMR (3600x2450).
    pub const RATEP_DMR: [u8; 12] = [
        0x04, 0x31, 0x07, 0x54, 0x24, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6F, 0x48,
    ];

    #[derive(Debug)]
    #[expect(clippy::large_enum_variant, reason = "Audio is short-lived on the stack in handle()")]
    pub enum Packet {
        Reset,
        /// Rate parameters requested by the client.
        Ratep([u8; 12]),
        /// Gain requested by the client: (input_db, output_db).
        Gain(i8, i8),
        Ambe([u8; AMBE_FEC_BYTES]),
        Audio([i16; PCM_FRAME_SAMPLES]),
    }

    #[derive(Debug, thiserror::Error)]
    pub enum ParseError {
        #[error("packet too short: {0} bytes")]
        TooShort(usize),
        #[error("bad start byte: 0x{0:02x}")]
        BadStartByte(u8),
        #[error("incomplete: have {have}, need {need}")]
        Incomplete { have: usize, need: usize },
        #[error("unknown packet type: 0x{0:02x}")]
        UnknownType(u8),
        #[error("bad field id: expected 0x{expected:02x}, got 0x{got:02x}")]
        BadFieldId { expected: u8, got: u8 },
        #[error("bad bit count: expected {expected}, got {got}")]
        BadBitCount { expected: u8, got: u8 },
        #[error("bad sample count: expected {expected}, got {got}")]
        BadSampleCount { expected: u8, got: u8 },
        #[error("empty control payload")]
        EmptyControl,
        #[error("unknown control field: 0x{0:02x}")]
        UnknownControl(u8),
        #[error("RATEP payload too short: {0} bytes")]
        RatepTooShort(usize),
        #[error("GAIN payload too short: {0} bytes")]
        GainTooShort(usize),
    }

    pub fn parse(buf: &[u8]) -> Result<Packet, ParseError> {
        if buf.len() < HDR_LEN {
            return Err(ParseError::TooShort(buf.len()));
        }
        if buf[0] != START_BYTE {
            return Err(ParseError::BadStartByte(buf[0]));
        }
        let payload_len = u16::from_be_bytes([buf[1], buf[2]]) as usize;
        let pkt_type = buf[3];
        let total = HDR_LEN + payload_len;
        if buf.len() < total {
            return Err(ParseError::Incomplete {
                have: buf.len(),
                need: total,
            });
        }
        let payload = &buf[HDR_LEN..total];
        match pkt_type {
            TYPE_CONTROL => parse_control(payload),
            TYPE_AMBE => parse_ambe(payload),
            TYPE_AUDIO => parse_audio(payload),
            other => Err(ParseError::UnknownType(other)),
        }
    }

    fn parse_control(payload: &[u8]) -> Result<Packet, ParseError> {
        if payload.is_empty() {
            return Err(ParseError::EmptyControl);
        }
        match payload[0] {
            CTRL_RESET => Ok(Packet::Reset),
            CTRL_RATEP => {
                let data = &payload[1..];
                if data.len() < RATEP_DMR.len() {
                    return Err(ParseError::RatepTooShort(data.len()));
                }
                let mut ratep = [0u8; 12];
                ratep.copy_from_slice(&data[..12]);
                Ok(Packet::Ratep(ratep))
            }
            CTRL_GAIN => {
                let data = &payload[1..];
                if data.len() < 2 {
                    return Err(ParseError::GainTooShort(data.len()));
                }
                Ok(Packet::Gain(data[0] as i8, data[1] as i8))
            }
            other => Err(ParseError::UnknownControl(other)),
        }
    }

    fn parse_ambe(payload: &[u8]) -> Result<Packet, ParseError> {
        let min = 2 + AMBE_FEC_BYTES;
        if payload.len() < min {
            return Err(ParseError::TooShort(payload.len()));
        }
        if payload[0] != FIELD_CHANNEL {
            return Err(ParseError::BadFieldId {
                expected: FIELD_CHANNEL,
                got: payload[0],
            });
        }
        if payload[1] != AMBE_BITS {
            return Err(ParseError::BadBitCount {
                expected: AMBE_BITS,
                got: payload[1],
            });
        }
        let mut frame = [0u8; AMBE_FEC_BYTES];
        frame.copy_from_slice(&payload[2..2 + AMBE_FEC_BYTES]);
        Ok(Packet::Ambe(frame))
    }

    fn parse_audio(payload: &[u8]) -> Result<Packet, ParseError> {
        let min = 2 + PCM_FRAME_SAMPLES * 2;
        if payload.len() < min {
            return Err(ParseError::TooShort(payload.len()));
        }
        if payload[0] != FIELD_SPEECH {
            return Err(ParseError::BadFieldId {
                expected: FIELD_SPEECH,
                got: payload[0],
            });
        }
        if payload[1] != PCM_FRAME_SAMPLES as u8 {
            return Err(ParseError::BadSampleCount {
                expected: PCM_FRAME_SAMPLES as u8,
                got: payload[1],
            });
        }
        let mut samples = [0i16; PCM_FRAME_SAMPLES];
        for (i, s) in samples.iter_mut().enumerate() {
            let off = 2 + i * 2;
            *s = i16::from_be_bytes([payload[off], payload[off + 1]]);
        }
        Ok(Packet::Audio(samples))
    }

    pub fn build_ready() -> Vec<u8> {
        build_control(CTRL_READY)
    }

    pub fn build_ratep_ack() -> Vec<u8> {
        build_control(CTRL_RATEP)
    }

    pub fn build_gain_ack() -> Vec<u8> {
        build_control(CTRL_GAIN)
    }

    pub fn build_ambe(frame: &[u8; AMBE_FEC_BYTES]) -> Vec<u8> {
        // payload: field_id(1) + num_bits(1) + frame(AMBE_FEC_BYTES)
        let payload_len = 2 + AMBE_FEC_BYTES;
        let mut buf = Vec::with_capacity(HDR_LEN + payload_len);
        push_header(&mut buf, TYPE_AMBE, payload_len);
        buf.push(FIELD_CHANNEL);
        buf.push(AMBE_BITS);
        buf.extend_from_slice(frame);
        buf
    }

    pub fn build_audio(pcm: &[i16; PCM_FRAME_SAMPLES]) -> Vec<u8> {
        // payload: field_id(1) + num_samples(1) + samples(PCM*2) + cmode_field(1) + cmode(2)
        let payload_len = 2 + PCM_FRAME_SAMPLES * 2 + 3;
        let mut buf = Vec::with_capacity(HDR_LEN + payload_len);
        push_header(&mut buf, TYPE_AUDIO, payload_len);
        buf.push(FIELD_SPEECH);
        buf.push(PCM_FRAME_SAMPLES as u8);
        for &s in pcm {
            buf.extend_from_slice(&s.to_be_bytes());
        }
        buf.push(FIELD_CMODE);
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf
    }

    fn build_control(field_id: u8) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HDR_LEN + 1);
        push_header(&mut buf, TYPE_CONTROL, 1);
        buf.push(field_id);
        buf
    }

    fn push_header(buf: &mut Vec<u8>, pkt_type: u8, payload_len: usize) {
        buf.push(START_BYTE);
        buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
        buf.push(pkt_type);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn round_trip_ambe() {
            let frame = [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x42];
            let buf = build_ambe(&frame);
            assert_eq!(buf[0], START_BYTE);
            let pkt = parse(&buf).unwrap();
            assert!(matches!(pkt, Packet::Ambe(f) if f == frame));
        }

        #[test]
        fn round_trip_audio() {
            let mut pcm = [0i16; PCM_FRAME_SAMPLES];
            for (i, s) in pcm.iter_mut().enumerate() {
                *s = (i as i16).wrapping_mul(100);
            }
            let buf = build_audio(&pcm);
            assert_eq!(buf[0], START_BYTE);
            let pkt = parse(&buf).unwrap();
            assert!(matches!(pkt, Packet::Audio(s) if s == pcm));
        }

        #[test]
        fn parse_reset() {
            let buf = [START_BYTE, 0x00, 0x01, 0x00, CTRL_RESET];
            assert!(matches!(parse(&buf).unwrap(), Packet::Reset));
        }

        #[test]
        fn parse_ratep_dmr() {
            let mut buf = vec![START_BYTE, 0x00, 0x0D, 0x00, CTRL_RATEP];
            buf.extend_from_slice(&RATEP_DMR);
            assert!(matches!(parse(&buf).unwrap(), Packet::Ratep(r) if r == RATEP_DMR));
        }

        #[test]
        fn parse_ratep_too_short() {
            let buf = [START_BYTE, 0x00, 0x01, 0x00, CTRL_RATEP];
            assert!(parse(&buf).is_err());
        }

        #[test]
        fn parse_gain() {
            let buf = [START_BYTE, 0x00, 0x03, 0x00, CTRL_GAIN, 0x00, 0x06];
            assert!(matches!(parse(&buf).unwrap(), Packet::Gain(0, 6)));
        }

        #[test]
        fn parse_gain_too_short() {
            let buf = [START_BYTE, 0x00, 0x01, 0x00, CTRL_GAIN];
            assert!(parse(&buf).is_err());
        }

        #[test]
        fn ready_packet_format() {
            // READY is server→client only; verify the byte layout directly.
            let buf = build_ready();
            assert_eq!(buf[0], START_BYTE);
            assert_eq!(u16::from_be_bytes([buf[1], buf[2]]), 1);
            assert_eq!(buf[3], 0x00); // TYPE_CONTROL
            assert_eq!(buf[4], CTRL_READY);
        }

        #[test]
        fn parse_bad_start_byte() {
            let buf = [0x00, 0x00, 0x01, 0x00, CTRL_RESET];
            assert!(parse(&buf).is_err());
        }

        #[test]
        fn parse_too_short() {
            assert!(parse(&[START_BYTE, 0x00]).is_err());
        }

        #[test]
        fn parse_incomplete_payload() {
            // Claims 100 bytes of payload but only provides 1.
            let buf = [START_BYTE, 0x00, 0x64, 0x00, CTRL_RESET];
            assert!(parse(&buf).is_err());
        }

        #[test]
        fn parse_unknown_type() {
            let buf = [START_BYTE, 0x00, 0x01, 0xFF, 0x00];
            assert!(parse(&buf).is_err());
        }

        #[test]
        fn parse_empty_control() {
            let buf = [START_BYTE, 0x00, 0x00, 0x00];
            assert!(parse(&buf).is_err());
        }

        #[test]
        fn parse_bad_ambe_field() {
            let mut buf = build_ambe(&[0u8; AMBE_FEC_BYTES]);
            buf[4] = 0xFF; // corrupt field_id
            assert!(parse(&buf).is_err());
        }

        #[test]
        fn parse_bad_audio_field() {
            let mut buf = build_audio(&[0i16; PCM_FRAME_SAMPLES]);
            buf[4] = 0xFF; // corrupt field_id
            assert!(parse(&buf).is_err());
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let addr = SocketAddr::new(args.bind, args.port);
    let sock = UdpSocket::bind(addr).await?;
    info!("softambeserver listening on {addr}");

    let mut current_client: Option<SocketAddr> = None;
    let mut buf = vec![0u8; 4096];
    loop {
        let (n, peer) = sock.recv_from(&mut buf).await?;

        if current_client != Some(peer) {
            info!("new client {peer} (was {current_client:?}); resetting codec");
            softambe::reset();
            current_client = Some(peer);
        }

        if let Err(e) = handle(&sock, peer, &buf[..n]).await {
            warn!("error from {peer}: {e}");
        }
    }
}

async fn handle(sock: &UdpSocket, peer: SocketAddr, raw: &[u8]) -> Result<()> {
    let response = match dv3000::parse(raw) {
        Err(e) => bail!("parse error: {e}"),
        Ok(dv3000::Packet::Reset) => {
            info!("reset from {peer}");
            dv3000::build_ready()
        }
        Ok(dv3000::Packet::Ratep(ratep)) => {
            if ratep != dv3000::RATEP_DMR {
                warn!("unsupported RATEP from {peer}: {ratep:02x?} (only DMR 3600x2450 supported)");
            } else {
                info!("RATEP DMR from {peer}");
            }
            dv3000::build_ratep_ack()
        }
        Ok(dv3000::Packet::Gain(in_db, out_db)) => {
            if in_db != 0 || out_db != 0 {
                warn!("gain request from {peer} (in={in_db} dB, out={out_db} dB) ignored: software codec has no gain control");
            }
            dv3000::build_gain_ack()
        }
        Ok(dv3000::Packet::Ambe(frame)) => {
            debug!("decode from {peer}");
            let pcm = softambe::decode_fec(&frame);
            dv3000::build_audio(&pcm)
        }
        Ok(dv3000::Packet::Audio(pcm)) => {
            debug!("encode from {peer}");
            let frame = softambe::encode_fec(&pcm);
            dv3000::build_ambe(&frame)
        }
    };
    sock.send_to(&response, peer).await?;
    Ok(())
}
