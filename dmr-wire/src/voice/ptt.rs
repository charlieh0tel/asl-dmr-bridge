//! Voice PTT state machine.
//!
//! Owns mutable call state, the shared vocoder, the outbound channels,
//! and the cancellation token.  The outer `voice_task` select loop
//! dispatches events (`on_dmrd`, `on_audio`, `on_timeout`,
//! `on_shutdown`) to methods here; each method fully owns its state
//! transitions.  Tests construct a `PttMachine` directly and drive
//! events without spinning up the full select loop.

use std::sync::Mutex;
use std::time::Duration;

use dsp::biquad::BiquadCascade;
use dsp::biquad::pre_encode_voice_8khz;

use ambe::Vocoder;
use ambe::VocoderError;
use dmr_events::CallDirection;
use dmr_events::CallMetadata;
use dmr_events::CallsignLookup;
use dmr_events::MetaEvent;
use dmr_events::StatsEvent;
use dmr_events::TerminationReason;
use dmr_types::AmbeFrame;
use dmr_types::PcmFrame;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::info;
use tracing::warn;

use crate::audio::AudioFrame;
use crate::bptc::build_data_burst;
use crate::bptc::build_voice_lc;
use crate::bptc::build_voice_lc_body;
use crate::dmrd::CallType;
use crate::dmrd::Dmrd;
use crate::dmrd::FrameType;
use crate::dmrd::PACKET_SIZE;
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
use super::PttDiagnostics;
use super::PttPolicy;
use super::SILENCE;
use super::VoiceConfig;
use super::build_dmrd;
use super::check_voice_lc;
use super::make_unkey_frame;
use super::make_voice_frame;
use super::matches_config;
use super::new_stream_id;

/// `std::sync::Mutex` (not tokio's async Mutex): the lock is held only
/// inside `block_in_place`, never across an `.await` point.
type SharedVocoder = Mutex<Box<dyn Vocoder>>;

/// Beyond this gap, frame-repeat compensation costs more wire
/// RTT than the burst budget allows and sounds broken past
/// ~200 ms anyway; treat as a stream boundary (one on_ptt_up
/// instead of per-frame compensation).
const GAP_BOUNDARY_PACKETS: u8 = 3;

/// A single encode or decode call that takes longer than this
/// will be logged at WARN level.  Even moderate overruns accumulate
/// into audible gaps because tx_task resets its pacing anchor on
/// each late frame.
const TRANSCODE_BUDGET: Duration = Duration::from_millis(20);

pub(crate) struct RxCall {
    pub(crate) stream_id: u32,
    src_id: dmr_types::SubscriberId,
    dst_id: u32,
    slot: dmr_types::Slot,
    started: Instant,
    last_voice: Instant,
    /// Last DMRD `seq` on a voice frame for this stream.  `None` until
    /// the first voice frame; thereafter, `Some(s)` so the next frame
    /// detects gaps via `pkt.seq.wrapping_sub(s + 1)`.  Wrapping deltas
    /// >= 128 treated as reorder/dup and ignored.
    last_seq: Option<u8>,
}

pub(crate) struct TxCall {
    pub(crate) stream_id: u32,
    dmrd_seq: u8,
    vseq: u8,
    pcm_buf: Vec<PcmFrame>,
    pub(crate) started: Instant,
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
}

pub(crate) enum PttState {
    Idle,
    Rx(RxCall),
    RxHang(Instant),
    Tx(TxCall),
}

/// Best-effort stats producer.  `None` channel means stats are
/// disabled (no allocation, no timing); `try_send` drops on full so
/// the voice path never backpressures on the stats consumer.
struct StatsEmitter {
    tx: Option<mpsc::Sender<StatsEvent>>,
}

impl StatsEmitter {
    fn new(tx: Option<mpsc::Sender<StatsEvent>>) -> Self {
        Self { tx }
    }

    fn voice_frame(&self, dir: CallDirection, transcode: Duration) {
        self.send(StatsEvent::VoiceFrame { dir, transcode });
    }

    fn frame_dropped(&self, dir: CallDirection) {
        self.send(StatsEvent::Drop { dir });
    }

