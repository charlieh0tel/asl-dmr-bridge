//! Raw FFI bindings to mbelib (szechyjs/mbelib).
//!
//! Patent notice: AMBE is patented by DVSI. This code is provided for
//! educational and experimental purposes only. Check patent restrictions
//! before compiling or using.

use std::os::raw::c_char;
use std::os::raw::c_int;
use std::os::raw::c_short;

/// Vocoder state parameters.
#[repr(C)]
pub struct MbeParms {
    pub w0: f32,
    pub l: c_int,
    pub k: c_int,
    pub vl: [c_int; 57],
    pub ml: [f32; 57],
    pub log2ml: [f32; 57],
    pub phil: [f32; 57],
    pub psil: [f32; 57],
    pub gamma: f32,
    pub un: c_int,
    pub repeat: c_int,
}

unsafe extern "C" {
    /// Initialize vocoder state.
    pub fn mbe_initMbeParms(
        cur_mp: *mut MbeParms,
        prev_mp: *mut MbeParms,
        prev_mp_enhanced: *mut MbeParms,
    );

    /// Decode AMBE+2 2450 data to PCM (short output).
    ///
    /// ambe_d: 49 chars, each 0 or 1 representing one bit.
    /// aout_buf: 160 i16 samples output.
    pub fn mbe_processAmbe2450Data(
        aout_buf: *mut c_short,
        errs: *mut c_int,
        errs2: *mut c_int,
        err_str: *mut c_char,
        ambe_d: *mut c_char,
        cur_mp: *mut MbeParms,
        prev_mp: *mut MbeParms,
        prev_mp_enhanced: *mut MbeParms,
        uvquality: c_int,
    );
}
