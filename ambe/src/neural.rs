//! Neural-vocoder backend.  Encode runs PCM through a tract-loaded
//! ONNX model whose 9 categorical heads (`b0_logits`..`b8_logits`)
//! argmax to VQ indices; the harness scatters them into mbelib
//! `ambe_d[]` order, permutes to chip order, and channel-encodes
//! via `crate::voice_channel`.  Decode delegates to the inner
//! `Mbelib` instance (the `neural` Cargo feature implies `mbelib`).

use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::Path;
use std::str::FromStr;

use tracing::info;
use tract_onnx::pb;
use tract_onnx::prelude::Framework;
use tract_onnx::prelude::InferenceModelExt;
use tract_onnx::prelude::IntoTensor;
use tract_onnx::prelude::TypedModel;
use tract_onnx::prelude::TypedRunnableModel;
use tract_onnx::prelude::tract_ndarray;
use tract_onnx::prelude::tvec;

use crate::AmbeFrame;
use crate::PCM_SAMPLES;
use crate::PcmFrame;
use crate::Vocoder;
use crate::VocoderError;
use crate::mbelib::Mbelib;

/// Field layout name carried in ONNX metadata (`nambe.layout`).
const LAYOUT_DMR_3600X2450: &str = "DMR_3600X2450";

/// One AMBE+2 categorical field as the model emits it: a single VQ
/// index (one of `vq_size` codes) that scatters into specific
/// `ambe_d[]` positions, MSB-first.
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
    pub(crate) harness_lookback_samples: usize,
}

pub(crate) struct NeuralVocoder {
    plan: TypedRunnableModel<TypedModel>,
    meta: NeuralMeta,
    /// Buffer cap = warm-up threshold.  Once `samples` first
    /// fills to this length, every subsequent call produces real
    /// bits; the model's input slice is the oldest
    /// `pcm_input_samples` of `samples`.
    buffer_cap: usize,
    samples: VecDeque<i16>,
    decoder: Mbelib,
}

impl NeuralVocoder {
    pub(crate) fn open(model_path: &Path) -> Result<Self, VocoderError> {
        let onnx = tract_onnx::onnx();
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

        validate_output_names(&proto)?;
        let buffer_cap = derive_buffer_cap(&meta);

        info!(
            path = %model_path.display(),
            layout = %meta.layout,
            pcm_input_samples = meta.pcm_input_samples,
            context_frames = meta.context_frames,
            context_lookahead = meta.context_lookahead,
            harness_lookback_samples = meta.harness_lookback_samples,
            buffer_cap,
            "neural model loaded",
        );

        Ok(Self {
            plan,
            meta,
            buffer_cap,
            samples: VecDeque::with_capacity(buffer_cap),
            decoder: Mbelib::new(),
        })
    }

    /// Run inference on the current buffer slice and argmax each of
    /// the 9 logit heads.  Output order matches FIELDS_DMR_3600X2450
    /// (validated at load).
    fn run_inference(&mut self) -> Result<[u16; 9], VocoderError> {
        let mut input = Vec::with_capacity(self.meta.pcm_input_samples);
        input.extend(
            self.samples
                .iter()
                .take(self.meta.pcm_input_samples)
                .map(|&s| f32::from(s) / 32768.0),
        );
        if input.len() != self.meta.pcm_input_samples {
            return Err(VocoderError::Encode(format!(
                "buffer slice short: {} samples, expected {}",
                input.len(),
                self.meta.pcm_input_samples,
            )));
        }

        let tensor = tract_ndarray::Array2::from_shape_vec((1, self.meta.pcm_input_samples), input)
            .map_err(|e| VocoderError::Encode(format!("input shape: {e}")))?
            .into_tensor();
        let outputs = self
            .plan
            .run(tvec!(tensor.into()))
            .map_err(|e| VocoderError::Encode(format!("inference: {e}")))?;

        let mut vq = [0u16; 9];
        for (field_idx, field) in FIELDS_DMR_3600X2450.iter().enumerate() {
            let logits = outputs[field_idx]
                .as_slice::<f32>()
                .map_err(|e| VocoderError::Encode(format!("{}: {e}", field.name)))?;
            if logits.len() != usize::from(field.vq_size) {
                return Err(VocoderError::Encode(format!(
                    "{}: logits len {} != vq_size {}",
                    field.name,
                    logits.len(),
                    field.vq_size,
                )));
            }
            let mut best_idx: u16 = 0;
            let mut best_val = f32::NEG_INFINITY;
            for (idx, &v) in logits.iter().enumerate() {
                if v > best_val {
                    best_val = v;
                    best_idx = idx as u16;
                }
            }
            vq[field_idx] = best_idx;
        }
        Ok(vq)
    }

