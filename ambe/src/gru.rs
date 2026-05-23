//! Native Rust GRU decoder: weights, one-step math, and the
//! `NativeGruDecoder` vocoder that replaces the ONNX step model with
//! this kernel while keeping the tract frame-conditioning model.
//!
//! Weight layout (all f32 LE, row-major on disk).  Dimensions that
//! vary with the model variant are written as `H` (gru_hidden from
//! meta.json); fixed dimensions are literal:
//!   sample_embed  [256,  8]
//!   W_ir/iz/in    [H, 136]  GRU input weights; split at load time into
//!                            embed-half [H, 8] and cond-half [H, 128]
//!                            for per-frame cond precomputation.
//!   W_hr/hz/hn    [H,   H]  GRU hidden weights (r, z, n gates)
//!   b_ir/iz/in    [H]       GRU input biases
//!   b_hr/hz/hn    [H]       GRU hidden biases
//!   fc1_weight    [FC,  H]  dual-FC layer 1 weight (FC = dual_fc_hidden)
//!   fc1_bias      [FC]
//!   fc2_weight    [256, FC] dual-FC layer 2 weight (output always 256)
//!   fc2_bias      [256]

use std::collections::VecDeque;
use std::path::Path;
use std::time::Instant;

use faer::Accum;
use faer::Mat;
use faer::MatMut;
use faer::MatRef;
use faer::Par;
use tracing::info;
use tract_onnx::prelude::Framework;
use tract_onnx::prelude::InferenceModelExt;
use tract_onnx::prelude::IntoTensor;
use tract_onnx::prelude::TypedModel;
use tract_onnx::prelude::TypedRunnableModel;
use tract_onnx::prelude::tract_ndarray;
use tract_onnx::prelude::tvec;

use crate::Vocoder;
use crate::VocoderError;
use dmr_types::AmbeFrame;
use dmr_types::PCM_SAMPLES;
use dmr_types::PcmFrame;

// Fixed dimensions shared by all model variants.
const INPUT: usize = 136; // EMBED_DIM(8) + COND_DIM(128)
const MU_CHANNELS: usize = 256;
const EMBED_DIM: usize = 8;
const COND_DIM: usize = 128;
const MU_SILENCE: u8 = 128;
// AMBE+2 b0 values >= 120 are special frames (erasure 120-123, silence 124,
// tone 125-127).  All bypass the GRU and output PCM silence.
const B0_SPECIAL_MIN: i64 = 120;

/// All GRU weight matrices and bias vectors, loaded from a flat-binary
/// weight directory.  `hidden` is read from `meta.json` and may be
/// 256, 128, or 96 depending on the model variant.
///
/// Weight matrices are stored as column-major `faer::Mat<f32>` (transposed
/// from the row-major on-disk layout); SIMD dispatch is handled by faer.
pub(crate) struct GruWeights {
    /// GRU hidden dimension for this model variant.
    pub(crate) hidden: usize,
    /// Dual-FC hidden dimension (may differ from `hidden`).
    pub(crate) dual_fc_hidden: usize,
    /// [256, 8] µ-law embedding lookup table.
    sample_embed: Box<[[f32; EMBED_DIM]; MU_CHANNELS]>,
    /// [H, 8] embed-half of GRU input weights for r, z, n gates.
    w_ir_e: Mat<f32>,
    w_iz_e: Mat<f32>,
    w_in_e: Mat<f32>,
    /// [H, 128] cond-half of GRU input weights for r, z, n gates.
    w_ir_c: Mat<f32>,
    w_iz_c: Mat<f32>,
    w_in_c: Mat<f32>,
    /// [H, H] GRU hidden weights for r, z, n gates.
    w_hr: Mat<f32>,
    w_hz: Mat<f32>,
    w_hn: Mat<f32>,
    /// [H] GRU biases: input and hidden, for r, z, n.
    b_ir: Box<[f32]>,
    b_iz: Box<[f32]>,
    b_in: Box<[f32]>,
    b_hr: Box<[f32]>,
    b_hz: Box<[f32]>,
    b_hn: Box<[f32]>,
    /// [FC, H] dual-FC layer 1; [256, FC] dual-FC layer 2.
    fc1_weight: Mat<f32>,
    fc1_bias: Box<[f32]>,
    /// fc2 output is always MU_CHANNELS=256 regardless of hidden size.
    fc2_weight: Mat<f32>,
    fc2_bias: Box<[f32]>,
}

