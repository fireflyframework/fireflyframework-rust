//! Default notification orchestration services (pyfly parity).
//!
//! The Rust counterpart of `pyfly.notifications.services`. Each service wraps a
//! provider and layers on the optional capabilities — preference-based opt-out
//! pruning, local template rendering, and metric counters — exactly as pyfly's
//! `DefaultEmailService` / `DefaultSmsService` / `DefaultPushService` do.
//!
//! # Behavior (matching pyfly)
//!
//! * **Opt-out pruning** — when a [`PreferenceService`] is injected, EVERY
//!   recipient is checked: for e-mail the full `to` + `cc` + `bcc` set, for push
//!   every device token. Opted-out recipients are pruned so the provider never
//!   delivers to them; each pruned recipient increments the `suppressed`
//!   counter. When *all* recipients have opted out, a
//!   [`DeliveryStatus::Suppressed`] result is returned **without** calling the
//!   provider.
//! * **Template precedence** — when a [`TemplateEngine`] is injected and the
//!   email has a `template_id`, the engine renders it into `body_html` and the
//!   provider-native `template_id` / `template_data` are cleared. Without an
//!   engine, those fields are forwarded untouched.
//! * **Error → FAILED** — a provider `Err(...)` is folded into a
//!   [`DeliveryStatus::Failed`] result (never propagated), matching pyfly's
//!   `_send_safely`.
//! * **Metrics** — on `SENT` the sent counter is incremented; on `FAILED` the
//!   failed counter; suppressed recipients increment the suppressed counter.

use std::sync::Arc;

use async_trait::async_trait;

use crate::metrics::NotificationMetrics;
use crate::models::{DeliveryStatus, EmailMessage, NotificationResult, PushMessage, SmsMessage};
use crate::ports::{
    EmailProvider, EmailService, PushProvider, PushService, SmsProvider, SmsService,
};
use crate::preferences::PreferenceService;
use crate::template::TemplateEngine;

/// Filters `addresses` to those opted IN to `channel`.
///
/// Opted-out addresses are dropped and (when `metrics` is present) counted as
/// suppressed. Mirrors pyfly's `_filter_opted_in` — every recipient is checked,
/// closing the cc/bcc / multi-token opt-out bypass.
async fn filter_opted_in(
    prefs: &dyn PreferenceService,
    metrics: Option<&dyn NotificationMetrics>,
    addresses: &[String],
    channel: &str,
) -> Vec<String> {
    let mut kept = Vec::new();
    for addr in addresses {
        if prefs.is_opted_in(addr, channel).await {
            kept.push(addr.clone());
        } else if let Some(m) = metrics {
            m.record_suppressed(channel);
        }
    }
    kept
}

/// Default [`EmailService`] implementation (pyfly `DefaultEmailService`).
///
/// Build with [`DefaultEmailService::new`] for the bare provider, then layer on
/// optional capabilities with [`with_template_engine`](DefaultEmailService::with_template_engine),
/// [`with_preference_service`](DefaultEmailService::with_preference_service),
/// and [`with_metrics`](DefaultEmailService::with_metrics).
pub struct DefaultEmailService {
    provider: Arc<dyn EmailProvider>,
    template_engine: Option<Arc<dyn TemplateEngine>>,
    preference_service: Option<Arc<dyn PreferenceService>>,
    metrics: Option<Arc<dyn NotificationMetrics>>,
}

impl DefaultEmailService {
    /// Builds a service that simply delegates to `provider`.
    pub fn new(provider: Arc<dyn EmailProvider>) -> Self {
        DefaultEmailService {
            provider,
            template_engine: None,
            preference_service: None,
            metrics: None,
        }
    }

    /// Injects a local [`TemplateEngine`] (enables local rendering precedence).
    #[must_use]
    pub fn with_template_engine(mut self, engine: Arc<dyn TemplateEngine>) -> Self {
        self.template_engine = Some(engine);
        self
    }

    /// Injects a [`PreferenceService`] (enables opt-out pruning).
    #[must_use]
    pub fn with_preference_service(mut self, prefs: Arc<dyn PreferenceService>) -> Self {
        self.preference_service = Some(prefs);
        self
    }

    /// Injects a [`NotificationMetrics`] recorder.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn NotificationMetrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }
}

