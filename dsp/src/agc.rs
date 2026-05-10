//! Per-call automatic gain control on the USRP-tx (digital -> analog)
//! path.
//!
//! Inbound DMR audio levels vary widely -- Brandmeister aggregates
//! many radios with different mic gains, codec versions, and on-air
//! signal strength.  AGC tracks a peak envelope and steers gain toward
//! a target dBFS so the FM listener hears consistent loudness.
//!
//! Standard one-pole envelope follower with asymmetric time
//! constants:
//!   - signal getting louder -> envelope rises fast (attack), target
//!     gain drops fast, we reduce gain quickly to avoid clipping.
//!   - signal getting quieter -> envelope falls slow (release), gain
//!     comes back up slowly to avoid pumping on speech pauses.
//!
//! `reset()` zeros state at call boundaries so each new talker starts
//! from a clean baseline rather than inheriting the previous call's
//! gain.  Off by default.

use std::collections::VecDeque;
use std::time::Duration;

/// 8 kHz / 20 ms per USRP voice frame -- matches `usrp_wire`.  Local
/// constant rather than an import to keep AGC math self-contained
/// and make the time-constant arithmetic obvious in tests.
const SAMPLE_RATE_HZ: f32 = 8000.0;

/// Look-ahead window for the peak limiter.  16 samples = 2 ms at
/// 8 kHz.  Smoother attack is matched to this window so gain
/// reduction reaches its minimum exactly when the predicted peak
/// arrives at the output, avoiding both an audible discontinuity
/// (instant attack) and a late-arriving GR (peak still clips).
const LOOKAHEAD_SAMPLES: usize = 16;

/// Output ceiling enforced by the limiter (linear).  1.0 = full
/// scale; the trailing `clamp(-1.0, 1.0)` after the limiter is a
/// safety belt that should never fire under correct operation.
const LIMITER_CEILING: f32 = 1.0;

/// 2^15 -- divide i16 by this to map i16::MIN onto -1.0 exactly.
/// Multiplying back relies on the `as i16` saturating cast for the
/// +1.0 -> 32768 case (out of i16 range, saturates to 32767).
const FULL_SCALE: f32 = 32768.0;

/// Static configuration for one AGC instance.
#[derive(Debug, Clone, Copy)]
pub struct AgcParams {
    /// Target peak in dBFS (negative; e.g. -6 leaves 6 dB headroom).
    pub target_dbfs: f32,
    /// One-pole time constant for the envelope's rise on louder
    /// signals.  Smaller = faster attack = better peak control,
    /// audible if too aggressive.
    pub attack: Duration,
    /// One-pole time constant for the envelope's fall on quieter
    /// signals.  Larger = smoother, less pumping.
    pub release: Duration,
    /// Cap on how much we amplify a quiet signal.  Prevents the AGC
    /// from boosting hum / noise floor when the input is silence.
    pub max_gain_db: f32,
    /// Below this envelope level, freeze gain and stop tracking.
    /// Prevents the AGC from chasing the noise floor during pauses
    /// and ramping the gain up to `max_gain_db`, which would slam
    /// background noise into the listener when speech resumes.
    pub noise_gate_dbfs: f32,
}

#[cfg(test)]
impl AgcParams {
    /// Sane defaults for DMR voice on an FM-side listener: -6 dBFS
    /// target with 10 ms attack, 200 ms release, 30 dB max boost.
    /// Test-only -- production builds AgcParams directly from
    /// config fields, so this would otherwise be dead code.
    fn default_voice() -> Self {
        Self {
            target_dbfs: -6.0,
            attack: Duration::from_millis(10),
            release: Duration::from_millis(200),
            max_gain_db: 12.0,
            noise_gate_dbfs: -50.0,
        }
    }
}

/// Per-call AGC behavior summary.  Populated as `process` runs;
/// drained + reset by `take_summary`.  All gains in linear units;
/// peaks in linear amplitude (`[0, 1]`) so the caller can format as
/// dB or raw.
#[derive(Debug, Clone, Copy, Default)]
pub struct AgcSummary {
    /// Total samples processed in this call.
    pub samples: u64,
    /// Samples where the noise-floor gate kept gain frozen.
    pub frozen_samples: u64,
    /// Min smoothed gain seen during the call (linear).  `None` if
    /// no samples processed.
    pub gain_min: Option<f32>,
    /// Max smoothed gain seen (linear).
    pub gain_max: Option<f32>,
    /// Sum of gains over all samples; divide by `samples` for mean.
    pub gain_sum: f32,
    /// Peak input amplitude (linear, in [0, 1]).
    pub peak_in: f32,
    /// Peak output amplitude (linear, in [0, 1]) post-clamp.
    pub peak_out: f32,
    /// Samples where the look-ahead limiter applied gain reduction
    /// (lookahead_gr < 1.0).  Stays 0 until the limiter ships.
    pub limited_samples: u64,
}

