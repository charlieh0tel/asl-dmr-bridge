//! Voice PTT state machine.
//!
//! Owns mutable call state, the shared vocoder, the outbound channels,
//! and the cancellation token.  The outer `voice_task` select loop
//! dispatches events (`on_dmrd`, `on_audio`, `on_timeout`,
//! `on_shutdown`) to methods here; each method fully owns its state
//! transitions.  Tests construct a `PttMachine` directly and drive
//! events without spinning up the full select loop.

use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use ambe::AmbeFrame;
use ambe::PcmFrame;
use ambe::Vocoder;
use ambe::VocoderError;
use dmr_events::CallMetadata;
use dmr_events::CallsignLookup;
use dmr_events::MetaEvent;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::info;
use tracing::warn;

use crate::audio::AudioFrame;
use crate::bptc::build_data_burst;
use crate::bptc::build_voice_lc;
use crate::dmrd::CallType;
use crate::dmrd::Dmrd;
use crate::dmrd::FrameType;
use crate::embedded_lc::build_fragments;
use crate::embedded_lc::lcss_for_fragment;
use crate::frame::assemble_burst;
use crate::frame::extract_ambe;
use crate::sync::BS_DATA_SYNC;
use crate::sync::BS_VOICE_SYNC;
use crate::sync::build_emb_section;
use crate::sync::build_null_emb;
use crate::talker_alias;

use super::ControlEvent;
use super::DATA_TYPE_VOICE_HEADER;
use super::DATA_TYPE_VOICE_TERMINATOR;
use super::Direction;
use super::FRAMES_PER_BURST;
use super::SILENCE;
use super::VoiceConfig;
use super::build_dmrd;
use super::check_voice_lc;
use super::make_unkey_frame;
use super::make_voice_frame;
use super::matches_config;
use super::new_stream_id;

/// Shared vocoder handle.
///
/// Only one caller (PttMachine's decode/encode methods, from a single
/// tokio task) ever acquires the lock, so contention is zero in
/// practice.  The `Arc<Mutex>` wrapping is nevertheless required for
/// two reasons:
///
/// 1. `spawn_blocking` needs a `'static + Send` closure; Arc gives us
///    shareable ownership we can move into the closure.
/// 2. If `cancel.cancelled()` wins the select in decode()/encode(),
///    we return Err and drop the JoinHandle, but the blocking task
///    keeps running to completion on its cloned Arc.  Without the
///    Mutex, the detached task and a fresh call could race.
///
/// `std::sync::Mutex` (not tokio's async Mutex) is correct here: the
/// lock is only held inside the spawn_blocking closure, never across
/// an `.await` point.
type SharedVocoder = Arc<Mutex<Box<dyn Vocoder>>>;

/// Which side of the vocoder lock holder panicked.  Used to pick
/// the correct `VocoderError` variant in `poisoned_err` without a
/// fragile string match.
#[derive(Debug, Clone, Copy)]
enum Stage {
    Encode,
    Decode,
}

/// Convert a poisoned-mutex lock error into a VocoderError.  A panic
/// inside decode/encode (e.g. mbelib FFI) poisons the Mutex; recovering
/// via `into_inner` would silently reuse potentially corrupt internal
/// state (C `MbeParms` structs, serial framing).  Surface it as an
/// error so the frame is dropped with a log entry, and subsequent
/// frames keep failing until the operator restarts the bridge.
fn poisoned_err(stage: Stage) -> VocoderError {
    let msg = format!("vocoder mutex poisoned (prior {stage:?} panic)");
    match stage {
        Stage::Encode => VocoderError::Encode(msg),
        Stage::Decode => VocoderError::Decode(msg),
    }
}

pub(crate) struct RxCall {
    pub(crate) stream_id: u32,
    src_id: u32,
    last_voice: Instant,
}

