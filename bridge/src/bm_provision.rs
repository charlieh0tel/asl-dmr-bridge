//! Brandmeister Halligan API integration: startup-time peer-profile
//! log and optional pure-set static-talkgroup reconciliation.
//!
//! Read path runs unconditionally (one anonymous GET, no auth needed)
//! so the bridge log surfaces what BM thinks our peer is subscribed
//! to -- the shape of question that took a 2-minute live diagnostic
//! to answer the first time we asked it.
//!
//! Write path runs only when [brandmeister_api] supplies an api_key
//! AND at least one `static_talkgroups_tsN` list.  The semantics are
//! pure-set: declared list = final state.  Omitting a slot leaves it
//! untouched; `[]` reduces it to empty.
//!
//! All failures are non-fatal: a bridge that can't reach the API
//! still functions for voice traffic, and we'd rather degrade with a
//! warning than refuse to start.

use std::time::Duration;

use brandmeister_api::client::Client;
use dmr_types::DmrId;
use dmr_types::Slot;
use dmr_types::Talkgroup;
use secrecy::ExposeSecret;
use tokio_util::sync::CancellationToken;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::config::BrandmeisterApiConfig;
use crate::config::Config;

/// Run startup provisioning: log peer profile, reconcile statics if
/// requested.  Errors are logged and swallowed -- the bridge does not
/// gate on API success.
pub(crate) async fn provision(config: &Config) {
    let device_id = config.repeater.dmr_id;
    let client = build_client(config.brandmeister_api.as_ref());
    run_once(&client, device_id, config.brandmeister_api.as_ref()).await;
}

/// Re-run `provision` on each tick so SelfCare edits made while the
/// bridge is up get corrected on the next pass.  Initial provisioning
/// is the caller's job; this timer skips the immediate tick.
///
/// Runs as a try_join! branch on the main task so panics propagate
/// up cleanly (process exits, systemd restarts).  All work is async
/// reqwest -- no blocking, no separate task needed.
pub(crate) async fn periodic_provision(
    device_id: DmrId,
    api_cfg: BrandmeisterApiConfig,
    interval: Duration,
    cancel: CancellationToken,
) {
    info!(?interval, "BM API reconcile timer enabled");
    let client = build_client(Some(&api_cfg));
    let mut ticker = tokio::time::interval(interval);
    // Avoid back-to-back catch-up runs if the runtime stalls.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = ticker.tick() => run_once(&client, device_id, Some(&api_cfg)).await,
        }
    }
}

fn build_client(api_cfg: Option<&BrandmeisterApiConfig>) -> Client {
    match api_cfg.and_then(|c| c.api_key.as_ref()) {
        Some(token) => Client::with_token(token.expose_secret().to_owned().into()),
        None => Client::new(),
    }
}

async fn run_once(client: &Client, device_id: DmrId, api_cfg: Option<&BrandmeisterApiConfig>) {
    log_profile(client, device_id).await;
    if let Some(cfg) = api_cfg
        && cfg.api_key.is_some()
    {
        reconcile_statics(client, device_id, cfg).await;
    }
}

async fn log_profile(client: &Client, device_id: DmrId) {
    match client.device_profile(device_id).await {
        Ok(profile) => {
            let statics: Vec<String> = profile
                .static_subscriptions
                .iter()
                .map(|s| format!("ts{}/{}", s.slot, s.talkgroup))
                .collect();
            info!(
                device_id = %device_id,
                statics = %statics.join(","),
                dynamics_present = !profile.dynamic_subscriptions.is_null()
                    && !is_empty_object(&profile.dynamic_subscriptions),
                timed_present = !profile.timed_subscriptions.is_null()
                    && !is_empty_object(&profile.timed_subscriptions),
                "Brandmeister peer profile"
            );
        }
        Err(e) => {
            warn!(device_id = %device_id, "Brandmeister peer profile fetch failed: {e}");
        }
    }
}

