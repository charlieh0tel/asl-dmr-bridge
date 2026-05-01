//! Wire types matching the Brandmeister Halligan API v2 schemas.
//!
//! Field names mirror the API's JSON exactly (camelCase, including the
//! upstream typo `lastKownMaster`).  Unknown fields on responses are
//! ignored so additive API changes don't break existing callers.

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;

/// Brandmeister DMR timeslot identifier on the wire (1 or 2).
pub type Slot = u8;

/// Brandmeister DMR talkgroup ID (24-bit on-air, but BM serializes as
/// integer up to 32 bits).
pub type TalkgroupId = u32;

/// Brandmeister device (peer / hotspot / repeater) ID.  Hotspot IDs
/// can exceed 24 bits (e.g., 9-digit US-style hotspot suffix).
pub type DeviceId = u32;

/// Static talkgroup subscription returned by GET /device/{id}/talkgroup
/// and GET /device/{id}/profile (under `staticSubscriptions`).
///
/// BM serializes integer IDs as JSON strings on this endpoint
/// (`{"talkgroup":"91","slot":"1","repeaterid":"310770201"}`).  The
/// flexible deserializers below accept either form to tolerate
/// upstream type drift.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StaticTalkgroup {
    #[serde(deserialize_with = "de_u32_flexible")]
    pub talkgroup: TalkgroupId,
    /// Repeater (device) ID this static is bound to.  Always equal to
    /// the `{id}` path parameter when the response comes from the
    /// device endpoints, but BM includes it explicitly.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_opt_u32_flexible"
    )]
    pub repeaterid: Option<DeviceId>,
    #[serde(deserialize_with = "de_u8_flexible")]
    pub slot: Slot,
}

/// Accept a u32 from JSON number or string.  Tolerates BM's habit of
/// returning numeric IDs as strings on some endpoints.
fn de_u32_flexible<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    use serde::de::Error;
    match serde_json::Value::deserialize(d)? {
        serde_json::Value::Number(n) => n
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| D::Error::custom("number out of u32 range")),
        serde_json::Value::String(s) => s.parse::<u32>().map_err(D::Error::custom),
        other => Err(D::Error::custom(format!(
            "expected number or string, got {other}"
        ))),
    }
}

fn de_u8_flexible<'de, D: Deserializer<'de>>(d: D) -> Result<u8, D::Error> {
    de_u32_flexible(d).and_then(|n| u8::try_from(n).map_err(serde::de::Error::custom))
}

fn de_opt_u32_flexible<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    use serde::de::Error;
    match Option::<serde_json::Value>::deserialize(d)? {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Number(n)) => Ok(n.as_u64().and_then(|n| u32::try_from(n).ok())),
        Some(serde_json::Value::String(s)) => s.parse::<u32>().map(Some).map_err(D::Error::custom),
        Some(other) => Err(D::Error::custom(format!(
            "expected number, string, or null, got {other}"
        ))),
    }
}

/// Body for POST /device/{id}/talkgroup (add static).
///
/// Note: the OpenAPI spec at api.brandmeister.network/api-docs lists
/// the body field as `talkgroup`, but the live API actually requires
/// `group` (matching the `{group}` path parameter on the DELETE
/// counterpart) and returns HTTP 455 "The group field is required."
/// otherwise.  The Rust field stays `talkgroup` for clarity; serde
/// renames it on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddStaticBody {
    #[serde(rename = "group")]
    pub talkgroup: TalkgroupId,
    pub slot: Slot,
}

/// Device info returned by GET /device/{id}.  Every field except `id`
/// and `callsign` is optional in practice -- BM omits unset fields.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Device {
    pub id: DeviceId,
    pub callsign: String,
    /// Master ID the device was last seen on (note: upstream JSON
    /// field is `lastKownMaster`, a typo preserved for compatibility).
    #[serde(rename = "lastKownMaster", default)]
    pub last_known_master: Option<u32>,
    #[serde(default)]
    pub linkname: Option<String>,
    #[serde(default)]
    pub hardware: Option<String>,
    #[serde(default)]
    pub firmware: Option<String>,
    #[serde(default)]
    pub tx: Option<String>,
    #[serde(default)]
    pub rx: Option<String>,
    #[serde(default)]
    pub colorcode: Option<u8>,
    /// Link state of the device.  BM uses this as a small enum but
    /// doesn't document the values stably; expose as raw integer.
    #[serde(default)]
    pub status: Option<u32>,
    #[serde(default)]
    pub lat: Option<f32>,
    #[serde(default)]
    pub lng: Option<f32>,
    #[serde(default)]
    pub city: Option<String>,
    #[serde(default)]
    pub website: Option<String>,
    #[serde(default)]
    pub pep: Option<u32>,
    #[serde(default)]
    pub agl: Option<u32>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Device profile (GET /device/{id}/profile): aggregates static,
