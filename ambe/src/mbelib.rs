//! mbelib software vocoder backend (decode only).
//!
//! Encode is not supported -- mbelib's encode quality is too poor for
//! on-air use.  Use ThumbDV or AMBEserver for encoding.
//!
//! Patent notice: AMBE is patented by DVSI. This backend is provided
//! for educational and experimental purposes only.

use std::mem::MaybeUninit;
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
        // mbe_initMbeParms fully initializes all three MbeParms structs
        // (w0, l, k, vl[57], ml[57], log2ml[57], phil[57], psil[57],
        // gamma, un, repeat -- all scalar POD) before we observe any
        // field.  MaybeUninit encodes that invariant explicitly; if
        // a future mbelib upgrade added a field that init does not
        // write, assume_init would surface it as UB to Miri and to
        // sanitizer-built runs.
        let mut cur_mp = MaybeUninit::<mbelib_sys::MbeParms>::uninit();
        let mut prev_mp = MaybeUninit::<mbelib_sys::MbeParms>::uninit();
        let mut prev_mp_enhanced = MaybeUninit::<mbelib_sys::MbeParms>::uninit();
        // SAFETY: mbe_initMbeParms writes every field of each struct.
        unsafe {
            mbelib_sys::mbe_initMbeParms(
                cur_mp.as_mut_ptr(),
                prev_mp.as_mut_ptr(),
                prev_mp_enhanced.as_mut_ptr(),
            );
            Self {
                cur_mp: cur_mp.assume_init(),
                prev_mp: prev_mp.assume_init(),
                prev_mp_enhanced: prev_mp_enhanced.assume_init(),
            }
        }
    }
}

impl Vocoder for Mbelib {
    fn encode(&mut self, _pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError> {
        Err(VocoderError::Unsupported("mbelib is decode-only"))
    }

    fn decode(&mut self, ambe: &AmbeFrame) -> Result<PcmFrame, VocoderError> {
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
        let result = m.decode(&silence);
        assert!(result.is_ok());
    }
}
