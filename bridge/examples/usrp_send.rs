//! Send USRP voice packets from raw PCM on stdin.
//!
//! Reads i16 LE mono samples at 8 kHz from stdin, packs them into
//! USRP voice frames (160 samples / 20 ms each), and sends them to
//! the bridge at 20 ms intervals.  Sends an unkey packet on EOF.
//!
//! Usage:
//!   arecord -f S16_LE -r 8000 -c 1 | cargo run --example usrp_send
//!   cargo run --example usrp_send < /tmp/voice.raw
//!
//! Default target is 127.0.0.1:34001 (the bridge's USRP listen port).

use std::env;
use std::io::Read;
use std::net::UdpSocket;
use std::thread;
use std::time::Duration;

const DEFAULT_TARGET: &str = "127.0.0.1:34001";

const USRP_MAGIC: &[u8; 4] = b"USRP";
const HEADER_SIZE: usize = 32;
const VOICE_SAMPLES: usize = 160;
const FRAME_BYTES: usize = VOICE_SAMPLES * 2;
const PACKET_SIZE: usize = HEADER_SIZE + FRAME_BYTES;

/// 20 ms per frame at 8 kHz.
const FRAME_INTERVAL: Duration = Duration::from_millis(20);

fn build_header(seq: u32, keyup: bool, talkgroup: u32) -> [u8; HEADER_SIZE] {
    let mut hdr = [0u8; HEADER_SIZE];
    hdr[0..4].copy_from_slice(USRP_MAGIC);
    hdr[4..8].copy_from_slice(&seq.to_be_bytes());
    hdr[12..16].copy_from_slice(&(keyup as u32).to_be_bytes());
    hdr[16..20].copy_from_slice(&talkgroup.to_be_bytes());
    // frame_type = 0 (Voice), mpxid/reserved = 0
    hdr
}

fn main() -> anyhow::Result<()> {
    let target = env::args().nth(1).unwrap_or_else(|| DEFAULT_TARGET.into());

    let socket = UdpSocket::bind("0.0.0.0:0")?;
    eprintln!("sending USRP to {target}");

    let mut stdin = std::io::stdin().lock();
    let mut seq: u32 = 0;
    let mut pcm_buf = [0u8; FRAME_BYTES];
    let mut frames_sent: u64 = 0;

    loop {
        match stdin.read_exact(&mut pcm_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }

        let mut pkt = [0u8; PACKET_SIZE];
        pkt[..HEADER_SIZE].copy_from_slice(&build_header(seq, true, 0));
        pkt[HEADER_SIZE..].copy_from_slice(&pcm_buf);
        socket.send_to(&pkt, &target)?;

        seq = seq.wrapping_add(1);
        frames_sent += 1;
        if frames_sent <= 3 || frames_sent.is_multiple_of(50) {
            eprintln!("sent {frames_sent} frames");
        }

        thread::sleep(FRAME_INTERVAL);
    }

    // Send unkey.
    let unkey = build_header(seq, false, 0);
    socket.send_to(&unkey, &target)?;
    eprintln!("sent unkey after {frames_sent} frames");

    Ok(())
}
