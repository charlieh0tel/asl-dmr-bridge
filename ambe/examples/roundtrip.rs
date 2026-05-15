use std::path::PathBuf;

use ambe::Vocoder;
use anyhow::Context;
use clap::Parser;
use clap::ValueEnum;
use dmr_types::PCM_SAMPLES;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Enc {
    Neural,
    Dynarmic,
    Thumbdv,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Dec {
    Neural,
    Dynarmic,
    Thumbdv,
}

#[derive(Parser)]
#[command(about = "Round-trip a WAV file through an encoder-decoder pair")]
struct Args {
    #[arg(long, value_enum)]
    encoder: Enc,
    #[arg(long, value_enum)]
    decoder: Dec,
    /// Input WAV (8 kHz mono i16).
    #[arg(long = "in")]
    input: PathBuf,
    /// Output WAV path.
    #[arg(long = "out")]
    output: PathBuf,
    /// ONNX model file (required for encoder=neural).
    #[arg(long)]
    encoder_model_path: Option<PathBuf>,
    /// Directory containing frame_model.onnx + step_model.onnx (required for decoder=neural).
    #[arg(long)]
    decoder_model_dir: Option<PathBuf>,
    /// Serial port shared by thumbdv encoder and/or decoder.
    #[arg(long)]
    serial_port: Option<String>,
    /// Print per-frame VQ indices as JSONL to stdout.
    #[arg(long)]
    dump_vq: bool,
}

fn open_encoder(args: &Args) -> anyhow::Result<Box<dyn Vocoder>> {
    let mut v: Box<dyn Vocoder> = match args.encoder {
        Enc::Neural => {
            let path = args
                .encoder_model_path
                .as_ref()
                .context("--encoder-model-path required for encoder=neural")?;
            ambe::open_neural(path)?
        }
        Enc::Dynarmic => ambe::open_dynarmic(),
        Enc::Thumbdv => open_thumbdv(args)?,
    };
    v.reset();
    Ok(v)
}

fn open_decoder(args: &Args) -> anyhow::Result<Box<dyn Vocoder>> {
    let mut v: Box<dyn Vocoder> = match args.decoder {
        Dec::Neural => {
            let dir = args
                .decoder_model_dir
                .as_ref()
                .context("--decoder-model-dir required for decoder=neural")?;
            ambe::open_neural_decoder(&dir.join("frame_model.onnx"), &dir.join("step_model.onnx"))?
        }
        Dec::Dynarmic => ambe::open_dynarmic(),
        Dec::Thumbdv => open_thumbdv(args)?,
    };
    v.reset();
    Ok(v)
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
    Ok(())
}
