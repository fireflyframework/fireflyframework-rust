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

//! [`Mono<T>`] — a reactive producer of **at most one** value, an
//! error, or empty completion. The Rust analog of Reactor's
//! `reactor.core.publisher.Mono`.
//!
//! A `Mono<T>` wraps a `Pin<Box<dyn Future<Output = Result<Option<T>,
//! FireflyError>> + Send>>`:
//!
//! - `Ok(Some(value))` — the single value;
//! - `Ok(None)` — empty completion (the `Mono.empty()` case);
//! - `Err(e)` — a terminal error.
//!
//! Everything is `Send + 'static`, so a `Mono` drops straight into an
//! axum handler or any Tokio task. See the [crate docs](crate) for the
//! full Reactor concept table.

use std::future::{Future, IntoFuture};
use std::sync::Arc;
use std::time::Duration;

use firefly_kernel::FireflyError;
use futures::future::BoxFuture;
use futures::FutureExt;

use crate::backoff::Backoff;
use crate::flux::Flux;
use crate::scheduler::Scheduler;

/// The boxed future a [`Mono`] is built from.
type MonoFuture<T> = BoxFuture<'static, Result<Option<T>, FireflyError>>;

/// A reactive producer of at most one value (`0..1` + error).
///
/// Construct one with a factory ([`Mono::just`], [`Mono::empty`],
/// [`Mono::error`], [`Mono::from_future`], …), transform it with the
/// operator methods, then terminate it with [`Mono::block`],
/// [`Mono::subscribe`], or by awaiting [`Mono::into_future`].
///
/// ```
/// use firefly_reactive::Mono;
///
/// # async fn ex() {
/// let value = Mono::just(2)
///     .map(|x| x + 3)
///     .filter(|x| *x > 4)
///     .block()
///     .await
///     .unwrap();
/// assert_eq!(value, Some(5));
/// # }
/// ```
#[must_use = "a Mono is lazy and does nothing unless subscribed, blocked, or awaited"]
pub struct Mono<T> {
    future: MonoFuture<T>,
}

