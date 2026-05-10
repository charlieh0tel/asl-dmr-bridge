//! Unified half-duplex voice task with PTT state machine.
//!
//! Handles both DMR->ASL3 (RX/decode) and ASL3->DMR (TX/encode)
//! with a single Vocoder instance.  PTT state determines direction;
//! only one can be active at a time.
//!
//! States: Idle -> Rx -> RxHang -> Idle, or Idle -> Tx -> Idle.
//! RX and RxHang block TX.  TX blocks RX.
//!
//! `PttMachine` and the per-state types live in the `ptt` submodule;
//! this file holds the public surface (`Direction`, `VoiceConfig`,
//! `voice_task`) plus the pure helpers that don't carry state.

use std::time::Duration;

use ambe::Vocoder;
use dmr_events::CallsignLookup;
use dmr_events::MetaEvent;
use dmr_events::StatsEvent;
use dmr_types::ColorCode;
use dmr_types::DmrId;
use dmr_types::PCM_SAMPLES;
use dmr_types::PcmFrame;
use dmr_types::Slot;
use dmr_types::SubscriberId;
use dmr_types::Talkgroup;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::warn;

use super::audio::AudioFrame;
use super::audio::VOICE_SAMPLES;
use super::bptc::decode_voice_lc_burst;
use super::dmrd::CallType;
use super::dmrd::DMR_DATA_SIZE;
use super::dmrd::Dmrd;
use super::dmrd::FrameType;
use super::dmrd::PACKET_SIZE;

mod diagnostics;
mod ptt;

const DATA_TYPE_VOICE_HEADER: u8 = 1;
const DATA_TYPE_VOICE_TERMINATOR: u8 = 2;
const FRAMES_PER_BURST: usize = 3;
const SILENCE: PcmFrame = [0i16; PCM_SAMPLES];

/// Out-of-band control messages for `voice_task`.
///
/// These are transport/lifecycle events rather than on-air audio or
/// DMR bursts, so they travel on a dedicated channel.  Keeping them
/// explicit avoids overloading `AudioFrame`/`Dmrd` with impossible
/// sentinel values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlEvent {
    /// The Homebrew session was (re)authenticated and any queued
    /// outbound DMRD from the prior session was discarded.  An active
    /// FM->DMR call must restart from `Idle` so the next keyed audio
    /// emits a fresh header on the new session instead of resuming
    /// mid-stream.
    NetworkReset,
}

// --- Pure helpers ---
//
// DMR-module types (`dmrd::Slot`, `dmrd::CallType`, `Direction`)
// mirror ETSI TS 102 361 wire shapes and are the only config types
// this module exposes.  The binary's `crate::config::GatewayMode`,
// `crate::types::TimeSlot`, and `crate::config::CallType` are
// application-layer aliases; translation happens at `VoiceConfig`
// construction in `main.rs`, not here.  This keeps the DMR layer
// free of application types so it can lift into a library crate.

/// Log-only validation of an inbound voice LC header or terminator.
/// Decodes the BPTC(196,96) matrix with Hamming correction, unmaskes
/// the RS(12,9) parity, and cross-checks the LC body's src/dst against
/// the DMRD header.  Never drops a frame -- just emits warnings if
/// something is off.
fn check_voice_lc(pkt: &Dmrd) {
    let Some(lc) = decode_voice_lc_burst(&pkt.dmr_data, pkt.dtype_vseq) else {
        warn!(stream_id = pkt.stream_id, "LC BPTC uncorrectable");
        return;
    };
    if lc.bptc_corrected_bits > 0 {
        debug!(
            stream_id = pkt.stream_id,
            bptc_corrected_bits = lc.bptc_corrected_bits,
            "LC BPTC bits corrected"
        );
    }
    if lc.rs_syndromes != [0, 0, 0] {
        if lc.rs_corrected {
            debug!(
                stream_id = pkt.stream_id,
                rs_syndromes = ?lc.rs_syndromes,
                "LC RS corrected single-byte error"
            );
        } else {
            warn!(
                stream_id = pkt.stream_id,
                rs_syndromes = ?lc.rs_syndromes,
                "LC RS uncorrectable (>= 2 byte errors)"
            );
        }
    }
    if lc.src_id != pkt.src_id.as_u32() || lc.dst_id != pkt.dst_id {
        warn!(
            stream_id = pkt.stream_id,
            lc_src = lc.src_id,
            lc_dst = lc.dst_id,
            hdr_src = pkt.src_id.as_u32(),
            hdr_dst = pkt.dst_id,
            "LC vs DMRD header mismatch"
        );
    }
}

