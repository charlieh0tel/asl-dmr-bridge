//! UDP transport for DV3000 packets.  Shared by `AmbeServer` (the
//! `Vocoder` impl) and `AmbeServerClient` (the lower-level
//! `ChipClient` impl) so both consumers share one bind / send /
//! recv / parse path.
//!
//! AMBEserver is the only DV3000 transport that uses UDP; serial
//! ThumbDV runs through a different code path entirely (see
//! `crate::thumbdv`).

use std::net::SocketAddr;
use std::net::UdpSocket;
use std::time::Duration;

use dv3000_wire::MAX_PACKET;
use dv3000_wire::Packet;
use dv3000_wire::parse;

use crate::VocoderError;

/// Per-recv blocking timeout.  Big enough to cover BM + AMBEserver
/// hop latencies; small enough that a stuck server fails the call
/// instead of wedging the voice task indefinitely.
const RECV_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct UdpDvTransport {
    socket: UdpSocket,
    buf: Vec<u8>,
}

impl UdpDvTransport {
    pub(crate) fn connect(addr: SocketAddr) -> Result<Self, VocoderError> {
        let bind_addr = match addr {
            SocketAddr::V4(_) => "0.0.0.0:0",
            SocketAddr::V6(_) => "[::]:0",
        };
        let socket = UdpSocket::bind(bind_addr)?;
        socket.connect(addr)?;
        socket.set_read_timeout(Some(RECV_TIMEOUT))?;
        Ok(Self {
            socket,
            buf: vec![0u8; MAX_PACKET],
        })
    }

    pub(crate) fn send_raw(&self, packet: &[u8]) -> Result<(), VocoderError> {
        self.socket.send(packet)?;
        Ok(())
    }

    pub(crate) fn recv(&mut self) -> Result<Packet, VocoderError> {
        let len = self.socket.recv(&mut self.buf)?;
        let (response, _) = parse(&self.buf[..len])?;
        Ok(response)
    }

    pub(crate) fn send_recv(&mut self, packet: &[u8]) -> Result<Packet, VocoderError> {
        self.send_raw(packet)?;
        self.recv()
    }
}
