//! [`Flux<T>`] — a reactive producer of **0..N** values followed by a
//! terminal completion or error. The Rust analog of Reactor's
//! `reactor.core.publisher.Flux`.
//!
//! A `Flux<T>` wraps a `Pin<Box<dyn Stream<Item = Result<T,
//! FireflyError>> + Send>>`. An `Err` item is **terminal**: every
//! operator short-circuits on the first error and propagates it
//! downstream, exactly like Reactor's `onError` signal. There is no
//! per-item error channel — use [`Flux::on_error_continue`] to skip a
//! failing element and keep going.
//!
//! Everything is `Send + 'static`, so a `Flux` streams straight out of
//! an axum handler (NDJSON / SSE) or any Tokio task.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::hash::Hash;
use std::sync::Arc;
use std::time::Duration;

use firefly_kernel::FireflyError;
use futures::stream::{BoxStream, Stream, StreamExt};

use crate::backoff::Backoff;
use crate::mono::{timeout_error, Mono};
use crate::scheduler::Scheduler;
use crate::sink::FluxSink;

/// The boxed stream a [`Flux`] is built from.
type FluxStream<T> = BoxStream<'static, Result<T, FireflyError>>;

/// A reactive producer of 0..N values plus a terminal completion/error.
///
/// Build one with a factory ([`Flux::just`], [`Flux::from_iter`],
/// [`Flux::range`], [`Flux::interval`], …), transform with the operator
/// methods, then terminate it: aggregate to a [`Mono`]
/// ([`collect_list`](Flux::collect_list), [`reduce`](Flux::reduce),
/// [`count`](Flux::count)), drain with [`subscribe`](Flux::subscribe),
/// or escape into a raw [`Stream`] with [`to_stream`](Flux::to_stream).
///
/// ```
/// use firefly_reactive::Flux;
///
/// # async fn ex() {
/// let out = Flux::range(1, 5)
///     .filter(|x| x % 2 == 1)
///     .map(|x| x * 10)
///     .collect_list()
///     .block()
///     .await
///     .unwrap()
///     .unwrap();
/// assert_eq!(out, vec![10, 30, 50]);
/// # }
/// ```
#[must_use = "a Flux is lazy and does nothing unless subscribed, collected, or drained"]
pub struct Flux<T> {
    stream: FluxStream<T>,
}

