//! AMBEserver UDP client backend.
//!
//! Connects to an existing AMBEserver daemon that proxies DV3000
//! packets over UDP. Default port 2460.

use std::net::SocketAddr;

use tracing::info;

use crate::Vocoder;
use crate::VocoderError;
use crate::udp_dv::UdpDvTransport;
use dv3000_wire::AmbeFrame;
use dv3000_wire::CONTROL_GAIN;
use dv3000_wire::Packet;
use dv3000_wire::PcmFrame;
use dv3000_wire::build_ambe;
use dv3000_wire::build_ambe_lost_frame;
use dv3000_wire::build_audio;
use dv3000_wire::build_gain;
use dv3000_wire::build_ratep_dmr;
use dv3000_wire::build_reset;
use dv3000_wire::is_ratep_ack;
use dv3000_wire::is_ready;

/// AMBEserver UDP client.
pub(crate) struct AmbeServer {
    transport: UdpDvTransport,
}

impl AmbeServer {
    /// Connect to an AMBEserver proxy and initialize the chip for DMR.
    pub(crate) fn connect(addr: SocketAddr) -> Result<Self, VocoderError> {
        let mut server = Self {
            transport: UdpDvTransport::connect(addr)?,
        };
        server.init()?;
        info!("connected to AMBEserver at {addr}");
        Ok(server)
    }

    fn init(&mut self) -> Result<(), VocoderError> {
        let response = self.transport.send_recv(&build_reset())?;
        if !is_ready(&response) {
            return Err(VocoderError::Init(format!(
                "expected READY after reset, got {response:?}"
            )));
        }

        let response = self.transport.send_recv(&build_ratep_dmr())?;
        if !is_ratep_ack(&response) {
            return Err(VocoderError::Init(format!(
                "expected RATEP ack, got {response:?}"
            )));
        }

        Ok(())
    }
}

impl Vocoder for AmbeServer {
    fn encode(&mut self, pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError> {
        match self.transport.send_recv(&build_audio(pcm))? {
            Packet::Ambe(frame) => Ok(frame),
            other => Err(VocoderError::Encode(format!(
                "expected AMBE response, got {other:?}"
            ))),
        }
    }

    fn decode(&mut self, ambe: Option<&AmbeFrame>) -> Result<PcmFrame, VocoderError> {
        // None -> CMODE LOST_FRAME packet so the chip ignores the
        // (placeholder) channel data and emits a predictor frame-
        // repeat (AMBE-3000R Users Manual sec. 6.9, bit 2 = LOST_FRAME).
        let pkt = match ambe {
            Some(ambe) => build_ambe(ambe),
            None => build_ambe_lost_frame(),
        };
        match self.transport.send_recv(&pkt)? {
            Packet::Audio(samples) => Ok(*samples),
            other => Err(VocoderError::Decode(format!(
                "expected audio response, got {other:?}"
            ))),
        }
    }

    // [TODO] @charlieh0tel: send chip RESET if field testing shows
    // audible inter-stream artifacts; replay RATEP+gain after.  Until
    // then the trait's default no-op reset is correct.

    fn set_gain(&mut self, in_db: dsp::dB, out_db: dsp::dB) -> Result<(), VocoderError> {
        let in_byte = in_db.to_chip_byte();
        let out_byte = out_db.to_chip_byte();
        match self.transport.send_recv(&build_gain(in_byte, out_byte))? {
            Packet::Control { field_id, .. } if field_id == CONTROL_GAIN => {
                tracing::info!(
                    "AMBEserver gain set: in={} dB, out={} dB",
                    in_byte,
                    out_byte
                );
                Ok(())
            }
            other => Err(VocoderError::Init(format!(
                "expected GAIN ack, got {other:?}"
            ))),
        }
    }
}
