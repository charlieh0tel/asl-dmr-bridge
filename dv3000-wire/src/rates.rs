//! DV3000 RATEP control words for known AMBE+2 rates.
//!
//! Each entry is the 12-byte RCW0..RCW5 payload of a `PKT_CONTROL`
//! RATEP request (field_id 0x0A).  Verified against the DVSI
//! AMBE-3000R datasheet and serialDV reference values where possible;
//! generic ones are labeled by RCW0 high/low/total bits.

/// Rate index 23: D-Star (3600x2400).
pub const RATEP_DSTAR: [u8; 12] = [
    0x01, 0x30, 0x07, 0x63, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x48,
];

/// Rate index 33: DMR / P25 half-rate, 2450 voice + 1150 FEC.
pub const RATEP_DMR: [u8; 12] = [
    0x04, 0x31, 0x07, 0x54, 0x24, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6F, 0x48,
];

/// Rate index 34: raw 2450 voice, 0 FEC.
pub const RATEP_RAW: [u8; 12] = [
    0x04, 0x31, 0x07, 0x54, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x70, 0x31,
];

/// Rate index 35: 3400 / 2250 voice + 1150 FEC.
pub const RATEP_IDX35: [u8; 12] = [
    0x04, 0x2D, 0x07, 0x54, 0x24, 0x00, 0x00, 0x00, 0x00, 0x00, 0x79, 0x44,
];

/// Catalog of known rates for log decoration.  `name` is short enough
/// to embed in a single log line; an unmatched payload prints as raw
/// hex at the call site.
pub const KNOWN_RATES: &[(&str, [u8; 12])] = &[
    ("DMR / P25 half-rate (idx 33)", RATEP_DMR),
    ("raw 2450 voice (idx 34)", RATEP_RAW),
    ("D-Star (idx 23)", RATEP_DSTAR),
    ("rate idx 35 (3400/2250/1150)", RATEP_IDX35),
];

/// Look up a human-readable name for a 12-byte RATEP payload, if it
/// matches a known rate.
pub fn rate_name(payload: &[u8; 12]) -> Option<&'static str> {
    KNOWN_RATES
        .iter()
        .find(|(_, rcws)| rcws == payload)
        .map(|(name, _)| *name)
}