    fn encode_real_frame(&mut self) -> Result<AmbeFrame, VocoderError> {
        let vq = self.run_inference()?;
        let mut ambe_d = [0u8; 49];
        for (field_idx, field) in FIELDS_DMR_3600X2450.iter().enumerate() {
            let value = vq[field_idx];
            for (i, &pos) in field.ambe_d.iter().enumerate() {
                let bit_pos = field.bits - 1 - i as u8;
                ambe_d[pos as usize] = ((value >> bit_pos) & 1) as u8;
            }
        }
        Ok(crate::voice_channel::encode_from_ambe_d(&ambe_d))
    }

    /// Snapshot of the model-input slice (oldest `pcm_input_samples`
    /// of the streaming buffer).  Empty until warm-up.
    pub(crate) fn current_input_slice(&self) -> Vec<i16> {
        if self.samples.len() < self.buffer_cap {
            return Vec::new();
        }
        self.samples
            .iter()
            .take(self.meta.pcm_input_samples)
            .copied()
            .collect()
    }

    /// Streaming-encoder wrapper that returns the per-frame VQ
    /// indices instead of channel-coded bytes.  `Ok(None)` until the
    /// warm-up window is filled, then `Ok(Some(vq))` per frame.
    pub(crate) fn encode_vq(&mut self, pcm: &PcmFrame) -> Result<Option<[u16; 9]>, VocoderError> {
        self.samples.extend(pcm.iter().copied());
        while self.samples.len() > self.buffer_cap {
            self.samples.pop_front();
        }
        if self.samples.len() < self.buffer_cap {
            return Ok(None);
        }
        Ok(Some(self.run_inference()?))
    }
}

