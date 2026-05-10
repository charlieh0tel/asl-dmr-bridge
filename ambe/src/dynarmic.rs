//! Dynarmic-emulated MD380 firmware vocoder backend.  Wraps the
//! `dynarmic-sys` FFI in a process-global mutex (the dynarmic JIT
//! holds a single emulator instance that is not thread-safe) and
//! exposes a `Vocoder` impl.

use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::OnceLock;

use crate::SILENCE_FRAME;
use crate::Vocoder;
use crate::VocoderError;
use dv3000_wire::AMBE_FRAME_SIZE;
use dv3000_wire::AmbeFrame;
use dv3000_wire::PCM_SAMPLES;
use dv3000_wire::PcmFrame;
use tracing::error;

static CODEC: OnceLock<Mutex<()>> = OnceLock::new();

fn lock() -> MutexGuard<'static, ()> {
    let m = CODEC.get_or_init(|| {
        let rc = unsafe { dynarmic_sys::md380_init() };
        if rc != 0 {
            error!(rc, "md380_init failed");
        }
        assert_eq!(rc, 0, "md380_init failed");
        // Pre-warm the dynarmic JIT for both encode and decode paths.
        // The first call to either otherwise pays a multi-second
        // compile cost that hangs server clients with sub-second
        // timeouts on the first frame.  Re-init afterwards so the
        // warm-up doesn't leak predictor state to the caller.
        let mut ambe = [0u8; AMBE_FRAME_SIZE];
        let pcm_in = [0i16; PCM_SAMPLES];
        let mut pcm_out = [0i16; PCM_SAMPLES];
        unsafe { dynarmic_sys::md380_encode_fec(ambe.as_mut_ptr(), pcm_in.as_ptr()) };
        unsafe { dynarmic_sys::md380_decode_fec(ambe.as_ptr(), pcm_out.as_mut_ptr()) };
        let rc = unsafe { dynarmic_sys::md380_init() };
        if rc != 0 {
            error!(rc, "md380_init failed after warm-up");
        }
        assert_eq!(rc, 0, "md380_init failed after warm-up");
        Mutex::new(())
    });
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) struct DynarmicVocoder {
    in_db: dsp::dB,
    out_db: dsp::dB,
}

impl DynarmicVocoder {
    pub(crate) fn new() -> Self {
        Self {
            in_db: dsp::dB::UNITY,
            out_db: dsp::dB::UNITY,
        }
    }
}

impl Vocoder for DynarmicVocoder {
    fn encode(&mut self, pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError> {
        let mut scaled = *pcm;
        self.in_db.apply(&mut scaled);
        let _g = lock();
        let mut ambe = [0u8; AMBE_FRAME_SIZE];
        unsafe { dynarmic_sys::md380_encode_fec(ambe.as_mut_ptr(), scaled.as_ptr()) };
        Ok(ambe)
    }

    fn decode(&mut self, frame: Option<&AmbeFrame>) -> Result<PcmFrame, VocoderError> {
        // Erasure: feed the channel-coded frame-repeat sentinel; the
        // emulated firmware emits its predictor's frame-repeat (the
        // prior synthesized frame again).  Matches the trait contract
        // for chip backends.
        let coded = frame.copied().unwrap_or(*SILENCE_FRAME);
        let _g = lock();
        let mut pcm = [0i16; PCM_SAMPLES];
        unsafe { dynarmic_sys::md380_decode_fec(coded.as_ptr(), pcm.as_mut_ptr()) };
        self.out_db.apply(&mut pcm);
        Ok(pcm)
    }

    fn reset(&mut self) {
        let _g = lock();
        let rc = unsafe { dynarmic_sys::md380_init() };
        if rc != 0 {
            error!(rc, "md380_init failed on reset");
        }
        assert_eq!(rc, 0, "md380_init failed on reset");
    }

    fn set_gain(&mut self, in_db: dsp::dB, out_db: dsp::dB) -> Result<(), VocoderError> {
        self.in_db = in_db;
        self.out_db = out_db;
        Ok(())
    }
}
