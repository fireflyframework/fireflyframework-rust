//! AOP core types — [`JoinPoint`], [`Invocation`], [`AdviceKind`], and the
//! [`Proceed`] continuation used by `around` advice.
//!
//! pyfly's `JoinPoint` is a dataclass carrying `target`, `method_name`, `args`,
//! `kwargs`, `return_value`, `exception`, and a `proceed` callable. Rust has no
//! reflective argument capture, so the Rust port models the *invocation
//! context* explicitly:
//!
//! * `type_name` / `method_name` form the qualified-name parts the pointcut
//!   matches against (`stereotype.ClassName.method`).
//! * `args` is an opaque `Arc<dyn Any + Send + Sync>` — advice that needs to
//!   inspect arguments downcasts it; advice that only audits the join point
//!   ignores it. A single boxed value (commonly a tuple) replaces Python's
//!   positional `args` + keyword `kwargs`.
//! * `result` / `error` are populated by the chain executor before the
//!   `after_returning` / `after_throwing` hooks run, mirroring pyfly's
//!   `return_value` / `exception`.

use std::any::Any;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// A boxed, thread-safe value of any type — the Rust analogue of pyfly's
/// dynamically-typed `args` / `return_value`. Advice downcasts it via
/// [`Any::downcast_ref`] when it needs the concrete type.
pub type AnyArc = Arc<dyn Any + Send + Sync>;

/// The boxed error carried across an intercepted call. Errors are type-erased
/// to `Box<dyn Error + Send + Sync>` so `firefly-aop` stays agnostic of the
/// advised method's error type — the equivalent of pyfly catching `Exception`.
pub type AdviceError = Box<dyn Error + Send + Sync>;

/// The result the intercepted method (and the whole advice chain) yields: an
/// opaque success value or a type-erased error.
pub type AdviceResult = Result<AnyArc, AdviceError>;

/// The future produced by [`Proceed`] and by `around` advice.
pub type AdviceFuture<'a> = Pin<Box<dyn Future<Output = AdviceResult> + Send + 'a>>;

/// The five kinds of advice, mirroring pyfly's advice decorators. The string
/// representations are wire-identical to pyfly (`"before"`, `"after_returning"`,
/// …) so cross-port configuration and diagnostics stay consistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdviceKind {
    /// Runs before the join point (pyfly `@before`).
    Before,
    /// Runs after the join point returns successfully (pyfly `@after_returning`).
    AfterReturning,
    /// Runs after the join point raises, before the error is re-propagated
    /// (pyfly `@after_throwing`).
    AfterThrowing,
    /// Always runs after the join point, success or error (pyfly `@after`).
    After,
    /// Wraps the join point and controls invocation via [`Proceed`]
    /// (pyfly `@around`).
    Around,
}

impl AdviceKind {
    /// The pyfly-identical string name of this advice kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            AdviceKind::Before => "before",
            AdviceKind::AfterReturning => "after_returning",
            AdviceKind::AfterThrowing => "after_throwing",
            AdviceKind::After => "after",
            AdviceKind::Around => "around",
        }
    }
}

impl fmt::Display for AdviceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `Proceed` is the continuation passed to `around` advice: calling it invokes
/// the next inner `around` link, or — at the innermost link — the original
/// method. It is the Rust analogue of pyfly's `jp.proceed(*jp.args,
/// **jp.kwargs)`.
///
/// Like pyfly's chain, `proceed` is single-shot: each link may call it at most
/// once. Calling it consumes the continuation (it is a `FnOnce`), so attempting
/// to invoke it twice is a compile error rather than undefined behaviour.
pub struct Proceed<'a> {
    inner: Box<dyn FnOnce() -> AdviceFuture<'a> + Send + 'a>,
}

impl<'a> Proceed<'a> {
    /// Wrap a continuation closure into a [`Proceed`].
    pub(crate) fn new<F>(f: F) -> Self
    where
        F: FnOnce() -> AdviceFuture<'a> + Send + 'a,
    {
        Self { inner: Box::new(f) }
    }

    /// Invoke the next link of the chain (or the original method), awaiting its
    /// result.
    #[must_use = "the proceed result must be propagated so later advice sees the outcome"]
    pub fn proceed(self) -> AdviceFuture<'a> {
        (self.inner)()
    }
}

impl fmt::Debug for Proceed<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Proceed").finish_non_exhaustive()
    }
}

