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

//! Ports of pyfly's notifications test cases for the rich parity layer:
//! `tests/notifications/test_template_and_preferences.py` and
//! `tests/notifications/test_optout_per_recipient.py`.
//!
//! Behavior is preserved 1:1; Python idioms are adapted (async fixtures →
//! `#[tokio::test]`, `MagicMock` counter → [`InMemoryNotificationMetrics`],
//! recording providers → small in-process fakes).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(feature = "minijinja")]
use firefly_notifications::MiniJinjaTemplateEngine;
use firefly_notifications::{
    DefaultEmailService, DefaultPushService, DefaultSmsService, DeliveryStatus, DummyEmailProvider,
    DummyPushProvider, DummySmsProvider, EmailMessage, EmailProvider, EmailService,
    InMemoryNotificationMetrics, InMemoryPreferenceService, NoOpTemplateEngine, NotificationResult,
    PreferenceService, PushMessage, PushProvider, PushService, SmsMessage, SmsService,
    TemplateEngine, TemplateError,
};

// ---------------------------------------------------------------------------
// Recording fakes (pyfly _RecordingEmailProvider / _RecordingPushProvider).
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RecordingEmailProvider {
    sent: std::sync::Mutex<Vec<EmailMessage>>,
}

#[async_trait]
impl EmailProvider for RecordingEmailProvider {
    fn name(&self) -> &str {
        "rec-email"
    }

    async fn send(&self, message: EmailMessage) -> Result<NotificationResult, String> {
        let id = message.id.clone();
        self.sent.lock().unwrap().push(message);
        Ok(NotificationResult::sent(id, "rec-email", None))
    }
}

#[derive(Default)]
struct RecordingPushProvider {
    sent: std::sync::Mutex<Vec<PushMessage>>,
}

#[async_trait]
impl PushProvider for RecordingPushProvider {
    fn name(&self) -> &str {
        "rec-push"
    }

    async fn send(&self, message: PushMessage) -> Result<NotificationResult, String> {
        let id = message.id.clone();
        self.sent.lock().unwrap().push(message);
        Ok(NotificationResult::sent(id, "rec-push", None))
    }
}

/// A provider that always errors — drives the FAILED path (pyfly BrokenProvider).
struct BrokenEmailProvider;

#[async_trait]
impl EmailProvider for BrokenEmailProvider {
    fn name(&self) -> &str {
        "broken"
    }

    async fn send(&self, _message: EmailMessage) -> Result<NotificationResult, String> {
        Err("boom".to_string())
    }
}

