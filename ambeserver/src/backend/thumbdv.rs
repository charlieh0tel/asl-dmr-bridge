//! Byte-for-byte serial relay to a real DVSI AMBE-3000R chip.
//! Doesn't parse; the chip is the source of truth for protocol
//! semantics.

use std::io::Read;
use std::io::Write;
use std::time::Duration;

use anyhow::Result;
use anyhow::ensure;
use dv3000_wire::START_BYTE;

use super::Backend;

const SERIAL_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) struct ThumbDvBackend {
    port: Box<dyn serialport::SerialPort>,
}

impl ThumbDvBackend {
    pub(crate) fn open(path: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(path, baud)
            .timeout(SERIAL_TIMEOUT)
            .open()
            .map_err(|e| anyhow::anyhow!("open {path} at {baud} baud: {e}"))?;
        port.clear(serialport::ClearBuffer::All)?;
        Ok(Self { port })
    }
}

impl Backend for ThumbDvBackend {
    fn handle(&mut self, request: &[u8]) -> Result<Option<Vec<u8>>> {
        self.port.write_all(request)?;
        self.port.flush()?;
        let mut header = [0u8; 4];
        self.port.read_exact(&mut header)?;
        ensure!(
            header[0] == START_BYTE,
            "chip: bad start byte {:#x}",
            header[0]
        );
        let payload_len = u16::from_be_bytes([header[1], header[2]]) as usize;
        let mut buf = vec![0u8; 4 + payload_len];
        buf[..4].copy_from_slice(&header);
        self.port.read_exact(&mut buf[4..])?;
        Ok(Some(buf))
    }
}