impl AgcSummary {
    /// Mean smoothed gain (linear).  Returns 1.0 (unity) if the
    /// summary is empty.
    pub fn gain_mean(&self) -> f32 {
        if self.samples == 0 {
            1.0
        } else {
            self.gain_sum / self.samples as f32
        }
    }
}

pub struct Agc {
    target: f32,
    max_gain: f32,
    noise_gate: f32,
    attack_alpha: f32,
    release_alpha: f32,
    envelope: f32,
    gain: f32,
    /// Look-ahead ring buffer of the most recent input samples
    /// (post-`/FULL_SCALE` normalization).  Per-iteration scan
    /// across this buffer drives the limiter's GR target, then we
    /// pop the oldest sample and apply gain to it -- so the GR is
    /// applied in time-coincidence with the peak that triggered it.
    lookahead_buf: VecDeque<f32>,
    /// Current smoothed gain reduction (linear, in (0, 1]).  1.0 =
    /// no reduction.  Drops on attack, rises on release.
    lookahead_gr: f32,
    /// One-pole alpha for the limiter's attack ramp; matched to the
    /// lookahead window so GR reaches its minimum exactly when the
    /// triggering peak arrives at the output.
    lookahead_attack_alpha: f32,
    /// One-pole alpha for the limiter's release back to unity.
    lookahead_release_alpha: f32,
    summary: AgcSummary,
}

impl Agc {
    pub fn new(params: AgcParams) -> Self {
        Self {
            target: db_to_linear(params.target_dbfs),
            max_gain: db_to_linear(params.max_gain_db),
            noise_gate: db_to_linear(params.noise_gate_dbfs),
            attack_alpha: alpha_for(params.attack),
            release_alpha: alpha_for(params.release),
            envelope: 0.0,
            gain: 1.0,
            lookahead_buf: VecDeque::with_capacity(LOOKAHEAD_SAMPLES + 1),
            lookahead_gr: 1.0,
            lookahead_attack_alpha: alpha_for(Duration::from_micros(1500)),
            lookahead_release_alpha: alpha_for(Duration::from_millis(50)),
            summary: AgcSummary::default(),
        }
    }

    /// Clear envelope, gain, and the look-ahead pipeline.  Call at
    /// call boundaries so the next talker starts from a neutral
    /// state.  Does NOT clear the summary -- callers grab it via
    /// `take_summary` first.
    pub fn reset(&mut self) {
        self.envelope = 0.0;
        self.gain = 1.0;
        self.lookahead_buf.clear();
        self.lookahead_gr = 1.0;
    }

    /// Drain the per-call summary and clear it for the next call.
    pub fn take_summary(&mut self) -> AgcSummary {
        std::mem::take(&mut self.summary)
    }

