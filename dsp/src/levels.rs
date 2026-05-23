//! Per-call PCM level accumulator (peak, RMS, voiced-RMS in dBFS).
//! Diagnostic only -- bridge logs the summary at each call boundary.

const VOICED_GATE_DBFS: f64 = -40.0;
const FULL_SCALE: f64 = 32768.0;

#[derive(Default)]
pub struct LevelAccumulator {
    peak_abs: i32,
    sum_sq: u64,
    sample_count: u64,
    voiced_sum_sq: u64,
    voiced_sample_count: u64,
}

impl LevelAccumulator {
    pub fn add_frame(&mut self, frame: &[i16]) {
        if frame.is_empty() {
            return;
        }
        let mut frame_sum_sq: u64 = 0;
        let mut frame_peak: i32 = 0;
        for &s in frame {
            let abs = i32::from(s.unsigned_abs());
            if abs > frame_peak {
                frame_peak = abs;
            }
            let v = i64::from(s);
            frame_sum_sq = frame_sum_sq.saturating_add((v * v) as u64);
        }
        if frame_peak > self.peak_abs {
            self.peak_abs = frame_peak;
        }
        self.sum_sq = self.sum_sq.saturating_add(frame_sum_sq);
        self.sample_count = self.sample_count.saturating_add(frame.len() as u64);

        let frame_rms_lin = (frame_sum_sq as f64 / frame.len() as f64).sqrt();
        let frame_rms_dbfs = if frame_rms_lin > 0.0 {
            20.0 * (frame_rms_lin / FULL_SCALE).log10()
        } else {
            f64::NEG_INFINITY
        };
        if frame_rms_dbfs > VOICED_GATE_DBFS {
            self.voiced_sum_sq = self.voiced_sum_sq.saturating_add(frame_sum_sq);
            self.voiced_sample_count = self.voiced_sample_count.saturating_add(frame.len() as u64);
        }
    }

    /// `(peak_dbfs, rms_dbfs, voiced_rms_dbfs)`.  Any field is
    /// `f64::NEG_INFINITY` when nothing observed (no samples or all
    /// silent).
    #[must_use]
    pub fn summary(&self) -> (f64, f64, f64) {
        let peak = if self.peak_abs > 0 {
            20.0 * (f64::from(self.peak_abs) / FULL_SCALE).log10()
        } else {
            f64::NEG_INFINITY
        };
        let rms = rms_dbfs(self.sum_sq, self.sample_count);
        let voiced_rms = rms_dbfs(self.voiced_sum_sq, self.voiced_sample_count);
        (peak, rms, voiced_rms)
    }
}

/// Format a dBFS value for log output.  `-inf` (no samples / all
/// silent) renders as `-infdBFS`; NaN renders as `NaNdBFS` so an
/// upstream computation bug stays visible instead of being masked.
pub fn fmt_dbfs(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.1}dBFS")
    } else if v.is_nan() {
        "NaNdBFS".into()
    } else {
        "-infdBFS".into()
    }
}

