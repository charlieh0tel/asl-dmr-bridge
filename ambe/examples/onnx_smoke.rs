//! Smoke-test loading a nambe ONNX file via tract: load, parse
//! metadata, optimize, build a runnable plan.  Reports each stage
//! via tracing logs (set `RUST_LOG=info`); exits non-zero on any
//! error.  Used to validate a fresh export from nambe before wiring
//! it into the bridge.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let path = match env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("usage: onnx_smoke <model.onnx>");
            return ExitCode::FAILURE;
        }
    };

    match ambe::open_neural(&path) {
        Ok(_v) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("load failed: {e}");
            ExitCode::FAILURE
        }
    }
}