pub(crate) struct TxCall {
    pub(crate) stream_id: u32,
    dmrd_seq: u8,
    vseq: u8,
    pcm_buf: Vec<PcmFrame>,
    started: Instant,
    /// Pre-encoded embedded-LC fragment sets cycled across
    /// superframes.  At minimum one entry (the voice LC); when the
    /// configured callsign fits as a Talker Alias header, a second
    /// entry holds the TA fragments and the rotation alternates
    /// voice / TA per superframe.  Each fragment-set has 4 entries
    /// for voice bursts B-E (vseq 1..=4).  Burst F (vseq 5) uses
    /// LCSS=0 null EMB; burst A (vseq 0) carries the sync pattern.
    pub(crate) lc_rotation: Vec<[[u8; 4]; 4]>,
    /// Counts completed superframes since TX start (each superframe
    /// = 6 voice bursts).  Indexes `lc_rotation` so the consumed LC
    /// alternates round-robin; advances when vseq wraps from 5 to 0.
    pub(crate) superframe_idx: u32,
    /// When `Some`, an unkey was received but the call is being held
    /// open until this deadline so a quick re-key counts as the same
    /// call (configured via `[dmr].min_tx_hang`).  Cleared when fresh
    /// keyup-with-audio arrives; on expiry the terminator fires and
    /// state goes Idle.  `None` when min_tx_hang = 0 or no unkey
    /// pending.
    pub(crate) pending_terminate: Option<Instant>,
}

pub(crate) enum PttState {
    Idle,
    Rx(RxCall),
    RxHang(Instant),
    Tx(TxCall),
}

pub(crate) struct PttMachine {
    cfg: VoiceConfig,
    vocoder: SharedVocoder,
    audio_tx: mpsc::Sender<AudioFrame>,
    dmrd_voice_out: mpsc::Sender<Vec<u8>>,
    dmrd_control_out: mpsc::UnboundedSender<Vec<u8>>,
    /// Out-of-band call metadata events destined for USRP TEXT
    /// frames (the bridge layer encodes to JSON).  try_send'd
    /// without backpressure: dropping a metadata frame is preferable
    /// to stalling the voice path.
    metadata_tx: mpsc::Sender<MetaEvent>,
    /// Optional resolver from on-air DMR ID to (callsign, first-name).
    /// `None` skips enrichment; the JSON omits `call` / `name`.
    callsign_lookup: Option<CallsignLookup>,
    cancel: CancellationToken,
    pub(crate) state: PttState,
}

