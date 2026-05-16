//! Native Rust GRU decoder: weights, one-step math, and the
//! `NativeGruDecoder` vocoder that replaces the ONNX step model with
//! this kernel while keeping the tract frame-conditioning model.
//!
//! Weight layout (all f32 LE, row-major on disk):
//!   sample_embed  [256,  8]
//!   W_ir/iz/in    [256, 136]  GRU input weights  (r, z, n gates)
//!   W_hr/hz/hn    [256, 256]  GRU hidden weights (r, z, n gates)
//!   b_ir/iz/in    [256]       GRU input biases
//!   b_hr/hz/hn    [256]       GRU hidden biases
//!   fc1_weight    [256, 256]  dual-FC layer 1 weight
//!   fc1_bias      [256]
//!   fc2_weight    [256, 256]  dual-FC layer 2 weight
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

const HIDDEN: usize = 256;
const INPUT: usize = 136; // embed(8) + cond(128)
const MU_CHANNELS: usize = 256;
const EMBED_DIM: usize = 8;
const COND_DIM: usize = 128;
const MU_SILENCE: u8 = 128;

/// All GRU weight matrices and bias vectors, loaded from a flat-binary
/// weight directory.  Weight matrices are stored as column-major
/// `faer::Mat<f32>` (transposed from the row-major on-disk layout);
/// SIMD dispatch is handled automatically by faer's pulp backend.
pub(crate) struct GruWeights {
    /// [256, 8] µ-law embedding lookup table.
    sample_embed: Box<[[f32; EMBED_DIM]; MU_CHANNELS]>,
    /// [256, 136] GRU input weights for r, z, n gates.
    w_ir: Mat<f32>,
    w_iz: Mat<f32>,
    w_in: Mat<f32>,
    /// [256, 256] GRU hidden weights for r, z, n gates.
    w_hr: Mat<f32>,
    w_hz: Mat<f32>,
    w_hn: Mat<f32>,
    /// [256] GRU biases: input and hidden, for r, z, n.
    b_ir: [f32; HIDDEN],
    b_iz: [f32; HIDDEN],
    b_in: [f32; HIDDEN],
    b_hr: [f32; HIDDEN],
    b_hz: [f32; HIDDEN],
    b_hn: [f32; HIDDEN],
    /// [256, 256] dual-FC layer weights and biases.
    fc1_weight: Mat<f32>,
    fc1_bias: [f32; HIDDEN],
    fc2_weight: Mat<f32>,
    fc2_bias: [f32; HIDDEN],
}

impl GruWeights {
    pub(crate) fn load(dir: &Path) -> Result<Self, VocoderError> {
        validate_meta(dir)?;
        Ok(Self {
            sample_embed: load_embed(dir, "sample_embed.bin")?,
            w_ir: load_matrix_faer(dir, "W_ir.bin", HIDDEN, INPUT)?,
            w_iz: load_matrix_faer(dir, "W_iz.bin", HIDDEN, INPUT)?,
            w_in: load_matrix_faer(dir, "W_in.bin", HIDDEN, INPUT)?,
            w_hr: load_matrix_faer(dir, "W_hr.bin", HIDDEN, HIDDEN)?,
            w_hz: load_matrix_faer(dir, "W_hz.bin", HIDDEN, HIDDEN)?,
            w_hn: load_matrix_faer(dir, "W_hn.bin", HIDDEN, HIDDEN)?,
            b_ir: load_bias(dir, "b_ir.bin")?,
            b_iz: load_bias(dir, "b_iz.bin")?,
            b_in: load_bias(dir, "b_in.bin")?,
            b_hr: load_bias(dir, "b_hr.bin")?,
            b_hz: load_bias(dir, "b_hz.bin")?,
            b_hn: load_bias(dir, "b_hn.bin")?,
            fc1_weight: load_matrix_faer(dir, "fc1_weight.bin", HIDDEN, HIDDEN)?,
            fc1_bias: load_bias(dir, "fc1_bias.bin")?,
            fc2_weight: load_matrix_faer(dir, "fc2_weight.bin", HIDDEN, HIDDEN)?,
            fc2_bias: load_bias(dir, "fc2_bias.bin")?,
        })
    }
}

