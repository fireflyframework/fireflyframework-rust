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

//! Shared fixtures for the firefly-webhooks test suites.

#![allow(dead_code)] // each test binary uses a subset of the helpers

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::DateTime;

use firefly_webhooks::{Inbound, Processor, WebhookError};

/// A test processor that counts invocations, captures the last event,
/// and optionally fails — the analog of the Go suites' `procFunc` /
/// `captureProcessor`.
pub struct CaptureProcessor {
    provider: String,
    hits: AtomicU32,
    last: Mutex<Option<Inbound>>,
    fail_with: Option<String>,
}

impl CaptureProcessor {
    /// A processor for `provider` that always succeeds.
    pub fn new(provider: &str) -> Arc<Self> {
        Arc::new(Self {
            provider: provider.to_owned(),
            hits: AtomicU32::new(0),
            last: Mutex::new(None),
            fail_with: None,
        })
    }

    /// A processor for `provider` that always fails with `msg`.
    pub fn failing(provider: &str, msg: &str) -> Arc<Self> {
        Arc::new(Self {
            provider: provider.to_owned(),
            hits: AtomicU32::new(0),
            last: Mutex::new(None),
            fail_with: Some(msg.to_owned()),
        })
    }

    /// How many events this processor has seen.
    pub fn hits(&self) -> u32 {
        self.hits.load(Ordering::SeqCst)
    }

    /// The last event this processor saw.
    pub fn last(&self) -> Option<Inbound> {
        self.last.lock().expect("lock").clone()
    }
}

#[async_trait]
impl Processor for CaptureProcessor {
    fn provider(&self) -> &str {
        &self.provider
    }

    async fn process(&self, ev: &Inbound) -> Result<(), WebhookError> {
        self.hits.fetch_add(1, Ordering::SeqCst);
        *self.last.lock().expect("lock") = Some(ev.clone());
        match &self.fail_with {
            Some(msg) => Err(WebhookError::processor(msg.clone())),
            None => Ok(()),
        }
    }
}

/// Builds a bare [`Inbound`] — the analog of Go's
/// `whi.Inbound{Provider: …, EventType: …}` zero-value literals.
pub fn inbound(provider: &str, event_type: &str) -> Inbound {
    Inbound {
        id: String::new(),
        provider: provider.to_owned(),
        event_type: event_type.to_owned(),
        headers: BTreeMap::new(),
        payload: Vec::new(),
        received_at: DateTime::UNIX_EPOCH,
    }
}

/// Binds an axum router on a random localhost port and returns the
/// base URL — the `httptest.NewServer` analog (used by the SDK tests,
/// which exercise a real client over a real socket).
pub async fn spawn_server(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("http://{addr}")
}
