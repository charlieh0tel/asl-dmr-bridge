use clap::Parser;
use clap::ValueEnum;

const DEFAULT_BAUD: u32 = 460800;
const DEFAULT_LISTEN: &str = "127.0.0.1:2460";

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum Backend {
    /// Relay to a ThumbDV (or any DVSI AMBE-3000R) over serial.
    ThumbDV,
    /// In-process software vocoder via the dynarmic-emulated MD380
    /// firmware.
    Dynarmic,
    /// In-process neural encoder; decode delegates to dynarmic.
    Neural,
}

#[derive(Parser)]
#[command(
    name = "ambeserver",
    about = "UDP <-> AMBE-3000R proxy with one-holder exclusivity (ThumbDV or software vocoder)",
    version
)]
pub(crate) struct Args {
    /// Vocoder backend.
    #[arg(long, value_enum, default_value_t = Backend::ThumbDV)]
    pub(crate) backend: Backend,
    /// Serial device path (e.g. /dev/ttyUSB0).  Required when
    /// `--backend thumbdv`.
    #[arg(long, required_if_eq("backend", "thumbdv"))]
    pub(crate) serial: Option<String>,
    /// Baud rate (thumbdv backend only).
    #[arg(long, default_value_t = DEFAULT_BAUD)]
    pub(crate) baud: u32,
    /// ONNX model file.  Required when `--backend neural`.
    #[arg(long, required_if_eq("backend", "neural"))]
    pub(crate) model_path: Option<std::path::PathBuf>,
    /// UDP listen address.
    #[arg(long, default_value = DEFAULT_LISTEN)]
    pub(crate) listen: String,
}
