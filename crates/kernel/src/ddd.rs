//! Domain-driven design building blocks — the Rust port of `pyfly.domain`.
//!
//! This module is the zero-dependency DDD kit shared by every layer: a
//! composable in-memory [`Specification`] pattern, an [`Entity`]
//! identity contract, and the *non-event-sourced* aggregate primitives
//! — a [`PendingEvents`] buffer with raise/snapshot/drain semantics,
//! the [`EventMeta`] auto-identity pair, and the
//! [`TransientDomainEvent`] trait.
//!
//! It is deliberately distinct from the `firefly-eventsourcing` crate:
//! that crate's `AggregateRoot` is the *event-sourced* variant
//! (version-stamped events, wire format, `EventStore`-coupled), the
//! analog of `pyfly.eventsourcing`. The primitives here are for plain
//! state-persisted aggregates that merely collect transient events for
//! post-commit publication, the analog of `pyfly.domain`.
//!
//! Idiom adaptations from the Python original:
//!
//! * `Specification.of(callable)` becomes a blanket impl — any
//!   `Fn(&T) -> bool` closure *is* a [`Specification<T>`].
//! * The `&` / `|` / `~` operators become the [`Specification::and`],
//!   [`Specification::or`], and [`Specification::not`] combinators.
//! * The `Entity` base class becomes a trait; consumers implement
//!   `PartialEq` via [`Entity::same_identity`] instead of inheriting
//!   an `__eq__` override.
//! * `AggregateRoot._pending_events` becomes the standalone
//!   [`PendingEvents<E>`] buffer, generic over a per-aggregate event
//!   enum (or [`BoxedDomainEvent`] for heterogeneous buses).
//! * `ValueObject` is omitted: frozen-dataclass immutability and
//!   `replace()` are native Rust (`Clone` + struct-update syntax).
//! * `DomainRepository` is omitted: its semantics map onto
//!   `firefly_data::Repository<T, K>` (`save` ~ `add`, `find_by_id` ~
//!   `find`, `delete` ~ `remove`; `next_id` is `Uuid::new_v4()`).
//!
//! ```
//! use firefly_kernel::ddd::{PendingEvents, Specification};
//!
//! struct Customer { age: u32, premium: bool }
//!
//! struct IsAdult;
//! impl Specification<Customer> for IsAdult {
//!     fn is_satisfied_by(&self, c: &Customer) -> bool { c.age >= 18 }
//! }
//!
//! let spec = IsAdult.and(|c: &Customer| c.premium);
//! assert!(spec.is_satisfied_by(&Customer { age: 30, premium: true }));
//! assert!(!spec.is_satisfied_by(&Customer { age: 30, premium: false }));
//! ```

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::Clock;

// ---------------------------------------------------------------------------
// Specification
// ---------------------------------------------------------------------------

/// Composable in-memory predicate — the classic Eric Evans
/// Specification pattern. An object that knows whether a given domain
/// object satisfies a business rule; complex rules are assembled from
/// small, named building blocks via [`and`](Specification::and),
/// [`or`](Specification::or), and [`not`](Specification::not).
///
/// Any `Fn(&T) -> bool` closure is a `Specification<T>` through the
/// blanket impl, replacing pyfly's `Specification.of(callable)`:
///
/// ```
/// use firefly_kernel::ddd::Specification;
///
/// let is_even = |n: &i32| n % 2 == 0;
/// assert!(is_even.is_satisfied_by(&4));
/// assert!(is_even.not().is_satisfied_by(&3));
/// ```
///
/// This is the in-memory predicate used inside aggregates and domain
/// services; query-backend predicates (pushed down into SQL) are a
/// separate abstraction.
pub trait Specification<T> {
    /// Returns `true` iff `candidate` satisfies the rule.
    fn is_satisfied_by(&self, candidate: &T) -> bool;

    /// Combines two specifications with logical AND — pyfly's `&`.
    fn and<S>(self, other: S) -> AndSpec<Self, S>
    where
        Self: Sized,
        S: Specification<T>,
    {
        AndSpec {
            left: self,
            right: other,
        }
    }

    /// Combines two specifications with logical OR — pyfly's `|`.
    fn or<S>(self, other: S) -> OrSpec<Self, S>
    where
        Self: Sized,
        S: Specification<T>,
    {
        OrSpec {
            left: self,
            right: other,
        }
    }

    /// Negates this specification — pyfly's `~`.
    fn not(self) -> NotSpec<Self>
    where
        Self: Sized,
    {
        NotSpec { inner: self }
    }
}

/// Blanket impl: every `Fn(&T) -> bool` closure is a specification —
/// the Rust analog of pyfly's `Specification.of(callable)`.
impl<T, F> Specification<T> for F
where
    F: Fn(&T) -> bool,
{
    fn is_satisfied_by(&self, candidate: &T) -> bool {
        self(candidate)
    }
}