fn data(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

// ===========================================================================
// MiniJinjaTemplateEngine (pyfly Jinja2TemplateEngine)
// ===========================================================================

#[cfg(feature = "minijinja")]
#[tokio::test]
async fn minijinja_engine_renders_template() {
    let engine = MiniJinjaTemplateEngine::new([(
        "welcome".to_string(),
        "<h1>Hello, {{ name }}!</h1>".to_string(),
    )]);
    let result = engine
        .render("welcome", &data(&[("name", serde_json::json!("Alice"))]))
        .await
        .unwrap();
    assert_eq!(result, "<h1>Hello, Alice!</h1>");
}

#[cfg(feature = "minijinja")]
#[tokio::test]
async fn minijinja_engine_renders_multiple_variables() {
    let engine = MiniJinjaTemplateEngine::new([(
        "order".to_string(),
        "Order #{{ order_id }} for {{ customer }}".to_string(),
    )]);
    let result = engine
        .render(
            "order",
            &data(&[
                ("order_id", serde_json::json!(42)),
                ("customer", serde_json::json!("Bob")),
            ]),
        )
        .await
        .unwrap();
    assert_eq!(result, "Order #42 for Bob");
}

#[cfg(feature = "minijinja")]
#[tokio::test]
async fn minijinja_engine_unknown_template_errors() {
    let engine = MiniJinjaTemplateEngine::new([("a".to_string(), "hello".to_string())]);
    let err = engine
        .render("unknown_tmpl", &HashMap::new())
        .await
        .unwrap_err();
    assert_eq!(
        err,
        TemplateError::UnknownTemplate("unknown_tmpl".to_string())
    );
}

#[cfg(feature = "minijinja")]
#[tokio::test]
async fn minijinja_engine_autoescapes_html() {
    // autoescape=True must escape user-supplied HTML in variables.
    let engine =
        MiniJinjaTemplateEngine::new([("tmpl".to_string(), "<p>{{ body }}</p>".to_string())]);
    let result = engine
        .render(
            "tmpl",
            &data(&[("body", serde_json::json!("<script>alert(1)</script>"))]),
        )
        .await
        .unwrap();
    assert!(!result.contains("<script>"));
    assert!(result.contains("&lt;script&gt;"));
}

// ===========================================================================
// NoOpTemplateEngine
// ===========================================================================

#[tokio::test]
async fn noop_engine_errors_not_implemented() {
    let engine = NoOpTemplateEngine::new();
    let err = engine.render("any", &HashMap::new()).await.unwrap_err();
    assert_eq!(err, TemplateError::NotImplemented("any".to_string()));
    assert!(err.to_string().contains("NoOpTemplateEngine"));
}

// ===========================================================================
// Render-then-send integration
// ===========================================================================

#[cfg(feature = "minijinja")]
#[tokio::test]
async fn service_renders_template_and_clears_template_id() {
    let engine = Arc::new(MiniJinjaTemplateEngine::new([(
        "greet".to_string(),
        "<p>Hi {{ user }}</p>".to_string(),
    )]));
    let provider = Arc::new(DummyEmailProvider::new());
    let service = DefaultEmailService::new(provider.clone()).with_template_engine(engine);

    let msg = EmailMessage {
        to: vec!["u@example.com".to_string()],
        sender: "s@example.com".to_string(),
        subject: "Test".to_string(),
        template_id: Some("greet".to_string()),
        template_data: data(&[("user", serde_json::json!("Carol"))]),
        ..EmailMessage::new()
    };
    let result = service.send(msg).await;

    assert_eq!(result.status, DeliveryStatus::Sent);
    let sent = provider.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].body_html.as_deref(), Some("<p>Hi Carol</p>"));
    // template_id cleared so provider-native routing is NOT triggered.
    assert_eq!(sent[0].template_id, None);
    assert!(sent[0].template_data.is_empty());
}

#[cfg(feature = "minijinja")]
#[tokio::test]
async fn service_skips_render_when_no_template_id() {
    let engine = Arc::new(MiniJinjaTemplateEngine::new([(
        "t".to_string(),
        "x".to_string(),
    )]));
    let provider = Arc::new(DummyEmailProvider::new());
    let service = DefaultEmailService::new(provider.clone()).with_template_engine(engine);

    let msg = EmailMessage {
        to: vec!["u@example.com".to_string()],
        sender: "s@example.com".to_string(),
        subject: "Plain".to_string(),
        body_text: Some("just text".to_string()),
        ..EmailMessage::new()
    };
    let result = service.send(msg).await;

    assert_eq!(result.status, DeliveryStatus::Sent);
    let sent = provider.sent();
    assert_eq!(sent[0].body_html, None);
    assert_eq!(sent[0].body_text.as_deref(), Some("just text"));
}

#[tokio::test]
async fn service_passes_template_id_through_when_no_engine() {
    let provider = Arc::new(DummyEmailProvider::new());
    let service = DefaultEmailService::new(provider.clone());

    let msg = EmailMessage {
        to: vec!["u@example.com".to_string()],
        sender: "s@example.com".to_string(),
        subject: "Native".to_string(),
        template_id: Some("d-abc123".to_string()),
        template_data: data(&[("k", serde_json::json!("v"))]),
        ..EmailMessage::new()
    };
    let result = service.send(msg).await;

    assert_eq!(result.status, DeliveryStatus::Sent);
    let sent = provider.sent();
    assert_eq!(sent[0].template_id.as_deref(), Some("d-abc123"));
    assert_eq!(
        sent[0].template_data,
        data(&[("k", serde_json::json!("v"))])
    );
}

