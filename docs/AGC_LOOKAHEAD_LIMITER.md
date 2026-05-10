# Look-Ahead Limiter for the FM->DMR AGC

Status: implemented in `dsp/src/agc.rs` (shipped v2.7.0).
Background context lives in `docs/FUTURE_AGC.md`; this doc is the
design rationale and in-field result.

**Field result on FM->DMR: AGC is contraindicated.** The limiter
holds the ceiling cleanly (`clipped = 0` everywhere, `peak_out` at
-0.45 dBFS as designed) and `voiced_rms` reaches the loudness
target.  But listener tests at `max_gain_db = 18` showed FM->DMR
audio is audibly WORSE with AGC enabled than with AGC off plus a
static `fm_to_dmr_db = -6` pad before the vocoder.  The mechanism
is per-syllable pumping: between syllables the gain follower
ramps gain UP toward `max_gain_db` chasing the quiet inter-
syllable level, the next syllable arrives loud, the limiter
slams (`gr_min` -3 to -10 dB, `limited %` 30-90%), release
proceeds over 50 ms, cycle repeats.  AMBE+2 encodes the
modulated envelope faithfully and the listener hears compressed-
sounding voice.  Lowering `target_dbfs` from -6 toward -12
softened the effect but did not remove it.  Raising the noise
gate would freeze gain during pauses but also freeze legitimate
quiet voice.

Conclusion: peak-tracking AGC + a hard limiter is the wrong
architecture for vocoder input regardless of how well the
limiter does its narrow job.  Working configuration on FM->DMR:

```
[gain]
fm_to_dmr_db = -6.0

[agc.fm_to_dmr]
enabled = false
```

The limiter itself is correct code and stays in the tree -- it's
still the right answer for any AGC that does run (e.g.,
DMR->FM), and it's the foundation the RMS-target detector
(`docs/TODO.md`, AGC upgrade path) would build on if anyone ever
wants AGC on the encoder side.

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
4.  Smoother: when `gr_req < lookahead_gr` (a peak just entered that
    needs more reduction than is currently applied), **snap**
    `lookahead_gr = gr_req` -- instant attack.  Otherwise (release
    branch), one-pole toward 1.0 with ~50 ms time constant.
5.  Pop oldest buffered sample; output =
    `oldest * gain * lookahead_gr`, then `clamp(+/-1.0)` as a
    safety belt that should never fire under correct operation.

### Why instant attack

A one-pole smoother with attack time matched to the lookahead window
only reaches ~63% of the target value in one time constant (≈74% at
N=16 with τ=N).  The peak emerges from the delay line before GR has
fully settled, and the safety clamp ends up doing the work -- which
is hard digital clipping at full scale, exactly what we set out to
avoid.  An instant snap on the attack branch gets GR fully in place
the same sample the peak enters the buffer; by the time the peak
emerges N samples later, the GR has been at its target value for
N samples.

The cost of instant attack is a 1-sample step in GR on whatever
audio was 2 ms ahead of the triggering peak.  For voice + a
vocoder, the duck happens within the leading edge of the same
syllable and is masked by perceptual fusion -- the listener
doesn't hear it as a click.  If field testing ever shows audible
artifacts, the fallback is a fast one-pole (τ ~ 0.2 ms, settles in
~5 samples), which trades a small unsettled-residual for a softer
edge.

Composition with overlapping peaks: when multiple peaks are in the
buffer, instant attack snaps to the deepest required GR.  When
that worst peak emerges, GR is exactly right for it; subsequent
(less-deep) peaks emerge with GR stricter than they need --
slightly over-attenuating, which is the safe direction.

### Numbers

- `N` = 16 samples = 2 ms at 8 kHz.
- Attack = instantaneous (snap on the attack branch).
- Release = 50 ms one-pole.
- `LIMITER_CEILING` = 0.95 linear = -0.45 dBFS.  Safety margin
  against gain drift between push and pop, float imprecision, and
  any subtle math error.  The trailing `clamp(+/-1.0)` should
  never fire under correct operation; `clipped_samples` in the
  summary counts any time it does and is the definitive failure
  indicator.

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

