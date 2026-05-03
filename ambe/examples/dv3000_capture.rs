//! Capture (PCM, coded_72, raw_49) triples from a DVSI AMBE-3000R chip
//! for AMBE+2 channel-coding research.
//!
//! Encodes the same PCM stream through the chip twice:
//!
//! 1. Rate index 33 (DMR / P25 half-rate, 2450 voice + 1150 FEC) ->
//!    9-byte channel-coded frames.
//! 2. Rate index 34 (raw 2450 voice, 0 FEC) -> 7-byte raw codec
//!    frames carrying the pre-FEC 49-bit speech bits.
//!
//! Output: three sibling files alongside the input,
//!
//!   <prefix>.pcm        copy of the input stream (sanity)
//!   <prefix>.coded72    concatenated 9-byte channel-coded frames
//!   <prefix>.raw49      concatenated 7-byte raw codec frames
//!
//! Each frame slot at index `i` represents the same 20 ms of audio in
//! both `.coded72` and `.raw49`, so `(raw49[i], coded72[i])` is one
//! golden pair.
//!
//! Usage:
//!
//!   cargo run -p ambe --features thumbdv --example dv3000_capture -- \
//!     /dev/ttyUSB0 input.pcm output_prefix
//!
//! Requires the `thumbdv` feature.  Self-contained DV3000 protocol
//! handling -- bypasses the production `Vocoder` trait so it can switch
//! RATEP between passes without disturbing other crate consumers.

use std::env;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const PCM_SAMPLES: usize = 160;
const PCM_FRAME_BYTES: usize = PCM_SAMPLES * 2;

const START_BYTE: u8 = 0x61;
const TYPE_CONTROL: u8 = 0x00;
const TYPE_AMBE: u8 = 0x01;
const TYPE_AUDIO: u8 = 0x02;

const CONTROL_RATEP: u8 = 0x0A;
const CONTROL_RESET: u8 = 0x33;
const CONTROL_READY: u8 = 0x39;

const FIELD_SPEECH_DATA: u8 = 0x00;
const FIELD_CHANNEL_DATA: u8 = 0x01;
const FIELD_CMODE: u8 = 0x02;

/// Rate index 33: DMR / P25 half-rate, 2450 voice + 1150 FEC.
const RATEP_DMR: [u8; 12] = [
    0x04, 0x31, 0x07, 0x54, 0x24, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6F, 0x48,
];

/// Rate index 34: raw 2450 voice, 0 FEC.
const RATEP_RAW: [u8; 12] = [
    0x04, 0x31, 0x07, 0x54, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x70, 0x31,
];

const CODED_BYTES: usize = 9; // 72 bits
const RAW_BYTES: usize = 7; // 49 bits, padded to ceil(49/8)
const SERIAL_BAUD: u32 = 460_800;
const SERIAL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
enum CaptureError {
    Io(std::io::Error),
    Serial(serialport::Error),
    Protocol(String),
}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        CaptureError::Io(e)
    }
}
impl From<serialport::Error> for CaptureError {
    fn from(e: serialport::Error) -> Self {
        CaptureError::Serial(e)
    }
}
impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureError::Io(e) => write!(f, "I/O: {e}"),
            CaptureError::Serial(e) => write!(f, "serial: {e}"),
            CaptureError::Protocol(s) => write!(f, "protocol: {s}"),
        }
    }
}

struct Chip {
    port: Box<dyn serialport::SerialPort>,
}

impl Chip {
    fn open(path: &str) -> Result<Self, CaptureError> {
        let port = serialport::new(path, SERIAL_BAUD)
            .timeout(SERIAL_TIMEOUT)
            .open()?;
        // Drain any stale bytes from prior consumers (e.g. ambeserver).
        port.clear(serialport::ClearBuffer::All)?;
        Ok(Self { port })
    }

    fn write_packet(&mut self, payload_type: u8, payload: &[u8]) -> Result<(), CaptureError> {
        let mut buf = Vec::with_capacity(4 + payload.len());
        buf.push(START_BYTE);
        buf.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        buf.push(payload_type);
        buf.extend_from_slice(payload);
        self.port.write_all(&buf)?;
        self.port.flush()?;
        Ok(())
    }

