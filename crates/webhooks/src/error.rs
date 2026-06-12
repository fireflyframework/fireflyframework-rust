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

//! The typed error family of the webhook subsystem.

/// Errors produced while validating, dispatching, or dead-lettering an
/// inbound webhook.
///
/// The Go port exposes a single sentinel — `ErrSignatureMismatch` — and
/// lets processors return arbitrary `error` values; the Rust port makes
/// the failure classes explicit in one `thiserror` enum. The `Display`
/// strings are wire-compatible with the Go sentinels: a signature
/// failure always renders as `firefly/webhooks: signature mismatch`
/// (with a `: stale` suffix for an expired Stripe timestamp), so log
/// lines and HTTP error bodies match across runtimes byte-for-byte.
#[derive(Debug, thiserror::Error)]
pub enum WebhookError {
    /// The request's signature header was missing, malformed, or did
    /// not match the configured secret — the analog of Go's
    /// `ErrSignatureMismatch`. The ingestion endpoint maps this to
    /// `401 Unauthorized`.
    #[error("firefly/webhooks: signature mismatch")]
    SignatureMismatch,

    /// A Stripe `t=` timestamp fell outside the configured tolerance
    /// window — Go's `fmt.Errorf("%w: stale", ErrSignatureMismatch)`.
    /// Matched by [`WebhookError::is_signature_mismatch`] exactly as
    /// `errors.Is(err, ErrSignatureMismatch)` matches the wrapped Go
    /// error.
    #[error("firefly/webhooks: signature mismatch: stale")]
    StaleSignature,

    /// A downstream [`Processor`](crate::Processor) rejected the event.
    /// The message is the processor's own error text (Go processors
    /// return plain `error` values); it is what the DLQ records and
    /// what the ingestion endpoint returns with `500`.
    #[error("{0}")]
    Processor(String),

    /// A [`Dlq`](crate::Dlq) implementation failed to persist a
    /// dead-letter entry. The pipeline itself ignores DLQ push errors
    /// (as the Go port does); this variant exists for custom DLQ
    /// implementations to report their own failures.
    #[error("firefly/webhooks: dlq: {0}")]
    Dlq(String),
}

impl WebhookError {
    /// Builds a [`WebhookError::Processor`] from any message — the
    /// Rust spelling of a Go processor returning `errors.New(msg)`.
    pub fn processor(msg: impl Into<String>) -> Self {
        Self::Processor(msg.into())
    }

    /// Reports whether this error is a signature-verification failure —
    /// the analog of Go's `errors.Is(err, core.ErrSignatureMismatch)`,
    /// which also matches the wrapped "stale" variant.
    pub fn is_signature_mismatch(&self) -> bool {
        matches!(self, Self::SignatureMismatch | Self::StaleSignature)
    }
}