/// Logical AND of two specifications; built by [`Specification::and`].
#[derive(Debug, Clone, Copy)]
pub struct AndSpec<A, B> {
    left: A,
    right: B,
}

impl<T, A, B> Specification<T> for AndSpec<A, B>
where
    A: Specification<T>,
    B: Specification<T>,
{
    fn is_satisfied_by(&self, candidate: &T) -> bool {
        self.left.is_satisfied_by(candidate) && self.right.is_satisfied_by(candidate)
    }
}

/// Logical OR of two specifications; built by [`Specification::or`].
#[derive(Debug, Clone, Copy)]
pub struct OrSpec<A, B> {
    left: A,
    right: B,
}

impl<T, A, B> Specification<T> for OrSpec<A, B>
where
    A: Specification<T>,
    B: Specification<T>,
{
    fn is_satisfied_by(&self, candidate: &T) -> bool {
        self.left.is_satisfied_by(candidate) || self.right.is_satisfied_by(candidate)
    }
}

/// Logical NOT of a specification; built by [`Specification::not`].
#[derive(Debug, Clone, Copy)]
pub struct NotSpec<S> {
    inner: S,
}

impl<T, S> Specification<T> for NotSpec<S>
where
    S: Specification<T>,
{
    fn is_satisfied_by(&self, candidate: &T) -> bool {
        !self.inner.is_satisfied_by(candidate)
    }
}

// ---------------------------------------------------------------------------
// Entity
// ---------------------------------------------------------------------------

/// DDD entity contract — identity-based equality.
///
/// An *entity* is an object whose identity matters more than its
/// attribute values: two entities are the same iff they share an
/// identifier, regardless of any other state. Entities whose
/// [`id`](Entity::id) is `None` are *transient* (newly created, not
/// yet persisted) and never share identity with anything.
///
/// Where pyfly's `Entity` base class overrides `__eq__`/`__hash__`,
/// Rust consumers implement `PartialEq` explicitly via the provided
/// [`same_identity`](Entity::same_identity) helper:
///
/// ```
/// use firefly_kernel::ddd::Entity;
///
/// struct Account { id: Option<i64>, balance: i64 }
///
/// impl Entity for Account {
///     type Id = i64;
///     fn id(&self) -> Option<&i64> { self.id.as_ref() }
/// }
///
/// impl PartialEq for Account {
///     fn eq(&self, other: &Self) -> bool { self.same_identity(other) }
/// }
///
/// let a = Account { id: Some(1), balance: 100 };
/// let b = Account { id: Some(1), balance: 999 };
/// assert!(a == b); // same identity, different state
/// ```
///
/// Equality across *different* entity types — which pyfly rejects at
/// runtime via a `type(self) is not type(other)` check — is prevented
/// statically here: `same_identity` only accepts `&Self`.
pub trait Entity {
    /// The identifier type.
    type Id: PartialEq + std::hash::Hash;

    /// Returns the identifier, or `None` while the entity is transient.
    fn id(&self) -> Option<&Self::Id>;

    /// `true` while this entity has no identifier assigned yet
    /// (newly created, not yet persisted).
    fn is_transient(&self) -> bool {
        self.id().is_none()
    }

    /// Identity-based equality: `true` iff both entities carry an id
    /// and the ids are equal. Transient entities never share identity
    /// — pyfly's `id is None` rule.
    fn same_identity(&self, other: &Self) -> bool {
        match (self.id(), other.id()) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Transient domain events
// ---------------------------------------------------------------------------

/// Auto-assigned identity of a transient domain event: a fresh v4
/// [`Uuid`] and a UTC timestamp — pyfly `DomainEvent`'s
/// `event_id` / `occurred_at` default factories.
///
/// Embed one in each event struct and return it from
/// [`TransientDomainEvent::meta`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventMeta {
    /// Unique id of this event occurrence (UUID v4).
    pub event_id: Uuid,
    /// UTC instant at which the event was created.
    pub occurred_at: DateTime<Utc>,
}

impl EventMeta {
    /// Builds a meta with a fresh UUID v4 and the current UTC time.
    pub fn new() -> Self {
        Self {
            event_id: Uuid::new_v4(),
            occurred_at: Utc::now(),
        }
    }