impl<T> Flux<T>
where
    T: Send + 'static,
{
    /// Wraps a raw `Stream<Item = Result<T, FireflyError>>`. The
    /// lowest-level constructor every factory funnels through. Reactor's
    /// adaptation of an arbitrary `Publisher`.
    pub fn from_stream<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<T, FireflyError>> + Send + 'static,
    {
        Self {
            stream: stream.boxed(),
        }
    }

    // ----------------------------------------------------------------
    // Factories
    // ----------------------------------------------------------------

    /// A `Flux` emitting each item of `items` in order. Reactor's
    /// `Flux.just` / `Flux.fromIterable`.
    pub fn just(items: Vec<T>) -> Self {
        Self::from_iter(items)
    }

    /// A `Flux` from any [`IntoIterator`]. Reactor's
    /// `Flux.fromIterable`.
    ///
    /// Named `from_iter` for Reactor parity; it deliberately does not
    /// implement [`std::iter::FromIterator`] because the source items are
    /// already-unwrapped `T`, not `Result<T, _>`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_iter<I>(items: I) -> Self
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: Send + 'static,
    {
        let iter = items.into_iter();
        Self::from_stream(futures::stream::iter(iter.map(Ok)))
    }

    /// A `Flux` that emits no items and completes. Reactor's
    /// `Flux.empty`.
    pub fn empty() -> Self {
        Self::from_stream(futures::stream::empty())
    }

    /// A `Flux` that fails immediately with `err`. Reactor's
    /// `Flux.error`.
    pub fn error(err: FireflyError) -> Self {
        Self::from_stream(futures::stream::once(async move { Err(err) }))
    }

    /// A `Flux` that never emits and never completes. Reactor's
    /// `Flux.never` — useful as a neutral element / in `merge`.
    pub fn never() -> Self {
        Self::from_stream(futures::stream::pending())
    }

    /// Defers construction until subscription; `factory` runs once per
    /// subscription. Reactor's `Flux.defer`.
    pub fn defer<F>(factory: F) -> Self
    where
        F: FnOnce() -> Flux<T> + Send + 'static,
    {
        Self::from_stream(async_stream::try_stream! {
            let mut inner = factory().into_stream();
            while let Some(item) = inner.next().await {
                yield item?;
            }
        })
    }

    /// Bridges a raw [`Stream`] of plain `T` (infallible items) into a
    /// `Flux`. The escape hatch *in* from non-reactive streams.
    pub fn from_value_stream<S>(stream: S) -> Self
    where
        S: Stream<Item = T> + Send + 'static,
    {
        Self::from_stream(stream.map(Ok))
    }

    /// Drives a [`Flux`] imperatively through a [`FluxSink`]. Reactor's
    /// `Flux.create`. The callback pushes items via the sink.
    ///
    /// Emissions are buffered through an **unbounded** channel, matching
    /// Reactor's default `OverflowStrategy.BUFFER`: a synchronous burst
    /// producer that emits every item (and any terminal `error`) before
    /// the stream is first polled never drops an item or loses the
    /// terminal signal. For bounded backpressure (where
    /// [`FluxSink::send`] awaits a free slot) use
    /// [`create_with_buffer`](Flux::create_with_buffer).
    pub fn create<F>(producer: F) -> Self
    where
        F: FnOnce(FluxSink<T>) + Send + 'static,
    {
        let (sink, mut rx) = FluxSink::unbounded();
        producer(sink);
        Self::from_stream(async_stream::try_stream! {
            while let Some(item) = rx.recv().await {
                yield item?;
            }
        })
    }

    /// [`create`](Flux::create) with an explicit *bounded* backpressure
    /// buffer of `buffer` slots. Unlike [`create`](Flux::create)'s
    /// unbounded default, a producer using [`FluxSink::next`] here may be
    /// told (via a `false` return) that the buffer is full; use
    /// [`FluxSink::send`] to await a free slot and apply real
    /// backpressure.
    pub fn create_with_buffer<F>(buffer: usize, producer: F) -> Self
    where
        F: FnOnce(FluxSink<T>) + Send + 'static,
    {
        let (sink, mut rx) = FluxSink::channel(buffer);
        producer(sink);
        Self::from_stream(async_stream::try_stream! {
            while let Some(item) = rx.recv().await {
                yield item?;
            }
        })
    }

    // ----------------------------------------------------------------
    // Transforming operators
    // ----------------------------------------------------------------

    /// Synchronously maps each item. Reactor's `Flux.map`.
    pub fn map<U, F>(self, f: F) -> Flux<U>
    where
        U: Send + 'static,
        F: FnMut(T) -> U + Send + 'static,
    {
        let mut f = f;
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                yield f(item?);
            }
        })
    }

    /// Asynchronously maps each item, awaiting the mapping future in
    /// order (sequential). Reactor's `Flux.flatMap` with concurrency 1
    /// for the non-reactive case.
    pub fn map_async<U, F, Fut>(self, f: F) -> Flux<U>
    where
        U: Send + 'static,
        F: FnMut(T) -> Fut + Send + 'static,
        Fut: Future<Output = U> + Send + 'static,
    {
        let mut f = f;
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                yield f(item?).await;
            }
        })
    }

    /// Maps each item to a [`Flux`] and merges, running up to
    /// `concurrency` inner publishers at once (interleaved). Reactor's
    /// `Flux.flatMap(mapper, concurrency)`.
    ///
    /// Ordering across inner fluxes is not guaranteed — use
    /// [`concat_map`](Flux::concat_map) for sequential ordering.
    pub fn flat_map<U, F>(self, concurrency: usize, f: F) -> Flux<U>
    where
        U: Send + 'static,
        F: FnMut(T) -> Flux<U> + Send + 'static,
    {
        let mut f = f;
        let s = self.stream;
        let concurrency = concurrency.max(1);
        let inner_streams = async_stream::try_stream! {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                yield f(item?).into_stream();
            }
        };
        // flatten_unordered over a TryStream of streams, bounded.
        Flux::from_stream(
            inner_streams
                .map(|res: Result<FluxStream<U>, FireflyError>| match res {
                    Ok(stream) => stream,
                    Err(e) => futures::stream::once(async move { Err(e) }).boxed(),
                })
                .flatten_unordered(concurrency),
        )
    }

    /// Maps each item to a [`Flux`] and concatenates them in order
    /// (fully draining one before the next). Reactor's `Flux.concatMap`.
    pub fn concat_map<U, F>(self, f: F) -> Flux<U>
    where
        U: Send + 'static,
        F: FnMut(T) -> Flux<U> + Send + 'static,
    {
        let mut f = f;
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                let mut inner = f(item?).into_stream();
                while let Some(inner_item) = inner.next().await {
                    yield inner_item?;
                }
            }
        })
    }

    /// Maps each item to an iterable and flattens. Reactor's
    /// `Flux.flatMapIterable`.
    pub fn flat_map_iterable<U, I, F>(self, f: F) -> Flux<U>
    where
        U: Send + 'static,
        I: IntoIterator<Item = U> + Send + 'static,
        I::IntoIter: Send,
        F: FnMut(T) -> I + Send + 'static,
    {
        let mut f = f;
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                for out in f(item?) {
                    yield out;
                }
            }
        })
    }

    /// Keeps only items for which `predicate` holds. Reactor's
    /// `Flux.filter`.
    pub fn filter<F>(self, predicate: F) -> Flux<T>
    where
        F: FnMut(&T) -> bool + Send + 'static,
    {
        let mut predicate = predicate;
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                let v = item?;
                if predicate(&v) {
                    yield v;
                }
            }
        })
    }

    /// Emits at most the first `n` items, then completes. Reactor's
    /// `Flux.take`.
    pub fn take(self, n: usize) -> Flux<T> {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            if n == 0 { return; }
            futures::pin_mut!(s);
            let mut taken = 0usize;
            while let Some(item) = s.next().await {
                yield item?;
                taken += 1;
                if taken >= n { break; }
            }
        })
    }

    /// Emits items while `predicate` holds, then completes (without
    /// emitting the failing item). Reactor's `Flux.takeWhile`.
    pub fn take_while<F>(self, predicate: F) -> Flux<T>
    where
        F: FnMut(&T) -> bool + Send + 'static,
    {
        let mut predicate = predicate;
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                let v = item?;
                if !predicate(&v) { break; }
                yield v;
            }
        })
    }

    /// Emits only the last `n` items. Reactor's `Flux.takeLast`.
    pub fn take_last(self, n: usize) -> Flux<T> {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut buf: std::collections::VecDeque<T> = std::collections::VecDeque::new();
            while let Some(item) = s.next().await {
                buf.push_back(item?);
                if n > 0 && buf.len() > n { buf.pop_front(); }
            }
            if n == 0 { return; }
            for v in buf { yield v; }
        })
    }

    /// Skips the first `n` items. Reactor's `Flux.skip`.
    pub fn skip(self, n: usize) -> Flux<T> {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut skipped = 0usize;
            while let Some(item) = s.next().await {
                let v = item?;
                if skipped < n { skipped += 1; continue; }
                yield v;
            }
        })
    }

    /// Skips items while `predicate` holds, then emits the rest.
    /// Reactor's `Flux.skipWhile`.
    pub fn skip_while<F>(self, predicate: F) -> Flux<T>
    where
        F: FnMut(&T) -> bool + Send + 'static,
    {
        let mut predicate = predicate;
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut skipping = true;
            while let Some(item) = s.next().await {
                let v = item?;
                if skipping && predicate(&v) { continue; }
                skipping = false;
                yield v;
            }
        })
    }

    /// Drops duplicate items (by equality+hash), emitting each distinct
    /// value at most once over the whole stream. Reactor's
    /// `Flux.distinct`.
    pub fn distinct(self) -> Flux<T>
    where
        T: Clone + Eq + Hash,
    {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut seen: HashSet<T> = HashSet::new();
            while let Some(item) = s.next().await {
                let v = item?;
                if seen.insert(v.clone()) {
                    yield v;
                }
            }
        })
    }

    /// Drops *consecutive* duplicates only. Reactor's
    /// `Flux.distinctUntilChanged`.
    pub fn distinct_until_changed(self) -> Flux<T>
    where
        T: Clone + PartialEq,
    {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut last: Option<T> = None;
            while let Some(item) = s.next().await {
                let v = item?;
                if last.as_ref() != Some(&v) {
                    last = Some(v.clone());
                    yield v;
                }
            }
        })
    }

    /// Emits a running accumulation: the seed, then the result of
    /// folding each item into the accumulator. Reactor's `Flux.scan`.
    pub fn scan<A, F>(self, seed: A, f: F) -> Flux<A>
    where
        A: Clone + Send + 'static,
        F: FnMut(A, T) -> A + Send + 'static,
    {
        let mut f = f;
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut acc = seed;
            yield acc.clone();
            while let Some(item) = s.next().await {
                acc = f(acc.clone(), item?);
                yield acc.clone();
            }
        })
    }

    /// Pairs each item with its 0-based index. Reactor's `Flux.index`.
    pub fn index(self) -> Flux<(usize, T)> {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut i = 0usize;
            while let Some(item) = s.next().await {
                yield (i, item?);
                i += 1;
            }
        })
    }

    /// Prepends `items` ahead of this stream. Reactor's
    /// `Flux.startWith`.
    pub fn start_with<I>(self, items: I) -> Flux<T>
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: Send + 'static,
    {
        Flux::from_iter(items).concat_with(self)
    }

    // ----------------------------------------------------------------
    // Combining operators
    // ----------------------------------------------------------------

    /// Interleaves this `Flux` with `other` as items arrive from either.
    /// Reactor's `Flux.mergeWith`.
    pub fn merge_with(self, other: Flux<T>) -> Flux<T> {
        Flux::from_stream(futures::stream::select(self.stream, other.stream))
    }

    /// Concatenates: fully drains this `Flux`, then `other`. Reactor's
    /// `Flux.concatWith`.
    pub fn concat_with(self, other: Flux<T>) -> Flux<T> {
        let a = self.stream;
        let b = other.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(a);
            while let Some(item) = a.next().await {
                yield item?;
            }
            futures::pin_mut!(b);
            while let Some(item) = b.next().await {
                yield item?;
            }
        })
    }

    /// Pairs items positionally with `other`, completing when either
    /// completes. Reactor's `Flux.zipWith`.
    ///
    /// Short-circuits like Reactor: the first `onError` from either
    /// source is propagated immediately and the first `onComplete`
    /// terminates the pairing — neither waits on the other side to make
    /// progress. This matters when one source errors or completes while
    /// the other is slow or never-ending (e.g. `Flux::never()`).
    pub fn zip_with<U>(self, other: Flux<U>) -> Flux<(T, U)>
    where
        U: Send + 'static,
    {
        let a = self.stream;
        let b = other.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(a);
            futures::pin_mut!(b);
            loop {
                // Poll both sides concurrently but resolve the iteration
                // as soon as either side errors or completes — a value is
                // only yielded once BOTH sides have produced one. Using
                // `select` (rather than `join!`) means a terminal signal
                // on one side is not blocked behind a pending/never-ending
                // other side, matching Reactor's `zip` cancellation.
                let na = a.next();
                let nb = b.next();
                futures::pin_mut!(na);
                futures::pin_mut!(nb);
                // Each arm yields `Option<Result<(T, U), _>>`: `None`
                // means a source completed (terminate the zip); `Some`
                // carries the paired result (or the first error). The
                // first side resolved to a value still needs the other
                // side's next item, but a completion/error on the side
                // that resolves first short-circuits without awaiting the
                // other — that is the cancellation behavior.
                let paired = match futures::future::select(na, nb).await {
                    // `a.next()` resolved first.
                    futures::future::Either::Left((na_res, nb_fut)) => match na_res {
                        None => None,
                        Some(Err(e)) => Some(Err(e)),
                        Some(Ok(x)) => nb_fut.await.map(|y| y.map(|y| (x, y))),
                    },
                    // `b.next()` resolved first — symmetric.
                    futures::future::Either::Right((nb_res, na_fut)) => match nb_res {
                        None => None,
                        Some(Err(e)) => Some(Err(e)),
                        Some(Ok(y)) => na_fut.await.map(|x| x.map(|x| (x, y))),
                    },
                };
                match paired {
                    Some(pair) => yield pair?,
                    None => break,
                }
            }
        })
    }

    /// Emits a fresh tuple of the *latest* value from each source
    /// whenever either emits (once both have produced at least one).
    /// Reactor's `Flux.combineLatest`.
    pub fn combine_latest<U>(self, other: Flux<U>) -> Flux<(T, U)>
    where
        T: Clone,
        U: Clone + Send + 'static,
    {
        let a = self.stream.map(Either::Left);
        let b = other.stream.map(Either::Right);
        let merged = futures::stream::select(a, b);
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(merged);
            let mut last_a: Option<T> = None;
            let mut last_b: Option<U> = None;
            while let Some(item) = merged.next().await {
                match item {
                    Either::Left(r) => last_a = Some(r?),
                    Either::Right(r) => last_b = Some(r?),
                }
                if let (Some(x), Some(y)) = (&last_a, &last_b) {
                    yield (x.clone(), y.clone());
                }
            }
        })
    }

    /// Switches to `alternative` if this `Flux` completes without
    /// emitting anything. Reactor's `Flux.switchIfEmpty`.
    pub fn switch_if_empty(self, alternative: Flux<T>) -> Flux<T> {
        let s = self.stream;
        let alt = alternative.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut emitted = false;
            while let Some(item) = s.next().await {
                emitted = true;
                yield item?;
            }
            if !emitted {
                futures::pin_mut!(alt);
                while let Some(item) = alt.next().await {
                    yield item?;
                }
            }
        })
    }

    /// Emits `default` if this `Flux` is empty. Reactor's
    /// `Flux.defaultIfEmpty`.
    pub fn default_if_empty(self, default: T) -> Flux<T> {
        self.switch_if_empty(Flux::just(vec![default]))
    }

    // ----------------------------------------------------------------
    // Backpressure operators
    // ----------------------------------------------------------------

    /// Buffers items in a bounded channel of `capacity`; a slow consumer
    /// applies backpressure to the producer. Reactor's
    /// `Flux.onBackpressureBuffer`.
    pub fn on_backpressure_buffer(self, capacity: usize) -> Flux<T> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(capacity.max(1));
        let s = self.stream;
        tokio::spawn(async move {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                if tx.send(item).await.is_err() {
                    break;
                }
            }
        });
        Flux::from_stream(async_stream::try_stream! {
            while let Some(item) = rx.recv().await {
                yield item?;
            }
        })
    }

    /// Buffers up to `capacity`; when full, *drops the newest* items
    /// rather than blocking. Reactor's `Flux.onBackpressureDrop`.
    pub fn on_backpressure_drop(self, capacity: usize) -> Flux<T> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(capacity.max(1));
        let s = self.stream;
        tokio::spawn(async move {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                // try_send drops the item when the buffer is full.
                if let Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) = tx.try_send(item) {
                    break;
                }
            }
        });
        Flux::from_stream(async_stream::try_stream! {
            while let Some(item) = rx.recv().await {
                yield item?;
            }
        })
    }

    /// Keeps only the *latest* item when the consumer falls behind: each
    /// new value overwrites any still-unconsumed predecessor, so a slow
    /// consumer always observes the most recent value rather than a
    /// stale backlog. Reactor's `Flux.onBackpressureLatest`.
    ///
    /// Backed by a single shared slot guarded by a mutex and a
    /// [`Notify`](tokio::sync::Notify); the source drives ahead on a
    /// spawned task while the consumer pulls at its own pace.
    pub fn on_backpressure_latest(self) -> Flux<T> {
        use std::sync::Mutex as StdMutex;
        use tokio::sync::Notify;

        // slot: latest pending item; `done` flips when the source ends.
        let slot: Arc<StdMutex<Option<Result<T, FireflyError>>>> = Arc::new(StdMutex::new(None));
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notify = Arc::new(Notify::new());

        let producer_slot = slot.clone();
        let producer_done = done.clone();
        let producer_notify = notify.clone();
        let s = self.stream;
        tokio::spawn(async move {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                *producer_slot.lock().expect("slot mutex") = Some(item);
                producer_notify.notify_one();
            }
            producer_done.store(true, std::sync::atomic::Ordering::SeqCst);
            producer_notify.notify_one();
        });

        Flux::from_stream(async_stream::try_stream! {
            loop {
                // Bind to a local so the guard drops before any await/yield.
                let pulled = slot.lock().expect("slot mutex").take();
                match pulled {
                    Some(item) => yield item?,
                    None => {
                        if done.load(std::sync::atomic::Ordering::SeqCst) {
                            // Drain any final value published just before completion.
                            let last = slot.lock().expect("slot mutex").take();
                            if let Some(item) = last {
                                yield item?;
                            }
                            break;
                        }
                        notify.notified().await;
                    }
                }
            }
        })
    }

    /// Bounds in-flight demand to `n` items at a time via a bounded
    /// channel. Reactor's `Flux.limitRate`.
    pub fn limit_rate(self, n: usize) -> Flux<T> {
        self.on_backpressure_buffer(n)
    }

    // ----------------------------------------------------------------
    // Windowing / batching
    // ----------------------------------------------------------------

    /// Groups items into vectors of up to `size`. The final batch may be
    /// shorter. Reactor's `Flux.buffer(size)`.
    pub fn buffer(self, size: usize) -> Flux<Vec<T>> {
        let size = size.max(1);
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut batch: Vec<T> = Vec::with_capacity(size);
            while let Some(item) = s.next().await {
                batch.push(item?);
                if batch.len() >= size {
                    yield std::mem::take(&mut batch);
                }
            }
            if !batch.is_empty() {
                yield batch;
            }
        })
    }

    /// Splits the stream into consecutive sub-[`Flux`]es of up to `size`
    /// items each. Reactor's `Flux.window(size)`. Implemented as
    /// `buffer(size)` mapped back into fluxes.
    pub fn window(self, size: usize) -> Flux<Flux<T>> {
        self.buffer(size).map(Flux::from_iter)
    }

    /// Partitions items by `key_fn` into a `Flux` of `(key,
    /// Flux<value>)` groups. Reactor's `Flux.groupBy`.
    ///
    /// Because Rust streams are pull-based, this materializes the source
    /// (eagerly draining it) and then re-emits per-key sub-fluxes — a
    /// faithful logical equivalent for finite streams.
    pub fn group_by<K, F>(self, key_fn: F) -> Flux<(K, Flux<T>)>
    where
        K: Eq + Hash + Clone + Send + 'static,
        F: FnMut(&T) -> K + Send + 'static,
    {
        let mut key_fn = key_fn;
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut order: Vec<K> = Vec::new();
            let mut groups: HashMap<K, Vec<T>> = HashMap::new();
            while let Some(item) = s.next().await {
                let v = item?;
                let k = key_fn(&v);
                if !groups.contains_key(&k) {
                    order.push(k.clone());
                }
                groups.entry(k).or_default().push(v);
            }
            for k in order {
                let values = groups.remove(&k).unwrap_or_default();
                yield (k, Flux::from_iter(values));
            }
        })
    }

    // ----------------------------------------------------------------
    // Time-based operators
    // ----------------------------------------------------------------

    /// Delays each item by `duration` before emitting it. Reactor's
    /// `Flux.delayElements`.
    pub fn delay_elements(self, duration: Duration) -> Flux<T> {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                let v = item?;
                tokio::time::sleep(duration).await;
                yield v;
            }
        })
    }

    /// Emits the most recent item once per `duration` tick (periodic
    /// sampling). Reactor's `Flux.sample`.
    pub fn sample(self, duration: Duration) -> Flux<T>
    where
        T: Clone,
    {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut ticker = tokio::time::interval(duration);
            ticker.tick().await; // consume the immediate first tick
            let mut latest: Option<T> = None;
            let mut pending_err: Option<FireflyError> = None;
            let mut done = false;
            while !done {
                tokio::select! {
                    item = s.next() => {
                        match item {
                            Some(Ok(v)) => latest = Some(v),
                            Some(Err(e)) => { pending_err = Some(e); done = true; }
                            None => {
                                done = true;
                                if let Some(v) = latest.take() { yield v; }
                            }
                        }
                    }
                    _ = ticker.tick() => {
                        if let Some(v) = latest.take() { yield v; }
                    }
                }
            }
            if let Some(e) = pending_err { Err(e)?; }
        })
    }

    /// Emits an item only after `duration` of quiet (no newer item).
    /// Reactor's `Flux.debounce` (a.k.a. `sampleTimeout`).
    pub fn debounce(self, duration: Duration) -> Flux<T>
    where
        T: Clone,
    {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut pending: Option<T> = None;
            loop {
                match pending.take() {
                    None => {
                        match s.next().await {
                            Some(item) => pending = Some(item?),
                            None => break,
                        }
                    }
                    Some(v) => {
                        let next = tokio::select! {
                            biased;
                            next = s.next() => Some(next),
                            _ = tokio::time::sleep(duration) => None,
                        };
                        match next {
                            // The quiet window elapsed: emit the pending value.
                            None => yield v,
                            // A newer item arrived: it supersedes `v`.
                            Some(Some(item)) => pending = Some(item?),
                            // Source completed while waiting: flush `v` and stop.
                            Some(None) => { yield v; break; }
                        }
                    }
                }
            }
        })
    }

    // ----------------------------------------------------------------
    // Error-handling operators
    // ----------------------------------------------------------------

    /// Re-subscribes to the source up to `n` times on error. Reactor's
    /// `Flux.retry(n)`. The `factory` rebuilds the source on each
    /// attempt (Rust streams are single-use, so retry needs a builder).
    ///
    /// Note: items already emitted before the error are re-emitted on
    /// each re-subscription, matching Reactor's `retry` semantics.
    pub fn retry<F>(factory: F, n: usize) -> Flux<T>
    where
        F: Fn() -> Flux<T> + Send + 'static,
    {
        Flux::from_stream(async_stream::try_stream! {
            let mut attempts = 0usize;
            loop {
                let mut stream = factory().into_stream();
                let mut errored = None;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(v) => yield v,
                        Err(e) => { errored = Some(e); break; }
                    }
                }
                match errored {
                    None => return,
                    Some(e) => {
                        if attempts >= n { Err(e)?; }
                        attempts += 1;
                    }
                }
            }
        })
    }

    /// Re-subscribes on error with exponential [`Backoff`] delays.
    /// Reactor's `Flux.retryWhen(Retry.backoff(..))`.
    pub fn retry_backoff<F>(factory: F, backoff: Backoff) -> Flux<T>
    where
        F: Fn() -> Flux<T> + Send + 'static,
    {
        Flux::from_stream(async_stream::try_stream! {
            let mut attempt = 0u32;
            loop {
                let mut stream = factory().into_stream();
                let mut errored = None;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(v) => yield v,
                        Err(e) => { errored = Some(e); break; }
                    }
                }
                match errored {
                    None => return,
                    Some(e) => {
                        if attempt >= backoff.max_retries { Err(e)?; }
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

    /// Fails with a 504 [`FireflyError`] if more than `duration` elapses
    /// *between* items (or before the first). Reactor's `Flux.timeout`.
    pub fn timeout(self, duration: Duration) -> Flux<T> {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            loop {
                match tokio::time::timeout(duration, s.next()).await {
                    Ok(Some(item)) => yield item?,
                    Ok(None) => break,
                    Err(_) => { Err(timeout_error(duration))?; }
                }
            }
        })
    }

    /// Recovers from a terminal error by switching to the `Flux`
    /// produced by `f`. Items emitted before the error are kept.
    /// Reactor's `Flux.onErrorResume`.
    pub fn on_error_resume<F>(self, f: F) -> Flux<T>
    where
        F: FnOnce(FireflyError) -> Flux<T> + Send + 'static,
    {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut f = Some(f);
            loop {
                match s.next().await {
                    Some(Ok(v)) => yield v,
                    Some(Err(e)) => {
                        let f = f.take().expect("on_error_resume handler runs once");
                        let mut fallback = f(e).into_stream();
                        while let Some(item) = fallback.next().await {
                            yield item?;
                        }
                        return;
                    }
                    None => return,
                }
            }
        })
    }

    /// Drops a failing item and continues with the rest, invoking
    /// `handler` for each error. Reactor's `Flux.onErrorContinue`.
    ///
    /// Because an `Err` is terminal in the underlying stream, this only
    /// recovers errors produced by *upstream operators that re-signal*
    /// (e.g. a `map_try`); for a plain source stream that stops on the
    /// first `Err`, downstream items after it are not available. Use it
    /// with operators that surface per-item failures.
    pub fn on_error_continue<F>(self, handler: F) -> Flux<T>
    where
        F: FnMut(FireflyError) + Send + 'static,
    {
        let mut handler = handler;
        let s = self.stream;
        Flux::from_stream(async_stream::stream! {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                match item {
                    Ok(v) => yield Ok(v),
                    Err(e) => handler(e),
                }
            }
        })
    }

    // ----------------------------------------------------------------
    // Side-effect (peek) operators
    // ----------------------------------------------------------------

    /// Runs `f` on each item without changing it. Reactor's
    /// `Flux.doOnNext`.
    pub fn do_on_next<F>(self, f: F) -> Flux<T>
    where
        F: FnMut(&T) + Send + 'static,
    {
        let mut f = f;
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                let v = item?;
                f(&v);
                yield v;
            }
        })
    }

    /// Runs `f` once on normal completion (not on error). Reactor's
    /// `Flux.doOnComplete`.
    pub fn do_on_complete<F>(self, f: F) -> Flux<T>
    where
        F: FnOnce() + Send + 'static,
    {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut f = Some(f);
            while let Some(item) = s.next().await {
                yield item?;
            }
            if let Some(f) = f.take() { f(); }
        })
    }

    /// Runs `f` on a terminal error. Reactor's `Flux.doOnError`.
    pub fn do_on_error<F>(self, f: F) -> Flux<T>
    where
        F: FnOnce(&FireflyError) + Send + 'static,
    {
        let s = self.stream;
        Flux::from_stream(async_stream::try_stream! {
            futures::pin_mut!(s);
            let mut f = Some(f);
            while let Some(item) = s.next().await {
                match item {
                    Ok(v) => yield v,
                    Err(e) => {
                        if let Some(f) = f.take() { f(&e); }
                        Err(e)?;
                    }
                }
            }
        })
    }

    /// Runs `f` on any terminal signal (completion or error). Reactor's
    /// `Flux.doFinally`.
    pub fn do_on_finally<F>(self, f: F) -> Flux<T>
    where
        F: FnOnce() + Send + 'static,
    {
        let s = self.stream;
        Flux::from_stream(async_stream::stream! {
            futures::pin_mut!(s);
            let mut f = Some(f);
            while let Some(item) = s.next().await {
                let is_err = item.is_err();
                yield item;
                if is_err { break; }
            }
            if let Some(f) = f.take() { f(); }
        })
    }

    // ----------------------------------------------------------------
    // Scheduling
    // ----------------------------------------------------------------

    /// Runs the upstream source on `scheduler`, hopping items back
    /// through a channel. Reactor's `Flux.subscribeOn`.
    pub fn subscribe_on(self, scheduler: Scheduler) -> Flux<T> {
        match scheduler {
            Scheduler::Immediate => self,
            _ => self.channel_hop(),
        }
    }

    /// Hops downstream processing onto `scheduler` via a bounded
    /// channel. Reactor's `Flux.publishOn`.
    pub fn publish_on(self, scheduler: Scheduler) -> Flux<T> {
        match scheduler {
            Scheduler::Immediate => self,
            _ => self.channel_hop(),
        }
    }

    /// Internal: drains the source on a spawned task and re-emits via a
    /// bounded channel (the thread-hop / backpressure mechanism).
    fn channel_hop(self) -> Flux<T> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let s = self.stream;
        tokio::spawn(async move {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                if tx.send(item).await.is_err() {
                    break;
                }
            }
        });
        Flux::from_stream(async_stream::try_stream! {
            while let Some(item) = rx.recv().await {
                yield item?;
            }
        })
    }

    // ----------------------------------------------------------------
    // Aggregating terminals (→ Mono)
    // ----------------------------------------------------------------

    /// Collects every item into a `Vec`. Reactor's
    /// `Flux.collectList`.
    pub fn collect_list(self) -> Mono<Vec<T>> {
        let s = self.stream;
        Mono::from_raw(async move {
            futures::pin_mut!(s);
            let mut out = Vec::new();
            while let Some(item) = s.next().await {
                out.push(item?);
            }
            Ok(Some(out))
        })
    }

    /// Collects items into a `HashMap` keyed by `key_fn`. Later items
    /// with a duplicate key overwrite earlier ones. Reactor's
    /// `Flux.collectMap`.
    pub fn collect_map<K, F>(self, key_fn: F) -> Mono<HashMap<K, T>>
    where
        K: Eq + Hash + Send + 'static,
        F: FnMut(&T) -> K + Send + 'static,
    {
        let mut key_fn = key_fn;
        let s = self.stream;
        Mono::from_raw(async move {
            futures::pin_mut!(s);
            let mut out: HashMap<K, T> = HashMap::new();
            while let Some(item) = s.next().await {
                let v = item?;
                let k = key_fn(&v);
                out.insert(k, v);
            }
            Ok(Some(out))
        })
    }

    /// Folds items into an accumulator from `seed`. Reactor's
    /// `Flux.reduce(seed, fn)`. Always emits the (possibly untouched)
    /// accumulator.
    pub fn reduce<A, F>(self, seed: A, f: F) -> Mono<A>
    where
        A: Send + 'static,
        F: FnMut(A, T) -> A + Send + 'static,
    {
        let mut f = f;
        let s = self.stream;
        Mono::from_raw(async move {
            futures::pin_mut!(s);
            let mut acc = seed;
            while let Some(item) = s.next().await {
                acc = f(acc, item?);
            }
            Ok(Some(acc))
        })
    }

    /// Counts the items. Reactor's `Flux.count`.
    pub fn count(self) -> Mono<u64> {
        self.reduce(0u64, |acc, _| acc + 1)
    }

    /// `true` if every item satisfies `predicate` (short-circuits on the
    /// first false). Reactor's `Flux.all`.
    pub fn all<F>(self, predicate: F) -> Mono<bool>
    where
        F: FnMut(&T) -> bool + Send + 'static,
    {
        let mut predicate = predicate;
        let s = self.stream;
        Mono::from_raw(async move {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                if !predicate(&item?) {
                    return Ok(Some(false));
                }
            }
            Ok(Some(true))
        })
    }

    /// `true` if any item satisfies `predicate` (short-circuits on the
    /// first true). Reactor's `Flux.any`.
    pub fn any<F>(self, predicate: F) -> Mono<bool>
    where
        F: FnMut(&T) -> bool + Send + 'static,
    {
        let mut predicate = predicate;
        let s = self.stream;
        Mono::from_raw(async move {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                if predicate(&item?) {
                    return Ok(Some(true));
                }
            }
            Ok(Some(false))
        })
    }

    /// Discards items and completes when the source completes. Reactor's
    /// `Flux.then`.
    pub fn then(self) -> Mono<()> {
        let s = self.stream;
        Mono::from_raw(async move {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                item?;
            }
            Ok(Some(()))
        })
    }

    /// The first item, or empty if the source emits nothing. Reactor's
    /// `Flux.next`.
    pub fn next(self) -> Mono<T> {
        let s = self.stream;
        Mono::from_raw(async move {
            futures::pin_mut!(s);
            match s.next().await {
                Some(item) => Ok(Some(item?)),
                None => Ok(None),
            }
        })
    }

    /// The last item, or empty if the source emits nothing. Reactor's
    /// `Flux.last`.
    pub fn last(self) -> Mono<T> {
        let s = self.stream;
        Mono::from_raw(async move {
            futures::pin_mut!(s);
            let mut last = None;
            while let Some(item) = s.next().await {
                last = Some(item?);
            }
            Ok(last)
        })
    }

    /// The single item, erroring if the source emits zero or more than
    /// one. Reactor's `Flux.single`.
    pub fn single(self) -> Mono<T> {
        let s = self.stream;
        Mono::from_raw(async move {
            futures::pin_mut!(s);
            let first = match s.next().await {
                Some(item) => item?,
                None => {
                    return Err(FireflyError::new(
                        "REACTIVE_NO_ELEMENT",
                        "No Such Element",
                        500,
                        "single() expected exactly one element but the source was empty",
                    ))
                }
            };
            if s.next().await.is_some() {
                return Err(FireflyError::new(
                    "REACTIVE_TOO_MANY_ELEMENTS",
                    "Too Many Elements",
                    500,
                    "single() expected exactly one element but the source emitted more",
                ));
            }
            Ok(Some(first))
        })
    }

    /// The item at 0-based position `index`, or empty if out of range.
    /// Reactor's `Flux.elementAt`.
    pub fn element_at(self, index: usize) -> Mono<T> {
        let s = self.stream;
        Mono::from_raw(async move {
            futures::pin_mut!(s);
            let mut i = 0usize;
            while let Some(item) = s.next().await {
                let v = item?;
                if i == index {
                    return Ok(Some(v));
                }
                i += 1;
            }
            Ok(None)
        })
    }

    // ----------------------------------------------------------------
    // Terminals / conversions
    // ----------------------------------------------------------------

    /// Consumes the `Flux`, yielding the boxed underlying stream. The
    /// escape hatch *out* to a raw [`Stream`]. Reactor's
    /// `Flux.toStream` (conceptually).
    pub fn into_stream(self) -> FluxStream<T> {
        self.stream
    }

    /// Alias for [`into_stream`](Flux::into_stream): exposes the raw
    /// `Stream<Item = Result<T, FireflyError>>`.
    pub fn to_stream(self) -> FluxStream<T> {
        self.stream
    }

    /// Subscribes with per-item / completion / error callbacks, draining
    /// the pipeline on a spawned task. Reactor's
    /// `Flux.subscribe(consumer, errorConsumer, completeConsumer)`.
    pub fn subscribe<N, E, C>(self, mut on_next: N, on_error: E, on_complete: C)
    where
        N: FnMut(T) + Send + 'static,
        E: FnOnce(FireflyError) + Send + 'static,
        C: FnOnce() + Send + 'static,
    {
        let s = self.stream;
        tokio::spawn(async move {
            futures::pin_mut!(s);
            while let Some(item) = s.next().await {
                match item {
                    Ok(v) => on_next(v),
                    Err(e) => {
                        on_error(e);
                        return;
                    }
                }
            }
            on_complete();
        });
    }
}

