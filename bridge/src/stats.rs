//! Per-call summary + periodic heartbeat counters.
//!
//! Two log lines:
//!
//! - **Per-call** (one per call, at the call's end): `dir`, `src`,
//!   `dst`, `slot`, `dur`, `frames`, `drops`, `transcode_p50_ms`,
//!   `transcode_p99_ms`, `reason`.  Suppressed when the call's
//!   duration is below `stats.min_call_log_duration` (cumulative
//!   counters still update -- the call is real, just below the
//!   logging-noise floor).
//!
//! - **Heartbeat** (every `stats.heartbeat_interval`): cumulative
//!   `calls`, `frames`, `drops` per direction since process start.
//!   Suppressed when no frames advanced in either direction since
//!   the previous tick if `stats.skip_idle_heartbeat` is true.
//!
//! `Stats` holds the cumulative counters as plain `AtomicU64`s -- one
//! field per metric -- so a future Prometheus `/metrics` exporter can
//! walk them mechanically.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use dmr_events::CallDirection;
use dmr_events::StatsEvent;
use dmr_events::TerminationReason;
use dmr_types::Slot;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Cumulative counters per direction.  Read by the heartbeat task and
/// (later) by a `/metrics` exporter; written by the stats consumer
/// task.  All increments use `Relaxed` -- the heartbeat tolerates a
/// briefly-stale read, and there is no cross-counter invariant to
/// preserve.
#[derive(Debug, Default)]
pub(crate) struct DirectionStats {
    pub(crate) calls_completed: AtomicU64,
    pub(crate) voice_frames: AtomicU64,
    pub(crate) drops: AtomicU64,
}

#[derive(Debug)]
pub(crate) struct Stats {
    started_at: Instant,
    fm_to_dmr: DirectionStats,
    dmr_to_fm: DirectionStats,
}

impl Stats {
    pub(crate) fn new() -> Self {
        Self {
            started_at: Instant::now(),
            fm_to_dmr: DirectionStats::default(),
            dmr_to_fm: DirectionStats::default(),
        }
    }

    fn dir(&self, d: CallDirection) -> &DirectionStats {
        match d {
            CallDirection::FmToDmr => &self.fm_to_dmr,
            CallDirection::DmrToFm => &self.dmr_to_fm,
        }
    }
}

/// Per-direction in-flight call accumulator.  Frames + drops + per-
/// frame transcode latencies are appended here as `StatsEvent`s
/// arrive; on `CallEnd`, the consumer computes p50/p99, folds the
/// counts into the cumulative `Stats`, and emits the per-call log
/// line (subject to `min_call_log_duration`).
struct CallAccumulator {
    started_at: Instant,
    src_id: u32,
    dst_id: u32,
    slot: Slot,
    frames: u64,
    drops: u64,
    /// Per-frame transcode (encode for FM->DMR, decode for DMR->FM).
    /// Capacity hint = ~3000 frames (60s at 50 frames/s); typical
    /// calls are well under a minute.
    transcode_us: Vec<u32>,
}

impl CallAccumulator {
    fn new(src_id: u32, dst_id: u32, slot: Slot) -> Self {
        Self {
            started_at: Instant::now(),
            src_id,
            dst_id,
            slot,
            frames: 0,
            drops: 0,
            transcode_us: Vec::with_capacity(256),
        }
    }
}

/// Streaming-quantile via simple sort over the collected samples.
/// `transcode_us` is up to ~3000 entries per call; sorting in place
/// at end-of-call is O(n log n) on a tiny vector and beats running
/// a histogram for the per-call log path.  Returns `None` for an
/// empty input so the log can omit the field cleanly.
fn percentile_us(samples: &mut [u32], pct: f32) -> Option<u32> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_unstable();
    // Nearest-rank percentile, indexes clamped to last element.
    let idx = ((pct / 100.0) * samples.len() as f32).ceil() as usize;
    let idx = idx.saturating_sub(1).min(samples.len() - 1);
    Some(samples[idx])
}