impl GruWeights {
    pub(crate) fn load(dir: &Path) -> Result<Self, VocoderError> {
        let (hidden, dual_fc_hidden) = read_meta(dir)?;
        let (w_ir_e, w_ir_c) = load_input_matrix_split(dir, "W_ir.bin", hidden)?;
        let (w_iz_e, w_iz_c) = load_input_matrix_split(dir, "W_iz.bin", hidden)?;
        let (w_in_e, w_in_c) = load_input_matrix_split(dir, "W_in.bin", hidden)?;
        Ok(Self {
            hidden,
            dual_fc_hidden,
            sample_embed: load_embed(dir, "sample_embed.bin")?,
            w_ir_e,
            w_ir_c,
            w_iz_e,
            w_iz_c,
            w_in_e,
            w_in_c,
            w_hr: load_matrix_faer(dir, "W_hr.bin", hidden, hidden)?,
            w_hz: load_matrix_faer(dir, "W_hz.bin", hidden, hidden)?,
            w_hn: load_matrix_faer(dir, "W_hn.bin", hidden, hidden)?,
            b_ir: load_bias(dir, "b_ir.bin", hidden)?,
            b_iz: load_bias(dir, "b_iz.bin", hidden)?,
            b_in: load_bias(dir, "b_in.bin", hidden)?,
            b_hr: load_bias(dir, "b_hr.bin", hidden)?,
            b_hz: load_bias(dir, "b_hz.bin", hidden)?,
            b_hn: load_bias(dir, "b_hn.bin", hidden)?,
            fc1_weight: load_matrix_faer(dir, "fc1_weight.bin", dual_fc_hidden, hidden)?,
            fc1_bias: load_bias(dir, "fc1_bias.bin", dual_fc_hidden)?,
            fc2_weight: load_matrix_faer(dir, "fc2_weight.bin", MU_CHANNELS, dual_fc_hidden)?,
            fc2_bias: load_bias(dir, "fc2_bias.bin", MU_CHANNELS)?,
        })
    }
}

/// Pre-allocated working buffers for `gru_step`.  Owned by
/// `NativeGruDecoder` so the hot loop makes zero heap allocations.
pub(crate) struct GruWorkspace {
    /// Precomputed W_i{r,z,n}_c @ cond for the current frame (H each).
    /// Filled by `precompute_cond`; valid for all 160 steps of the frame.
    cond_wr: Box<[f32]>,
    cond_wz: Box<[f32]>,
    cond_wn: Box<[f32]>,
    wr_x: Box<[f32]>,   // hidden
    whr_h: Box<[f32]>,  // hidden
    r: Box<[f32]>,      // hidden
    wz_x: Box<[f32]>,   // hidden
    whz_h: Box<[f32]>,  // hidden
    z: Box<[f32]>,      // hidden
    wn_x: Box<[f32]>,   // hidden
    whn_h: Box<[f32]>,  // hidden
    n: Box<[f32]>,      // hidden
    a: Box<[f32]>,      // dual_fc_hidden
    logits: Box<[f32]>, // MU_CHANNELS
}

impl GruWorkspace {
    fn new(hidden: usize, dual_fc_hidden: usize) -> Self {
        let h = || vec![0f32; hidden].into_boxed_slice();
        Self {
            cond_wr: h(),
            cond_wz: h(),
            cond_wn: h(),
            wr_x: h(),
            whr_h: h(),
            r: h(),
            wz_x: h(),
            whz_h: h(),
            z: h(),
            wn_x: h(),
            whn_h: h(),
            n: h(),
            a: vec![0f32; dual_fc_hidden].into_boxed_slice(),
            logits: vec![0f32; MU_CHANNELS].into_boxed_slice(),
        }
    }
}

