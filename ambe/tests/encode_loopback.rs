//! Encode+decode loopback against a live AMBE-3000 chip via AMBEserver.
//!
//! Requires hardware running AMBEserver on a known port.  Set
//! AMBE_SERVER_ADDR=127.0.0.1:2460 and run with --ignored, single-
//! threaded (AMBEserver proxies only one client at a time):
//!
//!   AMBE_SERVER_ADDR=127.0.0.1:2460 \
//!     cargo test -p ambe --features mbelib --test encode_loopback \
//!       -- --ignored --test-threads=1
//!
//! Checks envelope / invariants, not golden PCM -- AMBE codec state
//! makes exact-byte comparison fragile.

use std::net::SocketAddr;

use ambe::AmbeFrame;
use ambe::PCM_SAMPLES;
use ambe::PcmFrame;
use ambe::open_ambeserver;

/// Frames of input signal.  20 * 20ms = 400ms.
const FRAMES: usize = 20;

/// Discard the first N decoded frames: AMBE codec state needs a few
/// frames to converge.
const WARMUP: usize = 5;

const TONE_FREQ: f32 = 1000.0;
const TONE_PEAK: f32 = 8000.0;
const SAMPLE_RATE: f32 = 8000.0;

fn server_addr() -> SocketAddr {
    std::env::var("AMBE_SERVER_ADDR")
        .expect("AMBE_SERVER_ADDR must be set")
        .parse()
        .expect("AMBE_SERVER_ADDR must be IP:PORT")
}

/// Generate `n` frames of a sine wave at `freq` Hz.  Amplitude peak.
fn tone(freq: f32, peak: f32, n: usize) -> Vec<PcmFrame> {
    let step = 1.0 / SAMPLE_RATE;
    let omega = 2.0 * std::f32::consts::PI * freq;
    let mut t = 0.0f32;
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let mut frame = [0i16; PCM_SAMPLES];
        for s in frame.iter_mut() {
            *s = ((omega * t).sin() * peak) as i16;
            t += step;
        }
        out.push(frame);
    }
    out
}

fn rms(frames: &[PcmFrame]) -> f64 {
    let mut sum_sq = 0.0f64;
    let mut n = 0u64;
    for f in frames {
        for &s in f {
            sum_sq += f64::from(s).powi(2);
            n += 1;
        }
    }
    if n == 0 {
        0.0
    } else {
        (sum_sq / n as f64).sqrt()
    }
}

fn peak_abs(frames: &[PcmFrame]) -> i32 {
    frames
        .iter()
        .flat_map(|f| f.iter())
        .map(|&s| i32::from(s).abs())
        .max()
        .unwrap_or(0)
}

fn assert_voice_shaped(frames: &[PcmFrame], label: &str) {
    let r = rms(frames);
    let p = peak_abs(frames);
    assert!(r > 200.0, "{label}: too quiet, rms={r:.1}");
    assert!(r < 30_000.0, "{label}: saturated, rms={r:.1}");
    assert!(p > 1000, "{label}: no significant peaks, max={p}");
}

#[test]
#[ignore = "requires AMBEserver daemon; run with AMBE_SERVER_ADDR and --ignored"]
fn chip_encode_decode_self() {
    let mut chip = open_ambeserver(server_addr(), None).expect("connect");
    let pcm = tone(TONE_FREQ, TONE_PEAK, FRAMES);
    let ambe: Vec<AmbeFrame> = pcm
        .iter()
        .map(|p| chip.encode(p).expect("encode"))
        .collect();
    let out: Vec<PcmFrame> = ambe
        .iter()
        .map(|a| chip.decode(a).expect("decode"))
        .collect();
    assert_voice_shaped(&out[WARMUP..], "chip->chip");
}

#[test]
#[cfg(feature = "mbelib")]
#[ignore = "requires AMBEserver daemon + mbelib; run with AMBE_SERVER_ADDR and --ignored"]
fn chip_encode_mbelib_decode() {
    let mut chip = open_ambeserver(server_addr(), None).expect("connect");
    let mut mb = ambe::open_mbelib();
    let pcm = tone(TONE_FREQ, TONE_PEAK, FRAMES);
    let ambe: Vec<AmbeFrame> = pcm
        .iter()
        .map(|p| chip.encode(p).expect("encode"))
        .collect();
    let out: Vec<PcmFrame> = ambe.iter().map(|a| mb.decode(a).expect("decode")).collect();
    assert_voice_shaped(&out[WARMUP..], "chip->mbelib");
}

/// Verify that setting a negative output gain attenuates the chip's
/// decoded PCM vs. setting a positive output gain for the same
/// input signal.  Proves the GAIN control packet actually reaches
/// the chip and changes behavior.
#[test]
#[ignore = "requires AMBEserver daemon; run with AMBE_SERVER_ADDR and --ignored"]
fn gain_affects_decoded_amplitude() {
    fn loopback_rms(gain: (i8, i8)) -> f64 {
        let mut chip = open_ambeserver(server_addr(), Some(gain)).expect("connect");
        let pcm = tone(TONE_FREQ, TONE_PEAK, FRAMES);
        let ambe: Vec<AmbeFrame> = pcm
            .iter()
            .map(|p| chip.encode(p).expect("encode"))
            .collect();
        let out: Vec<PcmFrame> = ambe
            .iter()
            .map(|a| chip.decode(a).expect("decode"))
            .collect();
        rms(&out[WARMUP..])
    }

    let quiet = loopback_rms((0, -12));
    let loud = loopback_rms((0, 12));
    assert!(
        loud > quiet * 2.0,
        "expected output gain to scale amplitude: loud={loud:.1}, quiet={quiet:.1}"
    );
}
