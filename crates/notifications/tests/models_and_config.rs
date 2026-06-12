//! Unit tests for the rich models, config selection, and dummy providers
//! (pyfly parity surface), not directly mirrored by a pyfly test file but
//! covering the wire shapes and selection logic ported from pyfly.

use firefly_notifications::{
    Attachment, DeliveryStatus, EmailMessage, EmailProviderSelection, NotificationResult,
    PreferenceStoreSelection, PushMessage, PushProviderSelection, SmsMessage, SmsProviderSelection,
    TemplateEngineSelection,
};

#[test]
fn delivery_status_wire_values_match_pyfly_strenum() {
    assert_eq!(DeliveryStatus::Queued.as_str(), "QUEUED");
    assert_eq!(DeliveryStatus::Sent.as_str(), "SENT");
    assert_eq!(DeliveryStatus::Delivered.as_str(), "DELIVERED");
    assert_eq!(DeliveryStatus::Bounced.as_str(), "BOUNCED");
    assert_eq!(DeliveryStatus::Failed.as_str(), "FAILED");
    assert_eq!(DeliveryStatus::Suppressed.as_str(), "SUPPRESSED");
    // JSON serialization uses the same upper-case wire value.
    assert_eq!(
        serde_json::to_string(&DeliveryStatus::Sent).unwrap(),
        "\"SENT\""
    );
    let back: DeliveryStatus = serde_json::from_str("\"SUPPRESSED\"").unwrap();
    assert_eq!(back, DeliveryStatus::Suppressed);
}

#[test]
fn email_message_defaults_assign_unique_ids() {
    let a = EmailMessage::new();
    let b = EmailMessage::new();
    assert_ne!(a.id, b.id, "each message gets a fresh uuid");
    assert!(a.to.is_empty());
    assert!(a.cc.is_empty());
    assert!(a.bcc.is_empty());
    assert_eq!(a.body_text, None);
    assert_eq!(a.body_html, None);
    assert_eq!(a.template_id, None);
}

#[test]
fn sms_and_push_defaults() {
    let s = SmsMessage::new();
    assert!(s.to.is_empty());
    assert_eq!(s.sender, None);
    let p = PushMessage::new();
    assert!(p.device_tokens.is_empty());
    assert!(p.data.is_empty());
}

#[test]
fn attachment_holds_raw_bytes() {
    let att = Attachment::new("report.pdf", "application/pdf", b"%PDF-1.4".to_vec());
    assert_eq!(att.filename, "report.pdf");
    assert_eq!(att.content_type, "application/pdf");
    assert_eq!(att.data, b"%PDF-1.4");
}

#[test]
fn notification_result_constructors() {
    let sent = NotificationResult::sent("id-1", "dummy", Some("pid-1".to_string()));
    assert_eq!(sent.status, DeliveryStatus::Sent);
    assert_eq!(sent.provider_id.as_deref(), Some("pid-1"));
    assert_eq!(sent.error, None);

    let supp = NotificationResult::suppressed("id-2", "dummy");
    assert_eq!(supp.status, DeliveryStatus::Suppressed);
    assert_eq!(supp.provider_id, None);

    let failed = NotificationResult::failed("id-3", "broken", "boom");
    assert_eq!(failed.status, DeliveryStatus::Failed);
    assert_eq!(failed.error.as_deref(), Some("boom"));
}

#[test]
fn notification_result_omits_none_fields_in_json() {
    let supp = NotificationResult::suppressed("id-2", "dummy");
    let json = serde_json::to_string(&supp).unwrap();
    assert_eq!(
        json,
        r#"{"id":"id-2","provider":"dummy","status":"SUPPRESSED"}"#
    );
}

#[test]
fn email_provider_selection_from_config() {
    assert_eq!(
        EmailProviderSelection::from_config("sendgrid"),
        EmailProviderSelection::SendGrid
    );
    assert_eq!(
        EmailProviderSelection::from_config("RESEND"),
        EmailProviderSelection::Resend
    );
    assert_eq!(
        EmailProviderSelection::from_config(" smtp "),
        EmailProviderSelection::Smtp
    );
    assert_eq!(
        EmailProviderSelection::from_config("dummy"),
        EmailProviderSelection::Dummy
    );
    // Unknown / empty → dummy fallback (pyfly's `else: DummyEmailProvider()`).
    assert_eq!(
        EmailProviderSelection::from_config("ses"),
        EmailProviderSelection::Dummy
    );
    assert_eq!(
        EmailProviderSelection::from_config(""),
        EmailProviderSelection::Dummy
    );
}

#[test]
fn sms_and_push_selection_from_config() {
    assert_eq!(
        SmsProviderSelection::from_config("twilio"),
        SmsProviderSelection::Twilio
    );
    assert_eq!(
        SmsProviderSelection::from_config("nexmo"),
        SmsProviderSelection::Dummy
    );
    assert_eq!(
        PushProviderSelection::from_config("firebase"),
        PushProviderSelection::Firebase
    );
    assert_eq!(
        PushProviderSelection::from_config("apns"),
        PushProviderSelection::Dummy
    );
}

#[test]
fn template_and_store_selection_from_config() {
    assert_eq!(
        TemplateEngineSelection::from_config("jinja2"),
        TemplateEngineSelection::MiniJinja
    );
    assert_eq!(
        TemplateEngineSelection::from_config("minijinja"),
        TemplateEngineSelection::MiniJinja
    );
    assert_eq!(
        TemplateEngineSelection::from_config("none"),
        TemplateEngineSelection::None
    );
    assert_eq!(
        PreferenceStoreSelection::from_config("memory"),
        PreferenceStoreSelection::Memory
    );
    assert_eq!(
        PreferenceStoreSelection::from_config("none"),
        PreferenceStoreSelection::None
    );
}
