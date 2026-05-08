use clap::Parser;

const DEFAULT_BAUD: u32 = 460800;
const DEFAULT_LISTEN: &str = "127.0.0.1:2460";

#[derive(Parser)]
#[command(
    name = "ambeserver",
    about = "UDP <-> AMBE-3000R serial proxy with one-holder exclusivity",
    version
)]
pub(crate) struct Args {
    /// Serial device path (e.g. /dev/ttyUSB0).
    #[arg(long)]
    pub(crate) serial: String,
    /// Baud rate.
    #[arg(long, default_value_t = DEFAULT_BAUD)]
    pub(crate) baud: u32,
    /// UDP listen address.
    #[arg(long, default_value = DEFAULT_LISTEN)]
    pub(crate) listen: String,
}
