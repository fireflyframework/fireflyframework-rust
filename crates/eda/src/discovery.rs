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

//! Link-time `@KafkaListener`-style listener discovery — the Rust analog of
//! Spring's listener-endpoint registry subscribing every `@*Listener`.
//!
//! Each `#[event_listener("topic")]` submits a [`ListenerRegistration`] via
//! [`inventory`]; [`subscribe_discovered_listeners`] collects them across the
//! crate graph and `await`s each subscription against a [`Broker`] — so a
//! service never hand-maintains a list of `subscribe_<fn>(&broker).await` calls.

use std::future::Future;
use std::pin::Pin;

use crate::{Broker, EdaResult};

/// The boxed future a generated subscribe-wrapper returns — a subscription that
/// borrows the broker for the duration of the `subscribe` call.
pub type BoxSubscribeFuture<'a> = Pin<Box<dyn Future<Output = EdaResult<()>> + Send + 'a>>;

/// One link-time listener-subscription thunk, `inventory::submit!`-ted once per
/// `#[event_listener("topic")]`. [`subscribe`](Self::subscribe) wraps the
/// generated `subscribe_<fn>(broker).await` helper as a boxed-future fn pointer.
pub struct ListenerRegistration {
    /// Subscribes this listener on the broker (the generated `subscribe_<fn>`).
    pub subscribe: for<'a> fn(&'a dyn Broker) -> BoxSubscribeFuture<'a>,
}

inventory::collect!(ListenerRegistration);

/// Subscribes every discovered (`inventory`-submitted) `#[event_listener]` on
/// `broker` — the turnkey replacement for hand-`await`ing each generated
/// `subscribe_<fn>(&broker)`. Short-circuits on the first subscription error.
/// Returns the number of listeners subscribed.
pub async fn subscribe_discovered_listeners(broker: &dyn Broker) -> EdaResult<usize> {
    let mut count = 0;
    for reg in inventory::iter::<ListenerRegistration> {
        (reg.subscribe)(broker).await?;
        count += 1;
    }
    Ok(count)
}

/// The number of `#[event_listener]` listeners discovered across the crate
/// graph — for the startup report and tests.
#[must_use]
pub fn discovered_listener_count() -> usize {
    inventory::iter::<ListenerRegistration>.into_iter().count()
}
