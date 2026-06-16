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

//! firefly-container — opt-in TypeId-keyed DI container.
//!
//! A Spring-style dependency-injection container ported from pyfly's
//! `container` package. It mirrors the *service-locator* half of pyfly's
//! surface faithfully — registrations, scopes, named beans, primary/order
//! resolution, trait-object bindings, deferred [`Provider<T>`], custom-scope
//! SPI, refresh scope, per-bean metrics, circular-dependency detection, and a
//! rich error taxonomy with hand-rolled fuzzy suggestions.
//!
//! It deliberately does **not** mirror pyfly's reflective-autowiring half
//! (type-hint-driven constructor injection, `Autowired` field injection,
//! package scanning, stereotype decorators): those rely on Python runtime
//! reflection with no Rust analog. The idiom inverts to **explicit factory
//! closures** — a factory resolves its own dependencies by calling
//! [`Container::resolve`], which is the Rust expression of constructor
//! injection.
//!
//! # Quick start
//!
//! ```
//! use firefly_container::{Container, Scope};
//! use std::sync::Arc;
//!
//! struct Greeter;
//! impl Greeter {
//!     fn greet(&self) -> &'static str { "hello" }
//! }
//!
//! struct UserService { greeter: Arc<Greeter> }
//!
//! let c = Container::new();
//! c.register_factory::<Greeter, _>(Scope::Singleton, |_| Ok(Greeter));
//! c.register_factory::<UserService, _>(Scope::Singleton, |c| {
//!     Ok(UserService { greeter: c.resolve::<Greeter>()? })
//! });
//!
//! let svc = c.resolve::<UserService>().unwrap();
//! assert_eq!(svc.greeter.greet(), "hello");
//! ```
//!
//! # Trait-object bindings
//!
//! `TypeId::of::<dyn Trait>()` is a valid registry key, so an interface can be
//! bound to a concrete implementation and resolved as a trait object:
//!
//! ```
//! use firefly_container::{Container, Scope};
//! use std::sync::Arc;
//!
//! trait Cache: Send + Sync { fn kind(&self) -> &'static str; }
//! struct RedisCache;
//! impl Cache for RedisCache { fn kind(&self) -> &'static str { "redis" } }
//!
//! let c = Container::new();
//! c.register_factory::<RedisCache, _>(Scope::Singleton, |_| Ok(RedisCache));
//! c.bind::<dyn Cache, RedisCache>(|impl_arc| impl_arc);
//! let cache: Arc<dyn Cache> = c.resolve::<dyn Cache>().unwrap();
//! assert_eq!(cache.kind(), "redis");
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod condition;
mod error;
mod provider;
mod registration;
mod scan;
mod scope;
mod value;

use std::any::{type_name, Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock, Weak};
use std::time::Instant;

pub use condition::{Condition, ConditionContext};
pub use error::ContainerError;
pub use provider::Provider;
pub use registration::{
    BeanMetrics, DestroyHook, Factory, Registration, HIGHEST_PRECEDENCE, LOWEST_PRECEDENCE,
};
pub use scan::{
    discovered, routes, schemas, BeanDescriptor, BeanStats, ComponentRegistration, RouteDescriptor,
    SchemaDescriptor, Stereotype as BeanStereotype,
};
pub use scope::{RefreshScope, Scope, ScopeHandler, ScopeSpec, SharedInstance, REFRESH_SCOPE_NAME};
pub use value::resolve_value;

// Re-export `inventory` so `firefly-macros`-generated `inventory::submit!`
// thunks resolve through `firefly_container::inventory` without the user crate
// listing `inventory` directly.
#[doc(hidden)]
pub use inventory;

/// Framework version stamp.
pub const VERSION: &str = "26.6.20";

/// Type-erased boxed `Arc<T>` — a sized fat pointer wrapped in `Box<dyn Any>`
/// so resolution can return `Arc<T>` for both sized and `?Sized` (trait-object)
/// `T`. `Arc<dyn Any>` cannot downcast to an unsized view directly, so each
/// query key carries a [`Caster`] that produces the right `Arc<T>`.
type ErasedArc = Box<dyn Any + Send + Sync>;

/// Converts a registration's stored [`SharedInstance`] into the concrete `Arc<T>`
/// expected under a particular query [`TypeId`], boxed as an [`ErasedArc`].
type Caster = Box<dyn Fn(SharedInstance) -> Option<ErasedArc> + Send + Sync>;

/// A boxed **async bean initializer**. Registered during [`Container::scan`] by
/// an `async fn` `#[bean]` factory and awaited once — as a batch — by
/// [`Container::init_async_beans`] after the synchronous scan completes. It
/// receives the shared container so the factory can resolve its (already
/// scanned, possibly already async-initialized) collaborators, builds the bean
/// asynchronously (a DB pool, an HTTP client, a warmed cache, …), and installs
/// it as a ready singleton. This is the Rust analog of a Spring bean whose
/// `@Bean` factory performs blocking I/O at context-refresh time — here the I/O
/// is `await`ed instead of blocking a thread.
type AsyncInit = Box<
    dyn FnOnce(Arc<Container>) -> Pin<Box<dyn Future<Output = Result<(), ContainerError>> + Send>>
        + Send,
>;

/// One entry under a query type id: a shared registration plus the caster that
/// views its instance as the queried type.
struct TypeEntry {
    registration: Arc<Registration>,
    caster: Caster,
}

thread_local! {
    /// Per-thread in-creation stack for cycle detection — thread-local so
    /// concurrent transient resolution (which does not hold the registry lock)
    /// cannot race on a shared structure or raise spurious cycle errors.
    /// Mirrors pyfly's `Container._resolving`.
    static RESOLVING: RefCell<Vec<(TypeId, String)>> = const { RefCell::new(Vec::new()) };
}

/// A Spring-style dependency-injection container.
///
/// Holds registrations keyed by [`TypeId`] (both concrete types and trait
/// objects), named beans, and custom scope handlers behind [`RwLock`]s so the
/// container is `Send + Sync` and shareable as `Arc<Container>`.
///
/// Ports pyfly's `container.Container`. See the [crate docs](crate) for the
/// adaptation rationale.
#[derive(Default)]
pub struct Container {
    by_type: RwLock<HashMap<TypeId, Vec<TypeEntry>>>,
    by_name: RwLock<HashMap<String, Arc<Registration>>>,
    scopes: RwLock<HashMap<String, Arc<dyn ScopeHandler>>>,
    /// Insertion-ordered registered type names, for fuzzy suggestions.
    type_names: RwLock<Vec<String>>,
    /// Self-keyed registrations in registration order, for introspection
    /// (`beans()`) and reverse-order `#[pre_destroy]` on shutdown.
    registered: RwLock<Vec<Arc<Registration>>>,
    /// The active condition context consulted during [`Container::scan`]
    /// (profiles + config properties). Empty by default.
    conditions: RwLock<ConditionContext>,
    /// A weak self-handle, populated when the container is wrapped in an `Arc`
    /// via [`Container::shared`] / [`Container::provider`]. Lets factory
    /// closures (which receive `&Container`) build a [`Provider<T>`], which
    /// needs an `Arc<Container>`.
    me: OnceLock<Weak<Container>>,
    /// Pending **async bean** initializers (`async fn` `#[bean]` factories),
    /// registered during [`scan`](Container::scan) and awaited as a batch by
    /// [`init_async_beans`](Container::init_async_beans) once the synchronous
    /// scan has registered every bean. Each carries its `#[bean(order = …)]` so
    /// the batch runs in precedence order (lower first), letting one async bean
    /// depend on another initialized earlier.
    ///
    /// A `Mutex` (not an `RwLock`) because an [`AsyncInit`] closure is `Send` but
    /// not `Sync` (it owns the `FnOnce` factory): `Mutex<T>` is `Sync` whenever
    /// `T: Send`, which keeps `Container: Sync`.
    async_inits: std::sync::Mutex<Vec<(i32, AsyncInit)>>,
}

