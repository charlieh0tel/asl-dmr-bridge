//! Capture (PCM, coded_72, raw_49) triples from a DVSI AMBE-3000R chip.
//!
//! Encodes the same PCM through the chip twice:
//!   - rate index 33 (DMR / P25 half-rate) -> 9-byte channel-coded
//!   - rate index 34 (raw 2450 voice, 0 FEC) -> 7-byte raw codec bits
//!
//! Output, alongside the input:
//!   <prefix>.pcm        copy of the input stream
//!   <prefix>.coded72    concatenated 9-byte channel-coded frames
//!   <prefix>.raw49      concatenated 7-byte raw codec frames
//!
//! Pick a transport:
//!
//!   # via an ambeserver (default; can share the chip)
//!   cargo run -p ambe --example dv3000_capture -- \
//!       --ambeserver 127.0.0.1:2460 input.pcm output_prefix
//!
//!   # direct serial (needs the thumbdv feature; takes exclusive chip)
//!   cargo run -p ambe --features thumbdv --example dv3000_capture -- \
//!       --serial /dev/ttyUSB0 input.pcm output_prefix

use std::env;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use ambe::chip::AmbeServerClient;
use ambe::chip::ChipClient;

const PCM_SAMPLES: usize = 160;
const PCM_FRAME_BYTES: usize = PCM_SAMPLES * 2;
const CODED_BYTES: usize = 9; // 72 bits
const RAW_BYTES: usize = 7; // 49 bits, padded to ceil(49/8)

/// Rate index 33: DMR / P25 half-rate, 2450 voice + 1150 FEC.
const RATEP_DMR: [u8; 12] = [
    0x04, 0x31, 0x07, 0x54, 0x24, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6F, 0x48,
];

/// Rate index 34: raw 2450 voice, 0 FEC.
const RATEP_RAW: [u8; 12] = [
    0x04, 0x31, 0x07, 0x54, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x70, 0x31,
];

#[derive(Clone)]
enum Transport {
    Ambeserver(SocketAddr),
    #[cfg(feature = "thumbdv")]
    Serial {
        path: String,
        baud: Option<u32>,
    },
}

struct Args {
    transport: Transport,
    input: PathBuf,
    prefix: PathBuf,
}

fn usage() -> ! {
    eprintln!(
        "usage: dv3000_capture (--ambeserver host:port | --serial PATH [--baud N]) \\\n\
         \tinput.pcm output_prefix\n\
         \n\
         Encodes input PCM through the chip in DMR rate (index 33) and\n\
         raw rate (index 34); writes <prefix>.{{pcm,coded72,raw49}}."
    );
    std::process::exit(2)
}

fn parse_args() -> Args {
    let mut transport: Option<Transport> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--ambeserver" => {
                let v = iter.next().unwrap_or_else(|| usage());
                let addr: SocketAddr = v.parse().unwrap_or_else(|e| {
                    eprintln!("--ambeserver {v}: {e}");
                    usage();
                });
                transport = Some(Transport::Ambeserver(addr));
            }
            "--serial" => {
                let path = iter.next().unwrap_or_else(|| usage());
                #[cfg(feature = "thumbdv")]
                {
                    transport = Some(Transport::Serial { path, baud: None });
                }
                #[cfg(not(feature = "thumbdv"))]
                {
                    let _ = path;
                    eprintln!("--serial requires the thumbdv feature");
                    std::process::exit(2);
                }
            }
            #[cfg(feature = "thumbdv")]
            "--baud" => {
                let v = iter.next().unwrap_or_else(|| usage());
                let baud: u32 = v.parse().unwrap_or_else(|_| {
                    eprintln!("--baud must be an integer, got {v}");
                    usage();
                });
                if let Some(Transport::Serial {
                    baud: ref mut b, ..
                }) = transport
                {
                    *b = Some(baud);
                } else {
                    eprintln!("--baud applies only with --serial");
                    usage();
                }
            }
            "-h" | "--help" => usage(),
            _ if arg.starts_with("--") => {
                eprintln!("unexpected argument: {arg}");
                usage();
            }
            _ => positional.push(arg),
        }
    }
    let transport = transport.unwrap_or_else(|| {
        eprintln!("--ambeserver or --serial is required");
        usage();
    });
    if positional.len() != 2 {
        usage();
    }
    Args {
        transport,
        input: PathBuf::from(&positional[0]),
        prefix: PathBuf::from(&positional[1]),
    }
}

