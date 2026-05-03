//! Multi-tenant UDP-to-serial proxy for the DVSI AMBE-3000R chip.
//!
//! Each `(srcaddr, srcport)` is a session.  Per-session state tracks
//! the last `RATEP` and `GAIN` control words the client set; the
//! server applies them to the chip on demand before forwarding the
//! client's next data packet, so two clients with different rates can
//! share the chip transparently.  Wire-compatible with OpenDV
//! `ambeserver` clients.
//!
//! Single-threaded sync loop: one chip in flight at a time.  Sessions
//! age out after `IDLE_TIMEOUT` of inactivity.

use std::collections::HashMap;
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
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const GC_INTERVAL: Duration = Duration::from_secs(60);

const START_BYTE: u8 = 0x61;
const TYPE_CONTROL: u8 = 0x00;
const CONTROL_RATEP: u8 = 0x0A;
const CONTROL_GAIN: u8 = 0x4B;

#[derive(Parser)]
#[command(about = "Multi-tenant UDP <-> AMBE-3000R serial proxy")]
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

struct Session {
    /// 12 bytes of the last RATEP control payload (RCW0..RCW5) the
    /// client set.  None until the client first sends one; thereafter
    /// the server re-applies it whenever another client has perturbed
    /// the chip.
    ratep: Option<[u8; 12]>,
    /// 2 bytes (in/out gain) from the last GAIN control packet.
    gain: Option<[u8; 2]>,
    last_seen: Instant,
}

impl Session {
    fn new() -> Self {
        Self {
            ratep: None,
            gain: None,
            last_seen: Instant::now(),
        }
    }
}

struct Chip {
    port: Box<dyn serialport::SerialPort>,
    /// Cache of what's currently programmed on the chip; avoids
    /// re-issuing identical RATEP/GAIN.
    ratep: Option<[u8; 12]>,
    gain: Option<[u8; 2]>,
}

impl Chip {
    fn open(path: &str, baud: u32) -> Result<Self> {
        let port = serialport::new(path, baud)
            .timeout(SERIAL_TIMEOUT)
            .open()
            .with_context(|| format!("open {path} at {baud} baud"))?;
        port.clear(serialport::ClearBuffer::All)?;
        Ok(Self {
            port,
            ratep: None,
            gain: None,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.port.write_all(bytes)?;
        self.port.flush()?;
        Ok(())
    }

    fn read_packet(&mut self) -> Result<Vec<u8>> {
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

    /// Send one packet, read one response.  Discards the response if
    /// the caller doesn't need it (used for chip-resync RATEP / GAIN).
    fn round_trip(&mut self, request: &[u8]) -> Result<Vec<u8>> {
        self.write(request)?;
        self.read_packet()
    }

    /// Make sure the chip's RATEP / GAIN match the session's, sending
    /// fresh control packets if they don't.  Replies from the chip are
    /// drained but not forwarded -- they are server-side housekeeping.
    fn sync_to(&mut self, session: &Session) -> Result<()> {
        if let Some(ratep) = session.ratep
            && self.ratep != Some(ratep)
        {
            let mut pkt = vec![START_BYTE, 0x00, 0x0D, TYPE_CONTROL, CONTROL_RATEP];
            pkt.extend_from_slice(&ratep);
            pkt[1..3].copy_from_slice(&((1 + ratep.len()) as u16).to_be_bytes());
            self.round_trip(&pkt).context("chip: RATEP resync")?;
            self.ratep = Some(ratep);
            debug!("chip RATEP resynced");
        }
        if let Some(gain) = session.gain
            && self.gain != Some(gain)
        {
            let pkt = vec![
                START_BYTE,
                0x00,
                0x03,
                TYPE_CONTROL,
                CONTROL_GAIN,
                gain[0],
                gain[1],
            ];
            self.round_trip(&pkt).context("chip: GAIN resync")?;
            self.gain = Some(gain);
            debug!("chip GAIN resynced");
        }
        Ok(())
    }
}

/// Classify the packet for session bookkeeping.  Returns `None` if
/// the packet is too short to look at; the caller forwards it
/// verbatim regardless.
fn classify(buf: &[u8]) -> PacketKind {
    if buf.len() >= 5 && buf[0] == START_BYTE && buf[3] == TYPE_CONTROL {
        match buf[4] {
            CONTROL_RATEP if buf.len() >= 5 + 12 => {
                let mut payload = [0u8; 12];
                payload.copy_from_slice(&buf[5..5 + 12]);
                return PacketKind::Ratep(payload);
            }
            CONTROL_GAIN if buf.len() >= 5 + 2 => {
                return PacketKind::Gain([buf[5], buf[6]]);
            }
            _ => return PacketKind::OtherControl,
        }
    }
    PacketKind::Data
}

enum PacketKind {
    Ratep([u8; 12]),
    Gain([u8; 2]),
    OtherControl,
    Data,
}

fn run(args: Args) -> Result<()> {
    let socket = UdpSocket::bind(&args.listen).with_context(|| format!("bind {}", args.listen))?;
    info!(listen = %args.listen, "listening");

    let mut chip = Chip::open(&args.serial, args.baud)?;
    info!(serial = %args.serial, baud = args.baud, "chip opened");

    let mut sessions: HashMap<SocketAddr, Session> = HashMap::new();
    let mut last_gc = Instant::now();
    let mut buf = vec![0u8; RECV_BUF];

    loop {
        let (n, peer) = socket.recv_from(&mut buf)?;
        let pkt = &buf[..n];
        let session = sessions.entry(peer).or_insert_with(Session::new);
        session.last_seen = Instant::now();

        match classify(pkt) {
            PacketKind::Ratep(payload) => {
                session.ratep = Some(payload);
                let ack = [START_BYTE, 0x00, 0x02, TYPE_CONTROL, CONTROL_RATEP, 0x00];
                socket.send_to(&ack, peer)?;
                debug!(%peer, "session RATEP set");
            }
            PacketKind::Gain(payload) => {
                session.gain = Some(payload);
                let ack = [START_BYTE, 0x00, 0x01, TYPE_CONTROL, CONTROL_GAIN];
                socket.send_to(&ack, peer)?;
                debug!(%peer, "session GAIN set");
            }
            PacketKind::OtherControl | PacketKind::Data => {
                let session = sessions.get(&peer).expect("just inserted");
                if let Err(e) = chip.sync_to(session) {
                    warn!(%peer, "chip resync failed: {e:#}");
                    continue;
                }
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

        if last_gc.elapsed() >= GC_INTERVAL {
            let now = Instant::now();
            let before = sessions.len();
            sessions.retain(|_, s| now.duration_since(s.last_seen) < IDLE_TIMEOUT);
            let after = sessions.len();
            if before != after {
                debug!(removed = before - after, remaining = after, "session GC");
            }
            last_gc = now;
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
