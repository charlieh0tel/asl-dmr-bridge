//! FFI bindings to the MD380 firmware vocoder, JIT-emulated by
//! dynarmic.  The `shim_*` C entry points are defined in
//! `src/shim.cpp` and exported with `extern "C"` linkage; the
//! cmake-driven build links the underlying `md380_vocoder` static
//! library plus its dynarmic + zydis + fmt + mcl transitive deps.

unsafe extern "C" {
    #[link_name = "shim_md380_init"]
    pub fn md380_init() -> std::ffi::c_int;
    #[link_name = "shim_md380_encode"]
    pub fn md380_encode(ambe: *mut u8, pcm: *const i16);
    #[link_name = "shim_md380_decode"]
    pub fn md380_decode(ambe: *const u8, pcm: *mut i16);
    #[link_name = "shim_md380_encode_fec"]
    pub fn md380_encode_fec(ambe: *mut u8, pcm: *const i16);
    #[link_name = "shim_md380_decode_fec"]
    pub fn md380_decode_fec(ambe: *const u8, pcm: *mut i16);
}
