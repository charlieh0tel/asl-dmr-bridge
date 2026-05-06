use std::sync::{Mutex, MutexGuard, OnceLock};

mod shim {
    extern "C" {
        #[link_name = "shim_md380_init"]
        pub(super) fn md380_init() -> std::ffi::c_int;
        #[link_name = "shim_md380_encode"]
        pub(super) fn md380_encode(ambe: *mut u8, pcm: *const i16);
        #[link_name = "shim_md380_decode"]
        pub(super) fn md380_decode(ambe: *const u8, pcm: *mut i16);
        #[link_name = "shim_md380_encode_fec"]
        pub(super) fn md380_encode_fec(ambe: *mut u8, pcm: *const i16);
        #[link_name = "shim_md380_decode_fec"]
        pub(super) fn md380_decode_fec(ambe: *const u8, pcm: *mut i16);
    }
}

/// PCM samples per AMBE+2 frame (8 kHz, 20 ms).
pub const PCM_FRAME_SAMPLES: usize = 160;

/// Raw AMBE+2 payload: 49 bits packed MSB-first into 7 bytes.
pub const AMBE_BYTES: usize = 7;

/// AMBE+2 with DMR FEC layer: 72 bits packed into 9 bytes.
pub const AMBE_FEC_BYTES: usize = 9;

// The Dynarmic JIT maintains a single global emulator instance that is not
// thread-safe.  All codec calls must hold this lock.
static CODEC: OnceLock<Mutex<()>> = OnceLock::new();

fn lock() -> MutexGuard<'static, ()> {
    let m = CODEC.get_or_init(|| {
        let rc = unsafe { shim::md380_init() };
        assert_eq!(rc, 0, "md380_init failed");
        Mutex::new(())
    });
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Encode 160 PCM samples (8 kHz, 16-bit signed) to 7 bytes of AMBE+2.
pub fn encode(pcm: &[i16; PCM_FRAME_SAMPLES]) -> [u8; AMBE_BYTES] {
    let _g = lock();
    let mut ambe = [0u8; AMBE_BYTES];
    unsafe { shim::md380_encode(ambe.as_mut_ptr(), pcm.as_ptr()) };
    ambe
}

/// Decode 7 bytes of AMBE+2 to 160 PCM samples.
pub fn decode(ambe: &[u8; AMBE_BYTES]) -> [i16; PCM_FRAME_SAMPLES] {
    let _g = lock();
    let mut pcm = [0i16; PCM_FRAME_SAMPLES];
    unsafe { shim::md380_decode(ambe.as_ptr(), pcm.as_mut_ptr()) };
    pcm
}

/// Encode 160 PCM samples to 9 bytes of AMBE+2 with DMR FEC.
pub fn encode_fec(pcm: &[i16; PCM_FRAME_SAMPLES]) -> [u8; AMBE_FEC_BYTES] {
    let _g = lock();
    let mut ambe = [0u8; AMBE_FEC_BYTES];
    unsafe { shim::md380_encode_fec(ambe.as_mut_ptr(), pcm.as_ptr()) };
    ambe
}

/// Decode 9 bytes of AMBE+2 with DMR FEC to 160 PCM samples.
pub fn decode_fec(ambe: &[u8; AMBE_FEC_BYTES]) -> [i16; PCM_FRAME_SAMPLES] {
    let _g = lock();
    let mut pcm = [0i16; PCM_FRAME_SAMPLES];
    unsafe { shim::md380_decode_fec(ambe.as_ptr(), pcm.as_mut_ptr()) };
    pcm
}

/// Reset the codec's internal predictor state.
///
/// Call this when a new client session begins so stale predictor state
/// from the previous session does not corrupt the new stream.
pub fn reset() {
    let _g = lock();
    let rc = unsafe { shim::md380_init() };
    assert_eq!(rc, 0, "md380_init failed on reset");
}
