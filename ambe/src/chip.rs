//! Low-level chip-control client API for DVSI AMBE-3000R.
//!
//! `Vocoder` covers the routine encode/decode-at-DMR-default-rate
//! case.  This trait exposes the primitives the chip itself supports
//! that don't fit in `Vocoder`: switching `RATEP` and `GAIN` mid-
//! session, encoding at non-72-bit rates, and decoding the matching
//! variable-bit-count input.
//!
//! Two implementations:
//!
//! - `AmbeServerClient` (always available): UDP to a chip behind an
//!   ambeserver.  The server gives one client at a time exclusive
//!   access to the chip; concurrent peers are refused until the
//!   current holder goes idle.
//! - `ThumbDvClient` (`thumbdv` feature): direct serial.  Caller has
//!   exclusive access to the serial device.

use crate::PcmFrame;
use crate::VocoderError;
use crate::dv3000;
use crate::wire;

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
fn build_ambe_for_bits(bits: u8, data: &[u8]) -> Vec<u8> {
    const FIELD_CHANNEL_DATA: u8 = 0x01;
    let payload_len = 1 + 1 + data.len();
    let mut buf = Vec::with_capacity(wire::HEADER_SIZE + payload_len);
    buf.push(wire::START_BYTE);
    buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
    buf.push(wire::TYPE_AMBE);
    buf.push(FIELD_CHANNEL_DATA);
    buf.push(bits);
    buf.extend_from_slice(data);
    buf
}

// ---------------------------------------------------------------- AMBEserver impl

/// UDP client to an ambeserver proxy.  Each instance is a new
/// session from the server's perspective; per-session `RATEP` /
/// `GAIN` state lives on the server side.
pub struct AmbeServerClient {
    socket: std::net::UdpSocket,
    buf: Vec<u8>,
}

impl AmbeServerClient {
    pub fn connect(addr: std::net::SocketAddr) -> Result<Self, VocoderError> {
        let bind_addr = match addr {
            std::net::SocketAddr::V4(_) => "0.0.0.0:0",
            std::net::SocketAddr::V6(_) => "[::]:0",
        };
        let socket = std::net::UdpSocket::bind(bind_addr)?;
        socket.connect(addr)?;
        socket.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
        Ok(Self {
            socket,
            buf: vec![0u8; dv3000::MAX_PACKET],
        })
    }

    fn send_recv(&mut self, packet: &[u8]) -> Result<dv3000::Packet, VocoderError> {
        self.socket.send(packet)?;
        let len = self.socket.recv(&mut self.buf)?;
        let (response, _) = dv3000::parse(&self.buf[..len])?;
        Ok(response)
    }
}

impl ChipClient for AmbeServerClient {
    fn reset(&mut self) -> Result<(), VocoderError> {
        let response = self.send_recv(&dv3000::build_reset())?;
        if !dv3000::is_ready(&response) {
            return Err(VocoderError::Protocol(format!(
                "expected READY after reset, got {response:?}"
            )));
        }
        Ok(())
    }

    fn set_ratep(&mut self, rcws: &[u8; 12]) -> Result<(), VocoderError> {
        let response = self.send_recv(&dv3000::build_ratep_custom(rcws))?;
        if !dv3000::is_ratep_ack(&response) {
            return Err(VocoderError::Protocol(format!(
                "expected RATEP ack, got {response:?}"
            )));
        }
        Ok(())
    }

    fn set_gain(&mut self, in_db: i8, out_db: i8) -> Result<(), VocoderError> {
        let response = self.send_recv(&dv3000::build_gain(in_db, out_db))?;
        if !dv3000::is_gain_ack(&response) {
            return Err(VocoderError::Protocol(format!(
                "expected GAIN ack, got {response:?}"
            )));
        }
        Ok(())
    }

    fn encode_raw(&mut self, pcm: &PcmFrame) -> Result<(u8, Vec<u8>), VocoderError> {
        match self.send_recv(&dv3000::build_audio(pcm))? {
            dv3000::Packet::Ambe(frame) => Ok((72, frame.to_vec())),
            dv3000::Packet::AmbeBits { bits, data } => Ok((bits, data)),
            other => Err(VocoderError::Encode(format!(
                "expected AMBE response, got {other:?}"
            ))),
        }
    }

    fn decode_raw(&mut self, bits: u8, data: &[u8]) -> Result<PcmFrame, VocoderError> {
        match self.send_recv(&build_ambe_for_bits(bits, data))? {
            dv3000::Packet::Audio(samples) => Ok(*samples),
            other => Err(VocoderError::Decode(format!(
                "expected audio response, got {other:?}"
            ))),
        }
    }
}

// ---------------------------------------------------------------- ThumbDV impl