    fn read_packet(&mut self) -> Result<(u8, Vec<u8>), CaptureError> {
        let mut header = [0u8; 4];
        self.port.read_exact(&mut header)?;
        if header[0] != START_BYTE {
            return Err(CaptureError::Protocol(format!(
                "bad start byte 0x{:02x}",
                header[0]
            )));
        }
        let payload_len = u16::from_be_bytes([header[1], header[2]]) as usize;
        let payload_type = header[3];
        let mut payload = vec![0u8; payload_len];
        self.port.read_exact(&mut payload)?;
        Ok((payload_type, payload))
    }

    fn reset(&mut self) -> Result<(), CaptureError> {
        self.write_packet(TYPE_CONTROL, &[CONTROL_RESET])?;
        let (ty, payload) = self.read_packet()?;
        if ty != TYPE_CONTROL || payload.first() != Some(&CONTROL_READY) {
            return Err(CaptureError::Protocol(format!(
                "expected READY after reset, got type=0x{ty:02x} payload[0]={:?}",
                payload.first()
            )));
        }
        Ok(())
    }

    fn set_ratep(&mut self, ratep: &[u8; 12]) -> Result<(), CaptureError> {
        let mut payload = Vec::with_capacity(1 + ratep.len());
        payload.push(CONTROL_RATEP);
        payload.extend_from_slice(ratep);
        self.write_packet(TYPE_CONTROL, &payload)?;
        let (ty, resp) = self.read_packet()?;
        if ty != TYPE_CONTROL || resp.first() != Some(&CONTROL_RATEP) {
            return Err(CaptureError::Protocol(format!(
                "expected RATEP ack, got type=0x{ty:02x} payload[0]={:?}",
                resp.first()
            )));
        }
        // Byte 1 of the RATEP response indicates error if non-zero.
        if resp.get(1).copied().unwrap_or(0) != 0 {
            return Err(CaptureError::Protocol(format!(
                "RATEP rejected by chip: status={:?}",
                resp.get(1)
            )));
        }
        Ok(())
    }

    /// Encode one PCM frame, returning (bits, packed_data_bytes).  PCM is
    /// 160 i16 samples; data length is `ceil(bits/8)`.
    fn encode(&mut self, pcm: &[i16; PCM_SAMPLES]) -> Result<(u8, Vec<u8>), CaptureError> {
        // PKT_AUDIO: field_id(1) + num_samples(1) + samples(320, big-endian) +
        // cmode_field(1) + cmode(2)
        let mut payload = Vec::with_capacity(1 + 1 + PCM_FRAME_BYTES + 1 + 2);
        payload.push(FIELD_SPEECH_DATA);
        payload.push(PCM_SAMPLES as u8);
        for sample in pcm {
            payload.extend_from_slice(&sample.to_be_bytes());
        }
        payload.push(FIELD_CMODE);
        payload.extend_from_slice(&0u16.to_be_bytes());
        self.write_packet(TYPE_AUDIO, &payload)?;

        let (ty, resp) = self.read_packet()?;
        if ty != TYPE_AMBE {
            return Err(CaptureError::Protocol(format!(
                "expected AMBE response, got type=0x{ty:02x}"
            )));
        }
        if resp.first() != Some(&FIELD_CHANNEL_DATA) {
            return Err(CaptureError::Protocol(format!(
                "expected CHAND field, got 0x{:02x?}",
                resp.first()
            )));
        }
        let bits = *resp
            .get(1)
            .ok_or_else(|| CaptureError::Protocol("AMBE response missing bit count".into()))?;
        let data_len = (bits as usize).div_ceil(8);
        if resp.len() < 2 + data_len {
            return Err(CaptureError::Protocol(format!(
                "AMBE response truncated: bits={bits} need {} have {}",
                2 + data_len,
                resp.len()
            )));
        }
        Ok((bits, resp[2..2 + data_len].to_vec()))
    }
}

