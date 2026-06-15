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

//! firefly-notifications-smtp — a real SMTP e-mail provider over
//! [`lettre`](https://docs.rs/lettre).
//!
//! This crate is the Rust port of pyfly
//! `pyfly.notifications.providers.smtp.SmtpEmailProvider`. It builds a
//! standards-compliant MIME message from a rich [`EmailMessage`] (separate
//! text/HTML bodies, attachments, custom headers, cc/bcc) and delivers it
//! over SMTP with optional STARTTLS and SMTP AUTH.
//!
//! # Surface
//!
//! * [`EmailMessage`] / [`Attachment`] / [`EmailStatus`] /
//!   [`NotificationResult`] — the rich e-mail domain model (the email subset
//!   of pyfly `notifications.models`).
//! * [`EmailProvider`] — the delivery port (pyfly `EmailProvider` protocol).
//! * [`SmtpConfig`] — STARTTLS / auth / host / port wiring, with
//!   [`SmtpConfig::from_config`] config-key parsing.
//! * [`SmtpEmailProvider`] — the [`EmailProvider`] implementation; also
//!   implements the thin [`firefly_notifications::Channel`] mapping for the
//!   Go-parity dispatcher.
//! * [`build_message`] — the pure message-builder used internally and exposed
//!   so callers (and tests) can inspect the exact `lettre::Message` that would
//!   be transmitted without contacting a server.
//!
//! # Structure tests plus an env-gated live round-trip
//!
//! Per the framework's test policy, the crate is verified on a bare machine.
//! [`build_message`] is a pure function over [`EmailMessage`]; the unit tests
//! assert the resulting `lettre::Message` (headers, MIME parts, envelope,
//! bcc-not-leaked) without a live SMTP server. A genuine end-to-end send lives
//! in `tests/smtp_integration.rs`, **env-gated** on `FIREFLY_TEST_SMTP_ADDR`
//! (MailHog): unset, it skips and `cargo test` stays green; set, it delivers a
//! real e-mail and verifies it arrived via the MailHog HTTP API.
//!
//! # Example
//!
//! ```
//! use firefly_notifications_smtp::{EmailMessage, SmtpConfig, SmtpEmailProvider};
//!
//! let provider = SmtpEmailProvider::new(SmtpConfig {
//!     host: "smtp.example.com".into(),
//!     port: 587,
//!     username: Some("apikey".into()),
//!     password: Some("secret".into()),
//!     use_tls: true,
//! });
//!
//! // Inspect the message the provider would transmit, no network needed.
//! let msg = EmailMessage {
//!     to: vec!["dest@example.com".into()],
//!     sender: "from@example.com".into(),
//!     subject: "Hello SMTP".into(),
//!     body_text: Some("plain text body".into()),
//!     ..EmailMessage::default()
//! };
//! let built = firefly_notifications_smtp::build_message(&msg).unwrap();
//! assert!(String::from_utf8_lossy(&built.formatted()).contains("Subject: Hello SMTP"));
//! let _ = provider;
//! ```

mod email;