/// Precompute the cond contributions to all three GRU input gates.
/// Must be called once per frame before the `gru_step` loop, whenever
/// `cond` changes.  Results are cached in `s.cond_wr/wz/wn`.
pub(crate) fn precompute_cond(cond: &[f32], w: &GruWeights, s: &mut GruWorkspace) {
    faer_gemv(&w.w_ir_c, cond, &mut s.cond_wr);
    faer_gemv(&w.w_iz_c, cond, &mut s.cond_wz);
    faer_gemv(&w.w_in_c, cond, &mut s.cond_wn);
}

/// GRU + FC step: given the previous µ-law code and hidden state,
/// produce the next µ-law code.  `h` is updated in-place; `s` is a
/// workspace buffer (no heap allocation).  Caller must have called
/// `precompute_cond` for the current frame before entering the loop.
///
/// PyTorch GRU convention:
///   r = sigmoid(W_ir @ x + b_ir + W_hr @ h + b_hr)
///   z = sigmoid(W_iz @ x + b_iz + W_hz @ h + b_hz)
///   n = tanh(W_in @ x + b_in + r * (W_hn @ h + b_hn))
///   h' = (1 - z) * n + z * h
///   a  = tanh(fc1 @ h' + fc1_bias)
///   next_mu = argmax(fc2 @ a + fc2_bias)
///
/// x = concat(embed[prev_mu], cond); the cond part is precomputed
/// by `precompute_cond`; only the 8-element embed part is computed here.
pub(crate) fn gru_step(prev_mu: u8, h: &mut [f32], w: &GruWeights, s: &mut GruWorkspace) -> u8 {
    let hidden = w.hidden;
    let embed = &w.sample_embed[usize::from(prev_mu)];

    // r gate: W_ir_e @ embed + cond_wr (precomputed) + b_ir + W_hr @ h + b_hr
    faer_gemv(&w.w_ir_e, embed, &mut s.wr_x);
    faer_gemv(&w.w_hr, h, &mut s.whr_h);
    for i in 0..hidden {
        s.r[i] = sigmoid(s.wr_x[i] + s.cond_wr[i] + w.b_ir[i] + s.whr_h[i] + w.b_hr[i]);
    }

    // z gate
    faer_gemv(&w.w_iz_e, embed, &mut s.wz_x);
    faer_gemv(&w.w_hz, h, &mut s.whz_h);
    for i in 0..hidden {
        s.z[i] = sigmoid(s.wz_x[i] + s.cond_wz[i] + w.b_iz[i] + s.whz_h[i] + w.b_hz[i]);
    }

    // n gate
    faer_gemv(&w.w_in_e, embed, &mut s.wn_x);
    faer_gemv(&w.w_hn, h, &mut s.whn_h);
    for i in 0..hidden {
        s.n[i] = (s.wn_x[i] + s.cond_wn[i] + w.b_in[i] + s.r[i] * (s.whn_h[i] + w.b_hn[i])).tanh();
    }

    // h' = (1 - z) * n + z * h  (written back into h in-place)
    for ((h_i, &z_i), &n_i) in h.iter_mut().zip(s.z.iter()).zip(s.n.iter()) {
        *h_i = (1.0 - z_i) * n_i + z_i * *h_i;
    }

    // dual FC: a = tanh(fc1 @ h' + fc1_bias)
    faer_gemv(&w.fc1_weight, h, &mut s.a);
    for (v, &b) in s.a.iter_mut().zip(w.fc1_bias.iter()) {
        *v = (*v + b).tanh();
    }

    // logits = fc2 @ a + fc2_bias
    faer_gemv(&w.fc2_weight, &s.a, &mut s.logits);
    for (v, &b) in s.logits.iter_mut().zip(w.fc2_bias.iter()) {
        *v += b;
    }

    // next_mu = argmax(logits)
    s.logits
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(i, _)| i as u8)
        .unwrap_or(MU_SILENCE)
}

