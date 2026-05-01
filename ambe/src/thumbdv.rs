//! ThumbDV serial backend.
//!
//! Communicates directly with a DVSI AMBE-3000 device (ThumbDV, DV3000,
//! etc.) over USB-serial using the DV3000 packet protocol.

use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use tracing::info;

use crate::AmbeFrame;
use crate::PcmFrame;
use crate::Vocoder;
use crate::VocoderError;
use crate::dv3000;

const DEFAULT_BAUD: u32 = 460_800;
const SERIAL_TIMEOUT: Duration = Duration::from_secs(2);
const REQUIRED_LATENCY_MS: u32 = 1;

/// Check that the FTDI latency_timer is configured for low latency.
/// Since kernel 4.4.52 the default is 16ms, too slow for ThumbDV.
/// We only detect and reject; setting it requires elevated privileges.
#[cfg(target_os = "linux")]
fn check_latency_timer(path: &str) -> Result<(), VocoderError> {
    let dev_name = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let sysfs = format!("/sys/bus/usb-serial/devices/{dev_name}/latency_timer");
    let content = match std::fs::read_to_string(&sysfs) {
        Ok(s) => s,
        Err(_) => return Ok(()), // not an FTDI device or sysfs not available
    };
    let latency: u32 = content.trim().parse().map_err(|e| {
        // Refuse to silently pass: an unparseable latency_timer means
        // the sysfs file is wrong, not that the device is fast.
        VocoderError::Init(format!(
            "{path}: cannot parse {sysfs} contents {:?}: {e}",
            content.trim(),
        ))
    })?;
    if latency > REQUIRED_LATENCY_MS {
        return Err(VocoderError::Init(format!(
            "{path}: FTDI latency_timer is {latency}ms (need {REQUIRED_LATENCY_MS}ms). \
             Fix with: echo {REQUIRED_LATENCY_MS} | sudo tee {sysfs}"
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn check_latency_timer(_path: &str) -> Result<(), VocoderError> {
    Ok(())
}

/// ThumbDV serial vocoder.
pub(crate) struct ThumbDv {
    port: Box<dyn serialport::SerialPort>,
    buf: Vec<u8>,
}

impl ThumbDv {
    /// Open the serial device and initialize for DMR (AMBE+2).
    ///
    /// `gain_db`: optional (input_db, output_db) to apply after RATEP.
    /// Each is clamped to [-90, 90] dB.  `None` leaves the chip at
    /// its default gain (0 dB).
    ///
    /// Checks that the FTDI latency_timer is set to 1 (low latency).
    /// Since kernel 4.4.52 the default is 16ms, which is too slow for
    /// the ThumbDV packet flow.  Fix with:
    ///   echo 1 | sudo tee /sys/bus/usb-serial/devices/ttyUSB0/latency_timer
    /// Reference: https://github.com/f4exb/serialDV
    pub(crate) fn open(
        path: &str,
        baud: Option<u32>,
        gain_db: Option<(i8, i8)>,
    ) -> Result<Self, VocoderError> {
        check_latency_timer(path)?;
        let baud = baud.unwrap_or(DEFAULT_BAUD);
        let port = serialport::new(path, baud)
            .timeout(SERIAL_TIMEOUT)
            .open()
            .map_err(|e| VocoderError::Init(format!("opening {path} at {baud} baud: {e}")))?;

        let mut dv = Self {
            port,
            buf: vec![0u8; dv3000::MAX_PACKET],
        };

        dv.init(gain_db)?;
        Ok(dv)
    }

    fn init(&mut self, gain_db: Option<(i8, i8)>) -> Result<(), VocoderError> {
        self.send_raw(&dv3000::build_reset())?;
        let response = self.recv()?;
        if !dv3000::is_ready(&response) {
            return Err(VocoderError::Init(format!(
                "expected READY after reset, got {response:?}"
            )));
        }
        info!("ThumbDV reset OK");

        self.send_raw(&dv3000::build_prodid())?;
        let response = self.recv()?;
        if let dv3000::Packet::Control { data, .. } = &response {
            let id = String::from_utf8_lossy(data);
            info!("ThumbDV product: {id}");
        }

        self.send_raw(&dv3000::build_ratep_dmr())?;
        let response = self.recv()?;
        if !dv3000::is_ratep_ack(&response) {
            return Err(VocoderError::Init(format!(
                "expected RATEP ack, got {response:?}"
            )));
        }
        info!("ThumbDV configured for DMR");

        if let Some((in_db, out_db)) = gain_db {
            self.send_raw(&dv3000::build_gain(in_db, out_db))?;
            let response = self.recv()?;
            if !dv3000::is_gain_ack(&response) {
                return Err(VocoderError::Init(format!(
                    "expected GAIN ack, got {response:?}"
                )));
            }
            info!("ThumbDV gain set: in={in_db} dB, out={out_db} dB");
        }

        Ok(())
    }

    fn send_raw(&mut self, data: &[u8]) -> Result<(), VocoderError> {
        self.port.write_all(data)?;
        self.port.flush()?;
        Ok(())
    }

    fn recv(&mut self) -> Result<dv3000::Packet, VocoderError> {
        let mut header = [0u8; 4];
        self.port.read_exact(&mut header)?;
        if header[0] != dv3000::START_BYTE {
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
}

impl Vocoder for ThumbDv {
    fn encode(&mut self, pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError> {
        self.send_raw(&dv3000::build_audio(pcm))?;
        match self.recv()? {
            dv3000::Packet::Ambe(frame) => Ok(frame),
            other => Err(VocoderError::Encode(format!(
                "expected AMBE response, got {other:?}"
            ))),
        }
    }

    fn decode(&mut self, ambe: &AmbeFrame) -> Result<PcmFrame, VocoderError> {
        self.send_raw(&dv3000::build_ambe(ambe))?;
        match self.recv()? {
            dv3000::Packet::Audio(samples) => Ok(*samples),
            other => Err(VocoderError::Decode(format!(
                "expected audio response, got {other:?}"
            ))),
        }
    }
}