use async_trait::async_trait;
use firefly_notifications as notifications;
use firefly_notifications::{DeliveryResult, Kind, Notification, NotificationError};
use lettre::message::header::{ContentType, HeaderName, HeaderValue};
use lettre::message::{Attachment as LettreAttachment, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::AsyncSmtpTransport;
use lettre::{AsyncTransport, Message, Tokio1Executor};
use thiserror::Error;

pub use email::{Attachment, EmailMessage, EmailProvider, EmailStatus, NotificationResult};

/// Framework version stamp.
pub const VERSION: &str = "26.6.6";

/// The stable provider name used as the `provider` field of results.
pub const PROVIDER_NAME: &str = "smtp";

/// Header names lettre populates itself; custom-header entries that collide
/// with these are dropped so the standard headers are not clobbered (matching
/// pyfly's reserved-header guard).
const RESERVED_HEADERS: [&str; 5] = ["from", "to", "cc", "bcc", "subject"];

/// Errors raised while *building* a `lettre::Message` from an
/// [`EmailMessage`].
///
/// These are surfaced from [`build_message`]; inside
/// [`SmtpEmailProvider::send`] a build error is folded into a
/// [`EmailStatus::Failed`] result, never returned to the caller.
#[derive(Debug, Error)]
pub enum BuildError {
    /// An address (sender or recipient) could not be parsed as a mailbox.
    #[error("invalid address {address:?}: {source}")]
    Address {
        /// The offending address text.
        address: String,
        /// The parse error from lettre.
        #[source]
        source: lettre::address::AddressError,
    },
    /// A custom header name or value was rejected by lettre.
    #[error("invalid header {name:?}: {message}")]
    Header {
        /// The offending header name.
        name: String,
        /// The reason the header was rejected.
        message: String,
    },
    /// lettre rejected the assembled message (e.g. no recipients, bad body).
    #[error("message assembly failed: {0}")]
    Assembly(#[from] lettre::error::Error),
}

/// SMTP connection + auth configuration (pyfly `SmtpEmailProvider.__init__`
/// keyword arguments).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpConfig {
    /// SMTP server host.
    pub host: String,
    /// SMTP server port (pyfly default `587`).
    pub port: u16,
    /// AUTH username; `None` disables authentication.
    pub username: Option<String>,
    /// AUTH password; `None` disables authentication.
    pub password: Option<String>,
    /// Whether to upgrade the connection with STARTTLS (pyfly default `true`).
    pub use_tls: bool,
}

impl Default for SmtpConfig {
    /// pyfly defaults: port 587, no auth, STARTTLS enabled.
    fn default() -> Self {
        SmtpConfig {
            host: String::new(),
            port: 587,
            username: None,
            password: None,
            use_tls: true,
        }
    }
}

impl SmtpConfig {
    /// Parses configuration from flat config keys, mirroring the
    /// `from_config` constructor idiom used across the Rust port.
    ///
    /// Recognised keys (all optional except `host`):
    ///
    /// * `host` — SMTP server host (required for a usable provider).
    /// * `port` — integer port; defaults to `587`. Unparseable values fall
    ///   back to the default.
    /// * `username` / `password` — AUTH credentials. Empty strings are
    ///   treated as absent.
    /// * `use_tls` — `"true"`/`"false"` (case-insensitive); defaults to
    ///   `true`. Unrecognised values fall back to the default.
    pub fn from_config<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let nonempty = |key: &str| get(key).filter(|v| !v.is_empty());
        let defaults = SmtpConfig::default();
        SmtpConfig {
            host: get("host").unwrap_or_default(),
            port: get("port")
                .and_then(|p| p.parse().ok())
                .unwrap_or(defaults.port),
            username: nonempty("username"),
            password: nonempty("password"),
            use_tls: get("use_tls")
                .and_then(|v| match v.to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" => Some(true),
                    "false" | "0" | "no" => Some(false),
                    _ => None,
                })
                .unwrap_or(defaults.use_tls),
        }
    }
}

/// Builds the `lettre::Message` that [`SmtpEmailProvider`] would transmit for
/// `message`, without contacting any server.
///
/// The MIME structure mirrors pyfly's stdlib `EmailMessage` construction:
///
/// * text only → a single `text/plain` part;
/// * HTML only → a single `text/html` part;
/// * text + HTML → a `multipart/alternative` (plain first, then HTML);
/// * any of the above plus attachments → a `multipart/mixed` wrapping the
///   body, followed by one `application/...` attachment part each.
///
/// `Bcc` recipients are added to the message's envelope (so they receive the
/// mail) but lettre strips the `Bcc` header from [`Message::formatted`], so
/// they are never leaked to other recipients. Custom headers whose name
/// (case-insensitively) is one of `From`/`To`/`Cc`/`Bcc`/`Subject` are
/// ignored.
///
/// # Errors
///
/// Returns [`BuildError`] if an address or custom header is invalid, or if
/// lettre rejects the assembled message.
pub fn build_message(message: &EmailMessage) -> Result<Message, BuildError> {
    let mut builder = Message::builder();

    builder = builder.from(parse_mailbox(&message.sender)?);
    for addr in &message.to {
        builder = builder.to(parse_mailbox(addr)?);
    }
    for addr in &message.cc {
        builder = builder.cc(parse_mailbox(addr)?);
    }
    for addr in &message.bcc {
        builder = builder.bcc(parse_mailbox(addr)?);
    }
    builder = builder.subject(&message.subject);

    // Custom headers (skipping the reserved ones lettre owns).
    for (name, value) in &message.headers {
        if RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            continue;
        }
        let header_name =
            HeaderName::new_from_ascii(name.clone()).map_err(|e| BuildError::Header {
                name: name.clone(),
                message: e.to_string(),
            })?;
        builder = builder.raw_header(HeaderValue::new(header_name, value.clone()));
    }

    let body = build_body(message)?;
    Ok(builder.multipart(body)?)
}

