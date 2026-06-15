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

//! The process-global aspect registry and crate-graph discovery — the runtime
//! the declarative `#[aspect]` macro expands against.
//!
//! [`AspectRegistry`] is the in-hand, caller-owned registry; this module adds a
//! single, build-once **process-wide** [`AspectRegistry`] plus `inventory`-based
//! discovery, mirroring `firefly_transactional`'s event-listener registry. The
//! `#[aspect]` macro emits one [`AspectRegistration`] thunk per aspect via
//! [`inventory::submit!`]; the first weave (or an explicit
//! [`register_discovered_aspects`]) drains them once, so an aspect declared
//! anywhere in the crate graph is live without manual wiring.
//!
//! # Weaving is still explicit
//!
//! Rust has no transparent runtime proxies, so registration does not, by itself,
//! intercept anything (see [`crate::intercept`]). [`advised`] is the explicit
//! weave point: wrap the real call in a closure and route it through the global
//! registry. It is the honest Rust equivalent of Spring's proxy weaving — the
//! aspect runs because the call site asked it to, not because a proxy replaced
//! the method behind the caller's back.

use std::sync::{Arc, Once, OnceLock, RwLock};

use crate::aspect::Aspect;
use crate::join_point::{AdviceResult, AnyArc};
use crate::registry::{AdviceBinding, AspectRegistry};

/// One registration thunk per `#[aspect]`, collected across the whole crate
/// graph via [`inventory`]. Each thunk calls [`register_aspect`] for its aspect.
pub struct AspectRegistration {
    /// Registers this aspect into the process-global registry.
    pub register: fn(),
}

inventory::collect!(AspectRegistration);

/// The process-wide aspect registry, built once on first access. A plain
/// [`std::sync::RwLock`] (not a tokio lock): every access reads or writes
/// quickly and the lock is always released before any advice runs (see
/// [`matching_bindings`]), so it is never held across an `.await`.
fn registry() -> &'static RwLock<AspectRegistry> {
    static REGISTRY: OnceLock<RwLock<AspectRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(AspectRegistry::new()))
}

/// Registers `aspect` bound to `pointcut` with the given `order` into the
/// process-global registry.
///
/// The `#[aspect]` macro calls this from its `inventory` thunk; tests and
/// programmatic setups call it directly. Equivalent to
/// [`AspectRegistry::register`] on the global registry.
pub fn register_aspect(aspect: Arc<dyn Aspect>, pointcut: &str, order: i32) {
    registry()
        .write()
        .expect("aspect registry poisoned")
        .register(aspect, pointcut, order);
}

/// Drains the discovered (`inventory`-submitted) aspect registrations exactly
/// once. Idempotent and safe to call from any thread; [`matching_bindings`] and
/// [`advised`] call it lazily so declared aspects are live without explicit
/// startup wiring.
pub fn register_discovered_aspects() {
    static DISCOVER: Once = Once::new();
    DISCOVER.call_once(|| {
        for reg in inventory::iter::<AspectRegistration> {
            (reg.register)();
        }
    });
}

/// Returns the bindings in the process-global registry whose pointcut matches
/// `qualified_name`, in global order.
///
/// Discovery is run first (once), then the registry lock is taken, the matching
/// bindings are cloned out, and the lock is **dropped before returning** — so a
/// caller may freely `.await` advice over the result without ever holding the
/// lock across a suspension point.
#[must_use]
pub fn matching_bindings(qualified_name: &str) -> Vec<AdviceBinding> {
    register_discovered_aspects();
    let guard = registry().read().expect("aspect registry poisoned");
    let bindings = guard.get_matching(qualified_name);
    drop(guard);
    bindings
}