/// `serde_json::Value` is `null`, `{}`, or `[]` for "no entries"
/// across the BM API depending on whether the bucket is populated;
/// treat all three as empty for the log message.
fn is_empty_object(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Object(m) => m.is_empty(),
        serde_json::Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

async fn reconcile_statics(client: &Client, device_id: DmrId, api_cfg: &BrandmeisterApiConfig) {
    let current = match client.device_talkgroups(device_id).await {
        Ok(v) => v,
        Err(e) => {
            error!(
                device_id = %device_id,
                "static-TG reconcile aborted, list fetch failed: {e}"
            );
            return;
        }
    };

    if let Some(desired) = api_cfg.static_talkgroups_ts1.as_deref() {
        reconcile_slot(client, device_id, Slot::One, desired, &current).await;
    }
    if let Some(desired) = api_cfg.static_talkgroups_ts2.as_deref() {
        reconcile_slot(client, device_id, Slot::Two, desired, &current).await;
    }
}

async fn reconcile_slot(
    client: &Client,
    device_id: DmrId,
    slot: Slot,
    desired: &[Talkgroup],
    current_all_slots: &[brandmeister_api::types::StaticTalkgroup],
) {
    let current: Vec<Talkgroup> = current_all_slots
        .iter()
        .filter(|s| s.slot == slot)
        .map(|s| s.talkgroup)
        .collect();

    let to_add: Vec<Talkgroup> = desired
        .iter()
        .copied()
        .filter(|tg| !current.contains(tg))
        .collect();
    let to_remove: Vec<Talkgroup> = current
        .iter()
        .copied()
        .filter(|tg| !desired.contains(tg))
        .collect();

    if to_add.is_empty() && to_remove.is_empty() {
        info!(
            device_id = %device_id,
            slot = %slot,
            current = ?current,
            "static TGs already match config",
        );
        return;
    }

    info!(
        device_id = %device_id,
        slot = %slot,
        current = ?current,
        desired = ?desired,
        adds = ?to_add,
        removes = ?to_remove,
        "reconciling static TGs"
    );

    // Removes first so a "swap" (current=[91], desired=[3100]) doesn't
    // transiently over-subscribe in the rare case BM enforces a slot
    // cap; it also keeps the failure recovery story simpler -- if a
    // remove fails halfway, no spurious add happened.
    for tg in to_remove {
        if let Err(e) = client.remove_static_talkgroup(device_id, slot, tg).await {
            error!(device_id = %device_id, slot = %slot, tg = %tg, "remove static TG failed: {e}");
        }
    }
    for tg in to_add {
        if let Err(e) = client.add_static_talkgroup(device_id, slot, tg).await {
            error!(device_id = %device_id, slot = %slot, tg = %tg, "add static TG failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brandmeister_api::types::StaticTalkgroup;

    fn talkgroup(n: u32) -> Talkgroup {
        Talkgroup::try_from(n).unwrap()
    }

    fn static_talkgroup(slot: Slot, tg: u32) -> StaticTalkgroup {
        StaticTalkgroup {
            talkgroup: talkgroup(tg),
            repeaterid: None,
            slot,
        }
    }

    /// Pure helper for diff logic, mirrors the body of reconcile_slot.
    /// Returned tuple is (to_add, to_remove).
    fn diff(
        slot: Slot,
        desired: &[Talkgroup],
        current_all_slots: &[StaticTalkgroup],
    ) -> (Vec<Talkgroup>, Vec<Talkgroup>) {
        let current: Vec<Talkgroup> = current_all_slots
            .iter()
            .filter(|s| s.slot == slot)
            .map(|s| s.talkgroup)
            .collect();
        let to_add: Vec<Talkgroup> = desired
            .iter()
            .copied()
            .filter(|tg| !current.contains(tg))
            .collect();
        let to_remove: Vec<Talkgroup> = current
            .iter()
            .copied()
            .filter(|tg| !desired.contains(tg))
            .collect();
        (to_add, to_remove)
    }

    #[test]
    fn diff_no_change() {
        let current = vec![
            static_talkgroup(Slot::One, 91),
            static_talkgroup(Slot::One, 3100),
        ];
        let (add, remove) = diff(Slot::One, &[talkgroup(91), talkgroup(3100)], &current);
        assert!(add.is_empty());
        assert!(remove.is_empty());
    }

    #[test]
    fn diff_pure_add() {
        let current = vec![];
        let (add, remove) = diff(Slot::One, &[talkgroup(91)], &current);
        assert_eq!(add, vec![talkgroup(91)]);
        assert!(remove.is_empty());
    }

    #[test]
    fn diff_pure_remove() {
        let current = vec![
            static_talkgroup(Slot::One, 91),
            static_talkgroup(Slot::One, 3100),
        ];
        let (add, remove) = diff(Slot::One, &[], &current);
        assert!(add.is_empty());
        assert_eq!(remove, vec![talkgroup(91), talkgroup(3100)]);
    }

    #[test]
    fn diff_swap() {
        let current = vec![static_talkgroup(Slot::One, 91)];
        let (add, remove) = diff(Slot::One, &[talkgroup(3100)], &current);
        assert_eq!(add, vec![talkgroup(3100)]);
        assert_eq!(remove, vec![talkgroup(91)]);
    }

    #[test]
    fn diff_ignores_other_slot() {
        // Reconciling TS1 must not touch TS2 statics.
        let current = vec![
            static_talkgroup(Slot::One, 91),
            static_talkgroup(Slot::Two, 9990),
        ];
        let (add, remove) = diff(Slot::One, &[talkgroup(91)], &current);
        assert!(add.is_empty());
        assert!(remove.is_empty());
    }

    #[tokio::test]
    async fn reconcile_slot_removes_before_adds() {
        // Swap (current=[91], desired=[3100]) must DELETE 91 before
        // POSTing 3100, otherwise the peer transiently holds two
        // statics on slot 1 -- a problem if BM ever enforces a slot
        // cap, and a worse one if a remove later fails (we'd be left
        // with the unwanted static still subscribed).
        use brandmeister_api::client::Client;
        use secrecy::SecretString;
        use url::Url;
        use wiremock::Mock;
        use wiremock::MockServer;
        use wiremock::ResponseTemplate;
        use wiremock::matchers::method;
        use wiremock::matchers::path;

        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/device/12345/talkgroup/1/91"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/device/12345/talkgroup"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let base = Url::parse(&format!("{}/", server.uri())).unwrap();
        let client = Client::builder()
            .base_url(base)
            .token(SecretString::from("test-token"))
            .build();

        let current = vec![static_talkgroup(Slot::One, 91)];
        reconcile_slot(
            &client,
            DmrId::try_from(12345).unwrap(),
            Slot::One,
            &[talkgroup(3100)],
            &current,
        )
        .await;

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 2, "expected DELETE+POST, got {received:?}");
        assert_eq!(received[0].method.as_str(), "DELETE");
        assert_eq!(received[1].method.as_str(), "POST");
    }
}
