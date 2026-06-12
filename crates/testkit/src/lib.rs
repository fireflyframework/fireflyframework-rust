//! firefly-testkit — the framework's shared testing toolkit.
//!
//! `firefly-testkit` collects the helpers every `#[cfg(test)]` module in
//! every Firefly service ends up needing:
//!
//! | Group        | Helper                                                              |
//! |--------------|---------------------------------------------------------------------|
//! | HMAC signers | [`sign_hmac`] / [`sign_stripe`] / [`sign_github`] / [`sign_twilio`] |
//! | Event spy    | [`SpyBroker::record`], [`SpyBroker::find_by_type`], [`SpyBroker::reset`], [`SpyBroker::len`] |
//! | JSON         | [`must_encode`] / [`must_decode`]                                   |
//!
//! Every signer matches the wire shape of its corresponding `webhooks`
//! validator — drop them into a test handler and a real Stripe / GitHub /
//! Twilio webhook will validate identically.
//!
//! # Quick start
//!
//! ```
//! use firefly_testkit::{must_encode, sign_stripe, SpyBroker};
//!
//! // Sign a webhook body exactly like Stripe would.
//! let sig = sign_stripe(b"whsec_test", br#"{"type":"charge.succeeded"}"#, 1_700_000_000);
//! assert!(sig.starts_with("t=1700000000,v1="));
//!
//! // Assert which events a handler emitted.
//! let spy = SpyBroker::new();
//! let body = must_encode(&serde_json::json!({ "id": 1 }));
//! spy.record("orders", "OrderPlaced", &body);
//! assert_eq!(spy.find_by_type("OrderPlaced").len(), 1);
//! ```

mod broker;
mod json;
mod signers;

pub use broker::{RecordedEvent, SpyBroker};
pub use json::{must_decode, must_encode};
pub use signers::{sign_github, sign_hmac, sign_stripe, sign_twilio};

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";
