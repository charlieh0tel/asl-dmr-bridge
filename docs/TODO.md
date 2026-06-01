# TODO

Tracked-but-deferred work.  Each entry is sized roughly: a single
line of context + the rough effort + the trigger that would make it
worth picking up.

## Maybe

Plausible but value-uncertain; defer until a real-world need
appears.

- **`ambe-tool` encode/decode/roundtrip binary.**  Swiss-Army CLI
  with three subcommands: `encode --encoder X --in audio.wav
  --out utt.ambe`, `decode --decoder X --in utt.ambe --out out.wav`,
  `roundtrip --encoder X --decoder Y`.  `roundtrip` is two sequential
  steps (encode to temp, then decode) so thumbdv->thumbdv works
  without serial double-open.  Enables the full 9-cell
  encoder x decoder grid.

- **Pre-encode LP 3000 -> 3400 Hz A/B.**  AMBE+2 at DMR rate is
  documented for 250-3400 Hz; current LP at 3000 Hz is the
  conservative P25/DMR pre-emphasis target and loses sibilance
  above 3 kHz.  Stretch only if a listener reports dull FM-side
  audio; the codec at half-rate may not preserve the extra 400 Hz
  anyway.

- **OpenBridge peering.**  Worth picking up for multi-TG peering
  on one instance, peering to non-BM networks (FreeDMR, TGIF),
  or server-to-server linking.  New wire crate, ~500 LOC, no
  impact on the FM<->DMR core.  Trigger: an actual use case.

- **XLX reflector support.**  Most XLXd accepts MMDVM-Homebrew on
  the DMR side, so this is likely `network.profile = "xlx"` plus
  TG-field-as-module-letter.  Investigate protocol; if ~50 LOC
  plus config, just do it.

- **AGC upgrade path; only if anyone wants FM->DMR AGC again.**
  AGC on FM->DMR has been ruled out by listener tests
  (`docs/AGC_LOOKAHEAD_LIMITER.md`, Field result section): the
  peak-tracking gain follower + limiter combination pumps at
  speech-syllable rate in a way the AMBE+2 vocoder encodes
  audibly badly, even though the in-bridge metrics
  (`clipped = 0`, `peak_out` at ceiling, `voiced_rms` at target)
  look clean.  Static `fm_to_dmr_db` pad before the vocoder is
  the working answer.  AGC on DMR->FM remains useful because the
  listener is downstream of the codec.
  If FM->DMR AGC is ever wanted again, the only path that can
  work is switching the level detector from peak (|x|) to
  RMS / syllabic envelope so the gain follower stops chasing
  inter-syllable pauses.  That's a meaningful DSP change
  (~100 LOC + careful tuning), not a small tweak.  Optional
  follow-on: dual-decay release.