/// GRU + FC step: given the previous µ-law code, frame conditioning
/// vector, and hidden state, produce the next µ-law code and new state.
///
/// PyTorch GRU convention:
///   r = sigmoid(W_ir @ x + b_ir + W_hr @ h + b_hr)
///   z = sigmoid(W_iz @ x + b_iz + W_hz @ h + b_hz)
///   n = tanh(W_in @ x + b_in + r * (W_hn @ h + b_hn))
///   h' = (1 - z) * n + z * h
///   a  = tanh(fc1 @ h' + fc1_bias)
///   next_mu = argmax(fc2 @ a + fc2_bias)
pub(crate) fn gru_step(
    prev_mu: u8,
    cond: &[f32; COND_DIM],
    h: &[f32; HIDDEN],
    w: &GruWeights,
) -> (u8, [f32; HIDDEN]) {
    // Build input: concat(embed[prev_mu], cond) -> [136]
    let mut x = [0f32; INPUT];
    x[..EMBED_DIM].copy_from_slice(&w.sample_embed[usize::from(prev_mu)]);
    x[EMBED_DIM..].copy_from_slice(cond);

    // r gate
    let mut wr_x = [0f32; HIDDEN];
    let mut whr_h = [0f32; HIDDEN];
    faer_gemv(&w.w_ir, &x, &mut wr_x);
    faer_gemv(&w.w_hr, h, &mut whr_h);
    let mut r = [0f32; HIDDEN];
    for i in 0..HIDDEN {
        r[i] = sigmoid(wr_x[i] + w.b_ir[i] + whr_h[i] + w.b_hr[i]);
    }

    // z gate
    let mut wz_x = [0f32; HIDDEN];
    let mut whz_h = [0f32; HIDDEN];
    faer_gemv(&w.w_iz, &x, &mut wz_x);
    faer_gemv(&w.w_hz, h, &mut whz_h);
    let mut z = [0f32; HIDDEN];
    for i in 0..HIDDEN {
        z[i] = sigmoid(wz_x[i] + w.b_iz[i] + whz_h[i] + w.b_hz[i]);
    }

    // n gate
    let mut wn_x = [0f32; HIDDEN];
    let mut whn_h = [0f32; HIDDEN];
    faer_gemv(&w.w_in, &x, &mut wn_x);
    faer_gemv(&w.w_hn, h, &mut whn_h);
    let mut n = [0f32; HIDDEN];
    for i in 0..HIDDEN {
        n[i] = (wn_x[i] + w.b_in[i] + r[i] * (whn_h[i] + w.b_hn[i])).tanh();
    }

    // h' = (1 - z) * n + z * h
    let mut h_new = [0f32; HIDDEN];
    for i in 0..HIDDEN {
        h_new[i] = (1.0 - z[i]) * n[i] + z[i] * h[i];
    }

    // dual FC: a = tanh(fc1 @ h' + fc1_bias)
    let mut a = [0f32; HIDDEN];
    faer_gemv(&w.fc1_weight, &h_new, &mut a);
    for (v, &b) in a.iter_mut().zip(w.fc1_bias.iter()) {
        *v = (*v + b).tanh();
    }

    // logits = fc2 @ a + fc2_bias
    let mut logits = [0f32; HIDDEN];
    faer_gemv(&w.fc2_weight, &a, &mut logits);
    for (v, &b) in logits.iter_mut().zip(w.fc2_bias.iter()) {
        *v += b;
    }

    // next_mu = argmax(logits)
    let next_mu = logits
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(i, _)| i as u8)
        .unwrap_or(MU_SILENCE);

    (next_mu, h_new)
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
    h: Box<[f32; HIDDEN]>,
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
        info!(
            dir = %weights_dir.display(),
            "native GRU decoder: weights loaded"
        );

        Ok(Self {
            frame_plan,
            weights,
            frame_ns: 0,
            step_ns: 0,
            frames_timed: 0,
            pending: VecDeque::new(),
            history: VecDeque::new(),
            prev_mu: MU_SILENCE,
            h: Box::new([0.0; HIDDEN]),
            out_db: dsp::dB::UNITY,
        })
    }

    fn run_frame(&mut self, window: &[[i64; 9]; 5]) -> Result<PcmFrame, VocoderError> {
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

        // Native GRU: 160 steps
        let t_step = Instant::now();
        let mut out = [0i16; PCM_SAMPLES];
        let mut prev_mu = self.prev_mu;
        for s in &mut out {
            let (next_mu, h_new) = gru_step(prev_mu, &cond, &self.h, &self.weights);
            *self.h = h_new;
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
        *self.h = [0.0; HIDDEN];
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

/// Load a [nrows × ncols] weight matrix from a row-major f32 LE binary file
/// into a column-major `faer::Mat<f32>`.  The transpose at load time lets
/// faer's pulp GEMV kernel access columns contiguously.
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

/// Load a [256] bias vector from a raw f32 LE binary file.
fn load_bias(dir: &Path, name: &str) -> Result<[f32; HIDDEN], VocoderError> {
    let path = dir.join(name);
    let bytes = std::fs::read(&path)
        .map_err(|e| VocoderError::Init(format!("read {}: {e}", path.display())))?;
    let expected = HIDDEN * 4;
    if bytes.len() != expected {
        return Err(VocoderError::Init(format!(
            "{}: expected {expected} bytes, got {}",
            name,
            bytes.len()
        )));
    }
    let mut v = [0f32; HIDDEN];
    for (i, x) in v.iter_mut().enumerate() {
        let off = i * 4;
        *x = f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
    }
    Ok(v)
}

/// Parse `meta.json` from the weights directory and validate it against the
/// compile-time architecture constants.  Returns an error if the file is
/// missing, malformed, or contains unexpected values.
fn validate_meta(dir: &Path) -> Result<(), VocoderError> {
    let path = dir.join("meta.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| VocoderError::Init(format!("read {}: {e}", path.display())))?;
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| VocoderError::Init(format!("parse {}: {e}", path.display())))?;

    let check = |key: &str, expected: usize| -> Result<(), VocoderError> {
        let got = v[key].as_u64().ok_or_else(|| {
            VocoderError::Init(format!("meta.json: missing or non-integer '{key}'"))
        })? as usize;
        if got != expected {
            return Err(VocoderError::Init(format!(
                "meta.json: {key}={got}, expected {expected}"
            )));
        }
        Ok(())
    };

    check("gru_hidden", HIDDEN)?;
    check("gru_input_size", INPUT)?;
    check("sample_embed_dim", EMBED_DIM)?;
    check("cond_dim", COND_DIM)?;
    check("mu_channels", MU_CHANNELS)?;
    check("mu_silence", usize::from(MU_SILENCE))?;
    check("samples_per_frame", PCM_SAMPLES)?;
    check("dual_fc_hidden", HIDDEN)?;
    Ok(())
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

    /// Parity test: native GRU must produce the same µ-law sequence as
    /// the step-1 ONNX model over 500 consecutive steps from the same
    /// initial state.  Skips if the oracle model or weights aren't present.
    #[test]
    fn gru_step_matches_onnx_oracle() {
        let nambe = std::path::Path::new("/home/ch/src/nambe/runs");
        let step_model = nambe.join("decoder-d4-split/decoder_step.onnx");
        let weights_dir = nambe.join("decoder-d4-weights");
        if !step_model.exists() || !weights_dir.exists() {
            eprintln!("gru_step_matches_onnx_oracle: fixtures absent; skipping");
            return;
        }

        let weights = GruWeights::load(&weights_dir).expect("load weights");

        let onnx = tract_onnx::onnx();
        let step_plan = onnx
            .model_for_path(&step_model)
            .expect("load step model")
            .into_optimized()
            .expect("optimize")
            .into_runnable()
            .expect("runnable");

        // Shared initial state: silence mu, zero hidden.
        let mut h_native = [0f32; HIDDEN];
        let mut h_onnx = tract_ndarray::Array3::<f32>::zeros((1, 1, HIDDEN));
        let mut prev_mu_native: u8 = MU_SILENCE;
        let mut prev_mu_onnx: i64 = i64::from(MU_SILENCE);
        // Fixed conditioning: all zeros (arbitrary but reproducible).
        let cond = [0f32; COND_DIM];

        let mut max_h_err = 0f32;

        for step in 0..500usize {
            // Native GRU step.
            let (next_native, h_new) = gru_step(prev_mu_native, &cond, &h_native, &weights);
            h_native = h_new;

            // ONNX step-1 model: inputs are prev_mu [1] i64, cond [1,128] f32, h [1,1,256] f32.
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
                "step {step}: native={next_native} onnx={next_onnx}"
            );

            // Track worst-case hidden-state divergence.
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

        eprintln!("max h error over 500 steps: {max_h_err:.2e}");
        assert!(max_h_err < 1e-4, "max h error {max_h_err:.2e} exceeds 1e-4");
    }
}
