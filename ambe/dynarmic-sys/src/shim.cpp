#include "md380_vocoder.h"

extern "C" {
    int  shim_md380_init()                              { return md380_init(); }
    void shim_md380_encode(uint8_t *a, int16_t *p)      { md380_encode(a, p); }
    void shim_md380_decode(uint8_t *a, int16_t *p)      { md380_decode(a, p); }
    void shim_md380_encode_fec(uint8_t *a, int16_t *p)  { md380_encode_fec(a, p); }
    void shim_md380_decode_fec(uint8_t *a, int16_t *p)  { md380_decode_fec(a, p); }
}