/// A built body that is either a single part or a multipart subtree.
enum BodyPart {
    Single(SinglePart),
    Multi(MultiPart),
}

/// Assembles the MIME body tree (alternative/mixed) for `message`.
fn build_body(message: &EmailMessage) -> Result<MultiPart, BuildError> {
    // The "content" portion: text, html, or a text+html alternative.
    let content = match (&message.body_text, &message.body_html) {
        (Some(text), Some(html)) => BodyPart::Multi(MultiPart::alternative_plain_html(
            text.clone(),
            html.clone(),
        )),
        (Some(text), None) => BodyPart::Single(SinglePart::plain(text.clone())),
        (None, Some(html)) => BodyPart::Single(SinglePart::html(html.clone())),
        // Matching pyfly: an empty body yields an empty plain-text part.
        (None, None) => BodyPart::Single(SinglePart::plain(String::new())),
    };

    if message.attachments.is_empty() {
        // No attachments: wrap the content in a mixed container so the return
        // type is uniform. A single-child mixed part renders the child.
        return Ok(match content {
            BodyPart::Multi(m) => m,
            BodyPart::Single(s) => MultiPart::mixed().singlepart(s),
        });
    }

    // Attachments present: multipart/mixed = body + attachment parts.
    let mut mixed = match content {
        BodyPart::Multi(m) => MultiPart::mixed().multipart(m),
        BodyPart::Single(s) => MultiPart::mixed().singlepart(s),
    };
    for attachment in &message.attachments {
        let raw_type = if attachment.content_type.is_empty() {
            "application/octet-stream"
        } else {
            &attachment.content_type
        };
        let content_type = ContentType::parse(raw_type).unwrap_or_else(|_| {
            ContentType::parse("application/octet-stream").expect("static content type")
        });
        let part = LettreAttachment::new(attachment.filename.clone())
            .body(attachment.data.clone(), content_type);
        mixed = mixed.singlepart(part);
    }
    Ok(mixed)
}

/// Parses an address string into a lettre mailbox, mapping the error to
/// [`BuildError::Address`].
fn parse_mailbox(address: &str) -> Result<lettre::message::Mailbox, BuildError> {
    address
        .parse::<lettre::message::Mailbox>()
        .map_err(|source| BuildError::Address {
            address: address.to_string(),
            source,
        })
}

/// A real SMTP e-mail provider (pyfly `SmtpEmailProvider`).
///
/// Builds a MIME message with [`build_message`] and delivers it over
/// [`lettre`]'s async SMTP transport. STARTTLS and SMTP AUTH are configured
/// from [`SmtpConfig`].
#[derive(Debug, Clone)]
pub struct SmtpEmailProvider {
    cfg: SmtpConfig,
}

impl SmtpEmailProvider {
    /// Builds a provider from `cfg`.
    pub fn new(cfg: SmtpConfig) -> Self {
        SmtpEmailProvider { cfg }
    }

    /// Builds a provider from flat config keys (see [`SmtpConfig::from_config`]).
    pub fn from_config<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        SmtpEmailProvider::new(SmtpConfig::from_config(get))
    }

    /// Returns the configuration the provider was built with.
    pub fn config(&self) -> &SmtpConfig {
        &self.cfg
    }

    /// Assembles the async SMTP transport described by [`SmtpConfig`].
    ///
    /// With `use_tls` the connection is upgraded via STARTTLS
    /// (`starttls_relay`); otherwise an unencrypted `builder_dangerous`
    /// transport is used (suitable for in-process test servers). Credentials
    /// are attached only when both username and password are present.
    fn transport(
        &self,
    ) -> Result<AsyncSmtpTransport<Tokio1Executor>, lettre::transport::smtp::Error> {
        let mut builder = if self.cfg.use_tls {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.cfg.host)?
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&self.cfg.host)
        };
        builder = builder.port(self.cfg.port);
        if let (Some(user), Some(pass)) = (&self.cfg.username, &self.cfg.password) {
            builder = builder.credentials(Credentials::new(user.clone(), pass.clone()));
        }
        Ok(builder.build())
    }
}