    /// Apply AGC in place to one PCM frame.  Each i16 sample is
    /// rescaled to `[-1, 1]`, fed through the envelope follower and
    /// gain smoother, queued into the look-ahead buffer, and the
    /// oldest queued sample is emitted with `gain * lookahead_gr`
    /// applied.  Output is clamped to `[-1, 1]` as a safety belt
    /// (the limiter math should keep it in range on its own) and
    /// re-quantized to i16.
    ///
    /// Output is delayed by `LOOKAHEAD_SAMPLES` relative to input.
    /// The first `LOOKAHEAD_SAMPLES` slots after construction or
    /// `reset()` emit zero (buffer warmup); the trailing
    /// `LOOKAHEAD_SAMPLES` of input still in the buffer at
    /// `reset()` are dropped.  Both windows are 2 ms at 8 kHz --
    /// well below voice intelligibility thresholds.
    pub fn process(&mut self, samples: &mut [i16]) {
        for s in samples.iter_mut() {
            let x = f32::from(*s) / FULL_SCALE;
            let abs_x = x.abs();
            self.summary.samples += 1;
            if abs_x > self.summary.peak_in {
                self.summary.peak_in = abs_x;
            }

            // Envelope follower: fast attack, slow release.
            let env_alpha = if abs_x > self.envelope {
                self.attack_alpha
            } else {
                self.release_alpha
            };
            self.envelope += env_alpha * (abs_x - self.envelope);

            // Noise-floor gate on the instantaneous sample: when |x|
            // is below the gate we freeze gain.  Speech samples sit
            // well above the gate so the slow gain follower tracks
            // normally; pauses and noise consistently fall below it
            // and the gain stays put rather than chasing the noise
            // floor up toward max_gain.  The envelope keeps updating
            // either way so it stays current for the next syllable.
            //
            // Correctness depends on `release_alpha` being slow:
            // during a long gated stretch the envelope decays toward
            // 0, so the FIRST unmuted sample after silence has
            // target_gain = target / tiny_envelope clamped to
            // max_gain.  With release_alpha ~1.25e-4 the actual gain
            // barely moves on that single sample.  A faster release
            // would let it lurch up before the gate has a chance to
            // re-fire on a louder syllable.
            if abs_x >= self.noise_gate {
                let target_gain = (self.target / self.envelope.max(1e-6)).min(self.max_gain);
                // Smooth gain asymmetrically: target below current
                // (signal louder than expected) -> attack fast;
                // target above current (signal quieter) -> release
                // slow.
                let gain_alpha = if target_gain < self.gain {
                    self.attack_alpha
                } else {
                    self.release_alpha
                };
                self.gain += gain_alpha * (target_gain - self.gain);
            } else {
                self.summary.frozen_samples += 1;
            }

            // Track gain stats AFTER any update on this sample so
            // we record what's actually applied.
            self.summary.gain_sum += self.gain;
            self.summary.gain_min = Some(match self.summary.gain_min {
                Some(m) => m.min(self.gain),
                None => self.gain,
            });
            self.summary.gain_max = Some(match self.summary.gain_max {
                Some(m) => m.max(self.gain),
                None => self.gain,
            });

            // Look-ahead limiter.  Push the current sample, scan
            // the window for the largest |x| we will see in the
            // next LOOKAHEAD_SAMPLES, derive the gain reduction
            // needed to keep `max_future * gain` at or below the
            // ceiling, smooth GR with attack matched to the
            // window, and emit the oldest delayed sample.  Using
            // current `gain` to predict future output is fine: the
            // gain follower's 200 ms release means `gain` moves by
            // well under 1% across the 2 ms lookahead window.
            self.lookahead_buf.push_back(x);
            if self.lookahead_buf.len() <= LOOKAHEAD_SAMPLES {
                *s = 0;
                continue;
            }
            let max_future = self
                .lookahead_buf
                .iter()
                .map(|v| v.abs())
                .fold(0.0_f32, f32::max);
            let predicted = max_future * self.gain;
            let gr_req = if predicted > LIMITER_CEILING {
                LIMITER_CEILING / predicted
            } else {
                1.0
            };
            if gr_req < self.lookahead_gr {
                self.lookahead_gr += self.lookahead_attack_alpha * (gr_req - self.lookahead_gr);
            } else {
                self.lookahead_gr += self.lookahead_release_alpha * (1.0 - self.lookahead_gr);
                self.lookahead_gr = self.lookahead_gr.min(1.0);
            }
            if self.lookahead_gr < 1.0 {
                self.summary.limited_samples += 1;
            }
            // Pop the oldest sample (post-LOOKAHEAD push, the
            // buffer is one over capacity; pop_front returns
            // Some).  Apply gain + GR; the trailing clamp is a
            // safety belt that should never fire under correct
            // limiter operation.
            let oldest = self
                .lookahead_buf
                .pop_front()
                .expect("buffer is non-empty post-push");
            let y = (oldest * self.gain * self.lookahead_gr).clamp(-1.0, 1.0);
            let abs_y = y.abs();
            if abs_y > self.summary.peak_out {
                self.summary.peak_out = abs_y;
            }
            // i16 cast saturates at +/-32767; the clamp keeps `y`
            // in [-1, 1] so the cast never saturates in practice.
            *s = (y * FULL_SCALE) as i16;
        }
    }
}

