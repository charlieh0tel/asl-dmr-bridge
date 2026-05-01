//! End-to-end TX path verification via the Brandmeister parrot
//! talkgroup (TG 9990).  Plays the role of ASL3: sends a known tone
//! into the bridge's USRP input, listens on the bridge's USRP output
//! for the parrot's echoed playback, saves both for offline review.
//!
//! Setup (one-time):
//!   1. Edit your bridge config:  `talkgroup = 9990`, `gateway = "both"`.
//!   2. Start the bridge: `cargo run --release -p asl-dmr-bridge -- config.toml`.
//!   3. Wait for `authenticated with master` in the bridge log.
//!
//! Run the test:
//!   cargo run --example parrot_test
//!
//! Optional args (positional):
//!   <bridge_in_addr>   default 127.0.0.1:34001  (bridge's USRP listen port)
//!   <bridge_out_addr>  default 127.0.0.1:34002  (bridge's USRP send target)
//!   <duration_seconds> default 3                (length of the test tone)
//!
//! Outputs (under /tmp):
//!   parrot_in.raw   the tone we sent (S16_LE 8 kHz mono)
//!   parrot_out.raw  the parrot's playback as received
//!
//! Listen with:  aplay -f S16_LE -r 8000 -c 1 /tmp/parrot_out.raw
//!
//! Pass: parrot_out.raw RMS roughly matches parrot_in.raw at the
//! same scale (AMBE+2 is lossy but voice-band tones survive
//! recognisably).  Fail: empty output, or RMS < 100, means the
//! TX path didn't reach BM or BM isn't echoing.

use std::env;
use std::fs::File;
use std::io::Read;
use std::io::Write;
use std::net::SocketAddr;
use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use usrp_wire::Frame;
use usrp_wire::FrameType;
use usrp_wire::PACKET_SIZE;
use usrp_wire::RECV_SLACK;
use usrp_wire::VOICE_FRAME_INTERVAL;
use usrp_wire::VOICE_SAMPLES;

const SAMPLE_RATE: f32 = 8000.0;

/// Tone amplitude.  Big enough that AMBE+2 doesn't squash to silence,
/// small enough to avoid clipping after FEC/codec effects.
const TONE_PEAK: i16 = 10_000;
const TONE_FREQ: f32 = 1000.0;

/// Maximum time to wait after unkey for parrot's first reply packet
/// to arrive.  If no UDP arrives within this window, declare the test
/// failed (BM dropped the call, peer mis-routed, etc.).
const PARROT_REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Once we've started receiving from the bridge, exit when this much
/// time has passed without a packet.  Robust to varying parrot reply
/// length without a fixed-window cutoff that risks truncating the
/// tail.  USRP voice frames pace at 20 ms; 1.5 s is well past any
/// jitter or hang-time gap.
const QUIET_TIMEOUT: Duration = Duration::from_millis(1500);

fn build_packet(seq: u32, keyup: bool, talkgroup: u32, audio: Option<&[i16]>) -> Vec<u8> {
    let mut samples = [0i16; VOICE_SAMPLES];
    if let Some(src) = audio {
        assert_eq!(src.len(), VOICE_SAMPLES, "voice frame must be 160 samples");
        samples.copy_from_slice(src);
    }
    Frame {
        seq,
        keyup,
        talkgroup,
        frame_type: FrameType::Voice,
        audio: audio.map(|_| samples),
        text: None,
    }
    .serialize(false)
}