/// A point in program execution where advice can be applied — the Rust port of
/// pyfly's `JoinPoint` dataclass.
///
/// The chain executor builds one `JoinPoint` per intercepted call, runs the
/// advice chain over it, and populates [`result`](JoinPoint::result) /
/// [`error`](JoinPoint::error) before the corresponding after-hooks run.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use firefly_aop::JoinPoint;
///
/// let jp = JoinPoint::new("service.OrderService", "create", Arc::new((1u32, "x")));
/// assert_eq!(jp.qualified_name(), "service.OrderService.create");
/// // downcast args back to the concrete tuple the caller boxed
/// let (id, name) = jp.args.downcast_ref::<(u32, &str)>().unwrap();
/// assert_eq!((*id, *name), (1, "x"));
/// ```
#[derive(Clone)]
pub struct JoinPoint {
    /// The qualified type name of the advised target (pyfly: `type(target)`
    /// stereotype + class), e.g. `service.OrderService`.
    pub type_name: String,
    /// The name of the method being intercepted (pyfly: `method_name`).
    pub method_name: String,
    /// The boxed call arguments (pyfly: `args` + `kwargs`). Opaque to the
    /// framework; advice downcasts as needed.
    pub args: AnyArc,
    /// The boxed return value, set by the executor after a successful call
    /// (pyfly: `return_value`).
    pub result: Option<AnyArc>,
    /// A rendered message for any error raised by the call, set by the executor
    /// before `after_throwing` runs (pyfly: `str(exception)`). The error itself
    /// is type-erased and re-propagated by the executor, so advice that needs
    /// to *observe* the failure reads this string.
    pub error: Option<String>,
}

impl JoinPoint {
    /// Build a fresh join point for a call to `type_name.method_name` with the
    /// boxed `args`. `result` and `error` start empty and are filled in by the
    /// chain executor.
    #[must_use]
    pub fn new(type_name: impl Into<String>, method_name: impl Into<String>, args: AnyArc) -> Self {
        Self {
            type_name: type_name.into(),
            method_name: method_name.into(),
            args,
            result: None,
            error: None,
        }
    }

    /// The qualified name the pointcut matches against:
    /// `"{type_name}.{method_name}"`.
    #[must_use]
    pub fn qualified_name(&self) -> String {
        format!("{}.{}", self.type_name, self.method_name)
    }
}

impl fmt::Debug for JoinPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinPoint")
            .field("type_name", &self.type_name)
            .field("method_name", &self.method_name)
            .field("has_result", &self.result.is_some())
            .field("error", &self.error)
            .finish()
    }
}

/// A captured *invocation* — the boxed original call the executor runs at the
/// innermost link of the chain. `Invocation` plays the role of pyfly's
/// `_invoke_original`: a single-shot async thunk producing an [`AdviceResult`].
///
/// Build one with [`invocation`].
pub struct Invocation<'a> {
    inner: Box<dyn FnOnce() -> AdviceFuture<'a> + Send + 'a>,
}

impl<'a> Invocation<'a> {
    /// Run the captured call.
    pub(crate) fn call(self) -> AdviceFuture<'a> {
        (self.inner)()
    }
}

impl fmt::Debug for Invocation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invocation").finish_non_exhaustive()
    }
}

/// Box an async closure into an [`Invocation`] for [`crate::intercept`].
///
/// The closure must return an [`AdviceResult`] — box the method's real return
/// value with `Arc::new(...)` on success and convert its error into
/// `Box<dyn Error + Send + Sync>` on failure.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use firefly_aop::{invocation, AnyArc};
///
/// let inv = invocation(|| async { Ok(Arc::new(42u32) as AnyArc) });
/// # let _ = inv;
/// ```
pub fn invocation<'a, F, Fut>(f: F) -> Invocation<'a>
where
    F: FnOnce() -> Fut + Send + 'a,
    Fut: Future<Output = AdviceResult> + Send + 'a,
{
    Invocation {
        inner: Box::new(move || -> AdviceFuture<'a> { Box::pin(f()) }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Port of pyfly tests/aop/test_types.py::TestJoinPoint ----

    #[test]
    fn test_creation_with_all_fields() {
        let mut jp = JoinPoint::new("service.Svc", "do_work", Arc::new((1i32, 2i32)));
        jp.result = Some(Arc::new(42i32));
        jp.error = Some("boom".to_string());

        assert_eq!(jp.type_name, "service.Svc");
        assert_eq!(jp.method_name, "do_work");
        let args = jp.args.downcast_ref::<(i32, i32)>().unwrap();
        assert_eq!(*args, (1, 2));
        assert_eq!(
            *jp.result.as_ref().unwrap().downcast_ref::<i32>().unwrap(),
            42
        );
        assert_eq!(jp.error.as_deref(), Some("boom"));
    }

    #[test]
    fn test_defaults_are_none() {
        let jp = JoinPoint::new("m", "m", Arc::new(()));
        assert!(jp.result.is_none());
        assert!(jp.error.is_none());
    }

    #[test]
    fn qualified_name_joins_type_and_method() {
        let jp = JoinPoint::new("service.OrderService", "create", Arc::new(()));
        assert_eq!(jp.qualified_name(), "service.OrderService.create");
    }

    #[test]
    fn advice_kind_as_str_is_pyfly_identical() {
        assert_eq!(AdviceKind::Before.as_str(), "before");
        assert_eq!(AdviceKind::AfterReturning.as_str(), "after_returning");
        assert_eq!(AdviceKind::AfterThrowing.as_str(), "after_throwing");
        assert_eq!(AdviceKind::After.as_str(), "after");
        assert_eq!(AdviceKind::Around.as_str(), "around");
        assert_eq!(AdviceKind::Around.to_string(), "around");
    }
}
