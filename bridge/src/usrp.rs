//! Bridge between the USRP wire format (`usrp_wire`) and
//! `dmr_wire::audio::AudioFrame`.  rx_task strips the USRP-specific
//! fields (seq, talkgroup, FrameType) and forwards an AudioFrame to
//! `voice_task`; tx_task does the inverse, attaching wire fields and
//! pacing the voice frames out at 20 ms.

use std::net::SocketAddr;
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

use crate::agc::Agc;

/// Receive USRP packets from the socket, strip the wire-only fields
/// (seq, talkgroup, frame_type), and forward the resulting
/// `AudioFrame` to `voice_task`.  Non-voice frame types (DTMF, text)
/// are dropped at this seam since the voice path doesn't consume them.
///
/// Only packets whose source address matches `remote` are accepted;
/// everything else is dropped with a warn log.  ASL3 is the sole
/// peer in this bridge -- accepting voice from arbitrary senders
/// would let a network neighbor inject audio onto the DMR side.
pub(crate) async fn rx_task(
    socket: Arc<UdpSocket>,
    tx: mpsc::Sender<AudioFrame>,
    remote: SocketAddr,
    byte_swap: bool,
    cancel: tokio_util::sync::CancellationToken,
) -> anyhow::Result<()> {
    let mut buf = [0u8; PACKET_SIZE + RECV_SLACK];
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
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
                        let audio = AudioFrame {
                            keyup: frame.keyup,
                            samples: frame.audio,
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
                            Err(mpsc::error::TrySendError::Closed(_)) => return Ok(()),
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
    reason = "tx_task is the bridge's USRP-out hub: socket + 2 channel ends + remote addr + tg + byte_swap + agc + cancel; refactor when there's a clear grouping, not preemptively."
)]
pub(crate) async fn tx_task(
    socket: Arc<UdpSocket>,
    mut rx: mpsc::Receiver<AudioFrame>,
    mut metadata_rx: mpsc::Receiver<MetaEvent>,
    remote: SocketAddr,
    talkgroup: u32,
    byte_swap: bool,
    mut agc: Option<Agc>,
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
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
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
                    other => {
                        debug!(?other, "unknown MetaEvent variant; skipping");
                        continue;
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
                let Some(audio) = audio else { return Ok(()) };

                let is_voice = audio.keyup && audio.samples.is_some();
                if is_voice {
                    let deadline = next_voice_send.unwrap_or_else(Instant::now);
                    let now = Instant::now();
                    if now < deadline {
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => return Ok(()),
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
                let mut samples = audio.samples;
                if let Some(agc_state) = agc.as_mut() {
                    if !audio.keyup {
                        agc_state.reset();
                    } else if let Some(buf) = samples.as_mut() {
                        agc_state.process(buf);
                    }
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