impl Container {
    /// Create an empty container.
    #[must_use]
    pub fn new() -> Self {
        Container::default()
    }

    /// Create an empty container wrapped in an `Arc`, with its self-handle
    /// installed.
    ///
    /// Use this (rather than `Arc::new(Container::new())`) when any bean
    /// autowires a [`Provider<T>`] field: the generated factory builds the
    /// provider from the container's self-handle, which is only set when the
    /// container is created shared (or after a call to
    /// [`provider`](Container::provider)).
    #[must_use]
    pub fn shared() -> Arc<Self> {
        let arc = Arc::new(Container::default());
        let _ = arc.me.set(Arc::downgrade(&arc));
        arc
    }

    /// Install the weak self-handle from an owning `Arc`, enabling
    /// [`Provider<T>`] autowiring on a container that was created with
    /// [`new`](Container::new) and later wrapped in an `Arc`.
    ///
    /// Idempotent and safe to call repeatedly.
    pub fn install_shared_handle(self: &Arc<Self>) {
        let _ = self.me.set(Arc::downgrade(self));
    }

    // ------------------------------------------------------------------
    // Registration
    // ------------------------------------------------------------------

    /// Register `T` under `scope`, built with `factory`.
    ///
    /// `factory` receives the container so it can resolve its own dependencies.
    /// The bean is keyed by `TypeId::of::<T>()` and resolvable with
    /// [`resolve::<T>`](Container::resolve). Mirrors pyfly's `register`.
    pub fn register_factory<T, F>(&self, scope: Scope, factory: F)
    where
        T: Send + Sync + 'static,
        F: Fn(&Container) -> Result<T, ContainerError> + Send + Sync + 'static,
    {
        self.register_factory_named::<T, F>(scope, "", factory);
    }

    /// Register `T` under `scope` with an explicit bean `name`.
    ///
    /// A non-empty `name` makes the bean resolvable with
    /// [`resolve_named`](Container::resolve_named). Mirrors pyfly's
    /// `register(cls, scope=..., name=...)`.
    pub fn register_factory_named<T, F>(&self, scope: Scope, name: &str, factory: F)
    where
        T: Send + Sync + 'static,
        F: Fn(&Container) -> Result<T, ContainerError> + Send + Sync + 'static,
    {
        let erased: Factory = Box::new(move |c| {
            let value = factory(c)?;
            Ok(Arc::new(value) as SharedInstance)
        });
        self.install::<T>(ScopeSpec::Builtin(scope), name, false, 0, erased);
    }

    /// Register `T` with a custom scope name, built with `factory`.
    ///
    /// The custom scope must be registered via
    /// [`register_scope`](Container::register_scope) before the first resolve.
    pub fn register_factory_scoped<T, F>(&self, scope: impl Into<ScopeSpec>, name: &str, factory: F)
    where
        T: Send + Sync + 'static,
        F: Fn(&Container) -> Result<T, ContainerError> + Send + Sync + 'static,
    {
        let erased: Factory = Box::new(move |c| {
            let value = factory(c)?;
            Ok(Arc::new(value) as SharedInstance)
        });
        self.install::<T>(scope.into(), name, false, 0, erased);
    }

    /// Register `T` as a default-constructed singleton.
    ///
    /// Convenience for `register_factory::<T>(Scope::Singleton, |_| Ok(T::default()))`.
    /// Mirrors the common pyfly `register(cls)` of a no-arg type.
    pub fn register<T>(&self)
    where
        T: Default + Send + Sync + 'static,
    {
        self.register_factory::<T, _>(Scope::Singleton, |_| Ok(T::default()));
    }

    /// Register an already-constructed instance as a singleton.
    ///
    /// The supported way to install a pre-built object (Spring's
    /// `registerSingleton`). Mirrors pyfly's `register_instance`.
    pub fn register_instance<T>(&self, instance: T)
    where
        T: Send + Sync + 'static,
    {
        self.register_instance_named(instance, "");
    }

    /// Register an already-constructed instance as a named singleton.
    pub fn register_instance_named<T>(&self, instance: T, name: &str)
    where
        T: Send + Sync + 'static,
    {
        let shared: SharedInstance = Arc::new(instance);
        let cached = shared.clone();
        let erased: Factory = Box::new(move |_| Ok(cached.clone()));
        let reg = self.install::<T>(ScopeSpec::Builtin(Scope::Singleton), name, false, 0, erased);
        *reg.instance.lock().expect("registration mutex poisoned") = Some(shared);
    }

    /// Register an already-shared `Arc<T>` as a singleton, preserving the
    /// *existing* `Arc` so [`resolve::<T>()`](Container::resolve) hands back the
    /// very same instance other parts of the app already hold.
    ///
    /// Unlike [`register_instance`](Container::register_instance), which wraps a
    /// fresh `Arc` around a moved value, this installs the caller's `Arc`
    /// unchanged — the right call when an infrastructure object (a CQRS `Bus`,
    /// an event `Broker`, a shared service handle) is created outside the
    /// container and must be the same instance the container autowires into
    /// beans. Spring's `registerSingleton(name, existingBean)`.
    pub fn register_arc<T>(&self, instance: Arc<T>)
    where
        T: Send + Sync + 'static,
    {
        self.register_arc_named(instance, "");
    }

    /// Register an already-shared `Arc<T>` as a named singleton (see
    /// [`register_arc`](Container::register_arc)).
    pub fn register_arc_named<T>(&self, instance: Arc<T>, name: &str)
    where
        T: Send + Sync + 'static,
    {
        let shared: SharedInstance = instance;
        let cached = shared.clone();
        let erased: Factory = Box::new(move |_| Ok(cached.clone()));
        let reg = self.install::<T>(ScopeSpec::Builtin(Scope::Singleton), name, false, 0, erased);
        *reg.instance.lock().expect("registration mutex poisoned") = Some(shared);
    }

    /// Register an already-shared **trait-object port** `Arc<dyn Trait>` as a
    /// singleton keyed by the trait, so `resolve::<dyn Trait>()` and
    /// `#[autowired] field: Arc<dyn Trait>` hand back this exact instance.
    ///
    /// [`register_arc`](Container::register_arc) needs a `Sized` type; an
    /// infrastructure object that is only available pre-erased (a
    /// `Core`-provided `Arc<dyn Broker>` / `Arc<dyn Adapter>`, a hand-built
    /// `Arc<dyn EventStore>`) cannot go through it. This installs the erased
    /// `Arc<I>` behind a sized holder, with a caster that recovers the original
    /// `Arc<I>` on resolve — the missing primitive that lets the **framework**
    /// register port beans so application `@Bean` factories can autowire them.
    /// Spring's `registerSingleton(name, bean)` for an interface-typed bean.
    pub fn register_port<I>(&self, instance: Arc<I>)
    where
        I: ?Sized + Send + Sync + 'static,
    {
        self.register_port_named(instance, "");
    }

