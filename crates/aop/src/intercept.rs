//! The advice-chain executor — [`intercept`].
//!
//! This is the Rust port of pyfly's `weaver._build_async_wrapper`: it runs the
//! advice for every binding that matches a qualified name, in the exact pyfly
//! ordering, around a captured [`Invocation`] of the original call.
//!
//! ## Weaving is explicit
//!
//! pyfly's `weave_bean` walks a live bean, `setattr`-monkey-patches each public
//! method whose qualified name matches some binding, and skips `@property`
//! descriptors via `getattr_static`. **Rust has no analogue** — there is no
//! runtime mutation of a struct's methods, no descriptor protocol, and no bean
//! container to post-process. Weaving is therefore *explicit*: the call site
//! wraps the original call in an [`Invocation`] and routes it through
//! [`intercept`] at construction time. Skipping non-matching methods is
//! automatic — if no binding matches, [`intercept`] runs the invocation with
//! zero advice overhead (the same observable result as pyfly leaving an
//! unmatched method untouched).
//!
//! ## Ordering contract (identical to pyfly)
//!
//! For each matching binding, in registry order (lowest `order` first):
//!
//! 1. **`before`** advice runs for every matching binding, in order.
//! 2. **`around`** advice wraps the call: the first-registered (lowest-order)
//!    around is outermost; its [`Proceed`](crate::Proceed) invokes the next
//!    inner around, and the innermost around's `proceed` runs the original
//!    [`Invocation`].
//! 3. On **success**: the join point's `result` is set, then `after_returning`
//!    runs for every matching binding, in order.
//! 4. On **error**: the join point's `error` is set, then `after_throwing` runs
//!    for every matching binding, in order, and the error is **re-propagated**.
//! 5. **`after`** always runs for every matching binding, in order (success or
//!    error).

use std::sync::Arc;

use crate::join_point::{AdviceFuture, AdviceResult, AnyArc, Invocation, JoinPoint, Proceed};
use crate::registry::{AdviceBinding, AspectRegistry};

/// Run `invocation` through the advice chain for `qualified_name`.
///
/// `type_name` and `method_name` form the qualified name that pointcuts match
/// (`"{type_name}.{method_name}"`) and are exposed to advice on the
/// [`JoinPoint`]. `args` is the boxed call arguments (commonly a tuple), opaque
/// to the framework. `invocation` is the captured original call.
///
/// If no binding matches, the invocation runs directly with no overhead. On
/// error the join point's `error` is populated, `after_throwing` + `after` run,
/// and the original error is returned unchanged.
///
/// # Examples
///
/// ```
/// use std::sync::{Arc, Mutex};
/// use async_trait::async_trait;
/// use firefly_aop::{
///     intercept, invocation, AnyArc, AspectRegistry, Aspect, JoinPoint,
/// };
///
/// struct Logging(Arc<Mutex<Vec<String>>>);
///
/// #[async_trait]
/// impl Aspect for Logging {
///     async fn before(&self, jp: &JoinPoint) {
///         self.0.lock().unwrap().push(format!("before:{}", jp.method_name));
///     }
/// }
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let log = Arc::new(Mutex::new(Vec::new()));
/// let mut registry = AspectRegistry::new();
/// registry.register(Arc::new(Logging(log.clone())), "service.MyService.*", 0);
///
/// let out = intercept(
///     &registry,
///     "service.MyService",
///     "greet",
///     Arc::new(("alice",)),
///     invocation(|| async { Ok(Arc::new("hello alice".to_string()) as AnyArc) }),
/// )
/// .await
/// .unwrap();
///
/// assert_eq!(out.downcast_ref::<String>().unwrap(), "hello alice");
/// assert_eq!(*log.lock().unwrap(), vec!["before:greet"]);
/// # }
/// ```
pub async fn intercept(
    registry: &AspectRegistry,
    type_name: impl Into<String>,
    method_name: impl Into<String>,
    args: AnyArc,
    invocation: Invocation<'_>,
) -> AdviceResult {
    let mut jp = JoinPoint::new(type_name, method_name, args);
    let qualified = jp.qualified_name();
    let bindings = registry.get_matching(&qualified);

    intercept_with_bindings(&mut jp, &bindings, invocation).await
}

