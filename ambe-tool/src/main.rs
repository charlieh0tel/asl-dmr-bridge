//! AMBE+2 encode / decode / roundtrip utility.
//!
//! Each 20 ms vocoder frame encodes 160 i16 PCM samples to 9 AMBE bytes.
//! The `.ambe` file format is raw concatenated 9-byte frames with no header.

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand, ValueEnum};
use dmr_types::{AMBE_FRAME_SIZE, AmbeFrame, PCM_SAMPLES};
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use tracing::info;

#[derive(Parser)]
#[command(name = "ambe-tool")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Encode an 8 kHz mono i16 WAV to raw AMBE frames.
    Encode(EncodeArgs),
    /// Decode raw AMBE frames to an 8 kHz mono i16 WAV.
    Decode(DecodeArgs),
    /// Encode then decode; encoder closes before decoder opens.
    Roundtrip(RoundtripArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum EncoderBackend {
    Thumbdv,
    Ambeserver,
    Dynarmic,
    Neural,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DecoderBackend {
    Thumbdv,
    Ambeserver,
    Dynarmic,
    Neural,
}

// ---------------------------------------------------------------------------
// Subcommand args

#[derive(clap::Args)]
struct EncodeArgs {
    #[arg(long, value_enum)]
    encoder: EncoderBackend,
    /// Serial port for thumbdv encoder.
    #[arg(long, default_value = "/dev/ttyUSB0")]
    serial: String,
    /// AMBEserver UDP address.
    #[arg(long, default_value = "127.0.0.1:2460")]
    ambeserver: String,
    /// ONNX model path for neural encoder.
    #[arg(long)]
    model: Option<PathBuf>,
    #[arg(long = "in")]
    input: PathBuf,
    #[arg(long = "out")]
    output: PathBuf,
}

#[derive(clap::Args)]
struct DecodeArgs {
    #[arg(long, value_enum)]
    decoder: DecoderBackend,
    /// Serial port for thumbdv decoder.
    #[arg(long, default_value = "/dev/ttyUSB0")]
    serial: String,
    /// AMBEserver UDP address.
    #[arg(long, default_value = "127.0.0.1:2460")]
    ambeserver: String,
    /// Directory with decoder_frame.onnx and GRU weight files for neural decoder.
    #[arg(long)]
    decoder_dir: Option<PathBuf>,
    #[arg(long = "in")]
    input: PathBuf,
    #[arg(long = "out")]
    output: PathBuf,
}

#[derive(clap::Args)]
struct RoundtripArgs {
    #[arg(long, value_enum)]
    encoder: EncoderBackend,
    #[arg(long, value_enum)]
    decoder: DecoderBackend,
    /// Serial port for both thumbdv sides; overridden per-side by --encoder-serial/--decoder-serial.
    #[arg(long, default_value = "/dev/ttyUSB0")]
    serial: String,
    /// AMBEserver address for both sides; overridden per-side by --encoder-ambeserver/--decoder-ambeserver.
    #[arg(long, default_value = "127.0.0.1:2460")]
    ambeserver: String,
    /// Serial port for thumbdv encoder (overrides --serial).
    #[arg(long)]
    encoder_serial: Option<String>,
    /// AMBEserver address for encoder (overrides --ambeserver).
    #[arg(long)]
    encoder_ambeserver: Option<String>,
    /// ONNX model path for neural encoder.
    #[arg(long)]
    encoder_model: Option<PathBuf>,
    /// Serial port for thumbdv decoder (overrides --serial).
    #[arg(long)]
    decoder_serial: Option<String>,
    /// AMBEserver address for decoder (overrides --ambeserver).
    #[arg(long)]
    decoder_ambeserver: Option<String>,
    /// Directory with decoder_frame.onnx and GRU weight files for neural decoder.
    #[arg(long)]
    decoder_dir: Option<PathBuf>,
    #[arg(long = "in")]
    input: PathBuf,
    #[arg(long = "out")]
    output: PathBuf,
}

// ---------------------------------------------------------------------------
// Backend factories

fn open_encoder(
    backend: EncoderBackend,
    serial: &str,
    ambeserver: &str,
    model: Option<&Path>,
) -> Result<Box<dyn ambe::Vocoder>> {
    match backend {
        EncoderBackend::Thumbdv => {
            #[cfg(feature = "thumbdv")]
            {
                return Ok(ambe::open_thumbdv(serial, None)?);
            }
            #[cfg(not(feature = "thumbdv"))]
            bail!("thumbdv not compiled; rebuild with --features thumbdv")
        }
        EncoderBackend::Ambeserver => {
            let addr: SocketAddr = ambeserver
                .parse()
                .with_context(|| format!("parse --ambeserver {ambeserver}"))?;
            Ok(ambe::open_ambeserver(addr)?)
        }
        EncoderBackend::Dynarmic => {
            #[cfg(feature = "dynarmic")]
            {
                return Ok(ambe::open_dynarmic());
            }
            #[cfg(not(feature = "dynarmic"))]
            bail!("dynarmic not compiled; rebuild with --features dynarmic")
        }
        EncoderBackend::Neural => {
            #[cfg(feature = "neural")]
            {
                let path = model.context("--model required for neural encoder")?;
                return Ok(ambe::open_neural(path)?);
            }
            #[cfg(not(feature = "neural"))]
            {
                let _ = model;
                bail!("neural not compiled; rebuild with --features neural")
            }
        }
    }
}

fn open_decoder(
    backend: DecoderBackend,
    serial: &str,
    ambeserver: &str,
    decoder_dir: Option<&Path>,
) -> Result<Box<dyn ambe::Vocoder>> {
    match backend {
        DecoderBackend::Thumbdv => {
            #[cfg(feature = "thumbdv")]
            {
                return Ok(ambe::open_thumbdv(serial, None)?);
            }
            #[cfg(not(feature = "thumbdv"))]
            bail!("thumbdv not compiled; rebuild with --features thumbdv")
        }
        DecoderBackend::Ambeserver => {
            let addr: SocketAddr = ambeserver
                .parse()
                .with_context(|| format!("parse --ambeserver {ambeserver}"))?;
            Ok(ambe::open_ambeserver(addr)?)
        }
        DecoderBackend::Dynarmic => {
            #[cfg(feature = "dynarmic")]
            {
                return Ok(ambe::open_dynarmic());
            }
            #[cfg(not(feature = "dynarmic"))]
            bail!("dynarmic not compiled; rebuild with --features dynarmic")
        }
        DecoderBackend::Neural => {
            #[cfg(feature = "neural")]
            {
                // native_gru step: both model and weights in the same directory.
                let dir = decoder_dir.context("--decoder-dir required for neural decoder")?;
                return Ok(ambe::open_native_gru_decoder_from_dirs(dir, dir)?);
            }
            #[cfg(not(feature = "neural"))]
            {
                let _ = decoder_dir;
                bail!("neural not compiled; rebuild with --features neural")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Encode / decode

fn encode_all(vocoder: &mut dyn ambe::Vocoder, samples: &[i16]) -> Result<Vec<AmbeFrame>> {
    let frames = samples.len() / PCM_SAMPLES;
    let mut out = Vec::with_capacity(frames);
    for i in 0..frames {
        let mut pcm = [0i16; PCM_SAMPLES];
        pcm.copy_from_slice(&samples[i * PCM_SAMPLES..(i + 1) * PCM_SAMPLES]);
        out.push(vocoder.encode(&pcm).context("encode frame")?);
    }
    Ok(out)
}

fn decode_all(vocoder: &mut dyn ambe::Vocoder, frames: &[AmbeFrame]) -> Result<Vec<i16>> {
    let mut out = Vec::with_capacity(frames.len() * PCM_SAMPLES);
    for frame in frames {
        out.extend_from_slice(&vocoder.decode(Some(frame)).context("decode frame")?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// WAV / AMBE I/O

fn read_wav(path: &Path) -> Result<Vec<i16>> {
    let mut reader = WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let spec = reader.spec();
    ensure!(
        spec.channels == 1
            && spec.sample_rate == 8000
            && spec.bits_per_sample == 16
            && spec.sample_format == SampleFormat::Int,
        "{}: expected 8 kHz mono i16 PCM, got {spec:?}",
        path.display()
    );
    Ok(reader.samples::<i16>().collect::<Result<Vec<_>, _>>()?)
}

fn write_wav(path: &Path, samples: &[i16]) -> Result<()> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: 8000,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer =
        WavWriter::create(path, spec).with_context(|| format!("create {}", path.display()))?;
    for &s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(())
}

fn write_ambe(path: &Path, frames: &[AmbeFrame]) -> Result<()> {
    let mut f =
        std::fs::File::create(path).with_context(|| format!("create {}", path.display()))?;
    for frame in frames {
        f.write_all(frame)?;
    }
    Ok(())
}

fn read_ambe(path: &Path) -> Result<Vec<AmbeFrame>> {
    let data = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    ensure!(
        data.len() % AMBE_FRAME_SIZE == 0,
        "{}: size {} is not a multiple of {AMBE_FRAME_SIZE}",
        path.display(),
        data.len()
    );
    Ok(data
        .chunks_exact(AMBE_FRAME_SIZE)
        .map(|c| c.try_into().unwrap())
        .collect())
}

// ---------------------------------------------------------------------------
// Run

fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Encode(a) => {
            let mut enc = open_encoder(a.encoder, &a.serial, &a.ambeserver, a.model.as_deref())?;
            let samples = read_wav(&a.input)?;
            info!(frames = samples.len() / PCM_SAMPLES, path = %a.input.display(), "encoding");
            let frames = encode_all(enc.as_mut(), &samples)?;
            write_ambe(&a.output, &frames)?;
            info!(frames = frames.len(), path = %a.output.display(), "encoded");
        }
        Cmd::Decode(a) => {
            let mut dec = open_decoder(
                a.decoder,
                &a.serial,
                &a.ambeserver,
                a.decoder_dir.as_deref(),
            )?;
            let frames = read_ambe(&a.input)?;
            info!(frames = frames.len(), path = %a.input.display(), "decoding");
            let samples = decode_all(dec.as_mut(), &frames)?;
            write_wav(&a.output, &samples)?;
            info!(samples = samples.len(), path = %a.output.display(), "decoded");
        }
        Cmd::Roundtrip(a) => {
            let enc_serial = a.encoder_serial.as_deref().unwrap_or(&a.serial);
            let dec_serial = a.decoder_serial.as_deref().unwrap_or(&a.serial);
            let enc_ambeserver = a.encoder_ambeserver.as_deref().unwrap_or(&a.ambeserver);
            let dec_ambeserver = a.decoder_ambeserver.as_deref().unwrap_or(&a.ambeserver);
            // Encode and drop before opening decoder so thumbdv->thumbdv
            // doesn't double-open the serial port.
            let frames = {
                let mut enc = open_encoder(
                    a.encoder,
                    enc_serial,
                    enc_ambeserver,
                    a.encoder_model.as_deref(),
                )?;
                let samples = read_wav(&a.input)?;
                info!(
                    frames = samples.len() / PCM_SAMPLES,
                    path = %a.input.display(),
                    "encoding"
                );
                encode_all(enc.as_mut(), &samples)?
            };
            let samples = {
                let mut dec = open_decoder(
                    a.decoder,
                    dec_serial,
                    dec_ambeserver,
                    a.decoder_dir.as_deref(),
                )?;
                info!(frames = frames.len(), "decoding");
                decode_all(dec.as_mut(), &frames)?
            };
            write_wav(&a.output, &samples)?;
            info!(samples = samples.len(), path = %a.output.display(), "done");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    tracing_subscriber::fmt::init();
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