    /// Register an already-shared trait-object port `Arc<dyn Trait>` as a named
    /// singleton (see [`register_port`](Container::register_port)).
    pub fn register_port_named<I>(&self, instance: Arc<I>, name: &str)
    where
        I: ?Sized + Send + Sync + 'static,
    {
        /// A sized wrapper so an `Arc<dyn Trait>` can be stored as a
        /// [`SharedInstance`] (`Arc<dyn Any>`) and recovered by the caster.
        struct PortHolder<I: ?Sized>(Arc<I>);

        let type_id = TypeId::of::<I>();
        let type_name_str = type_name::<I>().to_string();
        let holder: Arc<PortHolder<I>> = Arc::new(PortHolder(instance));
        let shared: SharedInstance = holder;
        let cached = shared.clone();

        let reg = Arc::new(Registration {
            impl_type: type_id,
            type_name: type_name_str.clone(),
            scope: ScopeSpec::Builtin(Scope::Singleton),
            name: name.to_string(),
            primary: false,
            order: 0,
            factory: Box::new(move |_| Ok(cached.clone())),
            instance: std::sync::Mutex::new(Some(shared)),
            metrics: std::sync::Mutex::new(BeanMetrics::default()),
            stereotype: std::sync::Mutex::new(None),
            destroy: std::sync::Mutex::new(None),
            dependencies: std::sync::Mutex::new(Vec::new()),
        });

        // The caster recovers `Arc<I>` from the stored `Arc<PortHolder<I>>`.
        let caster: Caster = Box::new(|shared: SharedInstance| {
            let holder: Arc<PortHolder<I>> = shared.downcast::<PortHolder<I>>().ok()?;
            Some(Box::new(holder.0.clone()) as ErasedArc)
        });

        {
            let mut by_type = self.by_type.write().expect("by_type lock poisoned");
            let entries = by_type.entry(type_id).or_default();
            entries.retain(|e| {
                !(e.registration.impl_type == type_id && e.registration.name == reg.name)
            });
            entries.push(TypeEntry {
                registration: Arc::clone(&reg),
                caster,
            });
        }
        if !name.is_empty() {
            self.by_name
                .write()
                .expect("by_name lock poisoned")
                .insert(name.to_string(), Arc::clone(&reg));
        }
        {
            let mut names = self.type_names.write().expect("type_names lock poisoned");
            if !names.contains(&type_name_str) {
                names.push(type_name_str);
            }
        }
        {
            let mut registered = self.registered.write().expect("registered lock poisoned");
            registered.retain(|r| !(r.impl_type == type_id && r.name == reg.name));
            registered.push(Arc::clone(&reg));
        }
    }

    /// Mark a registration primary and/or set its order, then register.
    ///
    /// A builder-style variant of [`register_factory`](Container::register_factory)
    /// that records the `primary` flag (used by [`resolve`](Container::resolve)
    /// to disambiguate multiple bound implementations) and the `order` (used by
    /// [`resolve_all`](Container::resolve_all)).
    pub fn register_factory_with<T, F>(
        &self,
        scope: Scope,
        name: &str,
        primary: bool,
        order: i32,
        factory: F,
    ) where
        T: Send + Sync + 'static,
        F: Fn(&Container) -> Result<T, ContainerError> + Send + Sync + 'static,
    {
        let erased: Factory = Box::new(move |c| {
            let value = factory(c)?;
            Ok(Arc::new(value) as SharedInstance)
        });
        self.install::<T>(ScopeSpec::Builtin(scope), name, primary, order, erased);
    }

    /// Install an already-constructed `Arc<T>` as a ready singleton under
    /// `TypeId::of::<T>()` and `name`, pre-caching the instance so the first
    /// `resolve` returns it without running a factory.
    ///
    /// Like [`register_arc`](Container::register_arc) but carries the
    /// `scope` / `primary` / `order` flags, so it participates in
    /// multi-candidate disambiguation exactly like a factory bean. Used by
    /// [`register_async_factory`](Container::register_async_factory) to publish
    /// an async bean once its future has resolved.
    pub fn register_singleton_arc<T>(
        &self,
        instance: Arc<T>,
        scope: Scope,
        name: &str,
        primary: bool,
        order: i32,
    ) where
        T: Send + Sync + 'static,
    {
        let shared: SharedInstance = instance;
        let cached = shared.clone();
        let erased: Factory = Box::new(move |_| Ok(cached.clone()));
        let reg = self.install::<T>(ScopeSpec::Builtin(scope), name, primary, order, erased);
        *reg.instance.lock().expect("instance mutex poisoned") = Some(shared);
    }

