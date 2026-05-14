//! Neural-vocoder backend.  Encode runs PCM through a tract-loaded
//! ONNX model whose 9 categorical heads (`b0_logits`..`b8_logits`)
//! argmax to VQ indices; the harness scatters them into mbelib
//! `ambe_d[]` order, permutes to chip order, and channel-encodes
//! via `crate::voice_channel`.  Decode delegates to the inner
//! dynarmic `DynarmicVocoder` (the `neural` Cargo feature implies
//! `dynarmic`).

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

use crate::Vocoder;
use crate::VocoderError;
use crate::dynarmic::DynarmicVocoder;
use dmr_types::AmbeFrame;
use dmr_types::PCM_SAMPLES;
use dmr_types::PcmFrame;

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
    decoder: Box<dyn crate::Vocoder>,
    /// Pre-encode gain applied to PCM before it lands in `samples`.
    /// Output gain is the inner decoder's responsibility.
    in_db: dsp::dB,
}

impl NeuralVocoder {
    pub(crate) fn open(model_path: &Path) -> Result<Self, VocoderError> {
        Self::open_with_decoder(model_path, Box::new(DynarmicVocoder::new()))
    }

    pub(crate) fn open_with_decoder(
        model_path: &Path,
        decoder: Box<dyn crate::Vocoder>,
    ) -> Result<Self, VocoderError> {
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
            decoder,
            in_db: dsp::dB::UNITY,
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
        let mut scaled = *pcm;
        self.in_db.apply(&mut scaled);
        self.samples.extend(scaled.iter().copied());
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

    fn set_gain(&mut self, in_db: dsp::dB, out_db: dsp::dB) -> Result<(), VocoderError> {
        self.in_db = in_db;
        self.decoder.set_gain(dsp::dB::UNITY, out_db)
    }
}

/// Convert one channel-coded AMBE frame to the 9 VQ field indices the
/// decoder model expects.  Inverts `encode_real_frame`'s scatter step.
pub(crate) fn frame_to_vq(frame: &AmbeFrame) -> [i64; 9] {
    let mbelib = crate::voice_channel::to_source_bits(frame);
    let ambe_d = crate::voice_channel::unpack_msb_first(&mbelib);
    let mut vq = [0i64; 9];
    for (j, field) in FIELDS_DMR_3600X2450.iter().enumerate() {
        let mut v: i64 = 0;
        for (i, &pos) in field.ambe_d.iter().enumerate() {
            v |= i64::from(ambe_d[pos as usize]) << (field.bits - 1 - i as u8);
        }
        vq[j] = v;
    }
    vq
}

/// µ-law decode: 8-bit code 0..=255 → i16 PCM.
/// Code 128 = silence (~0), 255 = max positive, 0 = max negative.
fn ulaw_decode(code: i64) -> i16 {
    const MU: f32 = 255.0;
    let y = (code as f32) * 2.0 / 255.0 - 1.0;
    let x = y.signum() * ((1.0 + MU).powf(y.abs()) - 1.0) / MU;
    (x * 32768.0).clamp(-32768.0, 32767.0) as i16
}

fn parse_decoder_meta(proto: &pb::ModelProto) -> Result<i64, VocoderError> {
    let props: HashMap<&str, &str> = proto
        .metadata_props
        .iter()
        .map(|kv| (kv.key.as_str(), kv.value.as_str()))
        .collect();
    if let (Some(&ver), Some(&opset)) = (props.get("nambe.model_version"), props.get("nambe.opset"))
    {
        info!(
            model_version = ver,
            opset, "neural decoder model provenance"
        );
    }
    parse_kv(&props, "nambe.mu_silence")
}

/// Neural (ONNX) decoder using split frame+step models.  Implements
/// `Vocoder::decode`; `encode` returns `Unsupported`.
///
/// Uses a 5-frame symmetric context window with 2-frame lookahead: the
/// first 2 `decode` calls return silence while the lookahead buffer fills.
/// The up-to-2 buffered frames remaining at `reset` are discarded.
pub(crate) struct NeuralDecoderVocoder {
    /// Frame conditioning model.  Input: bits_window [1,5,9] int64.
    /// Output: cond [1,128] float32.  Called once per 20 ms frame.
    frame_plan: TypedRunnableModel<TypedModel>,
    /// Per-sample autoregressive step model.  Called 160x per frame.
    /// Inputs: prev_mu [1] int64, cond [1,128] float32, h_in [1,1,256] float32.
    /// Outputs: next_mu [1] int64, h_out [1,1,256] float32.
    step_plan: TypedRunnableModel<TypedModel>,
    /// Incoming frames waiting for 2-frame lookahead (fires when len >= 3).
    pending: VecDeque<[i64; 9]>,
    /// Last 2 decoded frames for left-edge context, oldest first.
    history: VecDeque<[i64; 9]>,
    prev_mu: i64,
    /// GRU hidden state [1, 1, 256].
    h: tract_ndarray::Array3<f32>,
    mu_silence: i64,
    out_db: dsp::dB,
}

impl NeuralDecoderVocoder {
    pub(crate) fn open(
        frame_model_path: &Path,
        step_model_path: &Path,
    ) -> Result<Self, VocoderError> {
        let onnx = tract_onnx::onnx();

        let frame_proto = onnx
            .proto_model_for_path(frame_model_path)
            .map_err(|e| init_err(format!("load {}: {e}", frame_model_path.display())))?;
        let mu_silence = parse_decoder_meta(&frame_proto)?;
        let frame_plan = onnx
            .parse(&frame_proto, None)
            .map_err(|e| init_err(format!("parse {}: {e}", frame_model_path.display())))?
            .model
            .into_typed()
            .map_err(|e| init_err(format!("into_typed {}: {e}", frame_model_path.display())))?
            .into_optimized()
            .map_err(|e| init_err(format!("optimize {}: {e}", frame_model_path.display())))?
            .into_runnable()
            .map_err(|e| init_err(format!("runnable {}: {e}", frame_model_path.display())))?;
        info!(
            path = %frame_model_path.display(),
            mu_silence,
            "neural decoder frame model loaded"
        );

        let step_proto = onnx
            .proto_model_for_path(step_model_path)
            .map_err(|e| init_err(format!("load {}: {e}", step_model_path.display())))?;
        let step_plan = onnx
            .parse(&step_proto, None)
            .map_err(|e| init_err(format!("parse {}: {e}", step_model_path.display())))?
            .model
            .into_typed()
            .map_err(|e| init_err(format!("into_typed {}: {e}", step_model_path.display())))?
            .into_optimized()
            .map_err(|e| init_err(format!("optimize {}: {e}", step_model_path.display())))?
            .into_runnable()
            .map_err(|e| init_err(format!("runnable {}: {e}", step_model_path.display())))?;
        info!(
            path = %step_model_path.display(),
            "neural decoder step model loaded"
        );

        Ok(Self {
            frame_plan,
            step_plan,
            pending: VecDeque::new(),
            history: VecDeque::new(),
            prev_mu: mu_silence,
            h: tract_ndarray::Array3::zeros((1, 1, 256)),
            mu_silence,
            out_db: dsp::dB::UNITY,
        })
    }

