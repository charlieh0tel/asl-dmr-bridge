use ambe::NeuralEncoder;
use ambe::PCM_SAMPLES;

fn main() {
    let model = std::env::args().nth(1).expect("model path");
    let mut enc = NeuralEncoder::open(std::path::Path::new(&model)).expect("open");
    let zero = [0i16; PCM_SAMPLES];
    let mut dumped = false;
    for i in 0..15 {
        let v = enc.encode_vq(&zero).expect("encode_vq");
        if v.is_some() && !dumped {
            let slice = enc.current_input_slice();
            let nonzero = slice.iter().filter(|&&s| s != 0).count();
            println!(
                "first_real_frame={i} slice_len={} nonzero={nonzero}",
                slice.len()
            );
            dumped = true;
        }
        println!("frame={i} vq={v:?}");
    }
}