#[async_trait]
impl EmailProvider for SmtpEmailProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    /// Delivers `message` over SMTP.
    ///
    /// Any failure (address/header build error, transport connection error,
    /// or server rejection) is folded into a [`EmailStatus::Failed`] result
    /// carrying the error text — the provider never surfaces an `Err` to the
    /// caller, matching pyfly's `_send_blocking` try/except.
    async fn send(&self, message: EmailMessage) -> NotificationResult {
        let built = match build_message(&message) {
            Ok(m) => m,
            Err(e) => return NotificationResult::failed(&message.id, PROVIDER_NAME, e.to_string()),
        };
        let transport = match self.transport() {
            Ok(t) => t,
            Err(e) => return NotificationResult::failed(&message.id, PROVIDER_NAME, e.to_string()),
        };
        match transport.send(built).await {
            Ok(_) => NotificationResult::sent(&message.id, PROVIDER_NAME, None),
            Err(e) => NotificationResult::failed(&message.id, PROVIDER_NAME, e.to_string()),
        }
    }
}

/// Maps a Go-parity [`Notification`] envelope to a rich [`EmailMessage`].
///
/// The single `to`/`subject`/`body` become a one-recipient plain-text e-mail;
/// `template` becomes `template_id`, and `variables` become `template_data`.
fn notification_to_email(n: &Notification) -> EmailMessage {
    EmailMessage {
        id: if n.id.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            n.id.clone()
        },
        to: if n.to.is_empty() {
            Vec::new()
        } else {
            vec![n.to.clone()]
        },
        subject: n.subject.clone(),
        body_text: Some(n.body.clone()),
        template_id: if n.template.is_empty() {
            None
        } else {
            Some(n.template.clone())
        },
        template_data: n.variables.clone().into_iter().collect(),
        ..EmailMessage::default()
    }
}

#[async_trait]
impl notifications::Channel for SmtpEmailProvider {
    /// Implements [`notifications::Channel::kind`]; returns [`Kind::EMAIL`].
    fn kind(&self) -> Kind {
        Kind::EMAIL
    }

    /// Implements [`notifications::Channel::send`] by mapping the thin
    /// envelope to an [`EmailMessage`] and delegating to
    /// [`EmailProvider::send`].
    ///
    /// # Errors
    ///
    /// Returns [`NotificationError::Delivery`] carrying the provider error
    /// text when the rich send reports [`EmailStatus::Failed`].
    async fn send(&self, n: Notification) -> DeliveryResult {
        let result = EmailProvider::send(self, notification_to_email(&n)).await;
        match result.status {
            EmailStatus::Failed => Err(NotificationError::Delivery(
                result
                    .error
                    .unwrap_or_else(|| "smtp delivery failed".into()),
            )),
            _ => Ok(()),
        }
    }

