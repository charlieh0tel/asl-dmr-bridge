//! Bridge between the USRP wire format (`usrp_wire`) and
//! `dmr_wire::audio::AudioFrame`.  rx_task strips the USRP-specific
//! fields (seq, talkgroup, FrameType) and forwards an AudioFrame to
//! `voice_task`; tx_task does the inverse, attaching wire fields and
//! pacing the voice frames out at 20 ms.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::sleep_until;
use tracing::debug;
use tracing::info;
use tracing::warn;

use dmr_events::MetaEvent;
use dmr_wire::audio::AudioFrame;
use usrp_wire::Frame;
use usrp_wire::FrameType;
use usrp_wire::PACKET_SIZE;
use usrp_wire::RECV_SLACK;
use usrp_wire::VOICE_FRAME_INTERVAL;

use dsp::agc::Agc;
use dsp::levels::LevelAccumulator;
use dsp::levels::fmt_dbfs;
use pcm_utils::wav::WavRecorder;

/// Receive USRP packets from the socket, strip the wire-only fields
/// (seq, talkgroup, frame_type), and forward the resulting
/// `AudioFrame` to `voice_task`.  Non-voice frame types (DTMF, text)
/// are dropped at this seam since the voice path doesn't consume them.
///
/// Only packets whose source address matches `remote` are accepted;
/// everything else is dropped with a warn log.  ASL3 is the sole
/// peer in this bridge -- accepting voice from arbitrary senders
/// would let a network neighbor inject audio onto the DMR side.
///
/// `agc`, when `Some`, processes voice samples in place per frame
/// (so the encoder sees the levelled PCM) and resets state on
/// unkey so each new talker starts neutral.  A per-call summary
/// is emitted as `call_agc dir=fm_to_dmr ...`.
pub(crate) async fn rx_task(
    socket: Arc<UdpSocket>,
    tx: mpsc::Sender<AudioFrame>,
    remote: SocketAddr,
    byte_swap: bool,
    mut agc: Option<Agc>,
    cancel: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let mut buf = [0u8; PACKET_SIZE + RECV_SLACK];
    let mut had_voice_in_call = false;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                finalize_fm_in_agc_call(agc.as_mut(), &mut had_voice_in_call);
                return Ok(());
            }
            result = socket.recv_from(&mut buf) => {
                let (len, addr) = match result {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("USRP rx: recv error: {e}");
                        continue;
                    }
                };
                if addr != remote {
                    warn!(%addr, %remote, "USRP rx: dropping packet from unexpected peer");
                    continue;
                }
                match Frame::parse(&buf[..len], byte_swap) {
                    Ok(frame) => {
                        debug!(seq = frame.seq, keyup = frame.keyup, "USRP rx");
                        if frame.frame_type != FrameType::Voice {
                            continue;
                        }
                        let mut samples = frame.audio;
                        if let Some(agc_state) = agc.as_mut() {
                            if frame.keyup {
                                if let Some(buf) = samples.as_mut() {
                                    agc_state.process(buf);
                                    had_voice_in_call = true;
                                }
                            } else {
                                finalize_fm_in_agc_call(Some(agc_state), &mut had_voice_in_call);
                            }
                        }
                        let audio = AudioFrame {
                            keyup: frame.keyup,
                            samples,
                            dmr_stream_id: None,
                        };
                        // try_send rather than send().await: backpressuring
                        // the recv loop would just push the drop down to
                        // the kernel UDP buffer where we couldn't see it.
                        // Better to drop visibly here.
                        match tx.try_send(audio) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => {
                                warn!("USRP rx: voice_task channel full, dropping frame");
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                finalize_fm_in_agc_call(agc.as_mut(), &mut had_voice_in_call);
                                return Ok(());
                            }
                        }
                    }
                    Err(e) => {
                        warn!("USRP rx: dropping malformed packet: {e}");
                    }
                }
            }
        }
    }
}

