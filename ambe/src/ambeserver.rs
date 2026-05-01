//! AMBEserver UDP client backend.
//!
//! Connects to an existing AMBEserver daemon that proxies DV3000
//! packets over UDP. Default port 2460.

use std::net::SocketAddr;
use std::net::UdpSocket;
use std::time::Duration;

use tracing::info;

use crate::AmbeFrame;
use crate::PcmFrame;
use crate::Vocoder;
use crate::VocoderError;
use crate::dv3000;

const RECV_TIMEOUT: Duration = Duration::from_secs(2);

/// AMBEserver UDP client.
pub(crate) struct AmbeServer {
    socket: UdpSocket,
    buf: Vec<u8>,
}

impl AmbeServer {
    /// Connect to an AMBEserver proxy and initialize the chip for DMR.
    ///
    /// `gain_db`: optional (input_db, output_db) to apply after RATEP.
    /// Each is clamped to [-90, 90] dB.  `None` leaves the chip at
    /// its default gain (0 dB).
    pub(crate) fn connect(
        addr: SocketAddr,
        gain_db: Option<(i8, i8)>,
    ) -> Result<Self, VocoderError> {
        let bind_addr = match addr {
            SocketAddr::V4(_) => "0.0.0.0:0",
            SocketAddr::V6(_) => "[::]:0",
        };
        let socket = UdpSocket::bind(bind_addr)?;
        socket.connect(addr)?;
        socket.set_read_timeout(Some(RECV_TIMEOUT))?;

        let mut server = Self {
            socket,
            buf: vec![0u8; dv3000::MAX_PACKET],
        };

        server.init(gain_db)?;
        info!("connected to AMBEserver at {addr}");
        Ok(server)
    }

    fn init(&mut self, gain_db: Option<(i8, i8)>) -> Result<(), VocoderError> {
        self.send_raw(&dv3000::build_reset())?;
        let response = self.recv()?;
        if !dv3000::is_ready(&response) {
            return Err(VocoderError::Init(format!(
                "expected READY after reset, got {response:?}"
            )));
        }

        self.send_raw(&dv3000::build_ratep_dmr())?;
        let response = self.recv()?;
        if !dv3000::is_ratep_ack(&response) {
            return Err(VocoderError::Init(format!(
                "expected RATEP ack, got {response:?}"
            )));
        }

        if let Some((in_db, out_db)) = gain_db {
            self.send_raw(&dv3000::build_gain(in_db, out_db))?;
            let response = self.recv()?;
            if !dv3000::is_gain_ack(&response) {
                return Err(VocoderError::Init(format!(
                    "expected GAIN ack, got {response:?}"
                )));
            }
            info!("AMBEserver gain set: in={in_db} dB, out={out_db} dB");
        }

        Ok(())
    }

    fn send_raw(&self, packet: &[u8]) -> Result<(), VocoderError> {
        self.socket.send(packet)?;
        Ok(())
    }

    fn recv(&mut self) -> Result<dv3000::Packet, VocoderError> {
        let len = self.socket.recv(&mut self.buf)?;
        let (response, _) = dv3000::parse(&self.buf[..len])?;
        Ok(response)
    }

    fn send_recv(&mut self, packet: &[u8]) -> Result<dv3000::Packet, VocoderError> {
        self.send_raw(packet)?;
        self.recv()
    }
}

impl Vocoder for AmbeServer {
    fn encode(&mut self, pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError> {
        match self.send_recv(&dv3000::build_audio(pcm))? {
            dv3000::Packet::Ambe(frame) => Ok(frame),
            other => Err(VocoderError::Encode(format!(
                "expected AMBE response, got {other:?}"
            ))),
        }
    }

    fn decode(&mut self, ambe: &AmbeFrame) -> Result<PcmFrame, VocoderError> {
        match self.send_recv(&dv3000::build_ambe(ambe))? {
            dv3000::Packet::Audio(samples) => Ok(*samples),
            other => Err(VocoderError::Decode(format!(
                "expected audio response, got {other:?}"
            ))),
        }
    }
}
