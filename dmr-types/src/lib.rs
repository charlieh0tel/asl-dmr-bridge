//! Shared on-air DMR ID newtypes.
//!
//! Foundational layer underneath dmr-wire (L2/burst/FEC) and
//! dmr-events (call-metadata events): the IDs that cross every
//! layer's boundaries.  Each type validates its range via `TryFrom`;
//! `Deserialize` delegates to `TryFrom` so config-load and
//! programmatic construction share one source of truth.
//!
//! `Frequency` and `Callsign` deliberately stay in the binary --
//! both encode RPTC wire-format details (9-digit zero-padded
//! decimal, 8-char ASCII) that aren't shared across crates.

use std::fmt;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use thiserror::Error;

/// Wire length of the Homebrew protocol's repeater_id field.
pub const REPEATER_ID_WIRE_LEN: usize = 4;

/// Maximum value that fits in the 24-bit DMRD src_id/dst_id wire fields.
const MAX_24BIT: u32 = 0x00FF_FFFF;
const MAX_COLOR_CODE: u8 = 15;
const DEFAULT_COLOR_CODE: u8 = 1;

/// Validation errors from `TryFrom` and `Deserialize` on this crate's
/// newtypes.  One enum across types so callers can match without
/// importing per-type error names.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum InvalidValue {
    #[error("DMR ID must be non-zero")]
    DmrIdZero,
    #[error("subscriber_id must be non-zero")]
    SubscriberIdZero,
    #[error("subscriber_id must fit in 24 bits, got {0}")]
    SubscriberIdTooLarge(u32),
    #[error("talkgroup must be non-zero")]
    TalkgroupZero,
    #[error("talkgroup must fit in 24 bits, got {0}")]
    TalkgroupTooLarge(u32),
    #[error("color code must be 0-{MAX_COLOR_CODE}, got {0}")]
    ColorCodeTooLarge(u8),
    #[error("slot must be 1 or 2, got {0}")]
    SlotInvalid(u8),
}

/// DMR Homebrew repeater identity (32-bit).
///
/// Carries the full 32-bit range because the Homebrew protocol's
/// `repeater_id` header field is 4 bytes wide and Brandmeister hotspot
/// IDs (e.g. AI6KG-01 = 310770201) legitimately exceed 24 bits.
///
/// The 24-bit on-air `src_id`/`dst_id` fields (DMRD body, voice LC)
/// are a DIFFERENT logical quantity -- use `SubscriberId` there.  For
/// hotspots with `repeater_id > 2^24`, the TX path must source the
/// 24-bit subscriber ID from a separately registered subscriber ID;
/// truncating `repeater_id` would alias onto an unrelated DMR user.
///
/// `to_be_bytes_3` enforces that invariant with a deliberate `assert!`
/// so a mis-routed repeater_id crashes loudly in test/dev rather than
/// silently emitting impostor traffic in production.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DmrId(u32);

impl DmrId {
    pub fn as_u32(self) -> u32 {
        self.0
    }

    pub fn to_be_bytes(self) -> [u8; REPEATER_ID_WIRE_LEN] {
        self.0.to_be_bytes()
    }

    /// Encode as 3 big-endian bytes for DMRD `src_id`/`dst_id`.
    /// Panics if the ID exceeds 24 bits -- intentional; see type doc.
    pub fn to_be_bytes_3(self) -> [u8; 3] {
        assert!(
            self.0 <= MAX_24BIT,
            "DMR ID {} exceeds 24-bit max for 3-byte encoding",
            self.0
        );
        let b = self.0.to_be_bytes();
        [b[1], b[2], b[3]]
    }
}

impl TryFrom<u32> for DmrId {
    type Error = InvalidValue;
    fn try_from(v: u32) -> Result<Self, InvalidValue> {
        if v == 0 {
            Err(InvalidValue::DmrIdZero)
        } else {
            Ok(DmrId(v))
        }
    }
}

impl fmt::Display for DmrId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<'de> Deserialize<'de> for DmrId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::try_from(u32::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl Serialize for DmrId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u32(self.0)
    }
}

/// On-air DMR subscriber ID (24-bit), used as the `src_id` in the
/// DMRD wire body and the embedded LC.  Distinct from `DmrId`
/// (32-bit Homebrew repeater identity): BM hotspot IDs like
/// 310770201 exceed 24 bits and would alias onto an unrelated
/// subscriber if used here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriberId(u32);

impl SubscriberId {
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

impl TryFrom<u32> for SubscriberId {
    type Error = InvalidValue;
    fn try_from(v: u32) -> Result<Self, InvalidValue> {
        if v == 0 {
            Err(InvalidValue::SubscriberIdZero)
        } else if v > MAX_24BIT {
            Err(InvalidValue::SubscriberIdTooLarge(v))
        } else {
            Ok(SubscriberId(v))
        }
    }
}

impl fmt::Display for SubscriberId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<'de> Deserialize<'de> for SubscriberId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::try_from(u32::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl Serialize for SubscriberId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u32(self.0)
    }
}

/// DMR talkgroup ID.  Non-zero, fits in 24 bits (DMRD dst_id width).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Talkgroup(u32);

impl Talkgroup {
    pub fn as_u32(self) -> u32 {
        self.0
    }

    pub fn to_be_bytes_3(self) -> [u8; 3] {
        // Range enforced at construction, so direct slice is safe.
        let b = self.0.to_be_bytes();
        [b[1], b[2], b[3]]
    }
}