/// dB to linear amplitude.
fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// One-pole filter coefficient for a target time constant.
/// `alpha = 1 - exp(-T_sample / tau)` where T_sample = 1 / 8 kHz.
/// Smaller tau -> larger alpha -> faster response.  A `tau = 0`
/// returns alpha = 1 (instantaneous), which is fine.
fn alpha_for(tau: Duration) -> f32 {
    let tau_secs = tau.as_secs_f32();
    if tau_secs <= 0.0 {
        return 1.0;
    }
    let samples = SAMPLE_RATE_HZ * tau_secs;
    1.0 - (-1.0 / samples).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a constant amplitude through the AGC long enough for it to
    /// converge and return the final per-sample peak.
    fn run_constant(agc: &mut Agc, level_dbfs: f32, frames: usize) -> f32 {
        let amp = (db_to_linear(level_dbfs) * FULL_SCALE) as i16;
        let mut buf = [amp; 160];
        let mut last_peak: u16 = 0;
        for _ in 0..frames {
            buf.fill(amp);
            agc.process(&mut buf);
            last_peak = buf
                .iter()
                .copied()
                .map(i16::unsigned_abs)
                .max()
                .unwrap_or(0);
        }
        f32::from(last_peak) / FULL_SCALE
    }

    #[test]
    fn convergence_brings_quiet_input_up_to_target() {
        // -12 dBFS input is within max_gain_db (12 dB) of the
        // target (-6 dBFS), so AGC can fully reach target.
        let mut agc = Agc::new(AgcParams::default_voice());
        // 200 frames * 20 ms = 4 s, plenty for 200 ms release to settle.
        let peak = run_constant(&mut agc, -12.0, 200);
        let peak_db = 20.0 * peak.log10();
        // Within 2 dB of target; the slow release means we approach
        // asymptotically.
        assert!(
            (peak_db - (-6.0)).abs() < 2.0,
            "expected ~-6 dBFS, got {peak_db:.2}"
        );
    }

    #[test]
    fn convergence_brings_loud_input_down_to_target() {
        // Loud input (-1 dBFS, near full-scale) should also converge
        // to target.
        let mut agc = Agc::new(AgcParams::default_voice());
        let peak = run_constant(&mut agc, -1.0, 50);
        let peak_db = 20.0 * peak.log10();
        // Loud input drives the attack path, faster convergence.
        assert!(
            (peak_db - (-6.0)).abs() < 2.0,
            "expected ~-6 dBFS, got {peak_db:.2}"
        );
    }

    #[test]
    fn silence_does_not_panic_or_overflow() {
        // All-zero input must be a no-op (no NaN, no boost-to-infinity).
        let mut agc = Agc::new(AgcParams::default_voice());
        let mut buf = [0i16; 160];
        for _ in 0..50 {
            agc.process(&mut buf);
        }
        assert!(buf.iter().all(|&s| s == 0));
    }

    #[test]
    fn reset_returns_to_neutral_state() {
        let mut agc = Agc::new(AgcParams::default_voice());
        let _ = run_constant(&mut agc, -12.0, 200);
        // Gain should be well above 1.0 after pulling -12 to -6.
        assert!(agc.gain > 1.5);
        agc.reset();
        assert_eq!(agc.envelope, 0.0);
        assert_eq!(agc.gain, 1.0);
    }

    #[test]
    fn max_gain_caps_silence_amplification() {
        // With max_gain_db = 0, AGC must never amplify.  Run a quiet
        // input and verify peak stays at or below the input level.
        // noise_gate_dbfs = -200 effectively disables the gate so
        // this test isolates the max_gain cap.
        let mut agc = Agc::new(AgcParams {
            target_dbfs: -6.0,
            attack: Duration::from_millis(10),
            release: Duration::from_millis(200),
            max_gain_db: 0.0,
            noise_gate_dbfs: -200.0,
        });
        let peak = run_constant(&mut agc, -30.0, 200);
        let peak_db = 20.0 * peak.log10();
        // Input was -30 dBFS; output peak must not exceed -30.
        assert!(peak_db <= -29.0, "max_gain=0 amplified anyway: {peak_db}");
    }

    #[test]
    fn noise_gate_freezes_gain_below_threshold() {
        // Input below the gate (-55 dBFS, gate at -50) must not pull
        // gain up.  Without the gate the envelope follower would
        // ramp gain to max_gain to chase the noise; with the gate
        // enabled the gain stays at its starting value.
        let mut agc = Agc::new(AgcParams::default_voice());
        let _ = run_constant(&mut agc, -55.0, 200);
        assert!(
            (agc.gain - 1.0).abs() < 0.01,
            "gate failed: gain drifted in noise: {}",
            agc.gain
        );
    }

    #[test]
    fn noise_gate_freezes_gain_after_speech() {
        // Pull gain up with speech-level input, then drop to noise
        // below the gate.  Gain must stay at the speech-time value
        // rather than ramping up further toward max_gain.
        let mut agc = Agc::new(AgcParams::default_voice());
        let _ = run_constant(&mut agc, -12.0, 200);
        let speech_gain = agc.gain;
        let _ = run_constant(&mut agc, -55.0, 200);
        assert!(
            (agc.gain - speech_gain).abs() < 0.05,
            "gate failed: gain {} drifted from speech-time {}",
            agc.gain,
            speech_gain,
        );
    }

    #[test]
    fn summary_tracks_gain_and_frozen_samples() {
        let mut agc = Agc::new(AgcParams::default_voice());
        // 100 frames of -12 dBFS converges gain above 1.0.
        let _ = run_constant(&mut agc, -12.0, 100);
        let s = agc.take_summary();
        assert_eq!(s.samples, 100 * 160);
        assert_eq!(s.frozen_samples, 0); // -12 dBFS is well above gate
        let gmax = s.gain_max.unwrap();
        let gmin = s.gain_min.unwrap();
        assert!(gmax > 1.0 && gmin >= 1.0);
        assert!(s.gain_mean() > 1.0);
        // No limiter yet -- field stays 0 until the look-ahead
        // limiter lands.
        assert_eq!(s.limited_samples, 0);
        // take_summary clears for next call.
        let s2 = agc.take_summary();
        assert_eq!(s2.samples, 0);
        assert!(s2.gain_min.is_none());
    }

    #[test]
    fn summary_counts_gated_samples() {
        let mut agc = Agc::new(AgcParams::default_voice());
        // -55 dBFS is below the -50 dBFS gate.
        let _ = run_constant(&mut agc, -55.0, 50);
        let s = agc.take_summary();
        assert_eq!(s.samples, 50 * 160);
        assert_eq!(s.frozen_samples, 50 * 160);
    }

    #[test]
    fn does_not_clip_full_scale_input() {
        // Full-scale input must not overflow i16 after AGC.  The
        // clamp before the i16 cast keeps the float in [-1, 1] so
        // the cast never saturate-overflows.  The actual assertion
        // is "didn't panic and produced non-trivial output".
        let mut agc = Agc::new(AgcParams::default_voice());
        let mut buf = [i16::MAX; 160];
        for _ in 0..10 {
            buf.fill(i16::MAX);
            agc.process(&mut buf);
        }
        // After convergence, full-scale input is attenuated to
        // ~target (-6 dBFS).  Just sanity-check that the output is
        // non-zero so we know the pipeline ran end-to-end.
        assert!(buf.iter().any(|&s| s != 0));
    }

    #[test]
    fn lookahead_caps_loud_transient_in_quiet_call() {
        // Drive a quiet sustained tone (gain ramps up toward
        // max_gain), then inject a transient that without the
        // limiter would push gain*x past full scale.  Output peak
        // must stay <= LIMITER_CEILING and limited_samples must
        // increment.
        let mut agc = Agc::new(AgcParams::default_voice());
        // 200 frames of -25 dBFS converges gain near max_gain.
        let _ = run_constant(&mut agc, -25.0, 200);
        // One frame of mostly quiet samples with a single near-
        // full-scale spike in the middle.
        let mut frame = [(db_to_linear(-25.0) * FULL_SCALE) as i16; 160];
        frame[80] = (db_to_linear(-1.0) * FULL_SCALE) as i16;
        agc.process(&mut frame);
        let peak = frame.iter().copied().map(i16::unsigned_abs).max().unwrap();
        let peak_lin = f32::from(peak) / FULL_SCALE;
        assert!(
            peak_lin <= LIMITER_CEILING + 1e-6,
            "limiter let peak through: {peak_lin}"
        );
        let s = agc.take_summary();
        assert!(
            s.limited_samples > 0,
            "limiter never engaged on a transient that would clip"
        );
    }

    #[test]
    fn lookahead_transparent_when_within_ceiling() {
        // Steady -20 dBFS through default AGC: gain pulls peaks
        // toward target (-6 dBFS), which is well under ceiling.
        // Limiter must never engage; limited_samples stays 0.
        let mut agc = Agc::new(AgcParams::default_voice());
        let _ = run_constant(&mut agc, -20.0, 200);
        let s = agc.take_summary();
        assert_eq!(
            s.limited_samples, 0,
            "limiter engaged on a signal that does not exceed ceiling"
        );
    }

    #[test]
    fn lookahead_buf_cleared_on_reset() {
        // Run loud input through the AGC, reset, then process
        // pure silence.  The output must not contain any echo
        // from the prior call's lookahead buffer.
        let mut agc = Agc::new(AgcParams::default_voice());
        let mut loud = [i16::MAX; 160];
        agc.process(&mut loud);
        agc.reset();
        let mut silence = [0i16; 160];
        agc.process(&mut silence);
        assert!(
            silence.iter().all(|&s| s == 0),
            "reset failed: prior call leaked into next via lookahead buffer"
        );
    }
}
