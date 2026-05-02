//! Bridge-local newtypes for fields that encode RPTC wire-format
//! details.  IDs that cross crate boundaries (DmrId, SubscriberId,
//! Talkgroup, ColorCode, Slot) live in `dmr-types`.

use std::fmt;

use serde::Deserialize;

const MAX_CALLSIGN_LEN: usize = 8;
const FREQ_DIGITS: usize = 9;

/// Frequency in Hz. Must be non-zero and fit in 9 decimal digits
/// (0 <= v <= 999_999_999) for the RPTC config packet wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Frequency(u32);

const MAX_FREQ_HZ: u32 = 999_999_999;

impl Frequency {
    /// Format as 9 zero-padded ASCII digits for RPTC wire format.
    pub(crate) fn as_rptc_digits(self) -> String {
        format!("{:0FREQ_DIGITS$}", self.0)
    }
}

impl<'de> Deserialize<'de> for Frequency {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = u32::deserialize(deserializer)?;
        if v == 0 {
            return Err(serde::de::Error::custom("frequency must be non-zero"));
        }
        if v > MAX_FREQ_HZ {
            return Err(serde::de::Error::custom(format!(
                "frequency must fit in {FREQ_DIGITS} digits (max {MAX_FREQ_HZ})"
            )));
        }
        Ok(Frequency(v))
    }
}

impl fmt::Display for Frequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Amateur radio callsign. ASCII alphanumeric only, max 8 chars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Callsign(String);

impl<'de> Deserialize<'de> for Callsign {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s.is_empty() {
            return Err(serde::de::Error::custom("callsign must not be empty"));
        }
        if s.len() > MAX_CALLSIGN_LEN {
            return Err(serde::de::Error::custom(format!(
                "callsign must be at most {MAX_CALLSIGN_LEN} chars, got {}",
                s.len()
            )));
        }
        if !s.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(serde::de::Error::custom(
                "callsign must be ASCII alphanumeric only",
            ));
        }
        Ok(Callsign(s))
    }
}

impl Callsign {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Callsign {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
