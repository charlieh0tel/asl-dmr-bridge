// server_loopback: read a WAV (8 kHz mono i16), send each frame to a
// running softambeserver as Audio packets, collect the Ambe responses,
// then send each Ambe back and collect Audio responses.  Writes the
// final PCM out as WAV.  This exercises the full UDP/DV3000 protocol
// path so we can A/B against the lib-only loopback.
//
// Assumes softambeserver is running on 127.0.0.1:2462 (or pass --addr).
//
// Usage:
//   cargo run --release --example server_loopback -- [<input.wav>] [<output.wav>] [--addr host:port]

use softambe::{AMBE_FEC_BYTES, PCM_FRAME_SAMPLES};
use std::net::UdpSocket;
use std::time::Duration;

const START_BYTE: u8 = 0x61;
const HDR_LEN: usize = 4;
const TYPE_CONTROL: u8 = 0x00;
const TYPE_AMBE: u8 = 0x01;
const TYPE_AUDIO: u8 = 0x02;
const CTRL_RESET: u8 = 0x33;
const CTRL_RATEP: u8 = 0x0A;
const FIELD_SPEECH: u8 = 0x00;
const FIELD_CHANNEL: u8 = 0x01;
const RATEP_DMR: [u8; 12] = [
    0x04, 0x31, 0x07, 0x54, 0x24, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6F, 0x48,
];

fn main() {
    let mut args = std::env::args().skip(1);
    let mut input = String::from("tests/fixtures/voice.wav");
    let mut output = String::from("/tmp/server_loopback.wav");
    let mut addr = String::from("127.0.0.1:2462");
    let mut positional = 0;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--addr" => addr = args.next().expect("--addr <host:port>"),
            other => {
                match positional {
                    0 => input = other.to_string(),
                    1 => output = other.to_string(),
                    _ => panic!("unexpected arg: {other}"),
                }
                positional += 1;
            }
        }
    }

    let pcm_in = read_wav_i16(&input);
    let frames = pcm_in.len() / PCM_FRAME_SAMPLES;
    eprintln!("input {input}: {} samples ({frames} frames)", pcm_in.len());

    let sock = UdpSocket::bind("127.0.0.1:0").expect("bind ephemeral");
    sock.connect(&addr).expect("connect");
    sock.set_read_timeout(Some(Duration::from_millis(500))).unwrap();

    // Reset + RATEP handshake.
    sock.send(&build_control(CTRL_RESET)).unwrap();
    let _ = recv(&sock);
    let mut rp = vec![START_BYTE];
    rp.extend_from_slice(&((1 + RATEP_DMR.len()) as u16).to_be_bytes());
    rp.push(TYPE_CONTROL);
    rp.push(CTRL_RATEP);
    rp.extend_from_slice(&RATEP_DMR);
    sock.send(&rp).unwrap();
    let _ = recv(&sock);

    // 1) Encode pass: PCM -> AMBE.
    let mut ambe_stream: Vec<[u8; AMBE_FEC_BYTES]> = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut frame = [0i16; PCM_FRAME_SAMPLES];
        frame.copy_from_slice(&pcm_in[f * PCM_FRAME_SAMPLES..(f + 1) * PCM_FRAME_SAMPLES]);
        sock.send(&build_audio(&frame)).unwrap();
        let resp = recv(&sock).expect("encode response");
        ambe_stream.push(parse_ambe(&resp).expect("parse ambe response"));
    }
    eprintln!("encoded {} frames", ambe_stream.len());

    // Reset between passes so the decoder doesn't carry encoder predictor state.
    sock.send(&build_control(CTRL_RESET)).unwrap();
    let _ = recv(&sock);

    // 2) Decode pass: AMBE -> PCM.
    let mut pcm_out: Vec<i16> = Vec::with_capacity(frames * PCM_FRAME_SAMPLES);
    for frame in &ambe_stream {
        sock.send(&build_ambe(frame)).unwrap();
        let resp = recv(&sock).expect("decode response");
        let samples = parse_audio(&resp).expect("parse audio response");
        pcm_out.extend_from_slice(&samples);
    }
    eprintln!("decoded {} samples", pcm_out.len());

    write_wav_i16(&output, &pcm_out);
    eprintln!("wrote {output}");
}

fn build_control(field_id: u8) -> Vec<u8> {
    vec![START_BYTE, 0x00, 0x01, TYPE_CONTROL, field_id]
}

fn build_audio(pcm: &[i16; PCM_FRAME_SAMPLES]) -> Vec<u8> {
    // field_id(1) + num_samples(1) + samples(320)
    let payload_len = 2 + PCM_FRAME_SAMPLES * 2;
    let mut buf = Vec::with_capacity(HDR_LEN + payload_len);
    buf.push(START_BYTE);
    buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
    buf.push(TYPE_AUDIO);
    buf.push(FIELD_SPEECH);
    buf.push(PCM_FRAME_SAMPLES as u8);
    for &s in pcm {
        buf.extend_from_slice(&s.to_be_bytes());
    }
    buf
}

fn build_ambe(frame: &[u8; AMBE_FEC_BYTES]) -> Vec<u8> {
    let payload_len = 2 + AMBE_FEC_BYTES;
    let mut buf = Vec::with_capacity(HDR_LEN + payload_len);
    buf.push(START_BYTE);
    buf.extend_from_slice(&(payload_len as u16).to_be_bytes());
    buf.push(TYPE_AMBE);
    buf.push(FIELD_CHANNEL);
    buf.push((AMBE_FEC_BYTES * 8) as u8);
    buf.extend_from_slice(frame);
    buf
}

fn recv(sock: &UdpSocket) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 4096];
    sock.recv(&mut buf).ok().map(|n| {
        buf.truncate(n);
        buf
    })
}

fn parse_ambe(buf: &[u8]) -> Option<[u8; AMBE_FEC_BYTES]> {
    if buf.len() < HDR_LEN + 2 + AMBE_FEC_BYTES {
        return None;
    }
    if buf[0] != START_BYTE || buf[3] != TYPE_AMBE {
        return None;
    }
    let payload = &buf[HDR_LEN..];
    if payload[0] != FIELD_CHANNEL {
        return None;
    }
    let mut out = [0u8; AMBE_FEC_BYTES];
    out.copy_from_slice(&payload[2..2 + AMBE_FEC_BYTES]);
    Some(out)
}

fn parse_audio(buf: &[u8]) -> Option<[i16; PCM_FRAME_SAMPLES]> {
    if buf.len() < HDR_LEN + 2 + PCM_FRAME_SAMPLES * 2 {
        return None;
    }
    if buf[0] != START_BYTE || buf[3] != TYPE_AUDIO {
        return None;
    }
    let payload = &buf[HDR_LEN..];
    if payload[0] != FIELD_SPEECH {
        return None;
    }
    let mut out = [0i16; PCM_FRAME_SAMPLES];
    for (i, s) in out.iter_mut().enumerate() {
        let off = 2 + i * 2;
        *s = i16::from_be_bytes([payload[off], payload[off + 1]]);
    }
    Some(out)
}

fn read_wav_i16(path: &str) -> Vec<i16> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    assert!(&bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE");
    assert!(&bytes[36..40] == b"data");
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
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&SR.to_le_bytes());
    buf.extend_from_slice(&(SR * 2).to_le_bytes());
    buf.extend_from_slice(&2u16.to_le_bytes());
    buf.extend_from_slice(&16u16.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, buf).expect("write wav");
}
