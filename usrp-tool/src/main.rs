//! Send, receive, or play USRP voice frames.
//!
//! Subcommands:
//!   send  -- read raw PCM or WAV, transmit as USRP frames to UDP or stdout
//!   recv  -- receive USRP frames from UDP; write PCM/WAV or play via audio device

use std::collections::VecDeque;
use std::io::Write as _;
use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use usrp_wire::{Frame, FrameType, PACKET_SIZE, RECV_SLACK, VOICE_FRAME_INTERVAL, VOICE_SAMPLES};

const DEFAULT_BIND: &str = "127.0.0.1:34002";
const DEFAULT_TARGET: &str = "127.0.0.1:34001";
const DEFAULT_FROM_PORT: u16 = 34002;

/// Two prebuffered frames (40 ms) absorb UDP jitter without audible keyup delay.
const PREBUFFER_SAMPLES: usize = VOICE_SAMPLES * 2;

#[derive(Parser)]
#[command(name = "usrp-tool", about = "Send/receive/play USRP voice frames")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Send raw PCM or WAV as USRP voice frames.
    Send(SendArgs),
    /// Receive USRP voice frames; write PCM/WAV or play via audio device.
    Recv(RecvArgs),
}

#[derive(clap::Args)]
struct SendArgs {
    /// Input file (.wav auto-detected; omit for raw S16LE 8 kHz from stdin).
    input: Option<PathBuf>,
    /// Destination: host:port, or "-" to write USRP wire frames to stdout.
    #[arg(long, default_value = DEFAULT_TARGET)]
    to: String,
    /// Source port for UDP (bridge whitelists by source port).
    #[arg(long, default_value_t = DEFAULT_FROM_PORT)]
    from_port: u16,
    /// Talkgroup embedded in USRP header.
    #[arg(long, default_value_t = 0)]
    talkgroup: u32,
}

#[derive(clap::Args)]
struct RecvArgs {
    /// Output: file path (.wav auto-detected), "-", or omit for raw S16LE
    /// 8 kHz PCM to stdout.  Mutually exclusive with --device.
    #[arg(conflicts_with = "device")]
    output: Option<PathBuf>,
    /// Bind address.
    #[arg(long, default_value = DEFAULT_BIND)]
    bind: String,
    /// Play through an audio device.  Omit the value for the system default;
    /// supply a name from --list-devices for a specific device.
    #[arg(long, value_name = "DEVICE", num_args = 0..=1, default_missing_value = "")]
    device: Option<String>,
    /// List available audio output devices and exit.
    #[arg(long)]
    list_devices: bool,
}

fn has_wav_extension(p: &Path) -> bool {
    p.extension()
        .map(|e| e.eq_ignore_ascii_case("wav"))
        .unwrap_or(false)
}

fn is_stdout_path(p: &Path) -> bool {
    p.as_os_str() == "-"
}

// ---- send ---------------------------------------------------------------

/// Reads 160-sample frames from a WAV file.  Partial final frames are
/// zero-padded to a full frame rather than dropped.
struct WavFrameIter {
    samples: hound::WavIntoSamples<std::io::BufReader<std::fs::File>, i16>,
}

impl Iterator for WavFrameIter {
    type Item = Result<[i16; VOICE_SAMPLES]>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut frame = [0i16; VOICE_SAMPLES];
        let mut filled = 0;
        for slot in frame.iter_mut() {
            match self.samples.next() {
                Some(Ok(s)) => {
                    *slot = s;
                    filled += 1;
                }
                Some(Err(e)) => return Some(Err(e.into())),
                None => break,
            }
        }
        if filled == 0 { None } else { Some(Ok(frame)) }
    }
}

/// Reads 160-sample frames from raw S16LE PCM (a file or stdin).
struct RawFrameIter {
    reader: Box<dyn std::io::Read>,
    buf: [u8; VOICE_SAMPLES * 2],
}

impl Iterator for RawFrameIter {
    type Item = Result<[i16; VOICE_SAMPLES]>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.reader.read_exact(&mut self.buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return None,
            Err(e) => return Some(Err(e.into())),
        }
        let mut frame = [0i16; VOICE_SAMPLES];
        for (i, slot) in frame.iter_mut().enumerate() {
            let bytes = self.buf[i * 2..i * 2 + 2].try_into().unwrap();
            *slot = i16::from_ne_bytes(bytes);
        }
        Some(Ok(frame))
    }
}

