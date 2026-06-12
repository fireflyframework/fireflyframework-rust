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

mod error;
mod provider;
mod registration;
mod scope;

use std::any::{type_name, Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

pub use error::ContainerError;
pub use provider::Provider;
pub use registration::{BeanMetrics, Factory, Registration, HIGHEST_PRECEDENCE, LOWEST_PRECEDENCE};
pub use scope::{RefreshScope, Scope, ScopeHandler, ScopeSpec, SharedInstance, REFRESH_SCOPE_NAME};

/// Framework version stamp.
pub const VERSION: &str = "26.6.1";

/// Type-erased boxed `Arc<T>` — a sized fat pointer wrapped in `Box<dyn Any>`
/// so resolution can return `Arc<T>` for both sized and `?Sized` (trait-object)
/// `T`. `Arc<dyn Any>` cannot downcast to an unsized view directly, so each
/// query key carries a [`Caster`] that produces the right `Arc<T>`.
type ErasedArc = Box<dyn Any + Send + Sync>;

/// Converts a registration's stored [`SharedInstance`] into the concrete `Arc<T>`
/// expected under a particular query [`TypeId`], boxed as an [`ErasedArc`].
type Caster = Box<dyn Fn(SharedInstance) -> Option<ErasedArc> + Send + Sync>;

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
}

impl Container {
    /// Create an empty container.
    #[must_use]
    pub fn new() -> Self {
        Container::default()
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
    /// wrapped in an `Arc`; clone the `Arc` you already share.
    #[must_use]
    pub fn provider<T: ?Sized + Send + Sync + 'static>(self: &Arc<Self>) -> Provider<T> {
        Provider::new(Arc::clone(self))
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
}
