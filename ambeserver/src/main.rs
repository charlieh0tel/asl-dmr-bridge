//! UDP <-> AMBE-3000R proxy with one-active-session exclusivity.
//!
//! Each `(srcaddr, srcport)` is a session.  At most one session may
//! drive the backend at a time: while a holder is active (any packet
//! within the last `EXCLUSIVE_HOLD`), other peers' packets are
//! dropped silently and they UDP-time-out cleanly.  When the holder
//! goes idle, the next peer to send a packet takes over.
//!
//! Two backends behind a uniform UDP wire surface:
//!
//! - `--backend thumbdv` (default): byte-for-byte serial relay to a
//!   real DVSI AMBE-3000R chip.  Wire-compatible with OpenDV
//!   ambeserver clients; clients init the chip themselves at startup
//!   (RESET -> RATEP -> optional GAIN).
//! - `--backend dynarmic` / `--backend neural`: in-process software
//!   vocoder.  ambeserver fabricates the chip's responses for control
//!   packets and runs encode/decode through `ambe::Vocoder`.  Refuses
//!   any non-DMR RATEP and poisons the session until the next RESET.
//!
//! Single-threaded sync loop.

use std::net::SocketAddr;
use std::net::UdpSocket;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;
use anyhow::Result;
use clap::Parser;
use dv3000_wire::CONTROL_GAIN;
use dv3000_wire::CONTROL_RATEP;
use dv3000_wire::CONTROL_RESET;
use dv3000_wire::START_BYTE;
use dv3000_wire::TYPE_CONTROL;
use tracing::debug;
use tracing::info;
use tracing::warn;
use tracing_subscriber::EnvFilter;

mod backend;
mod cli;
use backend::Backend;
use cli::Args;

const RECV_BUF: usize = 4096;
/// Minimum gap between a holder's last packet and another peer
/// taking over the backend.  Long enough to bridge inter-frame gaps
/// (50 fps voice = 20 ms) and brief processing pauses; short enough
/// that a crashed client doesn't wedge the backend.
const EXCLUSIVE_HOLD: Duration = Duration::from_secs(1);

/// If the packet is a control packet we know about, return a short
/// human-readable description for the log; otherwise None and we
/// keep quiet.
fn describe_control(buf: &[u8]) -> Option<String> {
    if buf.len() < 5 || buf[0] != START_BYTE || buf[3] != TYPE_CONTROL {
        return None;
    }
    match buf[4] {
        CONTROL_RESET => Some("RESET".to_string()),
        CONTROL_RATEP if buf.len() >= 5 + 12 => {
            let mut payload = [0u8; 12];
            payload.copy_from_slice(&buf[5..5 + 12]);
            let name = dv3000_wire::rates::rate_name(&payload)
                .map(str::to_string)
                .unwrap_or_else(|| format!("custom rcws={payload:02x?}"));
            Some(format!("RATEP {name}"))
        }
        CONTROL_GAIN if buf.len() >= 5 + 2 => {
            Some(format!("GAIN in={}dB out={}dB", buf[5] as i8, buf[6] as i8))
        }
        _ => None,
    }
}

fn make_backend(args: &Args) -> Result<Box<dyn Backend>> {
    match args.backend {
        #[cfg(feature = "thumbdv")]
        cli::Backend::ThumbDV => {
            let serial = args
                .serial
                .as_deref()
                .context("--backend thumbdv requires --serial")?;
            let b = backend::ThumbDvBackend::open(serial, args.baud)?;
            info!(serial = %serial, baud = args.baud, "thumbdv backend opened");
            Ok(Box::new(b))
        }
        #[cfg(not(feature = "thumbdv"))]
        cli::Backend::ThumbDV => {
            anyhow::bail!("thumbdv backend not compiled (build with --features thumbdv)")
        }
        #[cfg(feature = "dynarmic")]
        cli::Backend::Dynarmic => {
            let v = ambe::open_dynarmic();
            info!("dynarmic backend opened");
            Ok(Box::new(backend::SoftBackend::new(v, "dynarmic")))
        }
        #[cfg(not(feature = "dynarmic"))]
        cli::Backend::Dynarmic => {
            anyhow::bail!("dynarmic backend not compiled (build with --features dynarmic)")
        }
        #[cfg(feature = "neural")]
        cli::Backend::Neural => {
            let path = args
                .model_path
                .as_deref()
                .context("--backend neural requires --model-path")?;
            let v = ambe::open_neural(path)?;
            info!(model = %path.display(), "neural backend opened");
            Ok(Box::new(backend::SoftBackend::new(v, "neural")))
        }
        #[cfg(not(feature = "neural"))]
        cli::Backend::Neural => {
            anyhow::bail!("neural backend not compiled (build with --features neural)")
        }
    }
}