/// Run the chain over a pre-resolved set of bindings against a caller-owned
/// [`JoinPoint`].
///
/// This is the lower-level entry point [`intercept`] is built on. Use it when
/// you already hold the matching bindings (e.g. cached at construction time) or
/// want to inspect the join point's `result`/`error` after the chain resolves.
pub async fn intercept_with_bindings(
    jp: &mut JoinPoint,
    bindings: &[AdviceBinding],
    invocation: Invocation<'_>,
) -> AdviceResult {
    // 1. `before` advice (in registry order).
    for binding in bindings {
        binding.aspect.before(jp).await;
    }

    // 2. Execute the call, wrapped by the `around` chain. The around links need
    //    `&jp` available during the call while the executor still needs to write
    //    `result`/`error` afterward, so the chain borrows an immutable snapshot
    //    of the join point. `before` already ran and `around` advice only reads
    //    the join point during the call, so a snapshot is sufficient and matches
    //    pyfly (the original `jp.proceed(*jp.args, ...)` reads, never writes).
    //
    //    Every matching binding participates in the around chain: an aspect that
    //    does not override `around` contributes the default transparent
    //    pass-through, which is observably identical to pyfly grouping only
    //    `@around`-decorated methods (a non-around aspect contributes nothing).
    //    The first-registered binding ends up outermost (see `run_around_chain`).
    let snapshot = jp.clone();
    // Re-box the invocation into a fresh, locally-scoped `Invocation`. The
    // incoming `invocation` may carry a longer lifetime than the local
    // `snapshot`/`bindings` borrows; `Invocation` is invariant over its
    // lifetime parameter, so it cannot shrink by coercion alone (the same
    // reason `firefly-resilience`'s `Chain` re-boxes its `Operation`).
    let local_invocation =
        crate::join_point::invocation(move || async move { invocation.call().await });
    let chain_result = run_around_chain(&snapshot, bindings, local_invocation).await;

    match chain_result {
        Ok(value) => {
            // 3. Success: set result, run `after_returning`.
            jp.result = Some(value.clone());
            for binding in bindings {
                binding.aspect.after_returning(jp).await;
            }
            // 5. `after` always runs.
            run_after(jp, bindings).await;
            Ok(value)
        }
        Err(err) => {
            // 4. Error: set error message, run `after_throwing`, re-propagate.
            jp.error = Some(err.to_string());
            for binding in bindings {
                binding.aspect.after_throwing(jp).await;
            }
            // 5. `after` always runs (the `finally` in pyfly).
            run_after(jp, bindings).await;
            Err(err)
        }
    }
}

/// Run the `after` advice for every binding, in registry order. Factored out so
/// it runs on both the success and error paths (pyfly's `finally`).
async fn run_after(jp: &JoinPoint, bindings: &[AdviceBinding]) {
    for binding in bindings {
        binding.aspect.after(jp).await;
    }
}

/// Build and run the `around` chain so `bindings[0]` is outermost and the
/// innermost link runs the original `invocation`.
///
/// This mirrors pyfly's reverse-fold over `around_bindings`: the proceed chain
/// is constructed innermost-first, so the first-registered around ends up
/// wrapping all the others.
fn run_around_chain<'c>(
    jp: &'c JoinPoint,
    bindings: &'c [AdviceBinding],
    invocation: Invocation<'c>,
) -> AdviceFuture<'c> {
    match bindings.split_first() {
        // Innermost: no more around advice, run the captured call.
        None => invocation.call(),
        Some((outer, rest)) => {
            // The continuation for `outer` is the chain over the remaining
            // bindings; building it lazily keeps each `Proceed` single-shot.
            Box::pin(async move {
                let proceed = Proceed::new(move || run_around_chain(jp, rest, invocation));
                outer.aspect.around(jp, proceed).await
            })
        }
    }
}

