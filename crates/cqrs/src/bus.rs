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

//! The type-dispatched command/query bus and its middleware contract.

use std::any::{type_name, Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::future::BoxFuture;
use serde::Serialize;

use crate::authorization::AuthorizationResult;
use crate::context::ExecutionContext;
use crate::event::DomainEvent;
use crate::fluent::MessageMetadata;

/// Whether a [`Message`] is a **command** (mutates state) or a **query**
/// (reads state) — the CQRS write/read split. Set by `#[derive(Command)]`
/// (the default) / `#[derive(Query)]`, and recorded per handler so the bus can
/// report registered commands and queries separately (pyfly's
/// `get_registered_command_types()` / `get_registered_query_types()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessageKind {
    /// A state-mutating command.
    #[default]
    Command,
    /// A read-only query.
    Query,
}
use crate::CqrsError;

/// A command or query that can be dispatched through the [`Bus`].
///
/// Go lets *any* value be a message and discovers extra behaviour through
/// optional interfaces (`Validatable`, `Cacheable`) at runtime. Rust has
/// no runtime interface queries, so the optional interfaces become
/// default methods on this trait — override the ones you want, leave the
/// rest alone:
///
/// - [`Message::validate`] is Go's `Validatable.Validate()`; the default
///   always succeeds, so plain messages pass the validation middleware
///   untouched.
/// - [`Message::cache_ttl`] is Go's `Cacheable.CacheTTL()`; the default
///   `None` means "not cacheable", so commands fall straight through the
///   query cache.
///
/// The [`Serialize`] supertrait mirrors Go, where `json.Marshal` works on
/// any struct — the JSON encoding seeds the query-cache key. [`Clone`]
/// stands in for Go's pass-by-value handler invocation.
pub trait Message: Clone + Serialize + Send + Sync + 'static {
    /// Whether this message is a command or a query — the CQRS kind, used by
    /// the bus to segregate registered command and query handlers. Defaults to
    /// [`MessageKind::Command`]; `#[derive(Query)]` overrides it to
    /// [`MessageKind::Query`].
    fn kind() -> MessageKind {
        MessageKind::Command
    }

    /// Pre-dispatch validation hook honoured by [`ValidationMiddleware`].
    ///
    /// Return an error (conventionally [`CqrsError::validation`]) to
    /// short-circuit dispatch before the handler runs. The default
    /// implementation accepts everything.
    fn validate(&self) -> Result<(), CqrsError> {
        Ok(())
    }

    /// Cache opt-in honoured by [`QueryCache`](crate::QueryCache).
    ///
    /// Return `Some(ttl)` to memoise results for `ttl`;
    /// `Some(Duration::ZERO)` caches without expiry (Go's `ttl <= 0`).
    /// The default `None` disables caching for the type.
    fn cache_ttl(&self) -> Option<Duration> {
        None
    }

    /// Pre-dispatch authorization hook honoured by
    /// [`AuthorizationMiddleware`](crate::AuthorizationMiddleware) —
    /// pyfly's `authorize()` / `authorize_with_context(ctx)` pair
    /// collapsed into one method (same pattern as [`Message::validate`]).
    ///
    /// `ctx` is the [`ExecutionContext`] attached to the dispatch via
    /// [`Bus::send_with_context`] or a fluent builder, and `None` for a
    /// plain [`Bus::send`]. The default implementation authorizes
    /// everything, mirroring pyfly messages without an `authorize`
    /// method.
    fn authorize(&self, ctx: Option<&ExecutionContext>) -> AuthorizationResult {
        let _ = ctx;
        AuthorizationResult::success()
    }

    /// The domain events this command produced, harvested by
    /// [`DomainEventMiddleware`](crate::DomainEventMiddleware) after a
    /// successful dispatch — pyfly's `command.domain_events`.
    ///
    /// The default returns no events, so a plain message publishes nothing,
    /// mirroring a pyfly command without a `domain_events` attribute. A
    /// command that mutates aggregate state overrides this to surface the
    /// events to publish to the EDA broker.
    fn domain_events(&self) -> Vec<DomainEvent> {
        Vec::new()
    }
}