/// Read `AudioFrame`s from the channel and send them as USRP packets,
/// adding the wire-only fields (seq, talkgroup, FrameType) here.
///
/// Voice frames (keyup with audio) are paced at `VOICE_FRAME_INTERVAL`;
/// control frames (keyup transitions, unkey) fire immediately and
/// reset the pacing anchor so the next voice frame starts fresh.
/// `metadata_rx` carries out-of-band `MetaEvent`s emitted by the
/// voice task; `Call` is JSON-encoded and `Clear` becomes `"{}"`,
/// both sent as USRP TEXT (frame_type=2) packets at call boundaries.
/// `agc`, when `Some`, processes voice samples in place per frame
/// and resets state on unkey so each new talker starts neutral.
#[expect(
    clippy::too_many_arguments,
    reason = "tx_task is the bridge's USRP-out hub: socket + 2 channel ends + remote addr + tg + byte_swap + agc + record dir + cancel; refactor when there's a clear grouping, not preemptively."
)]
pub(crate) async fn tx_task(
    socket: Arc<UdpSocket>,
    mut rx: mpsc::Receiver<AudioFrame>,
    mut metadata_rx: mpsc::Receiver<MetaEvent>,
    remote: SocketAddr,
    talkgroup: u32,
    byte_swap: bool,
    mut agc: Option<Agc>,
    pcm_record_dir: Option<PathBuf>,
    cancel: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    // USRP wire sequence counter; the FM peer treats it as a per-
    // packet monotonic counter for loss detection.
    let mut seq: u32 = 0;
    // Scheduled emit time for the next voice frame.  None means the
    // next voice frame fires immediately (start of stream, or right
    // after a control frame).  Advanced by VOICE_FRAME_INTERVAL per
    // voice emit so pacing is absolute -- scheduler wake-up jitter
    // does not accumulate into drift.
    let mut next_voice_send: Option<Instant> = None;
    // Per-call accumulator + WAV on the post-AGC PCM (= what hits
    // the FM peer).  Resets and emits a summary on each unkey.
    let mut agc_out_levels = LevelAccumulator::default();
    let mut agc_out_recorder: Option<WavRecorder> = None;
    let mut current_dmr_stream_id: Option<u32> = None;
    let mut had_voice_in_call = false;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                finalize_agc_out_call(
                    &mut agc_out_levels,
                    &mut agc_out_recorder,
                    &mut current_dmr_stream_id,
                    &mut had_voice_in_call,
                    agc.as_mut(),
                );
                return Ok(());
            }
            event = metadata_rx.recv() => {
                let Some(event) = event else { return Ok(()) };
                let text = match event {
                    MetaEvent::Call(meta) => {
                        // Not PII: callsign + first name come from the
                        // public RadioID / DMR-MARC subscriber registry,
                        // and the call itself was just transmitted in
                        // clear over RF -- anyone within range heard it.
                        info!(
                            dmr_id = %meta.dmr_id,
                            tg = %meta.tg,
                            call = meta.call.as_deref().unwrap_or(""),
                            name = meta.name.as_deref().unwrap_or(""),
                            "metadata Call"
                        );
                        match serde_json::to_string(&meta) {
                            Ok(s) => s,
                            Err(e) => {
                                warn!("call metadata serialize: {e}");
                                continue;
                            }
                        }
                    }
                    MetaEvent::Clear => {
                        info!("metadata Clear");
                        "{}".to_string()
                    }
                };
                let buf = Frame::serialize_text(seq, &text);
                seq = seq.wrapping_add(1);
                debug!(seq, len = text.len(), "USRP tx text");
                if let Err(e) = socket.send_to(&buf, remote).await {
                    warn!("USRP tx: text send error: {e}");
                }
            }
            audio = rx.recv() => {
                let Some(audio) = audio else {
                    finalize_agc_out_call(
                        &mut agc_out_levels,
                        &mut agc_out_recorder,
                        &mut current_dmr_stream_id,
                        &mut had_voice_in_call,
                        agc.as_mut(),
                    );
                    return Ok(());
                };

                let is_voice = audio.keyup && audio.samples.is_some();
                if is_voice {
                    let deadline = next_voice_send.unwrap_or_else(Instant::now);
                    let now = Instant::now();
                    if now < deadline {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => {
                                finalize_agc_out_call(
                                    &mut agc_out_levels,
                                    &mut agc_out_recorder,
                                    &mut current_dmr_stream_id,
                                    &mut had_voice_in_call,
                                    agc.as_mut(),
                                );
                                return Ok(());
                            }
                            _ = sleep_until(deadline) => {}
                        }
                        // Anchor on deadline so jitter doesn't drift.
                        next_voice_send = Some(deadline + VOICE_FRAME_INTERVAL);
                    } else {
                        // Past deadline: anchor on now so we wait a
                        // full interval instead of firing back-to-back
                        // as catch-up.
                        next_voice_send = Some(now + VOICE_FRAME_INTERVAL);
                    }
                } else {
                    next_voice_send = None;
                }

                // Reset AGC on unkey so each new call starts neutral.
                // The per-call summary is drained later in
                // finalize_agc_out_call (reset() leaves it intact).
                let mut samples = audio.samples;
                if let Some(agc_state) = agc.as_mut() {
                    if !audio.keyup {
                        agc_state.reset();
                    } else if let Some(buf) = samples.as_mut() {
                        agc_state.process(buf);
                    }
                }

                // Per-call AGC-out diagnostics: open a WAV on the
                // first voice frame of a call, accumulate levels +
                // record while keyed, summarize and close on unkey.
                if audio.keyup {
                    if let Some(buf) = samples.as_ref() {
                        if !had_voice_in_call {
                            current_dmr_stream_id = audio.dmr_stream_id;
                            agc_out_recorder = pcm_utils::wav::open_call_recorder(
                                pcm_record_dir.as_deref(),
                                "dmr_to_fm_agc_out",
                                audio.dmr_stream_id.unwrap_or(0),
                            );
                        }
                        agc_out_levels.add_frame(buf);
                        if let Some(rec) = agc_out_recorder.as_mut()
                            && let Err(e) = rec.write(buf)
                        {
                            warn!("agc_out wav write: {e}");
                            agc_out_recorder = None;
                        }
                        had_voice_in_call = true;
                    }
                } else {
                    finalize_agc_out_call(
                        &mut agc_out_levels,
                        &mut agc_out_recorder,
                        &mut current_dmr_stream_id,
                        &mut had_voice_in_call,
                        agc.as_mut(),
                    );
                }

                let frame = Frame {
                    seq,
                    keyup: audio.keyup,
                    talkgroup,
                    frame_type: FrameType::Voice,
                    audio: samples,
                    text: None,
                };
                seq = seq.wrapping_add(1);
                let buf = frame.serialize(byte_swap);
                debug!(seq = frame.seq, keyup = frame.keyup, "USRP tx");
                if let Err(e) = socket.send_to(&buf, remote).await {
                    warn!("USRP tx: send error: {e}");
                }
            }
        }
    }
}

