//! PCM diagnostic helpers shared across the workspace.
//!
//! Domain-agnostic: this crate has no DMR / vocoder semantics, just
//! generic 8 kHz mono int16 PCM utilities (level metering, WAV
//! file writing).  Callers in dmr-wire and bridge use it for
//! per-call summary stats and per-call diagnostic capture.

pub mod biquad;
pub mod levels;
pub mod wav;