/// Matrix-vector product: out = W * x, written into `out`.
/// faer handles SIMD dispatch (AVX2+FMA on x86-64, NEON on aarch64)
/// via its pulp backend at runtime.
fn faer_gemv(w: &Mat<f32>, x: &[f32], out: &mut [f32]) {
    let nrows = w.nrows();
    let ncols = w.ncols();
    let x_ref = MatRef::<f32>::from_column_major_slice(x, ncols, 1);
    let out_mut = MatMut::<f32>::from_column_major_slice_mut(out, nrows, 1);
    faer::linalg::matmul::matmul(
        out_mut,
        Accum::Replace,
        w.as_ref(),
        x_ref,
        1.0_f32,
        Par::Seq,
    );
}

#[inline(always)]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// µ-law decode: 8-bit code 0..=255 → f32 PCM in [-1, 1].
/// Then scale to i16 range.
pub(crate) fn ulaw_decode(mu: u8) -> i16 {
    const MU: f32 = 255.0;
    let x = f32::from(mu) * 2.0 / MU - 1.0;
    let linear = x.signum() * ((1.0 + MU).powf(x.abs()) - 1.0) / MU;
    (linear * 32768.0).clamp(-32768.0, 32767.0) as i16
}

/// Neural decoder using the tract frame-conditioning model and a native
/// Rust GRU step kernel.  Drop-in replacement for `NeuralDecoderVocoder`
/// on architectures where ONNX runtime is unavailable or slow.
///
/// Context window: same 5-frame symmetric window with 2-frame lookahead
/// as `NeuralDecoderVocoder`.  First 2 `decode` calls return silence
/// while the buffer fills.
pub(crate) struct NativeGruDecoder {
    /// Tract frame model: [1,5,9] i64 → [1,128] f32.  Called once/frame.
    frame_plan: TypedRunnableModel<TypedModel>,
    weights: GruWeights,
    /// Cumulative wall-time in the frame model (ns).
    frame_ns: u64,
    /// Cumulative wall-time in GRU steps per frame (ns).
    step_ns: u64,
    /// Frames timed.
    frames_timed: u64,
    pending: VecDeque<[i64; 9]>,
    history: VecDeque<[i64; 9]>,
    prev_mu: u8,
    h: Box<[f32]>,
    scratch: GruWorkspace,
    out_db: dsp::dB,
}

impl NativeGruDecoder {
    pub(crate) fn open(frame_model_path: &Path, weights_dir: &Path) -> Result<Self, VocoderError> {
        let onnx = tract_onnx::onnx();
        let frame_proto = onnx
            .proto_model_for_path(frame_model_path)
            .map_err(|e| init_err(format!("load {}: {e}", frame_model_path.display())))?;

        let mu_silence_i64: i64 = {
            use std::collections::HashMap;
            use std::str::FromStr;
            let props: HashMap<&str, &str> = frame_proto
                .metadata_props
                .iter()
                .map(|kv| (kv.key.as_str(), kv.value.as_str()))
                .collect();
            props
                .get("nambe.mu_silence")
                .and_then(|s| i64::from_str(s).ok())
                .unwrap_or(128)
        };
        if mu_silence_i64 as u8 != MU_SILENCE {
            return Err(init_err(format!(
                "nambe.mu_silence={mu_silence_i64}, expected {MU_SILENCE}"
            )));
        }

        let frame_plan = onnx
            .parse(&frame_proto, None)
            .map_err(|e| init_err(format!("parse {}: {e}", frame_model_path.display())))?
            .model
            .into_typed()
            .map_err(|e| init_err(format!("into_typed: {e}")))?
            .into_optimized()
            .map_err(|e| init_err(format!("optimize: {e}")))?
            .into_runnable()
            .map_err(|e| init_err(format!("runnable: {e}")))?;

        info!(
            path = %frame_model_path.display(),
            "native GRU decoder: frame model loaded"
        );

        let weights = GruWeights::load(weights_dir)?;
        let hidden = weights.hidden;
        let dual_fc_hidden = weights.dual_fc_hidden;
        info!(
            dir = %weights_dir.display(),
            hidden,
            dual_fc_hidden,
            "native GRU decoder: weights loaded"
        );

        Ok(Self {
            frame_plan,
            scratch: GruWorkspace::new(hidden, dual_fc_hidden),
            weights,
            frame_ns: 0,
            step_ns: 0,
            frames_timed: 0,
            pending: VecDeque::new(),
            history: VecDeque::new(),
            prev_mu: MU_SILENCE,
            h: vec![0.0; hidden].into_boxed_slice(),
            out_db: dsp::dB::UNITY,
        })
    }