type ErasedRef<'a> = &'a (dyn Any + Send + Sync);

/// A type-erased message travelling down the middleware chain.
///
/// Plays the role of Go's `msg any` parameter: middleware sees the
/// envelope, not the concrete type, but can still consult the optional
/// capabilities ([`Envelope::validate`], [`Envelope::cache_ttl`],
/// [`Envelope::cache_json`]) that were captured from the [`Message`]
/// impl when the envelope was built.
pub struct Envelope {
    message: Box<dyn Any + Send + Sync>,
    type_id: TypeId,
    type_name: &'static str,
    validate_fn: fn(ErasedRef<'_>) -> Result<(), CqrsError>,
    cache_ttl_fn: fn(ErasedRef<'_>) -> Option<Duration>,
    cache_json_fn: fn(ErasedRef<'_>) -> Result<Vec<u8>, CqrsError>,
    authorize_fn: fn(ErasedRef<'_>, Option<&ExecutionContext>) -> AuthorizationResult,
    domain_events_fn: fn(ErasedRef<'_>) -> Vec<DomainEvent>,
    context: Option<Arc<ExecutionContext>>,
    metadata: Option<MessageMetadata>,
    cache_ttl_override: Option<Option<Duration>>,
    cache_key_override: Option<String>,
}

impl Envelope {
    /// Wraps a concrete message for dynamic dispatch, capturing its
    /// [`Message`] capabilities in type-erased form.
    pub fn new<C: Message>(message: C) -> Self {
        Self {
            message: Box::new(message),
            type_id: TypeId::of::<C>(),
            type_name: type_name::<C>(),
            validate_fn: |m: ErasedRef<'_>| erased::<C>(m).validate(),
            cache_ttl_fn: |m: ErasedRef<'_>| erased::<C>(m).cache_ttl(),
            cache_json_fn: |m: ErasedRef<'_>| Ok(serde_json::to_vec(erased::<C>(m))?),
            authorize_fn: |m: ErasedRef<'_>, ctx| erased::<C>(m).authorize(ctx),
            domain_events_fn: |m: ErasedRef<'_>| erased::<C>(m).domain_events(),
            context: None,
            metadata: None,
            cache_ttl_override: None,
            cache_key_override: None,
        }
    }

    /// Attaches an [`ExecutionContext`] to the dispatch — the Rust
    /// spelling of pyfly threading the context through the bus.
    /// Consulted by [`Envelope::authorize`] and handed to handlers
    /// registered via [`Bus::register_with_context`].
    #[must_use]
    pub fn with_context(mut self, context: ExecutionContext) -> Self {
        self.context = Some(Arc::new(context));
        self
    }

    /// Attaches dispatch [`MessageMetadata`] (set by the fluent
    /// builders) for middleware to read.
    #[must_use]
    pub fn with_metadata(mut self, metadata: MessageMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Overrides the message's [`Message::cache_ttl`] for this dispatch
    /// — `Some(ttl)` forces caching, `None` forces a bypass (pyfly's
    /// `QueryBuilder.cached(...)`).
    #[must_use]
    pub fn with_cache_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.cache_ttl_override = Some(ttl);
        self
    }

    /// Replaces the derived cache key with an explicit one for this
    /// dispatch (pyfly's `QueryBuilder.with_cache_key`).
    #[must_use]
    pub fn with_cache_key(mut self, key: impl Into<String>) -> Self {
        self.cache_key_override = Some(key.into());
        self
    }

    /// The [`ExecutionContext`] attached to this dispatch, if any.
    pub fn context(&self) -> Option<&ExecutionContext> {
        self.context.as_deref()
    }

    /// The dispatch [`MessageMetadata`] attached by a fluent builder,
    /// if any.
    pub fn metadata(&self) -> Option<&MessageMetadata> {
        self.metadata.as_ref()
    }

    /// The explicit cache key for this dispatch, if one was set via
    /// [`Envelope::with_cache_key`].
    pub fn cache_key(&self) -> Option<&str> {
        self.cache_key_override.as_deref()
    }

    /// Runs the message's [`Message::authorize`] hook against the
    /// attached [`ExecutionContext`] (if any).
    pub fn authorize(&self) -> AuthorizationResult {
        (self.authorize_fn)(self.message.as_ref(), self.context.as_deref())
    }

    /// Reads the message's [`Message::domain_events`] hook — the events
    /// [`DomainEventMiddleware`](crate::DomainEventMiddleware) publishes
    /// after a successful dispatch.
    pub fn domain_events(&self) -> Vec<DomainEvent> {
        (self.domain_events_fn)(self.message.as_ref())
    }

    /// Fully-qualified Rust type name of the wrapped message — the analog
    /// of Go's `reflect.TypeOf(msg).String()`, used as the cache-key
    /// prefix.
    pub fn type_name(&self) -> &'static str {
        self.type_name
    }

    /// [`TypeId`] of the wrapped message — the registry key.
    pub fn type_id(&self) -> TypeId {
        self.type_id
    }

    /// Borrows the wrapped message as its concrete type, or `None` if `C`
    /// is not the wrapped type — Go's `msg.(C)` assertion.
    pub fn downcast_ref<C: 'static>(&self) -> Option<&C> {
        self.message.downcast_ref::<C>()
    }

    /// Runs the message's [`Message::validate`] hook.
    pub fn validate(&self) -> Result<(), CqrsError> {
        (self.validate_fn)(self.message.as_ref())
    }

    /// Reads the message's [`Message::cache_ttl`] opt-in, honouring a
    /// per-dispatch override set via [`Envelope::with_cache_ttl`].
    pub fn cache_ttl(&self) -> Option<Duration> {
        match self.cache_ttl_override {
            Some(overridden) => overridden,
            None => (self.cache_ttl_fn)(self.message.as_ref()),
        }
    }

    /// JSON-encodes the message — the value half of the query-cache key,
    /// Go's `json.Marshal(msg)` inside `keyOf`.
    pub fn cache_json(&self) -> Result<Vec<u8>, CqrsError> {
        (self.cache_json_fn)(self.message.as_ref())
    }
}

/// Downcasts an erased message reference back to its concrete type. The
/// envelope constructor guarantees the invariant.
fn erased<C: Message>(message: ErasedRef<'_>) -> &C {
    message
        .downcast_ref::<C>()
        .expect("firefly/cqrs: envelope message type invariant")
}

/// A type-erased, shareable handler result — Go's `(any, error)` success
/// half. `Arc`-backed so the query cache can memoise it and hand the same
/// value to many callers.
#[derive(Clone)]
pub struct AnyResult {
    value: Arc<dyn Any + Send + Sync>,
    type_name: &'static str,
}

impl AnyResult {
    /// Erases a concrete handler result.
    pub fn new<R: Send + Sync + 'static>(value: R) -> Self {
        Self {
            value: Arc::new(value),
            type_name: type_name::<R>(),
        }
    }

    /// Fully-qualified Rust type name of the wrapped result — used in the
    /// `result type mismatch` diagnostic, like Go's `%T`.
    pub fn type_name(&self) -> &'static str {
        self.type_name
    }

    /// Borrows the wrapped result as its concrete type.
    pub fn downcast_ref<R: 'static>(&self) -> Option<&R> {
        self.value.downcast_ref::<R>()
    }

