//! Neural-vocoder backend.  Encode-only at v1: PCM through a tract-
//! loaded ONNX model produces 49 source bits in mbelib `ambe_d[]`
//! order; the harness scatters them, permutes to chip order, and
//! channel-encodes via `dmr_wire::voice_channel`.  Decode delegates
//! to the inner `Mbelib` instance (the `neural` Cargo feature
//! implies `mbelib`).
//!
//! Loading + inference are stubbed pending the first real ONNX
//! export; see `docs/NEURAL-VOCODER.md` and the Phase 0 spec.

use std::path::Path;

use crate::AmbeFrame;
use crate::PcmFrame;
use crate::Vocoder;
use crate::VocoderError;
use crate::mbelib::Mbelib;

/// Field layout name carried in ONNX metadata (`nambe.layout`).
#[expect(dead_code, reason = "consumed by load() when it lands")]
pub(crate) const LAYOUT_DMR_3600X2450: &str = "DMR_3600X2450";

/// Where the mel front-end runs: inside the ONNX graph, or in Rust
/// before tract is invoked (decided per Phase 0 §0.0 smoke test;
/// the chosen path is recorded in ONNX metadata as
/// `nambe.frontend`).
#[expect(dead_code, reason = "variants consumed by load() when it lands")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Frontend {
    /// Mel spectrogram is folded into the ONNX graph.  Harness
    /// passes raw PCM tensors through.
    Graph,
    /// Mel spectrogram is computed in Rust (rustfft + constant
    /// filterbank).  Mel parameters come from ONNX metadata.
    RustMel,
}

/// Bit ordering of the model's 49-bit output, before chip-order
/// permutation.  Only mbelib is supported at v1.
#[expect(dead_code, reason = "variants consumed by load() when it lands")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BitOrder {
    /// mbelib `ambe_d[0..49]` order.
    Mbelib,
}

/// One AMBE+2 categorical field as the model emits it: a single VQ
/// index (one of `vq_size` codes) that scatters into specific
/// `ambe_d[]` positions, MSB-first.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "fields consumed by encode() scatter when it lands"
    )
)]
pub(crate) struct Field {
    pub(crate) name: &'static str,
    pub(crate) bits: u8,
    pub(crate) vq_size: u16,
    pub(crate) ambe_d: &'static [u8],
}

/// DMR / P25 half-rate field layout.  Source of truth:
/// `nambe/field_layout.py::FIELDS_DMR_3600X2450`.  Stable since
/// project inception.  Total bits across all nine fields = 49.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "consumed by encode() scatter when it lands")
)]
pub(crate) const FIELDS_DMR_3600X2450: &[Field] = &[
    Field {
        name: "b0",
        bits: 7,
        vq_size: 128,
        ambe_d: &[0, 1, 2, 3, 37, 38, 39],
    },
    Field {
        name: "b1",
        bits: 5,
        vq_size: 32,
        ambe_d: &[4, 5, 6, 7, 35],
    },
    Field {
        name: "b2",
        bits: 5,
        vq_size: 32,
        ambe_d: &[8, 9, 10, 11, 36],
    },
    Field {
        name: "b3",
        bits: 9,
        vq_size: 512,
        ambe_d: &[12, 13, 14, 15, 16, 17, 18, 19, 40],
    },
    Field {
        name: "b4",
        bits: 7,
        vq_size: 128,
        ambe_d: &[20, 21, 22, 23, 41, 42, 43],
    },
    Field {
        name: "b5",
        bits: 5,
        vq_size: 32,
        ambe_d: &[24, 25, 26, 27, 44],
    },
    Field {
        name: "b6",
        bits: 4,
        vq_size: 16,
        ambe_d: &[28, 29, 30, 45],
    },
    Field {
        name: "b7",
        bits: 4,
        vq_size: 16,
        ambe_d: &[31, 32, 33, 46],
    },
    Field {
        name: "b8",
        bits: 3,
        vq_size: 8,
        ambe_d: &[34, 47, 48],
    },
];

/// Metadata read from the ONNX file at load time -- never hardcoded
/// in Rust, so retraining / context-window changes / mel-parameter
/// changes are swap-the-onnx events.
#[expect(
    dead_code,
    reason = "populated when load() lands; consumed by encode()"
)]
pub(crate) struct NeuralMeta {
    pub(crate) layout: String,
    pub(crate) pcm_input_samples: usize,
    pub(crate) context_frames: usize,
    pub(crate) context_lookahead: usize,
    pub(crate) frontend: Frontend,
    pub(crate) field_vq_sizes: [u16; 9],
    pub(crate) bit_order: BitOrder,
}

#[expect(dead_code, reason = "populated when load() lands")]
pub(crate) struct NeuralVocoder {
    meta: NeuralMeta,
    decoder: Mbelib,
}

impl NeuralVocoder {
    pub(crate) fn open(_model_path: &Path) -> Result<Self, VocoderError> {
        Err(VocoderError::Init(
            "neural backend not yet implemented; awaiting first ONNX export".into(),
        ))
    }
}

impl Vocoder for NeuralVocoder {
    fn encode(&mut self, _pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError> {
        Err(VocoderError::Encode(
            "neural encode not yet implemented".into(),
        ))
    }

    fn decode(&mut self, ambe: &AmbeFrame) -> Result<PcmFrame, VocoderError> {
        self.decoder.decode(ambe)
    }

    fn reset(&mut self) {
        self.decoder.reset();
        // [TODO] @charlieh0tel: clear past/future PCM ring buffers
        // when the streaming harness lands.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_layout_totals_49_bits() {
        let total: u8 = FIELDS_DMR_3600X2450.iter().map(|f| f.bits).sum();
        assert_eq!(total, 49);
    }

    #[test]
    fn field_positions_cover_0_to_48_exactly_once() {
        let mut seen = [false; 49];
        for field in FIELDS_DMR_3600X2450 {
            assert_eq!(field.ambe_d.len(), field.bits as usize);
            assert_eq!(1u16 << field.bits, field.vq_size);
            for &pos in field.ambe_d {
                assert!(pos < 49, "{}: position {pos} out of range", field.name);
                assert!(
                    !seen[pos as usize],
                    "{}: position {pos} doubly assigned",
                    field.name
                );
                seen[pos as usize] = true;
            }
        }
        assert!(seen.iter().all(|&b| b));
    }
}