impl TryFrom<u32> for Talkgroup {
    type Error = InvalidValue;
    fn try_from(v: u32) -> Result<Self, InvalidValue> {
        if v == 0 {
            Err(InvalidValue::TalkgroupZero)
        } else if v > MAX_24BIT {
            Err(InvalidValue::TalkgroupTooLarge(v))
        } else {
            Ok(Talkgroup(v))
        }
    }
}

impl fmt::Display for Talkgroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<'de> Deserialize<'de> for Talkgroup {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::try_from(u32::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Talkgroup {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u32(self.0)
    }
}

/// DMR color code (0-15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ColorCode(u8);

impl ColorCode {
    pub fn value(self) -> u8 {
        self.0
    }
}

impl Default for ColorCode {
    fn default() -> Self {
        ColorCode(DEFAULT_COLOR_CODE)
    }
}

impl TryFrom<u8> for ColorCode {
    type Error = InvalidValue;
    fn try_from(v: u8) -> Result<Self, InvalidValue> {
        if v > MAX_COLOR_CODE {
            Err(InvalidValue::ColorCodeTooLarge(v))
        } else {
            Ok(ColorCode(v))
        }
    }
}

impl fmt::Display for ColorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<'de> Deserialize<'de> for ColorCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::try_from(u8::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl Serialize for ColorCode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(self.0)
    }
}

/// DMR timeslot (TS1 or TS2).  Wire-format helpers (flag-byte
/// packing) live in `dmr-wire`; this enum is the cross-crate
/// identifier shared by config, metadata, and protocol layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    One,
    Two,
}

impl Slot {
    /// Wire/Display value: 1 for TS1, 2 for TS2.
    pub fn as_u8(self) -> u8 {
        match self {
            Slot::One => 1,
            Slot::Two => 2,
        }
    }

    /// Zero-indexed slot number for protocol flags.
    pub fn index(self) -> u8 {
        match self {
            Slot::One => 0,
            Slot::Two => 1,
        }
    }
}

impl TryFrom<u8> for Slot {
    type Error = InvalidValue;
    fn try_from(v: u8) -> Result<Self, InvalidValue> {
        match v {
            1 => Ok(Slot::One),
            2 => Ok(Slot::Two),
            _ => Err(InvalidValue::SlotInvalid(v)),
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_u8())
    }
}

impl<'de> Deserialize<'de> for Slot {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Self::try_from(u8::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl Serialize for Slot {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(self.as_u8())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct W<T> {
        v: T,
    }

    fn parse<T: serde::de::DeserializeOwned>(toml: &str) -> Result<T, toml::de::Error> {
        toml::from_str::<W<T>>(toml).map(|w| w.v)
    }

    #[test]
    fn dmr_id_valid() {
        let id = DmrId::try_from(1234567).unwrap();
        assert_eq!(id.to_be_bytes(), [0x00, 0x12, 0xD6, 0x87]);
    }

    #[test]
    fn dmr_id_zero_rejected() {
        assert_eq!(DmrId::try_from(0), Err(InvalidValue::DmrIdZero));
        assert!(parse::<DmrId>("v = 0").is_err());
    }

    #[test]
    fn dmr_id_large_accepted() {
        // 310770201 is a real BM hotspot ID — exceeds 24-bit but valid for 4-byte fields.
        let id = DmrId::try_from(310770201).unwrap();
        assert_eq!(id.to_be_bytes(), 310770201u32.to_be_bytes());
    }

    #[test]
    #[should_panic(expected = "exceeds 24-bit")]
    fn dmr_id_to_be_bytes_3_panics_on_large() {
        DmrId::try_from(MAX_24BIT + 1).unwrap().to_be_bytes_3();
    }

    #[test]
    fn dmr_id_to_be_bytes_3() {
        let id = DmrId::try_from(1234567).unwrap();
        assert_eq!(id.to_be_bytes_3(), [0x12, 0xD6, 0x87]);
    }

    #[test]
    fn subscriber_id_zero_rejected() {
        assert_eq!(
            SubscriberId::try_from(0),
            Err(InvalidValue::SubscriberIdZero)
        );
    }

    #[test]
    fn subscriber_id_too_large_rejected() {
        assert_eq!(
            SubscriberId::try_from(MAX_24BIT + 1),
            Err(InvalidValue::SubscriberIdTooLarge(MAX_24BIT + 1))
        );
    }

    #[test]
    fn talkgroup_valid() {
        let tg = Talkgroup::try_from(3100).unwrap();
        assert_eq!(tg.to_be_bytes_3(), [0x00, 0x0C, 0x1C]);
    }

    #[test]
    fn talkgroup_zero_rejected() {
        assert_eq!(Talkgroup::try_from(0), Err(InvalidValue::TalkgroupZero));
    }

    #[test]
    fn talkgroup_too_large_rejected() {
        assert!(matches!(
            Talkgroup::try_from(MAX_24BIT + 1),
            Err(InvalidValue::TalkgroupTooLarge(_))
        ));
    }

    #[test]
    fn color_code_valid() {
        let cc = ColorCode::try_from(15).unwrap();
        assert_eq!(cc.value(), 15);
    }

    #[test]
    fn color_code_too_large_rejected() {
        assert_eq!(
            ColorCode::try_from(16),
            Err(InvalidValue::ColorCodeTooLarge(16))
        );
    }

    #[test]
    fn slot_valid() {
        assert_eq!(Slot::try_from(1).unwrap().index(), 0);
        assert_eq!(Slot::try_from(2).unwrap().index(), 1);
    }

    #[test]
    fn slot_invalid_rejected() {
        assert!(matches!(
            Slot::try_from(0),
            Err(InvalidValue::SlotInvalid(_))
        ));
        assert!(matches!(
            Slot::try_from(3),
            Err(InvalidValue::SlotInvalid(_))
        ));
    }
}