/// A two-way tag used by [`Flux::combine_latest`] to merge two typed
/// source streams onto one channel.
enum Either<L, R> {
    Left(L),
    Right(R),
}

// --------------------------------------------------------------------
// Free-function factories
// --------------------------------------------------------------------

impl Flux<i64> {
    /// A `Flux` emitting `count` consecutive integers starting at
    /// `start`. Reactor's `Flux.range`.
    ///
    /// ```
    /// # use firefly_reactive::Flux;
    /// # async fn ex() {
    /// let out = Flux::range(2, 3).collect_list().block().await.unwrap();
    /// assert_eq!(out, Some(vec![2, 3, 4]));
    /// # }
    /// ```
    pub fn range(start: i64, count: i64) -> Flux<i64> {
        let end = start.saturating_add(count.max(0));
        Flux::from_iter(start..end)
    }
}

impl Flux<u64> {
    /// A `Flux` emitting an incrementing counter (`0, 1, 2, …`) every
    /// `period`. Reactor's `Flux.interval`. Infinite — pair with
    /// [`take`](Flux::take).
    pub fn interval(period: Duration) -> Flux<u64> {
        Flux::from_stream(async_stream::try_stream! {
            let mut ticker = tokio::time::interval(period);
            let mut n = 0u64;
            loop {
                ticker.tick().await;
                yield n;
                n += 1;
            }
        })
    }
}

