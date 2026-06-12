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

//! The [`Aspect`] trait — the Rust analogue of a pyfly `@aspect` class.
//!
//! pyfly declares advice as decorated methods on an `@aspect` class; the
//! decorators (`@before`, `@after_returning`, `@after_throwing`, `@after`,
//! `@around`) attach metadata that the registry discovers reflectively. Rust
//! has no runtime decorator discovery, so an aspect is a type implementing the
//! [`Aspect`] trait and overriding the hooks it cares about. Every hook has a
//! **default no-op implementation**, so an aspect only writes the advice it
//! needs — exactly the effect of decorating just some methods in pyfly.
//!
//! The pointcut a given aspect binds to is supplied explicitly when the aspect
//! is registered ([`crate::AspectRegistry::register`]), replacing pyfly's
//! per-method `@before("pointcut")` annotation.

use async_trait::async_trait;

use crate::join_point::{AdviceFuture, JoinPoint, Proceed};

/// A pyfly-style aspect: a bundle of advice hooks bound to a pointcut.
///
/// Override only the hooks you need; the rest are no-ops. The chain executor
/// invokes them in the pyfly ordering (see [`crate::intercept`]):
///
/// 1. [`before`](Aspect::before)
/// 2. [`around`](Aspect::around) (wraps the call; first-registered outermost)
/// 3. [`after_returning`](Aspect::after_returning) on success **or**
///    [`after_throwing`](Aspect::after_throwing) on error (the error is then
///    re-propagated)
/// 4. [`after`](Aspect::after) — always
///
/// `before`, `after_returning`, `after_throwing`, and `after` observe the
/// [`JoinPoint`] (and may run side effects) but cannot alter the result or
/// swallow the error — matching pyfly, where those advice kinds receive the
/// join point and the wrapper ignores their return value. Only `around` can
/// modify the outcome, by transforming what [`Proceed::proceed`] yields.
///
/// # Examples
///
/// ```
/// use std::sync::{Arc, Mutex};
/// use async_trait::async_trait;
/// use firefly_aop::{Aspect, AdviceFuture, AdviceResult, JoinPoint, Proceed};
///
/// struct LoggingAspect {
///     log: Arc<Mutex<Vec<String>>>,
/// }
///
/// #[async_trait]
/// impl Aspect for LoggingAspect {
///     async fn before(&self, jp: &JoinPoint) {
///         self.log.lock().unwrap().push(format!("before:{}", jp.method_name));
///     }
///
///     fn around<'a>(&'a self, _jp: &'a JoinPoint, proceed: Proceed<'a>) -> AdviceFuture<'a> {
///         Box::pin(async move {
///             let result: AdviceResult = proceed.proceed().await;
///             result
///         })
///     }
/// }
/// ```
#[async_trait]
pub trait Aspect: Send + Sync {
    /// Runs before the join point executes (pyfly `@before`). Default: no-op.
    ///
    /// At this point the join point's `result` and `error` are still empty.
    async fn before(&self, jp: &JoinPoint) {
        let _ = jp;
    }

    /// Runs after the join point returns successfully, with `jp.result`
    /// populated (pyfly `@after_returning`). Default: no-op.
    async fn after_returning(&self, jp: &JoinPoint) {
        let _ = jp;
    }

    /// Runs after the join point raises, with `jp.error` populated, **before**
    /// the error is re-propagated (pyfly `@after_throwing`). Default: no-op.
    async fn after_throwing(&self, jp: &JoinPoint) {
        let _ = jp;
    }

    /// Always runs after the join point, on success or error (pyfly `@after`).
    /// Default: no-op.
    async fn after(&self, jp: &JoinPoint) {
        let _ = jp;
    }

    /// Wraps the join point, deciding when (and whether) to invoke the next
    /// link via [`Proceed::proceed`] and optionally transforming its result
    /// (pyfly `@around`).
    ///
    /// The default implementation is a transparent pass-through: it simply
    /// proceeds and returns the inner result unchanged. Override it to time,
    /// retry, short-circuit, or modify the call.
    ///
    /// `around` is **not** `async fn` because it must thread the explicit
    /// lifetime `'a` shared by `self`, `jp`, and `proceed`; return a boxed
    /// future built with `Box::pin(async move { … })`.
    fn around<'a>(&'a self, jp: &'a JoinPoint, proceed: Proceed<'a>) -> AdviceFuture<'a> {
        let _ = jp;
        Box::pin(async move { proceed.proceed().await })
    }
}

/// A no-op aspect used as a default / placeholder. Every hook is the trait
/// default, so it observes nothing and passes calls through unchanged.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopAspect;

impl Aspect for NoopAspect {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::join_point::{AdviceResult, AnyArc};

    #[tokio::test]
    async fn noop_aspect_hooks_are_no_ops() {
        let jp = JoinPoint::new("service.Svc", "m", Arc::new(()));
        let aspect = NoopAspect;
        // before/after hooks return () without panic.
        aspect.before(&jp).await;
        aspect.after_returning(&jp).await;
        aspect.after_throwing(&jp).await;
        aspect.after(&jp).await;
    }

    #[tokio::test]
    async fn default_around_passes_result_through() {
        let jp = JoinPoint::new("service.Svc", "m", Arc::new(()));
        let aspect = NoopAspect;
        let proceed = Proceed::new(|| Box::pin(async { Ok(Arc::new(7u32) as AnyArc) }));
        let out: AdviceResult = aspect.around(&jp, proceed).await;
        let val = out.unwrap();
        assert_eq!(*val.downcast_ref::<u32>().unwrap(), 7);
    }
}
