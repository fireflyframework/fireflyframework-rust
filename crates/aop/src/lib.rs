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

//! firefly-aop — Spring-style aspect-oriented advice for the Firefly Framework.
//!
//! This crate is the Rust port of pyfly's `pyfly.aop` package: a pointcut glob
//! language, a [`JoinPoint`], five kinds of advice (`before`,
//! `after_returning`, `after_throwing`, `after`, `around`), an ordered
//! [`AspectRegistry`], and a chain executor ([`intercept`]) that runs matching
//! advice around an arbitrary service-method call in the exact pyfly ordering.
//!
//! # Pointcut language
//!
//! Patterns match dot-segmented *qualified names* of the form
//! `stereotype.ClassName.method` (e.g. `service.OrderService.create`):
//!
//! * `*` matches exactly one segment (never crosses a dot);
//! * `**` matches one or more segments (any depth);
//! * partial globs inside a segment use fnmatch rules (`get_*`, `*Service`).
//!
//! [`matches_pointcut`] is the one-shot matcher; [`Pointcut`] is the compiled,
//! reusable form the registry stores per binding. Both port pyfly's
//! `_segment_to_regex` / `_pattern_to_regex` byte-for-byte.
//!
//! # Aspects, registry, and interception
//!
//! An [`Aspect`] is a trait with a default no-op for each advice hook plus an
//! `around` hook that receives a [`Proceed`] continuation. Register an aspect
//! against a pointcut with an explicit order:
//!
//! ```
//! use std::sync::{Arc, Mutex};
//! use async_trait::async_trait;
//! use firefly_aop::{
//!     intercept, invocation, ok, AnyArc, Aspect, AspectRegistry, JoinPoint,
//! };
//!
//! struct Audit(Arc<Mutex<Vec<String>>>);
//!
//! #[async_trait]
//! impl Aspect for Audit {
//!     async fn before(&self, jp: &JoinPoint) {
//!         self.0.lock().unwrap().push(format!("calling {}", jp.qualified_name()));
//!     }
//! }
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() {
//! let log = Arc::new(Mutex::new(Vec::new()));
//! let mut registry = AspectRegistry::new();
//! registry.register(Arc::new(Audit(log.clone())), "service.*.*", 0);
//!
//! // Explicit weaving: wrap the real call in an `invocation` and route it
//! // through `intercept` at the call site.
//! let out = intercept(
//!     &registry,
//!     "service.OrderService",
//!     "create",
//!     Arc::new((42u32,)),
//!     invocation(|| async { ok("order-42".to_string()) }),
//! )
//! .await
//! .unwrap();
//!
//! assert_eq!(out.downcast_ref::<String>().unwrap(), "order-42");
//! assert_eq!(*log.lock().unwrap(), vec!["calling service.OrderService.create"]);
//! # }
//! ```
//!
//! # Weaving has no monkey-patch analogue in Rust
//!
//! pyfly's `weave_bean` mutates a live bean by `setattr`-replacing matching
//! public methods, driven by a `BeanPostProcessor` over the DI container, and
//! skips `@property` descriptors via `getattr_static`. **None of that exists in
//! Rust**: there is no runtime method mutation, no descriptor protocol, and no
//! bean container to post-process. The Rust port therefore makes weaving
//! **explicit** — each interception site wraps the original call in an
//! [`Invocation`] and routes it through [`intercept`]. "Non-matching methods
//! untouched" falls out for free: if no binding matches the qualified name,
//! [`intercept`] runs the invocation with no advice overhead.
//!
//! For HTTP-edge and bus-dispatch cross-cutting concerns, keep using
//! `firefly-web`'s tower layers and `firefly-cqrs`'s `Middleware`; `firefly-aop`
//! targets pattern-matched advice over arbitrary service methods.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod aspect;
mod global;
mod intercept;
mod join_point;
mod pointcut;
mod registry;

pub use aspect::{Aspect, NoopAspect};
pub use global::{
    advised, matching_bindings, register_aspect, register_discovered_aspects, AspectRegistration,
};
pub use intercept::{intercept, intercept_with_bindings, ok};
pub use join_point::{
    invocation, AdviceError, AdviceFuture, AdviceKind, AdviceResult, AnyArc, Invocation, JoinPoint,
    Proceed,
};
pub use pointcut::{matches_pointcut, Pointcut};
pub use registry::{AdviceBinding, AspectRegistry};

/// Re-export of [`async_trait`] so the declarative `#[aspect]` macro can emit the
/// `#[async_trait]` attribute on the `impl Aspect` it generates in the user's
/// crate (`#[firefly_aop::async_trait]`) without that crate depending on
/// `async-trait` directly.
pub use async_trait::async_trait;

/// Re-export of [`inventory`] so the declarative `#[aspect]` macro's generated
/// `inventory::submit!` thunks resolve through `firefly_aop::inventory` without
/// the user's crate depending on `inventory` directly — mirroring
/// `firefly_transactional`'s re-export for event listeners.
pub use inventory;

/// Framework version stamp.
pub const VERSION: &str = "26.6.8";
