# Look-Ahead Limiter for the FM->DMR AGC

Working design and implementation plan.  Background context lives
in `docs/FUTURE_AGC.md`; the doc here is what we run against.

## Why we need this

After enabling `[agc.fm_to_dmr]` with `target_dbfs = -3, max_gain_db
= 18`, live `call_levels` lines from the bridge:

```
peak=0.0dBFS rms=-15.0dBFS voiced_rms=-14.5dBFS
peak=0.0dBFS rms=-16.5dBFS voiced_rms=-16.0dBFS
peak=0.0dBFS rms=-13.9dBFS voiced_rms=-13.7dBFS
peak=0.0dBFS rms=-13.2dBFS voiced_rms=-13.0dBFS
```

`voiced_rms` is finally where it should be (was -23 to -28 dBFS
before AGC).  But every call hits `peak = 0.0 dBFS` -- the +/-1.0
clamp inside `Agc::process` is firing on transient peaks every
call, producing hard digital clipping that the AMBE+2 vocoder then
faithfully encodes.

Lowering `target_dbfs` did not help (next batch of calls):

```
gain_max=15.3 dB peak_in=-3.9  peak_out=0.0
gain_max=17.9 dB peak_in=-7.1  peak_out=-0.8
gain_max=15.7 dB peak_in=-13.2 peak_out=0.0
gain_max=17.8 dB peak_in=-10.4 peak_out=0.0
```

`gain_max` saturates at the 18 dB cap on every loud-input call.

## Why peak-only AGC can't fix this

Voice has crest factor of 12-15 dB (peak above RMS).  The envelope
follower with 10 ms attack has a -3 dB cutoff near 1.6 Hz, so it
tracks something close to RMS, not the instantaneous peak.

**Uncapped regime** (`gain` < `max_gain`):

```
gain     = target / envelope
peak_out = peak_in * gain
         = (envelope * crest) * (target / envelope)
         = target * crest                         <- independent of input!
```

So `peak_out` always lands at `target + crest_factor` regardless
of input level.  To guarantee `peak_out < 0 dBFS` we need
`target_dbfs <= -crest`, i.e. `target <= -13 to -15 dBFS`.  After
AGC, voiced_rms ends up around -25 dBFS -- worse than the soft
talker we started with.

**Capped regime** (`gain` == `max_gain`):

```
peak_out = peak_in + max_gain_db
```

`target_dbfs` falls out of the equation.  Only `max_gain_db`
matters.  Lowering `max_gain_db` cures clipping but reverses the
loudness fix.

There is no (target, max_gain) tuple on a peak-only AGC that gives
both `voiced_rms >= -16 dBFS` AND `peak_out < 0 dBFS`.  They are
antagonistic at the architecture level.

## Why look-ahead

A look-ahead limiter sits after the AGC.  It buffers the signal by
a short delay (~2 ms), runs a peak detector on the undelayed path,
and applies a smoothed gain reduction (GR) that arrives at the
output in time-coincidence with the peak.

Key property: average gain across the call is unchanged.  GR only
fires for the few milliseconds around each transient.  voiced_rms
(an across-call average) is unaffected; peak_out is bounded by the
ceiling.  Both wins simultaneously.

This is why the SOTA stack in `FUTURE_AGC.md` pairs syllabic
compression with a look-ahead limiter -- they handle different
parts of the dynamic-range problem.

## Architecture

```
                   detector    smoother
input -- |.| -- scan_max(N) -- ^ instant attack ----+----- GR
                                v slow release      |
                                                    |
input ---------- delay(N) ---------------------- x gain --- output
                                                    ^
                                                  GR (above)
```

Inside `Agc::process`:

1.  Maintain a ring buffer of N input samples (`f32` post-conversion).
2.  Each iteration: push current `x`, scan buffer for
    `max_future = max(|x|)` over the window.
3.  Required GR: `gr_req = ceiling / (max_future * gain)` if that's
    below 1.0, else 1.0.
4.  Smoother: ramp toward `gr_req` with attack alpha matching the
    lookahead window (so GR reaches minimum exactly when the
    transient hits the output); release back to 1.0 over ~50 ms.
5.  Pop oldest buffered sample; output =
    `oldest * gain * smoother_gr`, then `clamp(+/-1.0)` as a final
    safety belt.

Lookahead window N and smoother attack should match.  Mismatched
attack causes either a discontinuous GR step (audible click) or a
GR that arrives late (peak still clips).

### Numbers

- `N` = 16 samples = 2 ms at 8 kHz.
- Smoother attack = 1.5 ms.
- Smoother release = 50 ms.
- `ceiling` = 1.0 (full scale).  Could be -0.5 dBFS for safety
  margin if we observe quantization-edge clipping in practice.