    /// Clones the wrapped result out as its concrete type — Go's
    /// `res.(R)` assertion (Go copies the value; Rust clones).
    pub fn downcast_cloned<R: Clone + 'static>(&self) -> Option<R> {
        self.value.downcast_ref::<R>().cloned()
    }
}

/// The boxed future every dynamic handler returns.
pub type HandlerFuture = BoxFuture<'static, Result<AnyResult, CqrsError>>;

/// The dynamic dispatch handler shape — Go's
/// `anyHandler func(ctx, msg any) (any, error)`, made public so custom
/// [`Middleware`] can be written outside this crate.
pub type DynHandler = Arc<dyn Fn(Arc<Envelope>) -> HandlerFuture + Send + Sync>;

/// Decorates a [`DynHandler`] with a cross-cutting concern — validation,
/// authorization, query caching, tracing.
///
/// Go's `Middleware func(next anyHandler) anyHandler` as an object-safe
/// trait. Middleware registered first wraps outermost (see
/// [`Bus::use_middleware`]).
pub trait Middleware: Send + Sync + 'static {
    /// Returns a handler that runs this middleware's concern around
    /// `next`.
    fn wrap(&self, next: DynHandler) -> DynHandler;
}

/// A registered handler plus the message type name it serves (for
/// [`Bus::handler_names`]).
struct RegisteredHandler {
    name: &'static str,
    kind: MessageKind,
    handler: DynHandler,
}

