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

//! DI test slices over [`firefly_container::Container`].
//!
//! The Rust analog of pyfly's `slice_context` / `web_slice` / `data_slice` plus
//! `mock_bean`: build a minimal [`Container`] holding only the beans under test
//! (plus pre-built *overrides* — the mock-bean analog), then **resolve them
//! eagerly** so a missing collaborator fails at *build* time (matching Spring's
//! slice startup) rather than silently on first use.
//!
//! Rust has no package scanning or reflective autowiring, so a slice is an
//! explicit builder: you list the factories and overrides; the eager resolve is
//! the fail-fast gate.
//!
//! Available only with the `container` feature.
//!
//! ```
//! # #[cfg(feature = "container")] {
//! use firefly_testkit::Slice;
//! use firefly_container::Scope;
//! use std::sync::atomic::{AtomicUsize, Ordering};
//!
//! // A collaborator port and a fake (the `mock_bean` analog).
//! trait Repo: Send + Sync { fn count(&self) -> usize; }
//! #[derive(Default)]
//! struct FakeRepo { calls: AtomicUsize }
//! impl Repo for FakeRepo {
//!     fn count(&self) -> usize { self.calls.fetch_add(1, Ordering::SeqCst) }
//! }
//!
//! struct Service { repo: std::sync::Arc<dyn Repo> }
//!
//! let slice = Slice::new()
//!     // install the fake as the `dyn Repo` implementation
//!     .instance(FakeRepo::default())
//!     .bind::<dyn Repo, FakeRepo>(|a| a)
//!     // the service under test, wiring its dependency from the container
//!     .register::<Service, _>(Scope::Singleton, |c| {
//!         Ok(Service { repo: c.resolve::<dyn Repo>()? })
//!     })
//!     .eager::<Service>()  // resolve eagerly -> fail fast if `Repo` is missing
//!     .build();
//!
//! // Retrieve the shared fake to assert against it (interior mutability).
//! let svc = slice.get::<Service>();
//! assert_eq!(svc.repo.count(), 0);
//! # }
//! ```

use firefly_container::{Container, ContainerError, Scope, ScopeSpec};
use std::sync::Arc;

/// Eager-resolution thunk: resolves a registered type, discarding the result,
/// surfacing any [`ContainerError`] (the fail-fast gate).
type EagerResolve = Box<dyn FnOnce(&Container) -> Result<(), ContainerError>>;

/// A builder for a minimal DI [`Container`] holding only the beans under test.
///
/// Mirrors pyfly's `slice_context`: register a subset of beans plus pre-built
/// *overrides*, then [`build`](Slice::build) resolves the eager-marked beans so
/// a missing collaborator fails at build time.
#[derive(Default)]
pub struct Slice {
    container: Container,
    eager: Vec<EagerResolve>,
}

impl Slice {
    /// Start an empty slice.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a bean `T` built by `factory` under `scope`.
    ///
    /// The factory resolves its own dependencies via [`Container::resolve`],
    /// exactly like `firefly-container`. Mirrors registering a bean type in a
    /// pyfly slice. Chainable.
    #[must_use]
    pub fn register<T, F>(self, scope: Scope, factory: F) -> Self
    where
        T: Send + Sync + 'static,
        F: Fn(&Container) -> Result<T, ContainerError> + Send + Sync + 'static,
    {
        self.container.register_factory::<T, F>(scope, factory);
        self
    }

    /// Register a named bean `T` built by `factory` under `scope`.
    ///
    /// Resolvable later via [`BuiltSlice::get_named`]. Chainable.
    #[must_use]
    pub fn register_named<T, F>(self, scope: Scope, name: &str, factory: F) -> Self
    where
        T: Send + Sync + 'static,
        F: Fn(&Container) -> Result<T, ContainerError> + Send + Sync + 'static,
    {
        self.container
            .register_factory_named::<T, F>(scope, name, factory);
        self
    }