### Why current `gain` is a fine proxy

We compute `max_future * gain` using the AGC's *current* gain, but
the gain that will apply when we pop the oldest sample is the gain
N samples in the future.  With our 200 ms release time constant,
gain changes by under 1% per 2 ms window -- well below the
limiter's required precision.

If we ever want bit-exact: store `(x, gain_at_push)` pairs in the
ring buffer (trivial; costs 4 extra bytes per slot).

### Pitfalls and decisions

| concern | decision |
|---|---|
| Adds 2 ms latency to FM->DMR | accept; total bridge latency is hundreds of ms |
| First N samples per call output as silence | accept; 2 ms per call, imperceptible |
| Last N samples lost at unkey | accept; 2 ms tail, imperceptible |
| Per-sample O(N) scan | accept at N=16; sliding-window-max is O(1) but unnecessary |
| Limiter on / off | always on when AGC is on; no separate config knob |
| Configurability | hardcoded constants for the first cut; promote to config if tuning demands it |

## Implementation plan

### `dsp/src/agc.rs`

1.  New `const LOOKAHEAD_SAMPLES: usize = 16` (= 2 ms at 8 kHz).
2.  New `const CEILING: f32 = 1.0`.
3.  Add fields to `Agc`:
    - `lookahead_buf: VecDeque<f32>` (capacity = LOOKAHEAD_SAMPLES + 1).
    - `lookahead_gr: f32` (smoothed gain reduction, init 1.0).
    - `lookahead_attack_alpha: f32` (one-pole alpha for 1.5 ms).
    - `lookahead_release_alpha: f32` (one-pole alpha for 50 ms).
4.  In `process()`: push, scan, smooth, pop, apply (see Architecture).
5.  In `reset()`: clear `lookahead_buf`, set `lookahead_gr = 1.0`.
6.  Add `limited_samples: u64` to `AgcSummary` (count of samples
    where `lookahead_gr < 1.0`).
7.  In `take_summary`: drained as before.

Tests:
-  Pure tone above ceiling: `peak_out <= ceiling` after warmup.
-  Pure tone below ceiling: limiter transparent, output unchanged.
-  Single transient on quiet background: peak attenuated to
   ceiling, surrounding samples roughly unchanged after release.
-  `reset()` clears ring buffer (no audio bleed across calls).
-  `limited_samples` increments only when GR < 1.0.

### `bridge/src/usrp.rs`

Extend `emit_call_agc` log line with limiter telemetry:

```
call_agc dir=fm_to_dmr samples=... frozen=... gain_min=... gain_mean=...
         gain_max=... peak_in=... peak_out=... limited=N (P%)
```

`limited` is `s.limited_samples`, `P%` is `100 * limited / samples`.

### `config.example.toml`, `bridge/src/config.rs`, `bridge/src/main.rs`

No changes for the first cut.  Hardcoded constants in `dsp::agc`.

If post-deployment listening shows we want to tune lookahead /
release, promote `lookahead_ms` and `lookahead_release` to
`AgcConfig` (per-direction, like the other params) and plumb
through `AgcParams`.

### `docs/TODO.md`

Remove the step-(2) bullet from the "AGC upgrade path" entry once
this is shipped and listener-confirmed; leave (3) and (4) as
deferred follow-ups.

## Acceptance criteria

A representative listening session must show:

1.  `peak_out < 0 dBFS` on every `call_agc` line for FM->DMR.
2.  `voiced_rms` stays >= -16 dBFS on loud-input source paths.
3.  `limited > 0` on at least some calls (proof the limiter is
    engaging; if it never fires, lookahead is doing no work and
    we have a wiring bug).
4.  Subjective listening: no audible "ducking" or pumping on
    transient-heavy speech.  Listener should not be able to
    distinguish limiter-on from limiter-off except for the
    absence of digital-clip distortion.

## Post-deployment tuning

Once the limiter is in place, the FM->DMR config can be tuned more
aggressively because the limiter handles peak protection:

- Reasonable starting point: `target_dbfs = -6, max_gain_db = 18`.
- If `limited %` over a session is consistently > 5%, the AGC is
  overdriving the limiter; back off `target_dbfs` (more negative).
- If `limited %` is near 0 and listeners still report soft audio,
  raise `target_dbfs`.

The limiter itself should not need tuning unless we observe audible
artifacts.  If we ever do (audible ducking, lost transients, etc.),
the next steps are upgrade-path (3) (RMS-target detector) and (4)
(dual-decay release) from `docs/TODO.md`.
