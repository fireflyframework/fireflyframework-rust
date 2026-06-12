//! Per-channel notification opt-out / preference service (pyfly parity).
//!
//! The Rust counterpart of `pyfly.notifications.preferences`. Before delegating
//! to a provider, the default services consult
//! [`PreferenceService::is_opted_in`]; opted-out recipients are pruned and, if
//! every recipient opts out, the send is short-circuited with a
//! [`DeliveryStatus::Suppressed`](crate::models::DeliveryStatus::Suppressed)
//! result.
//!
//! The channel strings used internally are `"email"`, `"sms"`, and `"push"`.

use std::collections::HashSet;
use std::sync::RwLock;

use async_trait::async_trait;

/// Canonicalizes a recipient so opt-out records and look-ups match regardless
/// of casing/formatting.
///
/// Mirrors pyfly's `_normalize`: every recipient is trimmed and lower-cased;
/// SMS numbers additionally have all non-digit characters removed (preserving a
/// leading `+`). Without this, `Alice@X.com` could opt out yet still be e-mailed
/// when a send targets `alice@x.com`.
fn normalize(recipient: &str, channel: &str) -> String {
    let value = recipient.trim().to_lowercase();
    if channel == "sms" {
        let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
        if value.starts_with('+') {
            format!("+{digits}")
        } else {
            digits
        }
    } else {
        value
    }
}

/// Port for querying per-recipient, per-channel notification preferences.
///
/// Equivalent to pyfly's `NotificationPreferenceService` protocol.
#[async_trait]
pub trait PreferenceService: Send + Sync {
    /// Returns `true` if `recipient` has **not** opted out of `channel`.
    ///
    /// `recipient` is a channel-specific identifier — an e-mail address for
    /// `"email"`, a phone number for `"sms"`, or a device token for `"push"`.
    async fn is_opted_in(&self, recipient: &str, channel: &str) -> bool;
}

/// Thread-safe, in-memory [`PreferenceService`].
///
/// Equivalent to pyfly's `InMemoryPreferenceService`. All recipients are
/// opted-in by default; call [`opt_out`](InMemoryPreferenceService::opt_out) to
/// suppress future sends and [`opt_in`](InMemoryPreferenceService::opt_in) to
/// restore them. Recipients are normalized (see module docs) so opt-out is
/// case-insensitive and SMS formatting is ignored.
///
/// # Example
///
/// ```
/// use firefly_notifications::{InMemoryPreferenceService, PreferenceService};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let prefs = InMemoryPreferenceService::new();
/// prefs.opt_out("Alice@Example.com", "email");
/// assert!(!prefs.is_opted_in("alice@example.com", "email").await);
/// assert!(prefs.is_opted_in("alice@example.com", "sms").await);
/// # }
/// ```
#[derive(Default)]
pub struct InMemoryPreferenceService {
    // Set of (normalized recipient, channel) tuples that are opted OUT.
    opted_out: RwLock<HashSet<(String, String)>>,
}

impl InMemoryPreferenceService {
    /// Returns an empty preference store (everyone opted in).
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `recipient` has opted out of `channel`.
    pub fn opt_out(&self, recipient: &str, channel: &str) {
        self.opted_out
            .write()
            .expect("preference lock poisoned")
            .insert((normalize(recipient, channel), channel.to_string()));
    }

    /// Removes the opt-out record for `recipient` / `channel`.
    pub fn opt_in(&self, recipient: &str, channel: &str) {
        self.opted_out
            .write()
            .expect("preference lock poisoned")
            .remove(&(normalize(recipient, channel), channel.to_string()));
    }
}

#[async_trait]
impl PreferenceService for InMemoryPreferenceService {
    async fn is_opted_in(&self, recipient: &str, channel: &str) -> bool {
        !self
            .opted_out
            .read()
            .expect("preference lock poisoned")
            .contains(&(normalize(recipient, channel), channel.to_string()))
    }
}