    /// Register an **async bean** factory (`async fn` `#[bean]`).
    ///
    /// Unlike [`register_factory_with`](Container::register_factory_with), the
    /// factory is a `FnOnce` returning a `Future`: it is not run now but parked
    /// until [`init_async_beans`](Container::init_async_beans) awaits the whole
    /// batch after the synchronous scan, then installs the resolved value as a
    /// ready singleton (recording the given `stereotype` label — `"bean"`, or
    /// `"repository"`/… via `#[bean(stereotype = "…")]` — and the
    /// `dependencies` for the admin graph). This lets a bean perform real
    /// asynchronous construction — opening a connection pool, dialing a broker —
    /// the Spring Boot way, without blocking the scan or a worker thread. A
    /// factory failure is wrapped as [`ContainerError::BeanCreation`] carrying
    /// the bean's name, so startup fails fast with "error creating bean '…'".
    // The parameters mirror a bean's registration metadata (scope / name /
    // primary / order / stereotype / dependencies) plus the factory — splitting
    // them into a struct would only obscure the generated `#[bean]` call site.
    #[allow(clippy::too_many_arguments)]
    pub fn register_async_factory<T, F, Fut>(
        &self,
        scope: Scope,
        name: &str,
        primary: bool,
        order: i32,
        stereotype: &'static str,
        dependencies: &'static [&'static str],
        factory: F,
    ) where
        T: Send + Sync + 'static,
        F: FnOnce(Arc<Container>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, ContainerError>> + Send + 'static,
    {
        let name = name.to_string();
        let init: AsyncInit = Box::new(move |c: Arc<Container>| {
            Box::pin(async move {
                // Wrap the factory's failure with the bean's identity so a
                // missing dependency or async I/O error reads as Spring's
                // "Error creating bean named '…': <cause>".
                let value = factory(Arc::clone(&c))
                    .await
                    .map_err(|e| ContainerError::bean_creation(&name, e.to_string()))?;
                c.register_singleton_arc::<T>(Arc::new(value), scope, &name, primary, order);
                c.set_stereotype::<T>(stereotype);
                c.set_dependencies::<T>(dependencies);
                Ok(())
            })
        });
        self.async_inits
            .lock()
            .expect("async_inits lock poisoned")
            .push((order, init));
    }

    /// Await every registered [async bean](Container::register_async_factory),
    /// in `#[bean(order = …)]` precedence order (lower first), installing each as
    /// a ready singleton.
    ///
    /// Called once by the bootstrap (`FireflyApplication`) immediately after
    /// [`scan`](Container::scan), so async beans are live before controllers,
    /// handlers, and eager singletons resolve them. Idempotent: the pending
    /// list is drained, so a second call is a no-op. Any factory error aborts
    /// the batch and propagates (fail-fast startup).
    pub async fn init_async_beans(self: &Arc<Container>) -> Result<(), ContainerError> {
        let mut inits: Vec<(i32, AsyncInit)> = {
            let mut guard = self.async_inits.lock().expect("async_inits lock poisoned");
            std::mem::take(&mut *guard)
        };
        // Stable sort by precedence so equal-order beans keep registration order.
        inits.sort_by_key(|(order, _)| *order);
        for (_, init) in inits {
            init(Arc::clone(self)).await?;
        }
        Ok(())
    }

    /// Whether any `async fn` `#[bean]` factory is still parked — registered by
    /// the scan but not yet awaited by
    /// [`init_async_beans`](Container::init_async_beans).
    ///
    /// A synchronous build path (e.g. `ApplicationContextBuilder::build`) checks
    /// this to **fail fast** rather than silently leaving async beans
    /// uninitialized: the presence of any parked factory means the caller must
    /// take an async build path that awaits them.
    #[must_use]
    pub fn has_pending_async_beans(&self) -> bool {
        !self
            .async_inits
            .lock()
            .expect("async_inits lock poisoned")
            .is_empty()
    }

    /// Record the stereotype label of the most-recently-registered `T`.
    ///
    /// Used by the stereotype derives in `firefly-macros` so
    /// [`beans`](Container::beans) and the admin `/beans` view can group beans
    /// by layer. No-op if `T` is not registered.
    pub fn set_stereotype<T: ?Sized + 'static>(&self, label: &str) {
        let type_id = TypeId::of::<T>();
        let registered = self.registered.read().expect("registered lock poisoned");
        if let Some(reg) = registered.iter().rev().find(|r| r.impl_type == type_id) {
            *reg.stereotype.lock().expect("stereotype mutex poisoned") = Some(label.to_string());
        }
    }

    /// Record the short type names of the most-recently-registered `T`'s
    /// `#[autowired]` dependencies.
    ///
    /// Used by the stereotype derives in `firefly-macros` so
    /// [`beans`](Container::beans) and the admin `/beans/graph` dependency graph
    /// can draw edges from a bean to the beans it injects. No-op if `T` is not
    /// registered.
    pub fn set_dependencies<T: 'static>(&self, deps: &[&str]) {
        let type_id = TypeId::of::<T>();
        let registered = self.registered.read().expect("registered lock poisoned");
        if let Some(reg) = registered.iter().rev().find(|r| r.impl_type == type_id) {
            *reg.dependencies
                .lock()
                .expect("dependencies mutex poisoned") =
                deps.iter().map(|s| (*s).to_string()).collect();
        }
    }

    /// Attach a `#[pre_destroy]` teardown hook to the most-recently-registered
    /// `T`. The hook receives the shared instance and runs on
    /// [`destroy`](Container::destroy) in reverse construction order.
    ///
    /// Used by `firefly-macros` for `#[pre_destroy]` methods. No-op if `T` is
    /// not registered.
    pub fn set_destroy_hook<T, F>(&self, hook: F)
    where
        T: Send + Sync + 'static,
        F: Fn(&Arc<T>) + Send + Sync + 'static,
    {
        let type_id = TypeId::of::<T>();
        let registered = self.registered.read().expect("registered lock poisoned");
        if let Some(reg) = registered.iter().rev().find(|r| r.impl_type == type_id) {
            let erased: DestroyHook = Box::new(move |shared: &SharedInstance| {
                if let Ok(typed) = shared.clone().downcast::<T>() {
                    hook(&typed);
                }
            });
            *reg.destroy.lock().expect("destroy mutex poisoned") = Some(erased);
        }
    }

    /// Bind an interface (trait object) to a concrete, already-registered
    /// implementation `T`.
    ///
    /// `coerce` upcasts `Arc<T>` to `Arc<I>` (typically `|a| a`). After binding,
    /// `resolve::<I>()` and `resolve_all::<I>()` see the implementation. Multiple
    /// implementations may be bound to one interface; resolution then picks the
    /// primary one or raises [`ContainerError::NoUniqueBean`].
    ///
    /// Mirrors pyfly's `bind(interface, implementation)`.
    ///
    /// # Panics
    /// Panics if `T` is not registered (call a `register_*` method first).
    pub fn bind<I, T>(&self, coerce: impl Fn(Arc<T>) -> Arc<I> + Send + Sync + 'static)
    where
        I: ?Sized + Send + Sync + 'static,
        T: Send + Sync + 'static,
    {
        let impl_id = TypeId::of::<T>();
        let registration = {
            let by_type = self.by_type.read().expect("by_type lock poisoned");
            by_type
                .get(&impl_id)
                .and_then(|entries| {
                    entries
                        .iter()
                        .find(|e| e.registration.impl_type == impl_id)
                        .map(|e| Arc::clone(&e.registration))
                })
                .unwrap_or_else(|| {
                    panic!(
                        "bind::<{}>(): implementation {} is not registered",
                        type_name::<I>(),
                        type_name::<T>()
                    )
                })
        };
        let caster: Caster = Box::new(move |shared: SharedInstance| {
            let typed: Arc<T> = shared.downcast::<T>().ok()?;
            let view: Arc<I> = coerce(typed);
            Some(Box::new(view) as ErasedArc)
        });
        let mut by_type = self.by_type.write().expect("by_type lock poisoned");
        by_type
            .entry(TypeId::of::<I>())
            .or_default()
            .push(TypeEntry {
                registration,
                caster,
            });
    }

    /// Install a registration under `TypeId::of::<T>()` and return the
    /// shared [`Registration`]. Internal helper for the public `register_*` API.
    fn install<T>(
        &self,
        scope: ScopeSpec,
        name: &str,
        primary: bool,
        order: i32,
        factory: Factory,
    ) -> Arc<Registration>
    where
        T: Send + Sync + 'static,
    {
        let type_id = TypeId::of::<T>();
        let type_name_str = type_name::<T>().to_string();
        let reg = Arc::new(Registration {
            impl_type: type_id,
            type_name: type_name_str.clone(),
            scope,
            name: name.to_string(),
            primary,
            order,
            factory,
            instance: std::sync::Mutex::new(None),
            metrics: std::sync::Mutex::new(BeanMetrics::default()),
            stereotype: std::sync::Mutex::new(None),
            destroy: std::sync::Mutex::new(None),
            dependencies: std::sync::Mutex::new(Vec::new()),
        });

        let caster: Caster = Box::new(|shared: SharedInstance| {
            let typed: Arc<T> = shared.downcast::<T>().ok()?;
            Some(Box::new(typed) as ErasedArc)
        });

        {
            let mut by_type = self.by_type.write().expect("by_type lock poisoned");
            let entries = by_type.entry(type_id).or_default();
            // pyfly's `_registrations` keeps the last registration per type for
            // `resolve`; `_all` keeps every one for `resolve_all`. Replace the
            // self-keyed entry (same impl, same name) so re-registration mirrors
            // dict overwrite, but keep distinct-name entries for resolve_all.
            entries.retain(|e| {
                !(e.registration.impl_type == type_id && e.registration.name == reg.name)
            });
            entries.push(TypeEntry {
                registration: Arc::clone(&reg),
                caster,
            });
        }

        if !name.is_empty() {
            self.by_name
                .write()
                .expect("by_name lock poisoned")
                .insert(name.to_string(), Arc::clone(&reg));
        }

        {
            let mut names = self.type_names.write().expect("type_names lock poisoned");
            if !names.contains(&type_name_str) {
                names.push(type_name_str);
            }
        }

        {
            // Track self-keyed registrations in order for `beans()` /
            // reverse-order `destroy()`. Re-registering the same (type, name)
            // replaces the prior entry so introspection never double-counts.
            let mut registered = self.registered.write().expect("registered lock poisoned");
            registered.retain(|r| !(r.impl_type == type_id && r.name == reg.name));
            registered.push(Arc::clone(&reg));
        }

        reg
    }

    /// Register a custom bean scope (Spring's `registerScope`).
    ///
    /// Beans declared with that scope name resolve through `handler`. Built-in
    /// scope names (`singleton`, `transient`, `request`, `session`) are reserved.
    /// Mirrors pyfly's `register_scope`.
    ///
    /// # Errors
    /// Returns an error if `name` is empty or shadows a built-in scope.
    pub fn register_scope(
        &self,
        name: &str,
        handler: Arc<dyn ScopeHandler>,
    ) -> Result<(), ContainerError> {
        if name.is_empty() {
            return Err(ContainerError::NoSuchBean {
                bean_type: None,
                bean_name: None,
                required_by: None,
                parameter: Some("custom scope name must be a non-empty string".to_string()),
                suggestions: Vec::new(),
            });
        }
        if matches!(name, "singleton" | "transient" | "request" | "session") {
            return Err(ContainerError::NoSuchBean {
                bean_type: None,
                bean_name: None,
                required_by: None,
                parameter: Some(format!("cannot override built-in scope name: {name:?}")),
                suggestions: Vec::new(),
            });
        }
        self.scopes
            .write()
            .expect("scopes lock poisoned")
            .insert(name.to_string(), handler);
        Ok(())
    }

    /// Remove a previously registered custom scope (no-op if absent).
    /// Mirrors pyfly's `unregister_scope`.
    pub fn unregister_scope(&self, name: &str) {
        self.scopes
            .write()
            .expect("scopes lock poisoned")
            .remove(name);
    }

    /// Install the handler that backs the built-in [`Scope::Request`] scope.
    ///
    /// In pyfly REQUEST-scoped beans are resolved through a `RequestContext`
    /// keyed by the active request; Rust drives request lifecycle explicitly, so
    /// the framework registers a per-request [`ScopeHandler`] here (e.g. one that
    /// caches in a task-local map). Until a handler is installed, resolving a
    /// `Scope::Request` bean yields [`ContainerError::NoSuchBean`], matching
    /// pyfly's "no active request context".
    pub fn register_request_scope(&self, handler: Arc<dyn ScopeHandler>) {
        self.scopes
            .write()
            .expect("scopes lock poisoned")
            .insert(Scope::Request.name().to_string(), handler);
    }

    /// Install the handler that backs the built-in [`Scope::Session`] scope.
    ///
    /// The session-scoped counterpart of [`register_request_scope`](Container::register_request_scope).
    pub fn register_session_scope(&self, handler: Arc<dyn ScopeHandler>) {
        self.scopes
            .write()
            .expect("scopes lock poisoned")
            .insert(Scope::Session.name().to_string(), handler);
    }

    // ------------------------------------------------------------------
    // Resolution
    // ------------------------------------------------------------------

    /// Resolve a single instance of `T` (sized or trait object).
    ///
    /// - A direct registration under `TypeId::of::<T>()` resolves immediately.
    /// - A single bound implementation resolves to that implementation.
    /// - Multiple bound implementations require exactly one to be primary,
    ///   else [`ContainerError::NoUniqueBean`].
    /// - No match yields [`ContainerError::NoSuchBean`] with fuzzy suggestions.
    ///
    /// Mirrors pyfly's `resolve`.
    ///
    /// # Errors
    /// See the variant list above.
    pub fn resolve<T>(&self) -> Result<Arc<T>, ContainerError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let type_id = TypeId::of::<T>();
        let chosen = self.choose_entry::<T>(type_id)?;
        let shared = self.resolve_registration(&chosen.registration)?;
        self.apply_caster::<T>(&chosen, shared)
    }

    /// Pick the single applicable entry for the query `type_id`, applying the
    /// primary/order disambiguation rules.
    fn choose_entry<T: ?Sized + Send + Sync + 'static>(
        &self,
        type_id: TypeId,
    ) -> Result<ChosenEntry, ContainerError> {
        let by_type = self.by_type.read().expect("by_type lock poisoned");
        let entries = by_type.get(&type_id);
        let Some(entries) = entries else {
            return Err(ContainerError::no_such_type(
                type_name::<T>(),
                self.fuzzy_suggestions(type_name::<T>()),
            ));
        };
        if entries.is_empty() {
            return Err(ContainerError::no_such_type(
                type_name::<T>(),
                self.fuzzy_suggestions(type_name::<T>()),
            ));
        }
        if entries.len() == 1 {
            return Ok(ChosenEntry {
                registration: Arc::clone(&entries[0].registration),
                caster_index: 0,
                type_id,
            });
        }
        // Multiple candidates: pick the primary one.
        let primaries: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.registration.primary)
            .map(|(i, _)| i)
            .collect();
        if primaries.len() == 1 {
            return Ok(ChosenEntry {
                registration: Arc::clone(&entries[primaries[0]].registration),
                caster_index: primaries[0],
                type_id,
            });
        }
        let candidates: Vec<String> = entries
            .iter()
            .map(|e| e.registration.display_name())
            .collect();
        Err(ContainerError::NoUniqueBean {
            bean_type: type_name::<T>().to_string(),
            candidates,
        })
    }

    /// Resolve a bean by its registered name.
    ///
    /// Mirrors pyfly's `resolve_by_name`. The returned `Arc<T>` must match the
    /// type the bean was registered under, else [`ContainerError::NoSuchBean`].
    ///
    /// # Errors
    /// [`ContainerError::NoSuchBean`] if no bean has that name, or the named
    /// bean is not of type `T`.
    pub fn resolve_named<T>(&self, name: &str) -> Result<Arc<T>, ContainerError>
    where
        T: Send + Sync + 'static,
    {
        let reg = {
            let by_name = self.by_name.read().expect("by_name lock poisoned");
            by_name.get(name).map(Arc::clone)
        };
        let Some(reg) = reg else {
            let names = {
                let by_name = self.by_name.read().expect("by_name lock poisoned");
                by_name.keys().cloned().collect::<Vec<_>>()
            };
            return Err(ContainerError::no_such_name(name, names));
        };
        let shared = self.resolve_registration(&reg)?;
        shared
            .downcast::<T>()
            .map_err(|_| ContainerError::NoSuchBean {
                bean_type: Some(type_name::<T>().to_string()),
                bean_name: Some(name.to_string()),
                required_by: None,
                parameter: Some(format!(
                    "bean {name:?} is registered as {}, not assignable to {}",
                    reg.type_name,
                    type_name::<T>()
                )),
                suggestions: Vec::new(),
            })
    }

    /// Resolve a named bean to its type-erased [`SharedInstance`], warming the
    /// singleton cache without knowing the static type.
    ///
    /// Used by the `ApplicationContext` eager-init pass to fail-fast-build every
    /// named singleton at startup (running its `#[post_construct]`). Prefer the
    /// typed [`resolve_named`](Container::resolve_named) for actual use.
    ///
    /// # Errors
    /// [`ContainerError::NoSuchBean`] if no bean has that name; propagates a
    /// construction error from the factory.
    pub fn resolve_named_erased(&self, name: &str) -> Result<SharedInstance, ContainerError> {
        let reg = {
            let by_name = self.by_name.read().expect("by_name lock poisoned");
            by_name.get(name).map(Arc::clone)
        };
        let Some(reg) = reg else {
            let names = {
                let by_name = self.by_name.read().expect("by_name lock poisoned");
                by_name.keys().cloned().collect::<Vec<_>>()
            };
            return Err(ContainerError::no_such_name(name, names));
        };
        self.resolve_registration(&reg)
    }

    /// Resolve every bean registered or bound under `T`.
    ///
    /// Returns one `Arc<T>` per registration, deduplicated by instance identity
    /// (so a synthetic interface binding does not double-count an already-bound
    /// implementation), ordered by each registration's `order` (stable within
    /// equal orders). Mirrors pyfly's `resolve_all` / `list[T]` injection.
    ///
    /// # Errors
    /// Propagates the first construction error encountered.
    pub fn resolve_all<T>(&self) -> Result<Vec<Arc<T>>, ContainerError>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let type_id = TypeId::of::<T>();
        // Snapshot the entries (registration + index) ordered by `order`.
        let snapshot: Vec<(usize, Arc<Registration>)> = {
            let by_type = self.by_type.read().expect("by_type lock poisoned");
            let Some(entries) = by_type.get(&type_id) else {
                return Ok(Vec::new());
            };
            let mut indexed: Vec<(usize, Arc<Registration>)> = entries
                .iter()
                .enumerate()
                .map(|(i, e)| (i, Arc::clone(&e.registration)))
                .collect();
            indexed.sort_by_key(|(_, reg)| reg.order);
            indexed
        };

        let mut out: Vec<Arc<T>> = Vec::new();
        let mut seen: Vec<*const ()> = Vec::new();
        for (idx, reg) in snapshot {
            let shared = self.resolve_registration(&reg)?;
            let identity = Arc::as_ptr(&shared) as *const ();
            if seen.contains(&identity) {
                continue;
            }
            seen.push(identity);
            // Re-read the caster by index (entries are append-only per registration pass).
            let erased = {
                let by_type = self.by_type.read().expect("by_type lock poisoned");
                let entries = by_type.get(&type_id).expect("entries vanished");
                (entries[idx].caster)(shared)
            };
            if let Some(erased) = erased {
                out.push(downcast_arc::<T>(erased)?);
            }
        }
        Ok(out)
    }

    /// Resolve a single registration, honoring its scope.
    fn resolve_registration(
        &self,
        reg: &Arc<Registration>,
    ) -> Result<SharedInstance, ContainerError> {
        match &reg.scope {
            ScopeSpec::Builtin(Scope::Singleton) => {
                {
                    let cached = reg.instance.lock().expect("registration mutex poisoned");
                    if let Some(existing) = cached.as_ref() {
                        self.bump_resolution(reg);
                        return Ok(existing.clone());
                    }
                }
                // Build outside the instance lock so cycle detection (a separate
                // thread-local) governs reentrancy, then double-check under lock.
                let built = self.create_instance(reg)?;
                let mut cached = reg.instance.lock().expect("registration mutex poisoned");
                if let Some(existing) = cached.as_ref() {
                    self.bump_resolution(reg);
                    return Ok(existing.clone());
                }
                *cached = Some(built.clone());
                drop(cached);
                self.bump_resolution(reg);
                Ok(built)
            }
            ScopeSpec::Builtin(Scope::Transient) => {
                let built = self.create_instance(reg)?;
                self.bump_resolution(reg);
                Ok(built)
            }
            ScopeSpec::Builtin(Scope::Request) | ScopeSpec::Builtin(Scope::Session) => {
                // REQUEST/SESSION are driven through a custom ScopeHandler keyed
                // by the scope name (the Rust adaptation of pyfly's RequestContext).
                let handler_name = reg.scope.name();
                let built = self.via_scope_handler(reg, &handler_name)?;
                self.bump_resolution(reg);
                Ok(built)
            }
            ScopeSpec::Custom(name) => {
                let name = name.clone();
                let built = self.via_scope_handler(reg, &name)?;
                self.bump_resolution(reg);
                Ok(built)
            }
        }
    }

    /// Resolve through a custom [`ScopeHandler`] registered by `scope_name`.
    fn via_scope_handler(
        &self,
        reg: &Arc<Registration>,
        scope_name: &str,
    ) -> Result<SharedInstance, ContainerError> {
        let handler = {
            let scopes = self.scopes.read().expect("scopes lock poisoned");
            scopes.get(scope_name).map(Arc::clone)
        };
        let Some(handler) = handler else {
            return Err(ContainerError::NoSuchBean {
                bean_type: Some(reg.type_name.clone()),
                bean_name: None,
                required_by: None,
                parameter: Some(format!(
                    "custom scope {scope_name:?} is not registered for bean {}; \
                     call register_scope({scope_name:?}, handler) first",
                    reg.display_name()
                )),
                suggestions: Vec::new(),
            });
        };
        let cache_key = format!("__firefly_bean_{}", reg.type_name);
        let reg_clone = Arc::clone(reg);
        let factory = move || self.create_instance(&reg_clone);
        handler.get(&cache_key, &factory)
    }

    /// Create a fresh instance via the registration's factory, with
    /// thread-local circular-dependency detection.
    fn create_instance(&self, reg: &Arc<Registration>) -> Result<SharedInstance, ContainerError> {
        // Cycle check: is this type already in this thread's creation stack?
        let cycle = RESOLVING.with(|stack| {
            let stack = stack.borrow();
            if stack.iter().any(|(id, _)| *id == reg.impl_type) {
                let chain: Vec<String> = stack.iter().map(|(_, name)| name.clone()).collect();
                Some(ContainerError::CircularDependency {
                    chain,
                    current: reg.type_name.clone(),
                })
            } else {
                None
            }
        });
        if let Some(err) = cycle {
            return Err(err);
        }

        RESOLVING.with(|stack| {
            stack
                .borrow_mut()
                .push((reg.impl_type, reg.type_name.clone()))
        });
        let start = Instant::now();
        let result = (reg.factory)(self);
        // Always pop, even on error, so the stack is clean after a failed resolve.
        RESOLVING.with(|stack| {
            stack.borrow_mut().pop();
        });

        let instance = result?;
        let mut metrics = reg.metrics.lock().expect("metrics mutex poisoned");
        metrics.creation_time_ns = start.elapsed().as_nanos();
        Ok(instance)
    }

    fn bump_resolution(&self, reg: &Arc<Registration>) {
        reg.metrics
            .lock()
            .expect("metrics mutex poisoned")
            .resolution_count += 1;
    }

    /// A deferred [`Provider<T>`] for `T`.
    ///
    /// Mirrors pyfly's `Provider[T]` injection. Requires the container to be
    /// wrapped in an `Arc`; clone the `Arc` you already share. Also installs
    /// the self-handle so later `&Container`-only factory closures can build
    /// providers via [`provider_for`](Container::provider_for).
    #[must_use]
    pub fn provider<T: ?Sized + Send + Sync + 'static>(self: &Arc<Self>) -> Provider<T> {
        let _ = self.me.set(Arc::downgrade(self));
        Provider::new(Arc::clone(self))
    }

    /// Build a [`Provider<T>`] from a borrowed container — the helper a
    /// generated `Provider<T>` autowiring uses inside a factory closure.
    ///
    /// # Panics
    /// Panics if the container has no installed self-handle. Create the
    /// container with [`shared`](Container::shared) (or call
    /// [`install_shared_handle`](Container::install_shared_handle)) before
    /// resolving any bean that autowires a `Provider<T>` field.
    #[must_use]
    pub fn provider_for<T: ?Sized + Send + Sync + 'static>(&self) -> Provider<T> {
        let arc = self.me.get().and_then(Weak::upgrade).expect(
            "Container::provider_for requires a shared container: create it with \
                 Container::shared() (or call install_shared_handle) before resolving a \
                 bean that autowires a Provider<T> field",
        );
        Provider::new(arc)
    }

    // ------------------------------------------------------------------
    // Introspection / lifecycle
    // ------------------------------------------------------------------

    /// Whether a bean is registered under exactly the type `T`.
    /// Mirrors pyfly's `contains_type`.
    #[must_use]
    pub fn contains_type<T: ?Sized + 'static>(&self) -> bool {
        self.by_type
            .read()
            .expect("by_type lock poisoned")
            .contains_key(&TypeId::of::<T>())
    }

    /// Whether a named bean exists. Mirrors pyfly's `contains`.
    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.by_name
            .read()
            .expect("by_name lock poisoned")
            .contains_key(name)
    }

    /// Snapshot of all registered bean type names. Mirrors pyfly's
    /// `registered_types` (returning names, since `type` objects have no Rust
    /// analog).
    #[must_use]
    pub fn registered_types(&self) -> Vec<String> {
        self.type_names
            .read()
            .expect("type_names lock poisoned")
            .clone()
    }

    /// Drop the cached singleton instance of `T` so it is rebuilt on next
    /// resolve. Returns whether an instance was evicted. Mirrors pyfly's
    /// `reset_instance` (the refresh/config-reload hook).
    pub fn reset_instance<T: 'static>(&self) -> bool {
        let type_id = TypeId::of::<T>();
        let by_type = self.by_type.read().expect("by_type lock poisoned");
        let Some(entries) = by_type.get(&type_id) else {
            return false;
        };
        let mut evicted = false;
        for entry in entries {
            if entry.registration.impl_type == type_id {
                let mut cached = entry
                    .registration
                    .instance
                    .lock()
                    .expect("registration mutex poisoned");
                if cached.take().is_some() {
                    evicted = true;
                }
            }
        }
        evicted
    }

    /// Per-bean metrics for `T`, or `None` if never resolved/registered.
    /// Mirrors pyfly's `get_bean_metrics`.
    #[must_use]
    pub fn bean_metrics<T: 'static>(&self) -> Option<BeanMetrics> {
        let type_id = TypeId::of::<T>();
        let by_type = self.by_type.read().expect("by_type lock poisoned");
        by_type.get(&type_id).and_then(|entries| {
            entries
                .iter()
                .find(|e| e.registration.impl_type == type_id)
                .map(|e| {
                    *e.registration
                        .metrics
                        .lock()
                        .expect("metrics mutex poisoned")
                })
        })
    }

    /// Hand-rolled fuzzy suggestions: registered type names similar to `name`.
    ///
    /// A dependency-free analog of pyfly's `difflib.get_close_matches` — scores
    /// each registered name by a normalized longest-common-subsequence ratio and
    /// returns the closest matches above a `0.4` cutoff, best first (max 5).
    #[must_use]
    pub fn fuzzy_suggestions(&self, name: &str) -> Vec<String> {
        let short = short_name(name);
        if short.is_empty() {
            return Vec::new();
        }
        let names = self.type_names.read().expect("type_names lock poisoned");
        let mut scored: Vec<(f64, String)> = names
            .iter()
            .map(|candidate| (similarity(short, short_name(candidate)), candidate.clone()))
            .filter(|(score, _)| *score >= 0.4)
            .collect();
        // Sort by score desc, then name for determinism.
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        scored.into_iter().take(5).map(|(_, n)| n).collect()
    }

    // ------------------------------------------------------------------
    // Conditional context (profiles + config properties)
    // ------------------------------------------------------------------

    /// Install the [`ConditionContext`] that [`scan`](Container::scan) consults
    /// to gate conditional/profile-guarded beans.
    ///
    /// The `firefly` facade's `ApplicationContext` builds this from
    /// `firefly_config` (active profiles + flattened config map); a standalone
    /// container can set it directly. Replaces any previously-set context.
    pub fn set_condition_context(&self, ctx: ConditionContext) {
        *self.conditions.write().expect("conditions lock poisoned") = ctx;
    }

    /// A clone of the currently-installed [`ConditionContext`].
    #[must_use]
    pub fn condition_context(&self) -> ConditionContext {
        self.conditions
            .read()
            .expect("conditions lock poisoned")
            .clone()
    }

    /// The condition-context property map restricted to keys under `prefix`
    /// (prefix-stripped) — the input a `#[derive(ConfigProperties)]` bean binds
    /// from. See [`ConditionContext::properties_with_prefix`].
    #[must_use]
    pub fn config_properties(&self, prefix: &str) -> HashMap<String, String> {
        self.conditions
            .read()
            .expect("conditions lock poisoned")
            .properties_with_prefix(prefix)
    }

    // ------------------------------------------------------------------
    // Component scanning (pyfly scan_package / _auto_bind_interfaces)
    // ------------------------------------------------------------------

    /// Register every stereotype-annotated type discovered across the crate
    /// graph, honoring conditionals and profiles.
    ///
    /// The Rust analog of pyfly's `scan_package` + `scan_module_classes`:
    /// every `#[derive(Component)]` / `Service` / `Repository` /
    /// `Configuration` / `Controller` / `ConfigProperties` emits an
    /// [`inventory::submit!`] thunk, and `scan` collects them all via
    /// [`inventory::iter`]. Conditions and `#[profile(...)]` are evaluated
    /// against the installed [`ConditionContext`] in two passes (registry-
    /// independent first, then bean-dependent), mirroring pyfly's
    /// `_evaluate_conditions` / `_evaluate_bean_conditions`.
    ///
    /// Returns the number of beans registered.
    ///
    /// # Generics
    /// Generic types cannot be inventoried (the concrete monomorphization is
    /// chosen at the use site). Register those with the explicit
    /// `register_all!(container, [Foo::<Bar>, ...])` fallback after scanning.
    pub fn scan(&self) -> usize {
        let ctx = self.condition_context();
        self.scan_with(&ctx)
    }

    /// Like [`scan`](Container::scan) but against an explicit
    /// [`ConditionContext`] (which is also installed on the container for any
    /// later resolution-time use).
    pub fn scan_with(&self, ctx: &ConditionContext) -> usize {
        self.set_condition_context(ctx.clone());
        let all: Vec<&'static ComponentRegistration> = scan::discovered().collect();
        self.register_scanned(all)
    }

    /// Register only the components whose defining module path falls under one
    /// of `base_packages` — the Rust analog of Spring's
    /// `@ComponentScan(basePackages = …)`. A registration matches when its
    /// `module_path` equals a base package or is a submodule of it. Conditions
    /// and profiles are honoured exactly as in [`scan`](Container::scan).
    ///
    /// Returns the number of beans registered.
    pub fn scan_packages(&self, base_packages: &[&str]) -> usize {
        let ctx = self.condition_context();
        self.scan_packages_with(&ctx, base_packages)
    }

    /// Like [`scan_packages`](Container::scan_packages) but against an explicit
    /// [`ConditionContext`].
    pub fn scan_packages_with(&self, ctx: &ConditionContext, base_packages: &[&str]) -> usize {
        self.set_condition_context(ctx.clone());
        let selected: Vec<&'static ComponentRegistration> = scan::discovered()
            .filter(|r| in_base_packages(r.module_path, base_packages))
            .collect();
        self.register_scanned(selected)
    }

    /// The shared two-pass registration core behind [`scan`](Container::scan) /
    /// [`scan_packages`](Container::scan_packages): filter by registry-
    /// independent conditions, register the unconditional survivors in `order`,
    /// then evaluate bean-dependent conditions against the now-populated
    /// registry. This is exactly how pyfly registers then prunes — and it is
    /// what makes `@ConditionalOnMissingBean` auto-configuration beans yield to
    /// any user bean of the same type (the user bean, being unconditional,
    /// registers in the first pass; the auto-config bean is deferred and only
    /// fills the gap).
    fn register_scanned(&self, all: Vec<&'static ComponentRegistration>) -> usize {
        let ctx = self.condition_context();

        // Pass 1: keep only beans whose registry-independent conditions pass.
        let pass1: Vec<&'static ComponentRegistration> = all
            .into_iter()
            .filter(|r| ctx.pass1(&(r.conditions)()))
            .collect();

        let mut ordered = pass1;
        ordered.sort_by_key(|r| r.order);

        let (deferred, immediate): (Vec<_>, Vec<_>) = ordered
            .into_iter()
            .partition(|r| (r.conditions)().iter().any(Condition::is_bean_dependent));

        let mut count = 0usize;
        for reg in &immediate {
            (reg.register)(self);
            count += 1;
        }
        // Pass 2: evaluate bean-dependent conditions against the now-populated
        // registry, registering survivors in order.
        for reg in &deferred {
            if self.eval_bean_conditions(&(reg.conditions)()) {
                (reg.register)(self);
                count += 1;
            }
        }
        count
    }

    /// Evaluate the bean-dependent (pass-2) conditions against the current
    /// registry. Type matching is by short type name (the inventory thunk
    /// records the dependency type as a string, since `TypeId` of an arbitrary
    /// referenced type is not available at submit time).
    fn eval_bean_conditions(&self, conditions: &[Condition]) -> bool {
        conditions.iter().all(|c| match c {
            Condition::OnBean(ty) => self.count_assignable_by_name(ty) > 0,
            Condition::OnMissingBean(ty) => self.count_assignable_by_name(ty) == 0,
            Condition::OnSingleCandidate(ty) => self.count_assignable_by_name(ty) == 1,
            _ => true,
        })
    }

    /// Count registered beans whose short type name matches `name` (the
    /// dependency string a bean-dependent condition carries). Type-only, never
    /// resolves an instance — matching pyfly's registration-based counting.
    #[must_use]
    pub fn count_assignable_by_name(&self, name: &str) -> usize {
        let target = short_name(name);
        let registered = self.registered.read().expect("registered lock poisoned");
        registered
            .iter()
            .filter(|r| short_name(&r.type_name) == target)
            .count()
    }

    // ------------------------------------------------------------------
    // Bean introspection (admin /beans + overview)
    // ------------------------------------------------------------------

    /// A snapshot of every self-keyed bean registration, for the admin
    /// `/beans` view.
    ///
    /// Ports the shape pyfly's `BeansProvider.get_beans` returns
    /// (name/type/scope/stereotype/primary + initialized + resolution count).
    /// Synthetic interface bindings are excluded — only the concrete
    /// registrations are reported, one per bean.
    #[must_use]
    pub fn beans(&self) -> Vec<BeanDescriptor> {
        let registered = self.registered.read().expect("registered lock poisoned");
        registered
            .iter()
            .map(|reg| {
                let metrics = *reg.metrics.lock().expect("metrics mutex poisoned");
                BeanDescriptor {
                    name: reg.display_name(),
                    type_name: reg.type_name.clone(),
                    scope: reg.scope.name(),
                    stereotype: reg.stereotype(),
                    primary: reg.primary,
                    initialized: reg
                        .instance
                        .lock()
                        .expect("registration mutex poisoned")
                        .is_some(),
                    resolution_count: metrics.resolution_count,
                    dependencies: reg.dependencies(),
                }
            })
            .collect()
    }

    /// Aggregate bean counts (total + per-stereotype) for the admin overview.
    ///
    /// Mirrors pyfly `OverviewProvider`'s `beans` block.
    #[must_use]
    pub fn bean_stats(&self) -> BeanStats {
        let registered = self.registered.read().expect("registered lock poisoned");
        let mut stats = BeanStats {
            total: registered.len(),
            ..BeanStats::default()
        };
        for reg in registered.iter() {
            let label = reg.stereotype().unwrap_or_else(|| "component".to_string());
            *stats.stereotypes.entry(label).or_insert(0) += 1;
        }
        stats
    }

    /// The number of registered beans (counting only concrete self-keyed
    /// registrations, not synthetic interface bindings).
    #[must_use]
    pub fn bean_count(&self) -> usize {
        self.registered
            .read()
            .expect("registered lock poisoned")
            .len()
    }

    // ------------------------------------------------------------------
    // Lifecycle (pyfly _call_pre_destroy)
    // ------------------------------------------------------------------

    /// Invoke every `#[pre_destroy]` hook in reverse construction order, then
    /// clear cached singleton instances.
    ///
    /// Ports pyfly's `_call_pre_destroy` shutdown pass: hooks run on the
    /// already-built singleton instances, last-registered first, de-duplicated
    /// by instance identity. A hook is a teardown side-effect (errors are not
    /// propagated — Rust `Drop` still runs afterwards for deterministic cleanup).
    pub fn destroy(&self) {
        let regs: Vec<Arc<Registration>> = {
            let registered = self.registered.read().expect("registered lock poisoned");
            registered.iter().rev().cloned().collect()
        };
        let mut seen: Vec<*const ()> = Vec::new();
        for reg in regs {
            let instance = {
                let guard = reg.instance.lock().expect("registration mutex poisoned");
                guard.clone()
            };
            let Some(instance) = instance else { continue };
            let identity = Arc::as_ptr(&instance) as *const ();
            if seen.contains(&identity) {
                continue;
            }
            seen.push(identity);
            if let Some(hook) = reg.destroy.lock().expect("destroy mutex poisoned").as_ref() {
                hook(&instance);
            }
        }
        // Evict cached singletons so a subsequent resolve rebuilds them.
        let registered = self.registered.read().expect("registered lock poisoned");
        for reg in registered.iter() {
            *reg.instance.lock().expect("registration mutex poisoned") = None;
        }
    }
}

