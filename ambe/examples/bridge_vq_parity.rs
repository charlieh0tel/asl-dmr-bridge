//! Bridge-VQ parity harness.
//!
//! Reads a JSON manifest of expected per-frame VQ rows produced by a
//! PyTorch-eager reference, runs `ambe::neural` over the same captured
//! WAVs, and reports per-utterance mismatches.  Manual / runtime use
//! only -- not a `cargo test`.
//!
//! Manifest schema (one entry per WAV under `utterances`):
//! ```json
//! {
//!   "ckpt": "...", "ckpt_sha256": "...",
//!   "context_frames": 7, "lookback_samples": 128,
//!   "slice_samples_M": 1216,
//!   "fields": ["b0", ..., "b8"],
//!   "vq_sizes": [128, 32, ..., 8],
//!   "utterances": {
//!     "<basename>.wav": {
//!       "n_pcm_frames": N,
//!       "predictable_range": [a, b],
//!       "vq": [[9 ints], ...]
//!     }
//!   }
//! }
//! ```
//!
//! Frame mapping: Rust streaming emits its first real VQ on the
//! `context_lookahead+1`-th input frame.  The Python harness labels
//! that VQ as frame `predictable_range[0]`.  General mapping:
//! Rust input frame index `f_rust` produces VQ for Python frame
//! `f_rust - context_lookahead`.
//!
//! Usage:
//!   cargo run --features neural --example bridge_vq_parity -- \
//!       --manifest path/to/python_vq.json \
//!       --model   ambe/tests/fixtures/run19/model.onnx \
//!       --pcm-dir /tmp/asl-dmr-bridge-pcm

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use ambe::NeuralEncoder;
use clap::Parser;
use dmr_types::PCM_SAMPLES;
use serde::Deserialize;

#[derive(Parser, Debug)]
#[command(about = "Compare ambe::neural VQ rows against a PT-canonical manifest")]
struct Args {
    /// Path to the JSON manifest (e.g. python_vq.json).
    #[arg(long)]
    manifest: PathBuf,
    /// ONNX model file exported from the same .pt that produced the
    /// manifest's expected VQ rows.
    #[arg(long)]
    model: PathBuf,
    /// Directory holding the captured WAVs referenced by basename
    /// in the manifest.
    #[arg(long, default_value = "/tmp/asl-dmr-bridge-pcm")]
    pcm_dir: PathBuf,
    /// Cap mismatches printed per utterance.
    #[arg(long, default_value_t = 10)]
    max_mismatches: usize,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    ckpt: String,
    ckpt_sha256: String,
    context_frames: usize,
    lookback_samples: usize,
    #[serde(rename = "slice_samples_M")]
    slice_samples_m: usize,
    fields: Vec<String>,
    vq_sizes: Vec<u16>,
    utterances: BTreeMap<String, Utterance>,
}

#[derive(Debug, Deserialize)]
struct Utterance {
    n_pcm_frames: usize,
    predictable_range: [usize; 2],
    vq: Vec<[u16; 9]>,
}

fn read_wav_pcm(path: &Path) -> std::io::Result<Vec<i16>> {
    let bytes = fs::read(path)?;
    if bytes.len() < 44 {
        return Err(std::io::Error::other("WAV shorter than 44-byte header"));
    }
    Ok(bytes[44..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect())
}

/// Pad to whole-frame boundary; the manifest's n_pcm_frames is
/// already padded so the comparison is well-defined.
fn pad_frames(pcm: Vec<i16>) -> Vec<i16> {
    let leftover = pcm.len() % PCM_SAMPLES;
    if leftover == 0 {
        return pcm;
    }
    let mut padded = pcm;
    padded.extend(std::iter::repeat_n(0i16, PCM_SAMPLES - leftover));
    padded
}

fn run(args: &Args) -> Result<bool, String> {
    let manifest_text = fs::read_to_string(&args.manifest)
        .map_err(|e| format!("read manifest {}: {e}", args.manifest.display()))?;
    let manifest: Manifest =
        serde_json::from_str(&manifest_text).map_err(|e| format!("parse manifest: {e}"))?;

    println!(
        "manifest: ckpt={} sha256={} context_frames={} lookback={} slice={} fields={:?} vq_sizes={:?}",
        manifest.ckpt,
        manifest.ckpt_sha256,
        manifest.context_frames,
        manifest.lookback_samples,
        manifest.slice_samples_m,
        manifest.fields,
        manifest.vq_sizes,
    );

    let context_lookahead = manifest.context_frames / 2;
    let mut all_clean = true;

    for (name, utt) in &manifest.utterances {
        let wav_path = args.pcm_dir.join(name);
        let raw = match read_wav_pcm(&wav_path) {
            Ok(p) => p,
            Err(e) => {
                println!("[{name}] SKIP read {}: {e}", wav_path.display());
                all_clean = false;
                continue;
            }
        };
        let pcm = pad_frames(raw);
        let n_frames = pcm.len() / PCM_SAMPLES;
        if n_frames < utt.n_pcm_frames {
            println!(
                "[{name}] SKIP: WAV has {n_frames} frames, manifest expects {}",
                utt.n_pcm_frames
            );
            all_clean = false;
            continue;
        }

        let mut enc = NeuralEncoder::open(&args.model)
            .map_err(|e| format!("open model {}: {e}", args.model.display()))?;
        let mut rust_vq: Vec<Option<[u16; 9]>> = Vec::with_capacity(utt.n_pcm_frames);
        for f in 0..utt.n_pcm_frames {
            let mut frame = [0i16; PCM_SAMPLES];
            frame.copy_from_slice(&pcm[f * PCM_SAMPLES..(f + 1) * PCM_SAMPLES]);
            let v = enc
                .encode_vq(&frame)
                .map_err(|e| format!("[{name}] encode_vq frame {f}: {e}"))?;
            rust_vq.push(v);
        }

        let [pred_lo, pred_hi] = utt.predictable_range;
        if utt.vq.len() != pred_hi - pred_lo {
            println!(
                "[{name}] WARN: vq len {} != predictable_range span {}",
                utt.vq.len(),
                pred_hi - pred_lo
            );
        }

        let mut mismatches = 0usize;
        let mut total = 0usize;
        for f_py in pred_lo..pred_hi {
            let f_rust = f_py + context_lookahead;
            let Some(actual) = rust_vq.get(f_rust).and_then(|o| o.as_ref()) else {
                println!(
                    "[{name}] frame {f_py}: no Rust VQ at rust_idx={f_rust} (warm-up or out of range)"
                );
                mismatches += 1;
                total += 1;
                continue;
            };
            let expected = &utt.vq[f_py - pred_lo];
            total += 1;
            if actual != expected {
                if mismatches < args.max_mismatches {
                    println!("[{name}] frame {f_py}: actual={actual:?} expected={expected:?}");
                }
                mismatches += 1;
            }
        }

        if mismatches == 0 {
            println!("[{name}] OK ({total} frames)");
        } else {
            println!("[{name}] MISMATCH {mismatches}/{total} frames");
            all_clean = false;
        }
    }

    Ok(all_clean)
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    match run(&args) {
        Ok(true) => ExitCode::from(0),
        Ok(false) => {
            eprintln!("\nbridge_vq_parity: at least one utterance had mismatches.");
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("bridge_vq_parity: {e}");
            ExitCode::from(2)
        }
    }
}