    /// Install a pre-built `instance` as a singleton — the **override / mock**
    /// analog of pyfly's `mock_bean`.
    ///
    /// The container owns the resulting `Arc<T>`; retrieve the very same handle
    /// after [`build`](Slice::build) with [`BuiltSlice::get`] to configure (via
    /// interior mutability) and assert against it. Chainable.
    #[must_use]
    pub fn instance<T>(self, instance: T) -> Self
    where
        T: Send + Sync + 'static,
    {
        self.container.register_instance(instance);
        self
    }

    /// Install a pre-built `instance` under an explicit bean `name`. Chainable.
    #[must_use]
    pub fn instance_named<T>(self, instance: T, name: &str) -> Self
    where
        T: Send + Sync + 'static,
    {
        self.container.register_instance_named(instance, name);
        self
    }

    /// Bind an interface (trait object) `I` to an already-registered concrete
    /// implementation `T`.
    ///
    /// `coerce` upcasts `Arc<T>` to `Arc<I>` (typically `|a| a`). The standard
    /// way to override a port with a fake: `instance(Fake)` then
    /// `bind::<dyn Port, Fake>(|a| a)`. Chainable.
    ///
    /// # Panics
    /// Panics if `T` is not yet registered (call [`instance`](Slice::instance)
    /// or [`register`](Slice::register) for `T` first) — surfacing the wiring
    /// mistake immediately.
    #[must_use]
    pub fn bind<I, T>(self, coerce: impl Fn(Arc<T>) -> Arc<I> + Send + Sync + 'static) -> Self
    where
        I: ?Sized + Send + Sync + 'static,
        T: Send + Sync + 'static,
    {
        self.container.bind::<I, T>(coerce);
        self
    }

    /// Mark `T` to be resolved eagerly at [`build`](Slice::build).
    ///
    /// This is the fail-fast gate: if `T` (or any transitive dependency) cannot
    /// be resolved, `build` returns an `Err` instead of letting the failure
    /// surface lazily on first use — mirroring Spring/pyfly slice startup.
    /// Chainable.
    #[must_use]
    pub fn eager<T>(mut self) -> Self
    where
        T: ?Sized + Send + Sync + 'static,
    {
        self.eager
            .push(Box::new(|c: &Container| c.resolve::<T>().map(|_| ())));
        self
    }

    /// Direct access to the underlying [`Container`] for registrations the
    /// fluent surface doesn't cover (custom scopes, ordered/primary beans).
    #[must_use]
    pub fn container(&self) -> &Container {
        &self.container
    }

    /// Finish the slice: eagerly resolve every [`eager`](Slice::eager)-marked
    /// bean, returning the started [`BuiltSlice`].
    ///
    /// # Errors
    /// Returns the first [`ContainerError`] from eager resolution (e.g. a
    /// missing collaborator), so a misconfigured slice fails loudly at build.
    pub fn try_build(self) -> Result<BuiltSlice, ContainerError> {
        let container = Arc::new(self.container);
        for resolve in self.eager {
            resolve(&container)?;
        }
        Ok(BuiltSlice { container })
    }

    /// Finish the slice, panicking on an eager-resolution failure.
    ///
    /// The convenience entry point for tests — a missing collaborator fails the
    /// enclosing `#[test]` with the container's diagnostic (which includes fuzzy
    /// suggestions). Use [`try_build`](Slice::try_build) to handle the error.
    ///
    /// # Panics
    /// Panics if any eager bean cannot be resolved.
    #[must_use]
    pub fn build(self) -> BuiltSlice {
        match self.try_build() {
            Ok(built) => built,
            Err(err) => panic!("Slice::build: eager resolution failed: {err}"),
        }
    }

    /// The async sibling of [`try_build`](Slice::try_build): also **awaits every
    /// `async fn` `#[bean]`** on the slice's container (via
    /// [`Container::init_async_beans`]) before eager resolution. Use it when a
    /// slice under test wires an async bean (a DB pool, broker dial, …); the
    /// synchronous [`try_build`](Slice::try_build) cannot await them.
    ///
    /// # Errors
    /// Returns the first [`ContainerError`] from async-bean construction or
    /// eager resolution.
    pub async fn try_build_async(self) -> Result<BuiltSlice, ContainerError> {
        let container = Arc::new(self.container);
        container.init_async_beans().await?;
        for resolve in self.eager {
            resolve(&container)?;
        }
        Ok(BuiltSlice { container })
    }

    /// The async sibling of [`build`](Slice::build) (awaits async beans).
    ///
    /// # Panics
    /// Panics if async-bean construction or eager resolution fails.
    pub async fn build_async(self) -> BuiltSlice {
        match self.try_build_async().await {
            Ok(built) => built,
            Err(err) => panic!("Slice::build_async: failed: {err}"),
        }
    }
}

impl std::fmt::Debug for Slice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slice")
            .field("registered_types", &self.container.registered_types())
            .field("eager_count", &self.eager.len())
            .finish()
    }
}