#[cfg(feature = "thumbdv")]
const SERIAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
#[cfg(feature = "thumbdv")]
const DEFAULT_BAUD: u32 = 460_800;

/// Direct serial client to a ThumbDV / DV3000 chip.  Caller has
/// exclusive access to the serial device.
#[cfg(feature = "thumbdv")]
pub struct ThumbDvClient {
    port: Box<dyn serialport::SerialPort>,
    buf: Vec<u8>,
}

#[cfg(feature = "thumbdv")]
impl ThumbDvClient {
    /// Open the serial device, reset the chip, and wait for the
    /// READY response.  No RATEP / GAIN is set; caller configures
    /// before encoding.
    pub fn open(path: &str, baud: Option<u32>) -> Result<Self, VocoderError> {
        let baud = baud.unwrap_or(DEFAULT_BAUD);
        let port = serialport::new(path, baud)
            .timeout(SERIAL_TIMEOUT)
            .open()
            .map_err(|e| VocoderError::Init(format!("opening {path} at {baud} baud: {e}")))?;
        port.clear(serialport::ClearBuffer::All)
            .map_err(|e| VocoderError::Init(format!("clear serial buffers: {e}")))?;
        let mut client = Self {
            port,
            buf: vec![0u8; dv3000::MAX_PACKET],
        };
        client.send_raw(&dv3000::build_reset())?;
        let r = client.recv()?;
        if !dv3000::is_ready(&r) {
            return Err(VocoderError::Init(format!(
                "expected READY after reset, got {r:?}"
            )));
        }
        Ok(client)
    }

    fn send_raw(&mut self, packet: &[u8]) -> Result<(), VocoderError> {
        use std::io::Write as _;
        self.port.write_all(packet)?;
        self.port.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<dv3000::Packet, VocoderError> {
        use std::io::Read as _;
        let mut header = [0u8; wire::HEADER_SIZE];
        self.port.read_exact(&mut header)?;
        if header[0] != wire::START_BYTE {
            return Err(VocoderError::Protocol(format!(
                "bad start byte: 0x{:02x}",
                header[0]
            )));
        }
        let payload_len = u16::from_be_bytes([header[1], header[2]]) as usize;
        if payload_len + 4 > self.buf.len() {
            return Err(VocoderError::Protocol(format!(
                "payload too large: {payload_len}"
            )));
        }
        self.buf[..4].copy_from_slice(&header);
        self.port.read_exact(&mut self.buf[4..4 + payload_len])?;
        let (packet, _) = dv3000::parse(&self.buf[..4 + payload_len])?;
        Ok(packet)
    }

    fn send_recv(&mut self, packet: &[u8]) -> Result<dv3000::Packet, VocoderError> {
        self.send_raw(packet)?;
        self.recv()
    }
}

#[cfg(feature = "thumbdv")]
impl ChipClient for ThumbDvClient {
    fn reset(&mut self) -> Result<(), VocoderError> {
        let r = self.send_recv(&dv3000::build_reset())?;
        if !dv3000::is_ready(&r) {
            return Err(VocoderError::Protocol(format!(
                "expected READY after reset, got {r:?}"
            )));
        }
        Ok(())
    }

    fn set_ratep(&mut self, rcws: &[u8; 12]) -> Result<(), VocoderError> {
        let r = self.send_recv(&dv3000::build_ratep_custom(rcws))?;
        if !dv3000::is_ratep_ack(&r) {
            return Err(VocoderError::Protocol(format!(
                "expected RATEP ack, got {r:?}"
            )));
        }
        Ok(())
    }

    fn set_gain(&mut self, in_db: i8, out_db: i8) -> Result<(), VocoderError> {
        let r = self.send_recv(&dv3000::build_gain(in_db, out_db))?;
        if !dv3000::is_gain_ack(&r) {
            return Err(VocoderError::Protocol(format!(
                "expected GAIN ack, got {r:?}"
            )));
        }
        Ok(())
    }

    fn encode_raw(&mut self, pcm: &PcmFrame) -> Result<(u8, Vec<u8>), VocoderError> {
        match self.send_recv(&dv3000::build_audio(pcm))? {
            dv3000::Packet::Ambe(frame) => Ok((72, frame.to_vec())),
            dv3000::Packet::AmbeBits { bits, data } => Ok((bits, data)),
            other => Err(VocoderError::Encode(format!(
                "expected AMBE response, got {other:?}"
            ))),
        }
    }

    fn decode_raw(&mut self, bits: u8, data: &[u8]) -> Result<PcmFrame, VocoderError> {
        match self.send_recv(&build_ambe_for_bits(bits, data))? {
            dv3000::Packet::Audio(samples) => Ok(*samples),
            other => Err(VocoderError::Decode(format!(
                "expected audio response, got {other:?}"
            ))),
        }
    }
}
