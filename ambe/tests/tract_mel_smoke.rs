//! §0.0 in-tree regression: load `tests/fixtures/smoke_tract_mel/`
//! through tract and confirm output matches the bundled
//! onnxruntime reference within 1e-5.  See the example
//! `tract_mel_smoke` for the interactive form (defaults to the
//! same bundle but can take an out-of-tree path).

use std::fs;
use std::path::Path;

use tract_onnx::prelude::*;

const PCM_LEN: usize = 1280;
const REFERENCE_LEN: usize = 8 * 7;
const TOLERANCE: f32 = 1e-5;

fn read_f32_le(path: &Path, expected_len: usize) -> Vec<f32> {
    let bytes = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert_eq!(
        bytes.len(),
        expected_len * 4,
        "{}: expected {} bytes, got {}",
        path.display(),
        expected_len * 4,
        bytes.len(),
    );
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

#[test]
fn tract_matches_onnxruntime_reference() {
    let bundle = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smoke_tract_mel");
    let pcm = read_f32_le(&bundle.join("input_pcm.bin"), PCM_LEN);
    let reference = read_f32_le(&bundle.join("reference_output.bin"), REFERENCE_LEN);

    let plan = tract_onnx::onnx()
        .model_for_path(bundle.join("model.onnx"))
        .expect("load model")
        .into_optimized()
        .expect("optimize")
        .into_runnable()
        .expect("into_runnable");

    let input = tract_ndarray::Array2::from_shape_vec((1, PCM_LEN), pcm)
        .expect("input shape")
        .into_tensor();
    let outputs = plan.run(tvec!(input.into())).expect("inference");
    assert_eq!(outputs.len(), 1);
    let actual = outputs[0].as_slice::<f32>().expect("f32 output");
    assert_eq!(actual.len(), reference.len());

    let max_diff = actual
        .iter()
        .zip(reference.iter())
        .map(|(a, r)| (a - r).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_diff <= TOLERANCE,
        "max_abs_diff {max_diff:.3e} > {TOLERANCE:.0e}",
    );
}
