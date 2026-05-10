pub(crate) mod ambeserver;
pub mod chip;
pub mod cli;
#[cfg(feature = "dynarmic")]
pub(crate) mod dynarmic;
#[cfg(feature = "neural")]
pub(crate) mod neural;
#[cfg(feature = "thumbdv")]
pub(crate) mod thumbdv;
pub(crate) mod udp_dv;
pub mod voice_channel;

// `test_harness` + `test_vectors` exist only to feed the goldens'
// integration tests + `gen_golden`; gated behind a dedicated
// `testing` feature so the crate's public API stays minimal.
#[cfg(any(feature = "testing", test))]
pub mod test_harness;
#[cfg(any(feature = "testing", test))]
pub mod test_vectors;

use dv3000_wire::AmbeFrame;
use dv3000_wire::PcmFrame;

/// Channel-coded AMBE+2 frame-repeat sentinel (`b0=124`, others 0):
/// a valid silent frame for warm-up / hang padding.
pub static SILENCE_FRAME: std::sync::LazyLock<AmbeFrame> = std::sync::LazyLock::new(|| {
    // 124 = 0b1111100, MSB-first into b0 positions [0,1,2,3,37,38,39].
    let mut ambe_d = [0u8; 49];
    for &p in &[0usize, 1, 2, 3, 37] {
        ambe_d[p] = 1;
    }
    voice_channel::encode_from_ambe_d(&ambe_d)
});

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
    Parse(#[from] dv3000_wire::ParseError),
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

    /// Decode one AMBE frame to PCM.  `Some(&ambe)` is a real
    /// received frame; `None` is a known-missing slot in the stream
    /// (packet drop / erasure).  The current backends (chip + the
    /// dynarmic-emulated MD380 firmware) handle erasure by feeding
    /// the channel-coded frame-repeat sentinel into the codec, so
    /// the predictor emits its prior synthesized frame again;
    /// consecutive `None`s repeat that frame.  Per-stream decoder
    /// state advances either way so the next real frame isn't
    /// decoded against stale history.
    fn decode(&mut self, ambe: Option<&AmbeFrame>) -> Result<PcmFrame, VocoderError>;

    /// Reset transient per-stream state -- decoder predictor /
    /// smoother history, encoder lookahead buffers -- so a new
    /// stream does not inherit prior history.  Construction-time
    /// calibration (chip RATEP / gain, model weights) is preserved.
    ///
    /// Called at every PTT-up boundary on both TX and RX paths, so
    /// impls must be non-blocking (no I/O, no chip round-trips).
    /// Output is only required to be logically reset, not bit-equal
    /// to a freshly-constructed instance.  A no-op is valid when
    /// the backend exposes no per-stream state.
    fn reset(&mut self) {}

    /// Set static input/output gain.  Last call wins; `dB::UNITY`
    /// is a no-op.
    fn set_gain(&mut self, in_db: dsp::dB, out_db: dsp::dB) -> Result<(), VocoderError>;
}

// Factory functions are the only way to construct a backend.  The
// concrete backend types stay `pub(crate)` so the crate's public
// surface is just the trait + factories + supporting types -- no
// re-exports of internal structs.

/// Construct an AMBEserver UDP-proxy backend connected to `addr`.
/// Use `Vocoder::set_gain` to apply non-default gain.
pub fn open_ambeserver(addr: std::net::SocketAddr) -> Result<Box<dyn Vocoder>, VocoderError> {
    Ok(Box::new(ambeserver::AmbeServer::connect(addr)?))
}

/// Construct a ThumbDV (DVSI AMBE-3000R over FTDI serial) backend.
/// `baud` defaults to 460800 if `None`.  Use `Vocoder::set_gain` to
/// apply non-default gain.
#[cfg(feature = "thumbdv")]
pub fn open_thumbdv(port: &str, baud: Option<u32>) -> Result<Box<dyn Vocoder>, VocoderError> {
    Ok(Box::new(thumbdv::ThumbDv::open(port, baud)?))
}

/// Construct a dynarmic (software, JIT-emulated MD380 firmware)
/// backend.
#[cfg(feature = "dynarmic")]
pub fn open_dynarmic() -> Box<dyn Vocoder> {
    Box::new(dynarmic::DynarmicVocoder::new())
}

/// Construct a neural-vocoder backend from an ONNX model file.
/// Encode is neural; decode delegates to dynarmic.
#[cfg(feature = "neural")]
pub fn open_neural(model_path: &std::path::Path) -> Result<Box<dyn Vocoder>, VocoderError> {
    Ok(Box::new(neural::NeuralVocoder::open(model_path)?))
}

/// Like `open_neural`, but the caller supplies the decoder backend.
/// Encode is always neural; decode goes to whatever `decoder` does.
#[cfg(feature = "neural")]
pub fn open_neural_with_decoder(
    model_path: &std::path::Path,
    decoder: Box<dyn Vocoder>,
) -> Result<Box<dyn Vocoder>, VocoderError> {
    Ok(Box::new(neural::NeuralVocoder::open_with_decoder(
        model_path, decoder,
    )?))
}

/// Diagnostic handle around the neural encoder that exposes the
/// raw 9-int VQ row instead of channel-coded bytes.  Used by parity
/// harnesses comparing tract output against a PT-canonical reference.
#[cfg(feature = "neural")]
pub struct NeuralEncoder(neural::NeuralVocoder);

#[cfg(feature = "neural")]
impl NeuralEncoder {
    pub fn open(model_path: &std::path::Path) -> Result<Self, VocoderError> {
        Ok(Self(neural::NeuralVocoder::open(model_path)?))
    }

    /// `Ok(None)` until the warm-up window fills; then `Ok(Some(vq))`
    /// per frame, where `vq[i]` is the argmax of the `i`-th logit head
    /// in `FIELDS_DMR_3600X2450` order (`b0..b8`).
    pub fn encode_vq(&mut self, pcm: &PcmFrame) -> Result<Option<[u16; 9]>, VocoderError> {
        self.0.encode_vq(pcm)
    }

    /// Snapshot of the current model-input slice (the oldest
    /// `pcm_input_samples` of the streaming buffer).  Empty until
    /// the warm-up window fills.  For parity-debugging.
    pub fn current_input_slice(&self) -> Vec<i16> {
        self.0.current_input_slice()
    }
}
