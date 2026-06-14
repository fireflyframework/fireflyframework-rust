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

//! [`CorrelationMiddleware`] — propagates a correlation id across every CQRS
//! dispatch.
//!
//! The HTTP layer (`firefly-web`'s correlation layer) already sets a
//! correlation id task-local per request. But work that leaves the request
//! task — a command dispatched to the bus, the saga it starts, a `tokio::spawn`
//! it triggers — must keep the same id so one logical operation shares one
//! trace. This middleware enforces the single rule the audit called for:
//! **every dispatch boundary ensures-or-generates a correlation id and restores
//! the prior scope on the way out.** Add it to a [`Bus`](crate::Bus) via
//! [`Bus::use_middleware`](crate::Bus::use_middleware) (a starter does this by
//! default).

use std::sync::Arc;

use crate::bus::{DynHandler, Envelope, HandlerFuture, Middleware};

/// Ensures each command/query dispatch runs under a correlation id, reusing the
/// ambient one when present (set upstream by the HTTP layer) and generating a
/// fresh one otherwise. The id is restored to its prior value when the dispatch
/// completes (task-local scoping), so sibling operations never leak ids.
#[derive(Debug, Clone, Default)]
pub struct CorrelationMiddleware;

impl CorrelationMiddleware {
    /// Builds the middleware.
    pub fn new() -> Self {
        CorrelationMiddleware
    }
}

impl Middleware for CorrelationMiddleware {
    fn wrap(&self, next: DynHandler) -> DynHandler {
        Arc::new(move |env: Arc<Envelope>| -> HandlerFuture {
            let next = Arc::clone(&next);
            Box::pin(async move {
                if firefly_kernel::correlation_id().is_some() {
                    // Already in scope (e.g. the HTTP correlation layer) — reuse.
                    next(env).await
                } else {
                    // No ambient id — generate one for the span of this dispatch.
                    let id = firefly_kernel::new_correlation_id();
                    firefly_kernel::with_correlation_id(id, next(env)).await
                }
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Bus;
    use serde::Serialize;

    #[derive(Clone, Serialize)]
    struct Ping;
    impl crate::Message for Ping {}

    #[tokio::test]
    async fn dispatch_runs_under_a_correlation_id() {
        let bus = Bus::new();
        bus.use_middleware(CorrelationMiddleware::new());
        // The handler observes a correlation id even though none was set by the
        // caller — the middleware generated one for the dispatch.
        bus.register::<Ping, Option<String>, _, _>(|_p: Ping| async {
            Ok(firefly_kernel::correlation_id())
        });

        assert!(firefly_kernel::correlation_id().is_none());
        let seen: Option<String> = bus.send(Ping).await.unwrap();
        assert!(seen.is_some(), "handler saw a generated correlation id");
        // The scope was restored afterwards.
        assert!(firefly_kernel::correlation_id().is_none());
    }

    #[tokio::test]
    async fn existing_correlation_id_is_reused() {
        let bus = Bus::new();
        bus.use_middleware(CorrelationMiddleware::new());
        bus.register::<Ping, Option<String>, _, _>(|_p: Ping| async {
            Ok(firefly_kernel::correlation_id())
        });

        let seen: Option<String> = firefly_kernel::with_correlation_id("caller-123", async {
            bus.send(Ping).await.unwrap()
        })
        .await;
        assert_eq!(seen.as_deref(), Some("caller-123"), "ambient id reused");
    }
}