/// Emit the per-call agc_out level + AGC behavior summaries and
/// reset the in-flight recorder/accumulator state.  No-op when no
/// voice was observed in the current call (both unkey paths and
/// idle close-outs).
fn finalize_agc_out_call(
    levels: &mut LevelAccumulator,
    recorder: &mut Option<WavRecorder>,
    dmr_stream_id: &mut Option<u32>,
    had_voice: &mut bool,
    agc: Option<&mut Agc>,
) {
    if !*had_voice {
        return;
    }
    let sid = dmr_stream_id.unwrap_or(0);
    let (peak, rms, voiced_rms) = levels.summary();
    info!(
        "call_levels dir=dmr_to_fm point=agc_out stream_id={sid} \
         peak={} rms={} voiced_rms={}",
        fmt_dbfs(peak),
        fmt_dbfs(rms),
        fmt_dbfs(voiced_rms),
    );
    if let Some(agc) = agc {
        emit_call_agc("dmr_to_fm", Some(sid), agc);
    }
    *levels = LevelAccumulator::default();
    *recorder = None;
    *dmr_stream_id = None;
    *had_voice = false;
}

/// Drain the FM->DMR AGC summary and emit `call_agc dir=fm_to_dmr ...`.
/// No-op when no voice was observed in the current call.  Resets the
/// AGC envelope so the next talker starts neutral.
fn finalize_fm_in_agc_call(agc: Option<&mut Agc>, had_voice: &mut bool) {
    if !*had_voice {
        return;
    }
    if let Some(agc) = agc {
        emit_call_agc("fm_to_dmr", None, agc);
        agc.reset();
    }
    *had_voice = false;
}

