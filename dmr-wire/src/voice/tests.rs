use std::sync::Arc;
use std::sync::Mutex;

use dsp::biquad::pre_encode_voice_8khz;

use super::ptt::PttMachine;
use super::ptt::PttState;
use super::*;
use crate::dmrd::PACKET_SIZE;
use dmr_types::AMBE_FRAME_SIZE;
use dmr_types::AmbeFrame;
use dmr_types::ColorCode;
use dmr_types::DmrId;
use dmr_types::PCM_SAMPLES;
use dmr_types::PcmFrame;
use dmr_types::SubscriberId;
use dmr_types::Talkgroup;
use tokio::time::Instant;

struct StubVocoder;

impl Vocoder for StubVocoder {
    fn encode(&mut self, _pcm: &PcmFrame) -> Result<AmbeFrame, ambe::VocoderError> {
        Ok([0xAA; AMBE_FRAME_SIZE])
    }
    fn decode(&mut self, _ambe: Option<&AmbeFrame>) -> Result<PcmFrame, ambe::VocoderError> {
        Ok([1000i16; VOICE_SAMPLES])
    }
    fn reset(&mut self) {}
    fn set_gain(&mut self, _in_db: dsp::dB, _out_db: dsp::dB) -> Result<(), ambe::VocoderError> {
        Ok(())
    }
}

/// Encodes by panicking, simulating an FFI-side panic in a real
/// vocoder.  Used to exercise the fatal-vocoder cancel path.
struct PanickingVocoder;

/// Records every PCM frame passed to `encode` for post-hoc inspection.
struct SpyVocoder {
    recorded: Arc<Mutex<Vec<PcmFrame>>>,
}

impl Vocoder for SpyVocoder {
    fn encode(&mut self, pcm: &PcmFrame) -> Result<AmbeFrame, ambe::VocoderError> {
        self.recorded.lock().unwrap().push(*pcm);
        Ok([0u8; AMBE_FRAME_SIZE])
    }
    fn decode(&mut self, _ambe: Option<&AmbeFrame>) -> Result<PcmFrame, ambe::VocoderError> {
        Ok([0i16; VOICE_SAMPLES])
    }
    fn reset(&mut self) {}
    fn set_gain(&mut self, _in_db: dsp::dB, _out_db: dsp::dB) -> Result<(), ambe::VocoderError> {
        Ok(())
    }
}

impl Vocoder for PanickingVocoder {
    fn encode(&mut self, _pcm: &PcmFrame) -> Result<AmbeFrame, ambe::VocoderError> {
        panic!("simulated vocoder panic")
    }
    fn decode(&mut self, _ambe: Option<&AmbeFrame>) -> Result<PcmFrame, ambe::VocoderError> {
        Ok([0i16; VOICE_SAMPLES])
    }
    fn reset(&mut self) {}
    fn set_gain(&mut self, _in_db: dsp::dB, _out_db: dsp::dB) -> Result<(), ambe::VocoderError> {
        Ok(())
    }
}

type TestMachine = (
    PttMachine,
    mpsc::Receiver<AudioFrame>,
    mpsc::Receiver<[u8; PACKET_SIZE]>,
    mpsc::UnboundedReceiver<[u8; PACKET_SIZE]>,
    mpsc::Receiver<MetaEvent>,
);

fn test_voice_config() -> VoiceConfig {
    VoiceConfig {
        gateway: Direction::Both,
        slot: Slot::One,
        talkgroup: Talkgroup::try_from(91).unwrap(),
        call_type: CallType::Group,
        hang_time: Duration::from_millis(500),
        stream_timeout: Duration::from_secs(10),
        tx_timeout: Duration::from_secs(180),
        repeater_id: DmrId::try_from(12345).unwrap(),
        src_id: SubscriberId::try_from(12345).unwrap(),
        color_code: ColorCode::try_from(1).unwrap(),
        callsign: String::new(),
    }
}

/// Standard test rig: a PttMachine plus the receive ends of all
/// outbound channels so tests can assert on emitted frames.  No
/// callsign lookup configured (the JSON omits `call`/`name`).
fn make_machine() -> TestMachine {
    make_machine_with_lookup(None)
}

fn make_machine_with_lookup(callsign_lookup: Option<CallsignLookup>) -> TestMachine {
    let cfg = test_voice_config();
    let (audio_tx, audio_rx) = mpsc::channel(16);
    let (dmrd_voice_out, dmrd_voice_rx) = mpsc::channel(16);
    let (dmrd_control_out, dmrd_control_rx) = mpsc::unbounded_channel();
    let (metadata_tx, metadata_rx) = mpsc::channel(16);
    let m = PttMachine::new(
        cfg,
        PttDiagnostics::default(),
        PttPolicy::default(),
        Box::new(StubVocoder),
        audio_tx,
        dmrd_voice_out,
        dmrd_control_out,
        metadata_tx,
        None,
        callsign_lookup,
        CancellationToken::new(),
    );
    (m, audio_rx, dmrd_voice_rx, dmrd_control_rx, metadata_rx)
}

fn voice_dmrd(stream_id: u32) -> Dmrd {
    Dmrd {
        seq: 0,
        src_id: dmr_types::SubscriberId::try_from(12345).unwrap(),
        dst_id: 91,
        repeater_id: dmr_types::DmrId::try_from(12345).unwrap(),
        slot: Slot::One,
        call_type: CallType::Group,
        frame_type: FrameType::Voice,
        dtype_vseq: 0,
        stream_id,
        dmr_data: [0u8; DMR_DATA_SIZE],
    }
}