fn rms_dbfs(sum_sq: u64, count: u64) -> f64 {
    if count == 0 {
        return f64::NEG_INFINITY;
    }
    let rms_lin = (sum_sq as f64 / count as f64).sqrt();
    if rms_lin <= 0.0 {
        return f64::NEG_INFINITY;
    }
    20.0 * (rms_lin / FULL_SCALE).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(actual: f64, expected: f64, tol: f64) {
        assert!(
            (actual - expected).abs() < tol,
            "got {actual}, expected {expected} +- {tol}"
        );
    }

    #[test]
    fn empty_summary_is_neg_inf() {
        let acc = LevelAccumulator::default();
        let (peak, rms, voiced_rms) = acc.summary();
        assert_eq!(peak, f64::NEG_INFINITY);
        assert_eq!(rms, f64::NEG_INFINITY);
        assert_eq!(voiced_rms, f64::NEG_INFINITY);
    }

    #[test]
    fn empty_frame_is_no_op() {
        let mut acc = LevelAccumulator::default();
        acc.add_frame(&[]);
        let (peak, rms, voiced_rms) = acc.summary();
        assert_eq!(peak, f64::NEG_INFINITY);
        assert_eq!(rms, f64::NEG_INFINITY);
        assert_eq!(voiced_rms, f64::NEG_INFINITY);
    }

    #[test]
    fn all_zero_frame_keeps_summary_at_neg_inf() {
        let mut acc = LevelAccumulator::default();
        acc.add_frame(&[0i16; 160]);
        let (peak, rms, voiced_rms) = acc.summary();
        assert_eq!(peak, f64::NEG_INFINITY);
        assert_eq!(rms, f64::NEG_INFINITY);
        assert_eq!(voiced_rms, f64::NEG_INFINITY);
    }

    #[test]
    fn full_scale_frame_peaks_at_zero_dbfs() {
        // i16::MIN's abs is 32768, exactly FULL_SCALE -> peak = 0 dBFS.
        let mut acc = LevelAccumulator::default();
        acc.add_frame(&[i16::MIN; 4]);
        let (peak, _, _) = acc.summary();
        approx(peak, 0.0, 1e-9);
    }

    #[test]
    fn voiced_gate_excludes_quiet_frames() {
        // A frame ~-50 dBFS sits below the -40 dBFS voiced gate, so
        // it contributes to rms but not voiced_rms.
        let amplitude = (FULL_SCALE * 10f64.powf(-50.0 / 20.0)) as i16;
        let mut acc = LevelAccumulator::default();
        acc.add_frame(&[amplitude; 160]);
        let (_, rms, voiced_rms) = acc.summary();
        assert!(rms.is_finite(), "rms should not be -inf");
        assert_eq!(voiced_rms, f64::NEG_INFINITY);
    }

    #[test]
    fn voiced_gate_includes_loud_frames() {
        // -20 dBFS is well above the -40 dBFS gate.
        let amplitude = (FULL_SCALE * 10f64.powf(-20.0 / 20.0)) as i16;
        let mut acc = LevelAccumulator::default();
        acc.add_frame(&[amplitude; 160]);
        let (_, rms, voiced_rms) = acc.summary();
        approx(rms, -20.0, 0.05);
        approx(voiced_rms, -20.0, 0.05);
    }

    #[test]
    fn voiced_gate_just_above_threshold_included() {
        // Sample 332 -> RMS ~= -39.89 dBFS, fractionally above the
        // strict-> gate at -40 dBFS.
        let mut acc = LevelAccumulator::default();
        acc.add_frame(&[332i16; 160]);
        let (_, rms, voiced_rms) = acc.summary();
        assert!(rms.is_finite() && rms > -40.0);
        assert!(voiced_rms.is_finite() && voiced_rms > -40.0);
    }

    #[test]
    fn voiced_gate_just_below_threshold_excluded() {
        // Sample 324 -> RMS ~= -40.10 dBFS, fractionally below the gate.
        let mut acc = LevelAccumulator::default();
        acc.add_frame(&[324i16; 160]);
        let (_, rms, voiced_rms) = acc.summary();
        assert!(rms.is_finite() && rms < -40.0);
        assert_eq!(voiced_rms, f64::NEG_INFINITY);
    }

    #[test]
    fn saturating_accumulators_dont_panic_on_huge_input() {
        // Push 256 full-amplitude frames of length 65536 each = ~16 M
        // samples; exercise the saturating_add accumulators on a
        // realistically large stream.  Constant signal at i16::MAX
        // means peak == rms == voiced_rms, all just below 0 dBFS.
        let mut acc = LevelAccumulator::default();
        let frame = vec![i16::MAX; 65536];
        for _ in 0..256 {
            acc.add_frame(&frame);
        }
        let (peak, rms, voiced_rms) = acc.summary();
        approx(peak, -0.0003, 0.001);
        approx(rms, -0.0003, 0.001);
        approx(voiced_rms, -0.0003, 0.001);
    }

    #[test]
    fn fmt_dbfs_carries_unit() {
        assert_eq!(fmt_dbfs(-12.345), "-12.3dBFS");
        assert_eq!(fmt_dbfs(0.0), "0.0dBFS");
        assert_eq!(fmt_dbfs(f64::NEG_INFINITY), "-infdBFS");
        assert_eq!(fmt_dbfs(f64::NAN), "NaNdBFS");
    }
}