impl PttMachine {
    #[expect(
        clippy::too_many_arguments,
        reason = "PttMachine owns both bounded voice and unbounded control DMR outputs alongside the existing voice-task wiring."
    )]
    pub(crate) fn new(
        cfg: VoiceConfig,
        vocoder: Box<dyn Vocoder>,
        audio_tx: mpsc::Sender<AudioFrame>,
        dmrd_voice_out: mpsc::Sender<Vec<u8>>,
        dmrd_control_out: mpsc::UnboundedSender<Vec<u8>>,
        metadata_tx: mpsc::Sender<MetaEvent>,
        callsign_lookup: Option<CallsignLookup>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            cfg,
            vocoder: Arc::new(Mutex::new(vocoder)),
            audio_tx,
            dmrd_voice_out,
            dmrd_control_out,
            metadata_tx,
            callsign_lookup,
            cancel,
            state: PttState::Idle,
        }
    }

    /// Build + try_send a `MetaEvent::Call` for the given inbound
    /// DMRD packet.  Drops on full channel; metadata is best-effort
    /// and must never backpressure voice.
    fn emit_call_metadata(&self, pkt: &Dmrd) {
        // pkt fields come off the wire; if either ID fails range
        // validation just skip emission (metadata is best-effort).
        let Ok(dmr_id) = dmr_types::SubscriberId::try_from(pkt.src_id) else {
            debug!(src_id = pkt.src_id, "skipping metadata: invalid src_id");
            return;
        };
        let Ok(tg) = dmr_types::Talkgroup::try_from(pkt.dst_id) else {
            debug!(dst_id = pkt.dst_id, "skipping metadata: invalid dst_id");
            return;
        };
        let (call, name) = match self.callsign_lookup.as_ref().and_then(|f| f(pkt.src_id)) {
            Some((c, n)) => {
                let call = if c.is_empty() { None } else { Some(c) };
                let name = if n.is_empty() { None } else { Some(n) };
                (call, name)
            }
            None => (None, None),
        };
        let meta = CallMetadata {
            dmr_id,
            tg,
            slot: pkt.slot,
            cc: self.cfg.color_code,
            call,
            name,
        };
        let _ = self.metadata_tx.try_send(MetaEvent::Call(meta));
    }

    /// try_send a `MetaEvent::Clear` to clear the active-call
    /// metadata at end of call.  Drops on full channel.
    fn emit_clear_metadata(&self) {
        let _ = self.metadata_tx.try_send(MetaEvent::Clear);
    }

    /// `true` if the configured call_type is a group call.
    fn is_group_call(&self) -> bool {
        matches!(self.cfg.call_type, CallType::Group)
    }

    /// Take the current PTT state by value, leaving `Idle` in its
    /// place.  Replaces the `std::mem::replace(&mut self.state,
    /// PttState::Idle)` idiom at four call sites.
    fn take_state(&mut self) -> PttState {
        std::mem::replace(&mut self.state, PttState::Idle)
    }

    /// Deadline for the outer select-loop's sleep_until.  Idle uses a
    /// far-future sentinel since no timeout work is pending.  Tx with
    /// a pending terminate (mid min_tx_hang) returns the earlier of
    /// the hang expiry and the call's tx_timeout.
    pub(crate) fn deadline(&self) -> Instant {
        match &self.state {
            PttState::Rx(rx) => rx.last_voice + self.cfg.stream_timeout,
            PttState::RxHang(dl) => *dl,
            PttState::Tx(tx) => {
                let tx_timeout_dl = tx.started + self.cfg.tx_timeout;
                tx.pending_terminate
                    .map(|hang| hang.min(tx_timeout_dl))
                    .unwrap_or(tx_timeout_dl)
            }
            PttState::Idle => Instant::now() + Duration::from_secs(3600),
        }
    }

    // --- Vocoder (spawn_blocking + cancel race) ---

    async fn decode(&self, ambe: AmbeFrame) -> Result<PcmFrame, VocoderError> {
        let v = self.vocoder.clone();
        let handle = tokio::task::spawn_blocking(move || match v.lock() {
            Ok(mut guard) => guard.decode(&ambe),
            Err(_) => Err(poisoned_err(Stage::Decode)),
        });
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => Err(VocoderError::Decode("cancelled".into())),
            result = handle => result.map_err(|e| VocoderError::Decode(format!("vocoder task failed: {e}")))?,
        }
    }

    async fn encode(&self, pcm: PcmFrame) -> Result<AmbeFrame, VocoderError> {
        let v = self.vocoder.clone();
        let handle = tokio::task::spawn_blocking(move || match v.lock() {
            Ok(mut guard) => guard.encode(&pcm),
            Err(_) => Err(poisoned_err(Stage::Encode)),
        });
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => Err(VocoderError::Encode("cancelled".into())),
            result = handle => result.map_err(|e| VocoderError::Encode(format!("vocoder task failed: {e}")))?,
        }
    }

    // --- TX burst builders ---

    fn build_tx_header(&self, tx: &mut TxCall) -> Vec<u8> {
        let group = self.is_group_call();
        let lc = build_voice_lc(
            group,
            self.cfg.talkgroup.as_u32(),
            self.cfg.src_id.as_u32(),
            DATA_TYPE_VOICE_HEADER,
        );
        let burst = build_data_burst(
            &lc,
            DATA_TYPE_VOICE_HEADER,
            self.cfg.color_code.value(),
            &BS_DATA_SYNC,
        );
        let pkt = build_dmrd(
            tx.dmrd_seq,
            &self.cfg,
            tx.stream_id,
            FrameType::DataSync,
            DATA_TYPE_VOICE_HEADER,
            burst,
        );
        tx.dmrd_seq = tx.dmrd_seq.wrapping_add(1);
        pkt.serialize().to_vec()
    }

    fn build_tx_terminator(&self, tx: &mut TxCall) -> Vec<u8> {
        let group = self.is_group_call();
        let lc = build_voice_lc(
            group,
            self.cfg.talkgroup.as_u32(),
            self.cfg.src_id.as_u32(),
            DATA_TYPE_VOICE_TERMINATOR,
        );
        let burst = build_data_burst(
            &lc,
            DATA_TYPE_VOICE_TERMINATOR,
            self.cfg.color_code.value(),
            &BS_DATA_SYNC,
        );
        let pkt = build_dmrd(
            tx.dmrd_seq,
            &self.cfg,
            tx.stream_id,
            FrameType::DataSync,
            DATA_TYPE_VOICE_TERMINATOR,
            burst,
        );
        tx.dmrd_seq = tx.dmrd_seq.wrapping_add(1);
        pkt.serialize().to_vec()
    }

    async fn build_tx_voice(
        &self,
        pcm: &[PcmFrame; FRAMES_PER_BURST],
        tx: &mut TxCall,
    ) -> Option<Vec<u8>> {
        let mut ambe = [ambe::AmbeFrame::default(); FRAMES_PER_BURST];
        for (i, frame) in pcm.iter().enumerate() {
            match self.encode(*frame).await {
                Ok(encoded) => ambe[i] = encoded,
                Err(e) => {
                    warn!(vseq = tx.vseq, sub = i, "encode error: {e}");
                    return None;
                }
            }
        }
        // Burst A (vseq=0): voice sync pattern.
        // Bursts B-E (vseq 1..=4): embedded LC fragments 0..3 with
        // LCSS 1/3/3/2 per ETSI TS 102 361-1.  Fragment set is
        // chosen from lc_rotation by superframe index, so multi-LC
        // setups (voice + TA) alternate per superframe.
        // Burst F (vseq=5): null EMB (LCSS=0, RC slot unused).
        let sync = match tx.vseq {
            0 => BS_VOICE_SYNC,
            n @ 1..=4 => {
                let fragment_idx = (n - 1) as usize;
                let lc_idx = (tx.superframe_idx as usize) % tx.lc_rotation.len();
                build_emb_section(
                    self.cfg.color_code.value(),
                    lcss_for_fragment(fragment_idx),
                    &tx.lc_rotation[lc_idx][fragment_idx],
                )
            }
            _ => build_null_emb(self.cfg.color_code.value()),
        };
        let burst = assemble_burst(&ambe, &sync);
        let ft = if tx.vseq == 0 {
            FrameType::VoiceSync
        } else {
            FrameType::Voice
        };
        let pkt = build_dmrd(tx.dmrd_seq, &self.cfg, tx.stream_id, ft, tx.vseq, burst);
        tx.dmrd_seq = tx.dmrd_seq.wrapping_add(1);
        let next_vseq = (tx.vseq + 1) % 6;
        if next_vseq == 0 {
            tx.superframe_idx = tx.superframe_idx.wrapping_add(1);
        }
        tx.vseq = next_vseq;
        Some(pkt.serialize().to_vec())
    }

    /// try_send + warn-on-full for the bounded DMRD voice channel.
    /// Never awaits: if `homebrew_client::run` is in reconnect backoff,
    /// nothing is draining `dmrd_voice_out` for up to BACKOFF_MAX (60 s),
    /// and `connect_once` drains stale packets on reconnect anyway.
    /// Blocking here would freeze the whole voice task -- both
    /// directions, since `voice_task` is single-threaded over its
    /// select loop.
    fn try_send_voice_dmrd(&self, pkt: Vec<u8>, kind: &'static str) {
        if self.dmrd_voice_out.try_send(pkt).is_err() {
            warn!(kind, "DMRD out channel full, dropping packet");
        }
    }

    /// Headers and terminators define call boundaries.  Queue them on
    /// a dedicated unbounded control path so they are never dropped by
    /// bursty voice traffic filling the bounded voice queue.
    fn send_control_dmrd(&self, pkt: Vec<u8>, kind: &'static str) {
        if self.dmrd_control_out.send(pkt).is_err() {
            warn!(kind, "DMRD control channel closed");
        }
    }

    async fn flush_tx(&self, tx: &mut TxCall) {
        if tx.pcm_buf.is_empty() {
            return;
        }
        tx.pcm_buf.resize(FRAMES_PER_BURST, SILENCE);
        let pcm: [PcmFrame; FRAMES_PER_BURST] = tx.pcm_buf[..FRAMES_PER_BURST]
            .try_into()
            .expect("sliced to FRAMES_PER_BURST");
        tx.pcm_buf.clear();
        if let Some(pkt) = self.build_tx_voice(&pcm, tx).await {
            self.try_send_voice_dmrd(pkt, "tx_flush_voice");
        }
    }

    // --- Event handlers ---

    pub(crate) async fn on_control(&mut self, event: ControlEvent) {
        match event {
            ControlEvent::NetworkReset => self.on_network_reset().await,
        }
    }

    async fn on_network_reset(&mut self) {
        let PttState::Tx(tx) = self.take_state() else {
            return;
        };
        warn!(
            stream_id = tx.stream_id,
            buffered_pcm = tx.pcm_buf.len(),
            "Homebrew session reset during TX; restarting call on next audio"
        );
    }

    pub(crate) async fn on_dmrd(&mut self, pkt: &Dmrd) {
        if self.cfg.gateway == Direction::FmToDmr {
            return;
        }
        if !matches_config(pkt, &self.cfg) {
            return;
        }
        if matches!(self.state, PttState::Tx(_)) {
            return;
        }

        match pkt.frame_type {
            FrameType::DataSync if pkt.dtype_vseq == DATA_TYPE_VOICE_HEADER => {
                // BM (and most DMR masters) sends 3 redundant voice
                // headers per call for FEC; suppress the duplicates so
                // the log + metadata emission fire once per stream.
                if matches!(&self.state, PttState::Rx(rx) if rx.stream_id == pkt.stream_id) {
                    return;
                }
                info!(
                    stream_id = pkt.stream_id,
                    src_id = pkt.src_id,
                    dst_id = pkt.dst_id,
                    "RX header"
                );
                check_voice_lc(pkt);
                self.emit_call_metadata(pkt);
                self.state = PttState::Rx(RxCall {
                    stream_id: pkt.stream_id,
                    src_id: pkt.src_id,
                    last_voice: Instant::now(),
                });
            }
            FrameType::DataSync if pkt.dtype_vseq == DATA_TYPE_VOICE_TERMINATOR => {
                // matches! ends the immutable borrow on self.state before
                // we need to mutate it; an if-let binding would linger.
                let same_stream =
                    matches!(&self.state, PttState::Rx(rx) if rx.stream_id == pkt.stream_id);
                if same_stream {
                    info!(stream_id = pkt.stream_id, "RX terminator");
                    check_voice_lc(pkt);
                    self.emit_clear_metadata();
                    let _ = self.audio_tx.send(make_unkey_frame()).await;
                    self.state = PttState::RxHang(Instant::now() + self.cfg.hang_time);
                }
            }
            FrameType::Voice | FrameType::VoiceSync => {
                // Update existing Rx or implicit-start from Idle/RxHang.
                // Tx already excluded above, so the else branch covers
                // only Idle/RxHang.  `emit_metadata` is deferred until
                // after the borrow on self.state ends.
                let mut emit_metadata = false;
                if let PttState::Rx(rx) = &mut self.state {
                    if rx.stream_id != pkt.stream_id {
                        info!(old = rx.stream_id, new = pkt.stream_id, "RX stream change");
                        emit_metadata = true;
                        rx.stream_id = pkt.stream_id;
                        rx.src_id = pkt.src_id;
                    }
                    rx.last_voice = Instant::now();
                } else {
                    debug!(stream_id = pkt.stream_id, "RX implicit start");
                    emit_metadata = true;
                    self.state = PttState::Rx(RxCall {
                        stream_id: pkt.stream_id,
                        src_id: pkt.src_id,
                        last_voice: Instant::now(),
                    });
                }
                if emit_metadata {
                    self.emit_call_metadata(pkt);
                }

                let ambe_frames = extract_ambe(&pkt.dmr_data);
                // Reserve FRAMES_PER_BURST slots up-front and drop
                // the whole burst if they don't all fit.  Per-frame
                // try_send would let frames 1-2 of a 3-frame burst
                // sneak in while frame 3 dropped, producing an
                // audible 20 ms gap mid-burst on the FM side.  An
                // all-or-nothing reservation keeps the burst whole
                // or skips it cleanly.
                let mut permits = match self.audio_tx.try_reserve_many(FRAMES_PER_BURST) {
                    Ok(p) => p,
                    Err(_) => {
                        warn!(
                            stream_id = pkt.stream_id,
                            "audio tx channel full, dropping voice burst"
                        );
                        return;
                    }
                };
                for (i, ambe) in ambe_frames.iter().enumerate() {
                    let permit = permits.next().expect("reserved FRAMES_PER_BURST permits");
                    match self.decode(*ambe).await {
                        Ok(pcm) => permit.send(make_voice_frame(pcm)),
                        Err(e) => {
                            warn!(stream_id = pkt.stream_id, sub = i, "decode error: {e}");
                            // permit drops, releasing its slot.
                        }
                    }
                }
            }
            _ => {}
        }
    }

    pub(crate) async fn on_audio(&mut self, frame: &AudioFrame) {
        if self.cfg.gateway == Direction::DmrToFm {
            return;
        }

        if !frame.keyup {
            // Unkey: if we were TX'ing, flush + terminator (or defer
            // the terminator if min_tx_hang is set, so a quick re-key
            // continues the same call).  Stray unkey while in
            // Rx/RxHang/Idle is a no-op and must NOT clobber state.
            if matches!(self.state, PttState::Tx(_)) {
                let PttState::Tx(mut tx) = self.take_state() else {
                    unreachable!("just matched Tx");
                };
                if tx.pending_terminate.is_some() {
                    // Already in hang from a previous unkey.  A
                    // second unkey is a no-op: don't reset the
                    // deadline (would extend calls indefinitely on
                    // unkey bursts) and don't fire the terminator
                    // (would defeat the hang).
                    self.state = PttState::Tx(tx);
                    return;
                }
                self.flush_tx(&mut tx).await;
                if self.cfg.min_tx_hang.is_zero() {
                    let term = self.build_tx_terminator(&mut tx);
                    info!(stream_id = tx.stream_id, "TX terminator");
                    self.send_control_dmrd(term, "tx_terminator");
                } else {
                    tx.pending_terminate = Some(Instant::now() + self.cfg.min_tx_hang);
                    debug!(stream_id = tx.stream_id, "TX hang start");
                    self.state = PttState::Tx(tx);
                }
            }
            return;
        }

        let Some(audio) = frame.samples.as_ref().copied() else {
            return;
        };

        if matches!(self.state, PttState::Rx(_) | PttState::RxHang(_)) {
            return;
        }

        if matches!(self.state, PttState::Idle) {
            // Compute embedded LC fragments from the 72-bit LC body
            // (PF+FLCO+FID+opts+dst+src, without RS parity).  The same
            // body is used for header/terminator BPTC (with RS parity)
            // and for embedded LC fragments (with 5-bit CRC instead).
            let group = self.is_group_call();
            let lc96 = build_voice_lc(
                group,
                self.cfg.talkgroup.as_u32(),
                self.cfg.src_id.as_u32(),
                DATA_TYPE_VOICE_HEADER,
            );
            let lc_body: [u8; 72] = lc96[..72]
                .try_into()
                .expect("lc96 is [u8; 96], lc96[..72] is len 72");
            let voice_fragments = build_fragments(&lc_body);

            // Build the LC rotation: voice LC first (so the receiving
            // radio identifies the call before the TA), then optional
            // talker-alias header.  TA disabled = single-entry vec,
            // strict voice-LC behavior.
            let mut lc_rotation = vec![voice_fragments];
            if let Some(ta_bits) = talker_alias::encode_ta_header_bits(&self.cfg.callsign) {
                lc_rotation.push(build_fragments(&ta_bits));
            }

            let mut tx = TxCall {
                stream_id: new_stream_id(),
                dmrd_seq: 0,
                vseq: 0,
                pcm_buf: Vec::with_capacity(FRAMES_PER_BURST),
                started: Instant::now(),
                lc_rotation,
                superframe_idx: 0,
                pending_terminate: None,
            };
            info!(stream_id = tx.stream_id, "TX header");
            let hdr = self.build_tx_header(&mut tx);
            self.send_control_dmrd(hdr, "tx_header");
            tx.pcm_buf.push(audio);
            self.state = PttState::Tx(tx);
            return;
        }

        // Must be Tx now.  mem::replace to sidestep the borrow of
        // self.state that would otherwise block calls to self methods.
        let PttState::Tx(mut tx) = self.take_state() else {
            unreachable!("state was checked above");
        };
        // Re-key during the min_tx_hang window: cancel the pending
        // terminator so this audio extends the same call.
        if tx.pending_terminate.take().is_some() {
            debug!(stream_id = tx.stream_id, "TX hang cancelled (re-key)");
        }
        tx.pcm_buf.push(audio);
        if tx.pcm_buf.len() >= FRAMES_PER_BURST {
            let pcm: [PcmFrame; FRAMES_PER_BURST] = tx.pcm_buf[..FRAMES_PER_BURST]
                .try_into()
                .expect("sliced to FRAMES_PER_BURST");
            tx.pcm_buf.clear();
            let vseq = tx.vseq;
            if let Some(pkt) = self.build_tx_voice(&pcm, &mut tx).await {
                debug!(stream_id = tx.stream_id, vseq, "TX voice");
                self.try_send_voice_dmrd(pkt, "tx_voice");
            }
        }
        self.state = PttState::Tx(tx);
    }

    pub(crate) async fn on_timeout(&mut self) {
        match self.take_state() {
            PttState::Rx(rx) => {
                warn!(stream_id = rx.stream_id, "RX stream timeout");
                self.emit_clear_metadata();
                let _ = self.audio_tx.send(make_unkey_frame()).await;
                self.state = PttState::RxHang(Instant::now() + self.cfg.hang_time);
            }
            PttState::RxHang(_) => {
                debug!("RX hang expired");
                // state already Idle from mem::replace.
            }
            PttState::Tx(mut tx) => {
                // Two timeout cases:
                //   1. min_tx_hang expired (pending_terminate hit) ->
                //      send terminator, normal end-of-call.
                //   2. tx_timeout (long stuck call) -> warn + same.
                // flush_tx runs unconditionally so any audio buffered
                // by an in-flight burst is preserved before the
                // terminator fires.  Empty pcm_buf is a no-op flush.
                let hang_expired = tx.pending_terminate.is_some_and(|dl| Instant::now() >= dl);
                if hang_expired {
                    info!(stream_id = tx.stream_id, "TX hang expired -> terminator");
                } else {
                    warn!(stream_id = tx.stream_id, "TX timeout");
                }
                self.flush_tx(&mut tx).await;
                let term = self.build_tx_terminator(&mut tx);
                self.send_control_dmrd(term, "tx_timeout_terminator");
            }
            PttState::Idle => {}
        }
    }

    pub(crate) async fn on_shutdown(&mut self) {
        match self.take_state() {
            PttState::Rx(_) => {
                self.emit_clear_metadata();
                let _ = self.audio_tx.send(make_unkey_frame()).await;
            }
            PttState::RxHang(_) => {
                // Clear was already emitted on the Rx -> RxHang
                // transition (terminator or stream timeout).  Just
                // make sure the FM peer ends up unkeyed.
                let _ = self.audio_tx.send(make_unkey_frame()).await;
            }
            PttState::Tx(mut tx) => {
                self.flush_tx(&mut tx).await;
                let term = self.build_tx_terminator(&mut tx);
                self.send_control_dmrd(term, "tx_shutdown_terminator");
            }
            PttState::Idle => {}
        }
    }
}