#[async_trait]
impl EmailService for DefaultEmailService {
    async fn send(&self, mut message: EmailMessage) -> NotificationResult {
        let metrics = self.metrics.as_deref();

        // 1. Per-recipient opt-out filtering (to + cc + bcc, not just the first).
        if let Some(prefs) = self.preference_service.as_deref() {
            let had_recipients =
                !message.to.is_empty() || !message.cc.is_empty() || !message.bcc.is_empty();
            message.to = filter_opted_in(prefs, metrics, &message.to, "email").await;
            message.cc = filter_opted_in(prefs, metrics, &message.cc, "email").await;
            message.bcc = filter_opted_in(prefs, metrics, &message.bcc, "email").await;
            if had_recipients
                && message.to.is_empty()
                && message.cc.is_empty()
                && message.bcc.is_empty()
            {
                return NotificationResult::suppressed(message.id, self.provider.name());
            }
        }

        // 2. Local template rendering (takes priority over provider-native).
        if let Some(engine) = self.template_engine.as_deref() {
            if let Some(template_id) = message.template_id.clone() {
                match engine.render(&template_id, &message.template_data).await {
                    Ok(rendered) => {
                        message.body_html = Some(rendered);
                        message.template_id = None;
                        message.template_data.clear();
                    }
                    Err(e) => {
                        let result = NotificationResult::failed(
                            message.id,
                            self.provider.name(),
                            e.to_string(),
                        );
                        if let Some(m) = metrics {
                            m.record_failed("email", &result.provider);
                        }
                        return result;
                    }
                }
            }
        }

        // 3. Send via provider, folding any Err into a FAILED result.
        let id = message.id.clone();
        let provider_name = self.provider.name().to_string();
        let result = match self.provider.send(message).await {
            Ok(r) => r,
            Err(e) => NotificationResult::failed(id, provider_name, e),
        };

        // 4. Metrics.
        record_outcome(metrics, "email", &result);
        result
    }
}

/// Default [`SmsService`] implementation (pyfly `DefaultSmsService`).
pub struct DefaultSmsService {
    provider: Arc<dyn SmsProvider>,
    preference_service: Option<Arc<dyn PreferenceService>>,
    metrics: Option<Arc<dyn NotificationMetrics>>,
}

impl DefaultSmsService {
    /// Builds a service that simply delegates to `provider`.
    pub fn new(provider: Arc<dyn SmsProvider>) -> Self {
        DefaultSmsService {
            provider,
            preference_service: None,
            metrics: None,
        }
    }

    /// Injects a [`PreferenceService`] (enables opt-out suppression).
    #[must_use]
    pub fn with_preference_service(mut self, prefs: Arc<dyn PreferenceService>) -> Self {
        self.preference_service = Some(prefs);
        self
    }

    /// Injects a [`NotificationMetrics`] recorder.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn NotificationMetrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }
}

#[async_trait]
impl SmsService for DefaultSmsService {
    async fn send(&self, message: SmsMessage) -> NotificationResult {
        let metrics = self.metrics.as_deref();

        if let Some(prefs) = self.preference_service.as_deref() {
            if !message.to.is_empty() && !prefs.is_opted_in(&message.to, "sms").await {
                if let Some(m) = metrics {
                    m.record_suppressed("sms");
                }
                return NotificationResult::suppressed(message.id, self.provider.name());
            }
        }

        let id = message.id.clone();
        let provider_name = self.provider.name().to_string();
        let result = match self.provider.send(message).await {
            Ok(r) => r,
            Err(e) => NotificationResult::failed(id, provider_name, e),
        };

        record_outcome(metrics, "sms", &result);
        result
    }
}

/// Default [`PushService`] implementation (pyfly `DefaultPushService`).
pub struct DefaultPushService {
    provider: Arc<dyn PushProvider>,
    preference_service: Option<Arc<dyn PreferenceService>>,
    metrics: Option<Arc<dyn NotificationMetrics>>,
}

impl DefaultPushService {
    /// Builds a service that simply delegates to `provider`.
    pub fn new(provider: Arc<dyn PushProvider>) -> Self {
        DefaultPushService {
            provider,
            preference_service: None,
            metrics: None,
        }
    }

    /// Injects a [`PreferenceService`] (enables per-token opt-out pruning).
    #[must_use]
    pub fn with_preference_service(mut self, prefs: Arc<dyn PreferenceService>) -> Self {
        self.preference_service = Some(prefs);
        self
    }

    /// Injects a [`NotificationMetrics`] recorder.
    #[must_use]
    pub fn with_metrics(mut self, metrics: Arc<dyn NotificationMetrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }
}

#[async_trait]
impl PushService for DefaultPushService {
    async fn send(&self, mut message: PushMessage) -> NotificationResult {
        let metrics = self.metrics.as_deref();

        // Per-token opt-out filtering (every device token, not just the first).
        if let Some(prefs) = self.preference_service.as_deref() {
            if !message.device_tokens.is_empty() {
                message.device_tokens =
                    filter_opted_in(prefs, metrics, &message.device_tokens, "push").await;
                if message.device_tokens.is_empty() {
                    return NotificationResult::suppressed(message.id, self.provider.name());
                }
            }
        }

        let id = message.id.clone();
        let provider_name = self.provider.name().to_string();
        let result = match self.provider.send(message).await {
            Ok(r) => r,
            Err(e) => NotificationResult::failed(id, provider_name, e),
        };

        record_outcome(metrics, "push", &result);
        result
    }
}

/// Records the sent/failed metric for a finished send, matching pyfly's tail.
fn record_outcome(
    metrics: Option<&dyn NotificationMetrics>,
    channel: &str,
    result: &NotificationResult,
) {
    if let Some(m) = metrics {
        match result.status {
            DeliveryStatus::Sent => m.record_sent(channel, &result.provider),
            DeliveryStatus::Failed => m.record_failed(channel, &result.provider),
            _ => {}
        }
    }
}