impl Vocoder for NeuralVocoder {
    fn encode(&mut self, pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError> {
        self.samples.extend(pcm.iter().copied());
        while self.samples.len() > self.buffer_cap {
            self.samples.pop_front();
        }
        if self.samples.len() < self.buffer_cap {
            return Ok(*crate::SILENCE_FRAME);
        }
        self.encode_real_frame()
    }

    fn decode(&mut self, ambe: Option<&AmbeFrame>) -> Result<PcmFrame, VocoderError> {
        self.decoder.decode(ambe)
    }

    fn reset(&mut self) {
        self.decoder.reset();
        self.samples.clear();
    }
}

/// Smallest sample-buffer size that holds the model's input slice
/// for the first real-output frame.  Once `samples` first fills
/// to this length, every subsequent encode call produces real bits.
fn derive_buffer_cap(meta: &NeuralMeta) -> usize {
    let slice_end_offset = meta
        .pcm_input_samples
        .saturating_sub(meta.context_lookahead * PCM_SAMPLES)
        .saturating_sub(meta.harness_lookback_samples);
    let k_warmup = slice_end_offset.div_ceil(PCM_SAMPLES);
    (k_warmup + meta.context_lookahead) * PCM_SAMPLES + meta.harness_lookback_samples
}

/// Confirm the ONNX graph's outputs are `b0_logits`..`b8_logits`
/// in that order so encode can index them ordinally.
fn validate_output_names(proto: &pb::ModelProto) -> Result<(), VocoderError> {
    let graph = proto
        .graph
        .as_ref()
        .ok_or_else(|| init_err("ONNX has no graph".into()))?;
    if graph.output.len() != FIELDS_DMR_3600X2450.len() {
        return Err(init_err(format!(
            "ONNX has {} outputs, expected {}",
            graph.output.len(),
            FIELDS_DMR_3600X2450.len(),
        )));
    }
    for (i, field) in FIELDS_DMR_3600X2450.iter().enumerate() {
        let expected = format!("{}_logits", field.name);
        let got = graph.output[i].name.as_str();
        if got != expected {
            return Err(init_err(format!(
                "ONNX graph.output[{i}].name={got:?}, expected {expected:?}"
            )));
        }
    }
    Ok(())
}

fn init_err(msg: String) -> VocoderError {
    VocoderError::Init(msg)
}

/// Expected sample rate in Hz: PCM frames are 160 samples / 20 ms.
const EXPECTED_SAMPLE_RATE: u32 = 8000;
/// Expected PCM normalization mode (see `nambe/training/dataset.py:94`).
const EXPECTED_PCM_NORMALIZATION: &str = "i16_div_32768";

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
    expect_eq(
        &props,
        "nambe.sample_rate",
        &EXPECTED_SAMPLE_RATE.to_string(),
    )?;
    expect_eq(
        &props,
        "nambe.pcm_normalization",
        EXPECTED_PCM_NORMALIZATION,
    )?;
    let frame_hop_samples: usize = parse_kv(&props, "nambe.frame_hop_samples")?;
    if frame_hop_samples != crate::PCM_SAMPLES {
        return Err(init_err(format!(
            "nambe.frame_hop_samples={frame_hop_samples}, expected {} \
             (matches PCM_SAMPLES; the bridge feeds the model one
             20 ms PCM frame per encode call)",
            crate::PCM_SAMPLES,
        )));
    }
    let opset = require(&props, "nambe.opset")?;

    let pcm_input_samples = parse_kv(&props, "nambe.pcm_input_samples")?;
    let context_frames = parse_kv(&props, "nambe.context_frames")?;
    let context_lookahead = parse_kv(&props, "nambe.context_lookahead")?;
    let harness_lookback_samples = parse_kv(&props, "nambe.harness_lookback_samples")?;

    expect_eq(&props, "nambe.frontend", "graph")?;
    expect_eq(&props, "nambe.field_bit_order", "mbelib")?;

    let expected_field_names: String = FIELDS_DMR_3600X2450
        .iter()
        .map(|f| f.name)
        .collect::<Vec<_>>()
        .join(",");
    expect_eq(&props, "nambe.field_names", &expected_field_names)?;

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

    // Provenance: read + log; not stored.  Opset is also logged
    // here -- not strictly validated since tract is the real
    // arbiter of which opset versions it can ingest.
    let model_version = require(&props, "nambe.model_version")?;
    info!(model_version, opset, "neural model provenance");

    Ok(NeuralMeta {
        layout,
        pcm_input_samples,
        context_frames,
        context_lookahead,
        harness_lookback_samples,
    })
}

fn require<'a>(props: &HashMap<&str, &'a str>, key: &str) -> Result<&'a str, VocoderError> {
    props
        .get(key)
        .copied()
        .ok_or_else(|| init_err(format!("ONNX metadata missing required key {key:?}")))
}

fn expect_eq(props: &HashMap<&str, &str>, key: &str, expected: &str) -> Result<(), VocoderError> {
    let got = require(props, key)?;
    if got != expected {
        return Err(init_err(format!("{key}={got:?}, expected {expected:?}")));
    }
    Ok(())
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
    fn silence_sentinel_decodes_to_silence() {
        // mbelib must honor b0=124 and emit ~zero PCM from a freshly-
        // reset decoder.  If this fails, mbelib has a quirk and the
        // warm-up bytes will produce noise on the wire.
        let mut decoder = Mbelib::new();
        let pcm = decoder.decode(Some(&crate::SILENCE_FRAME)).unwrap();
        let mean_sq: f64 =
            pcm.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / pcm.len() as f64;
        let rms_dbfs = if mean_sq > 0.0 {
            10.0 * mean_sq.log10() - 20.0 * 32768.0_f64.log10()
        } else {
            f64::NEG_INFINITY
        };
        assert!(
            rms_dbfs < -50.0,
            "silence sentinel decoded to {rms_dbfs:.1}dBFS, expected < -50.0dBFS"
        );
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
