//! IIR biquad filter (transposed Direct Form II) and a small fixed-N
//! cascade.  Used by the FM->DMR pre-encode filter chain; coefficients
//! are caller-supplied so this crate stays free of any specific filter
//! design.

const FULL_SCALE: f32 = 32768.0;

/// Single biquad section with normalized coefficients (a0 == 1).
/// `Biquad::new` divides through by a0 so callers can pass denormalized
/// coefficients directly from a filter design.
///
/// State is two floats per the transposed DF-II topology, which trades
/// one extra add per sample for better numerical stability than direct
/// DF-I at audio sample rates.
#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    pub fn new(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Fixed-N biquad cascade.  Sections run in order; output of section i
/// feeds section i+1.
#[derive(Debug, Clone, Copy)]
pub struct BiquadCascade<const N: usize> {
    sections: [Biquad; N],
}

impl<const N: usize> BiquadCascade<N> {
    pub fn new(sections: [Biquad; N]) -> Self {
        Self { sections }
    }

    pub fn reset(&mut self) {
        for s in &mut self.sections {
            s.reset();
        }
    }

    pub fn process(&mut self, x: f32) -> f32 {
        let mut y = x;
        for s in &mut self.sections {
            y = s.process(y);
        }
        y
    }

    /// Filter a buffer of int16 PCM in place.  Each sample is scaled
    /// to [-1, 1], run through the cascade, and re-quantized with
    /// round-to-nearest and saturation: filter gain > 1 clamps to
    /// the i16 range instead of wrapping.
    pub fn process_pcm(&mut self, samples: &mut [i16]) {
        for s in samples.iter_mut() {
            let x = f32::from(*s) / FULL_SCALE;
            let y = (self.process(x) * FULL_SCALE).round();
            *s = y.clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
        }
    }
}

/// 3-section voice-band filter for the FM->DMR pre-encode path:
/// 4th-order Butterworth high-pass at 250 Hz cascaded with a 2nd-order
/// Butterworth low-pass at 3000 Hz, sample rate 8000 Hz.
///
/// To regenerate (scipy):
/// ```python
/// from scipy.signal import butter, tf2sos
/// hp = butter(4, 250, btype='high', fs=8000, output='sos')
/// lp = butter(2, 3000, btype='low',  fs=8000, output='sos')
/// sos = np.vstack([hp, lp])  # rows: b0 b1 b2 a0 a1 a2
/// ```
///
// [TODO] @charlieh0tel: add a narrow CTCSS-PL notch (single biquad,
// Q ~= 12) keyed on the operator's PL frequency.  Targets the PL
// carrier without disturbing the voice band -- a less invasive
// alternative to the HP/LP shaping above.
#[expect(
    clippy::excessive_precision,
    reason = "coefficients copied verbatim from scipy output, kept as fixed-precision decimals for direct comparison"
)]
pub fn pre_encode_voice_8khz() -> BiquadCascade<3> {
    BiquadCascade::new([
        Biquad::new(0.773347, -1.546694, 0.773347, 1.000000, -1.662010, 0.694571),
        Biquad::new(1.000000, -2.000000, 1.000000, 1.000000, -1.825298, 0.861057),
        Biquad::new(0.569036, 1.138071, 0.569036, 1.000000, 0.942809, 0.333333),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_biquad_preserves_signal() {
        let mut b = Biquad::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
        for x in [0.0_f32, 0.5, -0.25, 1.0, -1.0] {
            assert!((b.process(x) - x).abs() < 1e-6);
        }
    }

    #[test]
    fn reset_zeroes_state() {
        let mut b = Biquad::new(1.0, 0.5, 0.25, 1.0, -0.3, 0.1);
        for _ in 0..10 {
            b.process(0.5);
        }
        assert_ne!(b.z1, 0.0);
        b.reset();
        assert_eq!(b.z1, 0.0);
        assert_eq!(b.z2, 0.0);
    }

    #[test]
    fn cascade_passes_through_three_unit_sections() {
        let unit = Biquad::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);
        let mut c = BiquadCascade::new([unit; 3]);
        for x in [0.0_f32, 0.5, -0.25] {
            assert!((c.process(x) - x).abs() < 1e-6);
        }
    }

    #[test]
    fn process_pcm_clamps_overflow() {
        // b0=2 doubles the output; verify we clamp instead of wrapping.
        let mut c = BiquadCascade::new([Biquad::new(2.0, 0.0, 0.0, 1.0, 0.0, 0.0)]);
        let mut buf = [20000i16, -20000];
        c.process_pcm(&mut buf);
        assert_eq!(buf[0], i16::MAX);
        assert_eq!(buf[1], i16::MIN);
    }

    #[test]
    fn pre_encode_voice_band_response() {
        // Drive a steady sine through the cascade for a few hundred
        // ms, measure RMS of the settled tail vs. input amplitude:
        //   100 Hz   -- well below HP cutoff: deep attenuation
        //   1000 Hz  -- mid-band: passes
        //   3800 Hz  -- above LP cutoff (close to Nyquist): attenuated
        let fs = 8000.0;
        let n = 4000;
        let amplitude = 0.5;
        let rms = |xs: &[f32]| {
            let mss = xs.iter().map(|&v| v * v).sum::<f32>() / xs.len() as f32;
            mss.sqrt()
        };
        let measure = |freq: f32| {
            let mut f = pre_encode_voice_8khz();
            let mut tail = Vec::with_capacity(n / 2);
            for i in 0..n {
                let t = i as f32 / fs;
                let x = amplitude * (std::f32::consts::TAU * freq * t).sin();
                let y = f.process(x);
                if i >= n / 2 {
                    tail.push(y);
                }
            }
            rms(&tail) / amplitude
        };
        let r_100 = measure(100.0);
        let r_1000 = measure(1000.0);
        let r_3800 = measure(3800.0);
        assert!(r_100 < 0.05, "100Hz ratio {r_100} not <0.05");
        assert!(r_1000 > 0.5, "1kHz ratio {r_1000} not >0.5");
        // Measured ~0.026 (-32 dB).  Bound at 0.05 (-26 dB) gives ~2x
        // slack so coefficient retuning has room without weakening
        // the LP into voice band.
        assert!(r_3800 < 0.05, "3800Hz ratio {r_3800} not <0.05");
    }

    /// Long-run stability: feed a steady tone for a million samples
    /// (~2 minutes at 8 kHz) and assert the cascade never produces
    /// NaN/Inf and stays bounded.  Catches accidental regressions to
    /// a less-stable topology that would slowly accumulate error.
    #[test]
    fn pre_encode_long_run_stays_bounded() {
        let mut f = pre_encode_voice_8khz();
        let fs = 8000.0_f32;
        let freq = 1000.0_f32;
        let amp = 0.5_f32;
        let n = 1_000_000;
        let mut peak = 0.0_f32;
        for i in 0..n {
            let t = i as f32 / fs;
            let x = amp * (std::f32::consts::TAU * freq * t).sin();
            let y = f.process(x);
            assert!(y.is_finite(), "non-finite output at sample {i}: {y}");
            peak = peak.max(y.abs());
        }
        // Filter gain at 1 kHz is ~0.7 (passband, slight rolloff);
        // peak well under 1.0 confirms no slow drift.
        assert!(peak < 1.0, "peak {peak} suspiciously large");
    }
}