// ===========================================================================
// Preference / opt-out (test_template_and_preferences.py)
// ===========================================================================

#[tokio::test]
async fn email_opted_out_returns_suppressed_without_calling_provider() {
    let prefs = Arc::new(InMemoryPreferenceService::new());
    prefs.opt_out("alice@example.com", "email");
    let provider = Arc::new(DummyEmailProvider::new());
    let service = DefaultEmailService::new(provider.clone()).with_preference_service(prefs);

    let msg = EmailMessage {
        to: vec!["alice@example.com".to_string()],
        sender: "s@example.com".to_string(),
        subject: "Promo".to_string(),
        ..EmailMessage::new()
    };
    let result = service.send(msg).await;

    assert_eq!(result.status, DeliveryStatus::Suppressed);
    assert_eq!(provider.sent().len(), 0);
}

#[tokio::test]
async fn email_opted_in_delivers_normally() {
    let prefs = Arc::new(InMemoryPreferenceService::new());
    let provider = Arc::new(DummyEmailProvider::new());
    let service = DefaultEmailService::new(provider.clone()).with_preference_service(prefs);

    let msg = EmailMessage {
        to: vec!["bob@example.com".to_string()],
        sender: "s@example.com".to_string(),
        subject: "Hi".to_string(),
        ..EmailMessage::new()
    };
    let result = service.send(msg).await;

    assert_eq!(result.status, DeliveryStatus::Sent);
    assert_eq!(provider.sent().len(), 1);
}

#[tokio::test]
async fn email_opt_out_then_opt_in_delivers() {
    let prefs = Arc::new(InMemoryPreferenceService::new());
    prefs.opt_out("carol@example.com", "email");
    prefs.opt_in("carol@example.com", "email");
    let provider = Arc::new(DummyEmailProvider::new());
    let service = DefaultEmailService::new(provider).with_preference_service(prefs);

    let msg = EmailMessage {
        to: vec!["carol@example.com".to_string()],
        sender: "s@example.com".to_string(),
        subject: "Back".to_string(),
        ..EmailMessage::new()
    };
    assert_eq!(service.send(msg).await.status, DeliveryStatus::Sent);
}

#[tokio::test]
async fn sms_opted_out_returns_suppressed() {
    let prefs = Arc::new(InMemoryPreferenceService::new());
    prefs.opt_out("+10000000000", "sms");
    let provider = Arc::new(DummySmsProvider::new());
    let service = DefaultSmsService::new(provider.clone()).with_preference_service(prefs);

    let result = service
        .send(SmsMessage {
            to: "+10000000000".to_string(),
            body: "hi".to_string(),
            ..SmsMessage::new()
        })
        .await;
    assert_eq!(result.status, DeliveryStatus::Suppressed);
    assert_eq!(provider.sent().len(), 0);
}

#[tokio::test]
async fn push_opted_out_returns_suppressed() {
    let prefs = Arc::new(InMemoryPreferenceService::new());
    prefs.opt_out("device-token-xyz", "push");
    let provider = Arc::new(DummyPushProvider::new());
    let service = DefaultPushService::new(provider.clone()).with_preference_service(prefs);

    let result = service
        .send(PushMessage {
            device_tokens: vec!["device-token-xyz".to_string()],
            title: "hi".to_string(),
            body: "body".to_string(),
            ..PushMessage::new()
        })
        .await;
    assert_eq!(result.status, DeliveryStatus::Suppressed);
    assert_eq!(provider.sent().len(), 0);
}

// ===========================================================================
// Metrics (test_template_and_preferences.py)
// ===========================================================================

