//! Async HTTP client for the Brandmeister Halligan REST API (v2).
//!
//! Construction:
//! ```no_run
//! use brandmeister_api::client::Client;
//! use secrecy::SecretString;
//!
//! // Read-only (anonymous) client -- works for GETs that don't require
//! // bearer auth (device info, profile, talkgroups list, talkgroup
//! // info).  Mutations and `getRepeater`/`dropDynamicGroups` actions
//! // need a bearer token.
//! let _client = Client::new();
//!
//! let auth: SecretString = std::env::var("BRANDMEISTER_API_KEY")
//!     .unwrap_or_default()
//!     .into();
//! let _client = Client::with_token(auth);
//! ```

use std::time::Duration;

use reqwest::Method;
use reqwest::Response;
use reqwest::Url;
use reqwest::header::AUTHORIZATION;
use reqwest::header::HeaderValue;
use secrecy::ExposeSecret;
use secrecy::SecretString;
use serde::Serialize;
use serde::de::DeserializeOwned;

use dmr_types::DmrId;
use dmr_types::Slot;
use dmr_types::Talkgroup;

use crate::error::ApiError;
use crate::types::AddStaticBody;
use crate::types::Device;
use crate::types::DeviceProfile;
use crate::types::StaticTalkgroup;
use crate::types::TalkgroupInfo;

const DEFAULT_BASE_URL: &str = "https://api.brandmeister.network/v2/";

/// Total request timeout (connect + TLS + send + body read).  Bounds
/// how long a hung BM API can stall a caller; pick your own ceiling
/// via `ClientBuilder::http`.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// TCP+TLS connect timeout.  Shorter than the request timeout so a
/// dead network surfaces fast; bridge startup blocks on the first
/// `provision` call until this expires.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Brandmeister API client.
///
/// Cheap to clone -- shares the underlying `reqwest::Client` connection
/// pool.  Construct once per process and reuse.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: Url,
    token: Option<SecretString>,
}

impl Client {
    /// Anonymous client against the production Brandmeister API.  Use
    /// for read-only endpoints; mutations and protected GETs return
    /// `ApiError::Unauthenticated`.
    pub fn new() -> Self {
        Self::builder().build()
    }

    /// Authenticated client with the given bearer token (JWT issued by
    /// Brandmeister SelfCare).
    pub fn with_token(token: SecretString) -> Self {
        Self::builder().token(token).build()
    }

    /// Mutable builder for non-default base URL / token.
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    // --- Device endpoints ---

    /// GET /device/{id} -- device info (no auth).
    pub async fn device(&self, id: DmrId) -> Result<Device, ApiError> {
        self.get_json(&format!("device/{id}"), false).await
    }

    /// GET /device/{id}/profile -- aggregate static / dynamic / timed
    /// / blocked / cluster subscriptions in one response.  No auth.
    pub async fn device_profile(&self, id: DmrId) -> Result<DeviceProfile, ApiError> {
        self.get_json(&format!("device/{id}/profile"), false).await
    }

    /// GET /device/{id}/talkgroup -- current static talkgroup
    /// subscriptions for the device.  No auth.
    pub async fn device_talkgroups(&self, id: DmrId) -> Result<Vec<StaticTalkgroup>, ApiError> {
        self.get_json(&format!("device/{id}/talkgroup"), false)
            .await
    }

    /// POST /device/{id}/talkgroup -- add a static talkgroup
    /// subscription.  Requires bearer auth.
    pub async fn add_static_talkgroup(
        &self,
        id: DmrId,
        slot: Slot,
        talkgroup: Talkgroup,
    ) -> Result<(), ApiError> {
        let body = AddStaticBody { talkgroup, slot };
        self.post_no_response(&format!("device/{id}/talkgroup"), &body, true)
            .await
    }

    /// DELETE /device/{id}/talkgroup/{slot}/{group} -- remove a static
    /// talkgroup subscription.  Requires bearer auth.
    pub async fn remove_static_talkgroup(
        &self,
        id: DmrId,
        slot: Slot,
        talkgroup: Talkgroup,
    ) -> Result<(), ApiError> {
        let path = format!("device/{id}/talkgroup/{slot}/{talkgroup}");
        self.send_no_response(Method::DELETE, &path, true).await
    }