/// The chosen entry from [`Container::choose_entry`] — a registration plus the
/// index of the caster to apply (the caster is re-read under the read lock).
struct ChosenEntry {
    registration: Arc<Registration>,
    caster_index: usize,
    type_id: TypeId,
}

impl Container {
    /// Resolve, then apply the caster at `caster_index` (helper used by
    /// [`resolve`](Container::resolve) after [`choose_entry`](Container::choose_entry)).
    fn apply_caster<T: ?Sized + Send + Sync + 'static>(
        &self,
        chosen: &ChosenEntry,
        shared: SharedInstance,
    ) -> Result<Arc<T>, ContainerError> {
        let erased = {
            let by_type = self.by_type.read().expect("by_type lock poisoned");
            let entries = by_type.get(&chosen.type_id).expect("entries vanished");
            (entries[chosen.caster_index].caster)(shared)
        }
        .ok_or_else(|| {
            ContainerError::no_such_type(type_name::<T>(), self.fuzzy_suggestions(type_name::<T>()))
        })?;
        downcast_arc::<T>(erased)
    }
}

/// Downcast an [`ErasedArc`] (a boxed `Arc<T>`) back to `Arc<T>`.
fn downcast_arc<T: ?Sized + Send + Sync + 'static>(
    erased: ErasedArc,
) -> Result<Arc<T>, ContainerError> {
    erased
        .downcast::<Arc<T>>()
        .map(|boxed| *boxed)
        .map_err(|_| ContainerError::no_such_type(type_name::<T>(), Vec::new()))
}