/// Drain the stats event channel until close, accumulating per-call
/// stats and folding into cumulative `Stats` at each `CallEnd`.
/// Cancellation closes the producer side via the voice task; this
/// task naturally exits when `rx.recv()` returns `None`.
pub(crate) async fn consume_events(
    stats: Arc<Stats>,
    mut rx: mpsc::Receiver<StatsEvent>,
    min_call_log_duration: Duration,
) {
    let mut fm_to_dmr: Option<CallAccumulator> = None;
    let mut dmr_to_fm: Option<CallAccumulator> = None;
    while let Some(evt) = rx.recv().await {
        match evt {
            StatsEvent::CallStart {
                dir,
                src_id,
                dst_id,
                slot,
            } => {
                let slot_ref = match dir {
                    CallDirection::FmToDmr => &mut fm_to_dmr,
                    CallDirection::DmrToFm => &mut dmr_to_fm,
                };
                // Replace any in-flight accumulator for this direction
                // (a missed CallEnd would otherwise leave it
                // wedged); fold its counters in first so frames /
                // drops aren't lost.
                if let Some(prev) = slot_ref.take() {
                    fold_into_stats(&stats, dir, &prev);
                }
                *slot_ref = Some(CallAccumulator::new(src_id, dst_id, slot));
            }
            StatsEvent::VoiceFrame { dir, transcode } => {
                let acc = match dir {
                    CallDirection::FmToDmr => fm_to_dmr.as_mut(),
                    CallDirection::DmrToFm => dmr_to_fm.as_mut(),
                };
                if let Some(acc) = acc {
                    acc.frames += 1;
                    acc.transcode_us
                        .push(transcode.as_micros().min(u128::from(u32::MAX)) as u32);
                }
            }
            StatsEvent::Drop { dir } => {
                let acc = match dir {
                    CallDirection::FmToDmr => fm_to_dmr.as_mut(),
                    CallDirection::DmrToFm => dmr_to_fm.as_mut(),
                };
                if let Some(acc) = acc {
                    acc.drops += 1;
                }
            }
            StatsEvent::CallEnd { dir, reason } => {
                let acc = match dir {
                    CallDirection::FmToDmr => fm_to_dmr.take(),
                    CallDirection::DmrToFm => dmr_to_fm.take(),
                };
                if let Some(mut acc) = acc {
                    let dur = acc.started_at.elapsed();
                    fold_into_stats(&stats, dir, &acc);
                    if dur >= min_call_log_duration {
                        log_call(dir, &mut acc, dur, reason);
                    }
                }
            }
        }
    }
}

fn fold_into_stats(stats: &Stats, dir: CallDirection, acc: &CallAccumulator) {
    let s = stats.dir(dir);
    s.calls_completed.fetch_add(1, Ordering::Relaxed);
    s.voice_frames.fetch_add(acc.frames, Ordering::Relaxed);
    s.drops.fetch_add(acc.drops, Ordering::Relaxed);
}

fn log_call(
    dir: CallDirection,
    acc: &mut CallAccumulator,
    dur: Duration,
    reason: TerminationReason,
) {
    let p50 = percentile_us(&mut acc.transcode_us, 50.0);
    let p99 = percentile_us(&mut acc.transcode_us, 99.0);
    info!(
        target: "bridge::stats::call",
        dir = dir.as_str(),
        src = acc.src_id,
        dst = acc.dst_id,
        slot = ?acc.slot,
        dur_ms = dur.as_millis() as u64,
        frames = acc.frames,
        drops = acc.drops,
        transcode_p50_us = p50,
        transcode_p99_us = p99,
        reason = reason.as_str(),
        "call"
    );
}

/// Periodic cumulative-counters log.  Spawns its own ticker; exits
/// when `cancel` is tripped.  When `skip_idle` is true, suppresses
/// the line on a tick where no new frames advanced since the prior
/// tick (PttMachine quiescent or call-less).
pub(crate) async fn heartbeat_task(
    stats: Arc<Stats>,
    interval: Duration,
    skip_idle: bool,
    cancel: CancellationToken,
) {
    if interval.is_zero() {
        return;
    }
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // skip the immediate first tick
    let mut last_fm_frames: u64 = 0;
    let mut last_dmr_frames: u64 = 0;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = ticker.tick() => {
                let fm_frames = stats.fm_to_dmr.voice_frames.load(Ordering::Relaxed);
                let dmr_frames = stats.dmr_to_fm.voice_frames.load(Ordering::Relaxed);
                if skip_idle && fm_frames == last_fm_frames && dmr_frames == last_dmr_frames {
                    continue;
                }
                last_fm_frames = fm_frames;
                last_dmr_frames = dmr_frames;
                let uptime = stats.started_at.elapsed();
                info!(
                    target: "bridge::stats::heartbeat",
                    uptime_s = uptime.as_secs(),
                    fm_to_dmr_calls = stats.fm_to_dmr.calls_completed.load(Ordering::Relaxed),
                    fm_to_dmr_frames = fm_frames,
                    fm_to_dmr_drops = stats.fm_to_dmr.drops.load(Ordering::Relaxed),
                    dmr_to_fm_calls = stats.dmr_to_fm.calls_completed.load(Ordering::Relaxed),
                    dmr_to_fm_frames = dmr_frames,
                    dmr_to_fm_drops = stats.dmr_to_fm.drops.load(Ordering::Relaxed),
                    "heartbeat"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_us_empty_is_none() {
        let mut v: Vec<u32> = vec![];
        assert_eq!(percentile_us(&mut v, 50.0), None);
    }

    #[test]
    fn percentile_us_single_sample_is_that_sample() {
        let mut v = vec![42];
        assert_eq!(percentile_us(&mut v, 50.0), Some(42));
        assert_eq!(percentile_us(&mut v, 99.0), Some(42));
    }

    #[test]
    fn percentile_us_nearest_rank() {
        // 10 samples, sorted: 1..=10; p50 = 5th (idx 4), p99 = 10th (idx 9).
        let mut v = vec![5, 1, 9, 3, 7, 2, 8, 4, 6, 10];
        assert_eq!(percentile_us(&mut v, 50.0), Some(5));
        assert_eq!(percentile_us(&mut v, 99.0), Some(10));
    }
}
