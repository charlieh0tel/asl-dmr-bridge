//! Typed Rust client for the Brandmeister Halligan REST API (v2).
//!
//! Covers the device (peer/hotspot/repeater) and talkgroup endpoints
//! used to query peer state and manage static talkgroup subscriptions.
//! Reads are anonymous; mutations need a bearer JWT issued by
//! Brandmeister SelfCare.
//!
//! API base URL: `https://api.brandmeister.network/v2/`.
//!
//! See <https://api.brandmeister.network/api-docs> for the upstream
//! OpenAPI spec.

pub mod client;
pub mod error;
pub mod types;