#[derive(Default)]
struct Inner {
    handlers: HashMap<TypeId, RegisteredHandler>,
    middlewares: Vec<Arc<dyn Middleware>>,
}

/// The dispatch boundary. Commands and queries share the same registry —
/// they're disambiguated only by convention (commands mutate, queries
/// read).
///
/// The registry is keyed by [`TypeId`] (Go keys by `reflect.Type`);
/// interior mutability makes a shared `Arc<Bus>` registerable and
/// dispatchable from any task.
#[derive(Default)]
pub struct Bus {
    inner: RwLock<Inner>,
}

impl Bus {
    /// Returns an empty bus — Go's `cqrs.New()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a middleware to the dispatch chain — Go's `Bus.Use`.
    ///
    /// Middlewares run in registration order: the first registered wraps
    /// outermost. The chain is assembled per dispatch, so middleware
    /// added after a handler still applies to it.
    pub fn use_middleware(&self, middleware: impl Middleware) {
        self.inner
            .write()
            .expect("firefly/cqrs: bus lock poisoned")
            .middlewares
            .push(Arc::new(middleware));
    }

    /// Installs `handler` for messages of type `C` — Go's
    /// `cqrs.Register[C, R](bus, h)`. Registering twice for the same `C`
    /// overwrites the previous handler.
    pub fn register<C, R, H, Fut>(&self, handler: H)
    where
        C: Message,
        R: Send + Sync + 'static,
        H: Fn(C) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R, CqrsError>> + Send + 'static,
    {
        self.register_with_context(move |message: C, _ctx| handler(message));
    }