impl<T> Flux<T>
where
    T: Send + 'static,
{
    /// Unfolds a stream from a `seed` and a step function returning the
    /// next `(value, next_seed)` or `None` to complete. Reactor's
    /// `Flux.generate`.
    pub fn generate<S, F>(seed: S, mut step: F) -> Flux<T>
    where
        S: Send + 'static,
        F: FnMut(S) -> Option<(T, S)> + Send + 'static,
    {
        Flux::from_stream(async_stream::try_stream! {
            let mut state = seed;
            while let Some((value, next)) = step(state) {
                yield value;
                state = next;
            }
        })
    }
}

/// Merges several fluxes, interleaving items as they arrive. Reactor's
/// `Flux.merge`.
pub fn merge<T>(fluxes: Vec<Flux<T>>) -> Flux<T>
where
    T: Send + 'static,
{
    let streams = fluxes.into_iter().map(Flux::into_stream);
    Flux::from_stream(futures::stream::select_all(streams))
}

/// Concatenates several fluxes end-to-end, in order. Reactor's
/// `Flux.concat`.
pub fn concat<T>(fluxes: Vec<Flux<T>>) -> Flux<T>
where
    T: Send + 'static,
{
    Flux::from_stream(async_stream::try_stream! {
        for flux in fluxes {
            let mut s = flux.into_stream();
            while let Some(item) = s.next().await {
                yield item?;
            }
        }
    })
}

/// Zips two fluxes positionally into tuples. Free-function form of
/// [`Flux::zip_with`]; Reactor's `Flux.zip`.
pub fn zip<A, B>(a: Flux<A>, b: Flux<B>) -> Flux<(A, B)>
where
    A: Send + 'static,
    B: Send + 'static,
{
    a.zip_with(b)
}

/// Combines two fluxes by latest value. Free-function form of
/// [`Flux::combine_latest`]; Reactor's `Flux.combineLatest`.
pub fn combine_latest<A, B>(a: Flux<A>, b: Flux<B>) -> Flux<(A, B)>
where
    A: Clone + Send + 'static,
    B: Clone + Send + 'static,
{
    a.combine_latest(b)
}

#[cfg(test)]
#[path = "flux_tests.rs"]
mod tests;
