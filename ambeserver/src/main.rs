//! UDP-to-serial proxy for the DVSI AMBE-3000R chip with one-active-
//! session exclusivity.
//!
//! Each `(srcaddr, srcport)` is a session.  At most one session may
//! drive the chip at a time: while a holder is active (any packet
//! within the last `EXCLUSIVE_HOLD`), other peers' packets are
//! dropped silently and they UDP-time-out cleanly.  When the holder
//! goes idle, the next peer to send a packet takes over.
//!
//! Dumb relay: every accepted packet is forwarded to the chip
//! verbatim, the response goes back to the same peer.  No per-
//! session `RATEP` / `GAIN` bookkeeping -- clients are expected to
//! init the chip themselves at startup (the OpenDV-protocol
//! convention is `RESET` -> `RATEP` -> optional `GAIN`).  Wire-
//! compatible with OpenDV ambeserver clients.
//!
//! Single-threaded sync loop.

use std::io::Read;
use std::io::Write;
use std::net::SocketAddr;
use std::net::UdpSocket;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use tracing::debug;
use tracing::info;
use tracing::warn;
use tracing_subscriber::EnvFilter;

const DEFAULT_LISTEN: &str = "0.0.0.0:2460";
const DEFAULT_BAUD: u32 = 460_800;
const SERIAL_TIMEOUT: Duration = Duration::from_secs(2);
const RECV_BUF: usize = 4096;
/// Minimum gap between a holder's last packet and another peer
/// taking over the chip.  Long enough to bridge inter-frame gaps
/// (50 fps voice = 20 ms) and brief processing pauses; short
/// enough that a crashed client doesn't wedge the chip.
const EXCLUSIVE_HOLD: Duration = Duration::from_secs(1);

const START_BYTE: u8 = 0x61;

#[derive(Parser)]
#[command(about = "UDP <-> AMBE-3000R serial proxy with one-holder exclusivity")]
struct Args {
    /// Serial device path (e.g. /dev/ttyUSB0).
    #[arg(long)]
    serial: String,
    /// Baud rate.
    #[arg(long, default_value_t = DEFAULT_BAUD)]
    baud: u32,
    /// UDP listen address.
    #[arg(long, default_value = DEFAULT_LISTEN)]
    listen: String,
}

struct Chip {
    port: Box<dyn serialport::SerialPort>,
}

impl Chip {
    fn open(path: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(path, baud)
            .timeout(SERIAL_TIMEOUT)
            .open()
            .with_context(|| format!("open {path} at {baud} baud"))?;
        port.clear(serialport::ClearBuffer::All)?;
        Ok(Self { port })
    }

    fn round_trip(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        self.port.write_all(request)?;
        self.port.flush()?;
        let mut header = [0u8; 4];
        self.port.read_exact(&mut header)?;
        anyhow::ensure!(
            header[0] == START_BYTE,
            "chip: bad start byte {:#x}",
            header[0]
        );
        let payload_len = u16::from_be_bytes([header[1], header[2]]) as usize;
        let mut buf = vec![0u8; 4 + payload_len];
        buf[..4].copy_from_slice(&header);
        self.port.read_exact(&mut buf[4..])?;
        Ok(buf)
    }
}

fn run(args: Args) -> Result<()> {
    let socket = UdpSocket::bind(&args.listen).with_context(|| format!("bind {}", args.listen))?;
    info!(listen = %args.listen, "listening");

    let mut chip = Chip::open(&args.serial, args.baud)?;
    info!(serial = %args.serial, baud = args.baud, "chip opened");

    let mut holder: Option<(SocketAddr, Instant)> = None;
    let mut buf = vec![0u8; RECV_BUF];

    loop {
        let (n, peer) = socket.recv_from(&mut buf)?;
        let pkt = &buf[..n];
        let now = Instant::now();
        if let Some((h, t)) = holder
            && h != peer
            && now.duration_since(t) < EXCLUSIVE_HOLD
        {
            debug!(%peer, holder = %h, "refusing concurrent client");
            continue;
        }
        holder = Some((peer, now));
        match chip.round_trip(pkt) {
            Ok(resp) => {
                if let Err(e) = socket.send_to(&resp, peer) {
                    warn!(%peer, "send_to: {e}");
                }
            }
            Err(e) => warn!(%peer, "chip round trip failed: {e:#}"),
        }
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    run(Args::parse())
}
