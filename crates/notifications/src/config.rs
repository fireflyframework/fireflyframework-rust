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

//! Config-driven provider/engine/store selection helpers (pyfly parity).
//!
//! The Rust counterpart of the *selection* logic in pyfly's
//! `NotificationsAutoConfiguration`. The pyfly `@bean` methods read a config key
//! (e.g. `pyfly.notifications.email.provider`) and construct the matching
//! adapter. Here we keep that string → selection mapping, but leave the actual
//! construction of vendor adapters to the vendor crates (each provider lives in
//! its own crate and knows its own constructor arguments). This crate only owns
//! the in-process [`DummyEmailProvider`](crate::DummyEmailProvider) fallback.
//!
//! Selection helpers parse a raw config string (case-insensitively, trimmed)
//! into a typed selection enum. Wiring code matches on the enum and calls the
//! vendor constructor; an unrecognized value falls back to the dummy provider,
//! matching pyfly's `else: DummyXProvider()`.

/// The selected e-mail provider (pyfly `pyfly.notifications.email.provider`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmailProviderSelection {
    /// `"sendgrid"` — construct a `firefly-notifications-sendgrid` adapter.
    SendGrid,
    /// `"resend"` — construct a `firefly-notifications-resend` adapter.
    Resend,
    /// `"smtp"` — construct a `firefly-notifications-smtp` adapter.
    Smtp,
    /// Anything else (incl. `"dummy"`) — use the in-process dummy provider.
    Dummy,
}

impl EmailProviderSelection {
    /// Parses a raw config value into a selection, defaulting to
    /// [`EmailProviderSelection::Dummy`] for unrecognized / empty values.
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "sendgrid" => EmailProviderSelection::SendGrid,
            "resend" => EmailProviderSelection::Resend,
            "smtp" => EmailProviderSelection::Smtp,
            _ => EmailProviderSelection::Dummy,
        }
    }
}

/// The selected SMS provider (pyfly `pyfly.notifications.sms.provider`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmsProviderSelection {
    /// `"twilio"` — construct a `firefly-notifications-twilio` adapter.
    Twilio,
    /// Anything else (incl. `"dummy"`) — use the in-process dummy provider.
    Dummy,
}

impl SmsProviderSelection {
    /// Parses a raw config value into a selection, defaulting to
    /// [`SmsProviderSelection::Dummy`] for unrecognized / empty values.
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "twilio" => SmsProviderSelection::Twilio,
            _ => SmsProviderSelection::Dummy,
        }
    }
}

/// The selected push provider (pyfly `pyfly.notifications.push.provider`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushProviderSelection {
    /// `"firebase"` — construct a `firefly-notifications-firebase` adapter.
    Firebase,
    /// Anything else (incl. `"dummy"`) — use the in-process dummy provider.
    Dummy,
}

impl PushProviderSelection {
    /// Parses a raw config value into a selection, defaulting to
    /// [`PushProviderSelection::Dummy`] for unrecognized / empty values.
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "firebase" => PushProviderSelection::Firebase,
            _ => PushProviderSelection::Dummy,
        }
    }
}

/// The selected template engine (pyfly `pyfly.notifications.template.engine`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateEngineSelection {
    /// `"jinja2"` / `"minijinja"` — use the local
    /// [`MiniJinjaTemplateEngine`](crate::MiniJinjaTemplateEngine).
    MiniJinja,
    /// `"none"` / absent — no local engine; provider-native templates apply.
    None,
}

impl TemplateEngineSelection {
    /// Parses a raw config value into a selection. Both `"jinja2"` (the pyfly
    /// key) and `"minijinja"` map to the local engine; anything else is
    /// [`TemplateEngineSelection::None`].
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "jinja2" | "minijinja" => TemplateEngineSelection::MiniJinja,
            _ => TemplateEngineSelection::None,
        }
    }
}

/// The selected preference store (pyfly `pyfly.notifications.preference.store`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferenceStoreSelection {
    /// `"memory"` — use an
    /// [`InMemoryPreferenceService`](crate::InMemoryPreferenceService).
    Memory,
    /// `"none"` / absent — no preference store; opt-out suppression disabled.
    None,
}

impl PreferenceStoreSelection {
    /// Parses a raw config value into a selection, defaulting to
    /// [`PreferenceStoreSelection::None`] for unrecognized / empty values.
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "memory" => PreferenceStoreSelection::Memory,
            _ => PreferenceStoreSelection::None,
        }
    }
}