fn header_dmrd(stream_id: u32) -> Dmrd {
    Dmrd {
        seq: 0,
        src_id: dmr_types::SubscriberId::try_from(12345).unwrap(),
        dst_id: 91,
        repeater_id: dmr_types::DmrId::try_from(12345).unwrap(),
        slot: Slot::One,
        call_type: CallType::Group,
        frame_type: FrameType::DataSync,
        dtype_vseq: DATA_TYPE_VOICE_HEADER,
        stream_id,
        dmr_data: [0u8; DMR_DATA_SIZE],
    }
}

fn terminator_dmrd(stream_id: u32) -> Dmrd {
    Dmrd {
        seq: 0,
        src_id: dmr_types::SubscriberId::try_from(12345).unwrap(),
        dst_id: 91,
        repeater_id: dmr_types::DmrId::try_from(12345).unwrap(),
        slot: Slot::One,
        call_type: CallType::Group,
        frame_type: FrameType::DataSync,
        dtype_vseq: DATA_TYPE_VOICE_TERMINATOR,
        stream_id,
        dmr_data: [0u8; DMR_DATA_SIZE],
    }
}

fn voice_audio() -> AudioFrame {
    AudioFrame {
        keyup: true,
        samples: Some([1000i16; PCM_SAMPLES]),
        dmr_stream_id: None,
    }
}

fn unkey_audio() -> AudioFrame {
    AudioFrame {
        keyup: false,
        samples: None,
        dmr_stream_id: None,
    }
}

// matches_config: group calls match on dst_id == talkgroup, but
// private calls invert -- inbound dst_id is *our* src_id, since the
// remote peer addressed us.  A naive dst_id == talkgroup check
// (where talkgroup means "TX target") drops every private reply.

#[test]
fn matches_config_group_dst_eq_talkgroup() {
    let mut cfg = test_voice_config();
    cfg.call_type = CallType::Group;
    cfg.talkgroup = Talkgroup::try_from(91).unwrap();
    let mut pkt = voice_dmrd(1);
    pkt.call_type = CallType::Group;
    pkt.dst_id = 91;
    assert!(matches_config(&pkt, &cfg));
}

#[test]
fn matches_config_private_dst_eq_our_src_id() {
    let mut cfg = test_voice_config();
    cfg.call_type = CallType::Unit;
    cfg.talkgroup = Talkgroup::try_from(9990).unwrap();
    cfg.src_id = SubscriberId::try_from(1234567).unwrap();
    let mut pkt = voice_dmrd(1);
    pkt.call_type = CallType::Unit;
    pkt.dst_id = 1234567;
    pkt.src_id = SubscriberId::try_from(9990).unwrap();
    assert!(matches_config(&pkt, &cfg));
}

#[test]
fn matches_config_private_dst_eq_talkgroup_rejects() {
    let mut cfg = test_voice_config();
    cfg.call_type = CallType::Unit;
    cfg.talkgroup = Talkgroup::try_from(9990).unwrap();
    cfg.src_id = SubscriberId::try_from(1234567).unwrap();
    let mut pkt = voice_dmrd(1);
    pkt.call_type = CallType::Unit;
    pkt.dst_id = 9990;
    assert!(!matches_config(&pkt, &cfg));
}

#[test]
fn matches_config_call_type_mismatch_rejects() {
    let mut cfg = test_voice_config();
    cfg.call_type = CallType::Group;
    cfg.talkgroup = Talkgroup::try_from(91).unwrap();
    let mut pkt = voice_dmrd(1);
    pkt.call_type = CallType::Unit;
    pkt.dst_id = 91;
    assert!(!matches_config(&pkt, &cfg));
}

#[tokio::test]
async fn rx_header_starts_rx() {
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_dmrd(&header_dmrd(0xAA)).await;
    assert!(matches!(m.state, PttState::Rx(ref rx) if rx.stream_id == 0xAA));
}

#[tokio::test]
async fn rx_voice_produces_usrp() {
    // Each of the 3 AMBE codewords in the voice burst feeds StubVocoder,
    // which emits the constant `[1000i16; VOICE_SAMPLES]`.  Asserting
    // that every voice frame carries that exact constant catches a
    // bug where the audio path passes the wrong samples through (e.g.
    // a vocoder-output mix-up, sample-buffer reuse, or a silent-frame
    // substitution).
    let (mut m, mut audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_dmrd(&header_dmrd(0xBB)).await;
    m.on_dmrd(&voice_dmrd(0xBB)).await;
    let mut count = 0;
    while let Ok(frame) = audio_rx.try_recv() {
        count += 1;
        let samples = frame.samples.expect("voice frame must carry PCM samples");
        assert!(
            samples.iter().all(|&s| s == 1000),
            "frame {count} samples don't match StubVocoder output: {:?}...",
            &samples[..4]
        );
    }
    assert_eq!(count, 3);
}

#[tokio::test]
async fn rx_terminator_enters_hang() {
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_dmrd(&header_dmrd(0xCC)).await;
    m.on_dmrd(&terminator_dmrd(0xCC)).await;
    assert!(matches!(m.state, PttState::RxHang(_)));
}

/// Helper: assert the next event is `Call(meta)` and return `meta`.
fn expect_call(rx: &mut mpsc::Receiver<MetaEvent>) -> dmr_events::CallMetadata {
    match rx.try_recv().expect("expected metadata frame") {
        MetaEvent::Call(meta) => meta,
        other => panic!("expected Call, got {other:?}"),
    }
}

#[tokio::test]
async fn rx_header_uses_callsign_lookup_when_provided() {
    let lookup: CallsignLookup = std::sync::Arc::new(|id| {
        if id.as_u32() == 12345 {
            Some(("N0CALL".to_string(), "Test".to_string()))
        } else {
            None
        }
    });
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, mut metadata_rx) =
        make_machine_with_lookup(Some(lookup));
    m.on_dmrd(&header_dmrd(0xAA)).await;
    let meta = expect_call(&mut metadata_rx);
    assert_eq!(meta.call.as_deref(), Some("N0CALL"));
    assert_eq!(meta.name.as_deref(), Some("Test"));
}