    fn call_start(
        &self,
        dir: CallDirection,
        src_id: dmr_types::SubscriberId,
        dst_id: u32,
        slot: dmr_types::Slot,
    ) {
        self.send(StatsEvent::CallStart {
            dir,
            src_id,
            dst_id,
            slot,
        });
    }

    fn call_end(&self, dir: CallDirection, reason: TerminationReason) {
        self.send(StatsEvent::CallEnd { dir, reason });
    }

    fn send(&self, evt: StatsEvent) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(evt);
        }
    }
}

pub(crate) struct PttMachine {
    config: VoiceConfig,
    vocoder: SharedVocoder,
    audio_tx: mpsc::Sender<AudioFrame>,
    dmrd_voice_out: mpsc::Sender<[u8; PACKET_SIZE]>,
    dmrd_control_out: mpsc::UnboundedSender<[u8; PACKET_SIZE]>,
    /// Out-of-band call metadata events destined for USRP TEXT
    /// frames (the bridge layer encodes to JSON).  try_send'd
    /// without backpressure: dropping a metadata frame is preferable
    /// to stalling the voice path.
    metadata_tx: mpsc::Sender<MetaEvent>,
    /// Best-effort stats producer.  Wraps the optional sender +
    /// per-variant helpers so call sites don't repeat the
    /// `StatsEvent::Foo { dir, ... }` literal everywhere.
    stats: StatsEmitter,
    /// Optional resolver from on-air DMR ID to (callsign, first-name).
    /// `None` skips enrichment; the JSON omits `call` / `name`.
    callsign_lookup: Option<CallsignLookup>,
    cancel: CancellationToken,
    pub(crate) state: PttState,
    diag: super::diagnostics::CallDiagnostics,
    /// FM->DMR pre-encode filter; resets at TX call start.
    pre_encode_filter: Option<BiquadCascade<3>>,
}