    /// Implements [`notifications::Channel::name`]; returns `"notificationssmtp"`.
    fn name(&self) -> String {
        "notificationssmtp".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn base_msg() -> EmailMessage {
        EmailMessage {
            to: vec!["dest@example.com".into()],
            sender: "from@example.com".into(),
            subject: "Hello SMTP".into(),
            ..EmailMessage::default()
        }
    }

    // -----------------------------------------------------------------
    // build_message structure (port of test_smtp_behavior.py — but
    // asserting the built lettre Message instead of an aiosmtpd capture).
    // -----------------------------------------------------------------

    #[test]
    fn plain_text_message_headers_and_body() {
        let msg = EmailMessage {
            body_text: Some("plain text body".into()),
            ..base_msg()
        };
        let built = build_message(&msg).expect("build");
        let wire = String::from_utf8(built.formatted()).expect("utf8");

        assert!(wire.contains("From: from@example.com"), "{wire}");
        assert!(wire.contains("To: dest@example.com"), "{wire}");
        assert!(wire.contains("Subject: Hello SMTP"), "{wire}");
        assert!(wire.contains("plain text body"), "{wire}");
        assert!(wire.contains("text/plain"), "{wire}");
    }

    #[test]
    fn html_body_produces_text_html_part() {
        let msg = EmailMessage {
            body_text: Some("fallback".into()),
            body_html: Some("<h1>Hello</h1>".into()),
            ..base_msg()
        };
        let built = build_message(&msg).expect("build");
        let wire = String::from_utf8(built.formatted()).expect("utf8");

        assert!(wire.contains("multipart/alternative"), "{wire}");
        assert!(wire.contains("text/plain"), "{wire}");
        assert!(wire.contains("text/html"), "{wire}");
        assert!(wire.contains("<h1>Hello</h1>"), "{wire}");
        assert!(wire.contains("fallback"), "{wire}");
    }

    #[test]
    fn html_only_produces_single_html_part() {
        let msg = EmailMessage {
            body_html: Some("<p>only html</p>".into()),
            ..base_msg()
        };
        let built = build_message(&msg).expect("build");
        let wire = String::from_utf8(built.formatted()).expect("utf8");
        assert!(wire.contains("text/html"), "{wire}");
        assert!(!wire.contains("multipart/alternative"), "{wire}");
    }

    #[test]
    fn attachment_promotes_to_multipart_mixed() {
        let raw = b"binary-attachment-content".to_vec();
        let msg = EmailMessage {
            body_text: Some("see attached".into()),
            attachments: vec![Attachment::new(
                "hello.bin",
                "application/octet-stream",
                raw.clone(),
            )],
            ..base_msg()
        };
        let built = build_message(&msg).expect("build");
        let wire = String::from_utf8(built.formatted()).expect("utf8");

        assert!(wire.contains("multipart/mixed"), "{wire}");
        assert!(wire.contains("application/octet-stream"), "{wire}");
        // filename is carried in the Content-Disposition of the attachment part
        assert!(
            wire.contains("Content-Disposition: attachment; filename=\"hello.bin\""),
            "{wire}"
        );
        // The attachment payload is delivered intact (lettre chooses 7bit
        // here because the bytes are ASCII; binary bytes would be base64).
        let payload = String::from_utf8(raw).unwrap();
        assert!(
            wire.contains(&payload),
            "attachment payload missing: {wire}"
        );
    }

    #[test]
    fn binary_attachment_is_base64_encoded() {
        // Non-ASCII bytes force lettre to base64-encode the part.
        let raw: Vec<u8> = vec![0x00, 0x01, 0x02, 0xff, 0xfe, 0x80, 0x7f, 0x10];
        let msg = EmailMessage {
            body_text: Some("see attached".into()),
            attachments: vec![Attachment::new(
                "blob.bin",
                "application/octet-stream",
                raw.clone(),
            )],
            ..base_msg()
        };
        let built = build_message(&msg).expect("build");
        let wire = String::from_utf8(built.formatted()).expect("utf8");
        assert!(wire.contains("Content-Transfer-Encoding: base64"), "{wire}");
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&raw);
        assert!(wire.contains(&b64), "expected base64 payload in {wire}");
    }

    // -----------------------------------------------------------------
    // BCC is delivered (in the envelope) but never leaked in the headers.
    // -----------------------------------------------------------------

    #[test]
    fn bcc_in_envelope_but_not_in_formatted_headers() {
        let msg = EmailMessage {
            to: vec!["dest@example.com".into()],
            cc: vec!["carbon@example.com".into()],
            bcc: vec!["secret@example.com".into()],
            body_text: Some("hi".into()),
            ..base_msg()
        };
        let built = build_message(&msg).expect("build");

        // Envelope carries every recipient (to + cc + bcc) so the server
        // delivers to all of them.
        let envelope_recipients: Vec<String> = built
            .envelope()
            .to()
            .iter()
            .map(|a| a.to_string())
            .collect();
        assert!(envelope_recipients.contains(&"dest@example.com".to_string()));
        assert!(envelope_recipients.contains(&"carbon@example.com".to_string()));
        assert!(envelope_recipients.contains(&"secret@example.com".to_string()));

        // …but the formatted message must NOT expose the bcc recipient.
        let wire = String::from_utf8(built.formatted()).expect("utf8");
        assert!(
            !wire.to_ascii_lowercase().contains("bcc"),
            "Bcc header leaked: {wire}"
        );
        assert!(
            !wire.contains("secret@example.com"),
            "bcc address leaked: {wire}"
        );
        // cc IS a visible header
        assert!(wire.contains("Cc: carbon@example.com"), "{wire}");
    }