/// A built, started DI slice wrapping an `Arc<Container>`.
///
/// The Rust analog of the started `ApplicationContext` a pyfly slice yields.
#[derive(Clone)]
pub struct BuiltSlice {
    container: Arc<Container>,
}

impl BuiltSlice {
    /// Resolve a single `T` (sized or trait object).
    ///
    /// # Panics
    /// Panics if `T` cannot be resolved — for the fallible form, use
    /// [`container`](BuiltSlice::container) and call
    /// [`Container::resolve`] directly.
    #[must_use]
    pub fn get<T>(&self) -> Arc<T>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        self.container
            .resolve::<T>()
            .unwrap_or_else(|err| panic!("BuiltSlice::get: {err}"))
    }

    /// Resolve a bean by its registered `name`.
    ///
    /// # Panics
    /// Panics if no such named bean of type `T` exists.
    #[must_use]
    pub fn get_named<T>(&self, name: &str) -> Arc<T>
    where
        T: Send + Sync + 'static,
    {
        self.container
            .resolve_named::<T>(name)
            .unwrap_or_else(|err| panic!("BuiltSlice::get_named: {err}"))
    }

    /// The shared `Arc<Container>` backing this slice.
    #[must_use]
    pub fn container(&self) -> Arc<Container> {
        Arc::clone(&self.container)
    }
}

impl std::fmt::Debug for BuiltSlice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BuiltSlice")
            .field("registered_types", &self.container.registered_types())
            .finish()
    }
}

