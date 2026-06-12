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

//! Panic-safe function execution — the Rust port of the Go `Try` /
//! `TryOf` helpers (themselves mirrors of the Java `Try.of` and .NET
//! `Try.Of` helpers).

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Error returned by [`try_run`] and [`try_of`]: either the closure's
/// own error, or a panic captured and surfaced as an error — the Rust
/// analog of Go's `recover()`-to-`error` conversion.
#[derive(Debug, thiserror::Error)]
pub enum TryError<E: std::error::Error> {
    /// The closure panicked; the panic payload is rendered as a string.
    /// Mirrors Go's `panic: %v` wrapping.
    #[error("panic: {0}")]
    Panic(String),
    /// The closure returned an ordinary error, forwarded unchanged.
    #[error(transparent)]
    Inner(E),
}

impl<E: std::error::Error> TryError<E> {
    /// Reports whether this error was produced by a captured panic.
    pub fn is_panic(&self) -> bool {
        matches!(self, TryError::Panic(_))
    }
}

/// Runs `f` and recovers from any panic, returning the panic value
/// wrapped as a [`TryError::Panic`]. Ordinary errors pass through as
/// [`TryError::Inner`]. Mirrors the Go `utils.Try` helper.
///
/// The closure is executed under [`std::panic::catch_unwind`] with an
/// [`AssertUnwindSafe`] wrapper so that the helper is as universally
/// applicable as Go's `recover()`; callers that share mutable state
/// across the panic boundary should ensure it cannot be observed in a
/// broken state.
pub fn try_run<E, F>(f: F) -> Result<(), TryError<E>>
where
    E: std::error::Error,
    F: FnOnce() -> Result<(), E>,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(TryError::Inner(e)),
        Err(payload) => Err(TryError::Panic(panic_message(payload))),
    }
}

/// Runs `f` and recovers from any panic, returning the value or the
/// panic-as-error. The typed variant of [`try_run`], mirroring the Go
/// `utils.TryOf[T]` helper.
pub fn try_of<T, E, F>(f: F) -> Result<T, TryError<E>>
where
    E: std::error::Error,
    F: FnOnce() -> Result<T, E>,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(TryError::Inner(e)),
        Err(payload) => Err(TryError::Panic(panic_message(payload))),
    }
}

/// Renders a panic payload as a human-readable string, matching the
/// best-effort formatting Go gets from `panic: %v`.
fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::convert::Infallible;
    use std::fmt;

    #[derive(Debug, PartialEq)]
    struct TestErr(&'static str);

    impl fmt::Display for TestErr {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl std::error::Error for TestErr {}

    /// Port of Go `TestTry` — normal path, panic-as-error, typed value,
    /// typed panic. The panic-capturing calls run under a silenced hook
    /// so intentional panics do not pollute test output; this test owns
    /// the hook exclusively because it is the only panicking test.
    #[test]
    fn try_ports_go_test_try() {
        // Normal path returns Ok.
        assert!(try_run(|| Ok::<(), TestErr>(())).is_ok());

        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let err = try_run(|| -> Result<(), TestErr> { panic!("bad") });
        let typed_ok = try_of(|| Ok::<i32, TestErr>(5));
        let typed_panic = try_of(|| -> Result<i32, Infallible> { panic!("nope") });
        let string_payload =
            try_run(|| -> Result<(), TestErr> { std::panic::panic_any(format!("boom {}", 42)) });
        std::panic::set_hook(prev);

        // Panic surfaces as an error whose message contains "panic".
        let err = err.expect_err("expected panic-as-error");
        assert!(err.is_panic());
        assert!(err.to_string().contains("panic"));
        assert_eq!(err.to_string(), "panic: bad");

        // Typed try returns the value.
        assert_eq!(typed_ok.unwrap(), 5);

        // Typed try panic returns the panic-as-error.
        let err = typed_panic.expect_err("expected panic-as-error");
        assert_eq!(err.to_string(), "panic: nope");

        // String payloads (panic_any / format!) are captured too.
        let err = string_payload.expect_err("expected panic-as-error");
        assert_eq!(err.to_string(), "panic: boom 42");
    }

    /// Ordinary errors pass through transparently, preserving Display.
    #[test]
    fn try_forwards_inner_errors() {
        let err = try_run(|| Err::<(), _>(TestErr("kaput"))).expect_err("expected error");
        assert!(!err.is_panic());
        assert_eq!(err.to_string(), "kaput");
        match err {
            TryError::Inner(TestErr("kaput")) => {}
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    /// Rust-specific: the error type is Send + Sync so it can cross
    /// task and thread boundaries.
    #[test]
    fn try_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TryError<TestErr>>();
    }
}
