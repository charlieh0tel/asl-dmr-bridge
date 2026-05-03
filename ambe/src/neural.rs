//! Neural-vocoder backend.  Encode-only at v1: PCM through a tract-
//! loaded ONNX model produces 49 source bits in mbelib `ambe_d[]`
//! order; the harness scatters them, permutes to chip order, and
//! channel-encodes via `dmr_wire::voice_channel`.  Decode delegates
//! to the inner `Mbelib` instance (the `neural` Cargo feature
//! implies `mbelib`).
//!
//! This module currently implements load + metadata parsing;
//! inference (encode pipeline) is stubbed pending the first ONNX
//! export.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use tracing::info;
use tract_onnx::pb;
use tract_onnx::prelude::Framework;
use tract_onnx::prelude::InferenceModelExt;
use tract_onnx::prelude::TypedModel;
use tract_onnx::prelude::TypedRunnableModel;

use crate::AmbeFrame;
use crate::PcmFrame;
use crate::Vocoder;
use crate::VocoderError;
use crate::mbelib::Mbelib;

/// Field layout name carried in ONNX metadata (`nambe.layout`).
const LAYOUT_DMR_3600X2450: &str = "DMR_3600X2450";

/// `nambe.frontend` value.  Only `Graph` is implemented; `RustMel`
/// is parked (kept defined for re-enablement).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Frontend {
    Graph,
    #[expect(dead_code, reason = "parked; rejected at load time")]
    RustMel,
}

/// Bit ordering of the model's 49-bit output, before chip-order
/// permutation.  Only mbelib is supported at v1.
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
    expect(dead_code, reason = "consumed by encode() scatter when it lands")
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
pub(crate) struct NeuralMeta {
    pub(crate) layout: String,
    pub(crate) pcm_input_samples: usize,
    pub(crate) context_frames: usize,
    pub(crate) context_lookahead: usize,
    pub(crate) frontend: Frontend,
    pub(crate) bit_order: BitOrder,
}

pub(crate) struct NeuralVocoder {
    #[expect(dead_code, reason = "consumed by encode() when inference lands")]
    plan: TypedRunnableModel<TypedModel>,
    #[expect(dead_code, reason = "consumed by encode() when inference lands")]
    meta: NeuralMeta,
    decoder: Mbelib,
}

impl NeuralVocoder {
    pub(crate) fn open(model_path: &Path) -> Result<Self, VocoderError> {
        let onnx = tract_onnx::onnx();

        // Load proto first so we can read metadata_props.  parse()
        // converts to InferenceModel.
        let proto = onnx
            .proto_model_for_path(model_path)
            .map_err(|e| init_err(format!("load {}: {e}", model_path.display())))?;
        let meta = parse_metadata(&proto)?;

        let model = onnx
            .parse(&proto, None)
            .map_err(|e| init_err(format!("parse {}: {e}", model_path.display())))?
            .model;

        let plan = model
            .into_typed()
            .map_err(|e| init_err(format!("into_typed: {e}")))?
            .into_optimized()
            .map_err(|e| init_err(format!("optimize: {e}")))?
            .into_runnable()
            .map_err(|e| init_err(format!("into_runnable: {e}")))?;

        info!(
            path = %model_path.display(),
            layout = %meta.layout,
            frontend = ?meta.frontend,
            bit_order = ?meta.bit_order,
            pcm_input_samples = meta.pcm_input_samples,
            context_frames = meta.context_frames,
            context_lookahead = meta.context_lookahead,
            "neural model loaded",
        );

        Ok(Self {
            plan,
            meta,
            decoder: Mbelib::new(),
        })
    }
}

impl Vocoder for NeuralVocoder {
    fn encode(&mut self, _pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError> {
        Err(VocoderError::Encode(
            "neural encode not yet implemented".into(),
        ))
    }

    fn decode(&mut self, ambe: Option<&AmbeFrame>) -> Result<PcmFrame, VocoderError> {
        self.decoder.decode(ambe)
    }

    fn reset(&mut self) {
        self.decoder.reset();
        // [TODO] @charlieh0tel: clear past/future PCM ring buffers
        // when the streaming harness lands.
    }
}

fn init_err(msg: String) -> VocoderError {
    VocoderError::Init(msg)
}

fn parse_metadata(proto: &pb::ModelProto) -> Result<NeuralMeta, VocoderError> {
    let props: HashMap<&str, &str> = proto
        .metadata_props
        .iter()
        .map(|kv| (kv.key.as_str(), kv.value.as_str()))
        .collect();

    let layout = require(&props, "nambe.layout")?.to_string();
    if layout != LAYOUT_DMR_3600X2450 {
        return Err(init_err(format!(
            "nambe.layout={layout:?}, expected {LAYOUT_DMR_3600X2450:?}"
        )));
    }

    let pcm_input_samples = parse_kv(&props, "nambe.pcm_input_samples")?;
    let context_frames = parse_kv(&props, "nambe.context_frames")?;
    let context_lookahead = parse_kv(&props, "nambe.context_lookahead")?;

    let frontend = match require(&props, "nambe.frontend")? {
        "graph" => Frontend::Graph,
        "rust_mel" => {
            return Err(init_err(
                "nambe.frontend=\"rust_mel\": parked, not implemented".into(),
            ));
        }
        other => {
            return Err(init_err(format!(
                "nambe.frontend={other:?}; expected 'graph'"
            )));
        }
    };

    let bit_order = match require(&props, "nambe.field_bit_order")? {
        "mbelib" => BitOrder::Mbelib,
        other => {
            return Err(init_err(format!(
                "nambe.field_bit_order={other:?}; only 'mbelib' supported"
            )));
        }
    };

    let vq_sizes_str = require(&props, "nambe.field_vq_sizes")?;
    let vq_sizes: Vec<u16> = vq_sizes_str
        .split(',')
        .map(|s| {
            s.trim()
                .parse::<u16>()
                .map_err(|e| init_err(format!("nambe.field_vq_sizes: {e}")))
        })
        .collect::<Result<_, _>>()?;
    if vq_sizes.len() != FIELDS_DMR_3600X2450.len() {
        return Err(init_err(format!(
            "nambe.field_vq_sizes: expected {} entries, got {}",
            FIELDS_DMR_3600X2450.len(),
            vq_sizes.len(),
        )));
    }
    for (i, (declared, expected)) in vq_sizes
        .iter()
        .zip(FIELDS_DMR_3600X2450.iter().map(|f| f.vq_size))
        .enumerate()
    {
        if *declared != expected {
            return Err(init_err(format!(
                "nambe.field_vq_sizes[{i}]={declared}, expected {expected} \
                 (from FIELDS_DMR_3600X2450)"
            )));
        }
    }

    Ok(NeuralMeta {
        layout,
        pcm_input_samples,
        context_frames,
        context_lookahead,
        frontend,
        bit_order,
    })
}

fn require<'a>(props: &HashMap<&str, &'a str>, key: &str) -> Result<&'a str, VocoderError> {
    props
        .get(key)
        .copied()
        .ok_or_else(|| init_err(format!("ONNX metadata missing required key {key:?}")))
}

fn parse_kv<T: FromStr>(props: &HashMap<&str, &str>, key: &str) -> Result<T, VocoderError>
where
    T::Err: std::fmt::Display,
{
    require(props, key)?
        .parse()
        .map_err(|e| init_err(format!("{key}: {e}")))
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