#[tokio::test]
async fn metrics_sent_counter_increments_on_success() {
    let metrics = Arc::new(InMemoryNotificationMetrics::new());
    let provider = Arc::new(DummyEmailProvider::new());
    let service = DefaultEmailService::new(provider).with_metrics(metrics.clone());

    let msg = EmailMessage {
        to: vec!["u@example.com".to_string()],
        sender: "s@example.com".to_string(),
        subject: "m".to_string(),
        ..EmailMessage::new()
    };
    assert_eq!(service.send(msg).await.status, DeliveryStatus::Sent);

    let calls = metrics.sent_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].get("channel").map(String::as_str), Some("email"));
    assert_eq!(calls[0].get("provider").map(String::as_str), Some("dummy"));
}

#[tokio::test]
async fn metrics_failed_counter_increments_on_failure() {
    let metrics = Arc::new(InMemoryNotificationMetrics::new());
    let service =
        DefaultEmailService::new(Arc::new(BrokenEmailProvider)).with_metrics(metrics.clone());

    let msg = EmailMessage {
        to: vec!["u@example.com".to_string()],
        sender: "s@example.com".to_string(),
        subject: "x".to_string(),
        ..EmailMessage::new()
    };
    let result = service.send(msg).await;
    assert_eq!(result.status, DeliveryStatus::Failed);
    assert_eq!(result.error.as_deref(), Some("boom"));

    let calls = metrics.failed_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].get("channel").map(String::as_str), Some("email"));
    assert_eq!(calls[0].get("provider").map(String::as_str), Some("broken"));
}

#[tokio::test]
async fn metrics_suppressed_counter_increments_on_opt_out() {
    let prefs = Arc::new(InMemoryPreferenceService::new());
    prefs.opt_out("x@x.io", "email");
    let provider = Arc::new(DummyEmailProvider::new());
    let metrics = Arc::new(InMemoryNotificationMetrics::new());
    let service = DefaultEmailService::new(provider)
        .with_preference_service(prefs)
        .with_metrics(metrics.clone());

    let msg = EmailMessage {
        to: vec!["x@x.io".to_string()],
        sender: "s@x.io".to_string(),
        subject: "y".to_string(),
        ..EmailMessage::new()
    };
    assert_eq!(service.send(msg).await.status, DeliveryStatus::Suppressed);

    let calls = metrics.suppressed_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].get("channel").map(String::as_str), Some("email"));
}

#[tokio::test]
async fn no_metrics_no_error() {
    let provider = Arc::new(DummyEmailProvider::new());
    let service = DefaultEmailService::new(provider);
    let msg = EmailMessage {
        to: vec!["u@example.com".to_string()],
        sender: "s@example.com".to_string(),
        subject: "x".to_string(),
        ..EmailMessage::new()
    };
    assert_eq!(service.send(msg).await.status, DeliveryStatus::Sent);
}

// ===========================================================================
// Opt-out per recipient (test_optout_per_recipient.py)
// ===========================================================================

#[tokio::test]
async fn email_optout_prunes_cc_and_bcc_not_just_first() {
    let provider = Arc::new(RecordingEmailProvider::default());
    let prefs = Arc::new(InMemoryPreferenceService::new());
    prefs.opt_out("blocked@x.com", "email");
    let service = DefaultEmailService::new(provider.clone()).with_preference_service(prefs);

    let result = service
        .send(EmailMessage {
            to: vec!["alice@x.com".to_string(), "blocked@x.com".to_string()],
            cc: vec!["blocked@x.com".to_string(), "carol@x.com".to_string()],
            bcc: vec!["blocked@x.com".to_string()],
            sender: "me@x.com".to_string(),
            subject: "hi".to_string(),
            body_text: Some("body".to_string()),
            ..EmailMessage::new()
        })
        .await;

    assert_eq!(result.status, DeliveryStatus::Sent);
    let delivered = &provider.sent.lock().unwrap()[0];
    assert_eq!(delivered.to, vec!["alice@x.com".to_string()]);
    assert_eq!(delivered.cc, vec!["carol@x.com".to_string()]);
    assert!(delivered.bcc.is_empty());
}

