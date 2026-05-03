//! Convert AMBE+2 source bits to PCM via channel-encode + chip
//! decode, writing an 8 kHz mono int16 WAV.
//!
//! Usage:
//!
//!   ambe_bits2wav --input bits.bin --output audio.wav \
//!       [--ambeserver host:port] [--no-decode] [--quiet]
//!
//! Input: concatenated 7-byte frames, each 49 source bits packed
//! MSB-first in mbelib `ambe_d[]` order; low 7 bits of byte 6
//! zero-padded.  One frame per 20 ms.
//!
//! `--ambeserver` defaults to 127.0.0.1:2460.  `--no-decode` skips
//! the round trip and writes the 9-byte channel-coded stream to
//! `--output` instead of a WAV.

use std::env;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use dmr_wire::voice_channel::CODED_BYTES;
use dmr_wire::voice_channel::RAW_BYTES;
use dmr_wire::voice_channel::channel_encode;
use dmr_wire::voice_channel::permute_mbelib_to_chip;

const PCM_SAMPLE_RATE: u32 = 8000;
const PCM_SAMPLES_PER_FRAME: usize = 160;
const DEFAULT_AMBESERVER: &str = "127.0.0.1:2460";

#[derive(Default)]
struct Args {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    ambeserver: Option<String>,
    no_decode: bool,
    quiet: bool,
}

fn usage() -> ! {
    eprintln!(
        "usage: ambe_bits2wav --input bits.bin --output audio.wav \\\n\
         \t[--ambeserver host:port] [--no-decode] [--quiet]\n\
         \n\
         Reads concatenated 7-byte AMBE+2 source-bit frames (49 bits MSB-first\n\
         in mbelib's ambe_d[] order, low 7 bits of byte 6 unused) and writes\n\
         8 kHz mono int16 WAV decoded by an ambeserver.\n\
         \n\
         --no-decode skips the decode round trip and writes the 9-byte\n\
         channel-coded stream to --output instead of a WAV."
    );
    std::process::exit(2)
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--input" => args.input = Some(PathBuf::from(iter.next().unwrap_or_else(|| usage()))),
            "--output" => args.output = Some(PathBuf::from(iter.next().unwrap_or_else(|| usage()))),
            "--ambeserver" => args.ambeserver = Some(iter.next().unwrap_or_else(|| usage())),
            "--no-decode" => args.no_decode = true,
            "--quiet" => args.quiet = true,
            "-h" | "--help" => usage(),
            _ => {
                eprintln!("unexpected argument: {arg}");
                usage();
            }
        }
    }
    if args.input.is_none() || args.output.is_none() {
        usage();
    }
    args
}

/// 44-byte canonical PCM WAV header for mono int16 at 8 kHz.
fn write_wav(path: &PathBuf, pcm: &[i16]) -> std::io::Result<()> {
    let data_bytes = (pcm.len() * 2) as u32;
    let mut f = File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_bytes).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // fmt chunk size
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&PCM_SAMPLE_RATE.to_le_bytes())?;
    f.write_all(&(PCM_SAMPLE_RATE * 2).to_le_bytes())?; // byte rate
    f.write_all(&2u16.to_le_bytes())?; // block align
    f.write_all(&16u16.to_le_bytes())?; // bits per sample
    f.write_all(b"data")?;
    f.write_all(&data_bytes.to_le_bytes())?;
    for &s in pcm {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

fn run(args: &Args) -> Result<(), String> {
    let input = args.input.as_ref().expect("checked by parse_args");
    let output = args.output.as_ref().expect("checked by parse_args");

    let mut bits_bytes = Vec::new();
    File::open(input)
        .and_then(|mut f| f.read_to_end(&mut bits_bytes))
        .map_err(|e| format!("read {}: {e}", input.display()))?;
    if !bits_bytes.len().is_multiple_of(RAW_BYTES) {
        return Err(format!(
            "input length {} is not a multiple of {RAW_BYTES} (one frame)",
            bits_bytes.len(),
        ));
    }
    let n_frames = bits_bytes.len() / RAW_BYTES;
    if !args.quiet {
        eprintln!(
            "loaded {n_frames} frames ({:.2}s) from {}",
            n_frames as f32 * 0.020,
            input.display()
        );
    }

    // Encode each frame to channel-coded form.
    let mut coded = Vec::with_capacity(n_frames * CODED_BYTES);
    for i in 0..n_frames {
        let mut mbelib_packed = [0u8; RAW_BYTES];
        mbelib_packed.copy_from_slice(&bits_bytes[i * RAW_BYTES..(i + 1) * RAW_BYTES]);
        let chip_packed = permute_mbelib_to_chip(&mbelib_packed);
        let cw = channel_encode(&chip_packed);
        coded.extend_from_slice(&cw);
    }

    if args.no_decode {
        File::create(output)
            .and_then(|mut f| f.write_all(&coded))
            .map_err(|e| format!("write {}: {e}", output.display()))?;
        if !args.quiet {
            eprintln!(
                "wrote {} ({} bytes, {n_frames} channel-coded frames)",
                output.display(),
                coded.len()
            );
        }
        return Ok(());
    }

    // Decode via ambeserver.
    let server = args.ambeserver.as_deref().unwrap_or(DEFAULT_AMBESERVER);
    let addr: SocketAddr = server
        .parse()
        .map_err(|e| format!("parse --ambeserver {server}: {e}"))?;
    let mut vocoder =
        ambe::open_ambeserver(addr, None).map_err(|e| format!("connect {server}: {e}"))?;
    if !args.quiet {
        eprintln!("connected to ambeserver at {server}");
    }

    let mut pcm = Vec::with_capacity(n_frames * PCM_SAMPLES_PER_FRAME);
    for i in 0..n_frames {
        let mut frame = [0u8; CODED_BYTES];
        frame.copy_from_slice(&coded[i * CODED_BYTES..(i + 1) * CODED_BYTES]);
        let samples = vocoder
            .decode(&frame)
            .map_err(|e| format!("decode frame {i}: {e}"))?;
        pcm.extend_from_slice(&samples);
        if !args.quiet && (i + 1) % 200 == 0 {
            eprintln!("  decoded {} / {n_frames}", i + 1);
        }
    }

    write_wav(output, &pcm).map_err(|e| format!("write {}: {e}", output.display()))?;
    if !args.quiet {
        eprintln!(
            "wrote {} ({n_frames} frames, {:.2}s)",
            output.display(),
            n_frames as f32 * 0.020
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = parse_args();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
