pub(crate) mod ambeserver;
// codeword is only used by the mbelib backend (deinterleave + 49-bit
// source extraction); gate it on the same feature so non-mbelib
// builds don't generate dead-code warnings.
#[cfg(feature = "mbelib")]
pub(crate) mod codeword;
pub(crate) mod dv3000;
#[cfg(feature = "mbelib")]
pub(crate) mod mbelib;
#[cfg(feature = "thumbdv")]
pub(crate) mod thumbdv;

// `test_harness` + `test_vectors` exist only to feed the goldens'
// integration tests + `gen_golden`; gated behind a dedicated
// `testing` feature so the crate's public API stays minimal.
#[cfg(any(feature = "testing", test))]
pub mod test_harness;
#[cfg(any(feature = "testing", test))]
pub mod test_vectors;

/// PCM frame: 160 samples, 20 ms at 8 kHz.
pub const PCM_SAMPLES: usize = 160;

/// AMBE+2 frame: 9 bytes (72 bits).
pub const AMBE_FRAME_SIZE: usize = 9;

/// AMBE+2 frame: 72 bits.
pub(crate) const AMBE_BITS: u8 = (AMBE_FRAME_SIZE * 8) as u8;

const _: () = assert!(PCM_SAMPLES <= u8::MAX as usize);
const _: () = assert!(AMBE_FRAME_SIZE * 8 <= u8::MAX as usize);

/// PCM sample buffer type.
pub type PcmFrame = [i16; PCM_SAMPLES];

/// AMBE+2 encoded frame type.
pub type AmbeFrame = [u8; AMBE_FRAME_SIZE];

#[derive(Debug, thiserror::Error)]
pub enum VocoderError {
    #[error("encode failed: {0}")]
    Encode(String),
    #[error("decode failed: {0}")]
    Decode(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("DV3000 parse error: {0}")]
    Parse(#[from] dv3000::ParseError),
    #[error("device init failed: {0}")]
    Init(String),
    /// Operation is not supported by this backend.  mbelib returns
    /// this for `encode`, since the software vocoder is decode-only.
    #[error("operation unsupported by this backend: {0}")]
    Unsupported(&'static str),
}

/// Vocoder backend trait for PCM <-> AMBE+2 transcoding.
pub trait Vocoder: Send {
    fn encode(&mut self, pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError>;
    fn decode(&mut self, ambe: &AmbeFrame) -> Result<PcmFrame, VocoderError>;
}

// Factory functions are the only way to construct a backend.  The
// concrete backend types stay `pub(crate)` so the crate's public
// surface is just the trait + factories + supporting types -- no
// re-exports of internal structs.

/// Construct an AMBEserver UDP-proxy backend connected to `addr`.
/// `gain_db` is `(input_db, output_db)`, each clamped to [-90, 90].
/// `None` leaves the chip at default 0 dB.
pub fn open_ambeserver(
    addr: std::net::SocketAddr,
    gain_db: Option<(i8, i8)>,
) -> Result<Box<dyn Vocoder>, VocoderError> {
    Ok(Box::new(ambeserver::AmbeServer::connect(addr, gain_db)?))
}

/// Construct a ThumbDV (DVSI AMBE-3000R over FTDI serial) backend.
/// `baud` defaults to 460800 if `None`.  `gain_db` semantics match
/// `open_ambeserver`.
#[cfg(feature = "thumbdv")]
pub fn open_thumbdv(
    port: &str,
    baud: Option<u32>,
    gain_db: Option<(i8, i8)>,
) -> Result<Box<dyn Vocoder>, VocoderError> {
    Ok(Box::new(thumbdv::ThumbDv::open(port, baud, gain_db)?))
}

/// Construct an mbelib (software-only, decode-only) backend.
#[cfg(feature = "mbelib")]
pub fn open_mbelib() -> Box<dyn Vocoder> {
    Box::new(mbelib::Mbelib::new())
}
