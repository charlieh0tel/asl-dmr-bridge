//! Generalized cross-backend encode/decode loopback.  Reads an
//! 8 kHz mono i16 WAV, encodes every frame with one backend,
//! decodes every frame with another, writes the result to a WAV.
//!
//! Backend specs:
//!   dynarmic                       (in-process, JIT-emulated)
//!   neural:/path/to/model.onnx     (in-process, encode-only)
//!   thumbdv:/dev/ttyUSB0           (USB AMBE-3000R hardware)
//!   udp:host:port                  (remote AMBEServer-compatible)
//!
//! Usage:
//!   cargo run -p ambe --features dynarmic,thumbdv,neural \
//!     --example cross_loopback -- \
//!     --encode <spec> --decode <spec> \
//!     [--input <in.wav>] [--output <out.wav>]
//!
//! Defaults: input ambe/tests/fixtures/voice.wav, output
//! /tmp/cross_loopback_<encode>_<decode>.wav.

use ambe::Vocoder;
use dmr_types::AmbeFrame;
use dmr_types::PCM_SAMPLES;
use dmr_types::PcmFrame;
use hound::SampleFormat;
use hound::WavReader;
use hound::WavSpec;
use hound::WavWriter;
use std::path::Path;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut encode_spec: Option<String> = None;
    let mut decode_spec: Option<String> = None;
    let mut input = format!("{}/tests/fixtures/voice.wav", env!("CARGO_MANIFEST_DIR"));
    let mut output: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--encode" => encode_spec = Some(args.next().expect("--encode <spec>")),
            "--decode" => decode_spec = Some(args.next().expect("--decode <spec>")),
            "--input" => input = args.next().expect("--input <wav>"),
            "--output" => output = Some(args.next().expect("--output <wav>")),
            other => anyhow::bail!("unexpected arg: {other}"),
        }
    }
    let encode_spec = encode_spec.ok_or_else(|| anyhow::anyhow!("--encode required"))?;
    let decode_spec = decode_spec.ok_or_else(|| anyhow::anyhow!("--decode required"))?;
    let output = output.unwrap_or_else(|| {
        format!(
            "/tmp/cross_loopback_{}_{}.wav",
            sanitize_for_filename(&encode_spec),
            sanitize_for_filename(&decode_spec),
        )
    });

    let pcm = read_wav_8k_mono_i16(&input)?;
    let frames = pcm.len() / PCM_SAMPLES;
    eprintln!("input {input}: {} samples ({frames} frames)", pcm.len());

    // Open and run the encoder, then drop it before opening the
    // decoder.  Hardware backends (thumbdv) hold an exclusive serial
    // session per chip; sequential lifetimes let same-spec
    // encode/decode pairs share one chip.
    let ambe_stream: Vec<AmbeFrame> = {
        let mut encoder = open_backend(&encode_spec)?;
        encoder.reset();
        let mut stream = Vec::with_capacity(frames);
        for f in 0..frames {
            let mut frame: PcmFrame = [0i16; PCM_SAMPLES];
            frame.copy_from_slice(&pcm[f * PCM_SAMPLES..(f + 1) * PCM_SAMPLES]);
            stream.push(encoder.encode(&frame)?);
        }
        stream
    };
    eprintln!("{encode_spec} encoded {} frames", ambe_stream.len());

    let out: Vec<i16> = {
        let mut decoder = open_backend(&decode_spec)?;
        decoder.reset();
        let mut samples = Vec::with_capacity(frames * PCM_SAMPLES);
        for frame in &ambe_stream {
            samples.extend_from_slice(&decoder.decode(Some(frame))?);
        }
        samples
    };
    eprintln!("{decode_spec} decoded {} samples", out.len());

    write_wav_8k_mono_i16(&output, &out)?;
    eprintln!("wrote {output}");
    Ok(())
}

/// Open the backend identified by `spec`.  See module docs for the
/// recognized syntax.
fn open_backend(spec: &str) -> anyhow::Result<Box<dyn Vocoder>> {
    if spec == "dynarmic" {
        return Ok(ambe::open_dynarmic());
    }
    if let Some(path) = spec.strip_prefix("thumbdv:") {
        return Ok(ambe::open_thumbdv(path, None)?);
    }
    if let Some(path) = spec.strip_prefix("neural:") {
        return Ok(ambe::open_neural(Path::new(path))?);
    }
    if let Some(addr) = spec.strip_prefix("udp:") {
        let sock_addr: std::net::SocketAddr = addr
            .parse()
            .map_err(|e| anyhow::anyhow!("udp:{addr}: {e}"))?;
        return Ok(ambe::open_ambeserver(sock_addr)?);
    }
    anyhow::bail!("unknown backend spec: {spec:?}")
}

fn sanitize_for_filename(spec: &str) -> String {
    spec.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn read_wav_8k_mono_i16(path: &str) -> anyhow::Result<Vec<i16>> {
    let mut reader = WavReader::open(path).map_err(|e| anyhow::anyhow!("open {path}: {e}"))?;
    let spec = reader.spec();
    anyhow::ensure!(
        spec.channels == 1
            && spec.sample_rate == 8000
            && spec.bits_per_sample == 16
            && spec.sample_format == SampleFormat::Int,
        "{path}: expected 8 kHz mono i16 PCM, got {spec:?}",
    );
    Ok(reader.samples::<i16>().collect::<Result<Vec<_>, _>>()?)
}

fn write_wav_8k_mono_i16(path: &str, samples: &[i16]) -> anyhow::Result<()> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: 8000,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec)?;
    for &s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(())
}
