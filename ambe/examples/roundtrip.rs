use std::path::PathBuf;

use ambe::Vocoder;
use ambe::cli::NeuralDecoderArgs;
use ambe::cli::NeuralDecoderStep;
use anyhow::Context;
use clap::Parser;
use clap::ValueEnum;
use dmr_types::AmbeFrame;
use dmr_types::PCM_SAMPLES;
use dmr_types::PcmFrame;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Encoder {
    Neural,
    Dynarmic,
    Thumbdv,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Decoder {
    /// Neural decoder (use --decoder-step to select onnx or native-gru).
    Neural,
    Dynarmic,
    Thumbdv,
}

#[derive(Parser)]
#[command(about = "Round-trip a WAV file through an encoder-decoder pair")]
struct Args {
    #[arg(long, value_enum)]
    encoder: Encoder,
    #[arg(long, value_enum)]
    decoder: Decoder,
    /// Input WAV (8 kHz mono i16).
    #[arg(long = "in")]
    input: PathBuf,
    /// Output WAV path.
    #[arg(long = "out")]
    output: PathBuf,
    /// ONNX model file (required for encoder=neural).
    #[arg(long)]
    encoder_model_path: Option<PathBuf>,
    /// Serial port shared by thumbdv encoder and/or decoder.
    #[arg(long)]
    serial_port: Option<String>,
    /// Print per-frame VQ indices as JSONL to stdout.
    #[arg(long)]
    dump_vq: bool,
    #[command(flatten)]
    neural: NeuralDecoderArgs,
}

enum DecoderHandle {
    Neural(Box<ambe::NeuralDecoderBench>),
    Other(Box<dyn Vocoder>),
}

impl DecoderHandle {
    fn decode(&mut self, ambe: Option<&AmbeFrame>) -> Result<PcmFrame, ambe::VocoderError> {
        match self {
            Self::Neural(b) => b.decode(ambe),
            Self::Other(v) => v.decode(ambe),
        }
    }

    fn print_timing(&self) {
        if let Self::Neural(b) = self {
            let t = b.timing();
            eprintln!(
                "frame_model:     {:6} us/frame  ({} frames)",
                t.frame_model_us, t.frames
            );
            eprintln!(
                "step_model x{:<3}: {:6} us/frame",
                t.step_stride, t.step_model_us
            );
        }
    }
}

fn open_encoder(args: &Args) -> anyhow::Result<Box<dyn Vocoder>> {
    let mut v: Box<dyn Vocoder> = match args.encoder {
        Encoder::Neural => {
            let path = args
                .encoder_model_path
                .as_ref()
                .context("--encoder-model-path required for encoder=neural")?;
            ambe::open_neural(path)?
        }
        Encoder::Dynarmic => ambe::open_dynarmic(),
        Encoder::Thumbdv => open_thumbdv(args)?,
    };
    v.reset();
    Ok(v)
}

fn open_decoder(args: &Args) -> anyhow::Result<DecoderHandle> {
    match args.decoder {
        Decoder::Neural => {
            let dir = args
                .neural
                .decoder_model_dir
                .as_ref()
                .context("--decoder-model-dir required for decoder=neural")?;
            match args.neural.decoder_step {
                NeuralDecoderStep::Onnx => {
                    let mut b = ambe::NeuralDecoderBench::open(
                        &dir.join("decoder_frame.onnx"),
                        &dir.join("decoder_step.onnx"),
                    )?;
                    b.reset();
                    Ok(DecoderHandle::Neural(Box::new(b)))
                }
                NeuralDecoderStep::NativeGru => {
                    let weights_dir =
                        args.neural.decoder_weights_dir.as_ref().context(
                            "--decoder-weights-dir required for decoder-step=native-gru",
                        )?;
                    let mut v = ambe::open_native_gru_decoder_from_dirs(dir, weights_dir)?;
                    v.reset();
                    Ok(DecoderHandle::Other(v))
                }
            }
        }
        Decoder::Dynarmic => {
            let mut v = ambe::open_dynarmic();
            v.reset();
            Ok(DecoderHandle::Other(v))
        }
        Decoder::Thumbdv => {
            let mut v = open_thumbdv(args)?;
            v.reset();
            Ok(DecoderHandle::Other(v))
        }
    }
}

fn open_thumbdv(args: &Args) -> anyhow::Result<Box<dyn Vocoder>> {
    #[cfg(feature = "thumbdv")]
    {
        let port = args
            .serial_port
            .as_deref()
            .context("--serial-port required for thumbdv")?;
        return Ok(ambe::open_thumbdv(port, None)?);
    }
    #[cfg(not(feature = "thumbdv"))]
    {
        let _ = args;
        anyhow::bail!("thumbdv not compiled; rebuild with --features thumbdv");
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let mut reader = hound::WavReader::open(&args.input)
        .with_context(|| format!("open {}", args.input.display()))?;
    let spec = reader.spec();
    anyhow::ensure!(
        spec.sample_rate == 8000 && spec.channels == 1 && spec.bits_per_sample == 16,
        "expected 8 kHz mono i16 WAV; got {}Hz {}ch {}bit",
        spec.sample_rate,
        spec.channels,
        spec.bits_per_sample,
    );
    let samples: Vec<i16> = reader
        .samples::<i16>()
        .collect::<Result<_, _>>()
        .context("read samples")?;

    let mut encoder = open_encoder(&args)?;
    let mut decoder = open_decoder(&args)?;

    let out_spec = hound::WavSpec {
        channels: 1,
        sample_rate: 8000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&args.output, out_spec)
        .with_context(|| format!("create {}", args.output.display()))?;

    for (frame_idx, chunk) in samples.chunks_exact(PCM_SAMPLES).enumerate() {
        let frame: &[i16; PCM_SAMPLES] = chunk.try_into().unwrap();
        let ambe = encoder.encode(frame).context("encode")?;
        if args.dump_vq {
            let vq = ambe::ambe_to_vq(&ambe);
            let vq_str = vq
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(",");
            println!("{{\"frame\":{frame_idx},\"vq\":[{vq_str}]}}");
        }
        let pcm = decoder.decode(Some(&ambe)).context("decode")?;
        for &s in pcm.iter() {
            writer.write_sample(s).context("write sample")?;
        }
    }

    writer.finalize().context("finalize WAV")?;
    decoder.print_timing();
    Ok(())
}
