//! Download real AMBE+2 sample captures from pbarfuss/mbelib-testing
//! into `ambe/tests/fixtures/amb/`.  The files are ISC-licensed code,
//! but their contents require an AMBE decoder (and thus implicate the
//! DVSI patent), same as the `mbelib-sys` build.
//!
//! Usage: `cargo run -p ambe --example fetch_amb_samples`
//!
//! The target directory is in .gitignore; the files are not committed.

use std::path::PathBuf;
use std::process::Command;

const BASE_URL: &str = "https://raw.githubusercontent.com/pbarfuss/mbelib-testing/master";

const FILES: &[&str] = &[
    "bmh_gasline.amb",
    "bmh_gasline_redux.amb",
    "davis_center_doors.amb",
];

fn main() -> std::io::Result<()> {
    let dst_dir: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("amb");
    std::fs::create_dir_all(&dst_dir)?;

    for name in FILES {
        let url = format!("{BASE_URL}/{name}");
        let dst = dst_dir.join(name);
        println!("fetching {url} -> {}", dst.display());
        let status = Command::new("curl")
            .args(["-sSLf", "-o"])
            .arg(&dst)
            .arg(&url)
            .status()?;
        if !status.success() {
            eprintln!("curl failed for {name}");
            std::process::exit(1);
        }
    }
    println!("done");
    Ok(())
}