fn matches_config(pkt: &Dmrd, config: &VoiceConfig) -> bool {
    // For group calls, inbound dst_id is the talkgroup we're listening
    // on.  For private calls, inbound dst_id is our own subscriber ID
    // (the target the remote peer addressed) -- matching against
    // config.talkgroup (the TX target) would drop every private reply.
    let dst_match = match config.call_type {
        CallType::Group => pkt.dst_id == config.talkgroup.as_u32(),
        CallType::Unit => pkt.dst_id == config.src_id.as_u32(),
    };
    dst_match && pkt.slot == config.slot && pkt.call_type == config.call_type
}

/// Generate a fresh 32-bit stream ID for an outgoing voice call.
/// One `getrandom(2)` syscall per call-key-up; collision probability
/// across overlapping streams is ~2^-32 which is fine for a stream
/// disambiguator (Brandmeister also tolerates collisions).
fn new_stream_id() -> u32 {
    let mut buf = [0u8; 4];
    getrandom::fill(&mut buf).expect("OS entropy source unavailable");
    u32::from_ne_bytes(buf)
}

// --- Audio frame builders ---

fn make_voice_frame(audio: [i16; VOICE_SAMPLES], dmr_stream_id: u32) -> AudioFrame {
    AudioFrame {
        keyup: true,
        samples: Some(audio),
        dmr_stream_id: Some(dmr_stream_id),
    }
}

fn make_unkey_frame(dmr_stream_id: Option<u32>) -> AudioFrame {
    AudioFrame {
        keyup: false,
        samples: None,
        dmr_stream_id,
    }
}

// --- DMRD builder ---

fn build_dmrd(
    seq: u8,
    config: &VoiceConfig,
    stream_id: u32,
    frame_type: FrameType,
    dtype_vseq: u8,
    dmr_data: [u8; DMR_DATA_SIZE],
) -> Dmrd {
    Dmrd {
        seq,
        src_id: config.src_id,
        dst_id: config.talkgroup.as_u32(),
        repeater_id: config.repeater_id,
        slot: config.slot,
        call_type: config.call_type,
        frame_type,
        dtype_vseq,
        stream_id,
        dmr_data,
    }
}

// --- Public config ---

/// Direction mode for the voice gateway.  Local to the DMR module so
/// the binary's `crate::config::GatewayMode` can be translated at the
/// `VoiceConfig` construction boundary and the DMR module stays free
/// of application types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Both directions: DMR <-> FM.
    Both,
    /// DMR -> FM only.
    DmrToFm,
    /// FM -> DMR only.
    FmToDmr,
}

/// Self-contained configuration for the voice task.  Owns primitives
/// and dmr-wire types only; transport-specific config (USRP local
/// addresses, BM credentials, etc.) belongs to the calling binary
/// and is translated to a `VoiceConfig` at construction time.
#[derive(Debug, Clone)]
pub struct VoiceConfig {
    pub gateway: Direction,
    pub slot: Slot,
    pub talkgroup: Talkgroup,
    pub call_type: CallType,
    pub hang_time: Duration,
    pub stream_timeout: Duration,
    pub tx_timeout: Duration,
    /// Homebrew-protocol repeater identity, used in the DMRD
    /// `repeater_id` field.
    pub repeater_id: DmrId,
    /// On-air DMR subscriber ID used in the DMRD `src_id` field and
    /// the embedded LC.  Must be a separately registered subscriber
    /// ID -- the 32-bit hotspot `repeater_id` would alias onto an
    /// unrelated subscriber if reused here.
    pub src_id: SubscriberId,
    pub color_code: ColorCode,
    /// Operator callsign / alias emitted as Talker Alias on
    /// alternate superframes during outbound DMR calls.  Up to 31
    /// ASCII chars: 7 fit in the TA Header (FLCO=4), the remainder
    /// stream across up to 3 TA Blocks (FLCO=5/6/7) one per
    /// superframe.  Empty / non-ASCII / over-31 disables TA
    /// emission and the voice LC is sent every superframe.
    pub callsign: String,
}