// `ScopeSpec` is re-exported so callers using the fluent `register_scoped`
// surface don't need a separate `firefly-container` import. It is used by the
// `register_scoped` convenience below.
impl Slice {
    /// Register a bean `T` under a *custom* scope (by [`ScopeSpec`]).
    ///
    /// For the common built-in scopes prefer [`register`](Slice::register);
    /// this exists for slices that exercise a custom-scope handler. Chainable.
    #[must_use]
    pub fn register_scoped<T, F>(self, scope: impl Into<ScopeSpec>, name: &str, factory: F) -> Self
    where
        T: Send + Sync + 'static,
        F: Fn(&Container) -> Result<T, ContainerError> + Send + Sync + 'static,
    {
        self.container
            .register_factory_scoped::<T, F>(scope, name, factory);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // A port + a fake implementation (the `mock_bean` analog) with interior
    // mutability so the test can configure/observe it after build.
    trait Repo: Send + Sync {
        fn next_id(&self) -> usize;
        fn calls(&self) -> usize;
    }

    #[derive(Default)]
    struct FakeRepo {
        calls: AtomicUsize,
    }
    impl Repo for FakeRepo {
        fn next_id(&self) -> usize {
            self.calls.fetch_add(1, Ordering::SeqCst) + 1
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    struct Service {
        repo: Arc<dyn Repo>,
    }
    impl Service {
        fn create(&self) -> usize {
            self.repo.next_id()
        }
    }

    fn wired_slice() -> BuiltSlice {
        Slice::new()
            .instance(FakeRepo::default())
            .bind::<dyn Repo, FakeRepo>(|a| a)
            .register::<Service, _>(Scope::Singleton, |c| {
                Ok(Service {
                    repo: c.resolve::<dyn Repo>()?,
                })
            })
            .eager::<Service>()
            .build()
    }

    #[test]
    fn resolves_subset_with_override_and_wires_dependency() {
        let slice = wired_slice();
        let svc = slice.get::<Service>();
        assert_eq!(svc.create(), 1);
        assert_eq!(svc.create(), 2);
    }

    #[test]
    fn override_handle_is_shared_with_the_container() {
        let slice = wired_slice();
        let svc = slice.get::<Service>();
        let _ = svc.create();
        let _ = svc.create();
        // The fake retrieved from the container is the SAME instance the service
        // wired in — observe its recorded calls (the mock-assertion analog).
        let fake = slice.get::<FakeRepo>();
        assert_eq!(fake.calls(), 2);
    }

    #[test]
    fn build_fails_fast_when_a_collaborator_is_missing() {
        // `Service` depends on `dyn Repo`, which we never register/bind here.
        let result = Slice::new()
            .register::<Service, _>(Scope::Singleton, |c| {
                Ok(Service {
                    repo: c.resolve::<dyn Repo>()?,
                })
            })
            .eager::<Service>()
            .try_build();
        assert!(result.is_err(), "missing collaborator must fail the build");
    }

    #[test]
    #[should_panic(expected = "eager resolution failed")]
    fn build_panics_on_missing_collaborator() {
        let _ = Slice::new()
            .register::<Service, _>(Scope::Singleton, |c| {
                Ok(Service {
                    repo: c.resolve::<dyn Repo>()?,
                })
            })
            .eager::<Service>()
            .build();
    }

    #[test]
    fn no_eager_marks_means_lazy_resolution_does_not_fail_build() {
        // Without `.eager`, a missing collaborator only surfaces on first use,
        // mirroring lazy resolution. The build itself succeeds.
        let slice = Slice::new()
            .register::<Service, _>(Scope::Singleton, |c| {
                Ok(Service {
                    repo: c.resolve::<dyn Repo>()?,
                })
            })
            .build();
        let result = slice.container().resolve::<Service>();
        assert!(result.is_err());
    }

    #[test]
    fn named_beans_resolve_by_name() {
        let slice = Slice::new()
            .instance_named(FakeRepo::default(), "primary")
            .register_named::<u32, _>(Scope::Singleton, "answer", |_| Ok(42))
            .build();
        assert_eq!(slice.get_named::<FakeRepo>("primary").calls(), 0);
        assert_eq!(*slice.get_named::<u32>("answer"), 42);
    }

    #[test]
    fn container_accessor_exposes_registered_types() {
        let slice = wired_slice();
        let types = slice.container().registered_types();
        assert!(types.iter().any(|t| t.contains("FakeRepo")));
        assert!(types.iter().any(|t| t.contains("Service")));
    }

    #[test]
    fn slice_and_built_slice_are_debug() {
        let slice = Slice::new().instance(FakeRepo::default());
        let dbg = format!("{slice:?}");
        assert!(dbg.contains("Slice"));
        let built = slice.build();
        assert!(format!("{built:?}").contains("BuiltSlice"));
    }

    #[test]
    fn built_slice_is_clone_and_shares_container() {
        let slice = wired_slice();
        let cloned = slice.clone();
        let _ = slice.get::<Service>().create();
        // Both handles see the same container -> same shared fake state.
        assert_eq!(cloned.get::<FakeRepo>().calls(), 1);
    }
}
