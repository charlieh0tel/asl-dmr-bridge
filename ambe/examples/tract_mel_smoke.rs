//! §0.0 smoke test: load the nambe MelSpectrogram -> log -> Conv1d
//! ONNX bundle through tract, run inference on the bundled PCM,
//! compare against the bundled onnxruntime reference output with
//! max_abs_diff <= 1e-5.
//!
//! Pass: tract handles ONNX opset-17 STFT + the rest of the mel
//! op stack, so graph-folded mel is viable for the real model.
//! Fail: fall back to Rust-side mel front-end.
//!
//! Usage:
//!     cargo run -p ambe --features neural --example tract_mel_smoke -- \
//!         path/to/smoke_tract_mel/

use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use tract_onnx::prelude::*;

const PCM_LEN: usize = 1280;
const REFERENCE_LEN: usize = 8 * 7;
const TOLERANCE: f32 = 1e-5;

fn read_f32_le(path: &Path, expected_len: usize) -> Result<Vec<f32>, String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if bytes.len() != expected_len * 4 {
        return Err(format!(
            "{}: expected {} bytes ({} f32), got {}",
            path.display(),
            expected_len * 4,
            expected_len,
            bytes.len(),
        ));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect())
}

fn run(bundle: &Path) -> Result<(), String> {
    let model_path = bundle.join("model.onnx");
    let pcm = read_f32_le(&bundle.join("input_pcm.bin"), PCM_LEN)?;
    let reference = read_f32_le(&bundle.join("reference_output.bin"), REFERENCE_LEN)?;

    let plan = tract_onnx::onnx()
        .model_for_path(&model_path)
        .map_err(|e| format!("load {}: {e:#}", model_path.display()))?
        .into_optimized()
        .map_err(|e| format!("optimize: {e:#}"))?
        .into_runnable()
        .map_err(|e| format!("into_runnable: {e:#}"))?;
    eprintln!("model loaded + optimized + runnable");

    let input = tract_ndarray::Array2::from_shape_vec((1, PCM_LEN), pcm)
        .map_err(|e| format!("input shape: {e}"))?
        .into_tensor();
    let outputs = plan
        .run(tvec!(input.into()))
        .map_err(|e| format!("inference: {e}"))?;
    if outputs.len() != 1 {
        return Err(format!("expected 1 output, got {}", outputs.len()));
    }
    let actual = outputs[0]
        .as_slice::<f32>()
        .map_err(|e| format!("output as f32 slice: {e}"))?;
    if actual.len() != reference.len() {
        return Err(format!(
            "output length: got {}, reference {}",
            actual.len(),
            reference.len(),
        ));
    }

    let (max_diff, max_idx) = actual.iter().zip(reference.iter()).enumerate().fold(
        (0.0f32, 0usize),
        |(d, i), (idx, (a, r))| {
            let nd = (a - r).abs();
            if nd > d { (nd, idx) } else { (d, i) }
        },
    );
    eprintln!("max_abs_diff = {max_diff:.3e} at idx {max_idx}");

    if max_diff > TOLERANCE {
        Err(format!(
            "FAIL: max_abs_diff {max_diff:.3e} > {TOLERANCE:.0e}"
        ))
    } else {
        eprintln!("PASS: tract output matches reference within {TOLERANCE:.0e}");
        Ok(())
    }
}

fn main() -> ExitCode {
    let bundle = match env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("usage: tract_mel_smoke <smoke_tract_mel_dir>");
            return ExitCode::FAILURE;
        }
    };
    match run(&bundle) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