    fn run_frame(&mut self, window: &[[i64; 9]; 5]) -> Result<PcmFrame, VocoderError> {
        // Run frame model once to get conditioning vector.
        let bits_data: Vec<i64> = window.iter().flat_map(|r| r.iter().copied()).collect();
        let bits_tensor = tract_ndarray::Array3::from_shape_vec((1usize, 5, 9), bits_data)
            .map_err(|e| VocoderError::Decode(format!("bits_window shape: {e}")))?
            .into_tensor();
        let frame_out = self
            .frame_plan
            .run(tvec![bits_tensor.into()])
            .map_err(|e| VocoderError::Decode(format!("frame inference: {e}")))?;
        let cond_val = frame_out.into_iter().next().unwrap();

        // Run step model 160x to produce one PCM frame.
        let mut prev_mu = self.prev_mu;
        let mut out = [0i16; PCM_SAMPLES];
        for sample in out.iter_mut() {
            let mu_tensor = tract_ndarray::arr1(&[prev_mu]).into_tensor();
            let h_tensor = self.h.clone().into_tensor();
            let step_out = self
                .step_plan
                .run(tvec![mu_tensor.into(), cond_val.clone(), h_tensor.into()])
                .map_err(|e| VocoderError::Decode(format!("step inference: {e}")))?;

            prev_mu = step_out[0]
                .as_slice::<i64>()
                .map_err(|e| VocoderError::Decode(format!("next_mu: {e}")))?[0];
            let h_slice = step_out[1]
                .as_slice::<f32>()
                .map_err(|e| VocoderError::Decode(format!("h_out: {e}")))?;
            self.h
                .as_slice_mut()
                .expect("h contiguous")
                .copy_from_slice(h_slice);
            *sample = ulaw_decode(prev_mu);
        }
        self.prev_mu = prev_mu;
        self.out_db.apply(&mut out);
        Ok(out)
    }
}

impl Vocoder for NeuralDecoderVocoder {
    fn encode(&mut self, _pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError> {
        Err(VocoderError::Unsupported(
            "NeuralDecoderVocoder does not support encode",
        ))
    }

    fn decode(&mut self, ambe: Option<&AmbeFrame>) -> Result<PcmFrame, VocoderError> {
        let vq = match ambe {
            Some(frame) => frame_to_vq(frame),
            None => frame_to_vq(&crate::SILENCE_FRAME),
        };
        self.pending.push_back(vq);
        if self.pending.len() < 3 {
            return Ok([0i16; PCM_SAMPLES]);
        }
        let target = self.pending[0];
        // Edge-replicate left context for the first 2 decoded frames.
        let past_m2 = self.history.front().copied().unwrap_or(target);
        let past_m1 = self.history.back().copied().unwrap_or(target);
        let future_1 = self.pending[1];
        let future_2 = self.pending[2];
        let window = [past_m2, past_m1, target, future_1, future_2];
        let pcm = self.run_frame(&window)?;
        self.history
            .push_back(self.pending.pop_front().expect("pending non-empty"));
        if self.history.len() > 2 {
            self.history.pop_front();
        }
        Ok(pcm)
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.history.clear();
        self.prev_mu = self.mu_silence;
        self.h.fill(0.0);
    }

    fn set_gain(&mut self, _in_db: dsp::dB, out_db: dsp::dB) -> Result<(), VocoderError> {
        self.out_db = out_db;
        Ok(())
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
    if frame_hop_samples != PCM_SAMPLES {
        return Err(init_err(format!(
            "nambe.frame_hop_samples={frame_hop_samples}, expected {} \
             (matches PCM_SAMPLES; the bridge feeds the model one
             20 ms PCM frame per encode call)",
            PCM_SAMPLES,
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
        // The decoder must honor b0=124 and emit ~zero PCM from a
        // freshly-reset state.  If this fails, the warm-up bytes
        // will produce noise on the wire.
        let mut decoder = DynarmicVocoder::new();
        decoder.reset();
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

    #[test]
    fn ulaw_decode_silence_near_zero() {
        // Code 128 is the silence code; should decode to near-zero PCM.
        let pcm = ulaw_decode(128);
        assert!(pcm.abs() < 10, "code 128 decoded to {pcm}, expected near 0");
    }

    #[test]
    fn frame_to_vq_silence_frame() {
        // b0 = 124 (0b1111100) occupies bits [0..6] of the 49-bit AMBE-D
        // word in MSB-first order.  b1..b8 should be 0.
        let vq = frame_to_vq(&crate::SILENCE_FRAME);
        assert_eq!(vq[0], 124, "b0");
        for (i, &v) in vq[1..].iter().enumerate() {
            assert_eq!(v, 0, "b{}", i + 1);
        }
    }

    /// Streaming buffer must yield the same slice the model would see
    /// if we reset and pushed the corresponding PCM window directly.
    /// Catches trim/warm-up off-by-ones independently of model
    /// correctness (which `neural_parity` covers).
    #[test]
    fn streaming_matches_offline_for_aug50() {
        // Covers the warm-up boundary plus a steady-state margin;
        // divergence shows on the first few frames anyway.
        const MAX_REFERENCE_FRAMES: usize = 50;

        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = manifest.join("tests").join("fixtures").join("aug50");
        let model_path = fixture.join("model.onnx");
        let wav_path = fixture.join("parity_input.wav");
        if !model_path.exists() || !wav_path.exists() {
            eprintln!(
                "streaming_offline_parity: fixtures missing ({} or {}); skipping",
                model_path.display(),
                wav_path.display(),
            );
            return;
        }

        let bytes = std::fs::read(&wav_path).unwrap();
        assert!(bytes.len() > 44, "WAV too short");
        let pcm: Vec<i16> = bytes[44..]
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(pcm.len() % PCM_SAMPLES, 0);
        let total_frames = pcm.len() / PCM_SAMPLES;

        let mut streaming = NeuralVocoder::open(&model_path).expect("open neural (streaming)");
        let mut streaming_vqs: Vec<(usize, [u16; 9])> = Vec::new();
        for k in 0..total_frames {
            let mut frame = [0i16; PCM_SAMPLES];
            frame.copy_from_slice(&pcm[k * PCM_SAMPLES..(k + 1) * PCM_SAMPLES]);
            if let Some(vq) = streaming.encode_vq(&frame).expect("encode_vq") {
                streaming_vqs.push((k, vq));
                if streaming_vqs.len() == MAX_REFERENCE_FRAMES {
                    break;
                }
            }
        }
        assert!(
            !streaming_vqs.is_empty(),
            "no post-warm-up frames produced; fixture too short?"
        );

        let mut reference = NeuralVocoder::open(&model_path).expect("open neural (reference)");
        let buffer_cap = reference.buffer_cap;
        let pcm_input_samples = reference.meta.pcm_input_samples;
        for (k, expected_vq) in &streaming_vqs {
            let total_seen = (k + 1) * PCM_SAMPLES;
            let slice_start = total_seen - buffer_cap;
            let slice = &pcm[slice_start..slice_start + pcm_input_samples];
            reference.samples.clear();
            reference.samples.extend(slice.iter().copied());
            let actual_vq = reference.run_inference().expect("run_inference");
            assert_eq!(
                &actual_vq, expected_vq,
                "VQ mismatch at frame {k}: streaming={expected_vq:?} offline={actual_vq:?}",
            );
        }
    }
}