#[tokio::test]
async fn rx_header_omits_call_name_on_lookup_miss() {
    let lookup: CallsignLookup = std::sync::Arc::new(|_| None);
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, mut metadata_rx) =
        make_machine_with_lookup(Some(lookup));
    m.on_dmrd(&header_dmrd(0xAA)).await;
    let meta = expect_call(&mut metadata_rx);
    assert!(meta.call.is_none());
    assert!(meta.name.is_none());
}

#[tokio::test]
async fn rx_header_emits_call_metadata() {
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, mut metadata_rx) = make_machine();
    m.on_dmrd(&header_dmrd(0xAA)).await;
    let meta = expect_call(&mut metadata_rx);
    // src_id 12345, dst_id 91 from header_dmrd; slot 1, cc 1 from
    // test_voice_config.
    assert_eq!(meta.dmr_id.as_u32(), 12345);
    assert_eq!(meta.tg.as_u32(), 91);
    assert_eq!(meta.slot, Slot::One);
    assert_eq!(meta.cc.value(), 1);
}

#[tokio::test]
async fn rx_terminator_clears_metadata() {
    // Header emits a populated Call; terminator emits Clear so a
    // downstream consumer that latches knows to clear.
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, mut metadata_rx) = make_machine();
    m.on_dmrd(&header_dmrd(0xBE)).await;
    let _hdr = expect_call(&mut metadata_rx);
    m.on_dmrd(&terminator_dmrd(0xBE)).await;
    assert!(matches!(metadata_rx.try_recv(), Ok(MetaEvent::Clear)));
}

#[tokio::test]
async fn shutdown_during_rxhang_does_not_double_clear() {
    // Terminator emits Clear and parks the machine in RxHang.
    // A subsequent shutdown while still in RxHang must not emit
    // a second Clear -- a downstream consumer that latches on
    // Clear would log the call twice or, worse, report an
    // active call cleared after it had already ended.
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, mut metadata_rx) = make_machine();
    m.on_dmrd(&header_dmrd(0xCC)).await;
    let _ = expect_call(&mut metadata_rx);
    m.on_dmrd(&terminator_dmrd(0xCC)).await;
    assert!(matches!(metadata_rx.try_recv(), Ok(MetaEvent::Clear)));
    assert!(matches!(m.state, PttState::RxHang(_)));
    m.on_shutdown().await;
    let extra = metadata_rx.try_recv();
    assert!(
        extra.is_err(),
        "unexpected metadata event after RxHang shutdown: {extra:?}"
    );
}

#[tokio::test]
async fn rx_stream_change_emits_new_metadata() {
    // Mid-Rx, a voice frame for a new stream_id triggers a fresh
    // metadata emission so the consumer learns about the new
    // talker.
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, mut metadata_rx) = make_machine();
    m.on_dmrd(&header_dmrd(0x111)).await;
    let _first = expect_call(&mut metadata_rx);
    let mut second = voice_dmrd(0x222);
    second.src_id = SubscriberId::try_from(67890).unwrap();
    m.on_dmrd(&second).await;
    let meta = expect_call(&mut metadata_rx);
    assert_eq!(meta.dmr_id.as_u32(), 67890);
}

