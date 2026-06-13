// Copyright 2026 Firefly Software Foundation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! firefly-testkit — the framework's shared testing toolkit.
//!
//! `firefly-testkit` collects the helpers every `#[cfg(test)]` module in
//! every Firefly service ends up needing:
//!
//! | Group            | Helper                                                              | Feature     |
//! |------------------|---------------------------------------------------------------------|-------------|
//! | HMAC signers     | [`sign_hmac`] / [`sign_stripe`] / [`sign_github`] / [`sign_twilio`] | *(default)* |
//! | Event spy        | [`SpyBroker::record`], [`SpyBroker::find_by_type`], [`SpyBroker::reset`], [`SpyBroker::len`] | *(default)* |
//! | Event assertions | [`assert_event_published`] / [`assert_event_published_with`] / [`assert_no_events_published`] | *(default)* |
//! | JSON             | [`must_encode`] / [`must_decode`]                                   | *(default)* |
//! | HTTP test client | [`TestClient`] / [`TestResponse`] (in-process over an axum `Router`) | `web`       |
//! | DI test slices   | [`Slice`] / [`BuiltSlice`] (subset + `Arc` overrides, eager resolve) | `container` |
//! | Testcontainers   | [`containers::ServiceContainer`] / [`containers::config_for`] / [`containers::docker_available`] | `testcontainers` |
//!
//! The default surface carries no heavy dependencies; the richer
//! migration-ergonomics helpers (the analogs of pyfly's `PyFlyTestClient`,
//! `slice_context`/`mock_bean`, and `testcontainers`) are opt-in behind the
//! `web`, `container`, and `testcontainers` features so a service that only
//! needs the signers gets a lean build.
//!
//! Every signer matches the wire shape of its corresponding `webhooks`
//! validator — drop them into a test handler and a real Stripe / GitHub /
//! Twilio webhook will validate identically.
//!
//! # Quick start
//!
//! ```
//! use firefly_testkit::{assert_event_published, must_encode, sign_stripe, SpyBroker};
//!
//! // Sign a webhook body exactly like Stripe would.
//! let sig = sign_stripe(b"whsec_test", br#"{"type":"charge.succeeded"}"#, 1_700_000_000);
//! assert!(sig.starts_with("t=1700000000,v1="));
//!
//! // Assert which events a handler emitted.
//! let spy = SpyBroker::new();
//! let body = must_encode(&serde_json::json!({ "id": 1 }));
//! spy.record("orders", "OrderPlaced", &body);
//! let event = assert_event_published(&spy, "OrderPlaced");
//! assert_eq!(event.topic, "orders");
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod assertions;
mod broker;
mod json;
mod signers;

#[cfg(feature = "web")]
mod client;
#[cfg(feature = "container")]
mod slice;

#[cfg(feature = "testcontainers")]
pub mod containers;

pub use assertions::{
    assert_event_published, assert_event_published_with, assert_no_events_published,
};
pub use broker::{RecordedEvent, SpyBroker};
pub use json::{must_decode, must_encode};
pub use signers::{sign_github, sign_hmac, sign_stripe, sign_twilio};

#[cfg(feature = "web")]
pub use client::{TestClient, TestResponse};
#[cfg(feature = "container")]
pub use slice::{BuiltSlice, Slice};

/// Framework version stamp.
pub const VERSION: &str = "26.6.3";
