//! Play USRP voice packets to the default audio output.
//!
//! Listens for USRP packets on a UDP port and plays the PCM audio
//! through the system's default output device (ALSA/PipeWire/etc.
//! via cpal).  Requests 8 kHz mono; fails if the device does not
//! support it.
//!
//! A small prebuffer absorbs UDP arrival jitter: the audio callback
//! outputs silence until the incoming channel has PREBUFFER_SAMPLES
//! queued, then plays steadily.  Unkey drains and re-arms so each new
//! transmission starts with a fresh prebuffer.  This is equivalent in
//! spirit to what a real Asterisk channel driver / jitterbuffer does
//! on the receiving end of an RTP-style UDP audio stream.
//!
//! Usage:
//!   cargo run --example usrp_play -- [bind_addr]
//!
//! Default bind address is 127.0.0.1:34002 (the default USRP remote
//! port in config.example.toml).

use std::collections::VecDeque;
use std::env;
use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::Mutex;

use cpal::traits::DeviceTrait;
use cpal::traits::HostTrait;
use cpal::traits::StreamTrait;

const SAMPLE_RATE: u32 = 8000;
const CHANNELS: u16 = 1;
const DEFAULT_BIND: &str = "127.0.0.1:34002";

/// USRP constants (mirrored from src/usrp.rs).
const USRP_MAGIC: &[u8; 4] = b"USRP";
const USRP_HEADER_SIZE: usize = 32;
const VOICE_SAMPLES: usize = 160;
const USRP_PACKET_SIZE: usize = USRP_HEADER_SIZE + VOICE_SAMPLES * 2;

/// Prebuffer depth before playback starts, in samples.  2 frames =
/// 40 ms of headroom; absorbs typical BM UDP arrival jitter without
/// adding perceptible keyup latency.
const PREBUFFER_SAMPLES: usize = VOICE_SAMPLES * 2;

/// Playback state shared between the UDP receive thread and the
/// cpal audio callback.  The callback outputs silence while `armed`
/// is false (prebuffering); flips to true once `buffer` reaches
/// PREBUFFER_SAMPLES; resets to false on unkey.
struct Playback {
    buffer: VecDeque<i16>,
    armed: bool,
}

fn main() -> anyhow::Result<()> {
    let bind_addr = env::args().nth(1).unwrap_or_else(|| DEFAULT_BIND.into());

    let socket = UdpSocket::bind(&bind_addr)?;
    eprintln!("listening on {bind_addr}");

    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no output audio device"))?;
    eprintln!("output device: {:?}", device.description());

    let config = cpal::StreamConfig {
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        buffer_size: cpal::BufferSize::Default,
    };

    let playback = Arc::new(Mutex::new(Playback {
        buffer: VecDeque::with_capacity(VOICE_SAMPLES * 20),
        armed: false,
    }));

    let cb_pb = playback.clone();
    let stream = device.build_output_stream(
        &config,
        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
            let mut pb = cb_pb.lock().unwrap();
            if pb.armed {
                for sample in data.iter_mut() {
                    *sample = pb.buffer.pop_front().unwrap_or(0);
                }
            } else {
                data.fill(0);
            }
        },
        |err| eprintln!("audio error: {err}"),
        None,
    )?;
    stream.play()?;
    eprintln!("playing 8 kHz mono (ctrl-c to stop)");

    let mut buf = [0u8; USRP_PACKET_SIZE + 64];
    let mut voice_count: u64 = 0;
    loop {
        let (len, _addr) = socket.recv_from(&mut buf)?;
        if len < USRP_HEADER_SIZE || &buf[..4] != USRP_MAGIC {
            continue;
        }

        let seq = u32::from_be_bytes(buf[4..8].try_into().unwrap());
        let keyup = u32::from_be_bytes(buf[12..16].try_into().unwrap()) != 0;

        if !keyup || len < USRP_PACKET_SIZE {
            if !keyup {
                eprintln!("seq={seq} unkey (voice_count={voice_count})");
                voice_count = 0;
                let mut pb = playback.lock().unwrap();
                pb.buffer.clear();
                pb.armed = false;
            }
            continue;
        }

        voice_count += 1;
        if voice_count <= 3 || voice_count.is_multiple_of(30) {
            let s0 = i16::from_ne_bytes(
                buf[USRP_HEADER_SIZE..USRP_HEADER_SIZE + 2]
                    .try_into()
                    .unwrap(),
            );
            eprintln!("seq={seq} voice #{voice_count} samples[0]={s0}");
        }

        let mut pb = playback.lock().unwrap();
        for i in 0..VOICE_SAMPLES {
            let offset = USRP_HEADER_SIZE + i * 2;
            let sample = i16::from_ne_bytes(buf[offset..offset + 2].try_into().unwrap());
            pb.buffer.push_back(sample);
        }
        if !pb.armed && pb.buffer.len() >= PREBUFFER_SAMPLES {
            pb.armed = true;
        }
    }
}
