//! Bridge-presentation types for inbound DMR call metadata.
//!
//! Lives outside `dmr-wire` because the wire crate is L2 / FEC /
//! burst layout -- this is application-layer "what does the bridge
//! tell the FM side about a call."

use std::sync::Arc;

use dmr_types::ColorCode;
use dmr_types::Slot;
use dmr_types::SubscriberId;
use dmr_types::Talkgroup;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct CallMetadata {
    /// On-air subscriber ID of the talker (DMRD `src_id`).
    pub dmr_id: SubscriberId,
    /// Talkgroup the call is on (group call) or addressee (private).
    pub tg: Talkgroup,
    pub slot: Slot,
    pub cc: ColorCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Optional resolver from on-air DMR ID to (callsign, first-name).
/// `dmr-wire`'s voice task takes one of these to enrich `CallMetadata`
/// without a direct dependency on `dmr-subscriber` or any specific
/// CSV format.
pub type CallsignLookup = Arc<dyn Fn(u32) -> Option<(String, String)> + Send + Sync>;

/// Channel event emitted by the voice task at call boundaries.
/// `Call` carries fully-built metadata for a new (or refreshed) call;
/// `Clear` signals end-of-call.  The bridge layer translates these
/// to USRP TEXT frames (JSON encoding for `Call`, "{}" for `Clear`).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum MetaEvent {
    Call(CallMetadata),
    Clear,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_metadata_json_shape_no_lookup() {
        // No callsign lookup -> call/name are None -> omitted from
        // the JSON via skip_serializing_if.  Locks the on-the-wire
        // shape consumers depend on; a quiet schema change would
        // break dialplan parsers silently.
        let m = CallMetadata {
            dmr_id: SubscriberId::try_from(1234567).unwrap(),
            tg: Talkgroup::try_from(91).unwrap(),
            slot: Slot::One,
            cc: ColorCode::try_from(1).unwrap(),
            call: None,
            name: None,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(s, r#"{"dmr_id":1234567,"tg":91,"slot":1,"cc":1}"#);
    }

    #[test]
    fn call_metadata_json_shape_with_lookup() {
        // call/name present when the lookup hit; appear after the
        // bare DMR fields, in order.
        let m = CallMetadata {
            dmr_id: SubscriberId::try_from(1234567).unwrap(),
            tg: Talkgroup::try_from(91).unwrap(),
            slot: Slot::One,
            cc: ColorCode::try_from(1).unwrap(),
            call: Some("N0CALL".into()),
            name: Some("Test".into()),
        };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(
            s,
            r#"{"dmr_id":1234567,"tg":91,"slot":1,"cc":1,"call":"N0CALL","name":"Test"}"#
        );
    }
}
