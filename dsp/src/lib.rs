//! Audio DSP primitives shared across the workspace.

mod db;
pub use db::dB;

#[cfg(test)]
mod tests;
