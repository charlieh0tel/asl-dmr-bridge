# AGC Design Notes for asl-dmr-bridge

## Scope

DSP-level audio AGC for the FM <-> DMR voice path: compression and
limiting between the USRP/ASL3 PCM seam and the AMBE+2 vocoder.
Out of scope:

- Receiver-side RF/IF gain control: handled inside the FM radio
  (limiting amplifier chain ahead of the discriminator).
- CTCSS/DCS injection: handled by ASL3 upstream of the bridge.
- Pre-emphasis / flat-audio architecture: there is no FM modulator
  in the bridge path; PCM goes straight into the vocoder.
- Transmitter deviation feedback: DMR is digital; no deviation
  envelope to control.

## Modern DSP-Based Approach

### Syllabic envelope tracking

Track the speech envelope at syllable rates rather than the
instantaneous peak amplitude.  Typical parameters:

- **Attack:** 3-10 ms (catch consonant bursts).
- **Release:** 50-150 ms (track syllable-level amplitude variation
  without audible pumping).

Anything shorter than ~200 ms release is technically "syllabic"
[Souza 2002], but 50-150 ms is the practical sweet spot.

### Look-ahead limiting

Buffer the signal by a short delay (1-5 ms typical) and run the
level detector on the undelayed path.  Gain reduction can then be
applied in time coincidence with a peak, before it causes clipping,
eliminating the attack-time distortion of conventional limiters
[Giannoulis et al. 2012; Dorrough US 5,737,434, expired].

### Dual-decay release

Branched / decoupled peak detector with two release time constants:
short for brief transients (avoids "sucking" after a peak), long
for sustained compression (keeps the noise floor from rising
audibly during quiet passages) [Giannoulis et al. 2012].

## Where this leaves asl-dmr-bridge

Current `Agc` (`dsp/src/agc.rs`):

| dimension        | now                 | target                |
|------------------|---------------------|-----------------------|
| envelope         | peak (\|x\|)        | syllabic / RMS        |
| attack / release | 10 / 200 ms         | 10 / ~120 ms          |
| limiter          | hard +/-1.0 clamp   | look-ahead 1-3 ms     |
| dual-decay       | no                  | optional, after RMS   |

AMBE+2 is sensitive to drive level; the encoder produces audibly
softer output when fed quiet PCM, and the decoder's output level
varies with the source talker.  The practical mitigation is to
normalize PCM to a consistent level on each direction before the
codec sees it; see `[agc.dmr_to_fm]` and `[agc.fm_to_dmr]` in
`config.example.toml`.

The upgrade order in `docs/TODO.md` ("AGC upgrade path") follows
the table above; defer until live `call_agc` summaries show
saturation or pumping that target/`max_gain_db` tuning cannot fix.

## References

- Souza, P. E. (2002). Effects of compression on speech acoustics,
  intelligibility, and sound quality. *Trends in Amplification*,
  6(4), 131-165.  https://doi.org/10.1177/108471380200600402

- Stone, M. A., & Moore, B. C. J. (1992). Syllabic compression:
  effective compression ratios for signals modulated at different
  rates. *British Journal of Audiology*, 26(6), 351-361.
  https://doi.org/10.3109/03005369209076659

- Giannoulis, D., Massberg, M., & Reiss, J. D. (2012). Digital
  dynamic range compressor design - a tutorial and analysis.
  *Journal of the Audio Engineering Society*, 60(6), 399-408.
  https://www.eecs.qmul.ac.uk/~josh/documents/2012/GiannoulisMassbergReiss-dynamicrangecompression-JAES2012.pdf
  (Canonical open-literature reference for look-ahead limiter and
  decoupled peak-detector implementation.)

- Dorrough, M. C. (1998). Multi-band audio compressor with
  look-ahead clipper.  US Patent 5,737,434 (expired ~2015).
  https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/5737434