impl PttMachine {
    #[expect(
        clippy::too_many_arguments,
        reason = "PttMachine owns both bounded voice and unbounded control DMR outputs alongside the existing voice-task wiring."
    )]
    pub(crate) fn new(
        config: VoiceConfig,
        diagnostics: PttDiagnostics,
        policy: PttPolicy,
        vocoder: Box<dyn Vocoder>,
        audio_tx: mpsc::Sender<AudioFrame>,
        dmrd_voice_out: mpsc::Sender<[u8; PACKET_SIZE]>,
        dmrd_control_out: mpsc::UnboundedSender<[u8; PACKET_SIZE]>,
        metadata_tx: mpsc::Sender<MetaEvent>,
        stats_tx: Option<mpsc::Sender<StatsEvent>>,
        callsign_lookup: Option<CallsignLookup>,
        cancel: CancellationToken,
    ) -> Self {
        let pre_encode_filter = policy.pre_encode_filter.then(pre_encode_voice_8khz);
        let diag = super::diagnostics::CallDiagnostics::new(diagnostics.pcm_record_dir);
        // Validate the configured callsign once: a non-empty value
        // that the TA encoder rejects (non-ASCII or >31 chars) means
        // outbound calls will silently emit no Talker Alias.  Warn at
        // construction so the operator notices instead of wondering
        // why their callsign never reaches the listener.
        if !config.callsign.is_empty()
            && talker_alias::encode_talker_alias_lcs(&config.callsign).is_empty()
        {
            warn!(
                callsign = %config.callsign,
                "TA disabled: callsign must be ASCII and <=31 chars",
            );
        }
        Self {
            config,
            vocoder: Mutex::new(vocoder),
            audio_tx,
            dmrd_voice_out,
            dmrd_control_out,
            metadata_tx,
            stats: StatsEmitter::new(stats_tx),
            callsign_lookup,
            cancel,
            state: PttState::Idle,
            diag,
            pre_encode_filter,
        }
    }

    // --- Diagnostic recording + level accumulator lifecycle.
    //     Strictly diagnostic concerns: vocoder reset stays in
    //     `on_ptt_up()` and is the caller's separate responsibility.

    fn on_tx_call_start(&mut self, stream_id: u32) {
        self.diag.on_tx_start(stream_id);
        if let Some(f) = self.pre_encode_filter.as_mut() {
            f.reset();
        }
    }

    fn on_tx_call_end(&mut self, stream_id: u32) {
        self.diag.on_tx_end(stream_id);
    }

    fn on_rx_call_start(&mut self, stream_id: u32) {
        self.diag.on_rx_start(stream_id);
    }

    fn on_rx_call_end(&mut self, stream_id: u32) {
        self.diag.on_rx_end(stream_id);
    }

    fn record_tx_frame(&mut self, pcm: &PcmFrame) {
        self.diag.record_tx_pcm(pcm);
    }

    /// Build + try_send a `MetaEvent::Call` for the given inbound
    /// DMRD packet.  Drops on full channel; metadata is best-effort
    /// and must never backpressure voice.
    fn emit_call_metadata(&self, pkt: &Dmrd) {
        // src_id is already a SubscriberId (validated at parse).  dst_id
        // is the wire u32; convert to Talkgroup for the metadata event,
        // skip emission if it's outside Talkgroup range (best-effort).
        let dmr_id = pkt.src_id;
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
            cc: self.config.color_code,
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
        matches!(self.config.call_type, CallType::Group)
    }

    /// Take the current PTT state by value, leaving `Idle` in its
    /// place.  Replaces the `std::mem::replace(&mut self.state,
    /// PttState::Idle)` idiom at four call sites.
    fn take_state(&mut self) -> PttState {
        std::mem::replace(&mut self.state, PttState::Idle)
    }

    /// Deadline for the outer select-loop's sleep_until.  Idle uses a
    /// far-future sentinel since no timeout work is pending.
    pub(crate) fn deadline(&self) -> Instant {
        match &self.state {
            PttState::Rx(rx) => rx.last_voice + self.config.stream_timeout,
            PttState::RxHang(dl) => *dl,
            PttState::Tx(tx) => tx.started + self.config.tx_timeout,
            PttState::Idle => Instant::now() + Duration::from_secs(3600),
        }
    }

    // --- Vocoder (spawn_blocking + cancel race) ---

    /// Vocoder panic / poisoned Mutex is unrecoverable: chip protocol
    /// state is mid-frame and decoder predictor history is corrupt.
    /// Cancel the task so `voice_task` exits via `on_shutdown` and
    /// peers see a clean call-end before the supervisor restarts us.
    fn fatal_vocoder(&self, what: &str) {
        if !self.cancel.is_cancelled() {
            tracing::error!("vocoder unusable: {what}; cancelling voice_task");
            self.cancel.cancel();
        }
    }

    /// `ambe = Some(frame)` decodes a real frame; `None` is a
    /// known-missing slot in the stream (RX seq gap), so the
    /// vocoder advances state and synthesizes compensation.
    async fn decode(&self, ambe: Option<AmbeFrame>) -> Result<PcmFrame, VocoderError> {
        if self.cancel.is_cancelled() {
            return Err(VocoderError::Decode("cancelled".into()));
        }
        let result = tokio::task::block_in_place(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.vocoder
                    .lock()
                    .expect("vocoder mutex poisoned")
                    .decode(ambe.as_ref())
            }))
        });
        match result {
            Ok(r) => {
                if self.cancel.is_cancelled() {
                    Err(VocoderError::Decode("cancelled".into()))
                } else {
                    r
                }
            }
            Err(_) => {
                self.fatal_vocoder("decode panic");
                Err(VocoderError::Decode("vocoder panic".into()))
            }
        }
    }

    async fn encode(&mut self, mut pcm: PcmFrame) -> Result<AmbeFrame, VocoderError> {
        if let Some(f) = self.pre_encode_filter.as_mut() {
            f.process_pcm(&mut pcm);
        }
        if self.cancel.is_cancelled() {
            return Err(VocoderError::Encode("cancelled".into()));
        }
        let result = tokio::task::block_in_place(|| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.vocoder
                    .lock()
                    .expect("vocoder mutex poisoned")
                    .encode(&pcm)
            }))
        });
        match result {
            Ok(r) => {
                if self.cancel.is_cancelled() {
                    Err(VocoderError::Encode("cancelled".into()))
                } else {
                    r
                }
            }
            Err(_) => {
                self.fatal_vocoder("encode panic");
                Err(VocoderError::Encode("vocoder panic".into()))
            }
        }
    }

    /// Hook fired at every PTT-up boundary -- both TX (new TxCall)
    /// and RX (new stream-id, including implicit start from
    /// Idle/RxHang).
    fn on_ptt_up(&self) {
        match self.vocoder.lock() {
            Ok(mut g) => g.reset(),
            Err(_) => self.fatal_vocoder("on_ptt_up: mutex poisoned"),
        }
    }

    // --- TX burst builders ---

    fn build_tx_header(&self, tx: &mut TxCall) -> [u8; PACKET_SIZE] {
        let group = self.is_group_call();
        let lc = build_voice_lc(
            group,
            self.config.talkgroup.as_u32(),
            self.config.src_id.as_u32(),
            DATA_TYPE_VOICE_HEADER,
        );
        let burst = build_data_burst(
            &lc,
            DATA_TYPE_VOICE_HEADER,
            self.config.color_code.value(),
            &BS_DATA_SYNC,
        );
        let pkt = build_dmrd(
            tx.dmrd_seq,
            &self.config,
            tx.stream_id,
            FrameType::DataSync,
            DATA_TYPE_VOICE_HEADER,
            burst,
        );
        tx.dmrd_seq = tx.dmrd_seq.wrapping_add(1);
        pkt.serialize()
    }

    fn build_tx_terminator(&self, tx: &mut TxCall) -> [u8; PACKET_SIZE] {
        let group = self.is_group_call();
        let lc = build_voice_lc(
            group,
            self.config.talkgroup.as_u32(),
            self.config.src_id.as_u32(),
            DATA_TYPE_VOICE_TERMINATOR,
        );
        let burst = build_data_burst(
            &lc,
            DATA_TYPE_VOICE_TERMINATOR,
            self.config.color_code.value(),
            &BS_DATA_SYNC,
        );
        let pkt = build_dmrd(
            tx.dmrd_seq,
            &self.config,
            tx.stream_id,
            FrameType::DataSync,
            DATA_TYPE_VOICE_TERMINATOR,
            burst,
        );
        tx.dmrd_seq = tx.dmrd_seq.wrapping_add(1);
        pkt.serialize()
    }

    async fn build_tx_voice(
        &mut self,
        pcm: &[PcmFrame; FRAMES_PER_BURST],
        tx: &mut TxCall,
    ) -> Option<([u8; PACKET_SIZE], [Duration; FRAMES_PER_BURST])> {
        let mut ambe = [AmbeFrame::default(); FRAMES_PER_BURST];
        let mut transcode_times = [Duration::ZERO; FRAMES_PER_BURST];
        for (i, frame) in pcm.iter().enumerate() {
            let t0 = Instant::now();
            match self.encode(*frame).await {
                Ok(encoded) => {
                    ambe[i] = encoded;
                    transcode_times[i] = t0.elapsed();
                    if transcode_times[i] > TRANSCODE_BUDGET {
                        warn!(
                            stream_id = tx.stream_id,
                            elapsed_ms = transcode_times[i].as_millis(),
                            "fm_to_dmr encode over 20ms budget"
                        );
                    }
                    self.diag.record_tx_ambe(&encoded);
                }
                Err(e) => {
                    warn!(vseq = tx.vseq, sub = i, "encode error: {e}");
                    return None;
                }
            }
        }
        Some((self.build_voice_burst(&ambe, tx), transcode_times))
    }

    /// Wrap 3 channel-coded AMBE frames in a DMR voice burst with the
    /// vseq-correct sync/EMB section, build the DMRD packet, and
    /// advance `tx`'s vseq + dmrd_seq + superframe_idx.  Burst A
    /// (vseq=0) carries `BS_VOICE_SYNC`; bursts B-E (vseq 1..=4) carry
    /// embedded-LC fragments 0..3 with LCSS 1/3/3/2 per ETSI
    /// TS 102 361-1; burst F (vseq=5) carries null EMB.
    fn build_voice_burst(
        &self,
        ambe: &[AmbeFrame; FRAMES_PER_BURST],
        tx: &mut TxCall,
    ) -> [u8; PACKET_SIZE] {
        let sync = match tx.vseq {
            0 => BS_VOICE_SYNC,
            n @ 1..=4 => {
                let fragment_idx = (n - 1) as usize;
                let lc_idx = (tx.superframe_idx as usize) % tx.lc_rotation.len();
                build_emb_section(
                    self.config.color_code.value(),
                    lcss_for_fragment(fragment_idx),
                    &tx.lc_rotation[lc_idx][fragment_idx],
                )
            }
            _ => build_null_emb(self.config.color_code.value()),
        };
        let burst = assemble_burst(ambe, &sync);
        let ft = if tx.vseq == 0 {
            FrameType::VoiceSync
        } else {
            FrameType::Voice
        };
        let pkt = build_dmrd(tx.dmrd_seq, &self.config, tx.stream_id, ft, tx.vseq, burst);
        tx.dmrd_seq = tx.dmrd_seq.wrapping_add(1);
        let next_vseq = (tx.vseq + 1) % 6;
        if next_vseq == 0 {
            tx.superframe_idx = tx.superframe_idx.wrapping_add(1);
        }
        tx.vseq = next_vseq;
        pkt.serialize()
    }

    /// try_send + warn-on-full for the bounded DMRD voice channel.
    /// Never awaits: if `homebrew_client::run` is in reconnect backoff,
    /// nothing is draining `dmrd_voice_out` for up to BACKOFF_MAX (60 s),
    /// and `connect_once` drains stale packets on reconnect anyway.
    /// Blocking here would freeze the whole voice task -- both
    /// directions, since `voice_task` is single-threaded over its
    /// select loop.
    ///
    /// Folds in stats emission so frame counts stay in lockstep with
    /// what reaches the wire: VoiceFrame on success, Drop on full.
    fn try_send_voice_dmrd(
        &self,
        pkt: [u8; PACKET_SIZE],
        transcode_times: [Duration; FRAMES_PER_BURST],
        kind: &'static str,
    ) {
        if self.dmrd_voice_out.try_send(pkt).is_ok() {
            for t in transcode_times {
                self.stats.voice_frame(CallDirection::FmToDmr, t);
            }
        } else {
            warn!(kind, "DMRD out channel full, dropping packet");
            for _ in 0..FRAMES_PER_BURST {
                self.stats.frame_dropped(CallDirection::FmToDmr);
            }
        }
    }

    /// Emit FRAMES_PER_BURST Drop events for an encode failure (whole
    /// burst lost; per-frame transcode latencies are not recorded for
    /// the partial-success prefix because the listener never hears it).
    fn drop_burst_fm_to_dmr(&self) {
        for _ in 0..FRAMES_PER_BURST {
            self.stats.frame_dropped(CallDirection::FmToDmr);
        }
    }

    /// Headers and terminators define call boundaries.  Queue them on
    /// a dedicated unbounded control path so they are never dropped by
    /// bursty voice traffic filling the bounded voice queue.
    fn send_control_dmrd(&self, pkt: [u8; PACKET_SIZE], kind: &'static str) {
        if self.dmrd_control_out.send(pkt).is_err() {
            warn!(kind, "DMRD control channel closed");
        }
    }

    async fn flush_tx(&mut self, tx: &mut TxCall) {
        if tx.pcm_buf.is_empty() {
            return;
        }
        tx.pcm_buf.resize(FRAMES_PER_BURST, SILENCE);
        let pcm: [PcmFrame; FRAMES_PER_BURST] = tx.pcm_buf[..FRAMES_PER_BURST]
            .try_into()
            .expect("sliced to FRAMES_PER_BURST");
        tx.pcm_buf.clear();
        for frame in &pcm {
            self.record_tx_frame(frame);
        }
        match self.build_tx_voice(&pcm, tx).await {
            Some((pkt, times)) => self.try_send_voice_dmrd(pkt, times, "tx_flush_voice"),
            None => self.drop_burst_fm_to_dmr(),
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
        self.stats
            .call_end(CallDirection::FmToDmr, TerminationReason::NetworkReset);
        self.on_tx_call_end(tx.stream_id);
    }

    pub(crate) async fn on_dmrd(&mut self, pkt: &Dmrd) {
        if self.config.gateway == Direction::FmToDmr {
            return;
        }
        if !matches_config(pkt, &self.config) {
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
                    src_id = pkt.src_id.as_u32(),
                    dst_id = pkt.dst_id,
                    "RX header"
                );
                check_voice_lc(pkt);
                self.emit_call_metadata(pkt);
                let now = Instant::now();
                self.stats
                    .call_start(CallDirection::DmrToFm, pkt.src_id, pkt.dst_id, pkt.slot);
                self.on_ptt_up();
                self.on_rx_call_start(pkt.stream_id);
                self.state = PttState::Rx(RxCall {
                    stream_id: pkt.stream_id,
                    src_id: pkt.src_id,
                    dst_id: pkt.dst_id,
                    slot: pkt.slot,
                    started: now,
                    last_voice: now,
                    last_seq: None,
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
                    self.stats
                        .call_end(CallDirection::DmrToFm, TerminationReason::Normal);
                    self.on_rx_call_end(pkt.stream_id);
                    let _ = self
                        .audio_tx
                        .send(make_unkey_frame(Some(pkt.stream_id)))
                        .await;
                    self.state = PttState::RxHang(Instant::now() + self.config.hang_time);
                }
            }
            FrameType::Voice | FrameType::VoiceSync => {
                // Update existing Rx or implicit-start from Idle/RxHang.
                // Tx already excluded above, so the else branch covers
                // only Idle/RxHang.  Emission of the call-boundary
                // events is deferred until after the borrow on
                // self.state ends.
                let mut emit_metadata = false;
                let mut emit_call_start = false;
                let mut prior_call_end: Option<u32> = None;
                let mut seq_gap: u8 = 0;
                let now = Instant::now();
                if let PttState::Rx(rx) = &mut self.state {
                    if rx.stream_id != pkt.stream_id {
                        info!(old = rx.stream_id, new = pkt.stream_id, "RX stream change");
                        emit_metadata = true;
                        emit_call_start = true;
                        prior_call_end = Some(rx.stream_id);
                        rx.stream_id = pkt.stream_id;
                        rx.src_id = pkt.src_id;
                        rx.dst_id = pkt.dst_id;
                        rx.slot = pkt.slot;
                        rx.started = now;
                        rx.last_seq = None;
                    } else if let Some(last) = rx.last_seq {
                        // Wrapping delta to expected (last+1).  0 = in
                        // order; 1..128 = gap; >=128 = reorder/dup, no
                        // count.  See RxCall.last_seq doc.
                        let delta = pkt.seq.wrapping_sub(last.wrapping_add(1));
                        if (1..128).contains(&delta) {
                            seq_gap = delta;
                        }
                    }
                    rx.last_seq = Some(pkt.seq);
                    rx.last_voice = now;
                } else {
                    debug!(stream_id = pkt.stream_id, "RX implicit start");
                    emit_metadata = true;
                    emit_call_start = true;
                    self.state = PttState::Rx(RxCall {
                        stream_id: pkt.stream_id,
                        src_id: pkt.src_id,
                        dst_id: pkt.dst_id,
                        slot: pkt.slot,
                        started: now,
                        last_voice: now,
                        last_seq: Some(pkt.seq),
                    });
                }
                if let Some(prior_sid) = prior_call_end {
                    self.stats
                        .call_end(CallDirection::DmrToFm, TerminationReason::Normal);
                    self.on_rx_call_end(prior_sid);
                    let _ = self.audio_tx.send(make_unkey_frame(Some(prior_sid))).await;
                }
                if emit_call_start {
                    self.on_ptt_up();
                    self.on_rx_call_start(pkt.stream_id);
                    self.stats
                        .call_start(CallDirection::DmrToFm, pkt.src_id, pkt.dst_id, pkt.slot);
                }
                if emit_metadata {
                    self.emit_call_metadata(pkt);
                }
                for _ in 0..seq_gap {
                    self.stats.frame_dropped(CallDirection::DmrToFm);
                }

                let ambe_frames = extract_ambe(&pkt.dmr_data);
                for frame in &ambe_frames {
                    self.diag.record_rx_ambe(frame);
                }
                let gap_frames = if seq_gap > GAP_BOUNDARY_PACKETS {
                    self.on_ptt_up();
                    0
                } else {
                    FRAMES_PER_BURST * seq_gap as usize
                };
                let total_frames = FRAMES_PER_BURST + gap_frames;
                // All-or-nothing reservation: per-frame try_send
                // would let some frames sneak in while later ones
                // dropped, producing an audible mid-burst gap.
                let mut permits = match self.audio_tx.try_reserve_many(total_frames) {
                    Ok(p) => p,
                    Err(_) => {
                        warn!(
                            stream_id = pkt.stream_id,
                            "audio tx channel full, dropping voice burst"
                        );
                        for _ in 0..FRAMES_PER_BURST {
                            self.stats.frame_dropped(CallDirection::DmrToFm);
                        }
                        return;
                    }
                };
                // Take recorder + levels out of self so the per-
                // frame work doesn't need `&mut self` while
                // `permits` holds a shared borrow on self.audio_tx.
                let mut rx_rec = self.diag.rx_recorder.take();
                let mut rx_acc = std::mem::take(&mut self.diag.rx_levels);
                for _ in 0..gap_frames {
                    let permit = permits.next().expect("reserved gap-fill permit");
                    let t0 = Instant::now();
                    match self.decode(None).await {
                        Ok(pcm) => {
                            let elapsed = t0.elapsed();
                            if elapsed > TRANSCODE_BUDGET {
                                warn!(
                                    stream_id = pkt.stream_id,
                                    elapsed_ms = elapsed.as_millis(),
                                    "dmr_to_fm gap decode over 20ms budget"
                                );
                            }
                            super::diagnostics::record_pcm(&mut rx_rec, &mut rx_acc, &pcm, "rx");
                            permit.send(make_voice_frame(pcm, pkt.stream_id));
                        }
                        Err(e) => {
                            warn!(stream_id = pkt.stream_id, "lost-frame decode error: {e}");
                            // permit drops, releasing its slot.
                        }
                    }
                }
                for (i, ambe) in ambe_frames.iter().enumerate() {
                    let permit = permits.next().expect("reserved voice-burst permit");
                    let t0 = Instant::now();
                    match self.decode(Some(*ambe)).await {
                        Ok(pcm) => {
                            let elapsed = t0.elapsed();
                            if elapsed > TRANSCODE_BUDGET {
                                warn!(
                                    stream_id = pkt.stream_id,
                                    elapsed_ms = elapsed.as_millis(),
                                    "dmr_to_fm decode over 20ms budget"
                                );
                            }
                            self.stats.voice_frame(CallDirection::DmrToFm, elapsed);
                            super::diagnostics::record_pcm(&mut rx_rec, &mut rx_acc, &pcm, "rx");
                            permit.send(make_voice_frame(pcm, pkt.stream_id));
                        }
                        Err(e) => {
                            warn!(stream_id = pkt.stream_id, sub = i, "decode error: {e}");
                            self.stats.frame_dropped(CallDirection::DmrToFm);
                            // permit drops, releasing its slot.
                        }
                    }
                }
                self.diag.rx_recorder = rx_rec;
                self.diag.rx_levels = rx_acc;
            }
            _ => {}
        }
    }

    pub(crate) async fn on_audio(&mut self, frame: &AudioFrame) {
        if self.config.gateway == Direction::DmrToFm {
            return;
        }

        if !frame.keyup {
            // Unkey: if we were TX'ing, flush + terminator.  Stray
            // unkey while in Rx/RxHang/Idle is a no-op and must NOT
            // clobber state.
            if matches!(self.state, PttState::Tx(_)) {
                let PttState::Tx(mut tx) = self.take_state() else {
                    unreachable!("just matched Tx");
                };
                self.flush_tx(&mut tx).await;
                let term = self.build_tx_terminator(&mut tx);
                info!(stream_id = tx.stream_id, "TX terminator");
                self.send_control_dmrd(term, "tx_terminator");
                self.stats
                    .call_end(CallDirection::FmToDmr, TerminationReason::Normal);
                self.on_tx_call_end(tx.stream_id);
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
            // (PF+FLCO+FID+opts+dst+src, no RS parity -- embedded LCs
            // carry a 5-bit CRC instead).
            let group = self.is_group_call();
            let lc_body = build_voice_lc_body(
                group,
                self.config.talkgroup.as_u32(),
                self.config.src_id.as_u32(),
            );
            let voice_fragments = build_fragments(&lc_body);

            // Build the LC rotation: voice LC first (so the receiving
            // radio identifies the call before the TA), then optional
            // talker-alias LCs (header + 0..=3 blocks).  TA disabled
            // = single-entry vec, strict voice-LC behavior.
            let mut lc_rotation = vec![voice_fragments];
            for ta_bits in talker_alias::encode_talker_alias_lcs(&self.config.callsign) {
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
            };
            info!(stream_id = tx.stream_id, "TX header");
            self.on_ptt_up();
            self.on_tx_call_start(tx.stream_id);
            self.stats.call_start(
                CallDirection::FmToDmr,
                self.config.src_id,
                self.config.talkgroup.as_u32(),
                self.config.slot,
            );
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
        tx.pcm_buf.push(audio);
        if tx.pcm_buf.len() >= FRAMES_PER_BURST {
            let pcm: [PcmFrame; FRAMES_PER_BURST] = tx.pcm_buf[..FRAMES_PER_BURST]
                .try_into()
                .expect("sliced to FRAMES_PER_BURST");
            tx.pcm_buf.clear();
            for frame in &pcm {
                self.record_tx_frame(frame);
            }
            let vseq = tx.vseq;
            match self.build_tx_voice(&pcm, &mut tx).await {
                Some((pkt, times)) => {
                    debug!(stream_id = tx.stream_id, vseq, "TX voice");
                    self.try_send_voice_dmrd(pkt, times, "tx_voice");
                }
                None => self.drop_burst_fm_to_dmr(),
            }
        }
        self.state = PttState::Tx(tx);
    }

    pub(crate) async fn on_timeout(&mut self) {
        match self.take_state() {
            PttState::Rx(rx) => {
                warn!(stream_id = rx.stream_id, "RX stream timeout");
                self.emit_clear_metadata();
                self.stats
                    .call_end(CallDirection::DmrToFm, TerminationReason::StreamTimeout);
                self.on_rx_call_end(rx.stream_id);
                let _ = self
                    .audio_tx
                    .send(make_unkey_frame(Some(rx.stream_id)))
                    .await;
                self.state = PttState::RxHang(Instant::now() + self.config.hang_time);
            }
            PttState::RxHang(_) => {
                debug!("RX hang expired");
                // state already Idle from mem::replace.
            }
            PttState::Tx(mut tx) => {
                let now = Instant::now();
                if now >= tx.started + self.config.tx_timeout {
                    warn!(stream_id = tx.stream_id, "TX timeout");
                    self.flush_tx(&mut tx).await;
                    let term = self.build_tx_terminator(&mut tx);
                    self.send_control_dmrd(term, "tx_timeout_terminator");
                    self.stats
                        .call_end(CallDirection::FmToDmr, TerminationReason::TxTimeout);
                    self.on_tx_call_end(tx.stream_id);
                } else {
                    self.state = PttState::Tx(tx);
                }
            }
            PttState::Idle => {}
        }
    }

    pub(crate) async fn on_shutdown(&mut self) {
        match self.take_state() {
            PttState::Rx(rx) => {
                self.emit_clear_metadata();
                self.stats
                    .call_end(CallDirection::DmrToFm, TerminationReason::Shutdown);
                self.on_rx_call_end(rx.stream_id);
                let _ = self
                    .audio_tx
                    .send(make_unkey_frame(Some(rx.stream_id)))
                    .await;
            }
            PttState::RxHang(_) => {
                // Clear was already emitted on the Rx -> RxHang
                // transition (terminator or stream timeout).  Just
                // make sure the FM peer ends up unkeyed.
                let _ = self.audio_tx.send(make_unkey_frame(None)).await;
            }
            PttState::Tx(mut tx) => {
                self.flush_tx(&mut tx).await;
                let term = self.build_tx_terminator(&mut tx);
                self.send_control_dmrd(term, "tx_shutdown_terminator");
                self.stats
                    .call_end(CallDirection::FmToDmr, TerminationReason::Shutdown);
                self.on_tx_call_end(tx.stream_id);
            }
            PttState::Idle => {}
        }
    }
}