1.  `const LOOKAHEAD_SAMPLES: usize = 16` (= 2 ms at 8 kHz).
2.  `const LIMITER_CEILING: f32 = 0.95` (= -0.45 dBFS).
3.  Fields on `Agc`:
    - `lookahead_buf: VecDeque<f32>` (capacity = LOOKAHEAD_SAMPLES + 1).
    - `lookahead_gr: f32` (gain reduction, init 1.0).
    - `lookahead_release_alpha: f32` (one-pole alpha for 50 ms).
4.  `process()`: push, scan max, snap GR on attack, one-pole on
    release, pop oldest, apply gain * GR, count clips, emit.
5.  `reset()`: clear `lookahead_buf`, set `lookahead_gr = 1.0`.
6.  Fields on `AgcSummary`:
    - `limited_samples: u64` (count of samples where GR < 1.0).
    - `gr_min: Option<f32>` (deepest GR applied during the call).
    - `clipped_samples: u64` (post-limiter safety-clamp fires;
      should always be 0 under correct operation).

Tests:
-  Pure tone above ceiling: `peak_out <= ceiling`, `clipped == 0`,
   `limited > 0`, `gr_min` recorded.
-  Pure tone below ceiling: limiter transparent, `limited == 0`,
   `gr_min == None`, `clipped == 0`.
-  `reset()` clears ring buffer (no audio bleed across calls).

### `bridge/src/usrp.rs`

Extend `emit_call_agc` log line with limiter telemetry:

```
call_agc dir=... samples=... frozen=... gain_min=... gain_mean=...
         gain_max=... peak_in=... peak_out=... limited=N (P%)
         gr_min=... clipped=N
```

`limited`/`P%` is engagement breadth.  `gr_min` is engagement
depth.  `clipped` is the failure indicator -- non-zero means the
limiter let a peak past the ceiling.

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

A representative session must show:

1.  `clipped = 0` on every `call_agc` line.  This is the
    definitive failure indicator -- non-zero means the limiter
    let a peak past full scale.
2.  `peak_out` at or just under -0.45 dBFS on limited calls;
    below that on unlimited.  (Below -0.45 dBFS but still showing
    `peak_out` near 0.0 dBFS would point at a different bug.)
3.  `voiced_rms` stays >= -16 dBFS on loud-input source paths
    once `max_gain_db` is back at 18.
4.  `limited > 0` and `gr_min` populated on at least some calls
    (proof the limiter is engaging; if it never fires, lookahead
    is doing no work and we have a wiring bug).
5.  Subjective listening: no audible "ducking" or pumping on
    transient-heavy speech.

## In-field test plan (one deb cycle)

Build the deb, install, restart the service.  Then iterate
config-only:

1.  `max_gain_db = 8` on both directions (current value, known
    clean for DMR->FM).  Make 5-10 calls each direction.  Check
    `clipped = 0` everywhere; sanity-check `gr_min` values.
2.  Listen on FM side for speech-onset clicks.  If clean, proceed.
3.  Bump `max_gain_db = 12`, restart, repeat.
4.  Bump `max_gain_db = 18` (the loudness target), restart,
    repeat.  voiced_rms should reach -14 to -16 dBFS on FM->DMR.

If any step shows `clipped > 0` or audible artifacts, stop and
report -- the parameter knobs in the next iteration will be
guided by which step broke.

## Post-deployment tuning

Once the limiter is confirmed working at `max_gain_db = 18`:

- If `limited %` over a session is consistently > 5%, the AGC is
  overdriving the limiter on average; back off `target_dbfs` (more
  negative).
- If `limited %` is near 0 and listeners still report soft audio,
  raise `target_dbfs`.

The limiter itself should not need tuning unless we observe
audible artifacts.  Fallback for clicks: replace instant attack
with a fast one-pole (alpha for 200 us).  Fallback for residual
softness despite `limited %` low: upgrade-path (2) (RMS-target
detector) from `docs/TODO.md`.