    /// Builds a meta with a fresh UUID v4 and the time read from the
    /// given [`Clock`] — for deterministic tests.
    pub fn with_clock(clock: &dyn Clock) -> Self {
        Self {
            event_id: Uuid::new_v4(),
            occurred_at: clock.now(),
        }
    }
}

impl Default for EventMeta {
    fn default() -> Self {
        Self::new()
    }
}

/// A *transient* domain event — something that happened in the domain,
/// collected by an aggregate during a transaction and dispatched by
/// the application service after the unit of work commits.
///
/// This is the non-event-sourced counterpart of the
/// `firefly-eventsourcing` `DomainEvent`: it carries **no version and
/// no wire format** — it is never replayed to rebuild state.
///
/// [`event_type`](TransientDomainEvent::event_type) defaults to the
/// short type name (the last path segment of
/// [`std::any::type_name`]), mirroring pyfly's
/// `type(self).__name__`.
pub trait TransientDomainEvent {
    /// The auto-assigned identity (event id + occurrence time).
    fn meta(&self) -> &EventMeta;

    /// Unique id of this event occurrence.
    fn event_id(&self) -> Uuid {
        self.meta().event_id
    }

    /// UTC instant at which the event occurred.
    fn occurred_at(&self) -> DateTime<Utc> {
        self.meta().occurred_at
    }

    /// Logical event type — defaults to the short type name (e.g.
    /// `"OrderPlaced"` for `my_app::orders::OrderPlaced`).
    fn event_type(&self) -> &'static str {
        short_type_name(std::any::type_name::<Self>())
    }
}

/// A type-erased transient domain event, for heterogeneous buffers and
/// buses: `PendingEvents<BoxedDomainEvent>` mirrors pyfly's untyped
/// `list[DomainEvent]` exactly.
pub type BoxedDomainEvent = Box<dyn TransientDomainEvent + Send + Sync>;

impl TransientDomainEvent for BoxedDomainEvent {
    fn meta(&self) -> &EventMeta {
        (**self).meta()
    }

    fn event_id(&self) -> Uuid {
        (**self).event_id()
    }

    fn occurred_at(&self) -> DateTime<Utc> {
        (**self).occurred_at()
    }

    fn event_type(&self) -> &'static str {
        (**self).event_type()
    }
}

/// Returns the last path segment of a (possibly generic) type name:
/// `"a::b::Event"` becomes `"Event"`. Generic parameters are kept
/// verbatim after the trim.
fn short_type_name(full: &'static str) -> &'static str {
    let base = full.split('<').next().unwrap_or(full);
    match base.rfind("::") {
        Some(i) => &full[i + 2..],
        None => full,
    }
}

// ---------------------------------------------------------------------------
// PendingEvents
// ---------------------------------------------------------------------------

/// The pending-events buffer of a *non-event-sourced* aggregate root —
/// pyfly `AggregateRoot`'s `raise_event` / `pending_events` /
/// `clear_events` triple, extracted into a standalone struct so any
/// aggregate can embed it.
///
/// State changes happen through methods on the aggregate, which
/// [`raise`](PendingEvents::raise) events; the application service
/// snapshots [`pending`](PendingEvents::pending) during the unit of
/// work and [`drain`](PendingEvents::drain)s once the aggregate has
/// been persisted, publishing the returned events to the event bus.
///
/// Generic over a per-aggregate event enum (idiomatic), or over
/// [`BoxedDomainEvent`] for heterogeneous buses.
///
/// ```
/// use firefly_kernel::ddd::PendingEvents;
///
/// #[derive(Debug, PartialEq)]
/// enum OrderEvent { Placed { total: u32 }, Shipped }
///
/// let mut events = PendingEvents::new();
/// events.raise(OrderEvent::Placed { total: 100 });
/// events.raise(OrderEvent::Shipped);
/// assert_eq!(events.pending().len(), 2);
///
/// let drained = events.drain();
/// assert_eq!(drained.len(), 2);
/// assert!(events.is_empty());
/// ```
#[derive(Debug, Clone)]
pub struct PendingEvents<E> {
    events: Vec<E>,
}

impl<E> PendingEvents<E> {
    /// Creates an empty buffer.
    pub fn new() -> Self {
        Self { events: Vec::new() }
    }

    /// Queues `event` for publication after the unit of work commits —
    /// pyfly's `raise_event`.
    pub fn raise(&mut self, event: E) {
        self.events.push(event);
    }

    /// Returns a view of the pending events — pyfly's
    /// `pending_events()`. Where Python returns a defensive copy, the
    /// borrow checker makes the slice safe: the buffer cannot raise
    /// more events while the view is alive (clone with `.to_vec()` for
    /// an owned snapshot).
    pub fn pending(&self) -> &[E] {
        &self.events
    }

    /// Drains the pending events and returns them — pyfly's
    /// `clear_events()`. Call once the aggregate has been persisted,
    /// then publish the returned events to the event bus.
    pub fn drain(&mut self) -> Vec<E> {
        std::mem::take(&mut self.events)
    }

    /// Number of pending events.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// `true` when no events are pending.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

impl<E> Default for PendingEvents<E> {
    fn default() -> Self {
        Self::new()
    }
}