impl<T> Mono<T>
where
    T: Send + 'static,
{
    /// Wraps a raw future yielding `Result<Option<T>, FireflyError>`.
    /// The lowest-level constructor every factory funnels through.
    pub fn from_raw<F>(future: F) -> Self
    where
        F: Future<Output = Result<Option<T>, FireflyError>> + Send + 'static,
    {
        Self {
            future: future.boxed(),
        }
    }

    // ----------------------------------------------------------------
    // Factories
    // ----------------------------------------------------------------

    /// A `Mono` that emits exactly `value`. Reactor's `Mono.just`.
    ///
    /// ```
    /// # use firefly_reactive::Mono;
    /// # async fn ex() {
    /// assert_eq!(Mono::just(42).block().await.unwrap(), Some(42));
    /// # }
    /// ```
    pub fn just(value: T) -> Self {
        Self::from_raw(async move { Ok(Some(value)) })
    }

    /// A `Mono` from an [`Option`]: `Some` emits, `None` completes
    /// empty. Reactor's `Mono.justOrEmpty`.
    pub fn just_or_empty(value: Option<T>) -> Self {
        Self::from_raw(async move { Ok(value) })
    }

    /// A `Mono` that completes empty without emitting. Reactor's
    /// `Mono.empty`.
    pub fn empty() -> Self {
        Self::from_raw(async move { Ok(None) })
    }

    /// A `Mono` that fails immediately with `err`. Reactor's
    /// `Mono.error`.
    pub fn error(err: FireflyError) -> Self {
        Self::from_raw(async move { Err(err) })
    }

    /// Adapts a plain future of `T` into a `Mono`. Reactor's
    /// `Mono.fromFuture` — the always-present-value form.
    pub fn from_future<F>(future: F) -> Self
    where
        F: Future<Output = T> + Send + 'static,
    {
        Self::from_raw(async move { Ok(Some(future.await)) })
    }

    /// Adapts a fallible future (`Result<T, FireflyError>`) into a
    /// `Mono`, mapping `Ok` to a present value.
    pub fn from_result_future<F>(future: F) -> Self
    where
        F: Future<Output = Result<T, FireflyError>> + Send + 'static,
    {
        Self::from_raw(async move { future.await.map(Some) })
    }

    /// Defers construction until subscription: `factory` runs once per
    /// subscription. Reactor's `Mono.defer` — use it to capture
    /// per-subscription state (e.g. a fresh timestamp) and to make retry
    /// re-run the source.
    pub fn defer<F>(factory: F) -> Self
    where
        F: Fn() -> Mono<T> + Send + 'static,
    {
        Self::from_raw(async move { factory().into_future().await })
    }

    /// Runs a synchronous, possibly fallible callable at subscription
    /// time. Reactor's `Mono.fromCallable`. `Ok(None)` is empty
    /// completion.
    pub fn from_callable<F>(f: F) -> Self
    where
        F: FnOnce() -> Result<Option<T>, FireflyError> + Send + 'static,
    {
        Self::from_raw(async move { f() })
    }

    // ----------------------------------------------------------------
    // Transforming operators
    // ----------------------------------------------------------------

    /// Synchronously maps the emitted value. Empty and error pass
    /// through untouched. Reactor's `Mono.map`.
    pub fn map<U, F>(self, f: F) -> Mono<U>
    where
        U: Send + 'static,
        F: FnOnce(T) -> U + Send + 'static,
    {
        Mono::from_raw(async move { Ok(self.future.await?.map(f)) })
    }

    /// Asynchronously maps the value with a future-returning closure.
    /// Reactor's `Mono.map` over an async transform (a thinner
    /// `flatMap` for the non-reactive case).
    pub fn map_async<U, F, Fut>(self, f: F) -> Mono<U>
    where
        U: Send + 'static,
        F: FnOnce(T) -> Fut + Send + 'static,
        Fut: Future<Output = U> + Send + 'static,
    {
        Mono::from_raw(async move {
            match self.future.await? {
                Some(v) => Ok(Some(f(v).await)),
                None => Ok(None),
            }
        })
    }

    /// Maps the value to another `Mono` and flattens. Empty stays empty.
    /// Reactor's `Mono.flatMap`.
    pub fn flat_map<U, F>(self, f: F) -> Mono<U>
    where
        U: Send + 'static,
        F: FnOnce(T) -> Mono<U> + Send + 'static,
    {
        Mono::from_raw(async move {
            match self.future.await? {
                Some(v) => f(v).into_future().await,
                None => Ok(None),
            }
        })
    }

    /// Maps the value to a [`Flux`] and flattens to a many-valued
    /// stream. Reactor's `Mono.flatMapMany`.
    pub fn flat_map_many<U, F>(self, f: F) -> Flux<U>
    where
        U: Send + 'static,
        F: FnOnce(T) -> Flux<U> + Send + 'static,
    {
        let fut = self.future;
        Flux::from_stream(async_stream::try_stream! {
            if let Some(v) = fut.await? {
                let mut inner = f(v).into_stream();
                while let Some(item) = futures::StreamExt::next(&mut inner).await {
                    yield item?;
                }
            }
        })
    }

    /// Keeps the value only if `predicate` holds; otherwise completes
    /// empty. Reactor's `Mono.filter`.
    pub fn filter<F>(self, predicate: F) -> Mono<T>
    where
        F: FnOnce(&T) -> bool + Send + 'static,
    {
        Mono::from_raw(async move { Ok(self.future.await?.filter(|v| predicate(v))) })
    }

    /// Emits `default` if the source is empty. Reactor's
    /// `Mono.defaultIfEmpty`.
    pub fn default_if_empty(self, default: T) -> Mono<T> {
        Mono::from_raw(async move { Ok(Some(self.future.await?.unwrap_or(default))) })
    }

    /// Switches to `alternative` if the source is empty. Reactor's
    /// `Mono.switchIfEmpty`.
    pub fn switch_if_empty(self, alternative: Mono<T>) -> Mono<T> {
        Mono::from_raw(async move {
            match self.future.await? {
                Some(v) => Ok(Some(v)),
                None => alternative.into_future().await,
            }
        })
    }

    /// Ignores this `Mono`'s value and continues with `next` once it
    /// completes. Reactor's `Mono.then`.
    pub fn then<U>(self, next: Mono<U>) -> Mono<U>
    where
        U: Send + 'static,
    {
        Mono::from_raw(async move {
            self.future.await?;
            next.into_future().await
        })
    }

    /// Ignores this `Mono`'s value and emits `value` on completion.
    /// Reactor's `Mono.thenReturn`.
    pub fn then_return<U>(self, value: U) -> Mono<U>
    where
        U: Send + 'static,
    {
        Mono::from_raw(async move {
            self.future.await?;
            Ok(Some(value))
        })
    }

    /// Combines this value with another `Mono`'s value into a tuple. If
    /// either is empty, the result is empty. Reactor's `Mono.zipWith`.
    ///
    /// Short-circuits on the first error like Reactor's `Mono.zip`: the
    /// error is surfaced immediately and the other source is cancelled
    /// rather than awaited to completion. This matters when one side
    /// errors while the other is slow or never resolves.
    pub fn zip_with<U>(self, other: Mono<U>) -> Mono<(T, U)>
    where
        U: Send + 'static,
    {
        Mono::from_raw(async move {
            // `try_join!` polls both futures concurrently and resolves
            // with the first `Err` (dropping/cancelling the other), so an
            // error short-circuits instead of waiting on a pending peer.
            match futures::try_join!(self.future, other.future)? {
                (Some(x), Some(y)) => Ok(Some((x, y))),
                _ => Ok(None),
            }
        })
    }

    // ----------------------------------------------------------------
    // Error-handling operators
    // ----------------------------------------------------------------

    /// Recovers from any error by emitting `fallback`. Reactor's
    /// `Mono.onErrorReturn`.
    pub fn on_error_return(self, fallback: T) -> Mono<T> {
        Mono::from_raw(async move {
            match self.future.await {
                Ok(v) => Ok(v),
                Err(_) => Ok(Some(fallback)),
            }
        })
    }

    /// Recovers from any error by switching to the `Mono` produced by
    /// `f`. Reactor's `Mono.onErrorResume`.
    pub fn on_error_resume<F>(self, f: F) -> Mono<T>
    where
        F: FnOnce(FireflyError) -> Mono<T> + Send + 'static,
    {
        Mono::from_raw(async move {
            match self.future.await {
                Ok(v) => Ok(v),
                Err(e) => f(e).into_future().await,
            }
        })
    }

    /// Transforms the error without recovering. Reactor's
    /// `Mono.onErrorMap`.
    pub fn on_error_map<F>(self, f: F) -> Mono<T>
    where
        F: FnOnce(FireflyError) -> FireflyError + Send + 'static,
    {
        Mono::from_raw(async move { self.future.await.map_err(f) })
    }

    /// Re-subscribes to the source up to `n` times on error. Reactor's
    /// `Mono.retry(n)`. Because it re-runs the source, pair it with
    /// [`Mono::defer`] / [`Mono::from_callable`] for an effect that must
    /// re-execute.
    pub fn retry<F>(factory: F, n: usize) -> Mono<T>
    where
        F: Fn() -> Mono<T> + Send + 'static,
    {
        Mono::from_raw(async move {
            let mut attempts = 0usize;
            loop {
                match factory().into_future().await {
                    Ok(v) => return Ok(v),
                    Err(e) => {
                        if attempts >= n {
                            return Err(e);
                        }
                        attempts += 1;
                    }
                }
            }
        })
    }

    /// Re-subscribes on error with an exponential [`Backoff`] delay
    /// between attempts. Reactor's `Mono.retryWhen(Retry.backoff(..))`.
    pub fn retry_backoff<F>(factory: F, backoff: Backoff) -> Mono<T>
    where
        F: Fn() -> Mono<T> + Send + 'static,
    {
        Mono::from_raw(async move {
            let mut attempt = 0u32;
            loop {
                match factory().into_future().await {
                    Ok(v) => return Ok(v),
                    Err(e) => {
                        if attempt >= backoff.max_retries {
                            return Err(e);
                        }
                        let delay = backoff.delay_for(attempt);
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                        attempt += 1;
                    }
                }
            }
        })
    }

    /// Fails with a 504 [`FireflyError`] if the source does not complete
    /// within `duration`. Reactor's `Mono.timeout`.
    pub fn timeout(self, duration: Duration) -> Mono<T> {
        Mono::from_raw(async move {
            match tokio::time::timeout(duration, self.future).await {
                Ok(r) => r,
                Err(_) => Err(timeout_error(duration)),
            }
        })
    }

    /// Delays emission of the value by `duration`. Reactor's
    /// `Mono.delayElement`.
    pub fn delay_element(self, duration: Duration) -> Mono<T> {
        Mono::from_raw(async move {
            let out = self.future.await?;
            if out.is_some() {
                tokio::time::sleep(duration).await;
            }
            Ok(out)
        })
    }

    // ----------------------------------------------------------------
    // Side-effect (peek) operators
    // ----------------------------------------------------------------

    /// Runs `f` on the value if one is emitted, without changing it.
    /// Reactor's `Mono.doOnNext`.
    pub fn do_on_next<F>(self, f: F) -> Mono<T>
    where
        F: FnOnce(&T) + Send + 'static,
    {
        Mono::from_raw(async move {
            let out = self.future.await?;
            if let Some(v) = &out {
                f(v);
            }
            Ok(out)
        })
    }

    /// Runs `f` with the terminal value (`Some`/`None`) on successful
    /// completion. Reactor's `Mono.doOnSuccess`.
    pub fn do_on_success<F>(self, f: F) -> Mono<T>
    where
        F: FnOnce(Option<&T>) + Send + 'static,
    {
        Mono::from_raw(async move {
            let out = self.future.await?;
            f(out.as_ref());
            Ok(out)
        })
    }

    /// Runs `f` on error, without recovering. Reactor's `Mono.doOnError`.
    pub fn do_on_error<F>(self, f: F) -> Mono<T>
    where
        F: FnOnce(&FireflyError) + Send + 'static,
    {
        Mono::from_raw(async move {
            match self.future.await {
                Ok(v) => Ok(v),
                Err(e) => {
                    f(&e);
                    Err(e)
                }
            }
        })
    }

    /// Runs `f` on any terminal signal (success or error). Reactor's
    /// `Mono.doFinally`.
    pub fn do_on_finally<F>(self, f: F) -> Mono<T>
    where
        F: FnOnce() + Send + 'static,
    {
        Mono::from_raw(async move {
            let r = self.future.await;
            f();
            r
        })
    }

    // ----------------------------------------------------------------
    // Scheduling
    // ----------------------------------------------------------------

    /// Runs the whole upstream chain on `scheduler`. Reactor's
    /// `Mono.subscribeOn` — affects where the *source* executes.
    pub fn subscribe_on(self, scheduler: Scheduler) -> Mono<T> {
        Mono::from_raw(async move { scheduler.run(self.future).await })
    }

    /// Hops onto `scheduler` for everything downstream. Reactor's
    /// `Mono.publishOn`. For a single value the practical effect matches
    /// [`subscribe_on`](Mono::subscribe_on) — the work is moved onto the
    /// target scheduler.
    pub fn publish_on(self, scheduler: Scheduler) -> Mono<T> {
        Mono::from_raw(async move { scheduler.run(self.future).await })
    }

    // ----------------------------------------------------------------
    // Caching / fan-out
    // ----------------------------------------------------------------

    /// Memoizes the terminal signal so it is computed at most once and
    /// replayed to every subscriber. Reactor's `Mono.cache`.
    ///
    /// The returned `Mono` is cloneable via repeated [`block`](Mono::block) /
    /// [`subscribe`](Mono::subscribe); each call replays the cached
    /// outcome rather than re-running the source.
    pub fn cache(self) -> CachedMono<T>
    where
        T: Clone,
    {
        // The source runs exactly once. We wrap its terminal result in an
        // `Arc` (so the `Output` is `Clone`, as `Shared` requires —
        // `FireflyError` itself is not `Clone`) and share it. Every clone
        // of the resulting `Shared` future drives the *same* underlying
        // computation concurrently: no async lock is held across the
        // await, so subscribers are not serialized, a cached future may
        // re-enter `block()` on the same cache without deadlocking, and
        // cancelling one waiter does not poison the cache.
        fn wrap<T>(r: Result<Option<T>, FireflyError>) -> Arc<Result<Option<T>, FireflyError>> {
            Arc::new(r)
        }
        let wrap: fn(_) -> _ = wrap::<T>;
        let shared = self.future.map(wrap).shared();
        CachedMono { shared }
    }

    // ----------------------------------------------------------------
    // Conversions / terminals
    // ----------------------------------------------------------------

    /// Views this `Mono` as a [`Flux`] of 0-or-1 items. Reactor's
    /// `Mono.flux`.
    pub fn as_flux(self) -> Flux<T> {
        let fut = self.future;
        Flux::from_stream(async_stream::try_stream! {
            if let Some(v) = fut.await? {
                yield v;
            }
        })
    }

    /// Consumes the `Mono`, yielding the boxed terminal future. The
    /// escape hatch into raw async.
    pub fn into_future(self) -> MonoFuture<T> {
        self.future
    }

    /// Subscribes and awaits the terminal signal, returning
    /// `Ok(Some)` / `Ok(None)` / `Err`. The Rust analog of
    /// `Mono.block()` — but `async`, so it never blocks a thread.
    ///
    /// ```
    /// # use firefly_reactive::Mono;
    /// # async fn ex() {
    /// assert_eq!(Mono::just(1).block().await.unwrap(), Some(1));
    /// assert_eq!(Mono::<i32>::empty().block().await.unwrap(), None);
    /// # }
    /// ```
    pub async fn block(self) -> Result<Option<T>, FireflyError> {
        self.future.await
    }

    /// Subscribes with explicit value / error callbacks, driving the
    /// pipeline to completion on a spawned task. Reactor's
    /// `Mono.subscribe(consumer, errorConsumer)`. Returns immediately.
    pub fn subscribe<N, E>(self, on_value: N, on_error: E)
    where
        N: FnOnce(Option<T>) + Send + 'static,
        E: FnOnce(FireflyError) + Send + 'static,
    {
        tokio::spawn(async move {
            match self.future.await {
                Ok(v) => on_value(v),
                Err(e) => on_error(e),
            }
        });
    }
}

