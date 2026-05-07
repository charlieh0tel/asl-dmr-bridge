//! Per-call diagnostic capture state owned by `PttMachine`.
//!
//! Bundles the WAV recorders + level accumulators (PCM at the
//! encoder input on TX and at the decoder output on RX) and the
//! AMBE source-bit writer (49 bits per encoded frame, MSB-packed
//! into 7 bytes, mbelib bit order; same wire format as
//! `parity_expected_49bit.bin`).

use std::fs::File;
use std::io::BufWriter;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use ambe::AmbeFrame;
use ambe::PcmFrame;
use pcm_utils::levels::LevelAccumulator;
use pcm_utils::levels::fmt_dbfs;
use pcm_utils::wav;
use pcm_utils::wav::WavRecorder;
use tracing::info;
use tracing::warn;

pub(crate) struct CallDiagnostics {
    pcm_record_dir: Option<PathBuf>,
    pub(crate) tx_recorder: Option<WavRecorder>,
    pub(crate) rx_recorder: Option<WavRecorder>,
    pub(crate) tx_levels: LevelAccumulator,
    pub(crate) rx_levels: LevelAccumulator,
    tx_ambe_writer: Option<BufWriter<File>>,
    rx_ambe_writer: Option<BufWriter<File>>,
}

impl CallDiagnostics {
    pub(crate) fn new(pcm_record_dir: Option<PathBuf>) -> Self {
        Self {
            pcm_record_dir,
            tx_recorder: None,
            rx_recorder: None,
            tx_levels: LevelAccumulator::default(),
            rx_levels: LevelAccumulator::default(),
            tx_ambe_writer: None,
            rx_ambe_writer: None,
        }
    }

    pub(crate) fn on_tx_start(&mut self, stream_id: u32) {
        self.tx_levels = LevelAccumulator::default();
        self.tx_recorder = wav::open_call_recorder(
            self.pcm_record_dir.as_deref(),
            "fm_to_dmr_encode_in",
            stream_id,
        );
        self.tx_ambe_writer = open_ambe_writer(
            self.pcm_record_dir.as_deref(),
            "fm_to_dmr_encoded",
            stream_id,
        );
    }

    pub(crate) fn on_tx_end(&mut self, stream_id: u32) {
        log_call_levels("fm_to_dmr", "encode_in", stream_id, &self.tx_levels);
        // `Drop` finalizes the recorder and flushes the writer.
        self.tx_recorder = None;
        self.tx_ambe_writer = None;
        self.tx_levels = LevelAccumulator::default();
    }

    pub(crate) fn on_rx_start(&mut self, stream_id: u32) {
        self.rx_levels = LevelAccumulator::default();
        self.rx_recorder = wav::open_call_recorder(
            self.pcm_record_dir.as_deref(),
            "dmr_to_fm_decode_out",
            stream_id,
        );
        self.rx_ambe_writer = open_ambe_writer(
            self.pcm_record_dir.as_deref(),
            "dmr_to_fm_decode_in",
            stream_id,
        );
    }

    pub(crate) fn on_rx_end(&mut self, stream_id: u32) {
        log_call_levels("dmr_to_fm", "decode_out", stream_id, &self.rx_levels);
        self.rx_recorder = None;
        self.rx_ambe_writer = None;
        self.rx_levels = LevelAccumulator::default();
    }

    pub(crate) fn record_tx_pcm(&mut self, pcm: &PcmFrame) {
        record_pcm(&mut self.tx_recorder, &mut self.tx_levels, pcm, "tx");
    }

    pub(crate) fn record_tx_ambe(&mut self, coded: &AmbeFrame) {
        record_ambe(&mut self.tx_ambe_writer, coded, "tx");
    }

    pub(crate) fn record_rx_ambe(&mut self, coded: &AmbeFrame) {
        record_ambe(&mut self.rx_ambe_writer, coded, "rx");
    }
}

fn record_ambe(writer: &mut Option<BufWriter<File>>, coded: &AmbeFrame, kind: &str) {
    if let Some(w) = writer.as_mut() {
        let bits = ambe::voice_channel::to_source_bits(coded);
        if let Err(e) = w.write_all(&bits) {
            warn!("{kind} ambe writer error: {e}; dropping for the rest of this call");
            *writer = None;
        }
    }
}

/// PCM-record helper exposed for the RX-side call site that takes
/// the recorder + accumulator out of `CallDiagnostics` so per-frame
/// work doesn't need `&mut self` while a shared borrow on
/// `audio_tx` is alive.  `CallDiagnostics::record_tx_pcm` uses this
/// internally for the TX path.
pub(crate) fn record_pcm(
    recorder: &mut Option<WavRecorder>,
    accumulator: &mut LevelAccumulator,
    pcm: &PcmFrame,
    kind: &str,
) {
    accumulator.add_frame(pcm);
    if let Some(rec) = recorder.as_mut()
        && let Err(e) = rec.write(pcm)
    {
        warn!("{kind} pcm record write: {e}");
        *recorder = None;
    }
}

fn open_ambe_writer(dir: Option<&Path>, kind: &str, stream_id: u32) -> Option<BufWriter<File>> {
    let dir = dir?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("{kind}_{now_ms}_{stream_id}.bin"));
    match File::create(&path) {
        Ok(f) => Some(BufWriter::new(f)),
        Err(e) => {
            warn!("open ambe writer {}: {e}", path.display());
            None
        }
    }
}

fn log_call_levels(dir: &str, point: &str, stream_id: u32, levels: &LevelAccumulator) {
    let (peak, rms, voiced_rms) = levels.summary();
    info!(
        "call_levels dir={dir} point={point} stream_id={stream_id} \
         peak={} rms={} voiced_rms={}",
        fmt_dbfs(peak),
        fmt_dbfs(rms),
        fmt_dbfs(voiced_rms),
    );
}