    /// Installs a **context-aware** handler for messages of type `C` —
    /// the Rust spelling of pyfly's `ExecutionContext`-aware handlers
    /// (`do_handle(self, command, context)`).
    ///
    /// The handler receives the [`ExecutionContext`] attached to the
    /// dispatch via [`Bus::send_with_context`] /
    /// [`Bus::query_with_context`] or a fluent builder, and `None` for a
    /// plain [`Bus::send`]. Registering for the same `C` (through this
    /// method or [`Bus::register`]) overwrites the previous handler.
    pub fn register_with_context<C, R, H, Fut>(&self, handler: H)
    where
        C: Message,
        R: Send + Sync + 'static,
        H: Fn(C, Option<ExecutionContext>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<R, CqrsError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let erased: DynHandler = Arc::new(move |env: Arc<Envelope>| -> HandlerFuture {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let message = env
                    .downcast_ref::<C>()
                    .ok_or(CqrsError::HandlerTypeMismatch {
                        want: type_name::<C>(),
                        got: env.type_name(),
                    })?
                    .clone();
                let context = env.context().cloned();
                handler(message, context).await.map(AnyResult::new)
            })
        });
        self.inner
            .write()
            .expect("firefly/cqrs: bus lock poisoned")
            .handlers
            .insert(
                TypeId::of::<C>(),
                RegisteredHandler {
                    name: type_name::<C>(),
                    kind: C::kind(),
                    handler: erased,
                },
            );
    }

    /// Lists the fully-qualified type names of every registered message
    /// handler, sorted alphabetically — pyfly's
    /// `HandlerRegistry.get_registered_command_types()` /
    /// `get_registered_query_types()` surface, consumed by the admin
    /// actuator endpoint.
    pub fn handler_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self
            .inner
            .read()
            .expect("firefly/cqrs: bus lock poisoned")
            .handlers
            .values()
            .map(|registered| registered.name)
            .collect();
        names.sort_unstable();
        names
    }

    /// The fully-qualified type names of every registered handler of `kind`,
    /// sorted — the building block behind [`Bus::command_handler_names`] /
    /// [`Bus::query_handler_names`].
    pub fn handler_names_by_kind(&self, kind: MessageKind) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self
            .inner
            .read()
            .expect("firefly/cqrs: bus lock poisoned")
            .handlers
            .values()
            .filter(|registered| registered.kind == kind)
            .map(|registered| registered.name)
            .collect();
        names.sort_unstable();
        names
    }

    /// Registered **command** handler type names (pyfly's
    /// `get_registered_command_types()`).
    pub fn command_handler_names(&self) -> Vec<&'static str> {
        self.handler_names_by_kind(MessageKind::Command)
    }

    /// Registered **query** handler type names (pyfly's
    /// `get_registered_query_types()`).
    pub fn query_handler_names(&self) -> Vec<&'static str> {
        self.handler_names_by_kind(MessageKind::Query)
    }

    /// The number of registered handlers.
    pub fn handler_count(&self) -> usize {
        self.inner
            .read()
            .expect("firefly/cqrs: bus lock poisoned")
            .handlers
            .len()
    }

    /// Whether a handler is registered for message type `C`.
    pub fn has_handler<C: Message>(&self) -> bool {
        self.inner
            .read()
            .expect("firefly/cqrs: bus lock poisoned")
            .handlers
            .contains_key(&TypeId::of::<C>())
    }

    /// Removes the handler for message type `C`, returning whether one was
    /// present (pyfly's `HandlerRegistry.unregister`).
    pub fn unregister<C: Message>(&self) -> bool {
        self.inner
            .write()
            .expect("firefly/cqrs: bus lock poisoned")
            .handlers
            .remove(&TypeId::of::<C>())
            .is_some()
    }

    /// Dispatches a command and returns its typed result — Go's
    /// `cqrs.Send[C, R](ctx, bus, cmd)`.
    ///
    /// Fails with [`CqrsError::NoHandler`] when nothing is registered for
    /// `C` (middleware never runs in that case, matching Go), and with
    /// [`CqrsError::ResultTypeMismatch`] when the handler's result type
    /// is not `R`.
    pub async fn send<C, R>(&self, command: C) -> Result<R, CqrsError>
    where
        C: Message,
        R: Clone + Send + Sync + 'static,
    {
        self.dispatch_typed(Envelope::new(command)).await
    }

    /// A synonym for [`Bus::send`] — kept for readability, like Go's
    /// `cqrs.Query`.
    pub async fn query<Q, R>(&self, query: Q) -> Result<R, CqrsError>
    where
        Q: Message,
        R: Clone + Send + Sync + 'static,
    {
        self.send(query).await
    }

    /// [`Bus::send`] with an [`ExecutionContext`] attached — pyfly's
    /// `command_bus.send(cmd, context=ctx)`. The context reaches
    /// [`Message::authorize`], any middleware reading
    /// [`Envelope::context`], and handlers registered via
    /// [`Bus::register_with_context`].
    pub async fn send_with_context<C, R>(
        &self,
        command: C,
        context: ExecutionContext,
    ) -> Result<R, CqrsError>
    where
        C: Message,
        R: Clone + Send + Sync + 'static,
    {
        self.dispatch_typed(Envelope::new(command).with_context(context))
            .await
    }

    /// [`Bus::query`] with an [`ExecutionContext`] attached — pyfly's
    /// `query_bus.query(q, context=ctx)`.
    pub async fn query_with_context<Q, R>(
        &self,
        query: Q,
        context: ExecutionContext,
    ) -> Result<R, CqrsError>
    where
        Q: Message,
        R: Clone + Send + Sync + 'static,
    {
        self.send_with_context(query, context).await
    }

    /// Dispatches a pre-built [`Envelope`] and downcasts the result —
    /// the typed tail shared by [`Bus::send`], the `*_with_context`
    /// variants, and the fluent builders' `execute_with`.
    pub async fn dispatch_typed<R>(&self, envelope: Envelope) -> Result<R, CqrsError>
    where
        R: Clone + Send + Sync + 'static,
    {
        let result = self.dispatch_envelope(envelope).await?;
        result
            .downcast_cloned::<R>()
            .ok_or(CqrsError::ResultTypeMismatch {
                want: type_name::<R>(),
                got: result.type_name(),
            })
    }

    /// Dispatches a pre-built [`Envelope`] through the middleware chain
    /// and returns the type-erased result — exposed so custom dispatch
    /// surfaces (builders, transports) can attach context, metadata, or
    /// cache overrides before dispatching.
    pub async fn dispatch_envelope(&self, envelope: Envelope) -> Result<AnyResult, CqrsError> {
        let (handler, middlewares) = {
            let inner = self.inner.read().expect("firefly/cqrs: bus lock poisoned");
            let handler = inner
                .handlers
                .get(&envelope.type_id())
                .map(|registered| Arc::clone(&registered.handler))
                .ok_or(CqrsError::NoHandler {
                    type_name: envelope.type_name(),
                })?;
            (handler, inner.middlewares.clone())
        };
        let mut chain = handler;
        for middleware in middlewares.iter().rev() {
            chain = middleware.wrap(chain);
        }
        chain(Arc::new(envelope)).await
    }

    /// Dispatches `command` and then publishes the *result's* domain events
    /// through `publisher` — pyfly's `result.domain_events` half of
    /// `DefaultCommandBus._try_publish_events`.
    ///
    /// The full middleware chain runs first (including any
    /// [`DomainEventMiddleware`](crate::DomainEventMiddleware) that harvests
    /// the *command's* events); then, on success, the events the *result*
    /// type exposes via [`DomainEvents`](crate::DomainEvents) are published.
    /// Use this when a handler returns an aggregate/result carrying the
    /// events it produced (rather than the command).
    ///
    /// Events publish only after a successful dispatch, honouring `strategy`
    /// for publish failures (see
    /// [`publish_domain_events`](crate::publish_domain_events)).
    pub async fn send_publishing<C, R>(
        &self,
        command: C,
        publisher: &dyn crate::CommandEventPublisher,
        destination: Option<&str>,
        strategy: crate::EventFailureStrategy,
    ) -> Result<R, CqrsError>
    where
        C: Message,
        R: Clone + Send + Sync + 'static + crate::DomainEvents,
    {
        let result: R = self.send(command).await?;
        let events = result.domain_events();
        if !events.is_empty() {
            crate::publish_domain_events(publisher, &events, destination, strategy).await?;
        }
        Ok(result)
    }
}

/// Middleware that short-circuits dispatch when the message's
/// [`Message::validate`] hook fails — Go's `ValidationMiddleware()`.
///
/// Messages that keep the default (always-valid) hook pass through
/// untouched, mirroring Go's "doesn't implement `Validatable`" case.
#[derive(Clone, Copy, Debug, Default)]
pub struct ValidationMiddleware;

impl ValidationMiddleware {
    /// Returns the validation middleware.
    pub fn new() -> Self {
        Self
    }
}

impl Middleware for ValidationMiddleware {
    fn wrap(&self, next: DynHandler) -> DynHandler {
        Arc::new(move |env: Arc<Envelope>| -> HandlerFuture {
            let next = Arc::clone(&next);
            Box::pin(async move {
                env.validate()?;
                next(env).await
            })
        })
    }
}
