//! Regenerate a vocoder's golden PCM for the committed test vectors.
//!
//! Requires the `testing` feature (which exposes test_harness and
//! test_vectors), plus the relevant backend feature:
//!
//!   cargo run -p ambe --features mbelib,testing --example gen_golden -- mbelib
//!   cargo run -p ambe --features thumbdv,testing --example gen_golden -- thumbdv /dev/ttyUSB0
//!   cargo run -p ambe --features testing --example gen_golden -- ambeserver 127.0.0.1:2460
//!
//! Writes `ambe/tests/fixtures/<backend>_golden.bin` (the PCM bytes
//! the test pins) AND `<backend>_golden.meta.toml` recording the
//! regen timestamp, ambe-crate version, and the TEST_FRAMES content
//! used.  The meta is purely documentation: PR reviewers should
//! diff it alongside the .bin to confirm the regen was intentional
//! and that TEST_FRAMES matches what the .bin was generated from.

use std::fmt::Write as _;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use ambe::Vocoder;
use ambe::test_harness::decode_test_frames;
use ambe::test_vectors::TEST_FRAMES;

fn usage() -> ! {
    eprintln!("usage: gen_golden <mbelib|thumbdv|ambeserver> [arg]");
    std::process::exit(1);
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let backend = args.get(1).map(String::as_str).unwrap_or_else(|| usage());

    let mut vocoder: Box<dyn Vocoder> = match backend {
        #[cfg(feature = "mbelib")]
        "mbelib" => ambe::open_mbelib(),
        #[cfg(not(feature = "mbelib"))]
        "mbelib" => {
            eprintln!("error: built without mbelib feature");
            std::process::exit(1);
        }
        #[cfg(feature = "thumbdv")]
        "thumbdv" => {
            let path = args.get(2).unwrap_or_else(|| {
                eprintln!("thumbdv requires serial port path");
                std::process::exit(1);
            });
            ambe::open_thumbdv(path, None, None).unwrap_or_else(|e| panic!("opening thumbdv: {e}"))
        }
        #[cfg(not(feature = "thumbdv"))]
        "thumbdv" => {
            eprintln!("thumbdv not compiled (enable the 'thumbdv' feature)");
            std::process::exit(1);
        }
        "ambeserver" => {
            let addr: std::net::SocketAddr = args
                .get(2)
                .unwrap_or_else(|| {
                    eprintln!("ambeserver requires host:port");
                    std::process::exit(1);
                })
                .parse()
                .expect("parse addr");
            ambe::open_ambeserver(addr, None)
                .unwrap_or_else(|e| panic!("connecting to ambeserver: {e}"))
        }
        _ => usage(),
    };

    let pcm_bytes = decode_test_frames(vocoder.as_mut());

    let fixtures_dir: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let bin_path = fixtures_dir.join(format!("{backend}_golden.bin"));
    File::create(&bin_path)?.write_all(&pcm_bytes)?;
    println!("wrote {} ({} bytes)", bin_path.display(), pcm_bytes.len());

    let meta_path = fixtures_dir.join(format!("{backend}_golden.meta.toml"));
    let meta = build_manifest(backend);
    File::create(&meta_path)?.write_all(meta.as_bytes())?;
    println!("wrote {} ({} bytes)", meta_path.display(), meta.len());

    Ok(())
}

/// Format the per-backend manifest.  Hand-rolled TOML so the example
/// stays dep-free.  Format is stable -- if you change it, update the
/// reader in any future meta-validation test.
fn build_manifest(backend: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut s = String::with_capacity(512);
    s.push_str("# Companion manifest for the corresponding _golden.bin.\n");
    s.push_str("# Regenerate via `cargo run -p ambe --example gen_golden`.\n");
    s.push_str("# PR reviewers: diff this alongside the .bin to confirm the\n");
    s.push_str("# regen was intentional and that TEST_FRAMES still matches.\n\n");
    let _ = writeln!(s, "backend = {}", toml_str(backend));
    let _ = writeln!(s, "regenerated_at_unix = {now}");
    let _ = writeln!(s, "ambe_version = {}", toml_str(env!("CARGO_PKG_VERSION")));
    s.push('\n');
    s.push_str("# TEST_FRAMES inputs as hex; if these don't match the\n");
    s.push_str("# current ambe::test_vectors::TEST_FRAMES, the .bin is\n");
    s.push_str("# stale and must be regenerated.\n");
    s.push_str("test_frames = [\n");
    for frame in &TEST_FRAMES {
        s.push_str("    \"");
        for b in frame {
            let _ = write!(s, "{b:02x}");
        }
        s.push_str("\",\n");
    }
    s.push_str("]\n");
    s
}

/// Encode a string as a TOML basic-string literal.  Hand-rolled so
/// the example stays dep-free; relying on Rust's `{:?}` Debug format
/// only happens to coincide with TOML for ASCII-safe inputs and would
/// produce invalid output for any string containing a unicode escape
/// or a `\u{NN}`-style debug payload.
fn toml_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                let _ = write!(out, "\\u{:04X}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