    // -----------------------------------------------------------------
    // Custom headers (non-reserved) appear; reserved ones are ignored.
    // -----------------------------------------------------------------

    #[test]
    fn custom_headers_added_reserved_ignored() {
        let mut headers = BTreeMap::new();
        headers.insert("X-Campaign".to_string(), "spring-sale".to_string());
        headers.insert("X-Priority".to_string(), "1".to_string());
        // a reserved header must be ignored (not clobber lettre's Subject)
        headers.insert("Subject".to_string(), "HIJACKED".to_string());
        let msg = EmailMessage {
            body_text: Some("hi".into()),
            headers,
            ..base_msg()
        };
        let built = build_message(&msg).expect("build");
        let wire = String::from_utf8(built.formatted()).expect("utf8");

        assert!(wire.contains("X-Campaign: spring-sale"), "{wire}");
        assert!(wire.contains("X-Priority: 1"), "{wire}");
        assert!(wire.contains("Subject: Hello SMTP"), "{wire}");
        assert!(
            !wire.contains("HIJACKED"),
            "reserved header was not ignored: {wire}"
        );
    }

    #[test]
    fn invalid_address_is_a_build_error() {
        let msg = EmailMessage {
            to: vec!["not-an-email".into()],
            body_text: Some("hi".into()),
            ..base_msg()
        };
        let err = build_message(&msg).expect_err("invalid address");
        assert!(matches!(err, BuildError::Address { .. }), "{err:?}");
    }

    // -----------------------------------------------------------------
    // Provider send maps a failure to FAILED without raising
    // (port of test_smtp_returns_failed_on_connection_error).
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn send_returns_failed_on_connection_error() {
        // port 1 will not accept; no TLS so we attempt a plain connection
        let provider = SmtpEmailProvider::new(SmtpConfig {
            host: "127.0.0.1".into(),
            port: 1,
            use_tls: false,
            ..SmtpConfig::default()
        });
        let msg = EmailMessage {
            to: vec!["dest@example.com".into()],
            sender: "from@example.com".into(),
            subject: "Will fail".into(),
            body_text: Some("body".into()),
            ..EmailMessage::default()
        };
        let result = EmailProvider::send(&provider, msg.clone()).await;
        assert_eq!(result.status, EmailStatus::Failed);
        assert_eq!(result.provider, "smtp");
        assert_eq!(result.id, msg.id);
        assert!(result.provider_id.is_none());
        assert!(result.error.is_some());
    }

    #[tokio::test]
    async fn send_with_bad_address_fails_without_connecting() {
        let provider = SmtpEmailProvider::new(SmtpConfig {
            host: "127.0.0.1".into(),
            port: 1,
            use_tls: false,
            ..SmtpConfig::default()
        });
        let msg = EmailMessage {
            to: vec!["bad".into()],
            sender: "from@example.com".into(),
            subject: "x".into(),
            body_text: Some("b".into()),
            ..EmailMessage::default()
        };
        let result = EmailProvider::send(&provider, msg).await;
        assert_eq!(result.status, EmailStatus::Failed);
        assert!(result.error.unwrap().contains("invalid address"));
    }

    // -----------------------------------------------------------------
    // Config parsing.
    // -----------------------------------------------------------------

    #[test]
    fn config_default_matches_pyfly() {
        let c = SmtpConfig::default();
        assert_eq!(c.port, 587);
        assert!(c.use_tls);
        assert!(c.username.is_none());
        assert!(c.password.is_none());
    }

    #[test]
    fn from_config_parses_all_keys() {
        let map: BTreeMap<&str, &str> = BTreeMap::from([
            ("host", "smtp.example.com"),
            ("port", "2525"),
            ("username", "apikey"),
            ("password", "secret"),
            ("use_tls", "false"),
        ]);
        let cfg = SmtpConfig::from_config(|k| map.get(k).map(|v| v.to_string()));
        assert_eq!(cfg.host, "smtp.example.com");
        assert_eq!(cfg.port, 2525);
        assert_eq!(cfg.username.as_deref(), Some("apikey"));
        assert_eq!(cfg.password.as_deref(), Some("secret"));
        assert!(!cfg.use_tls);
    }

