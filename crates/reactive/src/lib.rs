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

//! # firefly-reactive
//!
//! A faithful, production-grade **Reactor / WebFlux-style reactive
//! core** for the Firefly Framework for Rust. It provides the two
//! publisher types every reactive Firefly integration builds on —
//! [`Mono<T>`] (0-or-1 + error) and [`Flux<T>`] (0..N + terminal error)
//! — plus a [`Scheduler`], a [`FluxSink`] for programmatic emission, and
//! a [`Backoff`] retry policy. It is the Rust analog of Project
//! Reactor's `Mono` / `Flux`, the engine behind Spring WebFlux and the
//! Java Firefly framework.
//!
//! The error type is fixed to [`firefly_kernel::FireflyError`], exactly
//! as WebFlux models everything as a `Throwable`. Fixing the error keeps
//! the operator surface ergonomic (no error type parameter) and wires
//! straight into the framework's RFC 7807 problem responses.
//!
//! Everything is `Send + 'static`, so a `Mono` or `Flux` drops directly
//! into an axum handler — `firefly-web` builds its streaming
//! (`application/x-ndjson` + SSE) responses on top of these types.
//!
//! ## Quick start
//!
//! ```
//! use firefly_reactive::{Flux, Mono};
//!
//! # async fn ex() {
//! // Mono: one value, lazily transformed, then awaited.
//! let n = Mono::just(20)
//!     .map(|x| x + 1)
//!     .filter(|x| *x > 10)
//!     .default_if_empty(0)
//!     .block()
//!     .await
//!     .unwrap();
//! assert_eq!(n, Some(21));
//!
//! // Flux: a stream of values, filtered + mapped, collected to a Vec.
//! // `collect_list` always yields a (possibly empty) list, so the
//! // `Mono`'s `Ok(Some(..))` is unwrapped twice.
//! let xs = Flux::range(1, 5)
//!     .filter(|x| x % 2 == 1)
//!     .map(|x| x * 10)
//!     .collect_list()
//!     .block()
//!     .await
//!     .unwrap()
//!     .unwrap();
//! assert_eq!(xs, vec![10, 30, 50]);
//! # }
//! ```
//!
//! ## Reactor → firefly-reactive concept map
//!
//! | Project Reactor                       | firefly-reactive                          |
//! |---------------------------------------|-------------------------------------------|
//! | `Mono<T>`                             | [`Mono<T>`]                               |
//! | `Flux<T>`                             | [`Flux<T>`]                               |
//! | `Throwable` (error signal)            | [`firefly_kernel::FireflyError`] (fixed)  |
//! | `Mono.empty()` / `onComplete`         | `Ok(None)` from a [`Mono`]                |
//! | `onError(t)`                          | `Err(FireflyError)` (terminal)            |
//! | `Mono.block()`                        | [`Mono::block`] (async, never parks a thread) |
//! | `Flux.subscribe(..)`                  | [`Flux::subscribe`]                       |
//! | `Schedulers.immediate()`              | [`Scheduler::Immediate`]                  |
//! | `Schedulers.parallel()`               | [`Scheduler::Parallel`]                   |
//! | `Schedulers.boundedElastic()`         | [`Scheduler::BoundedElastic`]             |
//! | `FluxSink` / `Flux.create`            | [`FluxSink`] / [`Flux::create`]           |
//! | `Retry.backoff(..)`                   | [`Backoff`] + `*::retry_backoff`          |
//! | `Mono.toFuture()`                     | [`Mono::into_future`] / `await`           |
//! | `Flux.toStream()` (escape hatch)      | [`Flux::to_stream`] / [`Flux::into_stream`] |
//!
//! ## Error semantics
//!
//! An `Err` item is **terminal** in a [`Flux`]: every operator
//! short-circuits on the first error and propagates it downstream — there
//! is no per-element error channel. To recover, use
//! [`Flux::on_error_resume`] (switch to a fallback stream) or
//! [`Flux::on_error_continue`] (drop the failing element and keep the
//! rest, for operators that re-signal per item). `retry` / `retry_backoff`
//! re-subscribe to a *factory*, since Rust streams and futures are
//! single-use. [`Mono::timeout`] / [`Flux::timeout`] map a missed
//! deadline to a 504 [`FireflyError`].
//!
//! ## Modules
//!
//! - [`mono`] — the [`Mono`] type and its operators/factories.
//! - [`flux`] — the [`Flux`] type and its operators/factories.
//! - [`scheduler`] — the [`Scheduler`] execution contexts.
//! - [`sink`] — the [`FluxSink`] push handle for [`Flux::create`].
//! - [`backoff`] — the [`Backoff`] retry policy.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod backoff;
pub mod flux;
pub mod mono;
pub mod scheduler;
pub mod sink;

pub use backoff::Backoff;
pub use flux::{
    combine_latest as flux_combine_latest, concat as flux_concat, merge as flux_merge,
    zip as flux_zip, Flux,
};
pub use mono::{zip as mono_zip, CachedMono, Mono};
pub use scheduler::Scheduler;
pub use sink::FluxSink;

/// The released framework version. Calendar-versioned (`YY.M.PATCH`)
/// expressed as valid semver, matching [`firefly_kernel::VERSION`].
pub const VERSION: &str = firefly_kernel::VERSION;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_kernel() {
        assert_eq!(VERSION, firefly_kernel::VERSION);
    }

    #[tokio::test]
    async fn end_to_end_mono_flux_bridge() {
        // A Mono producing a value, fanning into a Flux, back to a Mono.
        let total = Mono::just(4)
            .flat_map_many(|n| Flux::range(1, n))
            .reduce(0i64, |acc, x| acc + x)
            .block()
            .await
            .unwrap();
        assert_eq!(total, Some(1 + 2 + 3 + 4));
    }

    #[tokio::test]
    async fn reexports_are_usable() {
        let mut merged = flux_merge(vec![Flux::range(1, 1), Flux::range(9, 1)])
            .collect_list()
            .block()
            .await
            .unwrap()
            .unwrap();
        merged.sort();
        assert_eq!(merged, vec![1, 9]);

        let zipped = mono_zip(Mono::just(1), Mono::just(2))
            .block()
            .await
            .unwrap();
        assert_eq!(zipped, Some((1, 2)));
    }
}