/// Drain a per-call `AgcSummary` and emit `call_agc dir=<dir> ...` at
/// INFO.  `stream_id` is `Some` on directions where the bridge knows
/// the DMR stream id (DMR->FM); `None` on FM->DMR ingress, where
/// stream id is assigned later by the voice task.
fn emit_call_agc(dir: &str, stream_id: Option<u32>, agc: &mut Agc) {
    let s = agc.take_summary();
    if s.samples == 0 {
        return;
    }
    let frozen_pct = (s.frozen_samples as f64 * 100.0) / s.samples as f64;
    let limited_pct = (s.limited_samples as f64 * 100.0) / s.samples as f64;
    let stream_id = match stream_id {
        Some(sid) => format!(" stream_id={sid}"),
        None => String::new(),
    };
    // gr_min reports the deepest gain reduction the limiter
    // applied during the call.  None -> limiter never engaged ->
    // log "0.0 dB" (no reduction).  clipped is samples where the
    // post-limiter safety clamp fired; > 0 means the limiter let
    // a peak through and is the definitive failure indicator.
    info!(
        "call_agc dir={dir}{stream_id} samples={} \
         frozen={}/{} ({:.1}%) gain_min={} gain_mean={} \
         gain_max={} peak_in={} peak_out={} \
         limited={}/{} ({:.1}%) gr_min={} clipped={}",
        s.samples,
        s.frozen_samples,
        s.samples,
        frozen_pct,
        fmt_db(s.gain_min.unwrap_or(1.0)),
        fmt_db(s.gain_mean()),
        fmt_db(s.gain_max.unwrap_or(1.0)),
        fmt_dbfs(linear_to_dbfs(f64::from(s.peak_in))),
        fmt_dbfs(linear_to_dbfs(f64::from(s.peak_out))),
        s.limited_samples,
        s.samples,
        limited_pct,
        fmt_db(s.gr_min.unwrap_or(1.0)),
        s.clipped_samples,
    );
}

/// Format a linear gain factor as a dB string with one decimal.
fn fmt_db(linear: f32) -> String {
    if linear <= 0.0 {
        "-inf dB".to_string()
    } else {
        format!("{:.1} dB", 20.0 * linear.log10())
    }
}

/// Linear amplitude in [0, 1] -> dBFS.  Zero maps to -inf so
/// `fmt_dbfs` renders silence as "-infdBFS".
fn linear_to_dbfs(linear: f64) -> f64 {
    if linear > 0.0 {
        20.0 * linear.log10()
    } else {
        f64::NEG_INFINITY
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use usrp_wire::VOICE_SAMPLES;

    use super::*;

    fn make_voice_frame() -> Frame {
        let mut audio = [0i16; VOICE_SAMPLES];
        for (i, sample) in audio.iter_mut().enumerate() {
            *sample = i as i16 * 100;
        }
        Frame {
            seq: 42,
            keyup: true,
            talkgroup: 2,
            frame_type: FrameType::Voice,
            audio: Some(audio),
            text: None,
        }
    }

    #[tokio::test]
    async fn rx_task_drops_when_channel_full() {
        // Full voice channel must drop and keep looping, never block
        // or exit.
        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let listen_addr = socket.local_addr().unwrap();
        let (tx, mut rx) = mpsc::channel::<AudioFrame>(1);
        let cancel = tokio_util::sync::CancellationToken::new();

        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender_addr = sender.local_addr().unwrap();
        let task = tokio::spawn(rx_task(
            socket.clone(),
            tx,
            sender_addr,
            false,
            None,
            cancel.clone(),
        ));

        let buf = make_voice_frame().serialize(false);
        for _ in 0..4 {
            sender.send_to(&buf, listen_addr).await.unwrap();
        }

        tokio::time::sleep(Duration::from_millis(50)).await;

        let mut drained = 0;
        while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(10), rx.recv()).await {
            drained += 1;
        }
        assert!(
            (1..=4).contains(&drained),
            "expected >=1 frame drained, got {drained}"
        );

        cancel.cancel();
        task.await.unwrap().unwrap();
    }
}