/// Strip module path and generic arguments from a Rust type name so fuzzy
/// matching compares the short identifier (e.g. `foo::bar::Greeter<T>` ->
/// `Greeter`), the way pyfly compares `__name__`.
fn short_name(name: &str) -> &str {
    let no_generics = name.split('<').next().unwrap_or(name);
    no_generics.rsplit("::").next().unwrap_or(no_generics)
}

/// Whether a component's defining `module_path` falls under any of the given
/// base packages — used by [`Container::scan_packages`]. A module matches a
/// base package when it equals it exactly or is a descendant module (the base
/// package followed by `::`). The empty base-package list matches nothing.
fn in_base_packages(module_path: &str, base_packages: &[&str]) -> bool {
    base_packages.iter().any(|pkg| {
        module_path == *pkg
            || module_path
                .strip_prefix(pkg)
                .is_some_and(|rest| rest.starts_with("::"))
    })
}

/// Normalized similarity in `[0, 1]` between two strings, based on the length
/// of their longest common subsequence. Case-insensitive. Dependency-free.
fn similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let a: Vec<char> = a.to_ascii_lowercase().chars().collect();
    let b: Vec<char> = b.to_ascii_lowercase().chars().collect();
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let lcs = longest_common_subsequence(&a, &b);
    (2.0 * lcs as f64) / (a.len() + b.len()) as f64
}

