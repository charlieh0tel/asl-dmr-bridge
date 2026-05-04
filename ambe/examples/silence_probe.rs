use ambe::NeuralEncoder;
use ambe::PCM_SAMPLES;

fn main() {
    let model = std::env::args().nth(1).expect("model path");
    let mut enc = NeuralEncoder::open(std::path::Path::new(&model)).expect("open");
    let zero = [0i16; PCM_SAMPLES];
    for i in 0..15 {
        let v = enc.encode_vq(&zero).expect("encode_vq");
        println!("frame={i} vq={v:?}");
    }
}