fn read_pcm_frames(path: &Path) -> Result<Vec<[i16; PCM_SAMPLES]>, CaptureError> {
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    if bytes.len() % PCM_FRAME_BYTES != 0 {
        return Err(CaptureError::Protocol(format!(
            "PCM file {} length {} is not a multiple of {} (one 20 ms frame); \
             trim to whole frames before capture",
            path.display(),
            bytes.len(),
            PCM_FRAME_BYTES
        )));
    }
    let n_frames = bytes.len() / PCM_FRAME_BYTES;
    let mut frames = Vec::with_capacity(n_frames);
    for chunk in bytes.chunks_exact(PCM_FRAME_BYTES) {
        let mut frame = [0i16; PCM_SAMPLES];
        for (i, sample) in frame.iter_mut().enumerate() {
            *sample = i16::from_le_bytes([chunk[i * 2], chunk[i * 2 + 1]]);
        }
        frames.push(frame);
    }
    Ok(frames)
}

fn run(serial: &str, input: &Path, prefix: &Path) -> Result<(), CaptureError> {
    let frames = read_pcm_frames(input)?;
    eprintln!(
        "loaded {n} frames ({s:.2} s) from {path}",
        n = frames.len(),
        s = frames.len() as f32 * 0.020,
        path = input.display(),
    );

    let mut chip = Chip::open(serial)?;
    eprintln!("opened {serial}");

    // Pass 1: DMR rate (index 33), expect 9-byte / 72-bit frames.
    chip.reset()?;
    chip.set_ratep(&RATEP_DMR)?;
    eprintln!("pass 1: rate 33 (DMR/FEC, 72-bit channel)");
    let mut coded = Vec::with_capacity(frames.len() * CODED_BYTES);
    for (i, frame) in frames.iter().enumerate() {
        let (bits, data) = chip.encode(frame)?;
        if bits != 72 || data.len() != CODED_BYTES {
            return Err(CaptureError::Protocol(format!(
                "frame {i}: expected 72 bits / {CODED_BYTES} bytes, got {bits} bits / {} bytes",
                data.len()
            )));
        }
        coded.extend_from_slice(&data);
        if (i + 1) % 200 == 0 || i + 1 == frames.len() {
            eprintln!("  encoded {} / {}", i + 1, frames.len());
        }
    }

    // Pass 2: raw rate (index 34), expect 7-byte / 49-bit frames.
    chip.reset()?;
    chip.set_ratep(&RATEP_RAW)?;
    eprintln!("pass 2: rate 34 (raw 2450, 49-bit speech-only)");
    let mut raw = Vec::with_capacity(frames.len() * RAW_BYTES);
    for (i, frame) in frames.iter().enumerate() {
        let (bits, data) = chip.encode(frame)?;
        if bits != 49 || data.len() != RAW_BYTES {
            return Err(CaptureError::Protocol(format!(
                "frame {i}: expected 49 bits / {RAW_BYTES} bytes, got {bits} bits / {} bytes",
                data.len()
            )));
        }
        raw.extend_from_slice(&data);
        if (i + 1) % 200 == 0 || i + 1 == frames.len() {
            eprintln!("  encoded {} / {}", i + 1, frames.len());
        }
    }

    let pcm_path = prefix.with_extension("pcm");
    let coded_path = prefix.with_extension("coded72");
    let raw_path = prefix.with_extension("raw49");

    let mut pcm_bytes = Vec::with_capacity(frames.len() * PCM_FRAME_BYTES);
    for frame in &frames {
        for sample in frame {
            pcm_bytes.extend_from_slice(&sample.to_le_bytes());
        }
    }
    File::create(&pcm_path)?.write_all(&pcm_bytes)?;
    File::create(&coded_path)?.write_all(&coded)?;
    File::create(&raw_path)?.write_all(&raw)?;
    eprintln!(
        "wrote {} / {} / {}",
        pcm_path.display(),
        coded_path.display(),
        raw_path.display(),
    );
    Ok(())
}

fn usage() -> ! {
    eprintln!(
        "usage: dv3000_capture <serial-path> <input.pcm> <output-prefix>\n\
         \n\
         Encodes the input PCM through the chip twice -- once at rate 33 (DMR/FEC,\n\
         72-bit channel) and once at rate 34 (raw 2450, 49-bit speech-only) -- and\n\
         writes <prefix>.pcm, <prefix>.coded72, <prefix>.raw49."
    );
    std::process::exit(1)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 4 {
        usage();
    }
    let serial = &args[1];
    let input = PathBuf::from(&args[2]);
    let prefix = PathBuf::from(&args[3]);

    match run(serial, &input, &prefix) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
