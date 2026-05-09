//! Audio DSP primitives shared across the workspace.

pub mod agc;
pub mod biquad;
mod db;
pub mod levels;

// Carve-out from the no-re-export rule: `dB` lives at the crate
// root since callers use it everywhere.
pub use db::dB;

#[cfg(test)]
mod tests;
