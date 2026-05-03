//! mbelib software vocoder backend (decode only).
//!
//! Encode is not supported -- mbelib's encode quality is too poor for
//! on-air use.  Use ThumbDV or AMBEserver for encoding.
//!
//! Patent notice: AMBE is patented by DVSI. This backend is provided
//! for educational and experimental purposes only.

use std::os::raw::c_char;

#[cfg(test)]
use crate::AMBE_FRAME_SIZE;
use crate::AmbeFrame;
use crate::PcmFrame;
use crate::Vocoder;
use crate::VocoderError;
use crate::codeword::extract_source_bits;

const ERR_STR_LEN: usize = 256;
const UV_QUALITY: i32 = 3;

/// mbelib software vocoder (decode only).
pub(crate) struct Mbelib {
    cur_mp: mbelib_sys::MbeParms,
    prev_mp: mbelib_sys::MbeParms,
    prev_mp_enhanced: mbelib_sys::MbeParms,
}

impl Default for Mbelib {
    fn default() -> Self {
        Self::new()
    }
}

impl Mbelib {
    pub(crate) fn new() -> Self {
        // Zero the structs first, then have mbe_initMbeParms set the
        // fields it actually writes.  mbe_initMbeParms does *not*
        // touch the `un` field (it is dead code in mbelib's decoder,
        // never read), so leaving the struct uninitialized would give
        // `un` an indeterminate value that varies across instances.
        // Zeroing makes a reset return the struct to a state that's
        // byte-identical to a freshly-constructed one.
        // SAFETY: MbeParms is plain POD (only floats and ints); the
        // all-zeros bit pattern is a valid value of the type.
        let (cur_mp, prev_mp, prev_mp_enhanced) = unsafe {
            let mut cur = std::mem::zeroed::<mbelib_sys::MbeParms>();
            let mut prev = std::mem::zeroed::<mbelib_sys::MbeParms>();
            let mut prev_e = std::mem::zeroed::<mbelib_sys::MbeParms>();
            mbelib_sys::mbe_initMbeParms(&mut cur, &mut prev, &mut prev_e);
            (cur, prev, prev_e)
        };
        Self {
            cur_mp,
            prev_mp,
            prev_mp_enhanced,
        }
    }
}

impl Vocoder for Mbelib {
    fn encode(&mut self, _pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError> {
        Err(VocoderError::Unsupported("mbelib is decode-only"))
    }

    fn decode(&mut self, ambe: Option<&AmbeFrame>) -> Result<PcmFrame, VocoderError> {
        // None is a known erasure: mirror mbelib's internal `bad==2`
        // path (reset MbeParms + emit silence).
        let Some(ambe) = ambe else {
            self.reset();
            return Ok([0i16; crate::PCM_SAMPLES]);
        };

        let bits = extract_source_bits(ambe);
        let mut ambe_d: [c_char; 49] = bits.map(|b| b as c_char);
        let mut aout_buf = [0i16; crate::PCM_SAMPLES];
        let mut errs = 0i32;
        let mut errs2 = 0i32;
        let mut err_str = [0 as c_char; ERR_STR_LEN];

        unsafe {
            mbelib_sys::mbe_processAmbe2450Data(
                aout_buf.as_mut_ptr(),
                &mut errs,
                &mut errs2,
                err_str.as_mut_ptr(),
                ambe_d.as_mut_ptr(),
                &mut self.cur_mp,
                &mut self.prev_mp,
                &mut self.prev_mp_enhanced,
                UV_QUALITY,
            );
        }

        Ok(aout_buf)
    }

    /// Re-initialize the three MbeParms structs so the next decode
    /// starts from a clean predictor / smoother history -- otherwise
    /// the first frame of a new stream is decoded against the
    /// previous stream's parameters.  Note: this does not reseed
    /// libc rand(), which mbelib's synthesizer uses for unvoiced-
    /// band phase randomization, so PCM output is not guaranteed
    /// bit-equal across reset boundaries.  Decoder *state* is.
    fn reset(&mut self) {
        // Match `Self::new()`: zero then init, so post-reset state
        // is byte-identical to a freshly-constructed instance.
        // SAFETY: MbeParms is POD; all-zeros is a valid value.
        unsafe {
            std::ptr::write_bytes(&mut self.cur_mp, 0, 1);
            std::ptr::write_bytes(&mut self.prev_mp, 0, 1);
            std::ptr::write_bytes(&mut self.prev_mp_enhanced, 0, 1);
            mbelib_sys::mbe_initMbeParms(
                &mut self.cur_mp,
                &mut self.prev_mp,
                &mut self.prev_mp_enhanced,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_returns_error() {
        let mut m = Mbelib::new();
        assert!(m.encode(&[0; 160]).is_err());
    }

    #[test]
    fn decode_silence() {
        let mut m = Mbelib::new();
        let silence = [0u8; AMBE_FRAME_SIZE];
        let result = m.decode(Some(&silence));
        assert!(result.is_ok());
    }

    #[test]
    fn reset_returns_to_initial_state() {
        // Real chip-coded frame from utt000.coded72.
        let frame: AmbeFrame = [0x95, 0x4b, 0xe6, 0x50, 0x03, 0x10, 0xb0, 0x07, 0x77];

        let fresh = Mbelib::new();
        let mut dirty = Mbelib::new();
        let _ = dirty.decode(Some(&frame)).unwrap();

        // Sanity: decode actually mutates state -- otherwise reset()
        // would be vacuous and we'd want to know.
        assert_ne!(
            parms_bytes(&fresh.cur_mp),
            parms_bytes(&dirty.cur_mp),
            "decode left cur_mp untouched; predictor state isn't carrying"
        );

        dirty.reset();

        assert_eq!(parms_bytes(&fresh.cur_mp), parms_bytes(&dirty.cur_mp));
        assert_eq!(parms_bytes(&fresh.prev_mp), parms_bytes(&dirty.prev_mp));
        assert_eq!(
            parms_bytes(&fresh.prev_mp_enhanced),
            parms_bytes(&dirty.prev_mp_enhanced),
        );
    }

    fn parms_bytes(m: &mbelib_sys::MbeParms) -> &[u8] {
        // SAFETY: MbeParms is POD; reading its bytes is well-defined.
        unsafe {
            std::slice::from_raw_parts(
                (m as *const mbelib_sys::MbeParms).cast::<u8>(),
                std::mem::size_of::<mbelib_sys::MbeParms>(),
            )
        }
    }
}
