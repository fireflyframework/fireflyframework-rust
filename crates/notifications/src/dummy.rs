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

//! In-memory dummy providers (pyfly parity).
//!
//! The Rust counterpart of `pyfly.notifications.providers.dummy`. Each provider
//! records every message it is asked to send and always returns a `SENT`
//! [`NotificationResult`] — ideal for tests and local development, and the
//! fallback the `from_config` helpers select when no real provider is
//! configured.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::models::{EmailMessage, NotificationResult, PushMessage, SmsMessage};
use crate::ports::{EmailProvider, PushProvider, SmsProvider};

/// An [`EmailProvider`] that records every message and reports `SENT`.
///
/// Equivalent to pyfly's `DummyEmailProvider`; its `name` is `"dummy"`.
#[derive(Default)]
pub struct DummyEmailProvider {
    sent: Mutex<Vec<EmailMessage>>,
}

impl DummyEmailProvider {
    /// Returns a fresh dummy provider with no recorded messages.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of every message sent so far, in order.
    pub fn sent(&self) -> Vec<EmailMessage> {
        self.sent.lock().expect("dummy lock poisoned").clone()
    }
}

#[async_trait]
impl EmailProvider for DummyEmailProvider {
    fn name(&self) -> &str {
        "dummy"
    }

    async fn send(&self, message: EmailMessage) -> Result<NotificationResult, String> {
        let id = message.id.clone();
        self.sent.lock().expect("dummy lock poisoned").push(message);
        Ok(NotificationResult::sent(id.clone(), "dummy", Some(id)))
    }
}

/// An [`SmsProvider`] that records every message and reports `SENT`.
///
/// Equivalent to pyfly's `DummySmsProvider`; its `name` is `"dummy"`.
#[derive(Default)]
pub struct DummySmsProvider {
    sent: Mutex<Vec<SmsMessage>>,
}

impl DummySmsProvider {
    /// Returns a fresh dummy provider with no recorded messages.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of every message sent so far, in order.
    pub fn sent(&self) -> Vec<SmsMessage> {
        self.sent.lock().expect("dummy lock poisoned").clone()
    }
}

#[async_trait]
impl SmsProvider for DummySmsProvider {
    fn name(&self) -> &str {
        "dummy"
    }

    async fn send(&self, message: SmsMessage) -> Result<NotificationResult, String> {
        let id = message.id.clone();
        self.sent.lock().expect("dummy lock poisoned").push(message);
        Ok(NotificationResult::sent(id, "dummy", None))
    }
}

/// A [`PushProvider`] that records every message and reports `SENT`.
///
/// Equivalent to pyfly's `DummyPushProvider`; its `name` is `"dummy"`.
#[derive(Default)]
pub struct DummyPushProvider {
    sent: Mutex<Vec<PushMessage>>,
}

impl DummyPushProvider {
    /// Returns a fresh dummy provider with no recorded messages.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a snapshot of every message sent so far, in order.
    pub fn sent(&self) -> Vec<PushMessage> {
        self.sent.lock().expect("dummy lock poisoned").clone()
    }
}

#[async_trait]
impl PushProvider for DummyPushProvider {
    fn name(&self) -> &str {
        "dummy"
    }

    async fn send(&self, message: PushMessage) -> Result<NotificationResult, String> {
        let id = message.id.clone();
        self.sent.lock().expect("dummy lock poisoned").push(message);
        Ok(NotificationResult::sent(id, "dummy", None))
    }
}