fn longest_common_subsequence(a: &[char], b: &[char]) -> usize {
    let mut prev = vec![0usize; b.len() + 1];
    let mut curr = vec![0usize; b.len() + 1];
    for &ca in a {
        for (j, &cb) in b.iter().enumerate() {
            curr[j + 1] = if ca == cb {
                prev[j] + 1
            } else {
                prev[j + 1].max(curr[j])
            };
        }
        std::mem::swap(&mut prev, &mut curr);
        curr.iter_mut().for_each(|v| *v = 0);
    }
    prev[b.len()]
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn short_name_strips_path_and_generics() {
        assert_eq!(short_name("a::b::Greeter"), "Greeter");
        assert_eq!(short_name("Repository<User>"), "Repository");
        assert_eq!(short_name("Greeter"), "Greeter");
    }

    #[test]
    fn similarity_is_symmetric_and_bounded() {
        assert!((similarity("Greeter", "Greeter") - 1.0).abs() < 1e-9);
        assert_eq!(similarity("abc", ""), 0.0);
        let s1 = similarity("Greeter", "Greet");
        let s2 = similarity("Greet", "Greeter");
        assert!((s1 - s2).abs() < 1e-9);
        assert!(s1 > 0.4);
    }

    // `register_port` installs a pre-erased `Arc<dyn Trait>` so it resolves as
    // the trait object — the framework's path for infrastructure ports.
    #[test]
    fn register_port_resolves_trait_object() {
        trait Greeter: Send + Sync {
            fn hello(&self) -> &'static str;
        }
        struct Polite;
        impl Greeter for Polite {
            fn hello(&self) -> &'static str {
                "good day"
            }
        }

        let container = Container::new();
        let port: Arc<dyn Greeter> = Arc::new(Polite);
        container.register_port::<dyn Greeter>(Arc::clone(&port));
        container.set_stereotype::<dyn Greeter>("component");

        let resolved: Arc<dyn Greeter> = container.resolve::<dyn Greeter>().expect("port resolves");
        assert_eq!(resolved.hello(), "good day");
        // It is the SAME instance, not a copy.
        assert!(Arc::ptr_eq(&port, &resolved));
        // And it shows up in the bean listing under its trait name + stereotype.
        let beans = container.beans();
        assert!(beans.iter().any(
            |b| b.type_name.contains("Greeter") && b.stereotype.as_deref() == Some("component")
        ));
    }
}