impl<T> IntoFuture for Mono<T>
where
    T: Send + 'static,
{
    type Output = Result<Option<T>, FireflyError>;
    type IntoFuture = MonoFuture<T>;

    fn into_future(self) -> Self::IntoFuture {
        self.future
    }
}

// --------------------------------------------------------------------
// Free-function factories that combine several monos
// --------------------------------------------------------------------

impl Mono<()> {
    /// Completes once *all* the given monos complete, discarding their
    /// values. Reactor's `Mono.when`. Errors short-circuit: the first
    /// `onError` from any source is surfaced immediately and the
    /// remaining sources are cancelled rather than awaited to
    /// completion (so a sibling that is slow or never completes cannot
    /// stall the fast-failing error).
    pub fn when(monos: Vec<Mono<()>>) -> Mono<()> {
        Mono::from_raw(async move {
            let futures: Vec<_> = monos.into_iter().map(Mono::into_future).collect();
            // `try_join_all` resolves with the first `Err` and drops the
            // outstanding futures, instead of `join_all`'s wait-for-all.
            futures::future::try_join_all(futures).await?;
            Ok(Some(()))
        })
    }
}

/// Zips two monos into a tuple. Free-function form of
/// [`Mono::zip_with`]; Reactor's `Mono.zip`.
pub fn zip<A, B>(a: Mono<A>, b: Mono<B>) -> Mono<(A, B)>
where
    A: Send + 'static,
    B: Send + 'static,
{
    a.zip_with(b)
}

