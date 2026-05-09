//! Dump USRP voice packets to raw PCM on stdout.
//!
//! Listens for USRP packets and writes i16 LE samples to stdout.
//! Pipe to aplay for immediate playback:
//!
//!   cargo run --example usrp_dump | aplay -f S16_LE -r 8000 -c 1
//!
//! Or redirect to a file for later analysis:
//!
//!   cargo run --example usrp_dump > voice.raw
//!   aplay -f S16_LE -r 8000 -c 1 voice.raw

use std::env;
use std::io::Write;
use std::net::UdpSocket;

use usrp_wire::HEADER_SIZE;
use usrp_wire::PACKET_SIZE;
use usrp_wire::VOICE_SAMPLES;

const DEFAULT_BIND: &str = "127.0.0.1:34002";

/// USRP packet magic.  `usrp_wire`'s constant is private; mirrored
/// here so the example can validate inbound packets without going
/// through `Frame::parse`.
const USRP_MAGIC: &[u8; 4] = b"USRP";

fn main() -> anyhow::Result<()> {
    let bind_addr = env::args().nth(1).unwrap_or_else(|| DEFAULT_BIND.into());

    let socket = UdpSocket::bind(&bind_addr)?;
    eprintln!("listening on {bind_addr}, writing raw PCM to stdout");

    let mut stdout = std::io::stdout().lock();
    let mut buf = [0u8; PACKET_SIZE + 64];

    loop {
        let (len, _addr) = socket.recv_from(&mut buf)?;
        if len < HEADER_SIZE || &buf[..4] != USRP_MAGIC {
            continue;
        }

        let keyup = u32::from_be_bytes(buf[12..16].try_into().unwrap()) != 0;
        if !keyup || len < PACKET_SIZE {
            continue;
        }

        stdout.write_all(&buf[HEADER_SIZE..HEADER_SIZE + VOICE_SAMPLES * 2])?;
    }
}
