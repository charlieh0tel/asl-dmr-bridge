/// Decibel value.  The newtype guards against accidentally treating
/// raw f32 values as dB and vice-versa.
#[expect(non_camel_case_types, reason = "dB is the standard unit symbol")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct dB(pub f32);

impl dB {
    /// `0.0` dB == unity gain.
    pub const UNITY: Self = Self(0.0);

    /// Convert to a linear amplitude multiplier (`10^(db/20)`).
    #[must_use]
    pub fn linear(self) -> f32 {
        10.0_f32.powf(self.0 / 20.0)
    }

    /// Round and clamp to the DV3000 chip's accepted range (signed
    /// 8-bit, integer dB, +-90 dB).
    pub fn to_chip_byte(self) -> i8 {
        self.0.round().clamp(-90.0, 90.0) as i8
    }

    /// Scale a slice of i16 PCM samples by this gain, in-place.
    /// Saturating; unity is a no-op.
    pub fn apply(self, samples: &mut [i16]) {
        let g = self.linear();
        if g == 1.0 {
            return;
        }
        for s in samples.iter_mut() {
            let v = (*s as f32) * g;
            *s = v.clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }
    }
}

impl From<f32> for dB {
    fn from(v: f32) -> Self {
        Self(v)
    }
}
