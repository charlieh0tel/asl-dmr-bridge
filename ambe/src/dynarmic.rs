//! Dynarmic-emulated MD380 firmware vocoder backend.  Wraps the
//! `dynarmic-sys` FFI in a process-global mutex (the dynarmic JIT
//! holds a single emulator instance that is not thread-safe) and
//! exposes a `Vocoder` impl.

use std::f64::consts::TAU;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::OnceLock;

use crate::SILENCE_FRAME;
use crate::Vocoder;
use crate::VocoderError;
use dmr_types::AMBE_FRAME_SIZE;
use dmr_types::AmbeFrame;
use dmr_types::PCM_SAMPLES;
use dmr_types::PcmFrame;
use tracing::error;

static CODEC: OnceLock<Mutex<()>> = OnceLock::new();

fn lock() -> MutexGuard<'static, ()> {
    let m = CODEC.get_or_init(|| {
        let rc = unsafe { dynarmic_sys::md380_init() };
        if rc != 0 {
            error!(rc, "md380_init failed");
        }
        assert_eq!(rc, 0, "md380_init failed");
        // Pre-warm the dynarmic JIT for both encode and decode paths so
        // the first real call does not pay the JIT compile cost.
        // Silence alone takes a short path through the firmware; the
        // voiced-speech paths stay uncompiled and the first real call
        // stalls (hundreds of ms on a Pi 4).  Trailing silence lets
        // the predictor decay so voiced residue doesn't leak into the
        // first real call.
        let mut warm = warm_up_signal();
        warm.extend([[0i16; PCM_SAMPLES]; WARM_UP_SETTLE_FRAMES]);
        for pcm_in in &warm {
            let mut ambe = [0u8; AMBE_FRAME_SIZE];
            let mut pcm_out = [0i16; PCM_SAMPLES];
            unsafe { dynarmic_sys::md380_encode_fec(ambe.as_mut_ptr(), pcm_in.as_ptr()) };
            unsafe { dynarmic_sys::md380_decode_fec(ambe.as_ptr(), pcm_out.as_mut_ptr()) };
        }
        Mutex::new(())
    });
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Frames of synthetic voiced audio fed through the codec at init.
const WARM_UP_FRAMES: usize = 50;

/// Silence frames after the warm-up signal; 4 suffice in testing.
const WARM_UP_SETTLE_FRAMES: usize = 8;

const SAMPLE_RATE_HZ: f64 = 8000.0;

/// Pitch sweep range and rate (radians per frame) for the warm-up signal.
const WARM_UP_F0_MIN_HZ: f64 = 90.0;
const WARM_UP_F0_SPAN_HZ: f64 = 160.0;
const WARM_UP_F0_SWEEP_RATE: f64 = 0.37;

/// Peak amplitude sweep range and rate (radians per frame).
const WARM_UP_AMP_MIN: f64 = 4000.0;
const WARM_UP_AMP_SPAN: f64 = 8000.0;
const WARM_UP_AMP_SWEEP_RATE: f64 = 0.21;

/// Harmonics above this are omitted (AMBE+2 band edge is ~4 kHz).
const WARM_UP_MAX_HARMONIC_HZ: f64 = 3600.0;

/// Formants as (center, half-width, weight).
const WARM_UP_FORMANTS: [(f64, f64, f64); 2] = [(700.0, 300.0, 1.0), (1800.0, 400.0, 0.5)];

const WARM_UP_VOICE_GAIN: f64 = 0.3;
const WARM_UP_NOISE_GAIN: f64 = 0.05;

/// Speech-like test signal: a formant-shaped harmonic series with
/// sweeping pitch and level plus a little noise, so the encoder
/// exercises its voiced, unvoiced, and gain-tracking paths.
fn warm_up_signal() -> Vec<PcmFrame> {
    // Numerical Recipes LCG; any deterministic noise source works.
    let mut seed: u32 = 0x1234_5678;
    let mut phase = 0.0f64;
    (0..WARM_UP_FRAMES)
        .map(|frame| {
            let sweep = |rate: f64| (frame as f64 * rate).sin() * 0.5 + 0.5;
            let f0 = WARM_UP_F0_MIN_HZ + WARM_UP_F0_SPAN_HZ * sweep(WARM_UP_F0_SWEEP_RATE);
            let amp = WARM_UP_AMP_MIN + WARM_UP_AMP_SPAN * sweep(WARM_UP_AMP_SWEEP_RATE);
            let mut out = [0i16; PCM_SAMPLES];
            for sample in &mut out {
                phase = (phase + f0 / SAMPLE_RATE_HZ) % 1.0;
                let voice: f64 = (1..)
                    .map(|h| (h, f0 * f64::from(h)))
                    .take_while(|&(_, hz)| hz <= WARM_UP_MAX_HARMONIC_HZ)
                    .map(|(h, hz)| {
                        let weight: f64 = WARM_UP_FORMANTS
                            .iter()
                            .map(|&(center, width, w)| w / (1.0 + ((hz - center) / width).powi(2)))
                            .sum();
                        weight * (TAU * phase * f64::from(h)).sin()
                    })
                    .sum();
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let noise = f64::from(seed >> 16) / f64::from(1u32 << 15) - 1.0;
                let value = amp * (WARM_UP_VOICE_GAIN * voice + WARM_UP_NOISE_GAIN * noise);
                *sample = value.clamp(-f64::from(i16::MAX), f64::from(i16::MAX)) as i16;
            }
            out
        })
        .collect()
}

pub(crate) struct DynarmicVocoder {
    in_db: dsp::dB,
    out_db: dsp::dB,
}

impl DynarmicVocoder {
    pub(crate) fn new() -> Self {
        // Do NOT call lock() here; defer to warm_cache() so the
        // JIT init cost is paid only when the backend is first used.
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

    fn warm_cache(&mut self) {
        drop(lock());
    }

    fn reset(&mut self) {
        // No-op: recreating the JIT per PTT causes fastmem SIGSEGV; codec converges within frames.
    }

    fn set_gain(&mut self, in_db: dsp::dB, out_db: dsp::dB) -> Result<(), VocoderError> {
        self.in_db = in_db;
        self.out_db = out_db;
        Ok(())
    }
}