/// Convenience: box a plain value as an [`AnyArc`] for use in an
/// [`Invocation`]'s success result.
pub fn ok<T: Send + Sync + 'static>(value: T) -> AdviceResult {
    Ok(Arc::new(value) as AnyArc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use async_trait::async_trait;

    use crate::aspect::Aspect;
    use crate::join_point::invocation;

    // ---- Helper aspects ----------------------------------------------------

    type Log = Arc<Mutex<Vec<String>>>;

    struct BeforeLogger(Log);
    #[async_trait]
    impl Aspect for BeforeLogger {
        async fn before(&self, jp: &JoinPoint) {
            self.0
                .lock()
                .unwrap()
                .push(format!("before:{}", jp.method_name));
        }
    }

    struct ReturnLogger(Log);
    #[async_trait]
    impl Aspect for ReturnLogger {
        async fn after_returning(&self, jp: &JoinPoint) {
            let val = jp
                .result
                .as_ref()
                .and_then(|r| r.downcast_ref::<String>())
                .cloned()
                .unwrap_or_default();
            self.0
                .lock()
                .unwrap()
                .push(format!("after_returning:{val}"));
        }
    }

    struct ThrowLogger(Log);
    #[async_trait]
    impl Aspect for ThrowLogger {
        async fn after_throwing(&self, jp: &JoinPoint) {
            self.0.lock().unwrap().push(format!(
                "after_throwing:{}",
                jp.error.clone().unwrap_or_default()
            ));
        }
    }

    struct AfterLogger(Log);
    #[async_trait]
    impl Aspect for AfterLogger {
        async fn after(&self, jp: &JoinPoint) {
            self.0
                .lock()
                .unwrap()
                .push(format!("after:{}", jp.method_name));
        }
    }

    fn registry_with(bindings: Vec<(Arc<dyn Aspect>, &str, i32)>) -> AspectRegistry {
        let mut r = AspectRegistry::new();
        for (a, pc, order) in bindings {
            r.register(a, pc, order);
        }
        r
    }

    async fn greet() -> AdviceResult {
        ok("hello x".to_string())
    }

    async fn explode() -> AdviceResult {
        Err("boom".into())
    }

    // ---- Port of pyfly test_weaver.py::TestBeforeAdvice -------------------

    #[tokio::test]
    async fn before_runs_before_method() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let reg = registry_with(vec![(
            Arc::new(BeforeLogger(log.clone())),
            "service.MyService.*",
            0,
        )]);

        let out = intercept(
            &reg,
            "service.MyService",
            "greet",
            Arc::new(("alice",)),
            invocation(greet),
        )
        .await
        .unwrap();

        assert_eq!(out.downcast_ref::<String>().unwrap(), "hello x");
        assert_eq!(*log.lock().unwrap(), vec!["before:greet"]);
    }

    // ---- Port of TestAfterReturningAdvice ---------------------------------

    #[tokio::test]
    async fn after_returning_sees_return_value() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let reg = registry_with(vec![(
            Arc::new(ReturnLogger(log.clone())),
            "service.MyService.*",
            0,
        )]);

        intercept(
            &reg,
            "service.MyService",
            "greet",
            Arc::new(()),
            invocation(greet),
        )
        .await
        .unwrap();

        assert_eq!(*log.lock().unwrap(), vec!["after_returning:hello x"]);
    }

    // ---- Port of TestAfterThrowingAdvice ----------------------------------

    #[tokio::test]
    async fn after_throwing_sees_exception_and_reraises() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let reg = registry_with(vec![(
            Arc::new(ThrowLogger(log.clone())),
            "service.MyService.*",
            0,
        )]);

        let err = intercept(
            &reg,
            "service.MyService",
            "explode",
            Arc::new(()),
            invocation(explode),
        )
        .await
        .unwrap_err();

        assert_eq!(err.to_string(), "boom");
        assert_eq!(*log.lock().unwrap(), vec!["after_throwing:boom"]);
    }

    #[tokio::test]
    async fn after_throwing_not_called_on_success() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let reg = registry_with(vec![(
            Arc::new(ThrowLogger(log.clone())),
            "service.MyService.*",
            0,
        )]);

        intercept(
            &reg,
            "service.MyService",
            "greet",
            Arc::new(()),
            invocation(greet),
        )
        .await
        .unwrap();

        assert!(log.lock().unwrap().is_empty());
    }

    // ---- Port of TestAfterAdvice ------------------------------------------

    #[tokio::test]
    async fn after_runs_on_success() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let reg = registry_with(vec![(
            Arc::new(AfterLogger(log.clone())),
            "service.MyService.*",
            0,
        )]);

        intercept(
            &reg,
            "service.MyService",
            "greet",
            Arc::new(()),
            invocation(greet),
        )
        .await
        .unwrap();

        assert!(log.lock().unwrap().contains(&"after:greet".to_string()));
    }

    #[tokio::test]
    async fn after_runs_on_exception() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let reg = registry_with(vec![(
            Arc::new(AfterLogger(log.clone())),
            "service.MyService.*",
            0,
        )]);

        let _ = intercept(
            &reg,
            "service.MyService",
            "explode",
            Arc::new(()),
            invocation(explode),
        )
        .await
        .unwrap_err();

        assert!(log.lock().unwrap().contains(&"after:explode".to_string()));
    }

    // ---- Port of TestAroundAdvice -----------------------------------------

    struct AroundWrapper(Log);
    #[async_trait]
    impl Aspect for AroundWrapper {
        fn around<'a>(&'a self, _jp: &'a JoinPoint, proceed: Proceed<'a>) -> AdviceFuture<'a> {
            let log = self.0.clone();
            Box::pin(async move {
                log.lock().unwrap().push("around:before".to_string());
                let result = proceed.proceed().await;
                log.lock().unwrap().push("around:after".to_string());
                result
            })
        }
    }

    #[tokio::test]
    async fn around_wraps_execution() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let reg = registry_with(vec![(
            Arc::new(AroundWrapper(log.clone())),
            "service.MyService.*",
            0,
        )]);

        let out = intercept(
            &reg,
            "service.MyService",
            "greet",
            Arc::new(()),
            invocation(greet),
        )
        .await
        .unwrap();

        assert_eq!(out.downcast_ref::<String>().unwrap(), "hello x");
        assert_eq!(*log.lock().unwrap(), vec!["around:before", "around:after"]);
    }

    struct AroundModify;
    #[async_trait]
    impl Aspect for AroundModify {
        fn around<'a>(&'a self, _jp: &'a JoinPoint, proceed: Proceed<'a>) -> AdviceFuture<'a> {
            Box::pin(async move {
                let result = proceed.proceed().await?;
                let s = result.downcast_ref::<String>().unwrap().to_uppercase();
                Ok(Arc::new(s) as AnyArc)
            })
        }
    }

    #[tokio::test]
    async fn around_can_modify_result() {
        let reg = registry_with(vec![(Arc::new(AroundModify), "service.MyService.*", 0)]);

        let out = intercept(
            &reg,
            "service.MyService",
            "greet",
            Arc::new(()),
            invocation(greet),
        )
        .await
        .unwrap();

        assert_eq!(out.downcast_ref::<String>().unwrap(), "HELLO X");
    }

    // ---- Multi-around chaining: first-registered outermost ----------------

    struct NamedAround {
        name: &'static str,
        log: Log,
    }
    #[async_trait]
    impl Aspect for NamedAround {
        fn around<'a>(&'a self, _jp: &'a JoinPoint, proceed: Proceed<'a>) -> AdviceFuture<'a> {
            let name = self.name;
            let log = self.log.clone();
            Box::pin(async move {
                log.lock().unwrap().push(format!("{name}:before"));
                let result = proceed.proceed().await;
                log.lock().unwrap().push(format!("{name}:after"));
                result
            })
        }
    }

    #[tokio::test]
    async fn multi_around_first_registered_is_outermost() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let reg = registry_with(vec![
            (
                Arc::new(NamedAround {
                    name: "outer",
                    log: log.clone(),
                }),
                "service.MyService.*",
                1,
            ),
            (
                Arc::new(NamedAround {
                    name: "inner",
                    log: log.clone(),
                }),
                "service.MyService.*",
                2,
            ),
        ]);

        intercept(
            &reg,
            "service.MyService",
            "greet",
            Arc::new(()),
            invocation(greet),
        )
        .await
        .unwrap();

        assert_eq!(
            *log.lock().unwrap(),
            vec!["outer:before", "inner:before", "inner:after", "outer:after"]
        );
    }

    // ---- Port of TestMultipleAspectsOrdering ------------------------------

    #[tokio::test]
    async fn before_advice_ordered_by_aspect_order() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));

        struct Named {
            name: &'static str,
            log: Log,
        }
        #[async_trait]
        impl Aspect for Named {
            async fn before(&self, _jp: &JoinPoint) {
                self.log.lock().unwrap().push(self.name.to_string());
            }
        }

        // Register in the WRONG order to prove sorting works (pyfly test).
        let reg = registry_with(vec![
            (
                Arc::new(Named {
                    name: "second",
                    log: log.clone(),
                }),
                "service.MyService.*",
                10,
            ),
            (
                Arc::new(Named {
                    name: "first",
                    log: log.clone(),
                }),
                "service.MyService.*",
                1,
            ),
        ]);

        intercept(
            &reg,
            "service.MyService",
            "greet",
            Arc::new(()),
            invocation(greet),
        )
        .await
        .unwrap();

        assert_eq!(*log.lock().unwrap(), vec!["first", "second"]);
    }

    #[tokio::test]
    async fn combined_before_and_after_returning() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));

        struct Combined(Log);
        #[async_trait]
        impl Aspect for Combined {
            async fn before(&self, _jp: &JoinPoint) {
                self.0.lock().unwrap().push("before".to_string());
            }
            async fn after_returning(&self, jp: &JoinPoint) {
                let val = jp
                    .result
                    .as_ref()
                    .and_then(|r| r.downcast_ref::<String>())
                    .cloned()
                    .unwrap_or_default();
                self.0
                    .lock()
                    .unwrap()
                    .push(format!("after_returning:{val}"));
            }
        }

        let reg = registry_with(vec![(
            Arc::new(Combined(log.clone())),
            "service.MyService.*",
            0,
        )]);

        intercept(
            &reg,
            "service.MyService",
            "greet",
            Arc::new(()),
            invocation(greet),
        )
        .await
        .unwrap();

        assert_eq!(
            *log.lock().unwrap(),
            vec!["before", "after_returning:hello x"]
        );
    }

    // ---- Port of TestNonMatchingMethods -----------------------------------

    #[tokio::test]
    async fn non_matching_methods_untouched() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let reg = registry_with(vec![(
            Arc::new(BeforeLogger(log.clone())),
            "service.MyService.greet",
            0,
        )]);

        // greet IS intercepted.
        intercept(
            &reg,
            "service.MyService",
            "greet",
            Arc::new(()),
            invocation(greet),
        )
        .await
        .unwrap();
        assert_eq!(*log.lock().unwrap(), vec!["before:greet"]);

        // explode is NOT intercepted (pointcut does not match).
        log.lock().unwrap().clear();
        let err = intercept(
            &reg,
            "service.MyService",
            "explode",
            Arc::new(()),
            invocation(explode),
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "boom");
        assert!(log.lock().unwrap().is_empty());
    }

    // ---- Full ordering across all five advice kinds -----------------------

    struct AllFive(Log);
    #[async_trait]
    impl Aspect for AllFive {
        async fn before(&self, _jp: &JoinPoint) {
            self.0.lock().unwrap().push("before".to_string());
        }
        async fn after_returning(&self, _jp: &JoinPoint) {
            self.0.lock().unwrap().push("after_returning".to_string());
        }
        async fn after_throwing(&self, _jp: &JoinPoint) {
            self.0.lock().unwrap().push("after_throwing".to_string());
        }
        async fn after(&self, _jp: &JoinPoint) {
            self.0.lock().unwrap().push("after".to_string());
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

    #[tokio::test]
    async fn full_ordering_on_success() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let reg = registry_with(vec![(Arc::new(AllFive(log.clone())), "s.S.*", 0)]);
        intercept(&reg, "s.S", "m", Arc::new(()), invocation(greet))
            .await
            .unwrap();
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "before",
                "around:before",
                "around:after",
                "after_returning",
                "after"
            ]
        );
    }

    #[tokio::test]
    async fn full_ordering_on_error() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let reg = registry_with(vec![(Arc::new(AllFive(log.clone())), "s.S.*", 0)]);
        let _ = intercept(&reg, "s.S", "m", Arc::new(()), invocation(explode))
            .await
            .unwrap_err();
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                "before",
                "around:before",
                "around:after",
                "after_throwing",
                "after"
            ]
        );
    }

    #[tokio::test]
    async fn no_bindings_runs_invocation_directly() {
        let reg = AspectRegistry::new();
        let out = intercept(&reg, "s.S", "m", Arc::new(()), invocation(greet))
            .await
            .unwrap();
        assert_eq!(out.downcast_ref::<String>().unwrap(), "hello x");
    }

    #[tokio::test]
    async fn around_can_short_circuit_without_proceeding() {
        struct ShortCircuit;
        #[async_trait]
        impl Aspect for ShortCircuit {
            fn around<'a>(&'a self, _jp: &'a JoinPoint, _proceed: Proceed<'a>) -> AdviceFuture<'a> {
                // Never calls proceed — returns a canned value instead.
                Box::pin(async move { ok("short".to_string()) })
            }
        }
        let reg = registry_with(vec![(Arc::new(ShortCircuit), "s.S.*", 0)]);
        let out = intercept(&reg, "s.S", "m", Arc::new(()), invocation(greet))
            .await
            .unwrap();
        assert_eq!(out.downcast_ref::<String>().unwrap(), "short");
    }
}
