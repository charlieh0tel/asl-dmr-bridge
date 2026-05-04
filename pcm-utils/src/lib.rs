//! PCM helpers shared across the workspace: level metering, WAV
//! file writing, IIR biquad primitives, and bridge-specific filter
//! factories that build on the primitives.  Callers in dmr-wire and
//! bridge use it for per-call summary stats, per-call diagnostic
//! capture, and the FM->DMR pre-encode filter.

pub mod biquad;
pub mod levels;
pub mod wav;