    /// GET /device/{id}/action/getRepeater -- live state from the
    /// master the repeater is connected to.  Returns the response as
    /// raw JSON; the master payload is not stably documented.
    pub async fn get_repeater(&self, id: DmrId) -> Result<serde_json::Value, ApiError> {
        self.get_json(&format!("device/{id}/action/getRepeater"), true)
            .await
    }

    /// GET /device/{id}/action/dropDynamicGroups/{slot} -- clear all
    /// dynamic talkgroup subscriptions on the given slot.  Requires
    /// bearer auth.
    pub async fn drop_dynamic_groups(&self, id: DmrId, slot: Slot) -> Result<(), ApiError> {
        self.send_no_response(
            Method::GET,
            &format!("device/{id}/action/dropDynamicGroups/{slot}"),
            true,
        )
        .await
    }

    // --- Talkgroup endpoints ---

    /// GET /talkgroup/{id} -- talkgroup metadata.  No auth.
    pub async fn talkgroup(&self, id: Talkgroup) -> Result<TalkgroupInfo, ApiError> {
        self.get_json(&format!("talkgroup/{id}"), false).await
    }

    /// GET /talkgroup/{id}/devices -- devices that have this talkgroup
    /// statically subscribed.  No auth.
    pub async fn talkgroup_devices(&self, id: Talkgroup) -> Result<Vec<Device>, ApiError> {
        self.get_json(&format!("talkgroup/{id}/devices"), false)
            .await
    }

    // --- Internals ---

    async fn get_json<T: DeserializeOwned>(&self, path: &str, auth: bool) -> Result<T, ApiError> {
        let resp = self.send(Method::GET, path, None::<&()>, auth).await?;
        decode(path, resp).await
    }

    async fn post_no_response<B: Serialize>(
        &self,
        path: &str,
        body: &B,
        auth: bool,
    ) -> Result<(), ApiError> {
        let _ = self.send(Method::POST, path, Some(body), auth).await?;
        Ok(())
    }

    async fn send_no_response(
        &self,
        method: Method,
        path: &str,
        auth: bool,
    ) -> Result<(), ApiError> {
        let _ = self.send(method, path, None::<&()>, auth).await?;
        Ok(())
    }