fn open_client(t: &Transport) -> Result<Box<dyn ChipClient>, String> {
    match t {
        Transport::Ambeserver(addr) => {
            let c = AmbeServerClient::connect(*addr)
                .map_err(|e| format!("connect ambeserver {addr}: {e}"))?;
            eprintln!("connected to ambeserver at {addr}");
            Ok(Box::new(c))
        }
        #[cfg(feature = "thumbdv")]
        Transport::Serial { path, baud } => {
            let c = ambe::chip::ThumbDvClient::open(path, *baud)
                .map_err(|e| format!("open serial {path}: {e}"))?;
            eprintln!("opened serial at {path}");
            Ok(Box::new(c))
        }
    }
}

fn read_pcm_frames(path: &Path) -> Result<Vec<[i16; PCM_SAMPLES]>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|mut f| f.read_to_end(&mut bytes))
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    if !bytes.len().is_multiple_of(PCM_FRAME_BYTES) {
        return Err(format!(
            "PCM file {} length {} is not a multiple of {} (one 20 ms frame)",
            path.display(),
            bytes.len(),
            PCM_FRAME_BYTES
        ));
    }
    let n = bytes.len() / PCM_FRAME_BYTES;
    let mut frames = Vec::with_capacity(n);
    for chunk in bytes.chunks_exact(PCM_FRAME_BYTES) {
        let mut frame = [0i16; PCM_SAMPLES];
        for (i, sample) in frame.iter_mut().enumerate() {
            *sample = i16::from_le_bytes([chunk[i * 2], chunk[i * 2 + 1]]);
        }
        frames.push(frame);
    }
    Ok(frames)
}

fn encode_pass(
    client: &mut dyn ChipClient,
    ratep: &[u8; 12],
    expected_bits: u8,
    expected_bytes: usize,
    frames: &[[i16; PCM_SAMPLES]],
    label: &str,
) -> Result<Vec<u8>, String> {
    eprintln!("pass: {label}");
    // Reset wipes codec state so each pass starts from a known
    // baseline.  Without this, frame 0 of pass 2 would inherit pass
    // 1's accumulated state and produce different bits than a fresh
    // start would.
    client.reset().map_err(|e| format!("{label}: reset: {e}"))?;
    client
        .set_ratep(ratep)
        .map_err(|e| format!("{label}: set_ratep: {e}"))?;
    let mut out = Vec::with_capacity(frames.len() * expected_bytes);
    for (i, frame) in frames.iter().enumerate() {
        let (bits, data) = client
            .encode_raw(frame)
            .map_err(|e| format!("{label}: frame {i}: {e}"))?;
        if bits != expected_bits || data.len() != expected_bytes {
            return Err(format!(
                "{label}: frame {i}: expected {expected_bits} bits / {expected_bytes} bytes, got {bits} bits / {} bytes",
                data.len()
            ));
        }
        out.extend_from_slice(&data);
        if (i + 1) % 200 == 0 || i + 1 == frames.len() {
            eprintln!("  encoded {} / {}", i + 1, frames.len());
        }
    }
    Ok(out)
}

fn run(args: &Args) -> Result<(), String> {
    let frames = read_pcm_frames(&args.input)?;
    eprintln!(
        "loaded {n} frames ({:.2} s) from {}",
        frames.len() as f32 * 0.020,
        args.input.display(),
        n = frames.len(),
    );

    let mut client = open_client(&args.transport)?;
    let coded = encode_pass(
        &mut *client,
        &RATEP_DMR,
        72,
        CODED_BYTES,
        &frames,
        "rate 33 (DMR)",
    )?;
    let raw = encode_pass(
        &mut *client,
        &RATEP_RAW,
        49,
        RAW_BYTES,
        &frames,
        "rate 34 (raw)",
    )?;

    let pcm_path = args.prefix.with_extension("pcm");
    let coded_path = args.prefix.with_extension("coded72");
    let raw_path = args.prefix.with_extension("raw49");

    let mut pcm_bytes = Vec::with_capacity(frames.len() * PCM_FRAME_BYTES);
    for frame in &frames {
        for sample in frame {
            pcm_bytes.extend_from_slice(&sample.to_le_bytes());
        }
    }
    File::create(&pcm_path)
        .and_then(|mut f| f.write_all(&pcm_bytes))
        .map_err(|e| format!("write {}: {e}", pcm_path.display()))?;
    File::create(&coded_path)
        .and_then(|mut f| f.write_all(&coded))
        .map_err(|e| format!("write {}: {e}", coded_path.display()))?;
    File::create(&raw_path)
        .and_then(|mut f| f.write_all(&raw))
        .map_err(|e| format!("write {}: {e}", raw_path.display()))?;
    eprintln!(
        "wrote {} / {} / {}",
        pcm_path.display(),
        coded_path.display(),
        raw_path.display(),
    );
    Ok(())
}

fn main() -> ExitCode {
    let args = parse_args();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
