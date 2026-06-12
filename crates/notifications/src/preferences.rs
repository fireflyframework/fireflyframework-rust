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
use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};

/// Returns `true` for the same characters as Python's `str.isdigit()`.
///
/// pyfly's `_normalize` filters SMS numbers with `ch.isdigit()`, which is
/// Unicode-aware: it accepts decimal digits (Unicode general category `Nd` —
/// ASCII `0-9`, Arabic-Indic `٠-٩`, fullwidth `０-９`, Devanagari, etc.) **and**
/// the non-decimal characters whose `Numeric_Type` is `Digit` (superscripts
/// `¹²³`, subscripts `₀-₉`, circled digits `①-⑨`, …). A plain
/// `char::is_ascii_digit()` filter would diverge from pyfly on every one of
/// these, so a recipient opted out with a Unicode-digit phone number would
/// normalize to a different opt-out key between the two ports.
///
/// `char::is_numeric()` is *too* broad here (it also matches `Nl`/`No`
/// characters such as `½` or `Ⅷ`, which Python's `isdigit()` rejects), so this
/// helper combines the `Nd` general category with the explicit
/// `Numeric_Type=Digit`-but-not-decimal set instead.
fn is_unicode_digit(c: char) -> bool {
    matches!(c.general_category(), GeneralCategory::DecimalNumber) || is_non_decimal_digit(c)
}

/// The characters Python's `str.isdigit()` accepts that are *not* decimal
/// (`isdecimal()` / general category `Nd`) — i.e. those with `Numeric_Type=Digit`
/// in a non-`Nd` category (superscripts, subscripts, circled/parenthesized
/// digits, and a handful of historic scripts). This set is fixed in Unicode and
/// mirrors CPython's behaviour exactly.
fn is_non_decimal_digit(c: char) -> bool {
    matches!(c,
        '\u{00B2}'..='\u{00B3}'      // ² ³
        | '\u{00B9}'                 // ¹
        | '\u{1369}'..='\u{1371}'    // Ethiopic digits one..nine
        | '\u{19DA}'                 // New Tai Lue tham digit one
        | '\u{2070}'                 // ⁰
        | '\u{2074}'..='\u{2079}'    // ⁴..⁹
        | '\u{2080}'..='\u{2089}'    // ₀..₉
        | '\u{2460}'..='\u{2468}'    // ①..⑨
        | '\u{2474}'..='\u{247C}'    // ⑴..⑼
        | '\u{2488}'..='\u{2490}'    // ⒈..⒐
        | '\u{24EA}'                 // ⓪
        | '\u{24F5}'..='\u{24FD}'    // dingbat negative circled digits
        | '\u{24FF}'                 // ⓿
        | '\u{2776}'..='\u{277E}'    // dingbat negative circled ❶..❾
        | '\u{2780}'..='\u{2788}'    // dingbat circled sans-serif ➀..➈
        | '\u{278A}'..='\u{2792}'    // dingbat negative circled sans-serif ➊..➒
        | '\u{10A40}'..='\u{10A43}'  // Kharoshthi digits
        | '\u{10E60}'..='\u{10E68}'  // Rumi digits
        | '\u{11052}'..='\u{1105A}'  // Brahmi number one..nine
        | '\u{1F100}'..='\u{1F10A}'  // digit zero/one full stop, comma
    )
}

/// Canonicalizes a recipient so opt-out records and look-ups match regardless
/// of casing/formatting.
///
/// Mirrors pyfly's `_normalize`: every recipient is trimmed and lower-cased;
/// SMS numbers additionally have all non-digit characters removed (preserving a
/// leading `+`). "Digit" here is Unicode-aware — see [`is_unicode_digit`] —
/// matching pyfly's `ch.isdigit()` rather than an ASCII-only filter. Without
/// this, `Alice@X.com` could opt out yet still be e-mailed when a send targets
/// `alice@x.com`.
fn normalize(recipient: &str, channel: &str) -> String {
    let value = recipient.trim().to_lowercase();
    if channel == "sms" {
        let digits: String = value.chars().filter(|c| is_unicode_digit(*c)).collect();
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