#[tokio::test]
async fn email_all_recipients_optout_suppresses_and_skips_provider() {
    let provider = Arc::new(RecordingEmailProvider::default());
    let prefs = Arc::new(InMemoryPreferenceService::new());
    prefs.opt_out("a@x.com", "email");
    prefs.opt_out("b@x.com", "email");
    let service = DefaultEmailService::new(provider.clone()).with_preference_service(prefs);

    let result = service
        .send(EmailMessage {
            to: vec!["a@x.com".to_string()],
            cc: vec!["b@x.com".to_string()],
            sender: "me@x.com".to_string(),
            subject: "s".to_string(),
            body_text: Some("b".to_string()),
            ..EmailMessage::new()
        })
        .await;

    assert_eq!(result.status, DeliveryStatus::Suppressed);
    assert!(provider.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn push_optout_prunes_individual_tokens() {
    let provider = Arc::new(RecordingPushProvider::default());
    let prefs = Arc::new(InMemoryPreferenceService::new());
    prefs.opt_out("tok-bad", "push");
    let service = DefaultPushService::new(provider.clone()).with_preference_service(prefs);

    let result = service
        .send(PushMessage {
            device_tokens: vec!["tok-good".to_string(), "tok-bad".to_string()],
            title: "t".to_string(),
            body: "b".to_string(),
            ..PushMessage::new()
        })
        .await;

    assert_eq!(result.status, DeliveryStatus::Sent);
    assert_eq!(
        provider.sent.lock().unwrap()[0].device_tokens,
        vec!["tok-good".to_string()]
    );
}

#[tokio::test]
async fn push_all_tokens_optout_suppresses() {
    let provider = Arc::new(RecordingPushProvider::default());
    let prefs = Arc::new(InMemoryPreferenceService::new());
    prefs.opt_out("tok-1", "push");
    let service = DefaultPushService::new(provider.clone()).with_preference_service(prefs);

    let result = service
        .send(PushMessage {
            device_tokens: vec!["tok-1".to_string()],
            title: "t".to_string(),
            body: "b".to_string(),
            ..PushMessage::new()
        })
        .await;

    assert_eq!(result.status, DeliveryStatus::Suppressed);
    assert!(provider.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn email_optout_is_case_insensitive() {
    let prefs = InMemoryPreferenceService::new();
    prefs.opt_out("Alice@X.com", "email");
    assert!(!prefs.is_opted_in("alice@x.com", "email").await);
    assert!(!prefs.is_opted_in("  ALICE@x.COM ", "email").await);
    assert!(prefs.is_opted_in("bob@x.com", "email").await);
}

#[tokio::test]
async fn sms_optout_normalizes_phone_formatting() {
    let prefs = InMemoryPreferenceService::new();
    prefs.opt_out("+1 (555) 123-4567", "sms");
    assert!(!prefs.is_opted_in("+15551234567", "sms").await);
}

// ===========================================================================
// Regression tests for confirmed parity bugs.
// ===========================================================================

/// Bug 1 regression: a template-render failure is reported as FAILED but must
/// NOT increment the `failed` metric. pyfly calls `engine.render(...)` outside
/// any try/except, so a render error raises out of `send()` before the metrics
/// tail and the `failed` counter is never touched. The earlier Rust port folded
/// the render error into a FAILED result AND bumped `failed`, an undocumented
/// metric side-effect that diverged from pyfly.
#[tokio::test]
async fn render_failure_is_failed_but_records_no_metric() {
    // NoOpTemplateEngine always errors on render (pyfly's NoOpTemplateEngine).
    let engine = Arc::new(NoOpTemplateEngine::new());
    let provider = Arc::new(DummyEmailProvider::new());
    let metrics = Arc::new(InMemoryNotificationMetrics::new());
    let service = DefaultEmailService::new(provider.clone())
        .with_template_engine(engine)
        .with_metrics(metrics.clone());

    let msg = EmailMessage {
        to: vec!["u@example.com".to_string()],
        sender: "s@example.com".to_string(),
        subject: "Test".to_string(),
        template_id: Some("never-registered".to_string()),
        ..EmailMessage::new()
    };
    let result = service.send(msg).await;

    // The render error surfaces as a FAILED result carrying the error string.
    assert_eq!(result.status, DeliveryStatus::Failed);
    assert!(result.error.is_some());
    // The provider was never called (render happens before delivery).
    assert_eq!(provider.sent().len(), 0);
    // Crucially: NO metric was incremented on a render failure (pyfly parity).
    assert!(
        metrics.failed_calls().is_empty(),
        "render failure must not bump the `failed` counter"
    );
    assert!(metrics.sent_calls().is_empty());
    assert!(metrics.suppressed_calls().is_empty());
}

/// Bug 2 regression: SMS opt-out normalization must use Unicode-aware digit
/// detection (pyfly's `str.isdigit()`), not an ASCII-only filter.
///
/// pyfly's `_normalize` *keeps* every `isdigit()` character verbatim (it does
/// not transliterate to ASCII), so a Unicode-digit phone number preserves its
/// digits in the opt-out key. The earlier Rust port filtered with
/// `is_ascii_digit`, which **stripped** every non-ASCII digit — collapsing
/// distinct Unicode-digit numbers to the same key (e.g. both `+１` and `+２`
/// normalize to just `"+"`), causing false suppression collisions and a
/// cross-port mismatch with pyfly.
#[tokio::test]
async fn sms_optout_uses_unicode_digit_filter() {
    // --- Distinct fullwidth-digit numbers must NOT collide. ---
    // Fullwidth digits ０-９ (U+FF10..) are Unicode `Nd` — Python isdigit True.
    let prefs = InMemoryPreferenceService::new();
    prefs.opt_out("+\u{FF11}", "sms"); // "+１"
                                       // Opting out "+１" must NOT suppress the different number "+２".
                                       // Under the old ASCII filter both collapsed to "+", a false match.
    assert!(
        prefs.is_opted_in("+\u{FF12}", "sms").await, // "+２" still opted IN
        "distinct fullwidth-digit numbers must not collide to the same opt-out key"
    );
    // The exact same fullwidth number IS suppressed (digits are preserved).
    assert!(!prefs.is_opted_in("+\u{FF11}", "sms").await);
    // And an ASCII "+1" is a DIFFERENT key (digits kept verbatim, pyfly parity).
    assert!(prefs.is_opted_in("+1", "sms").await);

    // --- Arabic-Indic digits ٠-٩ (U+0660..) are also `Nd` and preserved. ---
    let prefs2 = InMemoryPreferenceService::new();
    prefs2.opt_out("+\u{0661}\u{0662}\u{0663}", "sms"); // "+١٢٣"
    assert!(!prefs2.is_opted_in("+\u{0661}\u{0662}\u{0663}", "sms").await);
    // Distinct Arabic-Indic number stays opted in (no ASCII-strip collision).
    assert!(prefs2.is_opted_in("+\u{0664}\u{0665}\u{0666}", "sms").await); // "+٤٥٦"

    // --- Superscript digits ¹²³ are NOT decimal but ARE isdigit-True in
    //     Python (`Numeric_Type=Digit`); they must be KEPT by the filter,
    //     matching pyfly. An ASCII-only filter would have stripped them. ---
    let prefs3 = InMemoryPreferenceService::new();
    prefs3.opt_out("\u{00B2}\u{00B3}", "sms"); // "²³"
    assert!(
        !prefs3.is_opted_in("\u{00B2}\u{00B3}", "sms").await,
        "superscript digits are isdigit-True and must be kept (pyfly parity)"
    );
    // Distinct superscript pair stays opted in (digits preserved, not stripped).
    assert!(prefs3.is_opted_in("\u{2074}\u{2075}", "sms").await); // "⁴⁵"
}
