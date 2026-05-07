use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(name = "asl-dmr-bridge", about = "ASL3 to DMR bridge", version)]
pub(crate) struct Args {
    /// Path to config TOML file
    pub(crate) config: PathBuf,

    /// Read the BM hotspot password from this file (single line,
    /// trailing whitespace stripped).  Alternatives, exactly one
    /// must apply: `BRANDMEISTER_PASSWORD` env var,
    /// `[network].password_file` in config, `[network].password`
    /// inline.
    #[arg(long, value_name = "FILE")]
    pub(crate) password_file: Option<PathBuf>,

    /// Read the Brandmeister Halligan API key from this file
    /// (single line, trailing whitespace stripped).  Alternatives:
    /// `BRANDMEISTER_API_KEY` env var, `[brandmeister_api].api_key_file`
    /// in config, `[brandmeister_api].api_key` inline.
    #[arg(long, value_name = "FILE")]
    pub(crate) api_key_file: Option<PathBuf>,
}