/// Weaves `call` through the process-global aspect registry — the explicit weave
/// point for declarative aspects.
///
/// `type_name` and `method` form the qualified name pointcuts match against
/// (`"{type_name}.{method}"`); `args` is the boxed call arguments (commonly a
/// tuple, or `Arc::new(())` when advice ignores them), exposed to advice on the
/// [`JoinPoint`](crate::JoinPoint). `call` produces the original method's
/// [`AdviceResult`].
///
/// When no declared aspect matches, `call` runs with zero advice overhead; when
/// one or more match, their `before` / `around` / `after_returning` /
/// `after_throwing` / `after` advice runs around the call in the exact
/// [`intercept`](crate::intercept) ordering. The registry lock is released
/// before any advice or the call itself runs.
///
/// This is the honest Rust analogue of Spring's proxy weaving: there is no
/// transparent runtime proxy, so the cross-cutting concern runs because the call
/// site routed through `advised`. Box the method's real success value with
/// `Arc::new(...)` and convert its error into `Box<dyn Error + Send + Sync>`.
///
/// # Examples
///
/// ```
/// use std::sync::{Arc, Mutex};
/// use async_trait::async_trait;
/// use firefly_aop::{advised, ok, register_aspect, Aspect, JoinPoint};
///
/// struct Audit(Arc<Mutex<Vec<String>>>);
///
/// #[async_trait]
/// impl Aspect for Audit {
///     async fn before(&self, jp: &JoinPoint) {
///         self.0.lock().unwrap().push(format!("call {}", jp.qualified_name()));
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let log = Arc::new(Mutex::new(Vec::new()));
/// register_aspect(Arc::new(Audit(log.clone())), "svc.Audited.*", 0);
///
/// let out = advised("svc.Audited", "run", Arc::new(()), || async {
///     ok(7u32)
/// })
/// .await
/// .unwrap();
///
/// assert_eq!(*out.downcast_ref::<u32>().unwrap(), 7);
/// assert_eq!(*log.lock().unwrap(), vec!["call svc.Audited.run"]);
/// # }
/// ```
pub async fn advised<F, Fut>(
    type_name: impl Into<String>,
    method: impl Into<String>,
    args: AnyArc,
    call: F,
) -> AdviceResult
where
    F: FnOnce() -> Fut + Send,
    Fut: std::future::Future<Output = AdviceResult> + Send,
{
    let type_name = type_name.into();
    let method = method.into();
    let qualified = format!("{type_name}.{method}");
    let bindings = matching_bindings(&qualified);

    let mut jp = crate::join_point::JoinPoint::new(type_name, method, args);
    crate::intercept::intercept_with_bindings(
        &mut jp,
        &bindings,
        crate::join_point::invocation(call),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use crate::join_point::{AdviceFuture, JoinPoint, Proceed};
    use crate::{intercept, invocation, ok};

    type Log = Arc<Mutex<Vec<String>>>;

    // A combined before+around aspect, so a single registration proves both that
    // `matching_bindings` finds it AND that the chain executor runs its advice.
    struct Probe(Log);

    #[async_trait]
    impl Aspect for Probe {
        async fn before(&self, jp: &JoinPoint) {
            self.0
                .lock()
                .unwrap()
                .push(format!("before:{}", jp.method_name));
        }
        fn around<'a>(&'a self, _jp: &'a JoinPoint, proceed: Proceed<'a>) -> AdviceFuture<'a> {
            let log = self.0.clone();
            Box::pin(async move {
                log.lock().unwrap().push("around:before".to_string());
                let r = proceed.proceed().await;
                log.lock().unwrap().push("around:after".to_string());
                r
            })
        }
    }

    // Each test registers on a distinct pointcut/qualified name so the shared
    // process-global registry never crosses test boundaries.

    #[test]
    fn register_aspect_is_found_by_matching_bindings() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        register_aspect(Arc::new(Probe(log)), "global.FindMe.*", 0);

        let bindings = matching_bindings("global.FindMe.run");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].pattern(), "global.FindMe.*");

        // A non-matching name returns nothing.
        assert!(matching_bindings("global.Other.run").is_empty());
    }

    #[tokio::test]
    async fn matching_bindings_feed_intercept() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        register_aspect(Arc::new(Probe(log.clone())), "global.Woven.*", 0);

        let bindings = matching_bindings("global.Woven.greet");
        let mut jp = JoinPoint::new("global.Woven", "greet", Arc::new(()));
        let out = intercept::intercept_with_bindings(
            &mut jp,
            &bindings,
            invocation(|| async { ok("hi".to_string()) }),
        )
        .await
        .unwrap();

        assert_eq!(out.downcast_ref::<String>().unwrap(), "hi");
        assert_eq!(
            *log.lock().unwrap(),
            vec!["before:greet", "around:before", "around:after"]
        );
    }

    #[tokio::test]
    async fn advised_helper_runs_before_and_around() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        register_aspect(Arc::new(Probe(log.clone())), "global.Helper.*", 0);

        let out = advised("global.Helper", "work", Arc::new(()), || async {
            ok(42u32)
        })
        .await
        .unwrap();

        assert_eq!(*out.downcast_ref::<u32>().unwrap(), 42);
        assert_eq!(
            *log.lock().unwrap(),
            vec!["before:work", "around:before", "around:after"]
        );
    }

    #[tokio::test]
    async fn advised_with_no_match_runs_call_directly() {
        // No aspect registered on this pointcut: the call runs with no advice.
        let out = advised("global.Unwoven", "plain", Arc::new(()), || async {
            ok("raw".to_string())
        })
        .await
        .unwrap();
        assert_eq!(out.downcast_ref::<String>().unwrap(), "raw");
    }
}