fn main() -> std::io::Result<()> {
    let in_addr: SocketAddr = env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:34001".into())
        .parse()
        .expect("bridge_in_addr must be ip:port");
    let out_bind: SocketAddr = env::args()
        .nth(2)
        .unwrap_or_else(|| "127.0.0.1:34002".into())
        .parse()
        .expect("bridge_out_addr must be ip:port");
    let duration_secs: f32 = env::args()
        .nth(3)
        .map(|s| s.parse().expect("duration must be float seconds"))
        .unwrap_or(3.0);

    let input_path = env::var("PARROT_TEST_INPUT").ok();

    // Bind output FIRST so we don't miss early playback packets.
    let out_sock = UdpSocket::bind(out_bind)?;
    out_sock.set_read_timeout(Some(Duration::from_millis(50)))?;

    let in_sock = UdpSocket::bind("0.0.0.0:0")?;
    in_sock.connect(in_addr)?;

    // Build the input PCM either from a file (PARROT_TEST_INPUT env
    // var) or by synthesising a 1 kHz tone.  File input must be
    // S16_LE 8 kHz mono raw PCM; non-frame-sized lengths are padded
    // with zero samples so the sender always emits whole 20 ms frames.
    let tone: Vec<i16> = if let Some(path) = input_path.as_deref() {
        let samples = read_pcm_s16le(path)?;
        let secs = samples.len() as f32 / SAMPLE_RATE;
        let n_frames = samples.len().div_ceil(VOICE_SAMPLES);
        let mut padded = samples;
        padded.resize(n_frames * VOICE_SAMPLES, 0);
        eprintln!(
            "[parrot_test] sending {secs:.1}s recorded PCM from {path} to {in_addr} ({n_frames} frames)"
        );
        padded
    } else {
        let n_frames = (duration_secs * SAMPLE_RATE / VOICE_SAMPLES as f32).round() as usize;
        let total_samples = n_frames * VOICE_SAMPLES;
        eprintln!(
            "[parrot_test] sending {duration_secs}s {TONE_FREQ:.0} Hz tone to {in_addr} ({n_frames} frames)"
        );
        let omega = 2.0 * std::f32::consts::PI * TONE_FREQ;
        let step = 1.0 / SAMPLE_RATE;
        let mut t = 0.0f32;
        let mut tone = Vec::with_capacity(total_samples);
        for _ in 0..total_samples {
            tone.push(((omega * t).sin() * TONE_PEAK as f32) as i16);
            t += step;
        }
        tone
    };
    eprintln!(
        "[parrot_test] listening on {out_bind}, exit on {QUIET_TIMEOUT:?} silence (or no reply within {PARROT_REPLY_TIMEOUT:?})"
    );
    write_pcm("/tmp/parrot_in.raw", &tone)?;

    // Spawn a sender thread so we can listen concurrently.  The
    // `tx_done` flag MUST be set on every exit path -- including
    // a send error -- otherwise the listener's `tx_done_at` never
    // arms and the loop hangs until manual SIGINT.  RAII guard
    // does it without an `if let Err` ladder.
    let tx_done = Arc::new(AtomicBool::new(false));
    let in_sock_send = in_sock;
    let tone_send = tone.clone();
    let sender = {
        let tx_done = Arc::clone(&tx_done);
        thread::spawn(move || -> std::io::Result<()> {
            struct DoneGuard(Arc<AtomicBool>);
            impl Drop for DoneGuard {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Release);
                }
            }
            let _guard = DoneGuard(tx_done);

            let tg = 9990u32;
            let mut seq = 1u32;
            // Keyup (header only).
            in_sock_send.send(&build_packet(seq, true, tg, None))?;
            seq = seq.wrapping_add(1);
            let start = Instant::now();
            for (i, chunk) in tone_send.chunks_exact(VOICE_SAMPLES).enumerate() {
                let pkt = build_packet(seq, true, tg, Some(chunk));
                in_sock_send.send(&pkt)?;
                seq = seq.wrapping_add(1);
                // Pace at deadline-anchored 20 ms intervals so jitter
                // doesn't drift across the call.
                let deadline = start + VOICE_FRAME_INTERVAL * (i as u32 + 1);
                if let Some(sleep_for) = deadline.checked_duration_since(Instant::now()) {
                    thread::sleep(sleep_for);
                }
            }
            // Unkey.
            in_sock_send.send(&build_packet(seq, false, tg, None))?;
            Ok(())
        })
    };

    // Concurrent listener.  Two exit conditions:
    //   - last_recv set and idle longer than QUIET_TIMEOUT -> parrot
    //     finished, silence-based stop (avoids fixed-window truncation
    //     of the parrot tail).
    //   - tx_done set, no packet ever received, and PARROT_REPLY_TIMEOUT
    //     elapsed -> declare the call lost.
    let mut captured: Vec<i16> = Vec::new();
    let mut buf = [0u8; PACKET_SIZE + RECV_SLACK];
    let mut tx_done_at: Option<Instant> = None;
    let mut last_recv: Option<Instant> = None;
    loop {
        let now = Instant::now();
        if tx_done_at.is_none() && tx_done.load(Ordering::Acquire) {
            tx_done_at = Some(now);
        }
        if let Some(t) = last_recv
            && now.duration_since(t) > QUIET_TIMEOUT
        {
            break;
        }
        if last_recv.is_none()
            && let Some(t0) = tx_done_at
            && now.duration_since(t0) > PARROT_REPLY_TIMEOUT
        {
            break;
        }
        match out_sock.recv_from(&mut buf) {
            Ok((len, _)) => {
                last_recv = Some(Instant::now());
                match Frame::parse(&buf[..len], false) {
                    Ok(frame) if frame.keyup => {
                        if let Some(samples) = frame.audio {
                            captured.extend_from_slice(&samples);
                        }
                    }
                    _ => {} // unkey, malformed, or non-voice; ignore
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {} // tick
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}   // tick
            Err(e) => return Err(e),
        }
    }
    sender.join().expect("sender panicked")?;

    write_pcm("/tmp/parrot_out.raw", &captured)?;
    report("input ", &tone);
    report("output", &captured);
    if captured.is_empty() {
        eprintln!("[parrot_test] FAIL: nothing captured -- is the bridge connected to BM TG 9990?");
        std::process::exit(1);
    }
    eprintln!("[parrot_test] saved /tmp/parrot_in.raw and /tmp/parrot_out.raw");
    eprintln!("[parrot_test] listen: aplay -f S16_LE -r 8000 -c 1 /tmp/parrot_out.raw");
    Ok(())
}

fn write_pcm(path: &str, samples: &[i16]) -> std::io::Result<()> {
    let mut f = File::create(path)?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

fn read_pcm_s16le(path: &str) -> std::io::Result<Vec<i16>> {
    let mut f = File::open(path)?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    if bytes.len() % 2 != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("PCM file {path} has odd byte count ({} bytes)", bytes.len()),
        ));
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect())
}

fn report(label: &str, samples: &[i16]) {
    if samples.is_empty() {
        eprintln!("[parrot_test] {label}: empty");
        return;
    }
    let n = samples.len();
    let sum_sq: f64 = samples.iter().map(|&s| f64::from(s).powi(2)).sum();
    let rms = (sum_sq / n as f64).sqrt();
    let max = samples.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0);
    eprintln!(
        "[parrot_test] {label}: {:.1}s ({n} samples), rms={rms:.0}, max|s|={max}",
        n as f32 / SAMPLE_RATE
    );
}