fn open_send_frames(
    input: Option<&Path>,
) -> Result<Box<dyn Iterator<Item = Result<[i16; VOICE_SAMPLES]>>>> {
    if input.map(has_wav_extension).unwrap_or(false) {
        let path = input.unwrap();
        let reader = WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
        let spec = reader.spec();
        if spec.sample_rate != 8000 {
            bail!("WAV sample rate {} Hz != 8000 Hz", spec.sample_rate);
        }
        if spec.channels != 1 {
            bail!("WAV channels {} != 1 (mono required)", spec.channels);
        }
        if spec.sample_format != SampleFormat::Int || spec.bits_per_sample != 16 {
            bail!(
                "WAV must be S16; got {:?} {}bps",
                spec.sample_format,
                spec.bits_per_sample
            );
        }
        Ok(Box::new(WavFrameIter {
            samples: reader.into_samples::<i16>(),
        }))
    } else {
        let reader: Box<dyn std::io::Read> = match input {
            Some(p) => {
                Box::new(std::fs::File::open(p).with_context(|| format!("open {}", p.display()))?)
            }
            None => {
                eprintln!("reading raw S16LE 8 kHz PCM from stdin");
                Box::new(std::io::stdin())
            }
        };
        Ok(Box::new(RawFrameIter {
            reader,
            buf: [0u8; VOICE_SAMPLES * 2],
        }))
    }
}

fn cmd_send(args: SendArgs) -> Result<()> {
    let frames = open_send_frames(args.input.as_deref())?;
    let to_stdout = args.to == "-";

    let mut seq: u32 = 0;
    let mut frames_sent: u64 = 0;

    if to_stdout {
        eprintln!("writing USRP wire frames to stdout");
        let mut out = std::io::stdout();
        for result in frames {
            let audio = result?;
            let frame = Frame {
                seq,
                keyup: true,
                talkgroup: args.talkgroup,
                frame_type: FrameType::Voice,
                audio: Some(audio),
                text: None,
            };
            out.write_all(&frame.serialize(false))?;
            seq = seq.wrapping_add(1);
            frames_sent += 1;
        }
        let unkey = Frame {
            seq,
            keyup: false,
            talkgroup: args.talkgroup,
            frame_type: FrameType::Voice,
            audio: None,
            text: None,
        };
        out.write_all(&unkey.serialize(false))?;
    } else {
        let socket = UdpSocket::bind(("0.0.0.0", args.from_port))
            .with_context(|| format!("bind port {}", args.from_port))?;
        eprintln!("sending USRP to {} from port {}", args.to, args.from_port);

        for result in frames {
            let audio = result?;
            let frame = Frame {
                seq,
                keyup: true,
                talkgroup: args.talkgroup,
                frame_type: FrameType::Voice,
                audio: Some(audio),
                text: None,
            };
            socket
                .send_to(&frame.serialize(false), &args.to)
                .context("send_to")?;
            seq = seq.wrapping_add(1);
            frames_sent += 1;
            if frames_sent <= 3 || frames_sent.is_multiple_of(50) {
                eprintln!("sent {} frames", frames_sent);
            }
            thread::sleep(VOICE_FRAME_INTERVAL);
        }

        let unkey = Frame {
            seq,
            keyup: false,
            talkgroup: args.talkgroup,
            frame_type: FrameType::Voice,
            audio: None,
            text: None,
        };
        socket
            .send_to(&unkey.serialize(false), &args.to)
            .context("send unkey")?;
    }

    eprintln!("sent unkey after {} frames", frames_sent);
    Ok(())
}

// ---- recv ---------------------------------------------------------------

const WAV_SPEC: WavSpec = WavSpec {
    channels: 1,
    sample_rate: 8000,
    bits_per_sample: 16,
    sample_format: SampleFormat::Int,
};

fn device_display_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "(unknown)".into())
}

/// Resolve an audio output device by name.  Empty name returns the default.
fn open_output_device(name: &str) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if name.is_empty() {
        return host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no default output audio device"));
    }
    for device in host.output_devices().context("enumerate output devices")? {
        if device_display_name(&device) == name {
            return Ok(device);
        }
    }
    bail!("audio device {:?} not found; use --list-devices", name);
}

fn list_audio_devices() -> Result<()> {
    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .map(|d| device_display_name(&d));
    for device in host.output_devices().context("enumerate output devices")? {
        let name = device_display_name(&device);
        let marker = if Some(&name) == default_name.as_ref() {
            " (default)"
        } else {
            ""
        };
        println!("{name}{marker}");
    }
    Ok(())
}