    fn run_frame(&mut self, window: &[[i64; 9]; 5]) -> Result<PcmFrame, VocoderError> {
        // Special frames (b0 >= 120): erasure, silence, tone.  Bypass the GRU
        // and return silence; h is preserved so the model recovers on speech.
        if window[2][0] >= B0_SPECIAL_MIN {
            self.prev_mu = MU_SILENCE;
            let mut out = [0i16; PCM_SAMPLES];
            self.out_db.apply(&mut out);
            return Ok(out);
        }

        // Frame model: [1,5,9] → cond [1,128]
        let bits_data: Vec<i64> = window.iter().flat_map(|r| r.iter().copied()).collect();
        let bits_tensor = tract_ndarray::Array3::from_shape_vec((1usize, 5, 9), bits_data)
            .map_err(|e| VocoderError::Decode(format!("bits_window shape: {e}")))?
            .into_tensor();

        let t_frame = Instant::now();
        let frame_out = self
            .frame_plan
            .run(tvec![bits_tensor.into()])
            .map_err(|e| VocoderError::Decode(format!("frame inference: {e}")))?;
        self.frame_ns += t_frame.elapsed().as_nanos() as u64;

        let cond_slice = frame_out[0]
            .as_slice::<f32>()
            .map_err(|e| VocoderError::Decode(format!("cond output: {e}")))?;
        if cond_slice.len() != COND_DIM {
            return Err(VocoderError::Decode(format!(
                "cond dim {} != {COND_DIM}",
                cond_slice.len()
            )));
        }
        let mut cond = [0f32; COND_DIM];
        cond.copy_from_slice(cond_slice);

        // Native GRU: 160 steps.  cond is constant across the frame, so
        // precompute its gate contributions once before the step loop.
        let t_step = Instant::now();
        precompute_cond(&cond, &self.weights, &mut self.scratch);
        let mut out = [0i16; PCM_SAMPLES];
        let mut prev_mu = self.prev_mu;
        for s in out.iter_mut() {
            let next_mu = gru_step(prev_mu, &mut self.h, &self.weights, &mut self.scratch);
            *s = ulaw_decode(next_mu);
            prev_mu = next_mu;
        }
        self.step_ns += t_step.elapsed().as_nanos() as u64;
        self.frames_timed += 1;
        self.prev_mu = prev_mu;
        self.out_db.apply(&mut out);
        Ok(out)
    }

    pub(crate) fn timing_us(&self) -> (u64, u64, u64) {
        let f = self.frames_timed.max(1);
        (
            self.frame_ns / f / 1000,
            self.step_ns / f / 1000,
            self.frames_timed,
        )
    }
}

impl Vocoder for NativeGruDecoder {
    fn encode(&mut self, _pcm: &PcmFrame) -> Result<AmbeFrame, VocoderError> {
        Err(VocoderError::Unsupported(
            "NativeGruDecoder does not support encode",
        ))
    }

    fn decode(&mut self, ambe: Option<&AmbeFrame>) -> Result<PcmFrame, VocoderError> {
        let vq = match ambe {
            Some(frame) => crate::neural::frame_to_vq(frame),
            None => crate::neural::frame_to_vq(&crate::SILENCE_FRAME),
        };
        self.pending.push_back(vq);
        if self.pending.len() < 3 {
            return Ok([0i16; PCM_SAMPLES]);
        }
        let target = self.pending[0];
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
        self.prev_mu = MU_SILENCE;
        self.h.fill(0.0);
    }

    fn set_gain(&mut self, _in_db: dsp::dB, out_db: dsp::dB) -> Result<(), VocoderError> {
        self.out_db = out_db;
        Ok(())
    }
}

// -- Weight file loading helpers --