    #[test]
    fn from_config_uses_defaults_for_missing_and_bad_values() {
        let map: BTreeMap<&str, &str> = BTreeMap::from([
            ("host", "h"),
            ("port", "not-a-number"),
            ("use_tls", "maybe"),
        ]);
        let cfg = SmtpConfig::from_config(|k| map.get(k).map(|v| v.to_string()));
        assert_eq!(cfg.port, 587);
        assert!(cfg.use_tls);
        assert!(cfg.username.is_none());
    }

    #[test]
    fn from_config_empty_credentials_are_none() {
        let map: BTreeMap<&str, &str> =
            BTreeMap::from([("host", "h"), ("username", ""), ("password", "")]);
        let cfg = SmtpConfig::from_config(|k| map.get(k).map(|v| v.to_string()));
        assert!(cfg.username.is_none());
        assert!(cfg.password.is_none());
    }

    // -----------------------------------------------------------------
    // Channel mapping (Go-parity dispatcher integration).
    // -----------------------------------------------------------------

    #[test]
    fn channel_identity() {
        let p = SmtpEmailProvider::new(SmtpConfig::default());
        assert_eq!(notifications::Channel::kind(&p), Kind::EMAIL);
        assert_eq!(notifications::Channel::name(&p), "notificationssmtp");
    }

    #[tokio::test]
    async fn channel_send_surfaces_delivery_error() {
        let p = SmtpEmailProvider::new(SmtpConfig {
            host: "127.0.0.1".into(),
            port: 1,
            use_tls: false,
            ..SmtpConfig::default()
        });
        let err = notifications::Channel::send(
            &p,
            Notification {
                channel: Kind::EMAIL,
                to: "dest@example.com".into(),
                subject: "s".into(),
                body: "b".into(),
                ..Notification::default()
            },
        )
        .await
        .expect_err("connection refused");
        assert!(matches!(err, NotificationError::Delivery(_)));
    }

    #[test]
    fn notification_maps_to_email() {
        let n = Notification {
            id: "n-1".into(),
            channel: Kind::EMAIL,
            to: "a@b.co".into(),
            subject: "Hi".into(),
            body: "body".into(),
            template: "welcome".into(),
            ..Notification::default()
        };
        let email = notification_to_email(&n);
        assert_eq!(email.id, "n-1");
        assert_eq!(email.to, vec!["a@b.co".to_string()]);
        assert_eq!(email.subject, "Hi");
        assert_eq!(email.body_text.as_deref(), Some("body"));
        assert_eq!(email.template_id.as_deref(), Some("welcome"));
    }

    // -----------------------------------------------------------------
    // Result and status wire shapes.
    // -----------------------------------------------------------------

    #[test]
    fn status_wire_strings() {
        assert_eq!(EmailStatus::Sent.as_str(), "SENT");
        assert_eq!(EmailStatus::Failed.as_str(), "FAILED");
        assert_eq!(
            serde_json::to_string(&EmailStatus::Suppressed).unwrap(),
            "\"SUPPRESSED\""
        );
    }

    #[test]
    fn result_omits_none_fields() {
        let sent = NotificationResult::sent("id-1", "smtp", None);
        let json = serde_json::to_string(&sent).unwrap();
        assert!(!json.contains("provider_id"));
        assert!(!json.contains("error"));
        assert!(json.contains("\"status\":\"SENT\""));
    }

    #[test]
    fn provider_implements_email_provider_trait_object() {
        let p: std::sync::Arc<dyn EmailProvider> =
            std::sync::Arc::new(SmtpEmailProvider::new(SmtpConfig::default()));
        assert_eq!(p.name(), "smtp");
    }

    #[test]
    fn types_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SmtpEmailProvider>();
        assert_send_sync::<SmtpConfig>();
        assert_send_sync::<EmailMessage>();
        assert_send_sync::<NotificationResult>();
        assert_send_sync::<std::sync::Arc<dyn EmailProvider>>();
    }
}