#[tokio::test]
async fn rx_stream_change_emits_unkey_for_prior_stream() {
    // The audio_tx consumer (usrp::tx_task) keys per-call diagnostic
    // recorders off keyup transitions; a stream change must surface
    // as unkey(prior_sid) -> keyup(new_sid).
    let (mut m, mut audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_dmrd(&voice_dmrd(0x111)).await;
    while let Ok(f) = audio_rx.try_recv() {
        assert_eq!(f.dmr_stream_id, Some(0x111));
        assert!(f.keyup);
    }
    m.on_dmrd(&voice_dmrd(0x222)).await;
    let unkey = audio_rx
        .try_recv()
        .expect("expected unkey for prior stream");
    assert!(
        !unkey.keyup,
        "first frame after stream change must be unkey"
    );
    assert_eq!(unkey.dmr_stream_id, Some(0x111));
    let next = audio_rx.try_recv().expect("expected new-stream voice");
    assert!(next.keyup);
    assert_eq!(next.dmr_stream_id, Some(0x222));
}

#[tokio::test]
async fn rx_hang_blocks_tx() {
    let (mut m, _audio_rx, mut dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.state = PttState::RxHang(Instant::now() + Duration::from_secs(10));
    m.on_audio(&voice_audio()).await;
    assert!(matches!(m.state, PttState::RxHang(_)));
    assert!(dmrd_voice_rx.try_recv().is_err());
}

#[tokio::test]
async fn tx_keyup_sends_header() {
    let (mut m, _audio_rx, _dmrd_voice_rx, mut dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_audio(&voice_audio()).await;
    assert!(matches!(m.state, PttState::Tx(_)));
    let pkt = dmrd_control_rx.try_recv().unwrap();
    let dmrd = Dmrd::parse(&pkt).unwrap();
    assert_eq!(dmrd.frame_type, FrameType::DataSync);
    assert_eq!(dmrd.dtype_vseq, DATA_TYPE_VOICE_HEADER);
}

/// make_machine with a custom callsign so the TX-entry path
/// builds the corresponding TA header LC into lc_rotation.
fn make_machine_with_callsign(callsign: &str) -> TestMachine {
    let mut cfg = test_voice_config();
    cfg.callsign = callsign.to_string();
    let (audio_tx, audio_rx) = mpsc::channel(16);
    let (dmrd_voice_out, dmrd_voice_rx) = mpsc::channel(16);
    let (dmrd_control_out, dmrd_control_rx) = mpsc::unbounded_channel();
    let (metadata_tx, metadata_rx) = mpsc::channel(16);
    let m = PttMachine::new(
        cfg,
        PttDiagnostics::default(),
        PttPolicy::default(),
        Box::new(StubVocoder),
        audio_tx,
        dmrd_voice_out,
        dmrd_control_out,
        metadata_tx,
        None,
        None,
        CancellationToken::new(),
    );
    (m, audio_rx, dmrd_voice_rx, dmrd_control_rx, metadata_rx)
}

#[tokio::test]
async fn tx_with_short_callsign_includes_ta_in_rotation() {
    // Configured callsign fits TA Header (<=7 ASCII chars), so
    // lc_rotation is [voice_lc, ta_header] and superframes
    // alternate.
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) =
        make_machine_with_callsign("N0CALL");
    m.on_audio(&voice_audio()).await;
    let PttState::Tx(tx) = &m.state else {
        unreachable!()
    };
    assert_eq!(tx.lc_rotation.len(), 2, "voice + TA expected");
}

#[tokio::test]
async fn tx_with_empty_callsign_omits_ta() {
    // No callsign -> no TA -> single-entry rotation, voice LC
    // every superframe (existing behavior preserved).
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) =
        make_machine_with_callsign("");
    m.on_audio(&voice_audio()).await;
    let PttState::Tx(tx) = &m.state else {
        unreachable!()
    };
    assert_eq!(tx.lc_rotation.len(), 1);
}

#[tokio::test]
async fn tx_with_long_callsign_includes_ta_blocks() {
    // 15-char alias -> header + 1 block -> rotation = voice + 2 TA = 3.
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) =
        make_machine_with_callsign("N0CALL Operator");
    m.on_audio(&voice_audio()).await;
    let PttState::Tx(tx) = &m.state else {
        unreachable!()
    };
    assert_eq!(tx.lc_rotation.len(), 3, "voice + header + 1 block expected");
}

#[tokio::test]
async fn tx_with_oversized_callsign_omits_ta() {
    // >31 chars -> TA encoder returns empty -> voice-only rotation.
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) =
        make_machine_with_callsign("THIRTYTWOCHARTHIRTYTWOCHARTHIRTY2");
    m.on_audio(&voice_audio()).await;
    let PttState::Tx(tx) = &m.state else {
        unreachable!()
    };
    assert_eq!(tx.lc_rotation.len(), 1);
}

#[tokio::test]
async fn tx_superframe_idx_advances_on_vseq_wrap() {
    // Send one full superframe worth of audio (6 bursts * 3 PCM
    // frames = 18 frames) and verify superframe_idx incremented.
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) =
        make_machine_with_callsign("N0CALL");
    // 18 voice frames -> 6 bursts (one per FRAMES_PER_BURST=3
    // pcm frames) -> superframe_idx = 1 after wrap.
    for _ in 0..18 {
        m.on_audio(&voice_audio()).await;
    }
    let PttState::Tx(tx) = &m.state else {
        unreachable!()
    };
    assert!(
        tx.superframe_idx >= 1,
        "expected superframe_idx>=1 after one superframe, got {}",
        tx.superframe_idx
    );
}

#[tokio::test]
async fn tx_lc_rotation_alternates_across_superframes() {
    // With voice + TA configured (rotation len 2), the embedded
    // LC fragment carried in a given vseq slot must differ
    // between consecutive superframes.  A rotation that gets
    // stuck on index 0 would emit the voice LC every superframe
    // and never put TA on air -- silent regression we'd only
    // notice with a DMR receiver.  Keying off vseq=1 (fragment 0)
    // since fragment 0 has the LCSS=1 distinct LC payload.
    let (mut m, _audio_rx, mut dmrd_voice_rx, mut dmrd_control_rx, _metadata_rx) =
        make_machine_with_callsign("N0CALL");
    // 36 PCM frames -> 12 bursts -> 2 full superframes.
    for _ in 0..36 {
        m.on_audio(&voice_audio()).await;
    }
    let mut frag0: Vec<[u8; 4]> = Vec::new();
    while let Ok(pkt) = dmrd_voice_rx.try_recv() {
        let dmrd = Dmrd::parse(&pkt).unwrap();
        if dmrd.frame_type == FrameType::Voice && dmrd.dtype_vseq == 1 {
            let emb = super::super::frame::extract_sync_section(&dmrd.dmr_data);
            frag0.push([emb[1], emb[2], emb[3], emb[4]]);
        }
    }
    assert!(
        frag0.len() >= 2,
        "expected >= 2 superframes; got {}",
        frag0.len()
    );
    assert_ne!(frag0[0], frag0[1], "LC rotation did not advance");
    assert!(matches!(
        Dmrd::parse(&dmrd_control_rx.try_recv().expect("expected header"))
            .unwrap()
            .dtype_vseq,
        DATA_TYPE_VOICE_HEADER
    ));
}