/// Load a [MU_CHANNELS × EMBED_DIM] lookup table from a raw f32 LE binary file.
fn load_embed(
    dir: &Path,
    name: &str,
) -> Result<Box<[[f32; EMBED_DIM]; MU_CHANNELS]>, VocoderError> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path)
        .map_err(|e| VocoderError::Init(format!("read {}: {e}", path.display())))?;
    let expected = MU_CHANNELS * EMBED_DIM * 4;
    if bytes.len() != expected {
        return Err(VocoderError::Init(format!(
            "{}: expected {expected} bytes, got {}",
            name,
            bytes.len()
        )));
    }
    let mut mat = Box::new([[0f32; EMBED_DIM]; MU_CHANNELS]);
    for (i, row) in mat.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            let off = (i * EMBED_DIM + j) * 4;
            *v = f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        }
    }
    Ok(mat)
}

/// Read a [nrows × INPUT] weight file and split it into embed [nrows × EMBED_DIM]
/// and cond [nrows × COND_DIM] halves (columns 0..8 and 8..136 respectively).
/// Both halves are stored column-major for faer GEMV.
fn load_input_matrix_split(
    dir: &Path,
    name: &str,
    nrows: usize,
) -> Result<(Mat<f32>, Mat<f32>), VocoderError> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path)
        .map_err(|e| VocoderError::Init(format!("read {}: {e}", path.display())))?;
    let expected = nrows * INPUT * 4;
    if bytes.len() != expected {
        return Err(VocoderError::Init(format!(
            "{}: expected {expected} bytes, got {}",
            name,
            bytes.len()
        )));
    }
    let embed = Mat::<f32>::from_fn(nrows, EMBED_DIM, |i, j| {
        let off = (i * INPUT + j) * 4;
        f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
    });
    let cond = Mat::<f32>::from_fn(nrows, COND_DIM, |i, j| {
        let off = (i * INPUT + EMBED_DIM + j) * 4;
        f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
    });
    Ok((embed, cond))
}

/// Load a [nrows × ncols] weight matrix from a row-major f32 LE binary file
/// into a column-major `faer::Mat<f32>`.  The transpose at load time lets
/// faer's GEMV kernel access columns contiguously.
fn load_matrix_faer(
    dir: &Path,
    name: &str,
    nrows: usize,
    ncols: usize,
) -> Result<Mat<f32>, VocoderError> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path)
        .map_err(|e| VocoderError::Init(format!("read {}: {e}", path.display())))?;
    let expected = nrows * ncols * 4;
    if bytes.len() != expected {
        return Err(VocoderError::Init(format!(
            "{}: expected {expected} bytes, got {}",
            name,
            bytes.len()
        )));
    }
    let mat = Mat::<f32>::from_fn(nrows, ncols, |i, j| {
        let off = (i * ncols + j) * 4;
        f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
    });
    Ok(mat)
}

/// Load a bias vector of `len` f32 values from a raw f32 LE binary file.
fn load_bias(dir: &Path, name: &str, len: usize) -> Result<Box<[f32]>, VocoderError> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path)
        .map_err(|e| VocoderError::Init(format!("read {}: {e}", path.display())))?;
    let expected = len * 4;
    if bytes.len() != expected {
        return Err(VocoderError::Init(format!(
            "{}: expected {expected} bytes, got {}",
            name,
            bytes.len()
        )));
    }
    let v: Box<[f32]> = (0..len)
        .map(|i| f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()))
        .collect();
    Ok(v)
}

/// Parse `meta.json` from the weights directory, validate fixed dimensions,
/// and return `(gru_hidden, dual_fc_hidden)` for runtime sizing.
fn read_meta(dir: &Path) -> Result<(usize, usize), VocoderError> {
    let path = dir.join("meta.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| VocoderError::Init(format!("read {}: {e}", path.display())))?;
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| VocoderError::Init(format!("parse {}: {e}", path.display())))?;

    let read_usize = |key: &str| -> Result<usize, VocoderError> {
        v[key]
            .as_u64()
            .ok_or_else(|| VocoderError::Init(format!("meta.json: missing or non-integer '{key}'")))
            .map(|n| n as usize)
    };
    let check = |key: &str, expected: usize| -> Result<(), VocoderError> {
        let got = read_usize(key)?;
        if got != expected {
            return Err(VocoderError::Init(format!(
                "meta.json: {key}={got}, expected {expected}"
            )));
        }
        Ok(())
    };

    check("gru_input_size", INPUT)?;
    check("sample_embed_dim", EMBED_DIM)?;
    check("cond_dim", COND_DIM)?;
    check("mu_channels", MU_CHANNELS)?;
    check("mu_silence", usize::from(MU_SILENCE))?;
    check("samples_per_frame", PCM_SAMPLES)?;

    let hidden = read_usize("gru_hidden")?;
    let dual_fc_hidden = read_usize("dual_fc_hidden")?;

    Ok((hidden, dual_fc_hidden))
}

