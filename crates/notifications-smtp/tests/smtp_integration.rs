//! Env-gated live SMTP round-trip for [`firefly_notifications_smtp::SmtpEmailProvider`]
//! against MailHog.
//!
//! This is an **env-gated** integration test, not `#[ignore]`-gated. It reads
//! `FIREFLY_TEST_SMTP_ADDR` (e.g. `localhost:1026`, the MailHog SMTP listener);
//! when that variable is **unset** it prints a one-line `skipping …` and
//! returns, so `cargo test` on a bare machine is green. When it is **set** the
//! test sends a real e-mail through the provider and then queries the MailHog
//! HTTP API to assert the message arrived with the expected subject and
//! recipient.
//!
//! Run against the docker-compose stack with:
//!
//! ```sh
//! export FIREFLY_TEST_SMTP_ADDR="localhost:1026"
//! cargo test -p firefly-notifications-smtp --test smtp_integration
//! ```
//!
//! The MailHog HTTP API base defaults to `http://<smtp-host>:8025`; override it
//! with `FIREFLY_TEST_MAILHOG_API` (the v2 messages endpoint is appended).
//!
//! Every send uses a unique subject token (derived from the test fn name, the
//! process id, and a process-wide atomic counter — never a random source), so
//! the verification matches exactly this test's message and runs are safe in
//! parallel and on a shared MailHog instance.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use firefly_notifications_smtp::{
    EmailMessage, EmailProvider, EmailStatus, SmtpConfig, SmtpEmailProvider,
};

/// Process-wide monotonic counter for unique subject tokens.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Reads the MailHog SMTP address (`host:port`) from the standard env var.
/// Returns `None` when unset so callers can early-skip.
fn smtp_addr() -> Option<String> {
    std::env::var("FIREFLY_TEST_SMTP_ADDR")
        .ok()
        .filter(|s| !s.is_empty())
}

/// Splits `host:port` into its parts, defaulting the port to 1025 (MailHog).
fn split_addr(addr: &str) -> (String, u16) {
    match addr.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(1025)),
        None => (addr.to_string(), 1025),
    }
}

/// The MailHog HTTP API base — `FIREFLY_TEST_MAILHOG_API` if set, else
/// `http://<smtp-host>:8025` (MailHog's default UI/API port).
fn mailhog_api_base(smtp_host: &str) -> String {
    std::env::var("FIREFLY_TEST_MAILHOG_API")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("http://{smtp_host}:8025"))
}

/// A unique token for this `slug`, stable within a process but distinct per
/// call, so concurrent tests never collide on a shared MailHog.
fn unique_token(slug: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("fftest-smtp-{slug}-{}-{n}", std::process::id())
}

/// Polls the MailHog search API for a message whose Subject contains `token`,
/// returning the matching message JSON (retrying briefly for delivery latency).
async fn find_message(api_base: &str, token: &str) -> Option<serde_json::Value> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v2/search", api_base.trim_end_matches('/'));
    for _ in 0..20 {
        let resp = client
            .get(&url)
            .query(&[("kind", "containing"), ("query", token)])
            .send()
            .await;
        if let Ok(resp) = resp {
            if resp.status().is_success() {
                if let Ok(body) = resp.json::<serde_json::Value>().await {
                    if let Some(items) = body.get("items").and_then(|v| v.as_array()) {
                        if let Some(first) = items.first() {
                            return Some(first.clone());
                        }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    None
}

#[tokio::test]
async fn live_smtp_round_trip_delivers_and_is_visible_in_mailhog() {
    let Some(addr) = smtp_addr() else {
        eprintln!(
            "skipping live_smtp_round_trip_delivers_and_is_visible_in_mailhog: \
             set FIREFLY_TEST_SMTP_ADDR (e.g. localhost:1026) to run"
        );
        return;
    };
    let (host, port) = split_addr(&addr);
    let api_base = mailhog_api_base(&host);
    let token = unique_token("roundtrip");

    // MailHog speaks plain SMTP with no auth and no TLS.
    let provider = SmtpEmailProvider::new(SmtpConfig {
        host: host.clone(),
        port,
        username: None,
        password: None,
        use_tls: false,
    });

    let recipient = format!("{token}@firefly.test");
    let subject = format!("Firefly SMTP round-trip {token}");
    let msg = EmailMessage {
        to: vec![recipient.clone()],
        sender: "sender@firefly.test".into(),
        subject: subject.clone(),
        body_text: Some(format!("hello from firefly integration test {token}")),
        ..EmailMessage::default()
    };

    let result = EmailProvider::send(&provider, msg).await;
    assert_eq!(
        result.status,
        EmailStatus::Sent,
        "send failed: {:?}",
        result.error
    );
    assert_eq!(result.provider, "smtp");

    // Verify the message reached MailHog with the right subject + recipient.
    let found = find_message(&api_base, &token)
        .await
        .expect("MailHog should have received the message");

    let subject_seen = found
        .pointer("/Content/Headers/Subject/0")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        subject_seen.contains(&token),
        "subject mismatch: got {subject_seen:?}, want token {token}"
    );

    let to_seen = found
        .pointer("/Content/Headers/To/0")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        to_seen.contains(&recipient),
        "recipient mismatch: got {to_seen:?}, want {recipient}"
    );

    // Clean up: delete every message MailHog matched for this unique token so
    // repeated runs stay tidy. Deleting by message id is the precise path.
    if let Some(id) = found.get("ID").and_then(|v| v.as_str()) {
        let client = reqwest::Client::new();
        let del = format!("{}/api/v1/messages/{id}", api_base.trim_end_matches('/'));
        let _ = client.delete(&del).send().await;
    }
}