fn run(args: Args) -> Result<()> {
    let socket = UdpSocket::bind(&args.listen).with_context(|| format!("bind {}", args.listen))?;
    info!(listen = %args.listen, "listening");

    let mut backend = make_backend(&args)?;
    let mut holder: Option<(SocketAddr, Instant)> = None;
    let mut buf = vec![0u8; RECV_BUF];

    loop {
        let (n, peer) = socket.recv_from(&mut buf)?;
        let pkt = &buf[..n];
        let now = Instant::now();
        let desc = describe_control(pkt);
        // A RESET from any peer is an explicit "I want the chip"
        // handshake -- always honor it.  Otherwise, while a holder is
        // active (within EXCLUSIVE_HOLD), refuse other peers so they
        // don't trample the holder's stream.
        let is_reset = desc.as_deref() == Some("RESET");
        if !is_reset
            && let Some((h, t)) = holder
            && h != peer
            && now.duration_since(t) < EXCLUSIVE_HOLD
        {
            debug!(%peer, holder = %h, "refusing concurrent client");
            continue;
        }
        let prior = holder.map(|(h, _)| h);
        holder = Some((peer, now));
        if prior != Some(peer) {
            info!(%peer, "client took over");
            backend.on_takeover();
        }
        if let Some(desc) = desc {
            info!(%peer, "{desc}");
        }
        match backend.handle(pkt) {
            Ok(Some(resp)) => {
                if let Err(e) = socket.send_to(&resp, peer) {
                    warn!(%peer, "send_to: {e}");
                }
            }
            Ok(None) => {}
            Err(e) => warn!(%peer, "backend handle failed: {e:#}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use dv3000_wire::rates::RATEP_DMR;
    use dv3000_wire::rates::RATEP_RAW;

    fn ratep_packet(rcws: &[u8; 12]) -> Vec<u8> {
        let mut buf = vec![START_BYTE, 0x00, 0x0D, TYPE_CONTROL, CONTROL_RATEP];
        buf.extend_from_slice(rcws);
        buf
    }

    #[test]
    fn describe_reset() {
        let buf = [START_BYTE, 0x00, 0x01, TYPE_CONTROL, CONTROL_RESET];
        assert_eq!(describe_control(&buf).as_deref(), Some("RESET"));
    }

    #[test]
    fn describe_ratep_known_dmr() {
        let buf = ratep_packet(&RATEP_DMR);
        assert_eq!(
            describe_control(&buf).as_deref(),
            Some("RATEP DMR / P25 half-rate (idx 33)"),
        );
    }

    #[test]
    fn describe_ratep_known_raw() {
        let buf = ratep_packet(&RATEP_RAW);
        assert_eq!(
            describe_control(&buf).as_deref(),
            Some("RATEP raw 2450 voice (idx 34)"),
        );
    }

    #[test]
    fn describe_ratep_unknown_falls_back_to_hex() {
        let buf = ratep_packet(&[0xAB; 12]);
        let s = describe_control(&buf).expect("ratep");
        assert!(s.starts_with("RATEP custom rcws="), "got: {s}");
    }

    #[test]
    fn describe_gain() {
        let buf = [
            START_BYTE,
            0x00,
            0x03,
            TYPE_CONTROL,
            CONTROL_GAIN,
            (-3i8) as u8,
            6u8,
        ];
        assert_eq!(
            describe_control(&buf).as_deref(),
            Some("GAIN in=-3dB out=6dB"),
        );
    }

    #[test]
    fn describe_non_control_returns_none() {
        // PKT_AMBE, not a control packet.
        let buf = [START_BYTE, 0x00, 0x0B, 0x01, 0x01, 72];
        assert!(describe_control(&buf).is_none());
    }
}
