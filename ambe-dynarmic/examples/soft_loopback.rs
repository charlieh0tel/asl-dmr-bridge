// soft_loopback: read a WAV (8 kHz mono i16), encode + decode every frame
// through the in-process softambe codec (no UDP), and write the result.
//
// Use this to listen to lib-only soft_soft output and check for ticks
// without involving the UDP server, network, or any other backend.
//
// Usage:
//   cargo run --release --example soft_loopback -- <input.wav> <output.wav>
//   cargo run --release --example soft_loopback   # uses test fixture

use softambe::PCM_FRAME_SAMPLES;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let input = args.get(1).map(String::as_str).unwrap_or("tests/fixtures/voice.wav");
    let output = args.get(2).map(String::as_str).unwrap_or("/tmp/soft_loopback.wav");

    let pcm = read_wav_i16(input);
    let frames = pcm.len() / PCM_FRAME_SAMPLES;
    eprintln!("input: {} samples ({} full frames)", pcm.len(), frames);

    softambe::reset();
    let mut ambe = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut frame = [0i16; PCM_FRAME_SAMPLES];
        frame.copy_from_slice(&pcm[f * PCM_FRAME_SAMPLES..(f + 1) * PCM_FRAME_SAMPLES]);
        ambe.push(softambe::encode_fec(&frame));
    }

    softambe::reset();
    let mut out = Vec::with_capacity(frames * PCM_FRAME_SAMPLES);
    for frame in &ambe {
        out.extend_from_slice(&softambe::decode_fec(frame));
    }

    write_wav_i16(output, &out);
    eprintln!("wrote {output} ({} samples)", out.len());
}

fn read_wav_i16(path: &str) -> Vec<i16> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    assert!(&bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE");
    assert!(&bytes[36..40] == b"data", "expected canonical 44-byte WAV header");
    let data_len = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
    let n = data_len / 2;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let off = 44 + i * 2;
        out.push(i16::from_le_bytes([bytes[off], bytes[off + 1]]));
    }
    out
}

fn write_wav_i16(path: &str, samples: &[i16]) {
    const SR: u32 = 8000;
    let data_bytes = samples.len() * 2;
    let mut buf = Vec::with_capacity(44 + data_bytes);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&1u16.to_le_bytes()); // mono
    buf.extend_from_slice(&SR.to_le_bytes());
    buf.extend_from_slice(&(SR * 2).to_le_bytes()); // byte rate
    buf.extend_from_slice(&2u16.to_le_bytes()); // block align
    buf.extend_from_slice(&16u16.to_le_bytes()); // bits/sample
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, buf).expect("write wav");
}