/// Constructs a 504 timeout [`FireflyError`].
pub(crate) fn timeout_error(duration: Duration) -> FireflyError {
    FireflyError::new(
        "REACTIVE_TIMEOUT",
        "Reactive Timeout",
        504,
        format!("operation timed out after {duration:?}"),
    )
}

// --------------------------------------------------------------------
// cache()
// --------------------------------------------------------------------

/// The shared, run-once future a [`CachedMono`] memoizes. The terminal
/// result is held behind an `Arc` so the `Output` is `Clone` (a
/// requirement of [`Shared`](futures::future::Shared)); the underlying
/// source future still runs exactly once.
type SharedCache<T> = futures::future::Shared<
    futures::future::Map<
        MonoFuture<T>,
        fn(Result<Option<T>, FireflyError>) -> Arc<Result<Option<T>, FireflyError>>,
    >,
>;

/// A [`Mono`] whose terminal signal is computed once and replayed. The
/// product of [`Mono::cache`]. Clone it freely; every clone shares the
/// same memoized outcome.
///
/// Subscribers share a single in-flight computation without serializing:
/// awaiting from many tasks (or re-entering `block()` from within the
/// cached future itself) polls the same shared future rather than
/// contending on a lock, so the cache never deadlocks or stalls
/// unrelated subscribers behind one slow computation. Cancelling one
/// subscriber's await (e.g. via `tokio::time::timeout`) leaves the cache
/// intact for the next caller.
#[derive(Clone)]
pub struct CachedMono<T> {
    shared: SharedCache<T>,
}

