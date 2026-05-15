/// 2^15 -- same convention as agc.rs.
const FULL_SCALE: f32 = 32768.0;

/// Per-frame brick-wall peak limiter.
///
/// Finds the frame peak; if it exceeds `ceiling`, scales all samples
/// down so the peak lands exactly at the ceiling.  Transparent when
/// the frame is within bounds.  Stateless: no attack/release, no
/// inter-frame memory.
pub struct Limiter {
    /// Linear amplitude ceiling (0..1].
    ceiling: f32,
}

impl Limiter {
    pub fn new(ceiling_dbfs: f32) -> Self {
        Self {
            ceiling: 10f32.powf(ceiling_dbfs / 20.0),
        }
    }

    pub fn process(&self, samples: &mut [i16]) {
        let peak = samples
            .iter()
            .map(|&s| (s as f32 / FULL_SCALE).abs())
            .fold(0.0f32, f32::max);
        if peak > self.ceiling {
            let scale = self.ceiling / peak;
            for s in samples.iter_mut() {
                *s = ((*s as f32) * scale) as i16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_ceiling_is_no_op() {
        let lim = Limiter::new(-3.0);
        let orig = [1000i16, -1000, 2000, -2000];
        let mut buf = orig;
        lim.process(&mut buf);
        assert_eq!(buf, orig);
    }

    #[test]
    fn silence_is_no_op() {
        let lim = Limiter::new(-1.0);
        let mut buf = [0i16; 160];
        lim.process(&mut buf);
        assert!(buf.iter().all(|&s| s == 0));
    }

    #[test]
    fn above_ceiling_peak_is_clamped() {
        let ceiling_dbfs = -6.0f32;
        let lim = Limiter::new(ceiling_dbfs);
        let ceiling_lin = 10f32.powf(ceiling_dbfs / 20.0);
        // -3 dBFS signal > -6 dBFS ceiling.
        let amp = (FULL_SCALE * 10f32.powf(-3.0 / 20.0)) as i16;
        let mut buf = [amp; 160];
        lim.process(&mut buf);
        let peak = buf
            .iter()
            .map(|&s| (s as f32 / FULL_SCALE).abs())
            .fold(0.0f32, f32::max);
        assert!(
            (peak - ceiling_lin).abs() < 1e-3,
            "peak {peak:.4} should match ceiling {ceiling_lin:.4}"
        );
    }
}