/// dynamic, timed, blocked, and cluster subscriptions into one
/// response.
///
/// BM serializes `dynamicSubscriptions`, `timedSubscriptions`,
/// `blockedGroups`, and `cluster` as `object` when empty (no
/// stable item shape documented), so they're surfaced as raw
/// `serde_json::Value` for now -- callers can dig deeper as
/// needed without the lib over-committing to a structure that may
/// drift.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct DeviceProfile {
    #[serde(rename = "staticSubscriptions", default)]
    pub static_subscriptions: Vec<StaticTalkgroup>,
    #[serde(rename = "dynamicSubscriptions", default)]
    pub dynamic_subscriptions: serde_json::Value,
    #[serde(rename = "timedSubscriptions", default)]
    pub timed_subscriptions: serde_json::Value,
    #[serde(rename = "blockedGroups", default)]
    pub blocked_groups: serde_json::Value,
    #[serde(default)]
    pub cluster: serde_json::Value,
}

/// Talkgroup metadata returned by GET /talkgroup/{id}.  BM includes
/// many optional fields (description, country, language); we surface
/// the stable ones.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[non_exhaustive]
pub struct Talkgroup {
    pub id: TalkgroupId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_talkgroup_decodes_with_repeaterid() {
        let json = r#"{"talkgroup": 91, "repeaterid": 310770201, "slot": 1}"#;
        let s: StaticTalkgroup = serde_json::from_str(json).unwrap();
        assert_eq!(s.talkgroup, 91);
        assert_eq!(s.repeaterid, Some(310770201));
        assert_eq!(s.slot, 1);
    }

    #[test]
    fn static_talkgroup_decodes_without_repeaterid() {
        // /device/{id}/talkgroup omits repeaterid (it's implied by the
        // path); decoding must not require the field.
        let json = r#"{"talkgroup": 9990, "slot": 2}"#;
        let s: StaticTalkgroup = serde_json::from_str(json).unwrap();
        assert!(s.repeaterid.is_none());
        assert_eq!(s.slot, 2);
    }

    #[test]
    fn static_talkgroup_decodes_stringly_typed_ids() {
        // Live BM /device/{id}/talkgroup wraps numeric fields as
        // strings: {"talkgroup":"91","slot":"1","repeaterid":"310770201"}.
        // Flexible deserializers must accept this form.
        let json = r#"{"talkgroup": "91", "slot": "1", "repeaterid": "310770201"}"#;
        let s: StaticTalkgroup = serde_json::from_str(json).unwrap();
        assert_eq!(s.talkgroup, 91);
        assert_eq!(s.slot, 1);
        assert_eq!(s.repeaterid, Some(310770201));
    }

    #[test]
    fn add_static_body_uses_group_wire_field() {
        // Live BM rejects {"talkgroup": ...} with HTTP 455
        // "The group field is required."; only `group` works on the
        // wire even though the OpenAPI doc names the schema Talkgroup.
        let body = AddStaticBody {
            talkgroup: 91,
            slot: 1,
        };
        let s = serde_json::to_string(&body).unwrap();
        assert_eq!(s, r#"{"group":91,"slot":1}"#);
    }

    #[test]
    fn device_decodes_with_typo_field_name() {
        // Upstream JSON field is `lastKownMaster` (typo).  The serde
        // rename keeps our Rust idiom while accepting the typo on the
        // wire.  Fail loudly if BM ever fixes it.
        let json = r#"{"id": 310770201, "callsign": "AI6KG", "lastKownMaster": 3104}"#;
        let d: Device = serde_json::from_str(json).unwrap();
        assert_eq!(d.id, 310770201);
        assert_eq!(d.callsign, "AI6KG");
        assert_eq!(d.last_known_master, Some(3104));
    }

    #[test]
    fn device_ignores_unknown_fields() {
        // Additive API changes shouldn't break decoding.
        let json = r#"{"id": 1, "callsign": "X", "totallyNew": "value"}"#;
        let d: Device = serde_json::from_str(json).unwrap();
        assert_eq!(d.id, 1);
    }

    #[test]
    fn device_profile_decodes_with_empty_objects() {
        // BM serializes empty subscription buckets as {} (not [] or
        // null).  staticSubscriptions is always an array per spec.
        let json = r#"{
            "staticSubscriptions": [{"talkgroup": 91, "repeaterid": 1, "slot": 1}],
            "dynamicSubscriptions": {},
            "timedSubscriptions": {},
            "blockedGroups": {},
            "cluster": {}
        }"#;
        let p: DeviceProfile = serde_json::from_str(json).unwrap();
        assert_eq!(p.static_subscriptions.len(), 1);
        assert_eq!(p.static_subscriptions[0].talkgroup, 91);
    }
}