    async fn send<B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        auth: bool,
    ) -> Result<Response, ApiError> {
        let url = self.base_url.join(path).map_err(|e| ApiError::Decode {
            context: format!("url join {path}"),
            source: serde::de::Error::custom(e.to_string()),
        })?;
        let mut req = self.http.request(method, url);
        if auth {
            let token = self.token.as_ref().ok_or(ApiError::Unauthenticated)?;
            // The bearer ends up in HeaderValue + reqwest's send
            // buffers, neither of which zeroize.  Tightening just
            // this allocation wouldn't change the threat model.
            let bearer = SecretString::from(format!("Bearer {}", token.expose_secret()));
            let mut hv = HeaderValue::from_str(bearer.expose_secret())
                .map_err(|_| ApiError::InvalidToken)?;
            hv.set_sensitive(true);
            req = req.header(AUTHORIZATION, hv);
        }
        if let Some(body) = body {
            req = req.json(body);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            return Err(http_error(resp).await);
        }
        Ok(resp)
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for `Client`.  Use `Client::builder()`.
#[derive(Debug, Default)]
pub struct ClientBuilder {
    base_url: Option<Url>,
    token: Option<SecretString>,
    http: Option<reqwest::Client>,
}

impl ClientBuilder {
    /// Override the base URL (default
    /// `https://api.brandmeister.network/v2/`).  Used by tests against
    /// a mock server.
    pub fn base_url(mut self, url: Url) -> Self {
        self.base_url = Some(url);
        self
    }

    /// Set the bearer token used for protected endpoints.
    pub fn token(mut self, token: SecretString) -> Self {
        self.token = Some(token);
        self
    }

    /// Inject a pre-configured `reqwest::Client` (e.g., custom
    /// timeouts).  When unset, a default client is constructed.
    pub fn http(mut self, http: reqwest::Client) -> Self {
        self.http = Some(http);
        self
    }

    pub fn build(self) -> Client {
        Client {
            http: self.http.unwrap_or_else(|| {
                reqwest::Client::builder()
                    .timeout(DEFAULT_REQUEST_TIMEOUT)
                    .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
                    .build()
                    .expect("default reqwest client builds")
            }),
            base_url: self
                .base_url
                .unwrap_or_else(|| Url::parse(DEFAULT_BASE_URL).expect("default base URL parses")),
            token: self.token,
        }
    }
}

/// Cap on a successful response body.  Generous (BM list endpoints
/// can return tens of thousands of talkgroups, ~5 MB) but bounds
/// memory if the API ever serves something pathological.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

async fn decode<T: DeserializeOwned>(path: &str, resp: Response) -> Result<T, ApiError> {
    let body = read_capped(path, resp, MAX_RESPONSE_BYTES).await?;
    serde_json::from_slice(&body).map_err(|source| ApiError::Decode {
        context: path.to_string(),
        source,
    })
}

/// Stream the response into a `Vec<u8>`, refusing to allocate beyond
/// `max` bytes total.  Reject early via `Content-Length` when present;
/// otherwise accumulate chunks and bail as soon as the cumulative size
/// would cross the cap.
async fn read_capped(context: &str, resp: Response, max: usize) -> Result<Vec<u8>, ApiError> {
    if let Some(len) = resp.content_length()
        && len as usize > max
    {
        return Err(ApiError::BodyTooLarge {
            context: context.to_string(),
            max,
        });
    }
    let mut buf = Vec::with_capacity(resp.content_length().unwrap_or(0) as usize);
    let mut stream = resp;
    while let Some(chunk) = stream.chunk().await? {
        if buf.len() + chunk.len() > max {
            return Err(ApiError::BodyTooLarge {
                context: context.to_string(),
                max,
            });
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

async fn http_error(resp: Response) -> ApiError {
    let status = resp.status();
    let body = truncate_body(
        resp.text().await.unwrap_or_default(),
        crate::error::HTTP_BODY_CAP_BYTES,
    );
    ApiError::Http { status, body }
}

fn truncate_body(mut s: String, max: usize) -> String {
    if s.len() <= max {
        return s;
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("...");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_body_passes_short_through() {
        assert_eq!(truncate_body("ok".into(), 256), "ok");
    }

    #[test]
    fn truncate_body_at_exact_max_does_not_add_ellipsis() {
        let s = "x".repeat(256);
        assert_eq!(truncate_body(s.clone(), 256), s);
    }

    #[test]
    fn truncate_body_cuts_long_ascii_with_ellipsis() {
        let s = "x".repeat(300);
        let out = truncate_body(s, 256);
        assert_eq!(out.len(), 256 + 3);
        assert!(out.ends_with("..."));
    }

    #[test]
    fn truncate_body_respects_utf8_char_boundary() {
        // 'é' is two bytes; if max lands inside it, cut backs up to
        // the last char boundary so the string stays valid UTF-8.
        let mut s = String::new();
        for _ in 0..200 {
            s.push('é');
        }
        let out = truncate_body(s, 257);
        assert!(out.is_char_boundary(out.len() - 3));
        assert!(out.ends_with("..."));
    }

    // --- HTTP method tests (wiremock) ---

    use secrecy::SecretString;
    use url::Url;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::header;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    fn dmr_id(n: u32) -> DmrId {
        DmrId::try_from(n).unwrap()
    }

    fn talkgroup(n: u32) -> Talkgroup {
        Talkgroup::try_from(n).unwrap()
    }

    fn slot(n: u8) -> Slot {
        Slot::try_from(n).unwrap()
    }

    fn make_client(server: &MockServer, with_token: bool) -> Client {
        let base = Url::parse(&format!("{}/", server.uri())).expect("server URI parses");
        let builder = Client::builder().base_url(base);
        let builder = if with_token {
            builder.token(SecretString::from("test-token"))
        } else {
            builder
        };
        builder.build()
    }

    #[tokio::test]
    async fn device_get_no_auth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/device/310770201"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": 310770201, "callsign": "AI6KG"})),
            )
            .mount(&server)
            .await;
        let client = make_client(&server, false);
        let d = client
            .device(dmr_id(310770201))
            .await
            .expect("device call ok");
        assert_eq!(d.id, dmr_id(310770201));
        assert_eq!(d.callsign, "AI6KG");
    }

    #[tokio::test]
    async fn device_profile_get_no_auth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/device/310770201/profile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "staticSubscriptions": [{"talkgroup": 91, "slot": 1}],
                "dynamicSubscriptions": {},
                "timedSubscriptions": {},
                "blockedGroups": {},
                "cluster": {},
            })))
            .mount(&server)
            .await;
        let client = make_client(&server, false);
        let p = client
            .device_profile(dmr_id(310770201))
            .await
            .expect("profile call ok");
        assert_eq!(p.static_subscriptions.len(), 1);
        assert_eq!(p.static_subscriptions[0].talkgroup, talkgroup(91));
    }

    #[tokio::test]
    async fn device_talkgroups_get_no_auth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/device/12345/talkgroup"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"talkgroup": 91, "slot": 1},
                {"talkgroup": 9990, "slot": 2},
            ])))
            .mount(&server)
            .await;
        let client = make_client(&server, false);
        let v = client
            .device_talkgroups(dmr_id(12345))
            .await
            .expect("talkgroups call ok");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].talkgroup, talkgroup(91));
        assert_eq!(v[1].slot, Slot::Two);
    }

    #[tokio::test]
    async fn add_static_talkgroup_posts_with_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device/12345/talkgroup"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = make_client(&server, true);
        client
            .add_static_talkgroup(dmr_id(12345), slot(1), talkgroup(91))
            .await
            .expect("add ok");
    }

    #[tokio::test]
    async fn remove_static_talkgroup_deletes_with_auth() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/device/12345/talkgroup/1/91"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = make_client(&server, true);
        client
            .remove_static_talkgroup(dmr_id(12345), slot(1), talkgroup(91))
            .await
            .expect("remove ok");
    }

    #[tokio::test]
    async fn get_repeater_returns_raw_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/device/12345/action/getRepeater"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"master": 3104, "online": true})),
            )
            .mount(&server)
            .await;
        let client = make_client(&server, true);
        let v = client
            .get_repeater(dmr_id(12345))
            .await
            .expect("getRepeater ok");
        assert_eq!(v["master"], 3104);
        assert_eq!(v["online"], true);
    }

    #[tokio::test]
    async fn drop_dynamic_groups_get_with_auth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/device/12345/action/dropDynamicGroups/1"))
            .and(header("authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let client = make_client(&server, true);
        client
            .drop_dynamic_groups(dmr_id(12345), slot(1))
            .await
            .expect("drop ok");
    }

    #[tokio::test]
    async fn talkgroup_get_no_auth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/talkgroup/91"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": 91, "name": "Worldwide"})),
            )
            .mount(&server)
            .await;
        let client = make_client(&server, false);
        let t = client.talkgroup(talkgroup(91)).await.expect("talkgroup ok");
        assert_eq!(t.id, talkgroup(91));
        assert_eq!(t.name.as_deref(), Some("Worldwide"));
    }

    #[tokio::test]
    async fn talkgroup_devices_get_no_auth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/talkgroup/91/devices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"id": 1, "callsign": "AAA"},
                {"id": 2, "callsign": "BBB"},
            ])))
            .mount(&server)
            .await;
        let client = make_client(&server, false);
        let v = client
            .talkgroup_devices(talkgroup(91))
            .await
            .expect("talkgroup_devices ok");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, dmr_id(1));
    }

    #[tokio::test]
    async fn http_error_surfaces_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/device/9"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Device not found"))
            .mount(&server)
            .await;
        let client = make_client(&server, false);
        let err = client.device(dmr_id(9)).await.expect_err("expected error");
        match err {
            ApiError::Http { status, body } => {
                assert_eq!(status.as_u16(), 404);
                assert!(body.contains("Device not found"), "body was: {body}");
            }
            other => panic!("expected ApiError::Http, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_json_returns_decode_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/device/1"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("not valid json")
                    .insert_header("content-type", "application/json"),
            )
            .mount(&server)
            .await;
        let client = make_client(&server, false);
        let err = client.device(dmr_id(1)).await.expect_err("expected error");
        assert!(matches!(err, ApiError::Decode { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn protected_call_without_token_is_unauthenticated() {
        let client = Client::new();
        let err = client
            .add_static_talkgroup(dmr_id(12345), slot(1), talkgroup(91))
            .await
            .expect_err("expected error");
        assert!(matches!(err, ApiError::Unauthenticated), "got {err:?}");
    }
}