#[tokio::test]
async fn tx_unkey_sends_terminator() {
    let (mut m, _audio_rx, _dmrd_voice_rx, mut dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_audio(&voice_audio()).await;
    m.on_audio(&unkey_audio()).await;
    assert!(matches!(m.state, PttState::Idle));
    let mut last = None;
    while let Ok(pkt) = dmrd_control_rx.try_recv() {
        last = Some(pkt);
    }
    let dmrd = Dmrd::parse(last.as_ref().unwrap()).unwrap();
    assert_eq!(dmrd.dtype_vseq, DATA_TYPE_VOICE_TERMINATOR);
}

#[tokio::test]
async fn tx_blocked_during_rx() {
    let (mut m, _audio_rx, _dmrd_voice_rx, mut dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_dmrd(&header_dmrd(0xDD)).await;
    assert!(matches!(m.state, PttState::Rx(_)));
    m.on_audio(&voice_audio()).await;
    assert!(matches!(m.state, PttState::Rx(_)));
    assert!(dmrd_control_rx.try_recv().is_err());
}

#[tokio::test]
async fn rx_blocked_during_tx() {
    let (mut m, mut audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_audio(&voice_audio()).await;
    assert!(matches!(m.state, PttState::Tx(_)));
    m.on_dmrd(&header_dmrd(0xEE)).await;
    assert!(matches!(m.state, PttState::Tx(_)));
    assert!(audio_rx.try_recv().is_err());
}

#[tokio::test]
async fn rx_during_hang_restarts_rx() {
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.state = PttState::RxHang(Instant::now() + Duration::from_secs(10));
    m.on_dmrd(&header_dmrd(0xFF)).await;
    assert!(matches!(m.state, PttState::Rx(ref rx) if rx.stream_id == 0xFF));
}

#[tokio::test]
async fn rx_timeout_emits_unkey_and_enters_hang() {
    // Simulate an Rx state whose last_voice is old enough that
    // the outer select would have fired sleep_until.  Calling
    // on_timeout unconditionally processes the state.
    let (mut m, mut audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_dmrd(&header_dmrd(0x11)).await;
    let _ = audio_rx.try_recv(); // drain anything from header
    m.on_timeout().await;
    let f = audio_rx.try_recv().expect("expected unkey on timeout");
    assert!(!f.keyup);
    assert!(matches!(m.state, PttState::RxHang(_)));
}

#[tokio::test]
async fn rx_hang_timeout_returns_to_idle() {
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.state = PttState::RxHang(Instant::now());
    m.on_timeout().await;
    assert!(matches!(m.state, PttState::Idle));
}

#[tokio::test]
async fn tx_timeout_emits_terminator() {
    let (mut m, _audio_rx, _dmrd_voice_rx, mut dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_audio(&voice_audio()).await; // enter Tx, emits header
    let _ = dmrd_control_rx.try_recv(); // drain header
    // Back-date started so tx_timeout has elapsed.
    if let PttState::Tx(ref mut tx) = m.state {
        tx.started = Instant::now() - Duration::from_secs(181);
    }
    m.on_timeout().await;
    let mut saw_term = false;
    while let Ok(pkt) = dmrd_control_rx.try_recv() {
        let dmrd = Dmrd::parse(&pkt).unwrap();
        if dmrd.frame_type == FrameType::DataSync && dmrd.dtype_vseq == DATA_TYPE_VOICE_TERMINATOR {
            saw_term = true;
        }
    }
    assert!(saw_term, "expected terminator after TX timeout");
}

#[tokio::test]
async fn idle_timeout_is_noop() {
    let (mut m, mut audio_rx, mut dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_timeout().await;
    assert!(matches!(m.state, PttState::Idle));
    assert!(audio_rx.try_recv().is_err());
    assert!(dmrd_voice_rx.try_recv().is_err());
}

#[tokio::test]
async fn rx_voice_for_different_stream_switches_stream_id() {
    // Mid-Rx, a voice frame for a new stream_id re-homes Rx onto
    // the new stream rather than dropping or starting a parallel
    // state.  Covers the voice.rs `"RX stream change"` branch.
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_dmrd(&header_dmrd(0xAAA1)).await;
    m.on_dmrd(&voice_dmrd(0xAAA2)).await;
    let PttState::Rx(rx) = &m.state else {
        panic!("expected Rx, got something else");
    };
    assert_eq!(rx.stream_id, 0xAAA2);
}

#[tokio::test]
async fn unkey_usrp_during_rx_does_not_drop_state() {
    // A stray USRP unkey arriving while we're decoding DMR
    // (Rx) must NOT clobber the call to Idle.  Earlier code
    // unconditionally take_state'd in the unkey branch and
    // silently lost the Rx context (and the hang timer that
    // would have followed terminator).
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_dmrd(&header_dmrd(0xBEEF)).await;
    assert!(matches!(m.state, PttState::Rx(_)));
    m.on_audio(&unkey_audio()).await;
    assert!(
        matches!(m.state, PttState::Rx(ref rx) if rx.stream_id == 0xBEEF),
        "unkey USRP must not affect non-Tx state, got {:?}",
        std::mem::discriminant(&m.state)
    );
}

#[tokio::test]
async fn unkey_usrp_during_rxhang_does_not_drop_state() {
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.state = PttState::RxHang(Instant::now() + Duration::from_secs(10));
    m.on_audio(&unkey_audio()).await;
    assert!(matches!(m.state, PttState::RxHang(_)));
}

#[tokio::test]
async fn terminator_for_other_stream_keeps_rx() {
    // Terminator with a stream_id different from the current Rx
    // is ignored (no transition to RxHang).
    let (mut m, _audio_rx, _dmrd_voice_rx, _dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_dmrd(&header_dmrd(0xAAA1)).await;
    m.on_dmrd(&terminator_dmrd(0xAAA2)).await;
    assert!(
        matches!(m.state, PttState::Rx(ref rx) if rx.stream_id == 0xAAA1),
        "terminator for other stream should not end Rx on current stream"
    );
}

// --- Integration tests: spawn the real voice_task and drive it
// --- through actual mpsc channels, so the select-loop dispatch,
// --- spawn_blocking, cancel plumbing, and channel-close shutdown
// --- all exercise alongside the per-handler unit tests above.

struct Rig {
    dmrd_in: mpsc::Sender<Dmrd>,
    audio_in: mpsc::Sender<AudioFrame>,
    control_in: mpsc::Sender<ControlEvent>,
    audio_out: mpsc::Receiver<AudioFrame>,
    dmrd_voice_out: mpsc::Receiver<[u8; PACKET_SIZE]>,
    dmrd_control_out: mpsc::UnboundedReceiver<[u8; PACKET_SIZE]>,
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<()>,
}

impl Rig {
    fn start() -> Self {
        let cfg = test_voice_config();
        let (dmrd_in, dmrd_rx) = mpsc::channel(16);
        let (audio_in, audio_rx) = mpsc::channel(16);
        let (control_in, control_rx) = mpsc::channel(16);
        let (audio_tx, audio_out) = mpsc::channel(16);
        let (dmrd_voice_out_tx, dmrd_voice_out) = mpsc::channel(16);
        let (dmrd_control_out_tx, dmrd_control_out) = mpsc::unbounded_channel();
        let (metadata_tx, _metadata_out) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(voice_task(
            dmrd_rx,
            audio_rx,
            control_rx,
            audio_tx,
            dmrd_voice_out_tx,
            dmrd_control_out_tx,
            metadata_tx,
            None,
            None,
            Box::new(StubVocoder),
            cfg,
            PttDiagnostics::default(),
            PttPolicy::default(),
            cancel.clone(),
        ));
        Self {
            dmrd_in,
            audio_in,
            control_in,
            audio_out,
            dmrd_voice_out,
            dmrd_control_out,
            cancel,
            handle,
        }
    }

    async fn shutdown(self) {
        self.cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(1), self.handle).await;
    }

    /// Drain emitted frames until the producer goes idle.  Uses
    /// a per-item `timeout` rather than a fixed `sleep`, so a
    /// slow CI box doesn't truncate the drain and a fast one
    /// doesn't over-wait.
    async fn drain_audio(&mut self) -> Vec<AudioFrame> {
        drain_with_idle_timeout(&mut self.audio_out).await
    }

    async fn drain_voice_dmrd(&mut self) -> Vec<[u8; PACKET_SIZE]> {
        drain_with_idle_timeout(&mut self.dmrd_voice_out).await
    }

    async fn drain_control_dmrd(&mut self) -> Vec<[u8; PACKET_SIZE]> {
        drain_unbounded_with_idle_timeout(&mut self.dmrd_control_out).await
    }
}

async fn drain_with_idle_timeout<T>(rx: &mut mpsc::Receiver<T>) -> Vec<T> {
    // 50 ms of producer silence means the burst is done.  Big
    // enough to absorb scheduler jitter on CI, small enough
    // that tests stay under a second even when they drain
    // multiple bursts.
    const IDLE_MS: u64 = 50;
    let mut out = Vec::new();
    while let Ok(Some(item)) = tokio::time::timeout(Duration::from_millis(IDLE_MS), rx.recv()).await
    {
        out.push(item);
    }
    out
}

async fn drain_unbounded_with_idle_timeout<T>(rx: &mut mpsc::UnboundedReceiver<T>) -> Vec<T> {
    const IDLE_MS: u64 = 50;
    let mut out = Vec::new();
    while let Ok(Some(item)) = tokio::time::timeout(Duration::from_millis(IDLE_MS), rx.recv()).await
    {
        out.push(item);
    }
    out
}

#[tokio::test]
async fn integration_full_rx_call() {
    let mut rig = Rig::start();

    // Header -> voice burst -> terminator.
    rig.dmrd_in.send(header_dmrd(0x1234)).await.unwrap();
    rig.dmrd_in.send(voice_dmrd(0x1234)).await.unwrap();
    rig.dmrd_in.send(terminator_dmrd(0x1234)).await.unwrap();

    let frames = rig.drain_audio().await;
    // 3 voice frames from the voice burst + 1 unkey from terminator.
    assert_eq!(frames.len(), 4, "got {} frames", frames.len());
    assert!(
        frames[..3].iter().all(|f| f.keyup && f.samples.is_some()),
        "expected 3 voice frames"
    );
    assert!(!frames[3].keyup, "expected final unkey");

    // No DMRD emitted in RX-only direction.
    assert!(rig.drain_voice_dmrd().await.is_empty());
    assert!(rig.drain_control_dmrd().await.is_empty());
    rig.shutdown().await;
}

#[tokio::test]
async fn integration_full_tx_call() {
    let mut rig = Rig::start();

    // Keyup + 3 voice frames + unkey.  After 3 PCM frames the
    // PttMachine flushes a DMRD voice burst; unkey adds the
    // terminator.
    rig.audio_in.send(voice_audio()).await.unwrap();
    rig.audio_in.send(voice_audio()).await.unwrap();
    rig.audio_in.send(voice_audio()).await.unwrap();
    rig.audio_in.send(unkey_audio()).await.unwrap();

    let voice_pkts = rig.drain_voice_dmrd().await;
    let control_pkts = rig.drain_control_dmrd().await;
    assert!(!voice_pkts.is_empty(), "expected at least one voice packet");
    assert!(
        control_pkts.len() >= 2,
        "expected header + terminator control packets, got {}",
        control_pkts.len()
    );

    let control: Vec<_> = control_pkts
        .iter()
        .map(|p| Dmrd::parse(p).unwrap())
        .collect();
    let voice: Vec<_> = voice_pkts.iter().map(|p| Dmrd::parse(p).unwrap()).collect();
    assert_eq!(control.first().unwrap().frame_type, FrameType::DataSync);
    assert_eq!(control.first().unwrap().dtype_vseq, DATA_TYPE_VOICE_HEADER);
    assert_eq!(control.last().unwrap().frame_type, FrameType::DataSync);
    assert_eq!(
        control.last().unwrap().dtype_vseq,
        DATA_TYPE_VOICE_TERMINATOR
    );

    let sid = control.first().unwrap().stream_id;
    assert!(control.iter().all(|p| p.stream_id == sid));
    assert!(voice.iter().all(|p| p.stream_id == sid));

    rig.shutdown().await;
}

#[tokio::test]
async fn integration_shutdown_in_rx_emits_unkey() {
    let mut rig = Rig::start();

    rig.dmrd_in.send(header_dmrd(0x4242)).await.unwrap();
    rig.dmrd_in.send(voice_dmrd(0x4242)).await.unwrap();
    // Drain the voice frames before shutdown so we can see the
    // cancel-path unkey in isolation.
    let _ = rig.drain_audio().await;

    rig.cancel.cancel();
    // Collect whatever the shutdown path emits.
    let mut tail = Vec::new();
    while let Ok(Some(f)) =
        tokio::time::timeout(Duration::from_millis(100), rig.audio_out.recv()).await
    {
        tail.push(f);
    }
    // on_shutdown emits a single unkey when state is Rx.
    assert_eq!(tail.len(), 1);
    assert!(!tail[0].keyup);
    let _ = tokio::time::timeout(Duration::from_secs(1), rig.handle).await;
}

#[tokio::test]
async fn integration_shutdown_in_tx_emits_terminator() {
    let mut rig = Rig::start();

    // Get into Tx with a keyup + voice frame.
    rig.audio_in.send(voice_audio()).await.unwrap();
    let _ = rig.drain_control_dmrd().await; // consume header

    rig.cancel.cancel();
    let mut tail = Vec::new();
    while let Ok(Some(p)) =
        tokio::time::timeout(Duration::from_millis(200), rig.dmrd_control_out.recv()).await
    {
        tail.push(p);
    }
    // on_shutdown flushes any partial burst then sends a terminator.
    let parsed: Vec<_> = tail.iter().map(|p| Dmrd::parse(p).unwrap()).collect();
    assert!(
        parsed
            .iter()
            .any(|p| p.frame_type == FrameType::DataSync
                && p.dtype_vseq == DATA_TYPE_VOICE_TERMINATOR),
        "shutdown should emit terminator; got {parsed:?}"
    );
    let _ = tokio::time::timeout(Duration::from_secs(1), rig.handle).await;
}

#[tokio::test]
async fn tx_network_reset_drops_active_call() {
    let (mut m, _audio_rx, _dmrd_voice_rx, mut dmrd_control_rx, _metadata_rx) = make_machine();
    m.on_audio(&voice_audio()).await;
    let hdr = Dmrd::parse(&dmrd_control_rx.try_recv().unwrap()).unwrap();

    m.on_audio(&voice_audio()).await;
    m.on_control(ControlEvent::NetworkReset).await;
    assert!(matches!(m.state, PttState::Idle));

    m.on_audio(&voice_audio()).await;
    let restarted = Dmrd::parse(&dmrd_control_rx.try_recv().unwrap()).unwrap();
    assert_eq!(restarted.frame_type, FrameType::DataSync);
    assert_eq!(restarted.dtype_vseq, DATA_TYPE_VOICE_HEADER);
    assert_ne!(restarted.stream_id, hdr.stream_id);
}

// --- Pre-encode filter integration tests ---

/// Continuous-phase sine wave as an AudioFrame starting at `frame_idx * VOICE_SAMPLES`.
fn sine_frame(freq: f32, frame_idx: usize) -> AudioFrame {
    let fs = 8000.0_f32;
    let amp = 20000.0_f32;
    let mut samples = [0i16; VOICE_SAMPLES];
    let offset = frame_idx * VOICE_SAMPLES;
    for (i, s) in samples.iter_mut().enumerate() {
        let t = (offset + i) as f32 / fs;
        *s = (amp * (std::f32::consts::TAU * freq * t).sin()) as i16;
    }
    AudioFrame {
        keyup: true,
        samples: Some(samples),
        dmr_stream_id: None,
    }
}

fn pcm_rms(frames: &[PcmFrame]) -> f32 {
    let n = frames.len() * VOICE_SAMPLES;
    let sum_sq: f64 = frames
        .iter()
        .flat_map(|f| f.iter().copied())
        .map(|s| (s as f64) * (s as f64))
        .sum();
    ((sum_sq / n as f64) as f32).sqrt()
}

/// Build a machine with `pre_encode_filter = true` and a spy vocoder,
/// drive 30 frames of a sine at `freq`, and return the RMS of the
/// settled tail (last 9 encode inputs = 3 bursts).
async fn measure_filtered_rms(freq: f32) -> f32 {
    let recorded: Arc<Mutex<Vec<PcmFrame>>> = Arc::new(Mutex::new(Vec::new()));
    let (audio_tx, _audio_rx) = mpsc::channel(64);
    let (dmrd_voice_out, _dmrd_voice_rx) = mpsc::channel(64);
    let (dmrd_control_out, _dmrd_control_rx) = mpsc::unbounded_channel();
    let (metadata_tx, _metadata_rx) = mpsc::channel(16);
    let mut m = PttMachine::new(
        test_voice_config(),
        PttDiagnostics::default(),
        PttPolicy {
            pre_encode_filter: true,
        },
        Box::new(SpyVocoder {
            recorded: recorded.clone(),
        }),
        audio_tx,
        dmrd_voice_out,
        dmrd_control_out,
        metadata_tx,
        None,
        None,
        CancellationToken::new(),
    );
    for i in 0..30 {
        m.on_audio(&sine_frame(freq, i)).await;
    }
    let guard = recorded.lock().unwrap();
    // Last 9 frames = 3 settled bursts; encode is awaited inline so
    // guard is fully populated by the time on_audio returns.
    let tail: Vec<PcmFrame> = guard[guard.len() - 9..].to_vec();
    pcm_rms(&tail)
}

#[tokio::test]
async fn pre_encode_filter_attenuates_sub_100hz() {
    // Verify the filter is wired into the encode path: 100 Hz input
    // (well below the 250 Hz HP cutoff) must be >10x attenuated
    // relative to 1 kHz midband.
    let rms_100 = measure_filtered_rms(100.0).await;
    let rms_1k = measure_filtered_rms(1000.0).await;
    let ratio = rms_100 / rms_1k;
    assert!(
        ratio < 0.1,
        "100 Hz / 1 kHz ratio {ratio:.3} >= 0.1; filter may not be applied"
    );
}

#[tokio::test]
async fn pre_encode_filter_output_matches_direct_filter() {
    // Drive FRAMES_PER_BURST frames through the machine and a reference
    // filter in parallel; assert sample-exact agreement.  Detects dropped
    // or reordered samples in the encode path that the RMS test would miss.
    let recorded: Arc<Mutex<Vec<PcmFrame>>> = Arc::new(Mutex::new(Vec::new()));
    let (audio_tx, _audio_rx) = mpsc::channel(16);
    let (dmrd_voice_out, _dmrd_voice_rx) = mpsc::channel(16);
    let (dmrd_control_out, _dmrd_control_rx) = mpsc::unbounded_channel();
    let (metadata_tx, _metadata_rx) = mpsc::channel(16);
    let mut m = PttMachine::new(
        test_voice_config(),
        PttDiagnostics::default(),
        PttPolicy {
            pre_encode_filter: true,
        },
        Box::new(SpyVocoder {
            recorded: recorded.clone(),
        }),
        audio_tx,
        dmrd_voice_out,
        dmrd_control_out,
        metadata_tx,
        None,
        None,
        CancellationToken::new(),
    );

    // Reference filter: fresh construction matches the machine's reset-on-tx-start.
    let mut ref_filter = pre_encode_voice_8khz();
    let mut expected: Vec<PcmFrame> = Vec::new();
    for i in 0..FRAMES_PER_BURST {
        let frame = sine_frame(440.0, i);
        let mut ref_pcm = frame.samples.unwrap();
        ref_filter.process_pcm(&mut ref_pcm);
        expected.push(ref_pcm);
        m.on_audio(&frame).await;
    }

    let spy = recorded.lock().unwrap();
    assert_eq!(
        spy.len(),
        FRAMES_PER_BURST,
        "expected {FRAMES_PER_BURST} encoded frames"
    );
    for (i, (got, want)) in spy.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            got, want,
            "frame {i}: spy output differs from reference filter"
        );
    }
}

#[tokio::test]
async fn integration_network_reset_restarts_tx_with_fresh_header() {
    let mut rig = Rig::start();

    rig.audio_in.send(voice_audio()).await.unwrap();
    let first = rig.drain_control_dmrd().await;
    let first_hdr = Dmrd::parse(first.first().unwrap()).unwrap();
    assert_eq!(first_hdr.dtype_vseq, DATA_TYPE_VOICE_HEADER);

    rig.audio_in.send(voice_audio()).await.unwrap();
    rig.control_in
        .send(ControlEvent::NetworkReset)
        .await
        .unwrap();
    rig.audio_in.send(voice_audio()).await.unwrap();

    let resumed = rig.drain_control_dmrd().await;
    let parsed: Vec<_> = resumed.iter().map(|p| Dmrd::parse(p).unwrap()).collect();
    let hdr = parsed
        .iter()
        .find(|p| p.frame_type == FrameType::DataSync && p.dtype_vseq == DATA_TYPE_VOICE_HEADER)
        .expect("expected fresh header after network reset");
    assert_ne!(hdr.stream_id, first_hdr.stream_id);

    rig.shutdown().await;
}

#[tokio::test]
async fn vocoder_panic_cancels_voice_task() {
    let cancel = CancellationToken::new();
    let (audio_tx, _audio_rx) = mpsc::channel(16);
    let (dmrd_voice_out, _dmrd_voice_rx) = mpsc::channel(16);
    let (dmrd_control_out, _dmrd_control_rx) = mpsc::unbounded_channel();
    let (metadata_tx, _metadata_rx) = mpsc::channel(16);
    let mut m = PttMachine::new(
        test_voice_config(),
        PttDiagnostics::default(),
        PttPolicy::default(),
        Box::new(PanickingVocoder),
        audio_tx,
        dmrd_voice_out,
        dmrd_control_out,
        metadata_tx,
        None,
        None,
        cancel.clone(),
    );
    for _ in 0..FRAMES_PER_BURST {
        m.on_audio(&voice_audio()).await;
    }
    assert!(
        cancel.is_cancelled(),
        "vocoder panic must cancel the task for orderly shutdown"
    );
}