fn init_err(msg: String) -> VocoderError {
    VocoderError::Init(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ulaw_decode_silence_near_zero() {
        let s = ulaw_decode(MU_SILENCE);
        assert!(s.abs() < 10, "code {MU_SILENCE} decoded to {s}");
    }

    #[test]
    fn ulaw_decode_monotone() {
        // Higher codes should generally decode to more positive values.
        let samples: Vec<i16> = (0u8..=255).map(ulaw_decode).collect();
        for w in samples.windows(2) {
            assert!(w[1] >= w[0], "not monotone at {} -> {}", w[0], w[1]);
        }
    }

    /// Run `steps` GRU steps with constant `cond`, comparing native and ONNX
    /// outputs at each step.  Asserts native == ONNX; returns max hidden-state
    /// absolute error.
    fn run_step_loop(
        label: &str,
        step_plan: &TypedRunnableModel<TypedModel>,
        weights: &GruWeights,
        workspace: &mut GruWorkspace,
        cond: &[f32],
        steps: usize,
    ) -> f32 {
        let hidden = weights.hidden;
        let mut h_native = vec![0f32; hidden];
        let mut h_onnx = tract_ndarray::Array3::<f32>::zeros((1, 1, hidden));
        let mut prev_mu_native: u8 = MU_SILENCE;
        let mut prev_mu_onnx: i64 = i64::from(MU_SILENCE);
        let mut max_h_err = 0f32;

        precompute_cond(cond, weights, workspace);
        for step in 0..steps {
            let next_native = gru_step(prev_mu_native, &mut h_native, weights, workspace);

            let mu_t = tract_ndarray::arr1(&[prev_mu_onnx]).into_tensor();
            let cond_t = tract_ndarray::Array2::from_shape_vec((1, COND_DIM), cond.to_vec())
                .unwrap()
                .into_tensor();
            let h_t = h_onnx.clone().into_tensor();
            let out = step_plan
                .run(tvec![mu_t.into(), cond_t.into(), h_t.into()])
                .expect("onnx step");
            let mu_onnx_slice = out[0].as_slice::<i64>().expect("mu_out");
            let h_onnx_slice = out[1].as_slice::<f32>().expect("h_out");
            let next_onnx = mu_onnx_slice[0] as u8;
            h_onnx.as_slice_mut().unwrap().copy_from_slice(h_onnx_slice);

            assert_eq!(
                next_native, next_onnx,
                "{label} step {step}: native={next_native} onnx={next_onnx}"
            );

            let step_h_err = h_native
                .iter()
                .zip(h_onnx_slice.iter())
                .map(|(&a, &b)| (a - b).abs())
                .fold(0f32, f32::max);
            if step_h_err > max_h_err {
                max_h_err = step_h_err;
            }

            prev_mu_native = next_native;
            prev_mu_onnx = i64::from(next_onnx);
        }

        max_h_err
    }

    /// Run the zero-cond ONNX oracle against the native GRU for 500 steps.
    /// Returns false and prints a skip message if either path is absent.
    fn run_onnx_oracle(label: &str, step_model: &Path, weights_dir: &Path) -> bool {
        if !step_model.exists() || !weights_dir.exists() {
            eprintln!("{label}: fixtures absent; skipping");
            return false;
        }

        let weights = GruWeights::load(weights_dir).expect("load weights");
        let mut workspace = GruWorkspace::new(weights.hidden, weights.dual_fc_hidden);
        let step_plan = tract_onnx::onnx()
            .model_for_path(step_model)
            .expect("load step model")
            .into_optimized()
            .expect("optimize")
            .into_runnable()
            .expect("runnable");

        let cond = [0f32; COND_DIM];
        let max_h_err = run_step_loop(label, &step_plan, &weights, &mut workspace, &cond, 500);
        eprintln!("{label}: max h error over 500 steps: {max_h_err:.2e}");
        assert!(
            max_h_err < 1e-4,
            "{label}: max h error {max_h_err:.2e} exceeds 1e-4"
        );
        true
    }

    /// Parity oracle for h256 weights (nambe dev path; skips if absent).
    #[test]
    fn gru_step_matches_onnx_oracle_h256() {
        let nambe = std::path::Path::new("/home/ch/src/nambe/runs");
        run_onnx_oracle(
            "h256",
            &nambe.join("decoder-d4-split/decoder_step.onnx"),
            &nambe.join("decoder-d4-weights"),
        );
    }

    /// Parity oracle for h96 weights (committed to models/; always runs).
    #[test]
    fn gru_step_matches_onnx_oracle_h96() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let weights_dir = manifest.join("../models/decoder-d6-h96-weights");
        let step_model = weights_dir.join("decoder_step.onnx");
        assert!(
            run_onnx_oracle("h96", &step_model, &weights_dir),
            "h96 oracle fixtures missing"
        );
    }

    /// Long-run oracle with constant silence cond from the frame model.
    /// Reproduces the regime that causes triangle-wave output in roundtrip:
    /// bench120s.wav is all-silence so every frame uses the silence cond.
    /// Zero-cond oracle passes at 500 steps; this test catches limit cycles
    /// that emerge under constant non-zero cond over many frames (3200 steps
    /// = 20 frames * 160 steps/frame).
    #[test]
    fn gru_step_silence_cond_oracle_h96() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let weights_dir = manifest.join("../models/decoder-d6-h96-weights");
        let step_model = weights_dir.join("decoder_step.onnx");
        let frame_model_path = weights_dir.join("decoder_frame.onnx");
        if !step_model.exists() || !weights_dir.exists() || !frame_model_path.exists() {
            eprintln!("gru_step_silence_cond_oracle_h96: fixtures absent; skipping");
            return;
        }

        // Derive silence cond: run frame model with 5 silence VQ frames.
        let silence_vq: [i64; 9] = [124, 16, 1, 52, 4, 18, 14, 12, 1];
        let window: [[i64; 9]; 5] = [silence_vq; 5];
        let bits_data: Vec<i64> = window.iter().flat_map(|r| r.iter().copied()).collect();
        let bits_tensor = tract_ndarray::Array3::from_shape_vec((1usize, 5, 9), bits_data)
            .unwrap()
            .into_tensor();
        let frame_plan = tract_onnx::onnx()
            .model_for_path(&frame_model_path)
            .expect("load frame model")
            .into_optimized()
            .expect("optimize")
            .into_runnable()
            .expect("runnable");
        let frame_out = frame_plan
            .run(tvec![bits_tensor.into()])
            .expect("frame inference");
        let cond_slice = frame_out[0].as_slice::<f32>().expect("cond output");
        assert_eq!(cond_slice.len(), COND_DIM);
        let mut silence_cond = [0f32; COND_DIM];
        silence_cond.copy_from_slice(cond_slice);
        eprintln!("silence_cond[:4] = {:?}", &silence_cond[..4]);

        let weights = GruWeights::load(&weights_dir).expect("load weights");
        let mut workspace = GruWorkspace::new(weights.hidden, weights.dual_fc_hidden);
        let step_plan = tract_onnx::onnx()
            .model_for_path(&step_model)
            .expect("load step model")
            .into_optimized()
            .expect("optimize")
            .into_runnable()
            .expect("runnable");

        let max_h_err = run_step_loop(
            "silence_cond h96",
            &step_plan,
            &weights,
            &mut workspace,
            &silence_cond,
            3200,
        );
        eprintln!("silence_cond oracle h96: max h error over 3200 steps: {max_h_err:.2e}");
        assert!(
            max_h_err < 1e-4,
            "silence_cond oracle h96: max h error {max_h_err:.2e} exceeds 1e-4"
        );
    }
}
