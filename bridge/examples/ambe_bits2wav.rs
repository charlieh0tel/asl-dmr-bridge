//! Convert AMBE+2 source bits to PCM via channel-encode + decode,
//! writing an 8 kHz mono int16 WAV.
//!
//! Input: concatenated 7-byte frames, each 49 source bits packed
//! MSB-first in mbelib `ambe_d[]` order; low 7 bits of byte 6
//! zero-padded.  One frame per 20 ms.  This is the format written
//! by the bridge diagnostic recorder (`dmr_to_fm_decode_in_*.bin`).
//!
//! Backend selection:
//!   chip (default): `--decoder chip` (ambeserver or thumbdv via --chip-backend args)
//!   neural ONNX:    `--decoder neural --decoder-model-dir <dir>`
//!   native GRU:     `--decoder neural --decoder-step native-gru --decoder-model-dir <dir> --decoder-weights-dir <dir>`
//!
//! `--no-decode` skips the round trip and writes the 9-byte
//! channel-coded stream to `--output` instead of a WAV.

use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use ambe::cli::ChipBackendArgs;
use ambe::cli::NeuralDecoderArgs;
use ambe::cli::NeuralDecoderStep;
use ambe::voice_channel::CODED_BYTES;
use ambe::voice_channel::RAW_BYTES;
use ambe::voice_channel::channel_encode;
use ambe::voice_channel::permute_mbelib_to_chip;
use clap::Parser;
use clap::ValueEnum;

const PCM_SAMPLE_RATE: u32 = 8000;
const PCM_SAMPLES_PER_FRAME: usize = 160;

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Backend {
    /// Chip backends (ambeserver, thumbdv) selected by --chip-backend args.
    #[default]
    Chip,
    /// Neural decoder; use --decoder-step to select onnx (default) or native-gru.
    Neural,
}

#[derive(Parser)]
#[command(about = "Convert AMBE+2 source bits (bridge .bin format) to PCM WAV")]
struct Args {
    /// Input file: concatenated 7-byte AMBE+2 source-bit frames.
    #[arg(long)]
    input: PathBuf,
    /// Output WAV (or 9-byte channel-coded stream with --no-decode).
    #[arg(long)]
    output: PathBuf,
    /// Skip decode; write the 9-byte channel-coded stream instead.
    #[arg(long)]
    no_decode: bool,
    /// Suppress progress messages on stderr.
    #[arg(long)]
    quiet: bool,
    /// Decoder backend.
    #[arg(long, default_value = "chip")]
    decoder: Backend,
    #[command(flatten)]
    chip_backend: ChipBackendArgs,
    #[command(flatten)]
    neural: NeuralDecoderArgs,
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

fn open_vocoder(args: &Args) -> Result<Box<dyn ambe::Vocoder>, String> {
    match args.decoder {
        Backend::Chip => args
            .chip_backend
            .open_vocoder()
            .map_err(|e| format!("open chip backend: {e}")),
        #[cfg(feature = "neural")]
        Backend::Neural => {
            let dir = args
                .neural
                .decoder_model_dir
                .as_deref()
                .ok_or("--decoder-model-dir required for --decoder neural")?;
            match args.neural.decoder_step {
                NeuralDecoderStep::Onnx => ambe::open_neural_decoder_from_dir(dir)
                    .map_err(|e| format!("open onnx decoder: {e}")),
                NeuralDecoderStep::NativeGru => {
                    let weights_dir =
                        args.neural.decoder_weights_dir.as_deref().ok_or(
                            "--decoder-weights-dir required for --decoder-step native-gru",
                        )?;
                    ambe::open_native_gru_decoder_from_dirs(dir, weights_dir)
                        .map_err(|e| format!("open native-gru decoder: {e}"))
                }
            }
        }
        #[cfg(not(feature = "neural"))]
        Backend::Neural => Err("neural backend requires --features neural".into()),
    }
}

fn run(args: &Args) -> Result<(), String> {
    let mut bits_bytes = Vec::new();
    File::open(&args.input)
        .and_then(|mut f| f.read_to_end(&mut bits_bytes))
        .map_err(|e| format!("read {}: {e}", args.input.display()))?;
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
            args.input.display()
        );
    }

    let frames: Vec<[u8; CODED_BYTES]> = bits_bytes
        .chunks_exact(RAW_BYTES)
        .map(|chunk| {
            let mut mbelib_packed = [0u8; RAW_BYTES];
            mbelib_packed.copy_from_slice(chunk);
            channel_encode(&permute_mbelib_to_chip(&mbelib_packed))
        })
        .collect();

    if args.no_decode {
        let coded: Vec<u8> = frames.iter().flat_map(|f| f.iter().copied()).collect();
        File::create(&args.output)
            .and_then(|mut f| f.write_all(&coded))
            .map_err(|e| format!("write {}: {e}", args.output.display()))?;
        if !args.quiet {
            eprintln!(
                "wrote {} ({} bytes, {n_frames} channel-coded frames)",
                args.output.display(),
                coded.len()
            );
        }
        return Ok(());
    }

    let mut vocoder = open_vocoder(args)?;
    vocoder.reset();

    let mut pcm = Vec::with_capacity(n_frames * PCM_SAMPLES_PER_FRAME);
    for (i, frame) in frames.iter().enumerate() {
        let samples = vocoder
            .decode(Some(frame))
            .map_err(|e| format!("decode frame {i}: {e}"))?;
        pcm.extend_from_slice(&samples);
        if !args.quiet && (i + 1) % 200 == 0 {
            eprintln!("  decoded {} / {n_frames}", i + 1);
        }
    }

    write_wav(&args.output, &pcm).map_err(|e| format!("write {}: {e}", args.output.display()))?;
    if !args.quiet {
        eprintln!(
            "wrote {} ({n_frames} frames, {:.2}s)",
            args.output.display(),
            n_frames as f32 * 0.020
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
