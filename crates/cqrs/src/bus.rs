//! The type-dispatched command/query bus and its middleware contract.

use std::any::{type_name, Any, TypeId};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use futures::future::BoxFuture;
use serde::Serialize;

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
        }
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

    /// Reads the message's [`Message::cache_ttl`] opt-in.
    pub fn cache_ttl(&self) -> Option<Duration> {
        (self.cache_ttl_fn)(self.message.as_ref())
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

#[derive(Default)]
struct Inner {
    handlers: HashMap<TypeId, DynHandler>,
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
                handler(message).await.map(AnyResult::new)
            })
        });
        self.inner
            .write()
            .expect("firefly/cqrs: bus lock poisoned")
            .handlers
            .insert(TypeId::of::<C>(), erased);
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
        let result = self.dispatch(Envelope::new(command)).await?;
        result
            .downcast_cloned::<R>()
            .ok_or(CqrsError::ResultTypeMismatch {
                want: type_name::<R>(),
                got: result.type_name(),
            })
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

    async fn dispatch(&self, envelope: Envelope) -> Result<AnyResult, CqrsError> {
        let (handler, middlewares) =
            {
                let inner = self.inner.read().expect("firefly/cqrs: bus lock poisoned");
                let handler = inner.handlers.get(&envelope.type_id()).cloned().ok_or(
                    CqrsError::NoHandler {
                        type_name: envelope.type_name(),
                    },
                )?;
                (handler, inner.middlewares.clone())
            };
        let mut chain = handler;
        for middleware in middlewares.iter().rev() {
            chain = middleware.wrap(chain);
        }
        chain(Arc::new(envelope)).await
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
