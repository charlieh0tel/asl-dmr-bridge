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

/// Format a dBFS value for log output.  `-inf` (no samples, all
/// silent) renders as `-infdBFS` so the unit is always present.
pub fn fmt_dbfs(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.1}dBFS")
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