impl<T> CachedMono<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Resolves the cached outcome, computing it on first access.
    pub async fn block(&self) -> Result<Option<T>, FireflyError> {
        // Poll a clone of the shared future: this neither holds a lock
        // across the await nor mutates shared state from this task, so it
        // is re-entrant- and cancellation-safe. The underlying source
        // runs once; we just clone its terminal result out of the `Arc`.
        let result = self.shared.clone().await;
        clone_result(result.as_ref())
    }

    /// Returns a fresh [`Mono`] backed by the cached outcome — usable
    /// anywhere a `Mono` is expected.
    pub fn as_mono(&self) -> Mono<T> {
        let me = self.clone();
        Mono::from_raw(async move { me.block().await })
    }
}

/// Clones a borrowed terminal result (`FireflyError` is not `Clone`, so
/// we reconstruct an equivalent error preserving code/title/status).
fn clone_result<T: Clone>(r: &Result<Option<T>, FireflyError>) -> Result<Option<T>, FireflyError> {
    match r {
        Ok(v) => Ok(v.clone()),
        Err(e) => Err(clone_error(e)),
    }
}

/// Reconstructs a [`FireflyError`] by value (the type holds a non-clone
/// `cause`, so the replay carries code/title/status/detail/fields but
/// not the original boxed cause).
pub(crate) fn clone_error(e: &FireflyError) -> FireflyError {
    let mut out = FireflyError::new(e.code.clone(), e.title.clone(), e.status, e.detail.clone());
    out.fields = e.fields.clone();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn just_emits_value() {
        assert_eq!(Mono::just(5).block().await.unwrap(), Some(5));
    }

    #[tokio::test]
    async fn empty_completes_empty() {
        assert_eq!(Mono::<i32>::empty().block().await.unwrap(), None);
    }

    #[tokio::test]
    async fn just_or_empty_none() {
        assert_eq!(
            Mono::<i32>::just_or_empty(None).block().await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn error_fails() {
        let e = Mono::<i32>::error(FireflyError::bad_request("x"))
            .block()
            .await
            .unwrap_err();
        assert_eq!(e.status, 400);
    }

    #[tokio::test]
    async fn map_transforms() {
        assert_eq!(
            Mono::just(2).map(|x| x * 10).block().await.unwrap(),
            Some(20)
        );
    }

    #[tokio::test]
    async fn map_async_transforms() {
        let out = Mono::just(3)
            .map_async(|x| async move { x + 1 })
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(4));
    }

    #[tokio::test]
    async fn flat_map_chains() {
        let out = Mono::just(2)
            .flat_map(|x| Mono::just(x * 3))
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(6));
    }

    #[tokio::test]
    async fn flat_map_many_fans_out() {
        let out = Mono::just(3i64)
            .flat_map_many(|x| Flux::range(0, x))
            .collect_list()
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(vec![0, 1, 2]));
    }

    #[tokio::test]
    async fn filter_keeps_and_drops() {
        assert_eq!(
            Mono::just(5).filter(|x| *x > 3).block().await.unwrap(),
            Some(5)
        );
        assert_eq!(
            Mono::just(2).filter(|x| *x > 3).block().await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn default_if_empty_fills() {
        assert_eq!(
            Mono::<i32>::empty()
                .default_if_empty(9)
                .block()
                .await
                .unwrap(),
            Some(9)
        );
    }

    #[tokio::test]
    async fn switch_if_empty_switches() {
        let out = Mono::<i32>::empty()
            .switch_if_empty(Mono::just(7))
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(7));
    }

    #[tokio::test]
    async fn then_and_then_return() {
        let out = Mono::just(1)
            .then(Mono::just("done"))
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some("done"));
        let out = Mono::just(1).then_return("ok").block().await.unwrap();
        assert_eq!(out, Some("ok"));
    }

    #[tokio::test]
    async fn zip_with_pairs() {
        let out = Mono::just(1)
            .zip_with(Mono::just("a"))
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some((1, "a")));
    }

    #[tokio::test]
    async fn zip_with_empty_is_empty() {
        let out = Mono::just(1)
            .zip_with(Mono::<&str>::empty())
            .block()
            .await
            .unwrap();
        assert_eq!(out, None);
    }

    #[tokio::test]
    async fn on_error_return_recovers() {
        let out = Mono::<i32>::error(FireflyError::internal("x"))
            .on_error_return(0)
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(0));
    }

    #[tokio::test]
    async fn on_error_resume_recovers() {
        let out = Mono::<i32>::error(FireflyError::internal("x"))
            .on_error_resume(|_| Mono::just(42))
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(42));
    }

    #[tokio::test]
    async fn on_error_map_transforms_error() {
        let e = Mono::<i32>::error(FireflyError::internal("x"))
            .on_error_map(|_| FireflyError::bad_request("mapped"))
            .block()
            .await
            .unwrap_err();
        assert_eq!(e.status, 400);
    }

    #[tokio::test]
    async fn retry_succeeds_after_failures() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = counter.clone();
        let out = Mono::retry(
            move || {
                let c = c.clone();
                Mono::from_callable(move || {
                    let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n < 2 {
                        Err(FireflyError::internal("retry"))
                    } else {
                        Ok(Some(n))
                    }
                })
            },
            5,
        )
        .block()
        .await
        .unwrap();
        assert_eq!(out, Some(2));
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_exhausts() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = counter.clone();
        let e = Mono::retry(
            move || {
                let c = c.clone();
                Mono::<i32>::from_callable(move || {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Err(FireflyError::internal("always"))
                })
            },
            2,
        )
        .block()
        .await
        .unwrap_err();
        assert_eq!(e.status, 500);
        // initial + 2 retries = 3 attempts
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_backoff_delays_and_succeeds() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = counter.clone();
        let out = Mono::retry_backoff(
            move || {
                let c = c.clone();
                Mono::from_callable(move || {
                    let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if n < 1 {
                        Err(FireflyError::internal("x"))
                    } else {
                        Ok(Some(n))
                    }
                })
            },
            Backoff::new(3, Duration::from_millis(10)),
        )
        .block()
        .await
        .unwrap();
        assert_eq!(out, Some(1));
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_fails() {
        let e = Mono::from_future(async {
            tokio::time::sleep(Duration::from_secs(10)).await;
            1
        })
        .timeout(Duration::from_millis(50))
        .block()
        .await
        .unwrap_err();
        assert_eq!(e.status, 504);
    }

    #[tokio::test(start_paused = true)]
    async fn timeout_passes_fast_value() {
        let out = Mono::just(1)
            .timeout(Duration::from_millis(50))
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(1));
    }

    #[tokio::test(start_paused = true)]
    async fn delay_element_delays() {
        let out = Mono::just(1)
            .delay_element(Duration::from_millis(20))
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(1));
    }

    #[tokio::test]
    async fn do_on_next_peeks() {
        let seen = Arc::new(std::sync::atomic::AtomicI32::new(0));
        let s = seen.clone();
        let out = Mono::just(7)
            .do_on_next(move |v| s.store(*v, std::sync::atomic::Ordering::SeqCst))
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(7));
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 7);
    }

    #[tokio::test]
    async fn do_on_error_and_finally() {
        let err_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fin_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let es = err_seen.clone();
        let fs = fin_seen.clone();
        let _ = Mono::<i32>::error(FireflyError::internal("x"))
            .do_on_error(move |_| es.store(true, std::sync::atomic::Ordering::SeqCst))
            .do_on_finally(move || fs.store(true, std::sync::atomic::Ordering::SeqCst))
            .block()
            .await;
        assert!(err_seen.load(std::sync::atomic::Ordering::SeqCst));
        assert!(fin_seen.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cache_runs_once() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = counter.clone();
        let cached = Mono::from_callable(move || {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(99))
        })
        .cache();
        assert_eq!(cached.block().await.unwrap(), Some(99));
        assert_eq!(cached.block().await.unwrap(), Some(99));
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn as_flux_roundtrips() {
        let out = Mono::just(3)
            .as_flux()
            .collect_list()
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(vec![3]));
        let out = Mono::<i32>::empty()
            .as_flux()
            .collect_list()
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(vec![]));
    }

    #[tokio::test]
    async fn defer_reruns() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = counter.clone();
        let m = Mono::defer(move || {
            let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Mono::just(n)
        });
        assert_eq!(m.block().await.unwrap(), Some(0));
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn when_waits_for_all() {
        let out = Mono::when(vec![Mono::just(()), Mono::just(())])
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(()));
    }

    #[tokio::test]
    async fn when_short_circuits_on_error() {
        let e = Mono::when(vec![
            Mono::just(()),
            Mono::error(FireflyError::internal("x")),
        ])
        .block()
        .await
        .unwrap_err();
        assert_eq!(e.status, 500);
    }

    #[tokio::test]
    async fn zip_free_fn() {
        let out = zip(Mono::just(1), Mono::just(2)).block().await.unwrap();
        assert_eq!(out, Some((1, 2)));
    }

    // --- regression: zip_with must short-circuit on error (bugs 2/6) ---
    // Reactor's `Mono.zip` surfaces the first `onError` and cancels the
    // other source. With `start_paused`, a hang would advance the virtual
    // clock to the 1h timeout; a correct short-circuit resolves instantly.
    #[tokio::test(start_paused = true)]
    async fn zip_with_short_circuits_on_error_against_pending_side() {
        let left: Mono<i32> = Mono::error(FireflyError::internal("boom"));
        let right: Mono<i32> = Mono::from_future(async { std::future::pending::<i32>().await });
        let res = tokio::time::timeout(Duration::from_secs(3600), left.zip_with(right).block())
            .await
            .expect("zip_with hung instead of short-circuiting on error");
        assert_eq!(res.unwrap_err().status, 500);
    }

    // Same defect via the free `zip` fn, with the error on the right side.
    #[tokio::test(start_paused = true)]
    async fn zip_free_fn_short_circuits_on_error() {
        let left: Mono<i32> = Mono::from_future(async { std::future::pending::<i32>().await });
        let right: Mono<i32> = Mono::error(FireflyError::internal("boom"));
        let res = tokio::time::timeout(Duration::from_secs(3600), zip(left, right).block())
            .await
            .expect("zip hung instead of short-circuiting on error");
        assert!(res.is_err());
    }

    // --- regression: when must fail fast on error (bugs 3/7) ---
    // The rustdoc promises "Errors short-circuit"; a sibling that never
    // completes must not stall the error.
    #[tokio::test(start_paused = true)]
    async fn when_short_circuits_against_never_completing_sibling() {
        let erroring: Mono<()> = Mono::error(FireflyError::internal("boom"));
        let never: Mono<()> = Mono::from_future(async { std::future::pending::<()>().await });
        let res = tokio::time::timeout(
            Duration::from_secs(3600),
            Mono::when(vec![erroring, never]).block(),
        )
        .await
        .expect("when hung instead of short-circuiting on error");
        assert_eq!(res.unwrap_err().status, 500);
    }

    // --- regression: cache() must not deadlock when a cached computation
    // re-enters block() on ANOTHER cache (bug 4) ---
    // The old implementation held a `tokio::sync::Mutex` guard across the
    // inner `.await`. Nesting a `block()` on a second cache inside the
    // first cache's computation, while a concurrent caller is parked on
    // that same outer cache, used to deadlock on the held guard (the
    // async Mutex is not reentrant and serializes across the await). The
    // shared-future cache holds no lock across the await, so it resolves.
    #[tokio::test(start_paused = true)]
    async fn cache_nested_block_does_not_deadlock() {
        let inner = Mono::just(3).cache();
        let inner_for_outer = inner.clone();
        let outer = Mono::from_future(async move {
            // Re-enter `block()` on a *different* cache from within this
            // cache's computation. Under the old lock-across-await this is
            // a held-guard hazard; with a shared future it just polls.
            let v = inner_for_outer.block().await.unwrap().unwrap();
            v + 4
        })
        .cache();

        // Drive the outer cache from two concurrent subscribers: with the
        // old code the second subscriber parked on the held outer guard
        // while the first was suspended inside the nested await.
        let a = outer.clone();
        let b = outer.clone();
        let res = tokio::time::timeout(Duration::from_secs(2), async move {
            tokio::join!(a.block(), b.block())
        })
        .await
        .expect("nested cache block deadlocked on held lock");
        assert_eq!(res.0.unwrap(), Some(7));
        assert_eq!(res.1.unwrap(), Some(7));
    }

    // --- regression: cache() survives a cancelled first await (bug 8) ---
    // Cancelling the first `block()` mid-flight (timeout) must not poison
    // the cache; the next `block()` must still resolve (previously it
    // panicked: "pending future present exactly once").
    #[tokio::test(start_paused = true)]
    async fn cache_survives_cancelled_first_block() {
        let cached = Mono::from_future(async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            42
        })
        .cache();
        // First call is cancelled (50ms timeout < 200ms work).
        let first = tokio::time::timeout(Duration::from_millis(50), cached.block()).await;
        assert!(first.is_err(), "first block should time out");
        // Second call must resolve, not panic.
        let second = cached.block().await.unwrap();
        assert_eq!(second, Some(42));
    }

    // --- regression: cache() does not serialize concurrent subscribers
    // behind one in-flight computation (bug 10) ---
    #[tokio::test(start_paused = true)]
    async fn cache_concurrent_blocks_share_one_run() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = counter.clone();
        let cached = Mono::from_future(async move {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(1)).await;
            5
        })
        .cache();
        let a = cached.clone();
        let b = cached.clone();
        // Both started concurrently; they share the single in-flight run.
        let (ra, rb) = tokio::join!(a.block(), b.block());
        assert_eq!(ra.unwrap(), Some(5));
        assert_eq!(rb.unwrap(), Some(5));
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn from_future_and_result_future() {
        assert_eq!(
            Mono::from_future(async { 5 }).block().await.unwrap(),
            Some(5)
        );
        let out = Mono::from_result_future(async { Ok::<_, FireflyError>(6) })
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(6));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_on_parallel() {
        let out = Mono::just(1)
            .subscribe_on(Scheduler::Parallel)
            .block()
            .await
            .unwrap();
        assert_eq!(out, Some(1));
    }

    #[tokio::test]
    async fn subscribe_invokes_callback() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        Mono::just(8).subscribe(
            move |v| {
                let _ = tx.send(v);
            },
            |_| {},
        );
        assert_eq!(rx.await.unwrap(), Some(8));
    }

    #[tokio::test]
    async fn into_future_interop() {
        let m: Mono<i32> = Mono::just(3);
        let out = m.await.unwrap();
        assert_eq!(out, Some(3));
    }

    fn assert_send_static<T: Send + 'static>(_: &T) {}

    #[tokio::test]
    async fn mono_is_send_static() {
        let m = Mono::just(1).map(|x| x + 1);
        assert_send_static(&m);
        let _ = m.block().await;
    }
}
