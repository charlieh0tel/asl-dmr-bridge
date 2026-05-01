use thiserror::Error;

/// Cap on response-body bytes carried in `ApiError::Http`.  Client
/// truncates at construction; Display re-truncates as defense.
pub const HTTP_BODY_CAP_BYTES: usize = 256;

/// Errors returned by the Brandmeister API client.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ApiError {
    /// Underlying transport / request error from `reqwest`.
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),

    /// Server returned a non-2xx HTTP status.  `:.256` mirrors
    /// `HTTP_BODY_CAP_BYTES`; keep in sync (thiserror precision can't
    /// name a const).
    #[error("HTTP {status}: {body:.256}")]
    Http {
        status: reqwest::StatusCode,
        body: String,
    },

    /// JSON deserialization of a response body failed.
    #[error("decode {context}: {source}")]
    Decode {
        /// What we were trying to decode (path or operation name).
        context: String,
        #[source]
        source: serde_json::Error,
    },

    /// Operation requires an API key but none was supplied to the
    /// `Client` builder.
    #[error("operation requires authentication, no API key configured")]
    Unauthenticated,

    /// Configured bearer token contained bytes that are not legal in
    /// an HTTP `Authorization` header value (non-ASCII, controls).
    /// Caller should re-issue the JWT or strip stray whitespace.
    #[error("configured bearer token is not a valid HTTP header value")]
    InvalidToken,

    /// Response body exceeded the per-request size cap before fully
    /// arriving.  Stops a malicious or misbehaving server from feeding
    /// us a multi-GB body.
    #[error("response from {context} exceeded {max} byte cap")]
    BodyTooLarge { context: String, max: usize },
}
