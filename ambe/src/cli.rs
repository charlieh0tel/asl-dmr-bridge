//! Shared `clap` args for selecting and opening a chip backend.
//!
//! `ChipBackendArgs` is `#[derive(Args)]` so callers can flatten it
//! into their own clap-derive struct and get a consistent
//! `--backend / --ambeserver / --serial / --baud / --gain-in /
//! --gain-out` surface across tools.
//!
//! Two factory methods cover the two consumer shapes:
//! - `open_vocoder()` for tools doing routine PCM <-> AMBE+2 transcode
//!   (returns `Box<dyn Vocoder>`; supports ambeserver / thumbdv /
//!   mbelib).
//! - `open_chip_client()` for tools that need rate / gain control
//!   (returns `Box<dyn ChipClient>`; mbelib is rejected since it has
//!   no encode path).

use std::net::SocketAddr;

use clap::Args;
use clap::ValueEnum;

use crate::Vocoder;
use crate::VocoderError;
use crate::chip::AmbeServerClient;
use crate::chip::ChipClient;

const DEFAULT_AMBESERVER: &str = "127.0.0.1:2460";
const DEFAULT_SERIAL: &str = "/dev/ttyUSB0";

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, ValueEnum)]
pub enum Backend {
    #[default]
    Ambeserver,
    Thumbdv,
    Mbelib,
}

#[derive(Args, Clone, Debug)]
pub struct ChipBackendArgs {
    /// Vocoder backend.
    #[arg(long, value_enum, default_value_t = Backend::Ambeserver)]
    pub backend: Backend,
    /// AMBEserver UDP address (used when backend = ambeserver).
    #[arg(long, default_value = DEFAULT_AMBESERVER)]
    pub ambeserver: String,
    /// Serial device path (used when backend = thumbdv).
    #[arg(long, default_value = DEFAULT_SERIAL)]
    pub serial: String,
    /// Serial baud rate; defaults to 460800 when omitted.
    #[arg(long)]
    pub baud: Option<u32>,
    /// Encoder input gain in dB; clamped to chip range.
    #[arg(long, allow_hyphen_values = true)]
    pub gain_in: Option<i8>,
    /// Decoder output gain in dB; clamped to chip range.
    #[arg(long, allow_hyphen_values = true)]
    pub gain_out: Option<i8>,
}

impl ChipBackendArgs {
    /// Returns `Some((in, out))` when the user passed at least one of
    /// `--gain-in` / `--gain-out`; the unspecified side defaults to 0.
    pub fn gain(&self) -> Option<(i8, i8)> {
        match (self.gain_in, self.gain_out) {
            (None, None) => None,
            (i, o) => Some((i.unwrap_or(0), o.unwrap_or(0))),
        }
    }

    fn parse_ambeserver(&self) -> Result<SocketAddr, VocoderError> {
        self.ambeserver
            .parse()
            .map_err(|e| VocoderError::Init(format!("parse --ambeserver {}: {e}", self.ambeserver)))
    }

    pub fn open_vocoder(&self) -> Result<Box<dyn Vocoder>, VocoderError> {
        match self.backend {
            Backend::Ambeserver => crate::open_ambeserver(self.parse_ambeserver()?, self.gain()),
            Backend::Thumbdv => {
                #[cfg(feature = "thumbdv")]
                {
                    crate::open_thumbdv(&self.serial, self.baud, self.gain())
                }
                #[cfg(not(feature = "thumbdv"))]
                Err(VocoderError::Init(
                    "thumbdv backend not compiled (build with --features thumbdv)".into(),
                ))
            }
            Backend::Mbelib => {
                #[cfg(feature = "mbelib")]
                {
                    Ok(crate::open_mbelib())
                }
                #[cfg(not(feature = "mbelib"))]
                Err(VocoderError::Init(
                    "mbelib backend not compiled (build with --features mbelib)".into(),
                ))
            }
        }
    }

    /// Open as a low-level `ChipClient`.  Gain is *not* auto-applied:
    /// the caller likely calls `reset()` mid-session, which wipes
    /// gain.  Use `apply_gain` after each reset instead.
    pub fn open_chip_client(&self) -> Result<Box<dyn ChipClient>, VocoderError> {
        match self.backend {
            Backend::Ambeserver => Ok(Box::new(AmbeServerClient::connect(
                self.parse_ambeserver()?,
            )?)),
            Backend::Thumbdv => {
                #[cfg(feature = "thumbdv")]
                {
                    Ok(Box::new(crate::chip::ThumbDvClient::open(
                        &self.serial,
                        self.baud,
                    )?))
                }
                #[cfg(not(feature = "thumbdv"))]
                Err(VocoderError::Init(
                    "thumbdv backend not compiled (build with --features thumbdv)".into(),
                ))
            }
            Backend::Mbelib => Err(VocoderError::Unsupported(
                "mbelib has no chip client (decode-only)",
            )),
        }
    }

    /// Apply configured gain to a `ChipClient`.  No-op when neither
    /// `--gain-in` nor `--gain-out` was given.  Call after each
    /// `reset()` since reset clears gain.
    pub fn apply_gain(&self, client: &mut dyn ChipClient) -> Result<(), VocoderError> {
        if let Some((i, o)) = self.gain() {
            client.set_gain(i, o)?;
        }
        Ok(())
    }
}
