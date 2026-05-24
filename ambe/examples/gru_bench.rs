use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;

#[derive(Parser)]
#[command(about = "Benchmark NativeGruDecoder: decode N frames and print timing")]
struct Args {
    /// Directory containing the binary GRU weight files (and meta.json).
    #[arg(long)]
    weights_dir: PathBuf,
    /// Directory containing decoder_frame.onnx.  Defaults to --weights-dir.
    #[arg(long)]
    model_dir: Option<PathBuf>,
    /// Number of frames to decode.
    #[arg(long, default_value_t = 500)]
    frames: usize,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let model_dir = args.model_dir.as_deref().unwrap_or(&args.weights_dir);
    let frame_model = model_dir.join("decoder_frame.onnx");

    let mut bench = ambe::NativeGruDecoderBench::open(&frame_model, &args.weights_dir)
        .context("open NativeGruDecoderBench")?;

    // All-zero AmbeFrame: b0=0 < B0_SPECIAL_MIN, so the full GRU step loop runs.
    let dummy_frame: dmr_types::AmbeFrame = [0u8; dmr_types::AMBE_FRAME_SIZE];

    use ambe::Vocoder;
    for _ in 0..args.frames {
        bench.decode(Some(&dummy_frame)).context("decode")?;
    }

    let t = bench.timing();
    eprintln!("frames:         {:6}", t.frames);
    eprintln!("frame_model:    {:6} us/frame", t.frame_model_us);
    eprintln!("gru_step x160:  {:6} us/frame", t.step_model_us);
    eprintln!(
        "total:          {:6} us/frame",
        t.frame_model_us + t.step_model_us
    );
    Ok(())
}