enum RecvOutput {
    Pcm(Box<dyn std::io::Write>),
    Wav(WavWriter<std::io::BufWriter<std::fs::File>>),
    /// Audio device mode; stream is held in a separate binding for lifetime.
    Device,
}

/// Playback state shared between the UDP receive thread and the cpal callback.
struct Playback {
    buffer: VecDeque<i16>,
    armed: bool,
}

fn cmd_recv(args: RecvArgs) -> Result<()> {
    if args.list_devices {
        return list_audio_devices();
    }

    // Build output before binding the socket so misconfiguration fails fast.
    // `_stream` is held here to keep the audio stream alive for the loop's
    // duration; it is not otherwise accessed after play() is called.
    let (out, playback, _stream) = if let Some(device_name) = &args.device {
        let device = open_output_device(device_name)?;
        eprintln!("output device: {}", device_display_name(&device));
        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: 8000,
            buffer_size: cpal::BufferSize::Default,
        };
        let pb = Arc::new(Mutex::new(Playback {
            buffer: VecDeque::with_capacity(VOICE_SAMPLES * 20),
            armed: false,
        }));
        let cb_pb = pb.clone();
        let stream = device.build_output_stream(
            &config,
            move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
                let mut state = cb_pb.lock().unwrap();
                if state.armed {
                    for sample in data.iter_mut() {
                        *sample = state.buffer.pop_front().unwrap_or(0);
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
        (
            RecvOutput::Device,
            Some(pb),
            Some(Box::new(stream) as Box<dyn StreamTrait>),
        )
    } else {
        let output = args.output.as_deref();
        let use_wav = output
            .map(|p| has_wav_extension(p) && !is_stdout_path(p))
            .unwrap_or(false);
        if use_wav {
            let path = output.unwrap();
            let writer = WavWriter::create(path, WAV_SPEC)
                .with_context(|| format!("create {}", path.display()))?;
            eprintln!("writing WAV to {}", path.display());
            (RecvOutput::Wav(writer), None, None)
        } else {
            let writer: Box<dyn std::io::Write> = match output {
                Some(p) if !is_stdout_path(p) => Box::new(
                    std::fs::File::create(p).with_context(|| format!("create {}", p.display()))?,
                ),
                _ => {
                    eprintln!("writing raw S16LE 8 kHz PCM to stdout");
                    Box::new(std::io::stdout())
                }
            };
            (RecvOutput::Pcm(writer), None, None)
        }
    };

    let socket = UdpSocket::bind(&args.bind).with_context(|| format!("bind {}", args.bind))?;
    eprintln!("listening on {}", args.bind);

    let mut buf = [0u8; PACKET_SIZE + RECV_SLACK];
    let mut voice_frames: u64 = 0;
    let mut out = out;

    loop {
        let (len, _) = match socket.recv_from(&mut buf) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => break,
            Err(e) => return Err(e.into()),
        };

        let frame = match Frame::parse(&buf[..len], false) {
            Ok(f) => f,
            Err(_) => continue,
        };

        if frame.frame_type != FrameType::Voice {
            continue;
        }

        if !frame.keyup {
            eprintln!("unkey (wrote {} frames)", voice_frames);
            voice_frames = 0;
            if let Some(pb) = &playback {
                let mut state = pb.lock().unwrap();
                state.buffer.clear();
                state.armed = false;
            }
            continue;
        }

        let Some(samples) = frame.audio else {
            continue;
        };

        voice_frames += 1;

        match &mut out {
            RecvOutput::Pcm(w) => {
                let mut bytes = [0u8; VOICE_SAMPLES * 2];
                for (i, s) in samples.iter().enumerate() {
                    bytes[i * 2..i * 2 + 2].copy_from_slice(&s.to_ne_bytes());
                }
                w.write_all(&bytes)?;
            }
            RecvOutput::Wav(w) => {
                for s in &samples {
                    w.write_sample(*s)?;
                }
            }
            RecvOutput::Device => {
                if let Some(pb) = &playback {
                    let mut state = pb.lock().unwrap();
                    state.buffer.extend(samples.iter());
                    if !state.armed && state.buffer.len() >= PREBUFFER_SAMPLES {
                        state.armed = true;
                    }
                }
            }
        }
    }

    if let RecvOutput::Wav(w) = out {
        w.finalize()?;
    }

    Ok(())
}

// ---- main ---------------------------------------------------------------

fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Send(args) => cmd_send(args),
        Cmd::Recv(args) => cmd_recv(args),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