/// Per-call diagnostic capture knobs for `PttMachine`.  Pure
/// observation; no effect on wire output.
#[derive(Debug, Clone, Default)]
pub struct PttDiagnostics {
    /// When `Some`, write per-call WAV files (8 kHz mono i16 LE) for
    /// encoder input on TX calls and pre-AGC decoder output on RX
    /// calls.
    pub pcm_record_dir: Option<std::path::PathBuf>,
}

/// Audio-policy knobs for `PttMachine`.  Affect what hits the wire.
#[derive(Debug, Clone, Copy, Default)]
pub struct PttPolicy {
    /// Apply the FM->DMR pre-encode voice-band filter; resets on TX
    /// call start.  Backend-agnostic.
    pub pre_encode_filter: bool,
}

// --- Main task ---

/// Infallible by design: all exit paths are graceful (cancel, channel
/// close).  Per-frame errors (decode, send) are logged and the loop
/// continues.  Kept as an ordinary `async fn` returning `()` so the
/// `dmr` module has no dependency on `anyhow`; main.rs wraps it for
/// `try_join!`.
#[expect(
    clippy::too_many_arguments,
    reason = "voice_task is the bridge's central wiring point: 4 channel ends + optional callsign lookup + vocoder + config + cancel; refactoring into a struct adds indirection without simplifying any single call site."
)]
pub async fn voice_task(
    mut dmrd_rx: mpsc::Receiver<Dmrd>,
    mut audio_rx: mpsc::Receiver<AudioFrame>,
    mut control_rx: mpsc::Receiver<ControlEvent>,
    audio_tx: mpsc::Sender<AudioFrame>,
    dmrd_voice_out: mpsc::Sender<[u8; PACKET_SIZE]>,
    dmrd_control_out: mpsc::UnboundedSender<[u8; PACKET_SIZE]>,
    metadata_tx: mpsc::Sender<MetaEvent>,
    stats_tx: Option<mpsc::Sender<StatsEvent>>,
    callsign_lookup: Option<CallsignLookup>,
    vocoder: Box<dyn Vocoder>,
    config: VoiceConfig,
    diagnostics: PttDiagnostics,
    policy: PttPolicy,
    cancel: CancellationToken,
) {
    let mut m = ptt::PttMachine::new(
        config,
        diagnostics,
        policy,
        vocoder,
        audio_tx,
        dmrd_voice_out,
        dmrd_control_out,
        metadata_tx,
        stats_tx,
        callsign_lookup,
        cancel.clone(),
    );
    // Every exit path (cancel or upstream channel close) must run
    // on_shutdown so an in-flight call ends cleanly: terminator on the
    // DMR side, unkey + Clear metadata on the FM side.  A bare `return`
    // here would strand peers mid-call -- the FM repeater stays keyed,
    // and BM never sees the LC terminator.
    loop {
        let deadline = m.deadline();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            event = control_rx.recv() => {
                let Some(event) = event else { break };
                m.on_control(event).await;
            }
            _ = tokio::time::sleep_until(deadline) => m.on_timeout().await,
            pkt = dmrd_rx.recv() => {
                let Some(pkt) = pkt else { break };
                m.on_dmrd(&pkt).await;
            }
            frame = audio_rx.recv() => {
                let Some(frame) = frame else { break };
                m.on_audio(&frame).await;
            }
        }
    }
    m.on_shutdown().await;
}

#[cfg(test)]
mod tests;
